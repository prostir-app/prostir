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
//! slider has rested a moment, so a slider being dragged never jumps back. The first
//! change of the device after the PC wrote it, to exactly that level and soon, is the
//! write's echo and asks nothing; so is a change to where the device already was.
//! Every other change is the person's, a 1 % one too (the sound system reads back the
//! percent it was given: `pactl` takes and says whole percent).
//!
//! The level on the wire is in percent; the headset's system counts its volume in
//! steps of its own ([`Steps`]), which only the headset knows. A request between two
//! steps goes to the next step the way the slider moved, so a slider moved by 1 % does
//! not stay stuck between two steps.
//!
//! Both sides are plain state with the time passed in, so the fake headset and the
//! tests drive them as the apps do.

use std::time::{Duration, Instant};

use crate::state::{Volume, VolumeAsk};

/// While the slider moves, the PC asks at most this often, with the newest level; and
/// it writes the headset's answer once the slider rested this long.
pub const ASK_EVERY: Duration = Duration::from_millis(50);
/// The level the PC wrote, coming back from the device this soon as its first change
/// after the write, is the write's echo. Later it is taken as a move to the level the
/// headset already has: one request more, and nothing changes.
pub const ECHO_WITHIN: Duration = Duration::from_secs(1);
/// Requests closer together than this are one move of the slider (the PC asks every
/// [`ASK_EVERY`] while it moves, and writes the answer only once it rested): each goes
/// on from where the one before asked, not from the step that one became.
pub const ONE_MOVE: Duration = Duration::from_millis(100);

/// Step `index` of a volume of `steps` above silence, in percent.
pub fn level_of(index: u8, steps: u8) -> u8 {
    let steps = u32::from(steps.max(1));
    let index = u32::from(index).min(steps);
    ((index * 200 + steps) / (2 * steps)) as u8
}

/// The step of a volume of `steps` above silence nearest `level` percent.
pub fn index_of(level: u8, steps: u8) -> u8 {
    let steps = u32::from(steps.max(1));
    let level = u32::from(level.min(100));
    ((level * steps * 2 + 100) / 200) as u8
}

/// The level of the step nearest `level`, of a volume of `steps` above silence.
pub fn step(level: u8, steps: u8) -> u8 {
    level_of(index_of(level, steps), steps)
}

/// Where a request for `asked` goes from `from` (both in percent), on a volume of
/// `steps` above silence: higher, to the step at or above it and at least one above
/// `from`'s; lower, to the step at or below it and at least one below; the same,
/// nowhere (`None`).
pub fn toward(from: u8, asked: u8, steps: u8) -> Option<u8> {
    let s = u32::from(steps.max(1));
    let (from, asked) = (u32::from(from.min(100)), u32::from(asked.min(100)));
    let index = if asked > from {
        let up = (asked * s).div_ceil(100);
        up.max(from * s / 100 + 1).min(s)
    } else if asked < from {
        let down = asked * s / 100;
        down.min((from * s).div_ceil(100).saturating_sub(1))
    } else {
        return None;
    };
    Some(level_of(index as u8, steps))
}

/// How a headset whose system volume goes in steps takes the PC's requests: each to
/// the next step the way it moved ([`toward`]). A request soon after one taken
/// ([`ONE_MOVE`]) goes on from the level that one asked for, while the headset stands
/// where it put it: while the slider is dragged, the headset is a step ahead of it, and
/// the next request, a little further on, must not go back.
#[derive(Debug, Clone)]
pub struct Steps {
    steps: u8,
    /// The last request taken: the level asked for, the level it became, and when.
    last: Option<(u8, u8, Instant)>,
}

impl Steps {
    /// A volume of `steps` above silence (1 at least).
    pub fn new(steps: u8) -> Self {
        Self {
            steps: steps.max(1),
            last: None,
        }
    }

    pub fn steps(&self) -> u8 {
        self.steps
    }

    /// The level a request for `asked` puts the headset at, now at `level`; `now` is
    /// when the request came.
    pub fn take(&mut self, level: u8, asked: u8, now: Instant) -> u8 {
        let from = match self.last {
            Some((asked_before, became, at))
                if became == level && now.saturating_duration_since(at) < ONE_MOVE =>
            {
                asked_before
            }
            _ => level,
        };
        let to = toward(from, asked, self.steps).unwrap_or(level);
        self.last = Some((asked.min(100), to, now));
        to
    }
}

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

