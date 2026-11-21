//! Whether the headset is on the user's head, as the PC is told: from signals every
//! headset on Android with OpenXR gives, each used when it comes, none by the
//! headset's name.
//!
//! - The session's state: the app is shown (`VISIBLE`, `FOCUSED`), running but not
//!   shown, or stopped. Every runtime has it, but a runtime may keep the session
//!   shown for a while after the headset is taken off (until the system sleeps).
//! - The activity paused or resumed (Android): the system put the app away, showed
//!   a dialog over it, or put the headset to sleep. It comes at once, before the
//!   system may freeze the app.
//! - The screen on or off for the user (Android's `PowerManager.isInteractive`),
//!   read by the shell only while the app is paused or not shown: off is the
//!   headset asleep, which a pause alone does not tell from a dialog.
//! - The user present or not (`XR_EXT_user_presence`, where the runtime has it):
//!   from the headset's own sensor, at once when it is taken off. A sensor can
//!   flicker while the headset is merely adjusted, so absence counts only after
//!   [`AWAY_GRACE`]; being back counts at once.
//! - The app's own permission prompt (the microphone): while it is up, the pause and
//!   the hidden session it brings are the app's own doing, not the user leaving.
//!
//! Worn means the app is shown, not paused, and the user not known to be away. Not
//! worn comes in two kinds (see [`State`]): asleep (put away and the screen off),
//! which the PC acts on at once, and merely off (taken off while awake, the app put
//! away), which the PC gives a while to come back.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

/// How long the user must be away, by the headset's sensor alone, before the
/// headset counts as taken off: longer than a flicker while the headset is adjusted,
/// short enough that the PC hears it within a second.
pub const AWAY_GRACE: Duration = Duration::from_millis(500);

/// How soon after the app asks for a permission the system's prompt pauses it; a
/// prompt not up by then is not coming (the system answered by itself).
pub const PROMPT_WAIT: Duration = Duration::from_secs(5);

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

/// What the PC is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// On the head, the app in front.
    Worn,
    /// Taken off while awake, or the app put away: it may come back soon.
    Off,
    /// The headset went to sleep (the app put away and the screen off): nothing comes
    /// back until it is woken up.
    Asleep,
}

