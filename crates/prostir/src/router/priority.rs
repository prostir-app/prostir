//! Which streams get every frame and which fewer, from where the headsets look (their
//! `Gaze`), the PC's active window and "Focus" in the headsets: a state machine with
//! the time given, which the router asks for each stream's level and sets the stream's
//! frame gate to (see `encoder/gate.rs`).
//!
//! - A stream looked at, or with the PC's active window, gets every frame; so does
//!   every stream while nothing is known of the gaze (a headset that does not say
//!   where it looks, or has not said yet). The others get half their monitor's rate,
//!   or 5 frames a second where every headset on the head has "Focus" on.
//! - Up at once, down slowly: a stream the gaze left keeps every frame for [`HOLD`],
//!   so a head that sweeps over the monitors changes nothing.
//! - Several headsets: a stream gets the most any headset on the head needs of it (it
//!   is encoded once for all). One taken off counts for nothing, and its gaze is
//!   forgotten: put on again, it sees everything until it says where it looks.
//!
//! None of this asks for a keyframe: a level only spaces the frames.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::encoder::Level;
use crate::events::SessionId;
use crate::screen::ScreenId;

/// How long a stream the gaze left keeps every frame.
pub const HOLD: Duration = Duration::from_secs(2);

/// What a headset said of where it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gaze {
    /// Nothing yet.
    Unknown,
    /// At this display.
    At(ScreenId),
    /// At none of this PC's displays (the menu, the room, another PC).
    Away,
}

/// One headset, as the levels see it.
#[derive(Debug)]
struct Look {
    worn: bool,
    /// It says where it looks (headset feature 6); one that does not sees everything.
    tells: bool,
    gaze: Gaze,
    /// "Focus" is on in it.
    focus: bool,
    /// Displays the gaze left, each with when it stops counting as looked at.
    left: Vec<(ScreenId, Instant)>,
}

impl Look {
    /// What this headset needs of stream `id` at `now`, and why.
    fn needs(&self, id: ScreenId, now: Instant) -> (Level, Why) {
        if !self.tells || self.gaze == Gaze::Unknown {
            return (Level::Full, Why::Unknown);
        }
        if self.gaze == Gaze::At(id) {
            return (Level::Full, Why::Looked);
        }
        if self.left.iter().any(|(i, until)| *i == id && *until > now) {
            return (Level::Full, Why::JustLeft);
        }
        if self.focus {
            (Level::Glance, Why::Aside)
        } else {
            (Level::Side, Why::Aside)
        }
    }
}

/// Why a stream has its level, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A headset looks at it.
    Looked,
    /// The gaze left it less than [`HOLD`] ago.
    JustLeft,
    /// The PC's active window is on it.
    Active,
    /// Nothing is known of where a headset looks, or none is on the head.
    Unknown,
    /// Nobody looks at it.
    Aside,
}

impl Why {
    pub fn says(self) -> &'static str {
        match self {
            Self::Looked => "looked at",
            Self::JustLeft => "looked at a moment ago",
            Self::Active => "the active window is on it",
            Self::Unknown => "where the headset looks is not known",
            Self::Aside => "nobody looks at it",
        }
    }
}

/// The headsets' gaze and the active window, for the levels of the streams.
#[derive(Debug, Default)]
pub struct Priority {
    headsets: BTreeMap<SessionId, Look>,
    /// The display with the PC's active window.
    active: Option<ScreenId>,
}

impl Priority {
    /// The headset of `session` came, on the head or not; `tells`: it says where it
    /// looks.
    pub fn attach(&mut self, session: SessionId, worn: bool, tells: bool) {
        self.headsets.insert(
            session,
            Look {
                worn,
                tells,
                gaze: Gaze::Unknown,
                focus: false,
                left: Vec::new(),
            },
        );
    }

    /// Its session ended.
    pub fn detach(&mut self, session: SessionId) {
        self.headsets.remove(&session);
    }

    /// It is on the head or not; taken off, where it looked is forgotten.
    pub fn set_worn(&mut self, session: SessionId, worn: bool) {
        if let Some(h) = self.headsets.get_mut(&session) {
            h.worn = worn;
            if !worn {
                h.gaze = Gaze::Unknown;
                h.left.clear();
            }
        }
    }

    /// "Focus" is on in it or not.
    pub fn set_focus(&mut self, session: SessionId, on: bool) {
        if let Some(h) = self.headsets.get_mut(&session) {
            h.focus = on;
        }
    }

