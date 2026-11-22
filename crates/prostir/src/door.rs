//! Who may come in, and when the PC goes back to sleep: the pairing window, the
//! headsets that asked for this PC lately, the notice of a new headset nearby, the
//! settings window that keeps the program running, and, when the sockets came from
//! outside (systemd or the sockets' watcher), when the program leaves. The rules
//! without threads, files or the clock, so they are tested with made-up times; the
//! control loop (`main.rs`) holds a [`Door`].
//!
//! The pairing window is open for [`PAIRING_TIME`] from "Pair a new headset" (the
//! tray, the settings window, `prostir ctl pair`, the notice's button), and closes by
//! itself, by "Stop pairing", or once a headset pairs. While it is open the PC answers
//! every headset that asks (`crate::find`). A headset can pair whenever it asks over
//! a connection (a typed address, USB): its code protects that, as before.
//!
//! A program whose sockets came from outside leaves once nothing needs it: no session
//! and no monitors waiting (the control loop is idle), no settings window, no pairing
//! window, no notice waiting for its button, for [`QUIET`]; and not before
//! [`FIRST_WAIT`] from its start, which is how long a headset that asked has to
//! connect. The sockets stay with their owner, which starts it again when a headset
//! comes.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::api;
use crate::find::{Asker, PairingOpen};

/// How long the pairing window stays open.
pub const PAIRING_TIME: Duration = Duration::from_secs(180);
/// How long a program whose sockets came from outside stays with nothing to do.
pub const QUIET: Duration = Duration::from_secs(10);
/// How long a program whose sockets came from outside waits for a headset after it
/// starts: one that asked for the PC connects within this.
pub const FIRST_WAIT: Duration = Duration::from_secs(30);
/// A headset that asked counts as nearby for this long.
pub const NEARBY_FOR: Duration = Duration::from_secs(60);
/// A notice of a new headset nearby, at most once in this.
pub const NOTICE_EVERY: Duration = Duration::from_secs(60);
/// A notice waits for its button this long at most.
pub const NOTICE_TIME: Duration = Duration::from_secs(120);

/// How long the pairing window stays open in this run: [`PAIRING_TIME`], or in a
/// build for the end-to-end tests what `PROSTIR_TEST_PAIR_SECONDS` says.
pub fn pairing_time() -> Duration {
    #[cfg(feature = "fake-desktop")]
    if let Some(s) = std::env::var("PROSTIR_TEST_PAIR_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return Duration::from_secs(s);
    }
    PAIRING_TIME
}

/// [`QUIET`] and [`FIRST_WAIT`] in this run, or in a build for the end-to-end tests
/// both what `PROSTIR_TEST_QUIET_SECONDS` says.
fn leave_times() -> (Duration, Duration) {
    #[cfg(feature = "fake-desktop")]
    if let Some(s) = std::env::var("PROSTIR_TEST_QUIET_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return (Duration::from_secs(s), Duration::from_secs(s));
    }
    (QUIET, FIRST_WAIT)
}

/// At most so many headsets nearby are kept (made-up addresses must not grow it).
pub const MAX_NEARBY: usize = 16;
/// A headset's name as the PC shows it, at most so many characters.
pub const MAX_NAME_CHARS: usize = 32;

/// Whether a headset that asked wants the notice of a new headset nearby: it looks for
/// a PC to pair with (its query gives its name; a paired headset that asks without
/// this PC's tag, say right after an update, gives none and wants nothing), it is not
/// one of this PC's, it got no answer (the PC is paired), the pairing window closed.
pub fn wants_notice(asker: &Asker, pairing: bool) -> bool {
    !asker.answered && !asker.ours && !pairing && has_name(asker.name.as_str())
}

/// Whether `name` keeps a letter once cleaned (see [`clean_name`]), without making
/// anything.
fn has_name(name: &str) -> bool {
    name.chars()
        .any(|c| !c.is_whitespace() && !c.is_control() && !bidi(c))
}

