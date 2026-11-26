//! The one place the headset's own files are read and written: once at the start,
//! and from then on by the `persist` thread.
//!
//! The thread is handed a file's text through a slot of its own: the newest text of a
//! file replaces one still waiting (the newest wins), and a file being written while a
//! new text comes is written again after. The texts move between three buffers for
//! each file, the main thread's draft, the slot and the thread's own, which are
//! swapped and never made anew. The main thread holds the lock only to swap a buffer,
//! the thread only to take one; the write itself (with its `fsync`) is done without
//! it.
//!
//! Android may start the app again in the same process. A thread an earlier start
//! left (it did not stop in time) must not write its older texts over the newer
//! start's: the newer start waits for it a little before it reads the files, and from
//! then on owns the folder ([`WRITERS`]), so the older thread writes nothing more.

#![expect(
    clippy::disallowed_methods,
    reason = "the files' writer: the shell's thread that reads and writes the headset's files"
)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::warn;

use super::{Dirty, File, LoadError, Skipped};
use crate::layout::{self, Desk, Placement};
use crate::profiles::{self, Slots};
use crate::settings::Settings;

/// How long a start waits for the `persist` thread of an earlier start, still
/// writing, before it reads the files (at the start, before the first frame).
const LEFT_FOR: Duration = Duration::from_secs(2);

/// Which start writes each folder's files, by its number. Held through each write, so
/// a start that takes a folder over knows no write of an older start is under way,
/// and none begins after.
static WRITERS: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());
/// The number of the last start.
static STARTS: AtomicU64 = AtomicU64::new(0);
/// Threads earlier starts left, still writing when they stopped.
static LEFT: Mutex<Vec<Left>> = Mutex::new(Vec::new());