    /// It looks at display `at` (none: at none of them) from `now`.
    pub fn gaze(&mut self, session: SessionId, at: Option<ScreenId>, now: Instant) {
        let Some(h) = self.headsets.get_mut(&session) else {
            return;
        };
        let gaze = at.map_or(Gaze::Away, Gaze::At);
        if h.gaze == gaze {
            return;
        }
        h.left
            .retain(|(i, until)| *until > now && Gaze::At(*i) != gaze);
        if let Gaze::At(old) = h.gaze {
            h.left.retain(|(i, _)| *i != old);
            h.left.push((old, now + HOLD));
        }
        h.gaze = gaze;
    }

    /// The PC's active window is on display `at` (none: on none of them).
    pub fn set_active(&mut self, at: Option<ScreenId>) {
        self.active = at;
    }

    /// The level of stream `id` at `now`, and why: the most any headset on the head
    /// needs of it, every frame without one.
    pub fn level(&self, id: ScreenId, now: Instant) -> (Level, Why) {
        if self.active == Some(id) {
            return (Level::Full, Why::Active);
        }
        self.headsets
            .values()
            .filter(|h| h.worn)
            .map(|h| h.needs(id, now))
            .max_by_key(|(level, _)| *level)
            .unwrap_or((Level::Full, Why::Unknown))
    }

