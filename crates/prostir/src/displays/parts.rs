//! What the headsets are told: the displays, encoding and sound parts of the state, and
//! what the desktop can do.

use super::*;

impl Displays {
    /// Every screen as the headset that can do what `headset` says is told about it
    /// (`Displays`): in the order of the rows, then those the rows leave out; window
    /// panels and the PC's own displays only for a headset that shows them; one
    /// streaming larger than it decodes, or beyond the streams it takes at once,
    /// failed for it. Texts are cut to fit.
    pub fn displays_part(&self, headset: &state::HeadsetCaps) -> state::Displays {
        self.display_list(Some(headset))
    }

    /// How the PC encodes, as the headset is told (`Encoding`): only codecs it decodes.
    pub fn encoding_part(
        &self,
        wanted: &Settings,
        headset: &state::HeadsetCaps,
    ) -> state::Encoding {
        let decodes = |c: Codec| c == Codec::H264 || headset.decodes(c);
        let (codec, software) = self.encoding_now();
        state::Encoding {
            wanted: state::Wanted {
                bitrate: u16::try_from(wanted.bitrate).unwrap_or(u16::MAX),
                codec: Some(wanted.codec).filter(|c| decodes(*c)),
                software: wanted.software,
                sharp: wanted.sharp,
            },
            using: Some((codec, !software)).filter(|(c, _)| decodes(*c)),
            choices: self
                .choices
                .iter()
                .copied()
                .filter(|(c, _)| decodes(*c))
                .collect(),
        }
    }

    /// Where the PC's sound goes, and its end of the headset microphone (`Sound`), as
    /// the headset is told whose microphone it takes (`mic`) or not (it is off then).
    pub fn sound_part(&self, mic: bool) -> state::Sound {
        let mic = match (&self.mic, &self.mic_failed) {
            _ if !mic => state::MicDevice::Off,
            (Some(_), _) => state::MicDevice::Ready,
            (None, Some(why)) if self.mic_wanted => why.device(),
            (None, _) => state::MicDevice::Off,
        };
        state::Sound {
            to_headset: self.settings.audio,
            mic,
        }
    }

