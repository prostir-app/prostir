//! What this headset can do ([`Caps`]), as the shell found it at the start: from the
//! extensions the OpenXR runtime gave and from what the system says (its decoders, its
//! volume). The core decides by these fields only, never by the headset's name or
//! maker, so a headset nobody tried yet gets what it offers and hides the rest.
//!
//! What the menu and the scene offer by which field is [`Offer`] and
//! [`Caps::offers`] (the table of the step 7 note, 4.4); the display rate is chosen by
//! a rule from the rates the runtime offers ([`Caps::rate`]), and the way the room is
//! shown from the ways it offers ([`room_ways`]), not from a list of headsets.

#![forbid(unsafe_code)]

use prostir_proto::Codec;
use prostir_proto::state::{self, HeadsetCaps, caps};

/// A codec the headset decodes: the widest and the tallest picture it takes (each
/// side's upper bound as the system says it), and how many pixels a picture may have
/// to decode at [`DECODE_RATE`]. A decoder's limit is pixels per frame and per second,
/// so a box cannot describe it: 8192 wide fits only short pictures at 60 frames a
/// second, while 3840x2160 and a portrait 1440x2560 both decode fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoder {
    pub codec: Codec,
    pub max_w: u32,
    pub max_h: u32,
    /// `None`: not known (the sides alone then).
    pub max_pixels: Option<u32>,
}

/// How the room behind the monitors is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomWay {
    /// Glasses: the display lets the room through, always (`ADDITIVE`). There is no
    /// "Room" to choose, and the room cannot be dimmed.
    Glasses,
    /// The frame is blended with the cameras' picture by its alpha (`ALPHA_BLEND`).
    Blend,
    /// The cameras' picture as a layer (`XR_FB_passthrough`).
    FbPassthrough,
    /// The cameras' picture as a layer (`XR_HTC_passthrough`).
    HtcPassthrough,
}

impl RoomWay {
    /// As the selftest writes it.
    pub fn name(self) -> &'static str {
        match self {
            RoomWay::Glasses => "glasses",
            RoomWay::Blend => "blend",
            RoomWay::FbPassthrough => "fb_passthrough",
            RoomWay::HtcPassthrough => "htc_passthrough",
        }
    }
}

/// The room behind the monitors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomCaps {
    pub way: RoomWay,
    /// A window onto the real keyboard (the cameras' picture on a rectangle where the
    /// keyboard is).
    pub keyboard_window: bool,
}

/// A way the runtime blends the frame with the world
/// (`xrEnumerateEnvironmentBlendModes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    Opaque,
    Additive,
    AlphaBlend,
}

/// What the runtime offers for the room: its blend modes, the runtime's own preference
/// first, and the extensions turned on for the cameras' picture.
#[derive(Debug, Clone, Copy)]
pub struct RoomFound<'a> {
    pub blend_modes: &'a [BlendMode],
    /// `XR_FB_passthrough` is on.
    pub fb_passthrough: bool,
    /// `XR_HTC_passthrough` is on.
    pub htc_passthrough: bool,
}

/// The ways to show the room this runtime offers, to try in this order (the first that
/// can be made is the only one used; none: no room). A display that lets the room
/// through (`ADDITIVE` preferred, or without `OPAQUE`) is glasses, and nothing else is
/// tried. Otherwise the maker's own way first, as native as the headset has it: the
/// cameras' layer of `XR_FB_passthrough`, then of `XR_HTC_passthrough`, and the frame
/// blended with the cameras (`ALPHA_BLEND`) last. Only one is ever used, so a layer and
/// the blend never fight over the background.
pub fn room_ways(found: &RoomFound<'_>) -> Vec<RoomWay> {
    let modes = found.blend_modes;
    let has = |m| modes.contains(&m);
    if modes.first() == Some(&BlendMode::Additive)
        || (has(BlendMode::Additive) && !has(BlendMode::Opaque))
    {
        return vec![RoomWay::Glasses];
    }
    let mut ways = Vec::new();
    if found.fb_passthrough {
        ways.push(RoomWay::FbPassthrough);
    }
    if found.htc_passthrough {
        ways.push(RoomWay::HtcPassthrough);
    }
    if has(BlendMode::AlphaBlend) {
        ways.push(RoomWay::Blend);
    }
    ways
}

/// The blend mode of a frame while the cameras' room is not blended in: opaque where the
/// runtime offers it (or says nothing), else the first it offers.
pub fn plain_blend(modes: &[BlendMode]) -> BlendMode {
    if modes.is_empty() || modes.contains(&BlendMode::Opaque) {
        BlendMode::Opaque
    } else {
        modes[0]
    }
}

