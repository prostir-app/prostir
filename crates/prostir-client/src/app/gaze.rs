//! Telling the PC which of its monitors the user looks at (step 7.12, decision 23): the
//! `Gaze` message, sent only to a PC that said it takes it (host feature 14), only when
//! the monitor looked at changes, and never faster than [`EVERY`]. The PC uses it for
//! which monitor gets the most bits. It works from the same ray that finds the monitor
//! for the mouse that follows the gaze, so a frame computes it once and allocates
//! nothing.
//!
//! None of this is shown in the menu, so none of it moves [`Model::rev`].

use std::time::{Duration, Instant};

use glam::{Vec2, Vec3};
use prostir_proto::ClientMsg;
use prostir_proto::state::caps;

use super::Model;
use crate::ids::{DisplayKey, PcId};
use crate::layout::Ray;
use crate::session::Link;
use crate::ui;

/// How long a monitor must stay the one looked at before the PC is told, so a gaze
/// that flickers over a seam between two monitors says nothing.
const STEADY: Duration = Duration::from_millis(150);
/// The shortest time between two messages: at most four a second.
const EVERY: Duration = Duration::from_millis(250);

/// What the PCs were last told about the gaze, and what is waiting to be told.
#[derive(Debug, Default)]
pub(super) struct Told {
    /// The monitor the PCs were last told of (`None`: none of them is looked at).
    monitor: Option<DisplayKey>,
    /// What is looked at now when it differs from `monitor`, and since when.
    candidate: Option<(Option<DisplayKey>, Instant)>,
    /// When a message went last.
    sent: Option<Instant>,
}

/// What to say once the gaze has settled on another monitor.
#[derive(Debug, PartialEq)]
pub(super) struct Say {
    /// The PC the gaze left: it hears that the user looks at none of its monitors
    /// (unless the next message is to it as well).
    pub leave: Option<PcId>,
    /// The monitor looked at now and the point on it (0 to 65535 from the top left).
    pub arrive: Option<(DisplayKey, u16, u16)>,
}

impl Told {
    /// A new session with `pc` knows nothing of what was told in the last one.
    pub(super) fn forget(&mut self, pc: PcId) {
        if self.monitor.is_some_and(|k| k.pc == pc) {
            self.monitor = None;
        }
        if self
            .candidate
            .is_some_and(|(k, _)| k.is_some_and(|k| k.pc == pc))
        {
            self.candidate = None;
        }
    }

    /// A message went out at `now`: the next waits for [`EVERY`].
    pub(super) fn said(&mut self, now: Instant) {
        self.sent = Some(now);
    }

    /// One frame: `looked` is the monitor the gaze is on and where on it (`None`: on
    /// none of those whose PC takes the gaze). What to say, if the gaze has rested on
    /// another monitor long enough and the last message is old enough.
    pub(super) fn step(&mut self, now: Instant, looked: Option<(DisplayKey, Vec2)>) -> Option<Say> {
        let key = looked.map(|(k, _)| k);
        if key == self.monitor {
            self.candidate = None;
            return None;
        }
        let since = match self.candidate {
            Some((k, since)) if k == key => since,
            _ => {
                self.candidate = Some((key, now));
                return None;
            }
        };
        let rested = now.saturating_duration_since(since) >= STEADY;
        let spaced = self
            .sent
            .is_none_or(|t| now.saturating_duration_since(t) >= EVERY);
        if !rested || !spaced {
            return None;
        }
        let left = std::mem::replace(&mut self.monitor, key);
        self.candidate = None;
        let point = |v: f32| (v.clamp(0.0, 1.0) * 65535.0) as u16;
        Some(Say {
            leave: left
                .map(|k| k.pc)
                .filter(|pc| key.is_none_or(|k| k.pc != *pc)),
            arrive: looked.map(|(k, uv)| (k, point(uv.x), point(uv.y))),
        })
    }
}

impl Model {
    /// PC `pc` is in a session and said it takes the gaze (host feature 14).
    fn takes_gaze(&self, pc: PcId) -> bool {
        self.session
            .pc(pc)
            .and_then(|p| p.state.caps.as_ref())
            .is_some_and(|c| c.has(caps::pc::GAZE))
    }