/// A character that changes the direction of text.
fn bidi(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Whether the notice of a new headset may be shown at `now`, [`NOTICE_EVERY`] at
/// most, however many programs ask (the watcher, its child, the light phase under
/// systemd and the full program after it): the time of the last one is kept in the
/// file `mark` (in the user's runtime directory), written when one is shown.
pub fn notice_due(mark: &std::path::Path, now: std::time::SystemTime) -> bool {
    let last = std::fs::read_to_string(mark)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .map(|s| std::time::UNIX_EPOCH + Duration::from_secs(s));
    // A clock set back counts as long ago.
    let quiet = last.is_none_or(|last| {
        now.duration_since(last)
            .map_or(true, |since| since >= NOTICE_EVERY)
    });
    if quiet {
        let secs = now
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Err(e) = std::fs::write(mark, format!("{secs}\n")) {
            log::warn!(
                "cannot keep the time of the notice ({}): {e}",
                mark.display()
            );
        }
    }
    quiet
}

/// A name a headset gave (it comes from the network): without control and
/// direction-changing characters (which could make it read as something else), trimmed,
/// [`MAX_NAME_CHARS`] at most.
pub fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && !bidi(*c))
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A headset that asked for this PC lately, looking for a PC to pair with.
struct Near {
    /// Its name.
    label: String,
    from: SocketAddr,
    at: Instant,
}

/// When a program whose sockets came from outside leaves.
struct Leave {
    started: Instant,
    /// Since when nothing needs it.
    quiet_since: Option<Instant>,
    /// [`QUIET`] and [`FIRST_WAIT`] in this run.
    quiet: Duration,
    first_wait: Duration,
}

impl Leave {
    fn at(&self, quiet_since: Instant) -> Instant {
        (quiet_since + self.quiet).max(self.started + self.first_wait)
    }
}

#[derive(Default)]
pub struct Door {
    /// The pairing window: until when, and for how long it was opened.
    pairing: Option<(Instant, Duration)>,
    /// Its copy for the `find` thread.
    open: PairingOpen,
    /// How the last pairing attempt ended.
    last: Option<api::PairResult>,
    nearby: Vec<Near>,

    /// A notice waits for its button, until then.
    notice: Option<Instant>,
    /// The settings window is on the bus.
    window: bool,
    leave: Option<Leave>,
}

impl Door {
    /// `leaves`: the sockets came from outside, so the program leaves once nothing
    /// needs it (counted from `now`).
    pub fn new(open: PairingOpen, leaves: bool, now: Instant) -> Self {
        Self {
            open,
            leave: leaves.then(|| {
                let (quiet, first_wait) = leave_times();
                Leave {
                    started: now,
                    quiet_since: None,
                    quiet,
                    first_wait,
                }
            }),
            ..Self::default()
        }
    }

    /// Opens the pairing window at `now` for `time` (again from now if it was open):
    /// the seconds it stays open.
    pub fn open_pairing(&mut self, now: Instant, time: Duration) -> u32 {
        self.pairing = Some((now + time, time));
        self.last = None;
        self.open.set(true);
        time.as_secs() as u32
    }

    pub fn stop_pairing(&mut self) {
        self.pairing = None;
        self.open.set(false);
    }

    pub fn pairing_open(&self) -> bool {
        self.pairing.is_some()
    }

    /// A headset paired, named `name`: the pairing window closes.
    pub fn paired(&mut self, name: String) {
        self.stop_pairing();
        self.last = Some(api::PairResult::Paired { name });
        self.nearby.clear();
    }

    /// A pairing attempt failed: the codes did not match.
    pub fn wrong_code(&mut self) {
        self.last = Some(api::PairResult::Wrong);
    }