/// What this headset can do (see the module).
#[derive(Debug, Clone, PartialEq)]
pub struct Caps {
    /// The display rates the runtime offers; empty: the runtime chooses the rate itself.
    pub refresh_rates: Vec<f32>,
    /// The runtime takes performance levels (`XR_EXT_performance_settings`).
    pub perf_levels: bool,
    /// Curved layers (`XR_KHR_composition_layer_cylinder`).
    pub cylinder: bool,
    /// Panoramas (`XR_KHR_composition_layer_equirect2`).
    pub equirect: bool,
    /// A layer's colour scaled (`XR_KHR_composition_layer_color_scale_bias`): monitors
    /// out of focus dimmed, monitors fading in.
    pub color_scale: bool,
    /// The room behind the monitors (`None`: no way to show it): the "Room"
    /// backgrounds where it is the cameras', and what they can do more.
    pub room: Option<RoomCaps>,
    /// The compositor turns a layer's picture over
    /// (`XR_FB_composition_layer_image_layout`); without, the PC sends a stream's rows
    /// bottom first (`caps::headset::ROWS_UP`), as the compositor shows a surface's.
    pub image_flip: bool,
    /// Tracked hands (`XR_EXT_hand_tracking`).
    pub hands: bool,
    /// The hands' ray and pinch from the runtime (`XR_EXT_hand_interaction`); without,
    /// they are worked out from the joints.
    pub hand_aim: bool,
    /// The system's menu gesture of the left hand (`XR_FB_hand_tracking_aim`).
    pub menu_gesture: bool,
    /// The layers the compositor takes in a frame (`maxLayerCount`).
    pub layers_max: u8,
    /// The codecs the headset decodes, their sides and their pixel budget at
    /// [`DECODE_RATE`] (see [`pixel_budget`]): what the PC is told.
    pub decoders: Vec<Decoder>,
    /// Streams decoded at once at most: the least of the decoders', and no more than
    /// the layers left for them (see [`streams_max`]).
    pub streams_max: u8,
    /// The system's volume steps above silence; `None`: the app cannot set it.
    pub volume_steps: Option<u8>,
    /// The headset as the system names it: shown on the PC and logged, never decided
    /// by.
    pub model: String,
}

/// What the menu and the scene offer, each by a field of [`Caps`] (the step 7 note,
/// 4.4). Without it the item is hidden, or the scene does without, and the rest works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Offer {
    /// The "Room" and "Dimmed room" backgrounds (`room`, by the cameras: not on
    /// glasses, where the room always shows); without, the others.
    RoomBackground,
    /// The window onto the real keyboard (`room` with its keyboard window).
    KeyboardWindow,
    /// The left hand's menu gesture (`menu_gesture`); without, the palm, the menu
    /// button and the controller's menu button open the menu.
    MenuGesture,
    /// The hands at all (`hands`): the "Hands" choice; without, controllers only.
    Hands,
    /// The hands' ray and pinch as the runtime gives them (`hand_aim`); without, they
    /// are worked out from the joints.
    HandRay,
    /// The display rate's choice, usual or saving the battery (two rates by the rule,
    /// from `refresh_rates`); without, the runtime chooses.
    DisplayRate,
    /// Curved monitors (`cylinder`); without, flat ones, the choice hidden.
    Curve,
    /// Panoramas (`equirect`); without, hidden, a black background.
    Panorama,
    /// Monitors out of focus dimmed and new ones fading in (`color_scale`); without,
    /// "Focus" only gives them fewer frames from the PC.
    Dimming,
    /// Performance levels (`perf_levels`); without, as the runtime decides.
    PerfLevels,
    /// One volume with the PC (`volume_steps`); without, each keeps its own.
    SharedVolume,
}

impl Offer {
    pub const ALL: [Offer; 11] = [
        Offer::RoomBackground,
        Offer::KeyboardWindow,
        Offer::MenuGesture,
        Offer::Hands,
        Offer::HandRay,
        Offer::DisplayRate,
        Offer::Curve,
        Offer::Panorama,
        Offer::Dimming,
        Offer::PerfLevels,
        Offer::SharedVolume,
    ];
}

