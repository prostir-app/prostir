//! What a headset is told about the PC: the PC's state built from what owns each part
//! (the screens, the store, the desktop), compared with what went to this session, so
//! a part goes out whole and only when it changed. Values of enums go out only when the
//! headset named the capability they need (see `PROTOCOL.md`, "Capabilities").

use std::collections::BTreeMap;

use prostir_proto::state::{
    self, Access, DisplayId, Focus, HeadsetCaps, HeadsetState, HostCaps, Limits, Mode, PcInfo,
    PcState, Prefs, SoundFrom, UpKind, UpWanted, caps, clip,
};
use prostir_proto::wire::WireError;
use prostir_proto::{Codec, HostMsg};

use crate::displays::{Displays, MONITORS_MAX, MONITORS_NO_LIMITS, STREAMS_MAX, WINDOWS_MAX};
use crate::screen::ScreenId;
use crate::store::Settings;

/// Monitor sizes the PC offers: the common ones, a 32:9 one where the headset decodes
/// it, and a portrait one.
pub const SIZES: [(u32, u32); 6] = [
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (3440, 1440),
    (5120, 1440),
    (1440, 2560),
];
/// Scales offered, in percent ("larger text"; the steps "Auto" chooses from).
pub const SCALES: [u16; 5] = [100, 125, 150, 175, 200];
/// The smallest and the largest monitor the PC makes, in pixels.
pub const MIN_SIZE: (u32, u32) = (320, 240);
pub const MAX_SIZE: (u32, u32) = (7680, 4320);
/// Displays of the PC itself shown at once, and with "No limits": as many as a PC
/// usually has, so each connected one can be shown. The streams the headset takes at
/// once bound them together with the monitors (`streams_max`); displays not
/// connected count for none.
pub(crate) const PHYSICAL_MAX: u8 = 4;
const PHYSICAL_NO_LIMITS: u8 = 8;

/// What the parts are built from at one moment.
pub struct Sources<'a> {
    pub displays: &'a Displays,
    pub wanted: &'a Settings,
    /// The screen the mouse is on.
    pub focus: Option<ScreenId>,
    /// The screen the sound comes from, while the headset asks for it.
    pub sound_from: Option<ScreenId>,
    /// The PC can share the volume with the headset (its sound device follows).
    pub volume: bool,
    /// This headset's microphone is the PC's end of the headset microphone (the
    /// microphone goes from one headset only).
    pub mic: bool,
    /// "No limits" of the main headset, which the PC's limits follow.
    pub no_limits: bool,
}

/// One session's view of the PC's state: the headset's own state, and what it was
/// told so far.
pub struct Publisher {
    pub headset: HeadsetState,
    /// What the headset can do, from its `Open` (which always says, and it never
    /// changes in a session).
    caps: HeadsetCaps,
    /// This headset's role and rights (see `settings.txt`).
    pub access: Access,
    sent: PcState,
}

impl Publisher {
    pub fn new(headset: HeadsetState, access: Access) -> Self {
        Self {
            caps: headset.caps.clone().unwrap_or_default(),
            headset,
            access,
            sent: PcState::default(),
        }
    }

    /// What the headset can do (`Open` always says).
    pub fn caps(&self) -> &HeadsetCaps {
        &self.caps
    }

    pub fn prefs(&self) -> Prefs {
        self.headset.prefs.unwrap_or_default()
    }

    /// What the PC allows this headset now. "No limits" of a guest is for its own
    /// budgets: the PC's limits follow the main headset's (`no_limits`). Only scales
    /// the desktop gives an output of every size offered are offered, so any pair the
    /// headset picks can be made.
    pub fn limits(&self, displays: &Displays, no_limits: bool) -> Limits {
        let prefs = Prefs {
            no_limits,
            ..self.prefs()
        };
        let mut limits = limits(prefs, self.caps(), displays.codec());
        keep_scales_of_every_size(&mut limits, |w, h| displays.scales_for(w, h));
        limits
    }