/// A `persist` thread that did not stop in time, and its folder.
struct Left {
    dir: PathBuf,
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the files in `dir` hold, and the thread that writes them from now on. A
/// thread an earlier start left on these files is waited for first (two seconds at
/// most); one still not done writes nothing more.
pub fn start(dir: &Path) -> (Loaded, Saver) {
    start_after(dir, LEFT_FOR)
}

fn start_after(dir: &Path, left_for: Duration) -> (Loaded, Saver) {
    wait_for_left(dir, left_for);
    let start = STARTS.fetch_add(1, Ordering::Relaxed) + 1;
    {
        let mut writers = locked(&WRITERS);
        match writers.iter_mut().find(|(d, _)| d.as_path() == dir) {
            Some(writer) => writer.1 = start,
            None => writers.push((dir.to_owned(), start)),
        }
    }
    let loaded = Loaded::read(dir);
    let saver = Saver::start(dir, start, loaded.untouched);
    (loaded, saver)
}

/// Waits until the threads earlier starts left on `dir` end, for `within` at most,
/// and joins those that did; the others are kept for a later start.
fn wait_for_left(dir: &Path, within: Duration) {
    let until = Instant::now() + within;
    let left: Vec<Left> = locked(&LEFT).extract_if(.., |l| l.dir == dir).collect();
    for l in left {
        if l.shared.ended_by(until) {
            if l.thread.join().is_err() {
                warn!("the persist thread of an earlier start panicked");
            }
        } else {
            warn!("the persist thread of an earlier start is still writing: it writes no more");
            locked(&LEFT).push(l);
        }
    }
}

/// Writes `text` as `file` in `dir` now, whole or not at all: for what the app's files
/// are to hold before it starts (the selftest's settings).
pub fn write_now(dir: &Path, file: File, text: &str) -> io::Result<()> {
    prostir_proto::secure::write_atomic(&dir.join(file.name()), text.as_bytes())
}

/// What the files held at the start.
pub struct Loaded {
    pub settings: Settings,
    pub desk: Desk,
    pub profiles: Slots,
    pub keyboard: Option<Placement>,
    /// Files that are there but could not be read nor set aside: not written in this
    /// start, so what they hold is not lost.
    pub untouched: Dirty,
}

impl Loaded {
    /// Reads the files in `dir`; one that is not there, or cannot be read, is as if
    /// empty. One this version does not read (an older version's, or one cut short)
    /// is set aside as `*.old` (see [`readable`]).
    fn read(dir: &Path) -> Self {
        let text = |file: File| std::fs::read_to_string(dir.join(file.name())).ok();
        let mut untouched = Dirty::default();
        Self {
            settings: text(File::Settings)
                .map(|t| Settings::parse(&t))
                .unwrap_or_default(),
            desk: readable(dir, File::Desk, Desk::parse, &mut untouched).unwrap_or_default(),
            profiles: readable(dir, File::Profiles, profiles::parse, &mut untouched)
                .unwrap_or_else(profiles::empty),
            keyboard: readable(dir, File::Keyboard, layout::parse_keyboard, &mut untouched)
                .flatten(),
            untouched,
        }
    }
}

/// What `file` in `dir` holds, read by `parse`; `None` when it is not there or cannot
/// be read. Lines `parse` passed over are said in the log, once. A file `parse` does
/// not read (no format line: an older version's, a newer format, an empty one), or
/// one that cannot be read at all, is renamed to `<name>.old` in its place, replacing
/// an older one, and the app starts as if it were not there: the log says so, once.
/// One that cannot be set aside either is marked `untouched`: this start does not
/// write it.
fn readable<T>(
    dir: &Path,
    file: File,
    parse: impl FnOnce(&str) -> Result<(T, Skipped), LoadError>,
    untouched: &mut Dirty,
) -> Option<T> {
    let path = dir.join(file.name());
    let why = match std::fs::read_to_string(&path) {
        Ok(text) => match parse(&text) {
            Ok((read, skipped)) => {
                if let Some(first) = skipped.first {
                    warn!(
                        "{}: {} lines passed over (the first: line {first}), the rest read",
                        path.display(),
                        skipped.count
                    );
                }
                return Some(read);
            }
            Err(e) => e.to_string(),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => "not text".to_owned(),
        Err(e) => e.to_string(),
    };
    let old = dir.join(format!("{}.old", file.name()));
    match std::fs::rename(&path, &old) {
        Ok(()) => warn!(
            "{} is not read ({why}): set aside as {}, the app starts without it",
            path.display(),
            old.display()
        ),
        Err(e) => {
            warn!(
                "{} is not read ({why}), and cannot be set aside ({e}): it is not written in this start",
                path.display()
            );
            untouched.mark(file);
        }
    }
    None
}

/// What the thread says back.
#[derive(Debug, PartialEq, Eq)]
pub enum PersistEvent {
    /// `file` could not be written; the next save of it tries again.
    Failed { file: File, error: String },
}

const FILES: usize = File::ALL.len();

/// A file's text on its way to the thread.
#[derive(Default)]
struct Slot {
    text: String,
    /// `text` waits to be written.
    queued: bool,
}

#[derive(Default)]
struct State {
    slots: [Slot; FILES],
    /// Where the thread looks for the next file waiting: after the one it wrote last,
    /// so a file saved over and over does not keep the others waiting.
    next_from: usize,
    /// The thread is writing a file.
    busy: bool,
    /// Asked to end: the thread writes what waits, then ends.
    stop: bool,
    /// The thread has ended (or never started): files are written in place.
    finished: bool,
    /// Writes done, for the tests.
    #[cfg(test)]
    writes: u32,
    /// The thread takes nothing while held, for the tests.
    #[cfg(test)]
    held: bool,
    /// The thread waits before writing the text it took, for the tests.
    #[cfg(test)]
    held_in_write: bool,
}

impl State {
    /// The next file waiting, unless the thread is held.
    fn next(&self) -> Option<usize> {
        #[cfg(test)]
        if self.held {
            return None;
        }
        (0..FILES)
            .map(|k| (self.next_from + k) % FILES)
            .find(|&i| self.slots.get(i).is_some_and(|s| s.queued))
    }

    /// Something is still to be written or being written.
    fn pending(&self) -> bool {
        self.busy || self.slots.iter().any(|s| s.queued)
    }
}

/// The lock and the one condition both sides wait on: work for the thread, a write
/// done or the thread ended for the main thread.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        locked(&self.state)
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_for<'a>(&self, state: MutexGuard<'a, State>, left: Duration) -> MutexGuard<'a, State> {
        self.changed
            .wait_timeout(state, left)
            .map_or_else(|e| e.into_inner().0, |(state, _)| state)
    }

    /// Waits until the thread has ended, until `until` at most; true when it has.
    fn ended_by(&self, until: Instant) -> bool {
        let mut state = self.lock();
        while !state.finished {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self.wait_for(state, left);
        }
        true
    }
}

