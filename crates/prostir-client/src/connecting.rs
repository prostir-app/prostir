//! When the search tries to connect again after a session ends (see
//! `crate::session`). Built and tested on any system.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use prostir_proto::PcGoodbye;

use crate::ids::PcId;

/// Between attempts to connect.
pub const RETRY_PAUSE: Duration = Duration::from_millis(300);
/// A session must have run this long for the next attempt to go at once: one that
/// breaks right after it opens is retried only after [`RETRY_PAUSE`], so a PC that
/// keeps dropping sessions is not hammered.
pub const QUICK_RETRY_AFTER: Duration = Duration::from_secs(2);

/// How a session went, for what to do next.
#[derive(Debug, Default)]
pub struct Ended {
    /// The PC is trusted and the session started.
    pub opened: bool,
    /// When the PC last sent an error: right before it hung up means it cannot
    /// serve the headset right now.
    pub refused: Option<Instant>,
    /// The PC ended the session on purpose, for this reason.
    pub goodbye: Option<PcGoodbye>,
    /// The PC speaks another protocol version: nothing changes until one side is
    /// updated.
    pub incompatible: bool,
    /// The user paired with the PC in this connection.
    pub paired: bool,
    /// The handshake showed this PC, which the dial was not to take (see
    /// `session::Takes`): let go before a word, and nothing opened.
    pub let_go: Option<PcId>,
}

impl Ended {
    /// Whether to connect again at once after this session, which lasted `lasted`:
    /// only one that opened, ran a while and broke off without the PC saying why (a
    /// goodbye or a refusal keep their pauses).
    pub fn retry_at_once(&self, lasted: Duration) -> bool {
        self.opened
            && self.goodbye.is_none()
            && self.refused.is_none()
            && lasted >= QUICK_RETRY_AFTER
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;

    #[test]
    fn only_a_session_that_broke_off_after_a_while_is_retried_at_once() {
        let t = Instant::now();
        let long = Duration::from_secs(60);
        let broke = Ended {
            opened: true,
            ..Ended::default()
        };
        assert!(broke.retry_at_once(long));
        assert!(broke.retry_at_once(QUICK_RETRY_AFTER));
        assert!(
            !broke.retry_at_once(Duration::from_millis(500)),
            "broke right after it opened"
        );
        assert!(!Ended::default().retry_at_once(long), "never opened");
        let goodbye = Ended {
            opened: true,
            goodbye: Some(PcGoodbye::from_code(1)),
            ..Ended::default()
        };
        assert!(!goodbye.retry_at_once(long));
        let refused = Ended {
            opened: true,
            refused: Some(t),
            ..Ended::default()
        };
        assert!(!refused.retry_at_once(long));
    }
}
