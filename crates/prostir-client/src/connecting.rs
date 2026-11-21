//! When the network thread tries to connect again: after a session ends, and how often
//! it probes the local network for a PC that does not announce itself. Built and
//! tested on any system.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use prostir_proto::PcGoodbye;

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

/// The first probe of the local network for a PC that does not announce itself, and
/// how much longer each next one waits, up to [`PROBE_AT_MOST`].
pub const PROBE_FIRST: Duration = Duration::from_millis(7500);
pub const PROBE_AT_MOST: Duration = Duration::from_secs(60);

/// When to probe the local network next: at once when the search starts, then less
/// and less often while no PC is found (each probe starts 253 short connections).
#[derive(Debug, Default)]
pub struct Probing {
    /// When the next probe is due, and the wait after it; `None` before the first.
    next: Option<(Instant, Duration)>,
}

impl Probing {
    /// Whether to probe at `now`; if so, the next one waits twice as long as the last.
    pub fn due(&mut self, now: Instant) -> bool {
        match self.next {
            Some((at, _)) if now < at => false,
            Some((_, wait)) => {
                self.next = Some((now + wait, (wait * 2).min(PROBE_AT_MOST)));
                true
            }
            None => {
                self.next = Some((now + PROBE_FIRST, PROBE_FIRST * 2));
                true
            }
        }
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

    #[test]
    fn probing_backs_off_to_once_a_minute() {
        let t = Instant::now();
        let mut probing = Probing::default();
        let mut at = Vec::new();
        // Looked at every half second for ten minutes.
        for k in 0..1200u32 {
            let now = t + Duration::from_millis(500) * k;
            if probing.due(now) {
                at.push((now - t).as_secs_f32());
            }
        }
        assert_eq!(&at[..6], [0.0, 7.5, 22.5, 52.5, 112.5, 172.5]);
        assert!(at.windows(2).skip(4).all(|w| w[1] - w[0] == 60.0));
        // A new search starts over.
        let mut probing = Probing::default();
        assert!(probing.due(t + Duration::from_secs(1000)));
    }
}