/// The `persist` thread of one start of the app, and the main thread's side of it.
/// Dropped, it writes what waits and ends within [`Saver::STOPS_WITHIN`].
pub struct Saver {
    shared: Arc<Shared>,
    dir: PathBuf,
    /// This start's number (see [`WRITERS`]).
    start: u64,
    /// Where each file is written.
    paths: [PathBuf; FILES],
    /// The main thread's buffer for each file's text, swapped into its slot.
    drafts: [String; FILES],
    events: Receiver<PersistEvent>,
    /// Kept for writing in place when the thread is not there.
    to_app: Sender<PersistEvent>,
    /// Files this start does not write (see [`Loaded::untouched`]).
    untouched: Dirty,
    thread: Option<JoinHandle<()>>,
}

impl Saver {
    /// How long the main thread waits for what waits to be written when the app is
    /// paused, the headset taken off, or the app closes.
    pub const STOPS_WITHIN: Duration = Duration::from_millis(300);

    /// Starts the thread, which writes the files in `dir` for start `start`.
    fn start(dir: &Path, start: u64, untouched: Dirty) -> Self {
        let paths = File::ALL.map(|f| dir.join(f.name()));
        let shared = Arc::new(Shared::default());
        let (to_app, events) = mpsc::channel();
        let thread = {
            let writer = Writer {
                shared: shared.clone(),
                dir: dir.to_owned(),
                start,
                paths: paths.clone(),
                to_app: to_app.clone(),
            };
            std::thread::Builder::new()
                .name("persist".into())
                .spawn(move || writer.run())
                .inspect_err(|e| warn!("no persist thread, files are written in place: {e}"))
                .ok()
        };
        if thread.is_none() {
            shared.lock().finished = true;
        }
        Self {
            shared,
            dir: dir.to_owned(),
            start,
            paths,
            drafts: Default::default(),
            events,
            to_app,
            untouched,
            thread,
        }
    }

    /// Hands `file` to the thread, its text written by `write` into a buffer kept from
    /// the last time. A text of it still waiting is replaced. Without the thread (it
    /// could not start, or it ended) the file is written in place.
    pub fn save(&mut self, file: File, write: impl FnOnce(&mut String)) {
        if self.untouched.has(file) {
            return;
        }
        let i = file.index();
        let Some(draft) = self.drafts.get_mut(i) else {
            return;
        };
        draft.clear();
        write(draft);
        let mut state = self.shared.lock();
        if !state.finished {
            if let Some(slot) = state.slots.get_mut(i) {
                std::mem::swap(&mut slot.text, draft);
                slot.queued = true;
            }
            drop(state);
            self.shared.changed.notify_all();
            return;
        }
        drop(state);
        if let Some(path) = self.paths.get(i) {
            write_file(file, path, (&self.dir, self.start), draft, &self.to_app);
        }
    }

    /// Waits until everything handed over is written, for `within` at most; true when
    /// it is.
    pub fn flush(&self, within: Duration) -> bool {
        let until = Instant::now() + within;
        let mut state = self.shared.lock();
        while state.pending() && !state.finished {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self.shared.wait_for(state, left);
        }
        !state.pending()
    }

    /// What the thread said since the last call.
    pub fn try_recv(&self) -> Option<PersistEvent> {
        self.events.try_recv().ok()
    }

    /// Asks the thread to write what waits and end, and waits for it for `within` at
    /// most. A thread that did not end by then is left (the log says so): the next
    /// start in the same process waits for it a little, and then it writes no more.
    pub fn stop(&mut self, within: Duration) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.lock().stop = true;
        self.shared.changed.notify_all();
        if !self.shared.ended_by(Instant::now() + within) {
            warn!(
                "the persist thread did not stop within {} ms",
                within.as_millis()
            );
            locked(&LEFT).push(Left {
                dir: self.dir.clone(),
                shared: self.shared.clone(),
                thread,
            });
        } else if thread.join().is_err() {
            warn!("the persist thread panicked");
        }
    }

    /// Holds the thread (it takes nothing) or lets it go on.
    #[cfg(test)]
    fn hold(&self, held: bool) {
        self.shared.lock().held = held;
        self.shared.changed.notify_all();
    }

    #[cfg(test)]
    fn writes(&self) -> u32 {
        self.shared.lock().writes
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        self.stop(Self::STOPS_WITHIN);
    }
}

