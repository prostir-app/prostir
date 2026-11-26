//! What the headsets hold down on the PC: mouse buttons, fingers and keys, each by the
//! session that pressed it. Only what this keeps reaches the desktop as let go, and
//! everything a session holds is let go when it goes (its end, the headset off the
//! head, two seconds without a word from it while it holds something), so nothing
//! stays pressed on the PC.
//!
//! Several sessions may hold the same button: the desktop has one, so it goes down with
//! the first and up with the last.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::events::SessionId;
use crate::platform::{MouseButton, Spot, TouchPhase};

/// A session holding something that says nothing for this long lets it all go: a
/// headset holding a button or a finger repeats where it is at least once a second.
pub const QUIET: Duration = Duration::from_secs(2);

/// Keys let go after the others when everything goes, so a shortcut does not fire
/// without its modifier on the way out: Ctrl, Shift, Alt and Meta, each side.
const MODIFIERS: [u16; 8] = [29, 97, 42, 54, 56, 100, 125, 126];

/// What the desktop is to be told.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    To(Spot),
    Button(MouseButton, bool),
    Scroll(i32, i32),
    /// The finger's number on the desktop, what it does, and where.
    Touch(u32, TouchPhase, Spot),
    Key(u16, bool),
}

/// What one session holds.
#[derive(Debug)]
struct Holding {
    buttons: BTreeSet<MouseButton>,
    /// Its fingers by slot: their number on the desktop and where they are.
    touches: BTreeMap<u8, (u32, Spot)>,
    keys: BTreeSet<u16>,
    /// When it last said anything.
    heard: Instant,
}

impl Holding {
    fn new(now: Instant) -> Self {
        Self {
            buttons: BTreeSet::new(),
            touches: BTreeMap::new(),
            keys: BTreeSet::new(),
            heard: now,
        }
    }

    fn is_empty(&self) -> bool {
        self.buttons.is_empty() && self.touches.is_empty() && self.keys.is_empty()
    }
}

/// Everything held, by session.
#[derive(Debug, Default)]
pub struct Held {
    by: BTreeMap<SessionId, Holding>,
}

impl Held {
    /// Session `from` said something at `now`.
    pub fn heard(&mut self, from: SessionId, now: Instant) {
        if let Some(h) = self.by.get_mut(&from) {
            h.heard = now;
        }
    }

    fn holding(&mut self, from: SessionId, now: Instant) -> &mut Holding {
        let h = self.by.entry(from).or_insert_with(|| Holding::new(now));
        h.heard = now;
        h
    }

    fn others_hold(&self, from: SessionId, held: impl Fn(&Holding) -> bool) -> bool {
        self.by.iter().any(|(s, h)| *s != from && held(h))
    }

    /// Button `button` of session `from` down or up: what the desktop is told, if
    /// anything (a second press of a button held, a release of one not held, or one
    /// another session still holds, tells it nothing).
    pub fn button(
        &mut self,
        from: SessionId,
        button: MouseButton,
        down: bool,
        now: Instant,
    ) -> Option<Act> {
        let others = self.others_hold(from, |h| h.buttons.contains(&button));
        let h = self.holding(from, now);
        let changed = if down {
            h.buttons.insert(button)
        } else {
            h.buttons.remove(&button)
        };
        self.forget_empty(from);
        (changed && !others).then_some(Act::Button(button, down))
    }

