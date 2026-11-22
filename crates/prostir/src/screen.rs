//! A screen of the PC shown in the headset: a virtual monitor, a window panel or a real
//! display, and the one key the PC knows it by.

use std::time::{Duration, Instant};

use prostir_proto::state::DisplayId;

use crate::platform::{MonitorSpec, OutputKey};
use crate::worker::Encoding;

/// The PC's key for a screen: in its set, its rows and its saved files. Numbers come
/// from a counter that never goes back (see `Store::new_id`), so a number is never
/// another screen's: a virtual monitor and a display of the PC keep theirs between
/// runs, and the headset finds each where it was.
///
/// The protocol's [`DisplayId`] is the same number, so [`ScreenId::wire`] and
/// [`ScreenId::from_wire`] are the only places they cross.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScreenId(u32);

impl ScreenId {
    /// A screen's number as saved.
    pub fn saved(n: u32) -> Self {
        Self(n)
    }

    /// The number saved for it.
    pub fn number(self) -> u32 {
        self.0
    }

    /// The display in protocol messages.
    pub fn wire(self) -> DisplayId {
        DisplayId(self.0)
    }

    /// The screen a protocol message names.
    pub fn from_wire(id: DisplayId) -> Self {
        Self(id.0)
    }
}

impl std::fmt::Display for ScreenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "screen {}", self.0)
    }
}

/// What a screen shows.
#[derive(Debug, Clone)]
pub enum ScreenKind {
    /// A virtual monitor made for the headset.
    Virtual { spec: MonitorSpec },
    /// An output made for one window, shown as a floating panel (the desktop's worker
    /// keeps the window as it was before, to put it back).
    Window { spec: MonitorSpec },
    /// A display of the PC itself (`builtin`: the laptop's own panel), streamed as it
    /// is while `shown`; `key` is how it is told from others (and saved), `label` what
    /// people see it called, as the desktop last said (the saved key's connector until
    /// it is seen), `size` its picture in pixels as the desktop last said (none until
    /// it is seen in this run).
    Physical {
        key: crate::store::PhysicalKey,
        label: String,
        builtin: bool,
        shown: bool,
        size: Option<(u32, u32)>,
    },
}

impl ScreenKind {
    /// The mode it was made with (none for a real display).
    pub fn spec(&self) -> Option<&MonitorSpec> {
        match self {
            Self::Virtual { spec } | Self::Window { spec, .. } => Some(spec),
            Self::Physical { .. } => None,
        }
    }
}

/// How a screen is doing.
#[derive(Debug, Clone)]
pub enum ScreenState {
    /// Being made: no output or encoder yet.
    Starting,
    Running,
    /// Its output or encoder could not start, or broke: it keeps its place in the set
    /// and the rows, and a monitor is tried again by itself after a pause (see
    /// [`retry_pause`]), at the next session, or when asked.
    Failed {
        error: prostir_proto::state::ErrorCode,
        why: String,
    },
    /// A display of the PC that is not connected now (the lid closed, the cable out):
    /// it keeps its number and its place.
    Absent,
}

/// A screen as the control loop keeps it: what it is, and what the desktop's worker
/// said of its output and encoder (which the worker owns).
pub struct Screen {
    pub id: ScreenId,
    pub kind: ScreenKind,
    /// The number its output goes by (`Prostir-N`): the lowest free one when it was
    /// made, so the names do not grow and KScreen finds the outputs it remembers. A
    /// display of the PC has one while it streams.
    pub number: u32,
    pub state: ScreenState,
    /// The desktop's key for its output, once the worker made it.
    pub output: Option<OutputKey>,
    /// How it encodes, while it streams.
    pub encoding: Option<Encoding>,
    /// Raised to have the worker try it again after a failure.
    pub attempt: u32,
    /// A monitor that failed: how many times in a row, and when it is tried again.
    pub failures: u32,
    pub retry_at: Option<Instant>,
    /// Since when it streams, as the worker last said.
    pub running_since: Option<Instant>,
    /// A display of the PC streams (it is shown and connected).
    pub streaming: bool,
}

impl Screen {
    /// A screen still to be made.
    pub fn starting(id: ScreenId, number: u32, kind: ScreenKind) -> Self {
        Self {
            id,
            kind,
            number,
            state: ScreenState::Starting,
            output: None,
            encoding: None,
            attempt: 0,
            failures: 0,
            retry_at: None,
            running_since: None,
            streaming: false,
        }
    }