    /// A headset that is not one of this PC's asked at `now`. One that looks for a PC
    /// to pair with (it gave its name) is nearby, by its name cleaned ([`MAX_NEARBY`]
    /// at most, the latest kept); one that gave none (a paired headset without this
    /// PC's tag) is not anyone's business. Whether it wants the notice of a new headset
    /// (see [`wants_notice`]; whether one is shown now is [`notice_due`]'s).
    pub fn asked(&mut self, asker: &Asker, now: Instant) -> bool {
        let label = clean_name(asker.name.as_str());
        if label.is_empty() {
            return false;
        }
        self.nearby.retain(|n| n.from.ip() != asker.from.ip());
        if self.nearby.len() >= MAX_NEARBY {
            self.nearby.remove(0);
        }
        self.nearby.push(Near {
            label,
            from: asker.from,
            at: now,
        });
        wants_notice(asker, self.pairing.is_some())
    }

    /// A notice now waits for its button (`true`), or is gone.
    pub fn notice_up(&mut self, up: bool, now: Instant) {
        self.notice = up.then_some(now + NOTICE_TIME);
    }

    /// Whether the program leaves when nothing needs it (its sockets came from
    /// outside).
    pub fn leaves(&self) -> bool {
        self.leave.is_some()
    }

    /// A notice waits for its button now.
    pub fn notice_waits(&self) -> bool {
        self.notice.is_some()
    }

    /// The settings window came onto the bus or left it.
    pub fn window(&mut self, on: bool) {
        self.window = on;
    }

    /// The headsets that asked lately, by name or address, the latest last.
    pub fn nearby(&self) -> Vec<String> {
        self.nearby.iter().map(|n| n.label.clone()).collect()
    }

    /// The pairing part of the local API's state at `now`: while the window is open,
    /// or the PC shows a code (`pc_code`), or a headset waits for its code to be typed
    /// (`code_for`).
    pub fn pairing_state(
        &self,
        now: Instant,
        pc_code: Option<String>,
        code_for: Option<String>,
    ) -> Option<api::Pairing> {
        if self.pairing.is_none() && pc_code.is_none() && code_for.is_none() {
            return None;
        }
        let (left, total) = self.pairing.map_or((0, 0), |(until, total)| {
            let left = until
                .saturating_duration_since(now)
                .as_millis()
                .div_ceil(1000);
            (left as u32, total.as_secs() as u32)
        });
        Some(api::Pairing {
            seconds_left: left,
            seconds_total: total,
            pc_code,
            code_for,
            last: self.last.clone(),
        })
    }