/// The usual display rate goes no higher than this: the video comes at 60 frames a
/// second, a faster display only warms the headset.
pub const USUAL_RATE_MAX: f32 = 90.0;
/// Saving the battery goes no lower than this: every frame of the video still shows,
/// and a slower display flickers in a headset.
pub const SAVER_RATE_MIN: f32 = 72.0;
/// Rates within this of a limit count as at it (a runtime may say 89.99).
const RATE_SLACK: f32 = 0.5;

/// The layers every OpenXR runtime takes (`XR_MIN_COMPOSITION_LAYERS_SUPPORTED`).
pub const LAYERS_AT_LEAST: u8 = 16;
/// The layers the app keeps for its own things, not the streams: the background, the
/// menu, its button, the hands and rays.
pub const RESERVED_LAYERS: u8 = 4;

/// The frame rate a decoder must keep at the size the PC is told: the PC sends 60.
pub const DECODE_RATE: f64 = 60.0;
/// The heights a decoder is asked at, as wide as it takes them, for its pixel budget
/// (8K, 5K, 4K, 1600p, 1440p, 1200p, 1080p, 720p).
pub const STEP_HEIGHTS: [u32; 8] = [4320, 2880, 2160, 1600, 1440, 1200, 1080, 720];
/// The sizes a decoder is asked about for its pixel budget, each also turned to
/// portrait: the monitors people use.
pub const PROBE_SIZES: [(u32, u32); 16] = [
    (1280, 720),
    (1920, 1080),
    (1920, 1200),
    (2560, 1080),
    (2560, 1440),
    (2560, 1600),
    (3440, 1440),
    (3840, 1600),
    (3840, 2160),
    (4096, 2160),
    (5120, 1440),
    (5120, 2160),
    (5120, 2880),
    (6144, 3456),
    (7680, 4320),
    (8192, 4320),
];

/// What the headset decodes when the system could not be asked: what the app took
/// for granted before it asked (H.264 up to 4096 on a side, H.265 up to 8192×4320).
pub const DECODERS_UNSAID: [Decoder; 2] = [
    Decoder {
        codec: Codec::H264,
        max_w: 4096,
        max_h: 4096,
        max_pixels: None,
    },
    Decoder {
        codec: Codec::H265,
        max_w: 8192,
        max_h: 4320,
        max_pixels: None,
    },
];
/// Streams at once when the system could not be asked (as before it was asked).
pub const STREAMS_UNSAID: u8 = 16;

impl Caps {
    /// A headset with no optional extension at all (`caps=minimal`, or a runtime that
    /// has none): what the system says about its decoders and its volume still counts.
    pub fn without_extensions(
        model: String,
        decoders: Vec<Decoder>,
        streams_max: u8,
        volume_steps: Option<u8>,
    ) -> Self {
        Self {
            refresh_rates: Vec::new(),
            perf_levels: false,
            cylinder: false,
            equirect: false,
            color_scale: false,
            room: None,
            image_flip: false,
            hands: false,
            hand_aim: false,
            menu_gesture: false,
            layers_max: LAYERS_AT_LEAST,
            decoders,
            streams_max,
            volume_steps,
            model,
        }
    }

    /// Whether the menu or the scene offers `offer` on this headset (see [`Offer`]).
    pub fn offers(&self, offer: Offer) -> bool {
        match offer {
            Offer::RoomBackground => self.room.is_some_and(|r| r.way != RoomWay::Glasses),
            Offer::KeyboardWindow => self.room.is_some_and(|r| r.keyboard_window),
            Offer::MenuGesture => self.menu_gesture,
            Offer::Hands => self.hands,
            Offer::HandRay => self.hand_aim,
            Offer::DisplayRate => self.rate_choice().is_some(),
            Offer::Curve => self.cylinder,
            Offer::Panorama => self.equirect,
            Offer::Dimming => self.color_scale,
            Offer::PerfLevels => self.perf_levels,
            Offer::SharedVolume => self.volume_steps.is_some_and(|s| s > 0),
        }
    }

    /// The display rate to ask for, saving the battery or not: the highest offered up
    /// to [`USUAL_RATE_MAX`] as usual, the lowest from [`SAVER_RATE_MIN`] to save the
    /// battery (past the limit, the nearest offered). `None`: the runtime chooses.
    pub fn rate(&self, saver: bool) -> Option<f32> {
        let rates = self.refresh_rates.iter().copied().filter(|r| *r > 0.0);
        if saver {
            let from = SAVER_RATE_MIN - RATE_SLACK;
            rates
                .clone()
                .filter(|r| *r >= from)
                .reduce(f32::min)
                .or_else(|| rates.reduce(f32::max))
        } else {
            let to = USUAL_RATE_MAX + RATE_SLACK;
            rates
                .clone()
                .filter(|r| *r <= to)
                .reduce(f32::max)
                .or_else(|| rates.reduce(f32::min))
        }
    }