    /// It failed at `now`: tried again after a pause that grows with each failure in
    /// a row (one after it streamed a good while starts the count again).
    pub fn failed_at(&mut self, now: Instant) {
        if self
            .running_since
            .is_some_and(|t| now.duration_since(t) >= STREAMED_WELL)
        {
            self.failures = 0;
        }
        self.running_since = None;
        self.failures += 1;
        self.retry_at = Some(now + retry_pause(self.failures));
    }

    pub fn running(&self) -> bool {
        matches!(self.state, ScreenState::Running)
    }

    /// The desktop's key for its output, while it has one.
    pub fn output_key(&self) -> Option<&OutputKey> {
        self.output.as_ref()
    }

    pub fn is_virtual(&self) -> bool {
        matches!(self.kind, ScreenKind::Virtual { .. })
    }

    pub fn is_window(&self) -> bool {
        matches!(self.kind, ScreenKind::Window { .. })
    }

    pub fn is_physical(&self) -> bool {
        matches!(self.kind, ScreenKind::Physical { .. })
    }

    /// A display of the PC the headset is to show.
    pub fn is_shown_physical(&self) -> bool {
        matches!(self.kind, ScreenKind::Physical { shown: true, .. })
    }

    /// The name its output and stream go by: `Prostir-N`, or the label of a display
    /// of the PC.
    pub fn name(&self) -> String {
        match &self.kind {
            ScreenKind::Physical { label, .. } => label.clone(),
            _ => format!("Prostir-{}", self.number),
        }
    }
}

/// The pause before a monitor that failed is tried again: 1 s, then twice as long
/// with each failure in a row, up to 30 s (each screen on its own: the others do not
/// wait).
pub const RETRY_FIRST: Duration = Duration::from_secs(1);
pub const RETRY_LONGEST: Duration = Duration::from_secs(30);
/// Streaming this long, a monitor that fails again is counted from its first failure.
const STREAMED_WELL: Duration = Duration::from_secs(60);

/// The pause after the `failures`-th failure in a row (from 1).
pub fn retry_pause(failures: u32) -> Duration {
    RETRY_FIRST
        .saturating_mul(1 << failures.saturating_sub(1).min(5))
        .min(RETRY_LONGEST)
}

/// Restores of one monitor's size allowed in [`RESTORE_WINDOW`].
const RESTORES_ALLOWED: usize = 3;
const RESTORE_WINDOW: Duration = Duration::from_secs(60);
/// How long a restart has to put the size back before the output is made anew.
const RESTORE_WAIT: Duration = Duration::from_secs(2);

/// What to do about a monitor's size (see [`SizeGuard::check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeAction {
    /// The size is right, or the last step still has time to work.
    Wait,
    /// The size is right again: lay the desktop out again for it.
    Restored,
    /// Start its encoder over: the desktop gives the output back its size.
    Restart,
    /// The restart did not help: make the output anew with the same number and spec.
    Recreate,
    /// Restored too often lately: say so once and leave the size as it is.
    GiveUp,
}

/// Puts a monitor's size back after another program changed it, without fighting
/// that program forever.
#[derive(Default)]
pub struct SizeGuard {
    /// When the size was restored (restarts and recreations), within the window.
    restores: Vec<Instant>,
    /// The last step taken and when, while the size is still wrong.
    last: Option<(SizeAction, Instant)>,
    /// Gave up (and said so): no more restores until the size is right again with
    /// room in the window.
    gave_up: bool,
    /// The size was wrong at the last check.
    wrong: bool,
}