/// The thread's side.
struct Writer {
    shared: Arc<Shared>,
    dir: PathBuf,
    start: u64,
    paths: [PathBuf; FILES],
    to_app: Sender<PersistEvent>,
}

/// However the thread ends, even in a panic: it is said to have ended, so nothing
/// waits for it, and the main thread writes in place from then on.
struct Ends<'a>(&'a Shared);

impl Drop for Ends<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.busy = false;
        state.finished = true;
        drop(state);
        self.0.changed.notify_all();
    }
}

impl Writer {
    /// Takes the next file waiting, writes it, and again, until asked to end with
    /// nothing waiting.
    fn run(self) {
        let _ends = Ends(&self.shared);
        let mut work = String::new();
        let mut state = self.shared.lock();
        loop {
            if let Some(i) = state.next() {
                if let Some(slot) = state.slots.get_mut(i) {
                    std::mem::swap(&mut slot.text, &mut work);
                    slot.queued = false;
                }
                state.next_from = (i + 1) % FILES;
                state.busy = true;
                #[cfg(test)]
                {
                    self.shared.changed.notify_all();
                    while state.held_in_write {
                        state = self.shared.wait(state);
                    }
                }
                drop(state);
                if let (Some(file), Some(path)) = (File::ALL.get(i), self.paths.get(i)) {
                    write_file(*file, path, (&self.dir, self.start), &work, &self.to_app);
                }
                state = self.shared.lock();
                state.busy = false;
                #[cfg(test)]
                {
                    state.writes += 1;
                }
                self.shared.changed.notify_all();
            } else if state.stop && !state.pending() {
                return;
            } else {
                state = self.shared.wait(state);
            }
        }
    }
}

