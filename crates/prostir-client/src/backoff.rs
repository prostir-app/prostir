//! How long to wait before trying again something that keeps failing (opening the
//! sound output, say): a short pause at first, longer ones while it goes on failing,
//! never longer than a ceiling. Plain data: it builds and is tested on any system.

#![forbid(unsafe_code)]

use std::time::Duration;

/// The pauses between attempts: `first`, then twice as long each time, up to `max`;
/// back to `first` once an attempt worked.
#[derive(Debug, Clone)]
pub struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub const fn new(first: Duration, max: Duration) -> Self {
        Self {
            first,
            max,
            next: first,
        }
    }

    /// An attempt failed: the pause before the next one.
    pub fn failed(&mut self) -> Duration {
        let pause = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        pause
    }

    /// An attempt worked: the next failure pauses for `first` again.
    pub fn worked(&mut self) {
        self.next = self.first;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_grow_to_the_ceiling_and_start_over_once_it_works() {
        let ms = Duration::from_millis;
        let mut b = Backoff::new(ms(300), ms(5000));
        let pauses: Vec<u64> = (0..7).map(|_| b.failed().as_millis() as u64).collect();
        assert_eq!(pauses, [300, 600, 1200, 2400, 4800, 5000, 5000]);
        b.worked();
        assert_eq!(b.failed(), ms(300));
    }
}
