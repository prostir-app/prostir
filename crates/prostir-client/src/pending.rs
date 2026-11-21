//! What the headset waits for from the PC: the menu's requests, each answered once
//! (or given up on), and a profile being switched to, until its monitors stand.
//!
//! Plain data with the time passed in, so the rules are tested on the PC: numbering
//! per session, answers to one's own requests and to none, a command of the PC that
//! waits for a request's answer, the 30 s limit, a disconnect, and a profile that
//! places its monitors by their numbers or, failing that, within a time limit.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use prostir_proto::request::Outcome;
use prostir_proto::state::ReqId;

use crate::ids::DisplayKey;
use crate::layout::Placement;
use crate::ui::Asked;

/// A request without an answer for this long counts as failed (a late answer is let go).
pub const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// How long the menu shows a failure under its button (see `note`).
pub const FAILURE_SHOWN_FOR: Duration = Duration::from_secs(8);
/// A profile whose monitors do not all stream (or fail) this long after the PC made
/// them is placed with what streams then, so the arrangement is never held for ever.
pub const PROFILE_STANDS_WITHIN: Duration = Duration::from_secs(8);

/// The menu's requests of this session.
#[derive(Debug, Default)]
pub struct Asks {
    /// Waiting for an answer: which control asked, and when.
    waiting: BTreeMap<ReqId, (Asked, Instant)>,
    /// The number of the last request this session.
    last: u32,
    /// Commands of the PC answered with the answer to one of these requests (a
    /// profile switched to from the PC's tray).
    command_after: BTreeMap<ReqId, ReqId>,
}

/// What an answer comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    /// The control that asked; `None` for a request nothing waits for (answered
    /// after it was given up on, or never asked).
    pub what: Option<Asked>,
    /// How long the answer took.
    pub took: Option<Duration>,
    /// A command of the PC to answer now: its number and its outcome (`Done`, or the
    /// failure as it came).
    pub command: Option<(ReqId, Outcome)>,
}

impl Asks {
    /// A new session: numbers start from 1 again.
    pub fn new_session(&mut self) {
        self.last = 0;
    }

    /// Control `what` asks, at `now`: the number its request goes with.
    pub fn ask(&mut self, what: Asked, now: Instant) -> ReqId {
        self.last += 1;
        let req = ReqId(self.last);
        self.waiting.insert(req, (what, now));
        req
    }

    /// The number of the last request of this session.
    pub fn last(&self) -> ReqId {
        ReqId(self.last)
    }

    /// Command `command` of the PC is answered with the answer to request `req`.
    pub fn answer_command_after(&mut self, req: ReqId, command: ReqId) {
        self.command_after.insert(req, command);
    }

    /// Request `req` was answered with `outcome`, at `now`.
    pub fn answered(&mut self, req: ReqId, outcome: &Outcome, now: Instant) -> Answered {
        let command = self.command_after.remove(&req).map(|command| {
            let outcome = match outcome {
                Outcome::Failed { .. } => outcome.clone(),
                _ => Outcome::Done,
            };
            (command, outcome)
        });
        let Some((what, at)) = self.waiting.remove(&req) else {
            return Answered {
                what: None,
                took: None,
                command,
            };
        };
        Answered {
            what: Some(what),
            took: Some(now.saturating_duration_since(at)),
            command,
        }
    }

    /// Requests without an answer for [`ANSWER_WITHIN`] fail as timed out: their
    /// numbers and controls.
    pub fn expire(&mut self, now: Instant) -> Vec<(ReqId, Asked)> {
        let late: Vec<ReqId> = self
            .waiting
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) >= ANSWER_WITHIN)
            .map(|(req, _)| *req)
            .collect();
        late.into_iter()
            .filter_map(|req| Some((req, self.waiting.remove(&req)?.0)))
            .collect()
    }

    /// The connection broke: nothing waiting gets an answer, nor do the PC's
    /// commands. The controls that waited.
    pub fn disconnected(&mut self) -> Vec<Asked> {
        self.command_after.clear();
        std::mem::take(&mut self.waiting)
            .into_values()
            .map(|(what, _)| what)
            .collect()
    }

    /// Whether control `what` waits for an answer.
    pub fn waits(&self, what: Asked) -> bool {
        self.waiting.values().any(|(w, _)| *w == what)
    }
}

