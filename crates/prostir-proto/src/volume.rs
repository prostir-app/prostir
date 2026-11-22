//! One volume for the PC's slider and the headset's buttons, without echoes.
//!
//! The headset owns the level: its system volume is what plays. The PC's sound device
//! for the headset ("Prostir: headset") follows it, and its slider asks the headset to
//! change it. Two counters keep the two sides from fighting:
//!
//! - the headset counts only its own changes (`seq`: buttons, the system menu);
//! - the PC counts its requests (`ask`), and each [`Volume`] the headset sends repeats
//!   the request it answers (0 for a change of its own).
//!
//! The headset takes a request only when the PC had seen all of its own changes
//! (`seen >= seq`); otherwise its own level stands and it says so. The PC writes the
//! headset's level to its device only when no newer request is on its way and the
//! slider has rested a moment, so a slider being dragged never jumps back; a device
//! change equal to what the PC wrote itself is its echo and asks nothing.
//!
//! Both sides are plain state with the time passed in, so the fake headset and the
//! tests drive them as the apps do.

use std::time::{Duration, Instant};

use crate::state::{Volume, VolumeAsk};

/// While the slider moves, the PC asks at most this often, with the newest level; and
/// it writes the headset's answer once the slider rested this long.
pub const ASK_EVERY: Duration = Duration::from_millis(50);
/// A level the PC wrote, give or take this, coming back from the device is its echo.
const ECHO: u8 = 1;

/// The headset's side.
#[derive(Debug, Clone)]
pub struct HeadsetVolume {
    level: u8,
    muted: bool,
    /// Changes made on the headset so far.
    seq: u32,
}

impl HeadsetVolume {
    pub fn new(level: u8, muted: bool) -> Self {
        Self {
            level: level.min(100),
            muted,
            seq: 0,
        }
    }

    /// The level now, as `Open` carries it.
    pub fn now(&self) -> Volume {
        Volume {
            level: self.level,
            muted: self.muted,
            seq: self.seq,
            ask: 0,
        }
    }

    /// The buttons (or the system menu) changed the volume: what to tell the PC, if it
    /// changed at all.
    pub fn changed_here(&mut self, level: u8, muted: bool) -> Option<Volume> {
        let level = level.min(100);
        if (level, muted) == (self.level, self.muted) {
            return None;
        }
        (self.level, self.muted) = (level, muted);
        self.seq += 1;
        Some(self.now())
    }

    /// The PC asks for `ask`'s level. Taken when the PC had seen every change made
    /// here: `set` puts the system volume as near as it can (a step of the headset)
    /// and gives what it became. The answer goes to the PC either way, with the level
    /// that stands.
    pub fn asked(&mut self, ask: &VolumeAsk, set: impl FnOnce(u8, bool) -> (u8, bool)) -> Volume {
        if ask.seen >= self.seq {
            let (level, muted) = set(ask.level.min(100), ask.muted);
            (self.level, self.muted) = (level.min(100), muted);
        }
        Volume {
            ask: ask.ask,
            ..self.now()
        }
    }
}

/// What the PC does next about the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Send this request to the headset.
    Ask(VolumeAsk),
    /// Put the device at this level.
    Write { level: u8, muted: bool },
}

/// The PC's side, for one session.
#[derive(Debug, Clone, Default)]
pub struct PcVolume {
    /// What the PC last wrote to the device, while the device has not moved since.
    written: Option<(u8, bool)>,
    /// The newest change of the headset's own that came.
    seen: u32,
    /// Requests sent, and the newest one answered.
    asked: u32,
    answered: u32,
    /// The device moved and the headset was not asked yet (sent at [`ASK_EVERY`]).
    waiting: Option<(u8, bool)>,
    /// The headset's level, to write once the device rested.
    answer: Option<(u8, bool)>,
    last_ask: Option<Instant>,
    last_move: Option<Instant>,
}

impl PcVolume {
    /// A [`Volume`] came from the headset (in `Open`, or since): what to do now.
    pub fn from_headset(&mut self, v: &Volume, now: Instant) -> Option<Act> {
        self.seen = self.seen.max(v.seq);
        self.answered = self.answered.max(v.ask);
        // A newer request is on its way: this level is already old.
        if self.answered < self.asked || self.waiting.is_some() {
            return None;
        }
        self.answer = Some((v.level.min(100), v.muted));
        self.tick(now)
    }