    /// The usual rate and the battery saver's, when they differ (the menu offers the
    /// choice only then).
    pub fn rate_choice(&self) -> Option<(f32, f32)> {
        let (usual, saver) = (self.rate(false)?, self.rate(true)?);
        (usual - saver >= RATE_SLACK).then_some((usual, saver))
    }

    /// The decoder of `codec`, if the headset has one.
    pub fn decoder(&self, codec: Codec) -> Option<&Decoder> {
        self.decoders.iter().find(|d| d.codec == codec)
    }

    /// What the PC is told this headset can do: the displays it shows, its decoders
    /// and their pixel budgets, streams and layers at once, and its model.
    pub fn headset_caps(&self) -> HeadsetCaps {
        let told = || self.decoders.iter().filter(|d| d.max_w > 0 && d.max_h > 0);
        let codec = |d: &Decoder| prostir_proto::wire::encode(&d.codec).map_or(0, |b| b[0]);
        let mut features = vec![
            caps::headset::PHYSICAL,
            caps::headset::WINDOWS,
            caps::headset::COMMANDS,
            caps::headset::MIC,
        ];
        // The compositor shows a surface's rows bottom first and cannot turn the layer
        // over: the PC turns the picture over instead.
        if !self.image_flip {
            features.push(caps::headset::ROWS_UP);
        }
        HeadsetCaps {
            features,
            decoders: told()
                .map(|d| state::Decoder {
                    codec: codec(d),
                    max_w: d.max_w,
                    max_h: d.max_h,
                })
                .collect(),
            streams_max: self.streams_max,
            layers_max: self.layers_max,
            model: self.model.clone(),
            pixels: told()
                .filter_map(|d| {
                    let max_pixels = d.max_pixels.filter(|p| *p > 0)?;
                    Some(state::PixelBudget {
                        codec: codec(d),
                        max_pixels,
                    })
                })
                .collect(),
        }
    }

    /// Which of the fields that turn something on are off, as the selftest writes it:
    /// `full` when none is, else their names (`cylinder,room`).
    pub fn missing(&self) -> String {
        let off = [
            ("refresh_rates", self.refresh_rates.is_empty()),
            ("perf_levels", !self.perf_levels),
            ("cylinder", !self.cylinder),
            ("equirect", !self.equirect),
            ("color_scale", !self.color_scale),
            ("room", self.room.is_none()),
            ("image_flip", !self.image_flip),
            (
                "keyboard_window",
                self.room.is_none_or(|r| !r.keyboard_window),
            ),
            ("hands", !self.hands),
            ("hand_aim", !self.hand_aim),
            ("menu_gesture", !self.menu_gesture),
            ("volume_steps", self.volume_steps.is_none()),
        ];
        let names: Vec<&str> = off.iter().filter(|(_, o)| *o).map(|(n, _)| *n).collect();
        if names.is_empty() {
            "full".to_owned()
        } else {
            names.join(",")
        }
    }
}

/// Streams at once from what each decoder takes (`None`: it did not say): the least of
/// them, [`STREAMS_UNSAID`] when none said; never more than the compositor's `layers`
/// less the app's own [`RESERVED_LAYERS`] (each stream shows as a layer of its own).
pub fn streams_max(instances: impl IntoIterator<Item = Option<u32>>, layers: u8) -> u8 {
    instances
        .into_iter()
        .flatten()
        .filter(|n| *n > 0)
        .min()
        .map_or(STREAMS_UNSAID, |n| u8::try_from(n).unwrap_or(u8::MAX))
        .min(layers.saturating_sub(RESERVED_LAYERS))
        .max(1)
}

/// The volume steps from what the system says: `max` steps above silence (255 at
/// most), and whether the volume is fixed (then the app cannot set it).
pub fn volume_steps(max: Option<i32>, fixed: bool) -> Option<u8> {
    let max = max.filter(|_| !fixed)?;
    Some(max.clamp(0, i32::from(u8::MAX)) as u8).filter(|s| *s > 0)
}

/// What the system can be asked of one decoder's sizes.
pub trait Sizes {
    /// The widest picture `height` high (`None`: that height is not taken).
    fn widest_at(&mut self, height: u32) -> Option<u32>;
    /// Whether a `w`×`h` picture decodes at [`DECODE_RATE`].
    fn keeps_up(&mut self, w: u32, h: u32) -> bool;
}

