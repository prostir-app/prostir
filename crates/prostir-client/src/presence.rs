//! Whether the headset is on the user's head, as the PC is told: from signals every
//! headset on Android with OpenXR gives, each used when it comes, none by the
//! headset's name.
//!
//! - The session's state: the app is shown (`VISIBLE`, `FOCUSED`), running but not
//!   shown, or stopped. Every runtime has it, but a runtime may keep the session
//!   shown for a while after the headset is taken off (until the system sleeps).
//! - The activity paused or resumed (Android): the system put the app away, or the
//!   headset to sleep. It comes at once, before the system may freeze the app.
//! - The user present or not (`XR_EXT_user_presence`, where the runtime has it):
//!   from the headset's own sensor, at once when it is taken off. A sensor can
//!   flicker while the headset is merely adjusted, so absence counts only after
//!   [`AWAY_GRACE`]; being back counts at once.
//!
//! Worn means all three agree: shown, not paused, and the user not known to be away.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

/// How long the user must be away, by the headset's sensor alone, before the
/// headset counts as taken off: longer than a flicker while the headset is adjusted,
/// short enough that the PC hears it within a second.
pub const AWAY_GRACE: Duration = Duration::from_millis(500);

/// The OpenXR session as presence sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// `VISIBLE` or `FOCUSED`: the app is in front of the user.
    Shown,
    /// Running, not shown (`READY`, `SYNCHRONIZED`).
    Hidden,
    /// Not running (`STOPPING`, `IDLE`): what the sensor said belongs to the session
    /// that ended.
    Stopped,
}

/// The signals as they came, and what the PC was last told.
#[derive(Debug, Clone)]
pub struct Presence {
    session: Session,
    paused: bool,
    /// What the sensor said last; `None` until it says something (or without the
    /// extension).
    user: Option<bool>,
    /// Since when the sensor says the user is away.
    away_since: Option<Instant>,
    told: bool,
}

impl Default for Presence {
    fn default() -> Self {
        Self::new()
    }
}

impl Presence {
    /// Worn, as the app starts (the PC is told so until a signal says otherwise).
    pub const fn new() -> Self {
        Self {
            session: Session::Shown,
            paused: false,
            user: None,
            away_since: None,
            told: true,
        }
    }

    /// The session's state changed.
    pub fn session(&mut self, session: Session) {
        self.session = session;
        if session == Session::Stopped {
            self.user = None;
            self.away_since = None;
        }
    }

    /// The activity was paused (`true`) or resumed.
    pub fn paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// The headset's sensor says whether the user is there.
    pub fn user(&mut self, now: Instant, present: bool) {
        if present {
            self.away_since = None;
        } else if self.user != Some(false) {
            self.away_since = Some(now);
        }
        self.user = Some(present);
    }

    /// Worn, as last told.
    pub fn worn(&self) -> bool {
        self.told
    }

    /// Worn by the signals so far, at `now`.
    fn wanted(&self, now: Instant) -> bool {
        let away = self
            .away_since
            .is_some_and(|since| now.saturating_duration_since(since) >= AWAY_GRACE);
        self.session == Session::Shown && !self.paused && !away
    }

    /// Once a pass, after the signals of the pass: `Some(worn)` when the PC is to be
    /// told something new.
    pub fn update(&mut self, now: Instant) -> Option<bool> {
        let worn = self.wanted(now);
        (worn != self.told).then(|| {
            self.told = worn;
            worn
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn worn_at_the_start_and_told_nothing_new() {
        let mut p = Presence::new();
        assert!(p.worn());
        assert_eq!(p.update(Instant::now()), None);
    }

    /// The power button: the activity pauses, the session stops a moment later. The
    /// pause alone is enough.
    #[test]
    fn a_pause_is_told_at_once() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        assert_eq!(p.update(t), Some(false));
        p.session(Session::Hidden);
        p.session(Session::Stopped);
        assert_eq!(p.update(t + ms(50)), None);
        assert!(!p.worn());
    }

    /// Taken off, the session may stay shown until the system sleeps: the sensor tells
    /// it after the grace, not later.
    #[test]
    fn the_sensor_tells_a_taken_off_headset_after_the_grace() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.user(t, true);
        assert_eq!(p.update(t), None);
        p.user(t + ms(100), false);
        assert_eq!(p.update(t + ms(100)), None);
        assert_eq!(p.update(t + ms(100) + AWAY_GRACE - ms(1)), None);
        assert_eq!(p.update(t + ms(100) + AWAY_GRACE), Some(false));
        // Put on again: at once.
        p.user(t + ms(2000), true);
        assert_eq!(p.update(t + ms(2000)), Some(true));
    }

    /// Adjusting the headset: the sensor flickers off and on within the grace, and the
    /// PC hears nothing.
    #[test]
    fn a_flicker_is_not_told() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.user(t, false);
        assert_eq!(p.update(t + ms(200)), None);
        p.user(t + ms(300), true);
        assert_eq!(p.update(t + ms(300)), None);
        assert_eq!(p.update(t + ms(5000)), None);
        assert!(p.worn());
    }

    /// The sensor saying "away" again does not restart the grace.
    #[test]
    fn away_said_twice_counts_from_the_first() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.user(t, false);
        p.user(t + ms(400), false);
        assert_eq!(p.update(t + AWAY_GRACE), Some(false));
    }

    /// Without the sensor (a runtime without the extension) the session's states and
    /// the activity decide alone, as before.
    #[test]
    fn without_the_sensor_the_session_decides() {
        let t = Instant::now();
        let mut p = Presence::new();
        // The app starts: idle, then ready, then shown.
        p.session(Session::Stopped);
        assert_eq!(p.update(t), Some(false));
        p.session(Session::Hidden);
        assert_eq!(p.update(t), None);
        p.session(Session::Shown);
        assert_eq!(p.update(t), Some(true));
        // The system menu over the app keeps it shown (VISIBLE).
        p.session(Session::Shown);
        assert_eq!(p.update(t), None);
        // Another app in front.
        p.session(Session::Hidden);
        assert_eq!(p.update(t), Some(false));
    }

    /// Asleep and woken up: what the sensor said before the sleep does not keep the
    /// headset "away" once the session is shown again and the sensor is quiet.
    #[test]
    fn a_new_session_forgets_the_old_sensor_reading() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.user(t, false);
        assert_eq!(p.update(t + AWAY_GRACE), Some(false));
        p.paused(true);
        p.session(Session::Stopped);
        assert_eq!(p.update(t + ms(15_000)), None);
        // Woken up and put on.
        p.paused(false);
        p.session(Session::Hidden);
        assert_eq!(p.update(t + ms(60_000)), None);
        p.session(Session::Shown);
        assert_eq!(p.update(t + ms(60_100)), Some(true));
    }

    /// Woken up while not on the head (the sensor says "away" as the session starts):
    /// shown counts as worn only until the grace is over.
    #[test]
    fn woken_while_away_is_told_after_the_grace() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        p.session(Session::Stopped);
        assert_eq!(p.update(t), Some(false));
        p.paused(false);
        p.session(Session::Shown);
        p.user(t, false);
        assert_eq!(p.update(t), Some(true));
        assert_eq!(p.update(t + AWAY_GRACE), Some(false));
    }

    /// Resumed but not shown yet is not worn; shown while still paused is not either.
    #[test]
    fn all_three_must_agree() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        assert_eq!(p.update(t), Some(false));
        p.session(Session::Shown);
        assert_eq!(p.update(t), None);
        p.paused(false);
        assert_eq!(p.update(t), Some(true));
    }
}