/// Writes `text` as `file` at `path` (whole or not at all) for the start of `dir` and
/// `start`, unless a newer start has the folder; says so when it cannot.
fn write_file(
    file: File,
    path: &Path,
    (dir, start): (&Path, u64),
    text: &str,
    to_app: &Sender<PersistEvent>,
) {
    let writers = locked(&WRITERS);
    if !writers.iter().any(|(d, s)| d == dir && *s == start) {
        drop(writers);
        warn!("{} is written by a newer start: not saved", path.display());
        return;
    }
    let written = prostir_proto::secure::write_atomic(path, text.as_bytes());
    drop(writers);
    if let Err(e) = written {
        warn!("cannot save {}: {e}", path.display());
        let _ = to_app.send(PersistEvent::Failed {
            file,
            error: e.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DisplayKey, PcId};
    use prostir_proto::state::DisplayId;

    /// A folder of its own for each test, empty.
    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("prostir-persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn saver(dir: &Path) -> Saver {
        start_after(dir, Duration::ZERO).1
    }

    fn read(dir: &Path, file: File) -> String {
        std::fs::read_to_string(dir.join(file.name())).unwrap_or_default()
    }

    fn release(shared: &Shared) {
        shared.lock().held = false;
        shared.changed.notify_all();
    }

    /// Saves of a file while it waits replace each other: the newest is written, once.
    #[test]
    fn the_newest_text_wins() {
        let dir = folder("newest");
        let mut saver = saver(&dir);
        saver.hold(true);
        for n in 0..100 {
            saver.save(File::Desk, |out| {
                use std::fmt::Write as _;
                let _ = writeln!(out, "{n}");
            });
        }
        assert!(!saver.flush(Duration::from_millis(20)), "held");
        assert_eq!(read(&dir, File::Desk), "");
        saver.hold(false);
        assert!(saver.flush(Duration::from_secs(10)));
        assert_eq!(read(&dir, File::Desk), "99\n");
        assert_eq!(saver.writes(), 1);
        assert_eq!(saver.try_recv(), None);
        drop(saver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A newer text that comes while the older one is being written is written after
    /// it: twice, the newest last.
    #[test]
    fn a_newer_text_while_the_older_is_written() {
        let dir = folder("while");
        let mut saver = saver(&dir);
        saver.shared.lock().held_in_write = true;
        saver.save(File::Settings, |out| out.push_str("1\n"));
        let mut state = saver.shared.lock();
        while !state.busy {
            state = saver.shared.wait(state);
        }
        drop(state);
        saver.save(File::Settings, |out| out.push_str("2\n"));
        saver.shared.lock().held_in_write = false;
        saver.shared.changed.notify_all();
        assert!(saver.flush(Duration::from_secs(10)));
        assert_eq!(saver.writes(), 2);
        assert_eq!(read(&dir, File::Settings), "2\n");
        drop(saver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Several files waiting are written the arrangement first, and a file saved again
    /// and again does not keep the others waiting.
    #[test]
    fn files_take_turns() {
        let mut state = State::default();
        for slot in &mut state.slots {
            slot.queued = true;
        }
        assert_eq!(state.next(), Some(File::Desk.index()));
        state.next_from = File::Settings.index();
        assert_eq!(state.next(), Some(File::Settings.index()));
        state.next_from = 0;
        state.slots[File::Desk.index()].queued = false;
        assert_eq!(state.next(), Some(File::Profiles.index()));
        state.next_from = File::Keyboard.index() + 1;
        state.slots[File::Desk.index()].queued = true;
        assert_eq!(state.next(), Some(File::Settings.index()));
    }

    /// Every file handed over is written by the flush, each with its own text; the
    /// buffers keep their memory, and a shorter text after a longer one is whole.
    #[test]
    fn a_flush_writes_every_file() {
        let dir = folder("flush");
        let mut saver = saver(&dir);
        for file in File::ALL {
            saver.save(file, |out| out.push_str("a long first text\n"));
        }
        assert!(saver.flush(Duration::from_secs(10)));
        for file in File::ALL {
            saver.save(file, |out| out.push_str(file.name()));
        }
        assert!(saver.flush(Duration::from_secs(10)));
        for file in File::ALL {
            assert_eq!(read(&dir, file), file.name());
        }
        drop(saver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that cannot be written is told, every time; the next save tries again
    /// and, once it can, writes it.
    #[test]
    fn a_failed_write_is_told_and_tried_again() {
        let parent = folder("failed");
        // Not there yet: nothing can be written in it.
        let dir = parent.join("later");
        let mut saver = saver(&dir);
        for _ in 0..2 {
            saver.save(File::Profiles, |out| out.push_str("[0]\n"));
            assert!(saver.flush(Duration::from_secs(10)));
            match saver.try_recv() {
                Some(PersistEvent::Failed { file, error }) => {
                    assert_eq!(file, File::Profiles);
                    assert!(!error.is_empty());
                }
                None => panic!("the failure is told"),
            }
        }
        std::fs::create_dir_all(&dir).unwrap();
        saver.save(File::Profiles, |out| out.push_str("[1]\n"));
        assert!(saver.flush(Duration::from_secs(10)));
        assert_eq!(saver.try_recv(), None);
        assert_eq!(read(&dir, File::Profiles), "[1]\n");
        drop(saver);
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Stopped, the thread writes what waits and ends; the main thread waits for it.
    /// After it, a save is written in place.
    #[test]
    fn stopping_writes_what_waits() {
        let dir = folder("stop");
        let mut saver = saver(&dir);
        saver.hold(true);
        saver.save(File::Settings, |out| out.push_str("mic off\n"));
        saver.hold(false);
        saver.stop(Duration::from_secs(10));
        assert!(saver.thread.is_none());
        assert_eq!(read(&dir, File::Settings), "mic off\n");
        saver.save(File::Settings, |out| out.push_str("mic on\n"));
        assert_eq!(read(&dir, File::Settings), "mic on\n");
        // Stopped twice, or dropped after: nothing more to wait for.
        saver.stop(Duration::from_secs(10));
        drop(saver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The thread ended by a panic is said to have ended: nothing waits for it, and a
    /// save is written in place.
    #[test]
    fn a_thread_that_ended_leaves_saving_in_place() {
        let dir = folder("ended");
        let mut saver = saver(&dir);
        let shared = saver.shared.clone();
        let panicked = std::thread::spawn(move || {
            let _ends = Ends(&shared);
            panic!("the thread's end, as a panic leaves it");
        });
        assert!(panicked.join().is_err());
        assert!(saver.shared.lock().finished);
        saver.save(File::Keyboard, |out| out.push_str("0 0 0 0 0 0 0 1 0.5\n"));
        assert!(saver.flush(Duration::from_secs(10)));
        assert_eq!(read(&dir, File::Keyboard), "0 0 0 0 0 0 0 1 0.5\n");
        drop(saver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A thread an earlier start left (it did not stop in time) never writes its older
    /// text over the newer start's: still held when the newer start has waited long
    /// enough, it writes nothing once let go.
    #[test]
    fn a_left_thread_never_writes_over_a_newer_start() {
        let dir = folder("left");
        let mut old = saver(&dir);
        old.hold(true);
        old.save(File::Settings, |out| out.push_str("mic off\n"));
        let old_shared = old.shared.clone();
        let started = Instant::now();
        old.stop(Duration::from_millis(30));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!old_shared.lock().finished, "left");
        drop(old);
        // The newer start waits for it a little, then takes the folder over.
        let (loaded, mut new) = start_after(&dir, Duration::from_millis(50));
        assert!(
            loaded.settings.mic,
            "nothing of the older start was written"
        );
        new.save(File::Settings, |out| out.push_str("mic on\n"));
        assert!(new.flush(Duration::from_secs(10)));
        release(&old_shared);
        assert!(old_shared.ended_by(Instant::now() + Duration::from_secs(10)));
        assert_eq!(read(&dir, File::Settings), "mic on\n");
        drop(new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A left thread that is done while the newer start waits for it is joined, and
    /// the newer start reads what it wrote.
    #[test]
    fn a_left_thread_is_waited_for() {
        let dir = folder("waited");
        let mut old = saver(&dir);
        old.hold(true);
        old.save(File::Settings, |out| out.push_str("mic off\n"));
        let old_shared = old.shared.clone();
        old.stop(Duration::from_millis(30));
        drop(old);
        let letting_go = {
            let shared = old_shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                release(&shared);
            })
        };
        let (loaded, new) = start_after(&dir, Duration::from_secs(10));
        letting_go.join().unwrap();
        assert!(old_shared.lock().finished);
        assert!(
            !loaded.settings.mic,
            "the older start's text, written before reading"
        );
        assert!(!locked(&LEFT).iter().any(|l| l.dir == dir), "joined");
        drop(new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The files of an earlier start come back; those not there are as if empty.
    #[test]
    fn the_start_reads_what_was_saved() {
        let dir = folder("read");
        let (empty, mut saver) = start_after(&dir, Duration::ZERO);
        assert_eq!(empty.settings, Settings::default());
        assert!(empty.desk.arrangement().placements.is_empty());
        assert!(empty.profiles.is_empty() && empty.keyboard.is_none());
        let settings = Settings {
            mic: false,
            ..Settings::default()
        };
        saver.save(File::Settings, |out| settings.write_text(out));
        let mut profiles = profiles::empty();
        profiles.push(profiles::Profile {
            name: Some("Робота".into()),
            auto: Vec::new(),
            monitors: vec![prostir_proto::state::Mode::new(2560, 1440)],
            primary: 0,
            shown: Vec::new(),
            places: Vec::new(),
            frame: None,
            rows: Vec::new(),
        });
        saver.save(File::Profiles, |out| profiles::write_text(out, &profiles));
        let place = Placement {
            position: glam::Vec3::new(0.0, 1.0, -0.5),
            rotation: glam::Quat::IDENTITY,
            width: 0.5,
        };
        saver.save(File::Keyboard, |out| layout::write_keyboard(out, &place));
        let mut desk = Desk::default();
        let three = DisplayKey::new(PcId([3; 8]), DisplayId(3));
        desk.keep(
            [(three, place)],
            &std::collections::HashMap::new(),
            ((glam::Vec3::ZERO, 0.0), layout::RADIUS, 0.0),
            false,
        );
        saver.save(File::Desk, |out| desk.write_text(out));
        saver.stop(Duration::from_secs(10));
        drop(saver);
        let (back, _saver) = start_after(&dir, Duration::ZERO);
        assert_eq!(back.settings, settings);
        assert_eq!(back.profiles, profiles);
        assert_eq!(back.keyboard, Some(place));
        assert_eq!(back.desk.arrangement().placements[&three], place);
        for file in [File::Desk, File::Profiles, File::Keyboard] {
            assert!(!dir.join(format!("{}.old", file.name())).exists(), "read");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that cannot be read nor set aside (here a folder where the file goes,
    /// and another where `*.old` goes) is not written in this start: what it holds is
    /// not lost.
    #[test]
    fn a_file_that_cannot_be_read_is_left_alone() {
        let dir = folder("alone");
        std::fs::create_dir_all(dir.join("layout.txt")).unwrap();
        std::fs::write(dir.join("layout.txt").join("inside"), "kept").unwrap();
        std::fs::create_dir_all(dir.join("layout.txt.old")).unwrap();
        std::fs::write(dir.join("layout.txt.old").join("inside"), "kept").unwrap();
        let (loaded, mut saver) = start_after(&dir, Duration::ZERO);
        assert!(loaded.untouched.has(File::Desk) && !loaded.untouched.has(File::Settings));
        saver.save(File::Desk, |out| out.push_str("format 2\n"));
        saver.save(File::Settings, |out| out.push_str("mic off\n"));
        assert!(saver.flush(Duration::from_secs(10)));
        assert_eq!(
            std::fs::read_to_string(dir.join("layout.txt").join("inside")).unwrap(),
            "kept"
        );
        assert_eq!(read(&dir, File::Settings), "mic off\n");
        drop(saver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file this version does not read (0.1's, one without `format 2`, one of a
    /// newer format, an empty one, rubbish) is set aside as `*.old`, whole, an older
    /// `*.old` replaced, and the start goes on as without it. One of this format with
    /// a line it does not know is read, without that line, and kept.
    #[test]
    fn files_not_read_are_set_aside() {
        let dir = folder("aside");
        let desk = "format 2\ncenter 0 1.6 0 0\nradius 1.3\n\
                    0123456789abcdef:3 0 1.6 -1.2 0 0 0 1 0.8 2560x1440\n";
        let keyboard = "format 2\nkeyboard 0 0.8 -0.4 -0.7071 0 0 0.7071 0.42\n";
        let not_read: [&[u8]; 5] = [
            b"center 0 1.6 0 0\n3 0 1.6 -1.2 0 0 0 1 0.8 2560x1440\n",
            b"",
            b"format 3\ncenter 0 1.6 0 0\n",
            b"\xff\xfe\x00rubbish",
            b"\nformat 2\n",
        ];
        for (n, text) in not_read.iter().enumerate() {
            for file in [File::Desk, File::Profiles, File::Keyboard] {
                std::fs::write(dir.join(file.name()), text).unwrap();
            }
            let (loaded, saver) = start_after(&dir, Duration::ZERO);
            drop(saver);
            assert!(loaded.desk.arrangement().placements.is_empty(), "case {n}");
            assert!(loaded.desk.arrangement().center.is_none(), "case {n}");
            assert!(loaded.keyboard.is_none(), "case {n}");
            assert!(loaded.profiles.is_empty(), "case {n}");
            for file in [File::Desk, File::Profiles, File::Keyboard] {
                assert!(!dir.join(file.name()).exists(), "case {n}: moved");
                let old = std::fs::read(dir.join(format!("{}.old", file.name()))).unwrap();
                assert_eq!(old, *text, "case {n}: whole");
            }
        }
        // Files of this version are read, and the `*.old` ones stay as they are; a line
        // a newer version added is passed over, and the file kept.
        std::fs::write(dir.join(File::Profiles.name()), "format 2\nlens 2\n").unwrap();
        std::fs::write(dir.join(File::Desk.name()), format!("{desk}lens 2\n")).unwrap();
        std::fs::write(dir.join(File::Keyboard.name()), keyboard).unwrap();
        let (loaded, saver) = start_after(&dir, Duration::ZERO);
        drop(saver);
        let three = DisplayKey::new(
            PcId([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
            DisplayId(3),
        );
        assert!(loaded.desk.arrangement().placements.contains_key(&three));
        assert_eq!(loaded.desk.arrangement().sizes[&three], (2560, 1440));
        assert_eq!(loaded.desk.arrangement().radius, Some(1.3));
        assert!((loaded.keyboard.unwrap().width - 0.42).abs() < 1e-6);
        assert_eq!(
            std::fs::read_to_string(dir.join(File::Desk.name())).unwrap(),
            format!("{desk}lens 2\n")
        );
        assert!(dir.join("layout.txt.old").exists());
        assert!(
            dir.join(File::Profiles.name()).exists(),
            "read, not set aside"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