    /// Key `code` of session `from`, as [`Held::button`].
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "keys come with the move between PCs (step 3)")
    )]
    pub fn key(&mut self, from: SessionId, code: u16, down: bool, now: Instant) -> Option<Act> {
        let others = self.others_hold(from, |h| h.keys.contains(&code));
        let h = self.holding(from, now);
        let changed = if down {
            h.keys.insert(code)
        } else {
            h.keys.remove(&code)
        };
        self.forget_empty(from);
        (changed && !others).then_some(Act::Key(code, down))
    }

    /// Finger `slot` of session `from`: what the desktop is told. A finger down gets
    /// the lowest number no finger on the desktop has; one moved or lifted that was
    /// never down tells nothing; down again while down moves it.
    pub fn touch(
        &mut self,
        from: SessionId,
        slot: u8,
        phase: TouchPhase,
        at: Spot,
        now: Instant,
    ) -> Option<Act> {
        let taken: BTreeSet<u32> = self
            .by
            .values()
            .flat_map(|h| h.touches.values().map(|(id, _)| *id))
            .collect();
        let h = self.holding(from, now);
        let act = match (phase, h.touches.get_mut(&slot)) {
            (TouchPhase::Down | TouchPhase::Move, Some((id, spot))) => {
                *spot = at.clone();
                Some(Act::Touch(*id, TouchPhase::Move, at))
            }
            (TouchPhase::Down, None) => {
                let id = (0..).find(|n| !taken.contains(n)).unwrap_or(0);
                h.touches.insert(slot, (id, at.clone()));
                Some(Act::Touch(id, TouchPhase::Down, at))
            }
            (TouchPhase::Up | TouchPhase::Cancel, Some(_)) => {
                let (id, _) = h.touches.remove(&slot)?;
                Some(Act::Touch(id, phase, at))
            }
            (_, None) => None,
        };
        self.forget_empty(from);
        act
    }

    fn forget_empty(&mut self, from: SessionId) {
        if self.by.get(&from).is_some_and(Holding::is_empty) {
            self.by.remove(&from);
        }
    }

    /// Everything session `from` holds, let go: fingers lifted without counting, keys
    /// (modifiers last), then buttons. What another session also holds stays down.
    pub fn release_all(&mut self, from: SessionId) -> Vec<Act> {
        let Some(h) = self.by.remove(&from) else {
            return Vec::new();
        };
        let mut acts: Vec<Act> = h
            .touches
            .into_values()
            .map(|(id, at)| Act::Touch(id, TouchPhase::Cancel, at))
            .collect();
        let (mods, plain): (Vec<u16>, Vec<u16>) =
            h.keys.into_iter().partition(|k| MODIFIERS.contains(k));
        for code in plain.into_iter().chain(mods) {
            if !self.others_hold(from, |o| o.keys.contains(&code)) {
                acts.push(Act::Key(code, false));
            }
        }
        for b in h.buttons {
            if !self.others_hold(from, |o| o.buttons.contains(&b)) {
                acts.push(Act::Button(b, false));
            }
        }
        acts
    }

    /// The sessions that hold something and said nothing for [`QUIET`].
    pub fn quiet(&self, now: Instant) -> Vec<SessionId> {
        self.by
            .iter()
            .filter(|(_, h)| now.duration_since(h.heard) >= QUIET)
            .map(|(s, _)| *s)
            .collect()
    }

    /// The sessions that hold something.
    pub fn sessions(&self) -> Vec<SessionId> {
        self.by.keys().copied().collect()
    }

    /// Whether anything is held.
    pub fn any(&self) -> bool {
        !self.by.is_empty()
    }

    /// Whether a finger of any session touches the desktop.
    pub fn touching(&self) -> bool {
        self.by.values().any(|h| !h.touches.is_empty())
    }

    /// The desktop's devices are gone, and what they held with them.
    pub fn clear(&mut self) {
        self.by.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::OutputKey;
    use prostir_proto::state::DeskRect;

    fn spot(x: f64) -> Spot {
        let rect = DeskRect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        Spot {
            output: OutputKey::new("Fake-1"),
            rect,
            desk: rect,
            x,
            y: 0.0,
        }
    }

    /// A press goes down once and up once: twice down, or up without down, says
    /// nothing more.
    #[test]
    fn a_button_goes_down_and_up_once() {
        let now = Instant::now();
        let mut held = Held::default();
        let left = MouseButton::Left;
        assert_eq!(
            held.button(1, left, true, now),
            Some(Act::Button(left, true))
        );
        assert_eq!(held.button(1, left, true, now), None);
        assert_eq!(
            held.button(1, left, false, now),
            Some(Act::Button(left, false))
        );
        assert_eq!(held.button(1, left, false, now), None);
        assert_eq!(held.button(1, MouseButton::Right, false, now), None);
        assert!(!held.any());
    }

    /// Two sessions on one button: down with the first, up with the last; the end of
    /// one leaves the other's down.
    #[test]
    fn a_button_two_sessions_hold_goes_up_with_the_last() {
        let now = Instant::now();
        let mut held = Held::default();
        let left = MouseButton::Left;
        assert!(held.button(1, left, true, now).is_some());
        assert_eq!(held.button(2, left, true, now), None);
        assert_eq!(held.release_all(1), Vec::new());
        assert_eq!(
            held.button(2, left, false, now),
            Some(Act::Button(left, false))
        );
    }

    /// Everything a session holds goes when it goes: fingers cancelled, keys with the
    /// modifiers last, then buttons; nothing is let go twice.
    #[test]
    fn a_session_that_goes_lets_everything_go() {
        let now = Instant::now();
        let mut held = Held::default();
        held.button(1, MouseButton::Right, true, now);
        held.key(1, 29, true, now); // left Ctrl
        held.key(1, 46, true, now); // C
        held.touch(1, 0, TouchPhase::Down, spot(5.0), now);
        held.touch(1, 0, TouchPhase::Move, spot(7.0), now);
        assert_eq!(
            held.release_all(1),
            vec![
                Act::Touch(0, TouchPhase::Cancel, spot(7.0)),
                Act::Key(46, false),
                Act::Key(29, false),
                Act::Button(MouseButton::Right, false),
            ]
        );
        assert_eq!(held.release_all(1), Vec::new());
        assert!(!held.any());
    }

    /// Fingers of two sessions get numbers of their own on the desktop; a finger never
    /// down is not moved or lifted.
    #[test]
    fn fingers_get_numbers_of_their_own() {
        let now = Instant::now();
        let mut held = Held::default();
        assert_eq!(held.touch(1, 3, TouchPhase::Move, spot(1.0), now), None);
        assert_eq!(held.touch(1, 3, TouchPhase::Up, spot(1.0), now), None);
        assert_eq!(
            held.touch(1, 3, TouchPhase::Down, spot(1.0), now),
            Some(Act::Touch(0, TouchPhase::Down, spot(1.0)))
        );
        assert_eq!(
            held.touch(2, 3, TouchPhase::Down, spot(2.0), now),
            Some(Act::Touch(1, TouchPhase::Down, spot(2.0)))
        );
        assert_eq!(
            held.touch(1, 4, TouchPhase::Down, spot(3.0), now),
            Some(Act::Touch(2, TouchPhase::Down, spot(3.0)))
        );
        assert_eq!(
            held.touch(1, 3, TouchPhase::Up, spot(4.0), now),
            Some(Act::Touch(0, TouchPhase::Up, spot(4.0)))
        );
        // The number lifted is free again.
        assert_eq!(
            held.touch(2, 0, TouchPhase::Down, spot(5.0), now),
            Some(Act::Touch(0, TouchPhase::Down, spot(5.0)))
        );
    }

    /// Two seconds without a word from a session that holds something: it is due to
    /// let go; one that holds nothing never is, and a word puts it off.
    #[test]
    fn a_quiet_session_holding_something_lets_go() {
        let start = Instant::now();
        let mut held = Held::default();
        held.button(1, MouseButton::Left, true, start);
        held.button(2, MouseButton::Left, true, start);
        held.button(2, MouseButton::Left, false, start);
        assert!(held.quiet(start + Duration::from_secs(1)).is_empty());
        held.heard(1, start + Duration::from_secs(1));
        assert!(held.quiet(start + Duration::from_secs(2)).is_empty());
        assert_eq!(held.quiet(start + Duration::from_secs(3)), vec![1]);
    }
}