/// A profile being switched to: its request, its places by its monitors' order (as
/// saved: see `profiles::standing`), and the numbers the PC gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSwitch {
    pub req: ReqId,
    pub places: Vec<Placement>,
    /// The frame the places are in (none: in the room) and the rows they were in, as
    /// the profile keeps them (see `profiles::Profile`).
    pub frame: Option<crate::profiles::ProfileFrame>,
    pub rows: Vec<Vec<usize>>,
    pub ids: Option<Vec<DisplayKey>>,
    /// Until when it waits for its monitors to stream, once the PC made them.
    pub until: Option<Instant>,
}

/// What to do once the PC made a profile's monitors.
#[derive(Debug, Clone, PartialEq)]
pub enum Made {
    /// Each monitor at its place, now.
    Places(Vec<(DisplayKey, Placement)>),
    /// The profile has no place for each (saved before they all had one): laid out by
    /// the preset once they stream (see [`ProfileSwitch::stands`]).
    Wait,
}

impl ProfileSwitch {
    pub fn new(req: ReqId, places: Vec<Placement>) -> Self {
        Self {
            req,
            places,
            frame: None,
            rows: Vec::new(),
            ids: None,
            until: None,
        }
    }

    /// The PC answered with the monitors' numbers (`ids`, in the order of the
    /// profile's sizes), at `now`.
    pub fn made(&mut self, ids: Vec<DisplayKey>, now: Instant) -> Made {
        if self.places.len() == ids.len() {
            return Made::Places(ids.into_iter().zip(self.places.clone()).collect());
        }
        self.ids = Some(ids);
        self.until = Some(now + PROFILE_STANDS_WITHIN);
        Made::Wait
    }

