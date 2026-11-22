//! The decoders the system has (`MediaCodecList`), asked once at the start: for each
//! codec the app plays, the decoder it would get, the largest picture that decoder
//! takes at 60 frames a second, and how many of it run at once. What the PC is told
//! comes from this, not from a list of headsets.

use jni::objects::{JObject, JObjectArray, JString, JValue};
use jni::strings::JNIStr;
use jni::{Env, jni_sig, jni_str};
use log::{info, warn};
use prostir_proto::Codec;

use super::with_app_context;
use crate::caps::{self, DECODE_RATE, Decoder, Sizes};

/// The codecs the app plays: what the PC may send. AV1 is left out until the app's
/// decoding of it has been tried on a headset.
const PLAYED: [Codec; 2] = [Codec::H264, Codec::H265];

/// `MediaCodecList.REGULAR_CODECS`.
const REGULAR_CODECS: i32 = 0;
/// Android 10 (API 29): `isAlias` and `isHardwareAccelerated` come with it.
const API_ALIASES: i32 = 29;

/// One decoder of the system's list, for one codec.
#[derive(Debug)]
pub struct Found {
    pub name: String,
    pub codec: Codec,
    pub hardware: bool,
    /// The largest picture as the system's ranges say it: the widest, and the tallest
    /// at that width.
    pub said: (u32, u32),
    /// The largest that decodes at 60 frames a second (0×0: none found).
    pub size: (u32, u32),
    /// `getMaxSupportedInstances`, when the system says.
    pub instances: Option<u32>,
}

impl Found {
    /// What is told of it: the size it keeps up at.
    pub fn decoder(&self) -> Decoder {
        Decoder {
            codec: self.codec,
            max_w: self.size.0,
            max_h: self.size.1,
        }
    }

    /// What the system's ranges said, before the check at 60 frames a second.
    pub fn said(&self) -> Decoder {
        Decoder {
            codec: self.codec,
            max_w: self.said.0,
            max_h: self.said.1,
        }
    }
}

/// The decoder of each codec the app plays (`None` when the system could not be asked
/// at all).
pub fn decoders() -> Option<Vec<Found>> {
    let mut found = with_app_context(|env, _| list(env))?;
    let mut out = Vec::new();
    for codec in PLAYED {
        // As a decoder is made for a type: the system's first one, a hardware one if
        // there is.
        let first_hardware = found.iter().position(|f| f.codec == codec && f.hardware);
        let Some(i) = first_hardware.or_else(|| found.iter().position(|f| f.codec == codec)) else {
            info!("no decoder for {}", codec.mime());
            continue;
        };
        let chosen = found.swap_remove(i);
        info!(
            "decoder for {}: {}, said up to {}x{}, at {DECODE_RATE} fps {}x{}, {} at once{}",
            codec.mime(),
            chosen.name,
            chosen.said.0,
            chosen.said.1,
            chosen.size.0,
            chosen.size.1,
            chosen
                .instances
                .map_or_else(|| "unsaid".to_owned(), |n| n.to_string()),
            if chosen.hardware { "" } else { " (software)" }
        );
        out.push(chosen);
    }
    Some(out)
}

/// `r`, or `None` with the Java exception it left cleared (the next call would fail
/// otherwise).
fn quietly<T>(env: &Env<'_>, r: jni::errors::Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(_) => {
            env.exception_clear();
            None
        }
    }
}

/// `Build.VERSION.SDK_INT`, 0 when it cannot be read.
fn sdk(env: &mut Env<'_>) -> i32 {
    let r = env
        .get_static_field(
            jni_str!("android/os/Build$VERSION"),
            jni_str!("SDK_INT"),
            jni_sig!("I"),
        )
        .and_then(jni::JValueOwned::i);
    quietly(env, r).unwrap_or(0)
}

/// Every decoder of the system's list for a codec the app plays.
fn list(env: &mut Env<'_>) -> jni::errors::Result<Vec<Found>> {
    let sdk = sdk(env);
    let list = env.new_object(
        jni_str!("android/media/MediaCodecList"),
        jni_sig!("(I)V"),
        &[JValue::Int(REGULAR_CODECS)],
    )?;
    let infos = env
        .call_method(
            &list,
            jni_str!("getCodecInfos"),
            jni_sig!("()[Landroid/media/MediaCodecInfo;"),
            &[],
        )?
        .l()?;
    let infos = env.cast_local::<JObjectArray<'_>>(infos)?;
    let mut out = Vec::new();
    for i in 0..infos.len(env)? {
        // Each in a frame of its own: a long list must not fill the table of local
        // references.
        let one = env.with_local_frame(32, |env| -> jni::errors::Result<Vec<Found>> {
            let info = infos.get_element(env, i)?;
            of_codec(env, &info, sdk)
        });
        match one {
            Ok(found) => out.extend(found),
            // One decoder the system cannot describe is left out, not the whole list
            // (its Java exception cleared, or the next call would fail too).
            Err(e) => {
                env.exception_clear();
                warn!("a decoder of the system's list: {e}");
            }
        }
    }
    Ok(out)
}

