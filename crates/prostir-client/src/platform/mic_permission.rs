//! While a PC program records from the headset's microphone: the wait until the system
//! lets the app record. The system is asked once; refused, the wait goes on looking
//! (without asking again), so "Ask again" in the menu or allowing it in the system's
//! settings brings the microphone back while the PC still records. A refusal counts
//! as soon as the system's prompt was answered without access, or after
//! [`ANSWER_WITHIN`] without an answer. Plain data with the time passed in: it builds
//! and is tested on any system; the microphone on Android looks every [`LOOK_EVERY`].

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

/// How often the wait looks whether the system lets the app record.
pub const LOOK_EVERY: Duration = Duration::from_millis(500);
/// A prompt with no answer seen for this long counts as refused.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(60);
/// Looks after the prompt was answered before access still missing counts as refused:
/// the system may say it was granted a moment after the app is back.
const LOOKS_AFTER_ANSWER: u8 = 2;

/// What the last system prompt for the microphone went through, as the activity shows
/// it: the prompt pauses the app, and the app is back once it was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    None,
    /// Asked for; not shown yet.
    Asked,
    /// Shown (the app paused).
    Shown,
    /// Answered (the app is back).
    Answered,
}

impl Prompt {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Asked,
            2 => Self::Shown,
            3 => Self::Answered,
            _ => Self::None,
        }
    }

    /// The prompt after the app was paused (`true`) or came back.
    pub fn activity(self, paused: bool) -> Self {
        match (self, paused) {
            (Self::Asked, true) => Self::Shown,
            (Self::Shown, false) => Self::Answered,
            (other, _) => other,
        }
    }
}

/// What to do after a look (see [`PermissionWait::look`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The system lets the app record.
    Record,
    /// Show the system's prompt now.
    Ask,
    /// Look again in a while.
    Wait,
    /// Refused: the menu says where to allow it. The wait goes on looking.
    Refused,
}

/// One recording's wait for access.
#[derive(Debug)]
pub struct PermissionWait {
    /// The prompts shown so far, as last seen (by anyone: "Ask again" shows one too).
    asks: u32,
    /// When the last prompt seen went.
    since: Option<Instant>,
    /// Looks since that prompt was answered.
    answered: u8,
}

impl PermissionWait {
    /// A wait that begins with `asks` prompts shown so far.
    pub fn new(asks: u32) -> Self {
        Self {
            asks,
            since: None,
            answered: 0,
        }
    }