    /// The device changed (the slider, the volume keys): what to do now.
    pub fn device(&mut self, level: u8, muted: bool, now: Instant) -> Option<Act> {
        if let Some((l, m)) = self.written
            && m == muted
            && l.abs_diff(level) <= ECHO
        {
            return None;
        }
        self.written = None;
        self.answer = None;
        self.waiting = Some((level.min(100), muted));
        self.last_move = Some(now);
        self.tick(now)
    }

    /// The PC puts the device at `level` itself (its settings window's slider): asked
    /// like a move of the device, whose echo of the level then asks nothing more.
    pub fn set_here(&mut self, level: u8, muted: bool, now: Instant) -> Option<Act> {
        let level = level.min(100);
        // The device left any level written before, even one within an echo of this.
        self.written = None;
        let act = self.device(level, muted, now);
        self.written = Some((level, muted));
        act
    }

    /// What is due at `now`: a waiting request once [`ASK_EVERY`] passed since the
    /// last one, or the headset's level once the device rested that long.
    pub fn tick(&mut self, now: Instant) -> Option<Act> {
        let since =
            |t: Option<Instant>| t.is_none_or(|t| now.saturating_duration_since(t) >= ASK_EVERY);
        if let Some((level, muted)) = self.waiting {
            if !since(self.last_ask) {
                return None;
            }
            self.waiting = None;
            self.asked += 1;
            self.last_ask = Some(now);
            return Some(Act::Ask(VolumeAsk {
                level,
                muted,
                seen: self.seen,
                ask: self.asked,
            }));
        }
        if self.answer.is_some() && since(self.last_move) {
            let (level, muted) = self.answer.take()?;
            self.written = Some((level, muted));
            return Some(Act::Write { level, muted });
        }
        None
    }