    /// At `now`: the pairing window and the notice at their time, the headsets nearby
    /// that asked long ago forgotten. Whether anything changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let changed = self.pairing.is_some_and(|(until, _)| now >= until);
        if changed {
            self.stop_pairing();
        }
        if self.notice.is_some_and(|until| now >= until) {
            self.notice = None;
        }
        let before = self.nearby.len();
        self.nearby
            .retain(|n| now.saturating_duration_since(n.at) < NEARBY_FOR);
        changed || self.nearby.len() != before
    }

    /// Whether the program is needed for anything of the door's: the settings window,
    /// the pairing window, a notice waiting for its button.
    fn needed(&self) -> bool {
        self.window || self.pairing.is_some() || self.notice.is_some()
    }

    /// When the program leaves, if its sockets came from outside: `busy`, the control
    /// loop has something (a session, monitors waiting, a connection being set up, a
    /// goodbye going out). Called on every pass, at `now`.
    pub fn leave_at(&mut self, now: Instant, busy: bool) -> Option<Instant> {
        let needed = busy || self.needed();
        let leave = self.leave.as_mut()?;
        if needed {
            leave.quiet_since = None;
            return None;
        }
        let quiet = *leave.quiet_since.get_or_insert(now);
        Some(leave.at(quiet))
    }

    /// When the control loop has to look again for the door's sake, if ever.
    pub fn due(&self) -> Option<Instant> {
        let nearby = self.nearby.iter().map(|n| n.at + NEARBY_FOR).min();
        crate::brain::earliest([
            self.pairing.map(|(until, _)| until),
            self.notice,
            nearby,
            self.leave
                .as_ref()
                .and_then(|l| l.quiet_since.map(|q| l.at(q))),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asker(ours: bool, answered: bool, name: &str) -> Asker {
        Asker {
            from: "192.168.1.30:5000".parse().unwrap(),
            ours,
            name: crate::find::HeadsetName::new(name),
            answered,
        }
    }

    /// The window opens for three minutes, which the `find` thread sees, and closes
    /// at its time.
    #[test]
    fn the_pairing_window_closes_at_its_time() {
        let now = Instant::now();
        let open = PairingOpen::default();
        let mut door = Door::new(open.clone(), false, now);
        assert_eq!(door.open_pairing(now, PAIRING_TIME), 180);
        assert!(open.get());
        let state = door.pairing_state(now + Duration::from_millis(500), None, None);
        let state = state.unwrap();
        assert_eq!((state.seconds_left, state.seconds_total), (180, 180));
        assert_eq!(door.due(), Some(now + PAIRING_TIME));
        assert!(!door.expire(now + PAIRING_TIME - Duration::from_millis(1)));
        assert!(door.pairing_open());
        assert!(door.expire(now + PAIRING_TIME));
        assert!(!door.pairing_open() && !open.get());
        assert!(door.pairing_state(now + PAIRING_TIME, None, None).is_none());
        // A code shown on the PC is pairing too, without a time limit.
        let shown = door
            .pairing_state(now, Some("123 456".into()), None)
            .unwrap();
        assert_eq!(
            (shown.seconds_total, shown.pc_code.as_deref()),
            (0, Some("123 456"))
        );
    }

    /// A headset pairing closes the window at once; a wrong code leaves it open.
    #[test]
    fn the_pairing_window_closes_when_a_headset_pairs() {
        let now = Instant::now();
        let open = PairingOpen::default();
        let mut door = Door::new(open.clone(), false, now);
        door.open_pairing(now, PAIRING_TIME);
        door.asked(&asker(false, true, "Quest 3S"), now);
        door.wrong_code();
        assert!(door.pairing_open());
        let state = door
            .pairing_state(now, None, Some("Quest 3S".into()))
            .unwrap();
        assert_eq!(state.last, Some(api::PairResult::Wrong));
        door.paired("Quest 3S".into());
        assert!(!door.pairing_open() && !open.get());
        assert!(door.nearby().is_empty());
        // Opened again: nothing of the last attempt.
        door.open_pairing(now, PAIRING_TIME);
        assert!(door.pairing_state(now, None, None).unwrap().last.is_none());
        door.stop_pairing();
        assert!(!door.pairing_open());
    }

    /// A stranger the PC does not answer wants the notice; not while the window is
    /// open, nor the PC's own headsets, nor one that got an answer.
    #[test]
    fn a_stranger_outside_the_window_wants_the_notice() {
        let now = Instant::now();
        let mut door = Door::new(PairingOpen::default(), false, now);
        assert!(!door.asked(&asker(true, true, ""), now), "one of its own");
        assert!(door.asked(&asker(false, false, "Quest 3S"), now));
        assert!(
            !door.asked(&asker(false, false, ""), now),
            "a paired headset without this PC's tag: it looks for no PC to pair with"
        );
        assert!(
            !door.asked(&asker(false, false, " \u{202e}\n"), now),
            "no name left"
        );
        door.open_pairing(now, PAIRING_TIME);
        assert!(
            !door.asked(&asker(false, false, "Quest 3S"), now),
            "the window is open"
        );
        door.stop_pairing();
        assert!(
            !door.asked(&asker(false, true, "Quest 3S"), now),
            "it was answered"
        );
    }

    /// The notice once a minute at most, kept in a file every program reads: a second
    /// program (another start, the watcher's child) sees the first one's.
    #[test]
    fn the_notice_is_shown_once_a_minute_across_programs() {
        let mark = std::env::temp_dir().join(format!("prostir-notice-{}", std::process::id()));
        let _ = std::fs::remove_file(&mark);
        let now = std::time::SystemTime::now();
        assert!(notice_due(&mark, now));
        assert!(!notice_due(&mark, now + Duration::from_secs(30)));
        assert!(notice_due(&mark, now + NOTICE_EVERY));
        // A clock set back: long ago.
        assert!(notice_due(&mark, now - Duration::from_secs(3600)));
        std::fs::write(&mark, "nonsense").unwrap();
        assert!(notice_due(&mark, now));
        std::fs::remove_file(&mark).unwrap();
    }

    /// Made-up addresses keep at most sixteen entries; names are cleaned.
    #[test]
    fn nearby_is_capped_and_names_cleaned() {
        let now = Instant::now();
        let mut door = Door::new(PairingOpen::default(), false, now);
        for i in 0..100u8 {
            let mut a = asker(false, false, &format!("Headset {i}"));
            a.from = SocketAddr::from(([10, 0, 0, i], 5000));
            door.asked(&a, now);
        }
        assert_eq!(door.nearby().len(), MAX_NEARBY);
        assert_eq!(door.nearby().last().unwrap(), "Headset 99");
        assert_eq!(clean_name("  Quest\u{202e}3S\n\t "), "Quest3S");
        assert_eq!(clean_name(&"я".repeat(50)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(clean_name("\u{2066}\u{0007}"), "");
    }

    /// Nearby: by name (a headset that gives none is not listed), one entry for each
    /// address, forgotten a minute after it last asked.
    #[test]
    fn headsets_nearby_are_forgotten_after_a_minute() {
        let now = Instant::now();
        let mut door = Door::new(PairingOpen::default(), false, now);
        door.asked(&asker(false, false, ""), now);
        assert!(door.nearby().is_empty(), "no name: not looking to pair");
        door.asked(&asker(false, false, "Quest 3"), now);
        assert_eq!(door.nearby(), ["Quest 3"]);
        door.asked(
            &asker(false, false, "Quest 3S"),
            now + Duration::from_secs(1),
        );
        assert_eq!(door.nearby(), ["Quest 3S"]);
        assert_eq!(door.due(), Some(now + Duration::from_secs(1) + NEARBY_FOR));
        assert!(door.expire(now + Duration::from_secs(1) + NEARBY_FOR));
        assert!(door.nearby().is_empty());
        assert_eq!(door.due(), None);
    }

    /// With sockets from outside: not before the first 30 s, then 10 s after the
    /// last thing that needed it; the window, pairing or a notice keep it.
    #[test]
    fn the_program_leaves_once_nothing_needs_it() {
        let start = Instant::now();
        let mut door = Door::new(PairingOpen::default(), true, start);
        let s = Duration::from_secs;
        // A headset asked and never came: it waits for it.
        assert_eq!(door.leave_at(start, false), Some(start + FIRST_WAIT));
        assert_eq!(door.leave_at(start + s(5), true), None, "a session");
        assert_eq!(door.leave_at(start + s(40), false), Some(start + s(50)));
        assert_eq!(door.due(), Some(start + s(50)));
        assert_eq!(door.leave_at(start + s(45), false), Some(start + s(50)));
        door.window(true);
        assert_eq!(
            door.leave_at(start + s(46), false),
            None,
            "the settings window"
        );
        door.window(false);
        door.open_pairing(start + s(47), PAIRING_TIME);
        assert_eq!(door.leave_at(start + s(47), false), None, "pairing");
        door.stop_pairing();
        door.notice_up(true, start + s(48));
        assert_eq!(door.leave_at(start + s(48), false), None, "a notice");
        door.expire(start + s(48) + NOTICE_TIME);
        let at = start + s(48) + NOTICE_TIME;
        assert_eq!(door.leave_at(at, false), Some(at + QUIET));
        // Sockets of its own: it never leaves by itself.
        let mut own = Door::new(PairingOpen::default(), false, start);
        assert_eq!(own.leave_at(start + s(100), false), None);
    }
}