/// The signals as they came, and what the PC was last told.
#[derive(Debug, Clone)]
pub struct Presence {
    session: Session,
    paused: bool,
    /// The screen is on for the user, as last read.
    interactive: bool,
    /// When the app asked for a permission, until its prompt is up.
    asked_at: Option<Instant>,
    /// The app's own permission prompt is up: paused since the app asked, until it is
    /// back in front.
    prompt_up: bool,
    /// What the sensor said last; `None` until it says something (or without the
    /// extension).
    user: Option<bool>,
    /// Since when the sensor says the user is away.
    away_since: Option<Instant>,
    told: State,
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
            interactive: true,
            asked_at: None,
            prompt_up: false,
            user: None,
            away_since: None,
            told: State::Worn,
        }
    }

    /// The session's state changed.
    pub fn session(&mut self, session: Session) {
        self.session = session;
        match session {
            // Shown, the screen is on.
            Session::Shown => self.interactive = true,
            Session::Stopped => {
                self.user = None;
                self.away_since = None;
            }
            Session::Hidden => {}
        }
    }

    /// The activity was paused (`true`) or resumed.
    pub fn paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused && self.asked_at.take().is_some() {
            self.prompt_up = true;
        }
        // Resumed, the screen is on.
        self.interactive |= !paused;
    }

    /// The screen is on (`true`) or off for the user, as the shell read it.
    pub fn interactive(&mut self, on: bool) {
        self.interactive = on;
    }

    /// The app asked the system for a permission at `now`: the prompt the system
    /// shows pauses the app, and that pause is not the user leaving.
    pub fn asked(&mut self, now: Instant) {
        self.asked_at = Some(now);
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
        self.told == State::Worn
    }

    /// As last told.
    pub fn state(&self) -> State {
        self.told
    }

    /// Whether the shell should read the screen (see [`Presence::interactive`]):
    /// while the app is put away and not yet known to be asleep. Never while it is
    /// shown, so never in a frame that is drawn.
    pub fn wants_screen(&self) -> bool {
        self.put_away() && self.told != State::Asleep
    }

    fn put_away(&self) -> bool {
        self.paused || self.session != Session::Shown
    }

    /// The state by the signals so far, at `now`.
    fn wanted(&self, now: Instant) -> State {
        let away = self
            .away_since
            .is_some_and(|since| now.saturating_duration_since(since) >= AWAY_GRACE);
        let asking = self.prompt_up || self.asked_at.is_some();
        if self.put_away() && !self.interactive {
            State::Asleep
        } else if (self.put_away() && !asking) || away {
            State::Off
        } else {
            State::Worn
        }
    }

    /// Once a pass, after the signals of the pass: `Some(state)` when the PC is to be
    /// told something new.
    pub fn update(&mut self, now: Instant) -> Option<State> {
        // The prompt is gone once the app is back in front; one that never came is
        // not waited for.
        if self.prompt_up && !self.put_away() {
            self.prompt_up = false;
        }
        if self
            .asked_at
            .is_some_and(|at| now.saturating_duration_since(at) >= PROMPT_WAIT)
        {
            self.asked_at = None;
        }
        let state = self.wanted(now);
        (state != self.told).then(|| {
            self.told = state;
            state
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
    use State::{Asleep, Off, Worn};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn worn_at_the_start_and_told_nothing_new() {
        let mut p = Presence::new();
        assert!(p.worn());
        assert!(!p.wants_screen());
        assert_eq!(p.update(Instant::now()), None);
    }

    /// The power button: the activity pauses with the screen off already; the session
    /// stops a moment later. The pause and the screen are enough.
    #[test]
    fn a_pause_is_told_at_once_as_asleep() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        assert!(p.wants_screen());
        p.interactive(false);
        assert_eq!(p.update(t), Some(Asleep));
        assert!(!p.wants_screen(), "asleep: nothing more to read");
        p.session(Session::Hidden);
        p.session(Session::Stopped);
        assert_eq!(p.update(t + ms(50)), None);
        assert!(!p.worn());
    }

    /// The screen goes off a moment after the pause: off first, asleep once read.
    #[test]
    fn the_screen_going_off_after_the_pause_says_asleep() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        assert_eq!(p.update(t), Some(Off));
        p.session(Session::Stopped);
        assert_eq!(p.update(t + ms(30)), None);
        assert!(p.wants_screen());
        p.interactive(false);
        assert_eq!(p.update(t + ms(250)), Some(Asleep));
    }

    /// A dialog of the system over the app, or another app in front: paused (and
    /// maybe stopped) with the screen on is off, never asleep.
    #[test]
    fn a_pause_while_shown_is_not_asleep() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        p.interactive(true);
        assert_eq!(p.update(t), Some(Off));
        p.session(Session::Hidden);
        p.session(Session::Stopped);
        p.interactive(true);
        assert_eq!(p.update(t + ms(100)), None);
        // Back in front.
        p.paused(false);
        p.session(Session::Hidden);
        p.session(Session::Shown);
        assert_eq!(p.update(t + ms(5000)), Some(Worn));
    }

    /// The app's own permission prompt pauses it and may hide its session: the user
    /// is still there, and the PC hears nothing. The power button meanwhile still
    /// says asleep.
    #[test]
    fn a_pause_during_our_own_permission_ask_changes_nothing() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.asked(t);
        assert_eq!(p.update(t + ms(300)), None);
        p.paused(true);
        p.session(Session::Hidden);
        p.interactive(true);
        assert_eq!(p.update(t + ms(400)), None);
        p.session(Session::Stopped);
        // However long the user takes to answer.
        assert_eq!(p.update(t + ms(30_000)), None);
        // Answered: resumed, then shown again a pass later.
        p.paused(false);
        assert_eq!(p.update(t + ms(30_100)), None);
        p.session(Session::Hidden);
        p.session(Session::Shown);
        assert_eq!(p.update(t + ms(30_200)), None);
        assert!(p.worn());
        // The prompt is over: a pause now is the user leaving.
        p.paused(true);
        assert_eq!(p.update(t + ms(40_000)), Some(Off));
        p.paused(false);
        assert_eq!(p.update(t + ms(40_100)), Some(Worn));
        // Asked again, and the power button pressed while the prompt is up.
        p.asked(t + ms(50_000));
        p.paused(true);
        p.interactive(false);
        assert_eq!(p.update(t + ms(50_500)), Some(Asleep));
    }

    /// The system answered by itself (no prompt came within `PROMPT_WAIT`): a pause
    /// after that counts again.
    #[test]
    fn a_prompt_that_never_came_is_not_waited_for() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.asked(t);
        assert_eq!(p.update(t + PROMPT_WAIT), None);
        p.paused(true);
        assert_eq!(p.update(t + PROMPT_WAIT + ms(100)), Some(Off));
    }

    /// Taken off, the session may stay shown until the system sleeps: the sensor tells
    /// it after the grace, as off; the sleep that follows says asleep.
    #[test]
    fn the_sensor_tells_a_taken_off_headset_after_the_grace() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.user(t, true);
        assert_eq!(p.update(t), None);
        p.user(t + ms(100), false);
        assert_eq!(p.update(t + ms(100)), None);
        assert_eq!(p.update(t + ms(100) + AWAY_GRACE - ms(1)), None);
        assert_eq!(p.update(t + ms(100) + AWAY_GRACE), Some(Off));
        assert!(!p.wants_screen(), "shown: the screen is not read");
        // Put on again: at once.
        p.user(t + ms(2000), true);
        assert_eq!(p.update(t + ms(2000)), Some(Worn));
        // Off again, and left until the system sleeps.
        p.user(t + ms(3000), false);
        assert_eq!(p.update(t + ms(4000)), Some(Off));
        p.paused(true);
        p.interactive(false);
        assert_eq!(p.update(t + ms(18_000)), Some(Asleep));
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
        assert_eq!(p.update(t + AWAY_GRACE), Some(Off));
    }

    /// Without the sensor (a runtime without the extension) the session's states, the
    /// activity and the screen decide alone.
    #[test]
    fn without_the_sensor_the_session_decides() {
        let t = Instant::now();
        let mut p = Presence::new();
        // The app starts: idle, then ready, then shown.
        p.session(Session::Stopped);
        assert_eq!(p.update(t), Some(Off));
        p.session(Session::Hidden);
        assert_eq!(p.update(t), None);
        p.session(Session::Shown);
        assert_eq!(p.update(t), Some(Worn));
        // The system menu over the app keeps it shown (VISIBLE).
        p.session(Session::Shown);
        assert_eq!(p.update(t), None);
        // Another app in front, this one still running.
        p.session(Session::Hidden);
        assert_eq!(p.update(t), Some(Off));
    }

    /// Asleep and woken up: what the sensor said before the sleep does not keep the
    /// headset "away" once the session is shown again and the sensor is quiet.
    #[test]
    fn a_new_session_forgets_the_old_sensor_reading() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.user(t, false);
        assert_eq!(p.update(t + AWAY_GRACE), Some(Off));
        p.paused(true);
        p.session(Session::Stopped);
        p.interactive(false);
        assert_eq!(p.update(t + ms(15_000)), Some(Asleep));
        // Woken up and put on.
        p.paused(false);
        p.session(Session::Hidden);
        assert_eq!(p.update(t + ms(60_000)), Some(Off));
        p.session(Session::Shown);
        assert_eq!(p.update(t + ms(60_100)), Some(Worn));
    }

    /// Woken up while not on the head (the sensor says "away" as the session starts):
    /// shown counts as worn only until the grace is over.
    #[test]
    fn woken_while_away_is_told_after_the_grace() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        p.interactive(false);
        assert_eq!(p.update(t), Some(Asleep));
        p.paused(false);
        p.session(Session::Shown);
        p.user(t, false);
        assert_eq!(p.update(t), Some(Worn));
        assert_eq!(p.update(t + AWAY_GRACE), Some(Off));
    }

    /// Resumed but not shown yet is not worn; shown while still paused is not either.
    #[test]
    fn all_three_must_agree() {
        let t = Instant::now();
        let mut p = Presence::new();
        p.paused(true);
        assert_eq!(p.update(t), Some(Off));
        p.session(Session::Hidden);
        p.paused(false);
        assert_eq!(p.update(t), None);
        p.session(Session::Shown);
        p.paused(true);
        assert_eq!(p.update(t), None);
        p.paused(false);
        assert_eq!(p.update(t), Some(Worn));
    }
}