    /// Whether to lay it out now: once every monitor it made has `settled` (streams,
    /// or failed and keeps its place), or once [`PROFILE_STANDS_WITHIN`] passed, with
    /// what streams then.
    pub fn stands(&self, now: Instant, settled: impl Fn(DisplayKey) -> bool) -> bool {
        let Some(ids) = &self.ids else {
            return false;
        };
        ids.iter().all(|id| settled(*id)) || self.until.is_some_and(|t| now >= t)
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;
    use prostir_proto::state::ErrorCode;

    /// Numbers count from 1 in each session; an answer to one's own request says
    /// which control asked and how long it took; one nobody waits for says nothing.
    #[test]
    fn answers_find_their_request() {
        let t = Instant::now();
        let mut asks = Asks::default();
        let a = asks.ask(Asked::AddMonitor, t);
        let b = asks.ask(Asked::Sound, t);
        assert_eq!((a, b), (ReqId(1), ReqId(2)));
        assert!(asks.waits(Asked::AddMonitor));
        let later = t + Duration::from_millis(40);
        let got = asks.answered(b, &Outcome::Done, later);
        assert_eq!(got.what, Some(Asked::Sound));
        assert_eq!(got.took, Some(Duration::from_millis(40)));
        assert_eq!(got.command, None);
        // Twice, or a number never asked: nothing waits for it.
        assert_eq!(asks.answered(b, &Outcome::Done, later).what, None);
        assert_eq!(asks.answered(ReqId(77), &Outcome::Done, later).what, None);
        // A failure is an answer too (the menu says it under the button).
        let failed = Outcome::failed(ErrorCode::LIMIT, "6");
        assert_eq!(
            asks.answered(a, &failed, later).what,
            Some(Asked::AddMonitor)
        );
        assert!(!asks.waits(Asked::AddMonitor));
        assert_eq!(asks.ask(Asked::AddMonitor, later), ReqId(3));
        assert!(asks.waits(Asked::AddMonitor));
        asks.new_session();
        assert_eq!(asks.ask(Asked::Mirror, later), ReqId(1));
    }

    /// A command of the PC waiting for a request's answer gets it: `Done` for any
    /// success, the failure as it came.
    #[test]
    fn a_command_gets_the_answer_of_its_request() {
        let t = Instant::now();
        let mut asks = Asks::default();
        let req = asks.ask(Asked::Profile(1), t);
        asks.answer_command_after(req, ReqId(9));
        let got = asks.answered(req, &Outcome::Ids { ids: vec![] }, t);
        assert_eq!(got.command, Some((ReqId(9), Outcome::Done)));
        let req = asks.ask(Asked::Profile(2), t);
        asks.answer_command_after(req, ReqId(10));
        let failed = Outcome::failed(ErrorCode::DESKTOP, "the desktop refused");
        let got = asks.answered(req, &failed, t);
        assert_eq!(got.command, Some((ReqId(10), failed)));
    }

    /// No answer within 30 s fails the request as timed out, and a late answer is let
    /// go; a disconnect fails everything waiting, and the PC's commands with it.
    #[test]
    fn late_answers_and_a_disconnect() {
        let t = Instant::now();
        let mut asks = Asks::default();
        let slow = asks.ask(Asked::Screenshot, t);
        let quick = asks.ask(Asked::Sound, t + Duration::from_secs(20));
        assert!(asks.expire(t + Duration::from_secs(29)).is_empty());
        let gone = asks.expire(t + ANSWER_WITHIN);
        assert_eq!(gone, [(slow, Asked::Screenshot)]);
        assert_eq!(
            asks.answered(slow, &Outcome::Done, t + ANSWER_WITHIN).what,
            None,
            "too late"
        );
        assert!(asks.waits(Asked::Sound));
        assert!(!asks.waits(Asked::Screenshot));
        asks.answer_command_after(quick, ReqId(5));
        let now = t + Duration::from_secs(21);
        assert_eq!(asks.disconnected(), [Asked::Sound]);
        assert!(!asks.waits(Asked::Sound));
        assert_eq!(asks.answered(quick, &Outcome::Done, now).command, None);
    }

    fn key(n: u32) -> DisplayKey {
        DisplayKey::new(crate::ids::PcId([1; 8]), prostir_proto::state::DisplayId(n))
    }

    fn place(x: f32) -> Placement {
        Placement {
            position: glam::Vec3::new(x, 0.0, -1.0),
            rotation: glam::Quat::IDENTITY,
            width: 0.5,
        }
    }

    /// A profile with a place for each monitor puts each at its place by the numbers
    /// the PC gave, in the order of its sizes.
    #[test]
    fn a_profile_takes_its_numbers() {
        let t = Instant::now();
        let mut switch = ProfileSwitch::new(ReqId(4), vec![place(-1.0), place(1.0)]);
        assert!(!switch.stands(t, |_| true), "not made yet");
        assert_eq!(
            switch.made(vec![key(107), key(103)], t),
            Made::Places(vec![(key(107), place(-1.0)), (key(103), place(1.0))])
        );
    }

    /// A profile without places waits for its monitors to stream or fail, but not for
    /// ever: once the time is up it is laid out with what streams.
    #[test]
    fn a_profile_without_places_stands_in_time() {
        let t = Instant::now();
        let mut switch = ProfileSwitch::new(ReqId(4), Vec::new());
        assert_eq!(switch.made(vec![key(107), key(103)], t), Made::Wait);
        let only_107 = |id: DisplayKey| id == key(107);
        assert!(!switch.stands(t + Duration::from_secs(1), only_107));
        assert!(switch.stands(t + Duration::from_secs(1), |_| true));
        assert!(
            switch.stands(t + PROFILE_STANDS_WITHIN, only_107),
            "the time is up"
        );
    }
}