    /// Whether the desktop can save a picture of one output by itself.
    pub fn screenshots_one_output(&self) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.caps.screenshot_one_output)
    }

    /// The scales the desktop gives an output of `w`x`h` pixels.
    pub fn scales_for(&self, w: u32, h: u32) -> Scales {
        let Some(facts) = &self.facts else {
            return Scales::Any { min: 100, max: 100 };
        };
        facts
            .scales
            .iter()
            .find(|(size, _)| *size == (w, h))
            .map_or_else(|| facts.caps.scales.clone(), |(_, s)| s.clone())
    }

    /// Whether the desktop lets the mouse be moved from here.
    pub fn moves_mouse(&self) -> bool {
        self.facts.as_ref().is_some_and(|f| f.caps.moves_pointer)
    }

    /// The desktop's name, for the headset.
    pub fn desktop_name(&self) -> &'static str {
        self.facts.as_ref().map_or("", |f| f.caps.name)
    }

    /// Every screen as it is on the PC, for no headset in particular (the local API):
    /// each kind, each in the state it is in, none failed for what one headset cannot
    /// decode or take (see [`Displays::displays_part`] for that).
    pub fn pc_view(&self) -> state::Displays {
        self.display_list(None)
    }

    /// The list for `headset` (see [`Displays::displays_part`]), or for none (see
    /// [`Displays::pc_view`]).
    pub(super) fn display_list(&self, headset: Option<&state::HeadsetCaps>) -> state::Displays {
        use prostir_proto::state::{
            DeskRect, Display, DisplayKind, DisplayState, Mode, caps, clip,
        };
        let mut order: Vec<ScreenId> = self
            .effective_rows()
            .iter()
            .flatten()
            .map(|e| e.id)
            .collect();
        let rest: Vec<ScreenId> = self
            .screens
            .keys()
            .filter(|id| !order.contains(id))
            .copied()
            .collect();
        order.extend(rest);
        let can = |feature| headset.is_none_or(|h| h.has(feature));
        let shown = |s: &Screen| match s.kind {
            ScreenKind::Virtual { .. } => true,
            ScreenKind::Window { .. } => can(caps::headset::WINDOWS),
            ScreenKind::Physical { .. } => can(caps::headset::PHYSICAL),
        };
        // The headset decodes a picture of this size in this codec (any, for none).
        let fits = |codec, w, h| headset.is_none_or(|hs| hs.fits(codec, w, h));
        // Streams this headset takes at once: those after it, in the order of the
        // list, stand failed for it alone.
        let streams_max = headset.map_or(usize::MAX, streams_max);
        let mut streams = 0;
        let several = self.router.receivers() > 1;
        let displays = order
            .into_iter()
            .filter_map(|id| self.screens.get(&id))
            .filter(|s| shown(s))
            .map(|s| {
                let kind = match &s.kind {
                    ScreenKind::Virtual { .. } => DisplayKind::Virtual,
                    ScreenKind::Window { .. } => DisplayKind::Window {
                        title: String::new(),
                    },
                    ScreenKind::Physical {
                        key,
                        builtin,
                        shown,
                        ..
                    } => DisplayKind::Physical {
                        builtin: *builtin,
                        connector: clip(&key.connector, state::MAX_CONNECTOR),
                        shown: *shown,
                    },
                };
                // A display of the PC that does not stream: as large as the desktop
                // last said it is (nothing known yet: no size).
                let (w, h) = self
                    .router
                    .stream_size(s.id)
                    .or_else(|| s.kind.spec().map(|m| (m.width, m.height)))
                    .or(match &s.kind {
                        ScreenKind::Physical { size, .. } => *size,
                        _ => None,
                    })
                    .unwrap_or_default();
                let scale = s.kind.spec().map_or(100, MonitorSpec::percent);
                // A display of the PC is on the desktop under its connector, shown or not.
                let output = match &s.kind {
                    ScreenKind::Physical { key, .. } => Some(key.connector.as_str()),
                    _ => s.output_key().map(OutputKey::as_str),
                };
                let desk: Option<DeskRect> = output
                    .filter(|_| !matches!(s.state, ScreenState::Absent))
                    .and_then(|name| self.on_desk.get(name).copied());
                // Streaming for other headsets, larger than this one decodes.
                let too_big = self
                    .router
                    .stream_format(s.id)
                    .filter(|((w, h), codec)| !fits(*codec, *w, *h));
                // Its size not known yet: starting still for a headset it might not fit
                // (as made, larger than it decodes; a display of the PC, as large as
                // it is, while several headsets watch).
                let pending = headset.is_some()
                    && self.router.stream_format(s.id).is_none()
                    && s.kind
                        .spec()
                        .map_or(several, |m| !fits(self.cfg.codec, m.width, m.height));
                let state = match (&s.state, too_big) {
                    (ScreenState::Running, Some((size, codec))) => DisplayState::Failed {
                        error: state::ErrorCode::BAD_SIZE,
                        detail: clip(&too_big_why(size, codec), state::MAX_DETAIL),
                    },
                    (ScreenState::Starting, _) => DisplayState::Starting,
                    (ScreenState::Running, None) if pending => DisplayState::Starting,
                    (ScreenState::Running, None) => DisplayState::Running,
                    (ScreenState::Failed { error, why }, _) => DisplayState::Failed {
                        error: *error,
                        detail: clip(why, state::MAX_DETAIL),
                    },
                    (ScreenState::Absent, _) => DisplayState::Absent,
                };
                let streams_here = matches!(state, DisplayState::Running | DisplayState::Starting)
                    && (!s.is_physical() || s.is_shown_physical());
                let state = if streams_here && streams >= streams_max {
                    let why = format!(
                        "{} ({streams_max})",
                        t(
                            "більше потоків, ніж шолом бере",
                            "more streams than the headset takes"
                        )
                    );
                    DisplayState::Failed {
                        error: state::ErrorCode::LIMIT,
                        detail: clip(&why, state::MAX_DETAIL),
                    }
                } else {
                    streams += usize::from(streams_here);
                    state
                };
                Display {
                    id: s.id.wire(),
                    kind,
                    name: clip(&s.name(), state::MAX_DISPLAY_NAME),
                    mode: Mode {
                        w,
                        h,
                        scale: scale.max(1),
                    },
                    desk,
                    state,
                }
            })
            .collect::<Vec<_>>();
        let displays = within_limit(displays);
        let primary = self
            .primary
            .map(ScreenId::wire)
            .filter(|p| displays.iter().any(|d| d.id == *p));
        state::Displays { displays, primary }
    }
}

/// `Displays` holds at most `MAX_DISPLAYS`: beyond that, displays of the PC that are
/// not connected go first (from the end), then the last ones, with a warning. Never
/// more than that, so the part always goes out.
pub(super) fn within_limit(mut displays: Vec<state::Display>) -> Vec<state::Display> {
    let max = state::MAX_DISPLAYS;
    if displays.len() <= max {
        return displays;
    }
    warn!(
        "{} displays, the headset is told about {max}: absent ones of the PC left out first",
        displays.len()
    );
    while displays.len() > max {
        match displays
            .iter()
            .rposition(|d| matches!(d.state, state::DisplayState::Absent))
        {
            Some(i) => {
                displays.remove(i);
            }
            None => displays.truncate(max),
        }
    }
    displays
}

/// `rows` kept in step with the outputs: only monitors (`monitor`) and other outputs
/// that show (`shown`) stay, each in the first place it is listed in, and any of
/// `monitors` left out go to the end of the bottom row. So every monitor is listed
/// exactly once.
pub(super) fn rows_in_step<K: Copy + PartialEq>(
    rows: &[Vec<desk::Entry<K>>],
    monitors: &[K],
    monitor: impl Fn(K) -> bool,
    shown: impl Fn(K) -> bool,
) -> Vec<Vec<desk::Entry<K>>> {
    let mut placed: Vec<K> = Vec::new();
    let mut kept: Vec<Vec<desk::Entry<K>>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .filter(|e| {
                    let new = (monitor(e.id) || shown(e.id)) && !placed.contains(&e.id);
                    if new {
                        placed.push(e.id);
                    }
                    new
                })
                .copied()
                .collect::<Vec<_>>()
        })
        .filter(|r| !r.is_empty())
        .collect();
    let mut missing = Vec::new();
    for &id in monitors {
        if monitor(id) && !placed.contains(&id) {
            placed.push(id);
            missing.push(desk::Entry::new(id));
        }
    }
    if !missing.is_empty() {
        match kept.last_mut() {
            Some(last) => last.extend(missing),
            None => kept.push(missing),
        }
    }
    kept
}
