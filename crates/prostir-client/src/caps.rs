//! What this headset can do ([`Caps`]), as the shell found it at the start: from the
//! extensions the OpenXR runtime gave and from what the system says (its decoders, its
//! volume). The core decides by these fields only, never by the headset's name or
//! maker, so a headset nobody tried yet gets what it offers and hides the rest.
//!
//! What the menu and the scene offer by which field is [`Offer`] and
//! [`Caps::offers`] (the table of the step 7 note, 4.4); the display rate is chosen by
//! a rule from the rates the runtime offers ([`Caps::rate`]), not from a list of
//! headsets.

#![forbid(unsafe_code)]

use prostir_proto::Codec;
use prostir_proto::state::{self, HeadsetCaps, caps};

/// A codec the headset decodes, and the largest picture it takes (each side's upper
/// bound as the system says it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoder {
    pub codec: Codec,
    pub max_w: u32,
    pub max_h: u32,
}

/// The cameras.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomCaps {
    /// A window onto the real keyboard (the cameras' picture on a rectangle where the
    /// keyboard is).
    pub keyboard_window: bool,
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
    /// The cameras: the "Room" backgrounds, and what they can do more.
    pub room: Option<RoomCaps>,
    /// Tracked hands (`XR_EXT_hand_tracking`).
    pub hands: bool,
    /// The hands' ray and pinch from the runtime (`XR_EXT_hand_interaction`, or
    /// `XR_FB_hand_tracking_aim`); without, they are worked out from the joints.
    pub hand_aim: bool,
    /// The system's menu gesture of the left hand (`XR_FB_hand_tracking_aim`).
    pub menu_gesture: bool,
    /// The layers the compositor takes in a frame (`maxLayerCount`).
    pub layers_max: u8,
    /// The codecs the headset decodes, as the system lists them.
    pub decoders: Vec<Decoder>,
    /// Streams decoded at once at most: the least of the decoders', and no more than
    /// the layers (see [`streams_max`]).
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
    /// The "Room" and "Dimmed room" backgrounds (`room`); without, the others.
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
/// Saving the battery goes no lower than this: every frame of the video still shows.
pub const SAVER_RATE_MIN: f32 = 60.0;
/// Rates within this of a limit count as at it (a runtime may say 89.99).
const RATE_SLACK: f32 = 0.5;

/// The layers every OpenXR runtime takes (`XR_MIN_COMPOSITION_LAYERS_SUPPORTED`).
pub const LAYERS_AT_LEAST: u8 = 16;

/// What the headset decodes when the system could not be asked: what the app took
/// for granted before it asked (H.264 up to 4096 on a side, H.265 up to 8192×4320).
pub const DECODERS_UNSAID: [Decoder; 2] = [
    Decoder {
        codec: Codec::H264,
        max_w: 4096,
        max_h: 4096,
    },
    Decoder {
        codec: Codec::H265,
        max_w: 8192,
        max_h: 4320,
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
            Offer::RoomBackground => self.room.is_some(),
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

    /// What the PC is told this headset can do: the displays it shows, its decoders,
    /// streams and layers at once, and its model.
    pub fn headset_caps(&self) -> HeadsetCaps {
        HeadsetCaps {
            features: vec![
                caps::headset::PHYSICAL,
                caps::headset::WINDOWS,
                caps::headset::COMMANDS,
                caps::headset::MIC,
            ],
            decoders: self
                .decoders
                .iter()
                .filter(|d| d.max_w > 0 && d.max_h > 0)
                .map(|d| state::Decoder {
                    codec: prostir_proto::wire::encode(&d.codec).map_or(0, |b| b[0]),
                    max_w: d.max_w,
                    max_h: d.max_h,
                })
                .collect(),
            streams_max: self.streams_max,
            layers_max: self.layers_max,
            model: self.model.clone(),
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
/// (each stream shows as a layer of its own).
pub fn streams_max(instances: impl IntoIterator<Item = Option<u32>>, layers: u8) -> u8 {
    instances
        .into_iter()
        .flatten()
        .filter(|n| *n > 0)
        .min()
        .map_or(STREAMS_UNSAID, |n| u8::try_from(n).unwrap_or(u8::MAX))
        .min(layers)
        .max(1)
}

/// The volume steps from what the system says: `max` steps above silence, and whether
/// the volume is fixed (then the app cannot set it).
pub fn volume_steps(max: Option<i32>, fixed: bool) -> Option<u8> {
    let max = max.filter(|_| !fixed)?;
    u8::try_from(max).ok().filter(|s| *s > 0)
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
                keyboard_window: true,
            }),
            hands: true,
            hand_aim: true,
            menu_gesture: true,
            layers_max: 16,
            decoders: DECODERS_UNSAID.to_vec(),
            streams_max: 16,
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
                    keyboard_window: false,
                });
            },
            &[Offer::KeyboardWindow],
        );
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
            "refresh_rates,perf_levels,cylinder,equirect,color_scale,room,keyboard_window,\
             hands,hand_aim,menu_gesture"
        );
        assert_eq!(c.rate(false), None);
        assert_eq!(c.rate(true), None);
        let told = c.headset_caps();
        assert_eq!(told.decoders.len(), 1);
        assert_eq!(told.streams_max, 4);
        assert_eq!(told.layers_max, LAYERS_AT_LEAST);
        assert_eq!(told.model, "Test");
    }

    /// The rate by the rule: the highest up to 90 as usual, the lowest from 60 to save
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
        let c = rates(&[60.0, 72.0, 90.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(90.0), Some(60.0)));
        // As the runtime writes them.
        let c = rates(&[59.94, 89.99]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(89.99), Some(59.94)));
        // Only faster rates: the slowest of them; only slower ones: the fastest.
        let c = rates(&[100.0, 120.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(100.0), Some(100.0)));
        assert_eq!(c.rate_choice(), None);
        let c = rates(&[30.0, 45.0]);
        assert_eq!((c.rate(false), c.rate(true)), (Some(45.0), Some(45.0)));
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
            },
            // A decoder of no size is not told (the PC would refuse the caps).
            Decoder {
                codec: Codec::H265,
                max_w: 0,
                max_h: 0,
            },
        ];
        c.streams_max = 3;
        c.layers_max = 32;
        let told = c.headset_caps();
        assert!(told.check().is_ok());
        assert_eq!(told.decoders.len(), 1);
        assert!(told.fits(Codec::H264, 1920, 1080));
        assert!(!told.fits(Codec::H264, 2560, 1440));
        assert!(!told.fits(Codec::H265, 1920, 1080));
        assert_eq!((told.streams_max, told.layers_max), (3, 32));
        assert!(told.has(caps::headset::MIC));
    }

    #[test]
    fn streams_and_volume_from_what_the_system_says() {
        assert_eq!(streams_max([Some(16), Some(6)], 16), 6);
        assert_eq!(streams_max([None, Some(0), Some(12)], 16), 12);
        // No more than the layers.
        assert_eq!(streams_max([None, Some(32)], 16), 16);
        assert_eq!(streams_max([Some(1000)], 64), 64);
        assert_eq!(streams_max([Some(1000)], u8::MAX), u8::MAX);
        assert_eq!(streams_max([None], 64), STREAMS_UNSAID);
        assert_eq!(streams_max([], 8), 8);
        assert_eq!(streams_max([], 0), 1);
        assert_eq!(volume_steps(Some(15), false), Some(15));
        assert_eq!(volume_steps(Some(15), true), None);
        assert_eq!(volume_steps(Some(0), false), None);
        assert_eq!(volume_steps(None, false), None);
        assert_eq!(volume_steps(Some(300), false), None);
    }
}
