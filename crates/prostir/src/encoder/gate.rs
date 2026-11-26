//! The frame gate before each encoder: how many frames of a monitor go to its encoder.
//! The monitor looked at gets every frame, the others half of their monitor's rate, or
//! 5 a second in "Focus".
//!
//! The router sets the level ([`Gate::set_level`], from `router/priority.rs`); the
//! capture asks the gate for each frame ([`Gate::turn`]) and keeps the newest one that
//! has to wait, sending it once its turn comes. So a monitor slowed down still shows
//! its last change, only later; and the first frame after a still while goes at once.
//! The gate asks for no keyframe and touches nothing of the encoder: the encoder only
//! sees fewer frames, the same on every encoder.
//!
//! Frames that must not wait pass at once: one after a keyframe was asked, after the
//! picture was asked again (the keyframe of a still screen, the one that sharpens it),
//! after a resume ([`Gate::open`]).
//!
//! Everything here is atomics: a frame takes no lock and no memory.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How many frames of a monitor go to its encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// "Focus" in the headset, and nobody looks at it: [`GLANCE_FPS`] a second.
    Glance,
    /// Nobody looks at it: half its monitor's rate.
    Side,
    /// Every frame: looked at, the active window's, or nothing known of the gaze.
    Full,
}

/// Frames a second of a monitor in "Focus" that nobody looks at.
pub const GLANCE_FPS: u32 = 5;

/// The rate a monitor counts at before its encoder said one, mHz.
const DEFAULT_RATE_MHZ: u32 = 60_000;

/// A frame that comes this much of its wait early passes: a source's frames come with
/// a little jitter, and one a hair early would wait for nothing.
const EARLY_PART: u32 = 8;

impl Level {
    fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Glance,
            1 => Self::Side,
            _ => Self::Full,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Glance => 0,
            Self::Side => 1,
            Self::Full => 2,
        }
    }
}

/// One encoder's gate, shared by the router (the level), the capture (the frames), and
/// whoever asks for a keyframe (which opens it).
#[derive(Debug)]
pub struct Gate {
    level: AtomicU8,
    /// The monitor's full rate, mHz, as its encoder is told it (0: not said yet).
    rate_mhz: AtomicU32,
    /// The next frame passes at once.
    open: AtomicBool,
    /// Frames that did not pass when they came, since the router's last report.
    held: AtomicU32,
    /// Frames not encoded because nothing on the picture changed, since then.
    unchanged: AtomicU32,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            level: AtomicU8::new(Level::Full.as_u8()),
            rate_mhz: AtomicU32::new(0),
            open: AtomicBool::new(false),
            held: AtomicU32::new(0),
            unchanged: AtomicU32::new(0),
        }
    }
}

/// What a gate counted since it was last asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Frames that waited for their turn (the newest of them went later, the others
    /// never).
    pub held: u32,
    /// Frames not encoded: the picture had not changed.
    pub unchanged: u32,
}

impl Gate {
    /// The level from now on; whether it changed. Asks for no keyframe: a frame
    /// waiting goes at its turn under the new level.
    pub fn set_level(&self, level: Level) -> bool {
        self.level.swap(level.as_u8(), Ordering::Relaxed) != level.as_u8()
    }