    /// Some PC in a session takes the gaze.
    fn any_takes_gaze(&self) -> bool {
        self.session.links().values().any(|l| {
            matches!(l, Link::Connected(p)
                if p.state.caps.as_ref().is_some_and(|c| c.has(caps::pc::GAZE)))
        })
    }

    /// The monitor the head looks at and where on it, as drawn; worked out only when
    /// something uses it (a PC that takes the gaze, or the mouse that follows it),
    /// and only while the app has the focus.
    pub(super) fn looked(&self, focused: bool) -> Option<(DisplayKey, Vec2)> {
        if !focused || !(self.settings.gaze_mouse || self.any_takes_gaze()) {
            return None;
        }
        let (head, forward) = self.head;
        let ray = Ray {
            origin: head,
            dir: forward.normalize_or(-Vec3::Z),
        };
        let session = &self.session;
        self.world
            .looked_at(ray, &self.settings, self.menu.open, |k| {
                session.waits(ui::Asked::Remove(k))
            })
    }

    /// The end of a frame's look: the PC whose monitor the gaze rests on is told when
    /// that changes. `looked` is as [`Model::looked`] gave it (`None` too when there
    /// is no frame).
    pub(super) fn tell_gaze(&mut self, looked: Option<(DisplayKey, Vec2)>) {
        let looked = looked.filter(|(k, _)| self.takes_gaze(k.pc));
        let Some(say) = self.gaze_told.step(self.now, looked) else {
            return;
        };
        let mut went = false;
        if let Some(pc) = say.leave {
            went |= self.say_gaze(
                pc,
                ClientMsg::Gaze {
                    display: None,
                    u: 0,
                    v: 0,
                },
            );
        }
        if let Some((k, u, v)) = say.arrive {
            went |= self.say_gaze(
                k.pc,
                ClientMsg::Gaze {
                    display: Some(k.id),
                    u,
                    v,
                },
            );
        }
        // A message to a PC that is gone does not count against the next one.
        if went {
            self.gaze_told.said(self.now);
        }
    }