    /// The PC's state now, as this headset is told it.
    pub fn state(&self, src: &Sources<'_>) -> PcState {
        let headset = self.caps();
        let prefs = self.prefs();
        let displays = src.displays.displays_part(headset);
        let listed = |id: Option<ScreenId>| {
            id.map(ScreenId::wire)
                .filter(|id| displays.get(*id).is_some())
        };
        let focus = Focus {
            display: listed(src.focus),
        };
        let sound_from = prefs.sound_from_screen.then(|| SoundFrom {
            display: listed(src.sound_from),
        });
        // The microphone as an up stream, where both sides do it: wanted of the one
        // headset whose microphone the PC takes.
        let mut up_wanted = BTreeMap::new();
        if headset.has(caps::headset::MIC) {
            let on = src.mic && src.displays.recording();
            up_wanted.insert(
                UpKind::Mic,
                UpWanted {
                    kind: UpKind::Mic,
                    on,
                },
            );
        }
        // Monitors in Auto, where the desktop changes scales in place.
        let scale_auto = src.displays.scales_in_place().then(|| {
            let mut part = src.displays.scale_auto_part();
            part.auto.retain(|id| displays.get(*id).is_some());
            part
        });
        PcState {
            scale_auto,
            info: Some(pc_info(src.displays)),
            caps: Some(host_caps(src.displays, src.volume)),
            access: Some(self.access),
            limits: Some(self.limits(src.displays, src.no_limits)),
            encoding: Some(src.displays.encoding_part(src.wanted, headset)),
            sound: Some(src.displays.sound_part(src.mic)),
            up_wanted,
            focus: Some(focus),
            sound_from,
            displays: Some(displays),
        }
    }

    /// The `Snapshot` that opens the session: everything there is.
    pub fn snapshot(&mut self, src: &Sources<'_>) -> Result<HostMsg, WireError> {
        let state = self.state(src);
        let msg = state.snapshot()?;
        self.sent = state;
        Ok(msg)
    }

    /// The parts that changed since they were last sent, `Displays` first.
    pub fn changed(&mut self, src: &Sources<'_>) -> Vec<HostMsg> {
        let state = self.state(src);
        let changed = state.changed(&self.sent);
        self.sent = state;
        changed
    }

    /// The displays the headset was told about that may stream to it: not those
    /// failed (for it alone, larger than it decodes or beyond the streams it takes,
    /// or for all).
    pub fn streamable(&self) -> impl Iterator<Item = DisplayId> + '_ {
        self.sent
            .displays
            .iter()
            .flat_map(|d| &d.displays)
            .filter(|d| !matches!(d.state, state::DisplayState::Failed { .. }))
            .map(|d| d.id)
    }
}

/// What the PC allows this headset: its own hard limits, more with "No limits", and
/// sizes only as large as the headset decodes in the codec the PC sends (its sides and
/// its pixel budget, see [`HeadsetCaps::fits`]).
pub fn limits(prefs: Prefs, headset: &HeadsetCaps, codec: Codec) -> Limits {
    let decoder = headset.decoder(codec);
    let fits = |(w, h): (u32, u32)| decoder.is_none() || headset.fits(codec, w, h);
    let max = decoder.map_or(MAX_SIZE, |d| {
        (MAX_SIZE.0.min(d.max_w), MAX_SIZE.1.min(d.max_h))
    });
    let streams = u8::try_from(crate::displays::streams_max(headset)).unwrap_or(STREAMS_MAX);
    Limits {
        virtual_max: if prefs.no_limits {
            MONITORS_NO_LIMITS as u8
        } else {
            MONITORS_MAX as u8
        },
        windows_max: WINDOWS_MAX as u8,
        physical_max: if prefs.no_limits {
            PHYSICAL_NO_LIMITS
        } else {
            PHYSICAL_MAX
        },
        streams_max: streams,
        min: MIN_SIZE,
        max,
        sizes: SIZES
            .into_iter()
            .filter(|s| fits(*s))
            .map(|(w, h)| Mode::new(w, h))
            .collect(),
        scales: SCALES.to_vec(),
    }
}

/// Keeps in `limits` only the scales `scales_for` gives every size offered.
fn keep_scales_of_every_size(
    limits: &mut Limits,
    scales_for: impl Fn(u32, u32) -> crate::platform::Scales,
) {
    let sizes = &limits.sizes;
    limits
        .scales
        .retain(|s| sizes.iter().all(|m| scales_for(m.w, m.h).allows(*s)));
}

/// What this PC can do.
fn host_caps(displays: &Displays, volume: bool) -> HostCaps {
    let mut features = vec![
        caps::pc::MIC,
        caps::pc::WINDOWS,
        caps::pc::PHYSICAL,
        caps::pc::NOTIFICATIONS,
        caps::pc::SOUND_FROM,
        caps::pc::NO_LIMITS,
        caps::pc::SCALE,
        caps::pc::COMMANDS,
        // Takes the headset's `Gaze` (for now it only writes it in its log).
        caps::pc::GAZE,
    ];
    if displays.moves_mouse() {
        features.push(caps::pc::MOVE_MOUSE);
    }
    if displays.scales_in_place() {
        features.push(caps::pc::SCALE_IN_PLACE);
    }
    if displays.screenshots_one_output() {
        features.push(caps::pc::SCREENSHOT_ONE);
    }
    if volume {
        features.push(caps::pc::VOLUME);
    }
    features.sort();
    HostCaps { features }
}