    /// A look at `now`: `granted` (the system lets the app record), `denied` (a
    /// refusal stands: no prompt of the app's own), `asks` (prompts shown so far),
    /// `prompt` (what the last one went through).
    pub fn look(
        &mut self,
        now: Instant,
        granted: bool,
        denied: bool,
        asks: u32,
        prompt: Prompt,
    ) -> Step {
        if granted {
            return Step::Record;
        }
        if asks != self.asks {
            self.asks = asks;
            self.since = Some(now);
            self.answered = 0;
        }
        if denied {
            return Step::Wait;
        }
        let Some(since) = self.since else {
            // The prompt the caller shows now.
            self.asks = asks.wrapping_add(1);
            self.since = Some(now);
            return Step::Ask;
        };
        if prompt == Prompt::Answered {
            self.answered = self.answered.saturating_add(1);
        }
        if self.answered >= LOOKS_AFTER_ANSWER
            || now.saturating_duration_since(since) >= ANSWER_WITHIN
        {
            // Said once: from now on the refusal stands (`denied`).
            self.since = None;
            self.answered = 0;
            return Step::Refused;
        }
        Step::Wait
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;

    /// The wait as the microphone does it: a look every 500 ms for `looks`, the
    /// prompt as `prompt` says at each; the steps, and the prompts shown.
    struct Mic {
        wait: PermissionWait,
        now: Instant,
        asks: u32,
        denied: bool,
        granted: bool,
        prompt: Prompt,
    }

    impl Mic {
        fn new(denied: bool) -> Self {
            Self {
                wait: PermissionWait::new(0),
                now: Instant::now(),
                asks: 0,
                denied,
                granted: false,
                prompt: Prompt::None,
            }
        }

        fn look(&mut self) -> Step {
            let step = self
                .wait
                .look(self.now, self.granted, self.denied, self.asks, self.prompt);
            match step {
                Step::Ask => {
                    self.asks += 1;
                    self.prompt = Prompt::Asked;
                }
                Step::Refused => self.denied = true,
                Step::Record | Step::Wait => {}
            }
            self.now += LOOK_EVERY;
            step
        }

        /// "Ask again" in the menu: a refusal no longer stands, and a prompt shows.
        fn ask_again(&mut self) {
            self.denied = false;
            self.asks += 1;
            self.prompt = Prompt::Asked;
        }
    }

    #[test]
    fn asked_once_and_recording_once_allowed() {
        let mut mic = Mic::new(false);
        assert_eq!(mic.look(), Step::Ask);
        for _ in 0..10 {
            assert_eq!(mic.look(), Step::Wait);
        }
        mic.granted = true;
        assert_eq!(mic.look(), Step::Record);
        assert_eq!(mic.asks, 1);
    }

    /// The prompt answered without access is a refusal at once, not after a minute;
    /// a prompt never seen answered counts as refused after a minute.
    #[test]
    fn a_refusal_is_noticed_when_the_prompt_is_answered() {
        let mut mic = Mic::new(false);
        assert_eq!(mic.look(), Step::Ask);
        mic.prompt = mic.prompt.activity(true);
        assert_eq!(mic.look(), Step::Wait, "the prompt is up");
        mic.prompt = mic.prompt.activity(false);
        assert_eq!(mic.look(), Step::Wait, "a moment for the grant to land");
        assert_eq!(mic.look(), Step::Refused);
        assert!(mic.denied);
        let mut mic = Mic::new(false);
        assert_eq!(mic.look(), Step::Ask);
        let looks = (ANSWER_WITHIN.as_millis() / LOOK_EVERY.as_millis()) as usize;
        let steps: Vec<Step> = (0..looks).map(|_| mic.look()).collect();
        assert!(steps[..looks - 1].iter().all(|s| *s == Step::Wait));
        assert_eq!(steps[looks - 1], Step::Refused);
    }

    /// Refused: nothing is asked of the system on its own, but the wait goes on, and
    /// "Ask again" or access given in the system's settings records.
    #[test]
    fn after_a_refusal_the_wait_goes_on() {
        let mut mic = Mic::new(true);
        for _ in 0..200 {
            assert_eq!(mic.look(), Step::Wait);
        }
        assert_eq!(mic.asks, 0, "no prompt of its own");
        mic.ask_again();
        assert_eq!(mic.look(), Step::Wait);
        mic.prompt = mic.prompt.activity(true);
        mic.prompt = mic.prompt.activity(false);
        mic.granted = true;
        assert_eq!(mic.look(), Step::Record);
        // Allowed in the system's settings while refused.
        let mut mic = Mic::new(true);
        assert_eq!(mic.look(), Step::Wait);
        mic.granted = true;
        assert_eq!(mic.look(), Step::Record);
        // "Ask again" refused once more: said again.
        let mut mic = Mic::new(true);
        mic.ask_again();
        mic.prompt = mic.prompt.activity(true).activity(false);
        assert_eq!(mic.look(), Step::Wait);
        assert_eq!(mic.look(), Step::Refused);
    }

    #[test]
    fn the_prompt_follows_the_activity() {
        assert_eq!(
            Prompt::Asked.activity(false),
            Prompt::Asked,
            "not shown yet"
        );
        assert_eq!(Prompt::None.activity(true), Prompt::None);
        assert_eq!(
            Prompt::Asked.activity(true).activity(false),
            Prompt::Answered
        );
        for p in [Prompt::None, Prompt::Asked, Prompt::Shown, Prompt::Answered] {
            assert_eq!(Prompt::from_code(p.code()), p);
        }
    }
}