/// What one entry of the list (`MediaCodecInfo`) decodes of the codecs the app plays,
/// on a system of API level `sdk`.
fn of_codec(env: &mut Env<'_>, info: &JObject<'_>, sdk: i32) -> jni::errors::Result<Vec<Found>> {
    let flag = |env: &mut Env<'_>, name| -> jni::errors::Result<bool> {
        env.call_method(info, name, jni_sig!("()Z"), &[])?.z()
    };
    // Before Android 10 the system cannot say: neither an alias nor hardware.
    let newer = sdk >= API_ALIASES;
    // An alias is another name of a decoder listed anyway.
    if flag(env, jni_str!("isEncoder"))? || (newer && flag(env, jni_str!("isAlias"))?) {
        return Ok(Vec::new());
    }
    let hardware = newer && flag(env, jni_str!("isHardwareAccelerated"))?;
    let name = env
        .call_method(
            info,
            jni_str!("getName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let name = env.cast_local::<JString<'_>>(name)?.try_to_string(env)?;
    let types = env
        .call_method(
            info,
            jni_str!("getSupportedTypes"),
            jni_sig!("()[Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let types = env.cast_local::<JObjectArray<'_, JString<'_>>>(types)?;
    let mut out = Vec::new();
    for t in 0..types.len(env)? {
        let mime = types.get_element(env, t)?.try_to_string(env)?;
        let Some(codec) = PLAYED
            .into_iter()
            .find(|c| mime.eq_ignore_ascii_case(c.mime()))
        else {
            continue;
        };
        let java_mime = env.new_string(&mime)?;
        let caps = env
            .call_method(
                info,
                jni_str!("getCapabilitiesForType"),
                jni_sig!("(Ljava/lang/String;)Landroid/media/MediaCodecInfo$CodecCapabilities;"),
                &[JValue::Object(&java_mime)],
            )?
            .l()?;
        if caps.is_null() {
            continue;
        }
        // A decoder only for protected or tunnelled playback is not what the app gets.
        let needs = |env: &mut Env<'_>, feature: &str| -> jni::errors::Result<bool> {
            let feature = env.new_string(feature)?;
            env.call_method(
                &caps,
                jni_str!("isFeatureRequired"),
                jni_sig!("(Ljava/lang/String;)Z"),
                &[JValue::Object(&feature)],
            )?
            .z()
        };
        if needs(env, "secure-playback")? || needs(env, "tunneled-playback")? {
            continue;
        }
        let video = env
            .call_method(
                &caps,
                jni_str!("getVideoCapabilities"),
                jni_sig!("()Landroid/media/MediaCodecInfo$VideoCapabilities;"),
                &[],
            )?
            .l()?;
        if video.is_null() {
            continue;
        }
        let range = env
            .call_method(
                &video,
                jni_str!("getSupportedWidths"),
                jni_sig!("()Landroid/util/Range;"),
                &[],
            )?
            .l()?;
        let max_w = upper(env, &range)?;
        let mut asked = Video { env, video: &video };
        let max_h = asked.tallest_at(max_w).unwrap_or(0);
        let size = caps::largest_size(max_w, max_h, &mut asked);
        let instances = env
            .call_method(
                &caps,
                jni_str!("getMaxSupportedInstances"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        out.push(Found {
            name: name.clone(),
            codec,
            hardware,
            said: (max_w, max_h),
            size,
            instances: u32::try_from(instances).ok().filter(|n| *n > 0),
        });
    }
    Ok(out)
}

/// A decoder's `VideoCapabilities`, asked about sizes.
struct Video<'a, 'e, 'v> {
    env: &'a mut Env<'e>,
    video: &'a JObject<'v>,
}

impl Video<'_, '_, '_> {
    /// The upper end of what `method(n)` gives (`Range<Integer>`); `None` when the
    /// system refuses `n`.
    fn range_for(&mut self, method: &'static JNIStr, n: u32) -> Option<u32> {
        let n = i32::try_from(n).ok()?;
        let r = self
            .env
            .call_method(
                self.video,
                method,
                jni_sig!("(I)Landroid/util/Range;"),
                &[JValue::Int(n)],
            )
            .and_then(jni::JValueOwned::l);
        let range = quietly(self.env, r)?;
        let r = upper(self.env, &range);
        quietly(self.env, r).filter(|n| *n > 0)
    }

    /// The tallest picture `width` wide.
    fn tallest_at(&mut self, width: u32) -> Option<u32> {
        self.range_for(jni_str!("getSupportedHeightsFor"), width)
    }
}

impl Sizes for Video<'_, '_, '_> {
    fn widest_at(&mut self, height: u32) -> Option<u32> {
        self.range_for(jni_str!("getSupportedWidthsFor"), height)
    }

    fn keeps_up(&mut self, w: u32, h: u32) -> bool {
        let (Ok(w), Ok(h)) = (i32::try_from(w), i32::try_from(h)) else {
            return false;
        };
        let r = self
            .env
            .call_method(
                self.video,
                jni_str!("areSizeAndRateSupported"),
                jni_sig!("(IID)Z"),
                &[JValue::Int(w), JValue::Int(h), JValue::Double(DECODE_RATE)],
            )
            .and_then(jni::JValueOwned::z);
        quietly(self.env, r).unwrap_or(false)
    }
}

/// The upper end of `range` (`Range<Integer>`), 0 when there is none.
fn upper(env: &mut Env<'_>, range: &JObject<'_>) -> jni::errors::Result<u32> {
    if range.is_null() {
        return Ok(0);
    }
    let value = env
        .call_method(
            range,
            jni_str!("getUpper"),
            jni_sig!("()Ljava/lang/Comparable;"),
            &[],
        )?
        .l()?;
    if value.is_null() {
        return Ok(0);
    }
    let n = env
        .call_method(&value, jni_str!("intValue"), jni_sig!("()I"), &[])?
        .i()?;
    Ok(u32::try_from(n).unwrap_or(0))
}