/// About this PC.
fn pc_info(displays: &Displays) -> PcInfo {
    PcInfo {
        name: clip(&crate::platform::hostname(), state::MAX_DISPLAY_NAME),
        app: clip(
            &prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
            state::MAX_SHORT,
        ),
        language: crate::i18n::code().into(),
        os: state::os::LINUX,
        desktop: displays.desktop_name().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Scales;
    use prostir_proto::state::Decoder;

    /// Scales that depend on the size (Windows, GNOME): only those every size offered
    /// takes are offered.
    #[test]
    fn scales_offered_fit_every_size() {
        let mut l = limits(Prefs::default(), &HeadsetCaps::default(), Codec::H264);
        assert_eq!(l.scales, [100, 125, 150, 175, 200]);
        keep_scales_of_every_size(&mut l, |_, _| Scales::Any { min: 50, max: 400 });
        assert_eq!(l.scales, [100, 125, 150, 175, 200]);
        // Hyprland: whole logical pixels for every size, so no 150 % (2560x1440).
        let mut hyprland = l.clone();
        keep_scales_of_every_size(&mut hyprland, |w, h| {
            let whole = |px: u32, p: u16| (px * 100) % u32::from(p) == 0;
            Scales::Steps(
                [100, 125, 150, 175, 200]
                    .into_iter()
                    .filter(|p| whole(w, *p) && whole(h, *p))
                    .collect(),
            )
        });
        assert_eq!(hyprland.scales, [100, 125, 200]);
        keep_scales_of_every_size(&mut l, |w, _| {
            Scales::Steps(if w < 2000 {
                vec![100, 125]
            } else {
                vec![100, 125, 150]
            })
        });
        assert_eq!(l.scales, [100, 125]);
    }

    fn headset(decoders: Vec<Decoder>) -> HeadsetCaps {
        HeadsetCaps {
            decoders,
            streams_max: 12,
            ..HeadsetCaps::default()
        }
    }

    /// "No limits" raises the counts; sizes stay within what the headset decodes.
    #[test]
    fn limits_follow_no_limits_and_the_decoders() {
        let h264 = Decoder {
            codec: 0,
            max_w: 4096,
            max_h: 4096,
        };
        let h265 = Decoder {
            codec: 1,
            max_w: 8192,
            max_h: 4320,
        };
        let caps = headset(vec![h264, h265]);
        let safe = limits(Prefs::default(), &caps, Codec::H264);
        assert_eq!((safe.virtual_max, safe.physical_max), (6, 4));
        assert_eq!(safe.streams_max, 12);
        assert_eq!(safe.max, (4096, 4096));
        assert!(
            !safe.sizes.contains(&Mode::new(5120, 1440)),
            "too wide for H.264"
        );
        let free = Prefs {
            no_limits: true,
            ..Prefs::default()
        };
        let wide = limits(free, &caps, Codec::H265);
        assert_eq!((wide.virtual_max, wide.physical_max), (10, 8));
        assert!(wide.sizes.contains(&Mode::new(5120, 1440)));
        assert_eq!(wide.max, (7680, 4320));
        assert!(wide.check().is_ok());
    }

    /// Quest 3 says its sides (8192 by 4320) and how many pixels decode at 60 frames a
    /// second (8192x1600 worth for H.265, 8192x2880 for H.264): every size offered
    /// fits, 4K and portrait among them; a headset that cannot do 4K at 60 is not
    /// offered it.
    #[test]
    fn sizes_follow_the_pixel_budget() {
        let decoder = |codec| Decoder {
            codec,
            max_w: 8192,
            max_h: 4320,
        };
        let budget = |codec, max_pixels| state::PixelBudget { codec, max_pixels };
        let quest = HeadsetCaps {
            pixels: vec![budget(0, 8192 * 2880), budget(1, 8192 * 1600)],
            ..headset(vec![decoder(0), decoder(1)])
        };
        for codec in [Codec::H264, Codec::H265] {
            let l = limits(Prefs::default(), &quest, codec);
            for (w, h) in SIZES {
                assert!(l.sizes.contains(&Mode::new(w, h)), "{codec:?} {w}x{h}");
            }
        }
        let weak = HeadsetCaps {
            pixels: vec![budget(0, 2560 * 1600)],
            ..headset(vec![decoder(0)])
        };
        let l = limits(Prefs::default(), &weak, Codec::H264);
        assert!(!l.sizes.contains(&Mode::new(3840, 2160)));
        assert!(l.sizes.contains(&Mode::new(2560, 1440)));
        assert!(l.sizes.contains(&Mode::new(1440, 2560)));
    }
}