/// The person's moves of the device this close together are one gesture: the next
/// may go on from the one before, whatever the PC wrote between (see
/// [`PcVolume::device`]).
pub const ONE_GESTURE: Duration = Duration::from_secs(1);

/// The PC's side, for one session.
#[derive(Debug, Clone, Default)]
pub struct PcVolume {
    /// Where the device was last said to be.
    known: Option<(u8, bool)>,
    /// The person's last move of the device, and when.
    own: Option<(u8, Instant)>,
    /// The headset's level, as it last said.
    headset: Option<(u8, bool)>,
    /// What the PC wrote to the device and when, until the device's next change.
    echo: Option<(u8, bool, Instant)>,
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
        self.headset = Some((v.level.min(100), v.muted));
        // A newer request is on its way: this level is already old.
        if self.answered < self.asked || self.waiting.is_some() {
            return None;
        }
        self.answer = Some((v.level.min(100), v.muted));
        self.tick(now)
    }

    /// The device changed (the slider, the volume keys): what to do now.
    ///
    /// The person's move asks the headset only once it passed the headset's level the
    /// way it went (up: above it, down: below it); short of it, the device goes back to
    /// the headset's step once it rests. The way is from the person's last move of the
    /// same gesture ([`ONE_GESTURE`]), or from where the device stood, whichever is
    /// nearer: a slider dragged slowly goes on from under the mouse though the PC wrote
    /// the headset's step meanwhile, a key goes on from the step written. So a slow
    /// drag never asks below the step the headset went up to, nor flips between two
    /// steps.
    pub fn device(&mut self, level: u8, muted: bool, now: Instant) -> Option<Act> {
        let level = level.min(100);
        if self.known == Some((level, muted)) {
            // Said again, nothing moved: the echo, if it was this level, came.
            self.echo.take_if(|(l, m, _)| (*l, *m) == (level, muted));
            return None;
        }
        let stood = self.known.replace((level, muted)).map(|(l, _)| l);
        if self.echo.take().is_some_and(|(l, m, at)| {
            (l, m) == (level, muted) && now.saturating_duration_since(at) < ECHO_WITHIN
        }) {
            return None;
        }
        let own = self
            .own
            .filter(|(l, at)| *l != level && now.saturating_duration_since(*at) < ONE_GESTURE)
            .map(|(l, _)| l);
        let from = match (own, stood) {
            (Some(o), Some(s)) if s.abs_diff(level) < o.abs_diff(level) => Some(s),
            (Some(o), _) => Some(o),
            (None, s) => s,
        };
        self.own = Some((level, now));
        self.last_move = Some(now);
        // With a request on its way, the headset's level is not known yet.
        let settled = self.answered >= self.asked;
        let passes = match self.headset {
            Some((h, h_muted)) if settled => {
                h_muted != muted
                    || match from.map(|f| level.cmp(&f)) {
                        Some(std::cmp::Ordering::Greater) => level > h,
                        Some(std::cmp::Ordering::Less) => level < h,
                        _ => level != h,
                    }
            }
            _ => true,
        };
        if passes {
            self.answer = None;
            self.waiting = Some((level, muted));
        } else {
            self.waiting = None;
            self.answer = self.headset;
        }
        self.tick(now)
    }

    /// The PC puts the device at `level` itself (its settings window's slider): asked
    /// like a move of the device, whose echo of the level then asks nothing more.
    pub fn set_here(&mut self, level: u8, muted: bool, now: Instant) -> Option<Act> {
        let level = level.min(100);
        // Not the echo of a level written before, even one within an echo of this.
        self.echo = None;
        let act = self.device(level, muted, now);
        self.echo = Some((level, muted, now));
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
            // A device already there does not change, and says nothing back.
            self.echo = (self.known != Some((level, muted))).then_some((level, muted, now));
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

    /// The steps the tests run with: the first headset's (15 above silence, 16 levels
    /// with silence), and a volume in whole percent.
    const STEPS: [u8; 2] = [15, 100];

    /// The level a request for `asked` from `from` ends at, on `steps`.
    fn to(from: u8, asked: u8, steps: u8) -> u8 {
        toward(from, asked, steps).unwrap_or(from)
    }

    /// A PC device, a headset and the network between them, in steps of 1 ms.
    struct World {
        start: Instant,
        ms: u64,
        pc: PcVolume,
        headset: HeadsetVolume,
        steps: Steps,
        /// The headset's level after each request it answered.
        answers: Vec<u8>,
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
        /// The device says a write this long after it.
        echo_after: u64,
    }

    impl World {
        fn new(latency: u64, level: u8, steps: u8) -> Self {
            let mut w = Self {
                start: Instant::now(),
                ms: 0,
                pc: PcVolume::default(),
                headset: HeadsetVolume::new(level, false),
                steps: Steps::new(steps),
                answers: Vec::new(),
                device: (100, false),
                writes: Vec::new(),
                to_headset: Vec::new(),
                to_pc: Vec::new(),
                device_events: Vec::new(),
                latency,
                messages: Vec::new(),
                echo_after: 2,
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
                    let at = self.ms + self.echo_after;
                    self.device_events.push((at, (level, muted)));
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
                    let level = self.headset.now().level;
                    let at = self.now();
                    let steps = &mut self.steps;
                    let v = self
                        .headset
                        .asked(&ask, |l, m| (steps.take(level, l, at), m));
                    self.answers.push(v.level);
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
        let mut w = World::new(5, 40, 15);
        w.run(300);
        assert_eq!(w.device, (40, false));
        assert_eq!(w.writes.len(), 1);
        assert!(w.messages.is_empty(), "the echo asks nothing");
    }

    #[test]
    fn the_buttons_reach_the_device_without_an_echo() {
        let mut w = World::new(5, 40, 15);
        w.run(100);
        w.buttons(53);
        w.run(400);
        assert_eq!(w.device.0, 53);
        assert_eq!(w.messages.len(), 1, "one Volume and nothing back");
    }

    #[test]
    fn the_slider_reaches_the_headset_at_its_step() {
        for steps in STEPS {
            let mut w = World::new(5, 40, steps);
            w.run(100);
            w.slide(71);
            w.run(500);
            let at = to(40, 71, steps);
            assert_eq!(w.headset.now().level, at, "{steps} steps");
            assert_eq!(w.device.0, at, "the slider stands on the step");
            // One request, one answer.
            assert_eq!(w.messages.len(), 2);
        }
        assert_eq!(to(40, 71, 15), 73, "the step above");
        assert_eq!(to(40, 71, 100), 71);
    }

    /// 50 changes in a second: the device is never written while the slider moves,
    /// the headset's level never goes back while it moves up, it ends on the step at
    /// or above the slider, few messages go, and none once it rests.
    #[test]
    fn dragging_the_slider_does_not_jump_back() {
        for steps in STEPS {
            for latency in [1, 5, 30, 120] {
                let mut w = World::new(latency, 20, steps);
                w.run(300);
                let writes_before = w.writes.len();
                let messages_before = w.messages.len();
                let answers_before = w.answers.len();
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
                let case = format!("{steps} steps, {latency} ms");
                assert!(
                    during.is_empty(),
                    "written while dragging ({case}): {during:?}"
                );
                // The first request is for 0, below where it stood; then up only.
                let answers = &w.answers[answers_before + 1..];
                assert!(
                    answers.windows(2).all(|a| a[0] <= a[1]),
                    "the headset went back ({case}): {answers:?}"
                );
                let at = to(97, 98, steps);
                assert_eq!(w.device.0, at, "at the step ({case})");
                assert_eq!(w.headset.now().level, at);
                let sent = w.messages.len() - messages_before;
                assert!(sent <= 2 * 22, "{sent} messages ({case})");
                let last = *w.messages.last().unwrap();
                assert!(
                    last < dragged_until + 200 + 2 * latency,
                    "quiet after ({case})"
                );
            }
            // Dragged slowly, 1 % every 120 or 200 ms, up and then down: the PC writes
            // the headset's step while the slider goes on (it rests long enough), yet
            // the headset never goes back the other way, nor does the device, and both
            // end on the step at or past where the slider stopped.
            for per in [120, 200] {
                for latency in [5, 30] {
                    let case = format!("{steps} steps, {per} ms per 1 %, {latency} ms");
                    let mut w = World::new(latency, 40, steps);
                    w.run(300);
                    for (to_level, d) in [(60u8, 1i16), (20, -1)] {
                        let answers = w.answers.len();
                        let writes = w.writes.len();
                        let mut slider = w.device.0;
                        while slider != to_level {
                            slider = (i16::from(slider) + d) as u8;
                            w.slide(slider);
                            w.run(w.ms + per);
                        }
                        w.run(w.ms + 1000);
                        let gone = |levels: Vec<u8>| {
                            levels
                                .windows(2)
                                .all(|p| i16::from(p[1]) * d >= i16::from(p[0]) * d)
                        };
                        let answered = w.answers[answers..].to_vec();
                        assert!(
                            gone(answered.clone()),
                            "headset back ({case}): {answered:?}"
                        );
                        let written: Vec<u8> = w.writes[writes..].iter().map(|w| w.1).collect();
                        assert!(gone(written.clone()), "device back ({case}): {written:?}");
                        let at = to((i16::from(to_level) - d) as u8, to_level, steps);
                        assert_eq!(w.headset.now().level, at, "{case}, to {to_level}");
                        assert_eq!(w.device.0, at, "{case}, to {to_level}");
                    }
                }
            }
        }
    }

    /// The slider moved by 1 % at a time, again and again, resting between moves (the
    /// volume keys of a desktop set to 1 %): every press asks the headset once, even
    /// the first after a level the PC wrote, and takes it a step on, never back, up to
    /// the top and down to silence.
    #[test]
    fn one_percent_at_a_time_is_never_stuck() {
        for steps in STEPS {
            // Pressed again after a rest, and soon (a person's fastest presses are
            // apart by more than `ONE_MOVE`; held, the key repeats faster, a drag).
            for rest in [300, 150] {
                let mut w = World::new(5, 40, steps);
                w.run(100);
                for d in [1i16, -1] {
                    let end = if d > 0 { 100 } else { 0 };
                    let mut level = w.headset.now().level;
                    let mut presses = 0;
                    while level != end {
                        let asks = w.answers.len();
                        let slider = (i16::from(w.device.0) + d).clamp(0, 100) as u8;
                        w.slide(slider);
                        w.run(w.ms + rest);
                        let now = w.headset.now().level;
                        let case = format!("{steps} steps, {rest} ms, {level} by {d}");
                        assert_eq!(w.answers.len(), asks + 1, "asked once ({case})");
                        assert!(i16::from(now) * d > i16::from(level) * d, "stuck ({case})");
                        assert_eq!(step(now, steps), now, "on a step ({case})");
                        assert_eq!(w.device.0, now, "the slider on the step ({case})");
                        level = now;
                        presses += 1;
                        assert!(presses <= 100, "{case}: too many presses");
                    }
                }
            }
        }
    }

    /// A write whose echo never comes on its own (the device was there already, or the
    /// sound system read it together with the person's next press): a 1 % press right
    /// after it is the person's, and asks.
    #[test]
    fn a_press_right_after_a_write_is_the_persons() {
        let start = Instant::now();
        let v = Volume {
            level: 40,
            muted: false,
            seq: 0,
            ask: 0,
        };
        let mut pc = PcVolume::default();
        let written = pc.from_headset(&v, start);
        assert_eq!(
            written,
            Some(Act::Write {
                level: 40,
                muted: false
            })
        );
        let soon = start + Duration::from_millis(10);
        assert!(matches!(pc.device(41, false, soon), Some(Act::Ask(_))));
    }

    /// The device says the PC's write at once, or late: that is the write's echo and
    /// asks nothing, nor does the same level said again; a press after it is the
    /// person's and asks.
    #[test]
    fn the_echo_of_a_write_asks_nothing() {
        for echo_after in [2, 400] {
            let case = format!("{echo_after} ms");
            let mut w = World::new(5, 40, 15);
            w.echo_after = echo_after;
            w.run(1000);
            w.buttons(53);
            w.run(2000);
            assert_eq!(w.writes.len(), 2, "{case}");
            assert_eq!(w.messages.len(), 1, "the buttons' Volume only ({case})");
            // The same level said again asks nothing either.
            w.device_events.push((w.ms + 1, w.device));
            w.run(w.ms + 300);
            assert_eq!(w.messages.len(), 1, "said again ({case})");
            let slider = w.device.0 + 1;
            w.slide(slider);
            w.run(w.ms + 300);
            assert_eq!(w.messages.len(), 3, "the press asked ({case})");
            assert!(w.headset.now().level > 53, "{case}");
        }
    }

    /// The PC's settings window sets the level: one request, even a step of 1 from
    /// what was written last, and the device's echo of it asks nothing more.
    #[test]
    fn the_pc_sets_the_level_itself() {
        for steps in STEPS {
            let mut w = World::new(5, 40, steps);
            w.run(100);
            for level in [41, 71] {
                let before = w.messages.len();
                let from = w.headset.now().level;
                let act = w.pc.set_here(level, false, w.now());
                w.act(act);
                w.device = (level, false);
                w.device_events.push((w.ms + 2, (level, false)));
                w.run(w.ms + 400);
                let at = to(from, level, steps);
                assert_eq!(w.headset.now().level, at);
                assert_eq!(w.device.0, at, "the device on the step");
                assert_eq!(w.messages.len() - before, 2, "one request, one answer");
            }
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
        let mut w = World::new(30, 40, 15);
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
        for steps in STEPS {
            let mut w = World::new(5, 40, steps);
            w.run(100);
            w.buttons(60);
            w.run(300);
            w.slide(20);
            w.run(600);
            assert_eq!(w.headset.now().level, to(60, 20, steps));
            assert_eq!(w.device.0, to(60, 20, steps));
        }
    }

    /// Percent and steps: each step's level comes back as that step, for any number
    /// of steps the system may have up to 100 (beyond, two steps may share a percent).
    #[test]
    fn steps_and_percent() {
        for steps in 1..=100u8 {
            for index in 0..=steps {
                assert_eq!(index_of(level_of(index, steps), steps), index, "{steps}");
            }
        }
        assert_eq!(level_of(7, 15), 47);
        assert_eq!(level_of(15, 15), 100);
        assert_eq!(index_of(46, 15), 7);
        assert_eq!(step(45, 15), 47);
        assert_eq!(step(42, 15), 40);
        assert_eq!(level_of(20, 15), 100, "no step beyond the top");
        assert_eq!(level_of(3, 0), 100, "one step at least");
    }

    /// A request goes the way it asks: higher to a step at or above it and above
    /// where it came from, lower to one at or below it and below; the same, nowhere.
    #[test]
    fn a_request_goes_the_way_it_asks() {
        assert_eq!(toward(40, 41, 15), Some(47));
        assert_eq!(toward(47, 46, 15), Some(40));
        assert_eq!(toward(40, 40, 15), None);
        assert_eq!(toward(0, 1, 15), Some(7));
        assert_eq!(toward(100, 99, 15), Some(93));
        assert_eq!(toward(100, 100, 15), None);
        assert_eq!(toward(93, 100, 15), Some(100));
        for steps in [1, 2, 7, 15, 16, 30, 100, 150, 255] {
            for index in 0..=steps {
                let from = level_of(index, steps);
                for asked in 0..=100u8 {
                    let Some(to) = toward(from, asked, steps) else {
                        assert_eq!(asked, from);
                        continue;
                    };
                    let case = format!("{steps} steps, {from} asked {asked}: {to}");
                    assert_eq!(step(to, steps), to, "on a step ({case})");
                    if asked > from {
                        assert!(to >= asked && to > from, "up ({case})");
                    } else {
                        assert!(to <= asked && to < from, "down ({case})");
                    }
                }
            }
        }
    }

    /// Requests within [`ONE_MOVE`] of each other go on from what the last asked for,
    /// while the headset stands where it put it; later ones, or after a change of the
    /// headset's own, from where the headset stands.
    #[test]
    fn requests_of_one_move_go_on_from_the_last() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut s = Steps::new(15);
        assert_eq!(s.take(40, 42, at(0)), 47, "up from 40");
        // Back below what it asked before: down.
        assert_eq!(s.take(47, 41, at(50)), 40, "down from 42 asked before");
        let mut s = Steps::new(15);
        assert_eq!(s.take(40, 42, at(0)), 47);
        // Still moving up, below the step it went to: not back down.
        assert_eq!(s.take(47, 44, at(50)), 47, "on up from 42");
        assert_eq!(s.take(47, 46, at(100)), 47, "on up from 44");
        assert_eq!(s.take(47, 48, at(150)), 53, "on up from 46");
        // At rest, the slider stands at 53 now: down from there.
        assert_eq!(s.take(53, 52, at(400)), 47, "down from where it stands");
        // A change of the headset's own between: from where it stands.
        assert_eq!(s.take(60, 59, at(450)), 53, "down from its own level");
    }
}