    /// When a level may next go down by itself (a display the gaze left stops counting
    /// as looked at).
    pub fn due(&self, now: Instant) -> Option<Instant> {
        self.headsets
            .values()
            .filter(|h| h.worn)
            .flat_map(|h| h.left.iter().map(|(_, until)| *until))
            .filter(|until| *until > now)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> ScreenId {
        ScreenId::from_wire(prostir_proto::state::DisplayId(n))
    }

    const MS: Duration = Duration::from_millis(1);

    /// One headset on the head that says where it looks.
    fn one() -> Priority {
        let mut p = Priority::default();
        p.attach(1, true, true);
        p
    }

    fn levels(p: &Priority, now: Instant) -> [Level; 3] {
        [id(0), id(1), id(2)].map(|i| p.level(i, now).0)
    }

    /// Before the headset says where it looks, every stream gets every frame; looked at,
    /// one keeps them at once, and the others go down to half.
    #[test]
    fn the_stream_looked_at_gets_every_frame() {
        let mut p = one();
        let t0 = Instant::now();
        assert_eq!(levels(&p, t0), [Level::Full; 3], "nothing known yet");
        assert_eq!(p.level(id(0), t0).1, Why::Unknown);
        p.gaze(1, Some(id(1)), t0);
        assert_eq!(levels(&p, t0), [Level::Side, Level::Full, Level::Side]);
        assert_eq!(p.level(id(1), t0).1, Why::Looked);
        assert_eq!(p.due(t0), None, "nothing to come down");
    }

    /// The stream the gaze left keeps every frame for 2 s, then goes down; the one it
    /// came to has them at once.
    #[test]
    fn a_stream_the_gaze_left_goes_down_after_2_s() {
        let mut p = one();
        let t0 = Instant::now();
        p.gaze(1, Some(id(0)), t0);
        p.gaze(1, Some(id(1)), t0 + MS);
        assert_eq!(levels(&p, t0 + MS), [Level::Full, Level::Full, Level::Side]);
        assert_eq!(p.level(id(0), t0 + MS).1, Why::JustLeft);
        assert_eq!(p.due(t0 + MS), Some(t0 + MS + HOLD));
        let later = t0 + MS + HOLD - MS;
        assert_eq!(p.level(id(0), later).0, Level::Full, "not yet");
        assert_eq!(p.level(id(0), t0 + MS + HOLD).0, Level::Side, "after 2 s");
        assert_eq!(p.due(t0 + MS + HOLD), None);
    }

    /// A head that sweeps over three monitors and back changes nothing for the one it
    /// came from: the others it passed hold every frame for 2 s each.
    #[test]
    fn a_sweep_changes_nothing() {
        let mut p = one();
        let t0 = Instant::now();
        p.gaze(1, Some(id(0)), t0);
        p.gaze(1, Some(id(1)), t0 + 250 * MS);
        p.gaze(1, Some(id(2)), t0 + 500 * MS);
        p.gaze(1, Some(id(0)), t0 + 750 * MS);
        let now = t0 + 800 * MS;
        assert_eq!(levels(&p, now), [Level::Full; 3]);
        assert_eq!(p.level(id(0), now).1, Why::Looked);
        assert_eq!(
            levels(&p, t0 + 750 * MS + HOLD),
            [Level::Full, Level::Side, Level::Side]
        );
        // Back to where it was a moment ago: that one counts as looked at, not left.
        p.gaze(1, Some(id(1)), t0 + 3 * HOLD);
        p.gaze(1, Some(id(0)), t0 + 3 * HOLD + MS);
        p.gaze(1, Some(id(1)), t0 + 3 * HOLD + 2 * MS);
        assert_eq!(p.level(id(1), t0 + 5 * HOLD).0, Level::Full);
    }

    /// Looking at none of the displays (the menu, another PC): every stream goes down
    /// after 2 s, but the one with the active window.
    #[test]
    fn looking_away_then_all_down_but_the_active_window() {
        let mut p = one();
        let t0 = Instant::now();
        p.set_active(Some(id(2)));
        p.gaze(1, Some(id(0)), t0);
        p.gaze(1, None, t0 + MS);
        assert_eq!(levels(&p, t0 + MS), [Level::Full, Level::Side, Level::Full]);
        assert_eq!(p.level(id(2), t0).1, Why::Active);
        assert_eq!(
            levels(&p, t0 + MS + HOLD),
            [Level::Side, Level::Side, Level::Full]
        );
        p.set_active(None);
        assert_eq!(levels(&p, t0 + MS + HOLD), [Level::Side; 3]);
    }

    /// "Focus": 5 frames a second aside, only where every headset on the head has it.
    #[test]
    fn focus_only_when_every_headset_has_it() {
        let mut p = one();
        let t0 = Instant::now();
        p.set_focus(1, true);
        p.gaze(1, Some(id(0)), t0);
        assert_eq!(levels(&p, t0), [Level::Full, Level::Glance, Level::Glance]);
        p.attach(2, true, true);
        p.gaze(2, Some(id(0)), t0);
        assert_eq!(
            levels(&p, t0),
            [Level::Full, Level::Side, Level::Side],
            "the other without Focus"
        );
        p.set_focus(2, true);
        assert_eq!(levels(&p, t0), [Level::Full, Level::Glance, Level::Glance]);
    }

    /// Two headsets: a stream gets the most either needs; one taken off counts for
    /// nothing, and put on again sees everything until it says where it looks.
    #[test]
    fn the_most_any_headset_on_the_head_needs() {
        let mut p = one();
        p.attach(2, true, true);
        let t0 = Instant::now();
        p.gaze(1, Some(id(0)), t0);
        assert_eq!(
            levels(&p, t0),
            [Level::Full; 3],
            "the second said nothing yet"
        );
        p.gaze(2, Some(id(1)), t0);
        assert_eq!(levels(&p, t0), [Level::Full, Level::Full, Level::Side]);
        p.set_worn(2, false);
        assert_eq!(levels(&p, t0), [Level::Full, Level::Side, Level::Side]);
        p.set_worn(2, true);
        assert_eq!(levels(&p, t0), [Level::Full; 3], "forgot where it looked");
        p.detach(2);
        assert_eq!(levels(&p, t0), [Level::Full, Level::Side, Level::Side]);
        // None on the head: every frame (the encoders pause anyway).
        p.set_worn(1, false);
        assert_eq!(levels(&p, t0), [Level::Full; 3]);
    }

    /// A headset that does not say where it looks (without feature 6) sees everything.
    #[test]
    fn an_older_headset_sees_everything() {
        let mut p = one();
        p.attach(2, true, false);
        let t0 = Instant::now();
        p.gaze(1, Some(id(0)), t0);
        assert_eq!(levels(&p, t0), [Level::Full; 3]);
        assert_eq!(p.level(id(1), t0).1, Why::Unknown);
    }

    /// A display that is no stream (gone, or not started yet) is looked at as any
    /// other: no stream gets every frame for it, and it does once it starts.
    #[test]
    fn a_display_without_a_stream() {
        let mut p = one();
        let t0 = Instant::now();
        p.gaze(1, Some(id(9)), t0);
        assert_eq!(levels(&p, t0), [Level::Side; 3]);
        assert_eq!(p.level(id(9), t0).0, Level::Full);
        // A gaze for a session that is not there changes nothing.
        p.gaze(5, Some(id(0)), t0);
        assert_eq!(levels(&p, t0), [Level::Side; 3]);
    }
}