    /// Sends `msg` to `pc` if it is in a session and takes the gaze; whether it went.
    fn say_gaze(&mut self, pc: PcId, msg: ClientMsg) -> bool {
        let takes = self.takes_gaze(pc);
        if takes {
            self.send_to(pc, msg);
        }
        takes
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;
    use prostir_proto::state::DisplayId;

    const A: PcId = PcId([1; 8]);
    const B: PcId = PcId([2; 8]);

    /// One frame as the model takes it: a message that was said counts at once.
    fn stepped(t: &mut Told, now: Instant, looked: Option<(DisplayKey, Vec2)>) -> Option<Say> {
        let say = t.step(now, looked);
        if say.is_some() {
            t.said(now);
        }
        say
    }

    fn key(pc: PcId, n: u32) -> DisplayKey {
        DisplayKey::new(pc, DisplayId(n))
    }

    fn at(pc: PcId, n: u32) -> Option<(DisplayKey, Vec2)> {
        Some((key(pc, n), Vec2::new(0.5, 0.25)))
    }

    /// Frames of 11 ms with the gaze on `looked`, for `ms` milliseconds from `from`:
    /// what was said, with when.
    fn gaze(
        t: &mut Told,
        from: Instant,
        ms: u64,
        looked: Option<(DisplayKey, Vec2)>,
    ) -> Vec<(u64, Say)> {
        (0..=ms / 11)
            .filter_map(|i| {
                let at = i * 11;
                stepped(t, from + Duration::from_millis(at), looked).map(|s| (at, s))
            })
            .collect()
    }

    #[test]
    fn a_monitor_looked_at_is_said_once_it_rests() {
        let t0 = Instant::now();
        let mut t = Told::default();
        let said = gaze(&mut t, t0, 2000, at(A, 1));
        assert_eq!(said.len(), 1, "once, not every frame");
        let (when, say) = &said[0];
        assert!(*when >= 150 && *when < 200, "after it rested: {when}");
        assert_eq!(
            *say,
            Say {
                leave: None,
                arrive: Some((key(A, 1), 32767, 16383)),
            }
        );
    }

    #[test]
    fn nothing_is_said_while_no_monitor_is_looked_at() {
        let mut t = Told::default();
        assert!(gaze(&mut t, Instant::now(), 2000, None).is_empty());
    }

    #[test]
    fn a_move_over_a_monitor_says_nothing() {
        let t0 = Instant::now();
        let mut t = Told::default();
        assert_eq!(gaze(&mut t, t0, 500, at(A, 1)).len(), 1);
        // The point on the same monitor wanders: not a change.
        let said: Vec<_> = (0..200u64)
            .filter_map(|i| {
                let uv = Vec2::new((i % 10) as f32 / 10.0, 0.5);
                stepped(
                    &mut t,
                    t0 + Duration::from_millis(500 + i * 11),
                    Some((key(A, 1), uv)),
                )
            })
            .collect();
        assert!(said.is_empty());
    }

    #[test]
    fn a_flicker_over_a_seam_says_nothing_and_a_rest_says_the_last() {
        let t0 = Instant::now();
        let mut t = Told::default();
        assert_eq!(gaze(&mut t, t0, 500, at(A, 1)).len(), 1);
        // Back and forth every 50 ms: never rests 150 ms.
        let mut said = Vec::new();
        for i in 0..40u64 {
            let looked = if i % 2 == 0 { at(A, 2) } else { at(A, 1) };
            said.extend(stepped(
                &mut t,
                t0 + Duration::from_millis(500 + i * 50),
                looked,
            ));
        }
        assert!(said.is_empty(), "{said:?}");
        let t1 = t0 + Duration::from_millis(3000);
        let said = gaze(&mut t, t1, 1000, at(A, 2));
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].1.arrive.map(|a| a.0), Some(key(A, 2)));
    }

    #[test]
    fn at_most_four_a_second_however_the_gaze_moves() {
        let t0 = Instant::now();
        let mut t = Told::default();
        let mut said = Vec::new();
        // Rests 300 ms on each of two monitors in turn, for 10 seconds.
        for i in 0..10_000u64 / 11 {
            let ms = i * 11;
            let looked = if (ms / 300) % 2 == 0 {
                at(A, 1)
            } else {
                at(A, 2)
            };
            said.extend(stepped(&mut t, t0 + Duration::from_millis(ms), looked).map(|_| ms));
        }
        assert!(said.len() > 15, "it does tell: {}", said.len());
        for w in said.windows(2) {
            assert!(w[1] - w[0] >= 250, "{w:?}");
        }
        assert!(said.len() <= 40);
    }

    #[test]
    fn looking_away_tells_the_pc_that_it_is_no_longer_looked_at() {
        let t0 = Instant::now();
        let mut t = Told::default();
        gaze(&mut t, t0, 500, at(A, 1));
        let said = gaze(&mut t, t0 + Duration::from_secs(1), 1000, None);
        assert_eq!(said.len(), 1);
        assert_eq!(
            said[0].1,
            Say {
                leave: Some(A),
                arrive: None
            }
        );
        // And from then on, nothing more.
        assert!(gaze(&mut t, t0 + Duration::from_secs(3), 2000, None).is_empty());
    }

    #[test]
    fn going_from_a_monitor_of_one_pc_to_one_of_another_tells_both() {
        let t0 = Instant::now();
        let mut t = Told::default();
        gaze(&mut t, t0, 500, at(A, 1));
        let said = gaze(&mut t, t0 + Duration::from_secs(1), 1000, at(B, 7));
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].1.leave, Some(A));
        assert_eq!(said[0].1.arrive.map(|a| a.0), Some(key(B, 7)));
        // Between two monitors of one PC, the new one says it all.
        let said = gaze(&mut t, t0 + Duration::from_secs(3), 1000, at(B, 8));
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].1.leave, None);
    }

    #[test]
    fn a_new_session_is_told_again() {
        let t0 = Instant::now();
        let mut t = Told::default();
        assert_eq!(gaze(&mut t, t0, 500, at(A, 1)).len(), 1);
        t.forget(B);
        assert!(gaze(&mut t, t0 + Duration::from_secs(1), 500, at(A, 1)).is_empty());
        t.forget(A);
        let said = gaze(&mut t, t0 + Duration::from_secs(2), 500, at(A, 1));
        assert_eq!(
            said.len(),
            1,
            "the PC that came back hears where the gaze is"
        );
    }
}