    /// When [`PcVolume::tick`] has something to do next, if ever.
    pub fn due(&self) -> Option<Instant> {
        let after = |t: Option<Instant>| t.map(|t| t + ASK_EVERY);
        if self.waiting.is_some() {
            return Some(after(self.last_ask).unwrap_or_else(Instant::now));
        }
        self.answer
            .map(|_| after(self.last_move).unwrap_or_else(Instant::now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Quest's volume has 16 steps: a level becomes the nearest one.
    fn quest_step(level: u8) -> u8 {
        let step = (f32::from(level) * 15.0 / 100.0).round();
        (step * 100.0 / 15.0).round() as u8
    }

    fn set_step(level: u8, muted: bool) -> (u8, bool) {
        (quest_step(level), muted)
    }

    /// A PC device, a headset and the network between them, in steps of 1 ms.
    struct World {
        start: Instant,
        ms: u64,
        pc: PcVolume,
        headset: HeadsetVolume,
        /// The PC's device: its level, and every level the PC wrote to it.
        device: (u8, bool),
        writes: Vec<(u64, u8)>,
        /// In flight: arrival time, and the message (to the headset or the PC).
        to_headset: Vec<(u64, VolumeAsk)>,
        to_pc: Vec<(u64, Volume)>,
        /// The device reports a change this long after it.
        device_events: Vec<(u64, (u8, bool))>,
        latency: u64,
        messages: Vec<u64>,
    }

    impl World {
        fn new(latency: u64, level: u8) -> Self {
            let mut w = Self {
                start: Instant::now(),
                ms: 0,
                pc: PcVolume::default(),
                headset: HeadsetVolume::new(level, false),
                device: (100, false),
                writes: Vec::new(),
                to_headset: Vec::new(),
                to_pc: Vec::new(),
                device_events: Vec::new(),
                latency,
                messages: Vec::new(),
            };
            // The session opens: the headset's level comes in `Open`.
            let open = w.headset.now();
            w.to_pc.push((latency, open));
            w
        }

        fn now(&self) -> Instant {
            self.start + Duration::from_millis(self.ms)
        }

        fn act(&mut self, act: Option<Act>) {
            match act {
                Some(Act::Ask(ask)) => {
                    self.messages.push(self.ms);
                    self.to_headset.push((self.ms + self.latency, ask));
                }
                Some(Act::Write { level, muted }) => {
                    self.writes.push((self.ms, level));
                    self.device = (level, muted);
                    self.device_events.push((self.ms + 2, (level, muted)));
                }
                None => {}
            }
        }

        /// The person moves the slider (or presses the volume keys) on the PC.
        fn slide(&mut self, level: u8) {
            self.device = (level, false);
            self.device_events.push((self.ms + 2, (level, false)));
        }

        fn buttons(&mut self, level: u8) {
            if let Some(v) = self.headset.changed_here(level, false) {
                self.messages.push(self.ms);
                self.to_pc.push((self.ms + self.latency, v));
            }
        }

        fn run(&mut self, until: u64) {
            while self.ms < until {
                self.ms += 1;
                let now = self.ms;
                let due: Vec<(u8, bool)> = take_due(&mut self.device_events, now);
                for (level, muted) in due {
                    let act = self.pc.device(level, muted, self.now());
                    self.act(act);
                }
                for ask in take_due(&mut self.to_headset, now) {
                    let v = self.headset.asked(&ask, set_step);
                    self.messages.push(now);
                    self.to_pc.push((now + self.latency, v));
                }
                for v in take_due(&mut self.to_pc, now) {
                    let act = self.pc.from_headset(&v, self.now());
                    self.act(act);
                }
                let act = self.pc.tick(self.now());
                self.act(act);
            }
        }
    }

    fn take_due<T>(queue: &mut Vec<(u64, T)>, now: u64) -> Vec<T> {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(queue)
            .into_iter()
            .partition(|(t, _)| *t <= now);
        *queue = later;
        due.into_iter().map(|(_, m)| m).collect()
    }

    #[test]
    fn the_session_starts_at_the_headsets_level() {
        let mut w = World::new(5, 40);
        w.run(300);
        assert_eq!(w.device, (40, false));
        assert_eq!(w.writes.len(), 1);
        assert!(w.messages.is_empty(), "the echo asks nothing");
    }

    #[test]
    fn the_buttons_reach_the_device_without_an_echo() {
        let mut w = World::new(5, 40);
        w.run(100);
        w.buttons(53);
        w.run(400);
        assert_eq!(w.device.0, 53);
        assert_eq!(w.messages.len(), 1, "one Volume and nothing back");
    }

    #[test]
    fn the_slider_reaches_the_headset_at_its_step() {
        let mut w = World::new(5, 40);
        w.run(100);
        w.slide(71);
        w.run(500);
        assert_eq!(w.headset.now().level, quest_step(71));
        assert_eq!(w.device.0, quest_step(71), "the slider stands on the step");
        // One request, one answer.
        assert_eq!(w.messages.len(), 2);
    }

    /// 50 changes in a second: the device is never written while the slider moves,
    /// ends on the headset's step, few messages go, and none once it rests.
    #[test]
    fn dragging_the_slider_does_not_jump_back() {
        for latency in [1, 5, 30, 120] {
            let mut w = World::new(latency, 20);
            w.run(300);
            let writes_before = w.writes.len();
            let messages_before = w.messages.len();
            for i in 0..50u64 {
                w.slide((i * 2) as u8);
                w.run(300 + (i + 1) * 20);
            }
            let dragged_until = w.ms;
            w.run(dragged_until + 1000);
            let during: Vec<_> = w.writes[writes_before..]
                .iter()
                .filter(|(t, _)| *t < dragged_until)
                .collect();
            assert!(
                during.is_empty(),
                "written while dragging ({latency} ms): {during:?}"
            );
            assert_eq!(w.device.0, quest_step(98), "at the step ({latency} ms)");
            assert_eq!(w.headset.now().level, quest_step(98));
            let sent = w.messages.len() - messages_before;
            assert!(sent <= 2 * 22, "{sent} messages ({latency} ms)");
            let last = *w.messages.last().unwrap();
            assert!(
                last < dragged_until + 200 + 2 * latency,
                "quiet after ({latency} ms)"
            );
        }
    }

    /// The PC's settings window sets the level: one request, even a step of 1 from
    /// what was written last, and the device's echo of it asks nothing more.
    #[test]
    fn the_pc_sets_the_level_itself() {
        let mut w = World::new(5, 40);
        w.run(100);
        for level in [41, 71] {
            let before = w.messages.len();
            let act = w.pc.set_here(level, false, w.now());
            w.act(act);
            w.device = (level, false);
            w.device_events.push((w.ms + 2, (level, false)));
            w.run(w.ms + 400);
            assert_eq!(w.headset.now().level, quest_step(level));
            assert_eq!(w.device.0, quest_step(level), "the device on the step");
            assert_eq!(w.messages.len() - before, 2, "one request, one answer");
        }
    }

    /// The PC numbers its requests 1, 2, ...: an answer to an older one, while a
    /// newer one is on its way, is not written to the device.
    #[test]
    fn requests_are_numbered_and_an_old_answer_waits() {
        let start = Instant::now();
        let mut pc = PcVolume::default();
        let ask = |act: Option<Act>| match act {
            Some(Act::Ask(ask)) => ask,
            other => panic!("not a request: {other:?}"),
        };
        let first = ask(pc.device(30, false, start));
        assert_eq!(first.ask, 1);
        let later = start + ASK_EVERY;
        let second = ask(pc.device(60, false, later));
        assert_eq!(second.ask, 2);
        let answer = |level, ask| Volume {
            level,
            muted: false,
            seq: 0,
            ask,
        };
        let rested = later + 2 * ASK_EVERY;
        assert_eq!(pc.from_headset(&answer(27, 1), rested), None, "old");
        assert_eq!(pc.tick(rested), None);
        assert_eq!(
            pc.from_headset(&answer(60, 2), rested),
            Some(Act::Write {
                level: 60,
                muted: false
            })
        );
    }

    /// `due` says when `tick` has something to do: a request that waits for
    /// [`ASK_EVERY`] since the last one, the headset's level once the device rested
    /// that long, and nothing when all is done.
    #[test]
    fn due_is_when_the_next_tick_acts() {
        let start = Instant::now();
        let mut pc = PcVolume::default();
        assert_eq!(pc.due(), None, "nothing to do");
        assert!(matches!(pc.device(30, false, start), Some(Act::Ask(_))));
        assert_eq!(pc.due(), None, "asked, nothing waits");
        let moved = start + Duration::from_millis(10);
        assert_eq!(pc.device(40, false, moved), None, "too soon to ask again");
        assert_eq!(pc.due(), Some(start + ASK_EVERY), "a request waits");
        assert!(matches!(pc.tick(start + ASK_EVERY), Some(Act::Ask(_))));
        let answer = Volume {
            level: 40,
            muted: false,
            seq: 0,
            ask: 2,
        };
        let answered = start + ASK_EVERY + Duration::from_millis(5);
        assert_eq!(pc.from_headset(&answer, answered), None, "still moving");
        assert_eq!(pc.due(), Some(moved + ASK_EVERY), "the level waits");
        assert!(matches!(
            pc.tick(moved + ASK_EVERY),
            Some(Act::Write { .. })
        ));
        assert_eq!(pc.due(), None, "all done");
    }

    /// The slider and the buttons at the same moment: the headset's level wins.
    #[test]
    fn at_the_same_time_the_headset_wins() {
        let mut w = World::new(30, 40);
        w.run(200);
        w.slide(90);
        w.run(205);
        w.buttons(13);
        w.run(1000);
        assert_eq!(w.headset.now().level, 13);
        assert_eq!(w.device.0, 13);
    }

    /// A change the PC knew of before it asked is older than its request: the
    /// request is taken.
    #[test]
    fn a_request_after_a_seen_change_is_taken() {
        let mut w = World::new(5, 40);
        w.run(100);
        w.buttons(60);
        w.run(300);
        w.slide(20);
        w.run(600);
        assert_eq!(w.headset.now().level, quest_step(20));
        assert_eq!(w.device.0, quest_step(20));
    }
}