/// The pixel budget of a decoder whose sides go up to `max_w` and `max_h`: the most
/// pixels of a picture that decodes at [`DECODE_RATE`], and that picture. Asked of
/// [`PROBE_SIZES`] both ways round, of the largest box, and of each of [`STEP_HEIGHTS`]
/// as wide as the decoder takes it, the largest first; `None` when none decodes in
/// time (then the codec falls back to what was taken for granted,
/// [`decoder_or_unsaid`], never "any size").
pub fn pixel_budget(max_w: u32, max_h: u32, sizes: &mut impl Sizes) -> Option<(u32, (u32, u32))> {
    if max_w == 0 || max_h == 0 {
        return None;
    }
    let mut asked: Vec<(u32, u32)> = PROBE_SIZES
        .into_iter()
        .flat_map(|(w, h)| [(w, h), (h, w)])
        .chain([(max_w, max_h)])
        .collect();
    for h in STEP_HEIGHTS.into_iter().filter(|h| *h <= max_h) {
        if let Some(w) = sizes.widest_at(h) {
            asked.push((w.min(max_w), h));
        }
    }
    asked.retain(|&(w, h)| w > 0 && h > 0 && w <= max_w && h <= max_h);
    let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
    asked.sort_by_key(|s| std::cmp::Reverse(area(*s)));
    asked.dedup();
    let (w, h) = asked.into_iter().find(|&(w, h)| sizes.keeps_up(w, h))?;
    Some((u32::try_from(area((w, h))).unwrap_or(u32::MAX), (w, h)))
}