impl SizeGuard {
    /// What to do at `now`, given the size another program gave the monitor
    /// (`None` while it has its own size). Records the step it returns.
    pub fn check(&mut self, resized: Option<(u32, u32)>, now: Instant) -> SizeAction {
        self.restores
            .retain(|&at| now.duration_since(at) < RESTORE_WINDOW);
        let full = self.restores.len() >= RESTORES_ALLOWED;
        if resized.is_none() {
            self.last = None;
            // A give-up ends once the window has room again, so it is said once.
            self.gave_up &= full;
            return if std::mem::take(&mut self.wrong) {
                SizeAction::Restored
            } else {
                SizeAction::Wait
            };
        }
        self.wrong = true;
        if self.gave_up {
            return SizeAction::Wait;
        }
        let step = match self.last {
            Some((_, at)) if now.duration_since(at) < RESTORE_WAIT => return SizeAction::Wait,
            Some((SizeAction::Restart, _)) => SizeAction::Recreate,
            _ => SizeAction::Restart,
        };
        if full {
            self.last = None;
            self.gave_up = true;
            return SizeAction::GiveUp;
        }
        self.restores.push(now);
        self.last = Some((step, now));
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monitor that keeps failing is tried again after 1, 2, 4… up to 30 s; one that
    /// streamed a good while before it failed starts at 1 s again.
    #[test]
    fn a_failed_monitor_waits_longer_each_time() {
        let pauses: Vec<u64> = (1..=8).map(|n| retry_pause(n).as_secs()).collect();
        assert_eq!(pauses, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_pause(u32::MAX), RETRY_LONGEST);
        let t0 = Instant::now();
        let kind = ScreenKind::Virtual {
            spec: MonitorSpec {
                width: 640,
                height: 360,
                scale: 1.0,
            },
        };
        let mut s = Screen::starting(ScreenId::saved(1), 1, kind);
        s.failed_at(t0);
        s.failed_at(t0);
        assert_eq!(s.retry_at, Some(t0 + Duration::from_secs(2)));
        s.running_since = Some(t0);
        s.failed_at(t0 + Duration::from_secs(5));
        assert_eq!(s.failures, 3, "it streamed only a moment");
        s.running_since = Some(t0);
        s.failed_at(t0 + STREAMED_WELL);
        assert_eq!(s.failures, 1);
        assert_eq!(s.retry_at, Some(t0 + STREAMED_WELL + RETRY_FIRST));
    }

    /// The numbers are the protocol's.
    #[test]
    fn numbers_are_the_protocols() {
        let id = |n| ScreenId::from_wire(DisplayId(n));
        assert_eq!(id(3).wire(), DisplayId(3));
        assert_eq!(id(70_000).number(), 70_000);
    }

    const SHRUNK: Option<(u32, u32)> = Some((320, 240));

    fn secs(start: Instant, s: f64) -> Instant {
        start + Duration::from_secs_f64(s)
    }

    #[test]
    fn a_resized_monitor_is_restarted_once_and_then_left_to_settle() {
        let start = Instant::now();
        let mut guard = SizeGuard::default();
        assert_eq!(guard.check(None, start), SizeAction::Wait);
        assert_eq!(guard.check(SHRUNK, secs(start, 1.0)), SizeAction::Restart);
        // The size comes back a moment after the restart.
        assert_eq!(guard.check(SHRUNK, secs(start, 1.5)), SizeAction::Wait);
        assert_eq!(guard.check(None, secs(start, 2.0)), SizeAction::Restored);
        assert_eq!(guard.check(None, secs(start, 2.5)), SizeAction::Wait);
        assert_eq!(guard.restores.len(), 1);
    }

    #[test]
    fn a_size_that_does_not_come_back_makes_the_output_anew() {
        let start = Instant::now();
        let mut guard = SizeGuard::default();
        assert_eq!(guard.check(SHRUNK, start), SizeAction::Restart);
        assert_eq!(guard.check(SHRUNK, secs(start, 1.9)), SizeAction::Wait);
        assert_eq!(guard.check(SHRUNK, secs(start, 2.0)), SizeAction::Recreate);
        // The new output gets its time too; still wrong after that, a restart again.
        assert_eq!(guard.check(SHRUNK, secs(start, 3.0)), SizeAction::Wait);
        assert_eq!(guard.check(SHRUNK, secs(start, 4.0)), SizeAction::Restart);
        assert_eq!(guard.check(None, secs(start, 4.5)), SizeAction::Restored);
    }

    #[test]
    fn a_program_that_keeps_resizing_is_not_fought_forever() {
        let start = Instant::now();
        let mut guard = SizeGuard::default();
        let mut actions = Vec::new();
        // Resized again every 3 s, each time after the size came back.
        for i in 0..5 {
            let at = f64::from(i) * 3.0;
            actions.push(guard.check(SHRUNK, secs(start, at)));
            guard.check(None, secs(start, at + 0.5));
        }
        use SizeAction::*;
        assert_eq!(actions, [Restart, Restart, Restart, GiveUp, Wait]);
        // Still shrunk: no more restores.
        assert_eq!(guard.check(SHRUNK, secs(start, 20.0)), Wait);
        assert_eq!(guard.check(SHRUNK, secs(start, 90.0)), Wait);
        // Back by other means (the headset taken off and put on): laid out again.
        assert_eq!(guard.check(None, secs(start, 91.0)), Restored);
        // After a quiet minute the guard starts over.
        assert_eq!(guard.check(SHRUNK, secs(start, 92.0)), Restart);
    }
}
