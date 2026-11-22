//! The decoders the system has (`MediaCodecList`), asked once at the start: for each
//! codec the app plays, the decoder it would get, the largest picture that decoder
//! takes and how many of it run at once. What the PC is told comes from this, not from
//! a list of headsets.

use jni::objects::{JObject, JObjectArray, JString, JValue};
use jni::{Env, jni_sig, jni_str};
use log::{info, warn};
use prostir_proto::Codec;

use super::with_app_context;
use crate::caps::Decoder;

/// The codecs the app plays: what the PC may send. AV1 is left out until the app's
/// decoding of it has been tried on a headset.
const PLAYED: [Codec; 2] = [Codec::H264, Codec::H265];

/// `MediaCodecList.REGULAR_CODECS`.
const REGULAR_CODECS: i32 = 0;

/// One decoder of the list, for one codec.
#[derive(Debug)]
struct Found {
    name: String,
    codec: Codec,
    hardware: bool,
    max_w: u32,
    max_h: u32,
    /// `getMaxSupportedInstances`, when the system says.
    instances: Option<u32>,
}

/// The decoder of each codec the app plays, with how many of it run at once (`None`:
/// the system did not say); `None` when the system could not be asked at all.
pub fn decoders() -> Option<Vec<(Decoder, Option<u32>)>> {
    let found = with_app_context(|env, _| list(env))?;
    let mut out = Vec::new();
    for codec in PLAYED {
        // As a decoder is made for a type: the system's first one, a hardware one if
        // there is.
        let mut of = found.iter().filter(|f| f.codec == codec);
        let Some(chosen) = of.clone().find(|f| f.hardware).or_else(|| of.next()) else {
            info!("no decoder for {}", codec.mime());
            continue;
        };
        info!(
            "decoder for {}: {} up to {}x{}, {} at once{}",
            codec.mime(),
            chosen.name,
            chosen.max_w,
            chosen.max_h,
            chosen
                .instances
                .map_or_else(|| "unsaid".to_owned(), |n| n.to_string()),
            if chosen.hardware { "" } else { " (software)" }
        );
        out.push((
            Decoder {
                codec,
                max_w: chosen.max_w,
                max_h: chosen.max_h,
            },
            chosen.instances,
        ));
    }
    Some(out)
}

/// Every decoder of the system's list for a codec the app plays.
fn list(env: &mut Env<'_>) -> jni::errors::Result<Vec<Found>> {
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
            of_codec(env, &info)
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

/// What one entry of the list (`MediaCodecInfo`) decodes of the codecs the app plays.
fn of_codec(env: &mut Env<'_>, info: &JObject<'_>) -> jni::errors::Result<Vec<Found>> {
    let flag = |env: &mut Env<'_>, name| -> jni::errors::Result<bool> {
        env.call_method(info, name, jni_sig!("()Z"), &[])?.z()
    };
    // An alias is another name of a decoder listed anyway.
    if flag(env, jni_str!("isEncoder"))? || flag(env, jni_str!("isAlias"))? {
        return Ok(Vec::new());
    }
    let hardware = flag(env, jni_str!("isHardwareAccelerated"))?;
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
        let max_w = upper(env, &video, jni_str!("getSupportedWidths"))?;
        let max_h = upper(env, &video, jni_str!("getSupportedHeights"))?;
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
            max_w,
            max_h,
            instances: u32::try_from(instances).ok().filter(|n| *n > 0),
        });
    }
    Ok(out)
}

/// The upper end of the range of whole numbers `method` of `video` gives
/// (`Range<Integer>`), 0 when it gives none.
fn upper(
    env: &mut Env<'_>,
    video: &JObject<'_>,
    method: &'static jni::strings::JNIStr,
) -> jni::errors::Result<u32> {
    let range = env
        .call_method(video, method, jni_sig!("()Landroid/util/Range;"), &[])?
        .l()?;
    if range.is_null() {
        return Ok(0);
    }
    let value = env
        .call_method(
            &range,
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