    pub fn level(&self) -> Level {
        Level::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// The monitor's full rate is `numer / denom` frames a second (its encoder is told
    /// so); none of 0.
    pub fn set_rate(&self, numer: i32, denom: i32) {
        let (Ok(numer), Ok(denom)) = (u64::try_from(numer), u64::try_from(denom)) else {
            return;
        };
        if numer == 0 || denom == 0 {
            return;
        }
        let mhz = u32::try_from(numer * 1000 / denom).unwrap_or(u32::MAX);
        self.rate_mhz.store(mhz, Ordering::Relaxed);
    }

    /// The monitor's full rate, mHz (60 Hz until its encoder said one).
    pub fn rate_mhz(&self) -> u32 {
        match self.rate_mhz.load(Ordering::Relaxed) {
            0 => DEFAULT_RATE_MHZ,
            r => r,
        }
    }

    /// Frames a second the level lets through, mHz.
    pub fn fps_mhz(&self) -> u32 {
        let full = self.rate_mhz();
        match self.level() {
            Level::Full => full,
            Level::Side => full / 2,
            Level::Glance => (full / 2).min(GLANCE_FPS * 1000),
        }
    }

    /// The shortest time between two frames now; none at the full rate.
    pub fn spacing(&self) -> Duration {
        match self.level() {
            Level::Full => Duration::ZERO,
            _ => Duration::from_micros(1_000_000_000 / u64::from(self.fps_mhz().max(1))),
        }
    }

    /// The next frame passes at once (a keyframe or the picture was asked for, the
    /// encoder resumed).
    pub fn open(&self) {
        self.open.store(true, Ordering::Relaxed);
    }

    /// Whether the next frame passes at once, whenever it comes.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// The turn of a frame there at `now`, the last having gone at `last`: `None` when
    /// it goes now (then the gate is no longer open), else when it may go. Counts
    /// nothing (see [`Gate::held`]).
    pub fn turn(&self, last: Option<Instant>, now: Instant) -> Option<Instant> {
        if self.open.swap(false, Ordering::Relaxed) {
            return None;
        }
        let spacing = self.spacing();
        let at = last? + spacing;
        // A frame there `spacing / 8` early or less goes now.
        (now + spacing / EARLY_PART < at).then_some(at)
    }

    /// A frame did not pass when it came: counted.
    pub fn held(&self) {
        self.held.fetch_add(1, Ordering::Relaxed);
    }

    /// A frame was not encoded, nothing on the picture having changed: counted.
    pub fn unchanged(&self) {
        self.unchanged.fetch_add(1, Ordering::Relaxed);
    }

    /// What it counted since this was last asked, counted anew from now.
    pub fn take_counts(&self) -> Counts {
        Counts {
            held: self.held.swap(0, Ordering::Relaxed),
            unchanged: self.unchanged.swap(0, Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// The rates of the levels, of a 60 Hz monitor and of a 120 Hz one.
    #[test]
    fn the_rate_of_each_level() {
        let gate = Gate::default();
        assert_eq!(
            gate.level(),
            Level::Full,
            "a new gate lets everything through"
        );
        assert_eq!(gate.spacing(), Duration::ZERO);
        assert_eq!(gate.fps_mhz(), 60_000, "60 Hz before the encoder said");
        assert!(gate.set_level(Level::Side));
        assert!(!gate.set_level(Level::Side), "no change");
        assert_eq!(gate.fps_mhz(), 30_000);
        assert_eq!(gate.spacing(), Duration::from_micros(33_333));
        gate.set_rate(120, 1);
        assert_eq!(gate.fps_mhz(), 60_000, "half of 120");
        gate.set_level(Level::Glance);
        assert_eq!(gate.fps_mhz(), 5_000);
        assert_eq!(gate.spacing(), Duration::from_millis(200));
        gate.set_rate(90, 1);
        gate.set_level(Level::Full);
        assert_eq!(gate.fps_mhz(), 90_000);
        gate.set_rate(0, 1);
        assert_eq!(gate.fps_mhz(), 90_000, "a rate of 0 says nothing");
        gate.set_rate(60_000, 1001);
        assert_eq!(gate.rate_mhz(), 59_940);
    }

    /// At half of 60 Hz, of frames every 16.7 ms every other goes at once and the one
    /// between waits; the first after a still while goes at once.
    #[test]
    fn half_the_frames_go_and_the_first_after_a_while_at_once() {
        let gate = Gate::default();
        gate.set_level(Level::Side);
        let t0 = Instant::now();
        assert_eq!(gate.turn(None, t0), None, "the first ever");
        let spacing = gate.spacing();
        let mut last = t0;
        let mut went = 0;
        for i in 1..=60u32 {
            let now = t0 + Duration::from_micros(16_667) * i;
            match gate.turn(Some(last), now) {
                None => {
                    went += 1;
                    last = now;
                }
                Some(at) => assert_eq!(at, last + spacing),
            }
        }
        assert_eq!(went, 30, "half of 60");
        assert_eq!(
            gate.turn(Some(last), last + 2 * spacing),
            None,
            "after a while"
        );
        // A frame a hair early goes too: the source's jitter.
        assert_eq!(gate.turn(Some(last), last + spacing - MS), None);
        assert!(gate.turn(Some(last), last + spacing / 2).is_some());
    }

    /// Opened (a keyframe asked for), the next frame goes at once, at any level, and
    /// only that one.
    #[test]
    fn a_keyframe_asked_for_is_never_held() {
        let gate = Gate::default();
        gate.set_level(Level::Glance);
        let t0 = Instant::now();
        assert!(gate.turn(Some(t0), t0 + MS).is_some());
        gate.open();
        assert!(gate.is_open());
        assert_eq!(gate.turn(Some(t0), t0 + MS), None, "the keyframe's frame");
        assert!(gate.turn(Some(t0 + MS), t0 + 2 * MS).is_some(), "once");
        // A change of level opens nothing and asks for nothing.
        gate.set_level(Level::Side);
        assert!(!gate.is_open());
        gate.set_level(Level::Full);
        assert_eq!(gate.turn(Some(t0), t0 + MS), None, "full: every frame");
    }

    #[test]
    fn counts_until_taken() {
        let gate = Gate::default();
        gate.held();
        gate.held();
        gate.unchanged();
        assert_eq!(
            gate.take_counts(),
            Counts {
                held: 2,
                unchanged: 1
            }
        );
        assert_eq!(gate.take_counts(), Counts::default());
    }
}