/// `d` as told: itself; for a decoder with sides but no budget, what was taken for
/// granted for its codec within those sides; for one of no size, what was taken for
/// granted (`None` for a codec without that).
pub fn decoder_or_unsaid(d: Decoder) -> Option<Decoder> {
    let sized = d.max_w > 0 && d.max_h > 0;
    if sized && d.max_pixels.is_some() {
        return Some(d);
    }
    let unsaid = DECODERS_UNSAID.into_iter().find(|u| u.codec == d.codec)?;
    Some(if sized {
        Decoder {
            max_w: d.max_w.min(unsaid.max_w),
            max_h: d.max_h.min(unsaid.max_h),
            ..unsaid
        }
    } else {
        unsaid
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything on.
    fn full() -> Caps {
        Caps {
            refresh_rates: vec![72.0, 80.0, 90.0, 120.0],
            perf_levels: true,
            cylinder: true,
            equirect: true,
            color_scale: true,
            room: Some(RoomCaps {
                way: RoomWay::FbPassthrough,
                keyboard_window: true,
            }),
            image_flip: true,
            hands: true,
            hand_aim: true,
            menu_gesture: true,
            layers_max: 16,
            decoders: DECODERS_UNSAID.to_vec(),
            streams_max: 12,
            volume_steps: Some(15),
            model: "Test headset".into(),
        }
    }

    fn offered(c: &Caps) -> Vec<Offer> {
        Offer::ALL.into_iter().filter(|o| c.offers(*o)).collect()
    }

    /// Everything offered with every field on.
    #[test]
    fn everything_with_everything() {
        assert_eq!(offered(&full()), Offer::ALL);
        assert_eq!(full().missing(), "full");
    }

    /// Each field off hides what depends on it, and only that (the table of 4.4).
    #[test]
    fn each_field_hides_its_own() {
        let without = |f: fn(&mut Caps), gone: &[Offer]| {
            let mut c = full();
            f(&mut c);
            let left = offered(&c);
            let expected: Vec<Offer> = Offer::ALL
                .into_iter()
                .filter(|o| !gone.contains(o))
                .collect();
            assert_eq!(left, expected, "{c:?}");
        };
        without(|c| c.refresh_rates.clear(), &[Offer::DisplayRate]);
        without(|c| c.perf_levels = false, &[Offer::PerfLevels]);
        without(|c| c.cylinder = false, &[Offer::Curve]);
        without(|c| c.equirect = false, &[Offer::Panorama]);
        without(|c| c.color_scale = false, &[Offer::Dimming]);
        without(
            |c| c.room = None,
            &[Offer::RoomBackground, Offer::KeyboardWindow],
        );
        without(
            |c| {
                c.room = Some(RoomCaps {
                    way: RoomWay::FbPassthrough,
                    keyboard_window: false,
                });
            },
            &[Offer::KeyboardWindow],
        );
        // Glasses: the room always shows, nothing to choose.
        without(
            |c| {
                c.room = Some(RoomCaps {
                    way: RoomWay::Glasses,
                    keyboard_window: false,
                });
            },
            &[Offer::RoomBackground, Offer::KeyboardWindow],
        );
        // The picture turned over by the PC instead: nothing hidden.
        without(|c| c.image_flip = false, &[]);
        without(|c| c.hands = false, &[Offer::Hands]);
        without(|c| c.hand_aim = false, &[Offer::HandRay]);
        without(|c| c.menu_gesture = false, &[Offer::MenuGesture]);
        without(|c| c.volume_steps = None, &[Offer::SharedVolume]);
        without(|c| c.volume_steps = Some(0), &[Offer::SharedVolume]);
        // One rate offered: nothing to choose.
        without(|c| c.refresh_rates = vec![90.0], &[Offer::DisplayRate]);
    }

    /// Without any extension nothing optional is offered, and what the system said
    /// stays.
    #[test]
    fn nothing_without_extensions() {
        let c = Caps::without_extensions("Test".into(), vec![DECODERS_UNSAID[0]], 4, Some(15));
        assert_eq!(offered(&c), [Offer::SharedVolume]);
        assert_eq!(
            c.missing(),
            "refresh_rates,perf_levels,cylinder,equirect,color_scale,room,image_flip,\
             keyboard_window,hands,hand_aim,menu_gesture"
        );
        assert_eq!(c.rate(false), None);
        assert_eq!(c.rate(true), None);
        let told = c.headset_caps();
        assert_eq!(told.decoders.len(), 1);
        assert_eq!(told.streams_max, 4);
        assert_eq!(told.layers_max, LAYERS_AT_LEAST);
        assert_eq!(told.model, "Test");
    }

    /// The rate by the rule: the highest up to 90 as usual, the lowest from 72 to save
    /// the battery; past a limit the nearest offered.
    #[test]
    fn the_rate_by_the_rule() {
        let rates = |r: &[f32]| Caps {
            refresh_rates: r.to_vec(),
            ..full()
        };
        let c = rates(&[72.0, 80.0, 90.0, 120.0]);
        assert_eq!(c.rate(false), Some(90.0));
        assert_eq!(c.rate(true), Some(72.0));
        assert_eq!(c.rate_choice(), Some((90.0, 72.0)));
        // 60 Hz flickers in a headset: never chosen while 72 or more is offered.
        let c = rates(&[60.0, 72.0, 90.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(90.0), Some(72.0)));
        // As the runtime writes them.
        let c = rates(&[71.94, 89.99]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(89.99), Some(71.94)));
        let c = rates(&[60.0, 90.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(90.0), Some(90.0)));
        assert_eq!(c.rate_choice(), None);
        // Only faster rates: the slowest of them; only slower ones: the fastest.
        let c = rates(&[100.0, 120.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(100.0), Some(100.0)));
        assert_eq!(c.rate_choice(), None);
        let c = rates(&[30.0, 60.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(60.0), Some(60.0)));
        // Nothing sensible offered.
        assert_eq!(rates(&[0.0]).rate(false), None);
        assert_eq!(rates(&[]).rate_choice(), None);
    }

    /// The PC hears the decoders, streams and layers this headset has, not fixed ones.
    #[test]
    fn the_pc_hears_what_the_headset_has() {
        let mut c = full();
        c.decoders = vec![
            Decoder {
                codec: Codec::H264,
                max_w: 1920,
                max_h: 1088,
                max_pixels: None,
            },
            // A decoder of no size is not told (the PC would refuse the caps).
            Decoder {
                codec: Codec::H265,
                max_w: 0,
                max_h: 0,
                max_pixels: Some(1000),
            },
        ];
        c.streams_max = 3;
        c.layers_max = 32;
        let told = c.headset_caps();
        assert!(told.check().is_ok());
        assert_eq!(told.decoders.len(), 1);
        assert!(told.pixels.is_empty(), "no budget said, none told");
        assert!(told.fits(Codec::H264, 1920, 1080));
        assert!(!told.fits(Codec::H264, 2560, 1440));
        assert!(!told.fits(Codec::H265, 1920, 1080));
        assert_eq!((told.streams_max, told.layers_max), (3, 32));
        assert!(told.has(caps::headset::MIC));
        // The compositor turns the picture over: the rows go top first.
        assert!(!told.has(caps::headset::ROWS_UP));
        c.image_flip = false;
        assert!(c.headset_caps().has(caps::headset::ROWS_UP));
    }

    /// The way the room is shown, from what the runtime offers (the families of
    /// headsets there are, as their runtimes say it), by what it says, not by name.
    #[test]
    fn the_room_from_what_the_runtime_offers() {
        use BlendMode::{Additive, AlphaBlend, Opaque};
        let ways = |modes: &[BlendMode], fb_passthrough, htc_passthrough| {
            room_ways(&RoomFound {
                blend_modes: modes,
                fb_passthrough,
                htc_passthrough,
            })
        };
        // Opaque only, and the cameras' layer of `XR_FB_passthrough`.
        assert_eq!(ways(&[Opaque], true, false), [RoomWay::FbPassthrough]);
        // The same without the extension (`caps=minimal`): no room.
        assert_eq!(ways(&[Opaque], false, false), []);
        // Blended and the layer both: the layer first, the blend if it cannot be made.
        assert_eq!(
            ways(&[Opaque, AlphaBlend], true, false),
            [RoomWay::FbPassthrough, RoomWay::Blend]
        );
        assert_eq!(
            ways(&[AlphaBlend, Opaque], false, true),
            [RoomWay::HtcPassthrough, RoomWay::Blend]
        );
        // Blended only.
        assert_eq!(ways(&[Opaque, AlphaBlend], false, false), [RoomWay::Blend]);
        assert_eq!(ways(&[AlphaBlend, Opaque], false, false), [RoomWay::Blend]);
        // The layer of `XR_HTC_passthrough`.
        assert_eq!(ways(&[Opaque], false, true), [RoomWay::HtcPassthrough]);
        // Both layers: the first that can be made, `XR_FB_passthrough` first.
        assert_eq!(
            ways(&[Opaque], true, true),
            [RoomWay::FbPassthrough, RoomWay::HtcPassthrough]
        );
        // Glasses: additive, alone or preferred; never a layer over them.
        assert_eq!(ways(&[Additive], true, true), [RoomWay::Glasses]);
        assert_eq!(ways(&[Additive, Opaque], false, false), [RoomWay::Glasses]);
        assert_eq!(
            ways(&[Additive, AlphaBlend], false, false),
            [RoomWay::Glasses]
        );
        // Additive offered but opaque preferred: not glasses.
        assert_eq!(
            ways(&[Opaque, Additive], true, false),
            [RoomWay::FbPassthrough]
        );
        // A runtime that says nothing.
        assert_eq!(ways(&[], false, false), []);
        // The frame without the room: opaque where offered, else the first offered.
        assert_eq!(plain_blend(&[Opaque, AlphaBlend]), Opaque);
        assert_eq!(plain_blend(&[AlphaBlend, Opaque]), Opaque);
        assert_eq!(plain_blend(&[AlphaBlend]), AlphaBlend);
        assert_eq!(plain_blend(&[Additive]), Additive);
        assert_eq!(plain_blend(&[]), Opaque);
    }

    #[test]
    fn streams_and_volume_from_what_the_system_says() {
        assert_eq!(streams_max([Some(16), Some(6)], 16), 6);
        assert_eq!(streams_max([None, Some(0), Some(10)], 16), 10);
        // No more than the layers the app's own things leave.
        assert_eq!(streams_max([None, Some(32)], 16), 16 - RESERVED_LAYERS);
        assert_eq!(streams_max([Some(1000)], 64), 64 - RESERVED_LAYERS);
        assert_eq!(streams_max([None], 64), STREAMS_UNSAID);
        assert_eq!(streams_max([], 8), 8 - RESERVED_LAYERS);
        assert_eq!(streams_max([], 2), 1);
        assert_eq!(volume_steps(Some(15), false), Some(15));
        assert_eq!(volume_steps(Some(15), true), None);
        assert_eq!(volume_steps(Some(0), false), None);
        assert_eq!(volume_steps(None, false), None);
        assert_eq!(volume_steps(Some(300), false), Some(u8::MAX));
        assert_eq!(volume_steps(Some(-1), false), None);
    }

    /// A decoder as a system would answer: pictures up to `area` pixels decode at 60
    /// frames a second, and at each height it is as wide as `wide`.
    struct System {
        area: u32,
        wide: u32,
        asked: Vec<(u32, u32)>,
    }

    impl Sizes for System {
        fn widest_at(&mut self, _height: u32) -> Option<u32> {
            Some(self.wide)
        }
        fn keeps_up(&mut self, w: u32, h: u32) -> bool {
            self.asked.push((w, h));
            w <= self.wide && w * h <= self.area
        }
    }

    /// The decoder as told for a system answering like `s`, sides `max_w` by `max_h`.
    fn told(codec: Codec, max_w: u32, max_h: u32, s: &mut System) -> Decoder {
        let budget = pixel_budget(max_w, max_h, s);
        decoder_or_unsaid(Decoder {
            codec,
            max_w,
            max_h,
            max_pixels: budget.map(|b| b.0),
        })
        .unwrap()
    }

    /// A headset as one tried says it: H.265 takes 8192x4320, but at 60 frames a
    /// second only as many pixels as 8192x1600, H.264 as 8192x2880. The PC hears the
    /// sides and the budget, so 4K, portrait 1440p and 32:9 all fit in both codecs; 8K
    /// does not.
    #[test]
    fn short_of_8k_still_takes_4k_and_portrait() {
        let mut hevc = System {
            area: 8192 * 1600,
            wide: 8192,
            asked: Vec::new(),
        };
        let mut avc = System {
            area: 8192 * 2880,
            wide: 8192,
            asked: Vec::new(),
        };
        let mut c = full();
        c.decoders = vec![
            told(Codec::H265, 8192, 4320, &mut hevc),
            told(Codec::H264, 8192, 4320, &mut avc),
        ];
        assert_eq!(c.decoders[0].max_pixels, Some(8192 * 1600));
        assert_eq!(c.decoders[1].max_pixels, Some(8192 * 2880));
        let pc = c.headset_caps();
        assert!(pc.check().is_ok());
        assert_eq!(pc.pixels.len(), 2);
        for codec in [Codec::H265, Codec::H264] {
            for (w, h) in [
                (3840, 2160),
                (1440, 2560),
                (2560, 1440),
                (5120, 1440),
                (2160, 3840),
                (1920, 1080),
            ] {
                assert!(pc.fits(codec, w, h), "{codec:?} {w}x{h}");
            }
            assert!(!pc.fits(codec, 7680, 4320), "{codec:?} 8K");
            assert!(!pc.fits(codec, 8193, 1000), "{codec:?} wider than it takes");
        }
        // The largest first: one question when the largest box keeps up.
        let mut big = System {
            area: u32::MAX,
            wide: 8192,
            asked: Vec::new(),
        };
        assert_eq!(
            pixel_budget(8192, 4320, &mut big),
            Some((8192 * 4320, (8192, 4320)))
        );
        assert_eq!(big.asked, [(8192, 4320)]);
    }

    /// A decoder that cannot do 4K at 60 frames a second: 4K is not told as fitting,
    /// 1440p still is; one that keeps up at nothing, or says no sizes, falls back to
    /// what was taken for granted.
    #[test]
    fn a_weaker_decoder_leaves_4k_out() {
        let mut s = System {
            area: 2560 * 1600,
            wide: 4096,
            asked: Vec::new(),
        };
        let d = told(Codec::H264, 4096, 4096, &mut s);
        assert_eq!(d.max_pixels, Some(2560 * 1600));
        let mut c = full();
        c.decoders = vec![d];
        let pc = c.headset_caps();
        assert!(!pc.fits(Codec::H264, 3840, 2160));
        assert!(pc.fits(Codec::H264, 2560, 1440));
        assert!(pc.fits(Codec::H264, 1440, 2560));
        assert!(pc.fits(Codec::H264, 2560, 1600));
        let mut none = System {
            area: 100,
            wide: 4096,
            asked: Vec::new(),
        };
        assert_eq!(pixel_budget(4096, 4096, &mut none), None);
        assert_eq!(pixel_budget(0, 0, &mut none), None);
        // Its sides still hold where they are smaller than what is taken for granted.
        let smaller = Decoder {
            max_w: 4096,
            max_h: 4096,
            ..DECODERS_UNSAID[1]
        };
        assert_eq!(told(Codec::H265, 4096, 4096, &mut none), smaller);
        assert_eq!(told(Codec::H264, 8192, 4320, &mut none), DECODERS_UNSAID[0]);
        let no_size = Decoder {
            codec: Codec::H265,
            max_w: 0,
            max_h: 0,
            max_pixels: None,
        };
        assert_eq!(decoder_or_unsaid(no_size), Some(DECODERS_UNSAID[1]));
        let av1 = Decoder {
            codec: Codec::Av1,
            max_w: 0,
            max_h: 0,
            max_pixels: None,
        };
        assert_eq!(decoder_or_unsaid(av1), None);
    }
}
