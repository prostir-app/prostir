//! What the PC keeps between runs, with one owner: the settings (`settings.txt`), the
//! set of screens with the rows the headset shows them in (`screens.txt`) and when
//! each paired headset was last seen (`headsets.txt`), all plain `key value` text in
//! `~/.config/prostir/`.
//!
//! - What is kept is what the person wants, not what came out of it: the codec chosen
//!   stays chosen when the graphics card made another one.
//! - Command-line options hold for this run only and are never written (see [`Run`]);
//!   a change made while running is.
//! - Lines this version does not know are kept and written back as they were, so a
//!   newer version run before loses nothing.
//! - A change only marks the file; [`Store::flush`], once a pass of the main loop,
//!   hands the text to a thread of its own that writes it (the latest of each file)
//!   atomically and durably. A crash loses at most that one pass; a half-written file
//!   is a temporary file beside the old one (`<file>.tmp.<process>.<number>`),
//!   removed on the next start.
//! - The first start of this version moves the settings of 0.1 over from
//!   `monitors.txt` (and the counter switch from `count`), which stay as they are for
//!   a downgrade to read; 0.1's `count-notice`, and `count` unless it is off, go once
//!   `settings.txt` is there.
//! - Screens are numbered from a counter that never goes back ([`Store::new_id`]):
//!   before the first number of each block of [`ID_BLOCK`] is given out, the end of the
//!   block is written and on disk, so a crash can leave numbers unused but never give
//!   one out twice.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use log::{info, warn};
use prostir_api::Counting;
use prostir_layout::desk::Entry;
use prostir_proto::Codec;
use prostir_proto::state::{Access, perms, role};

use crate::displays::MONITORS_NO_LIMITS;
use crate::platform::MonitorSpec;
use crate::screen::ScreenId;

mod screens;
mod settings;

pub use screens::{LEGACY_LAPTOP, Physical, PhysicalKey, SavedScreens, startup_set};
use screens::{migrate, parse_screens, screens_text};
pub use settings::{Settings, is_word, perm_words};
use settings::{hex, parse_settings, settings_text};

/// The file format this version writes. 3: clearer text is written only when chosen;
/// 4: the quality too (see `settings::parse_settings`).
const VERSION: u32 = 4;
/// Numbers of screens are reserved this many at a time (see [`Store::new_id`]).
pub const ID_BLOCK: u32 = 16;
/// The first number the counter gives: above every number earlier builds gave out
/// (monitors from 0, window panels 10 to 12, the laptop screen 100), so a new screen
/// never takes a place the headset kept for an old one.
const FIRST_ID: u32 = 101;
/// How long quitting waits for the files to be written.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// The video quality the PC offers, Mbit/s per monitor: a quality out of it (written
/// into `settings.txt` by hand, `--bitrate`) holds at its nearest end.
pub const BITRATES: std::ops::RangeInclusive<u32> = 5..=100;

/// Mbit/s for each monitor of 2560x1440 or more over USB while the person did not
/// choose the quality: text on a large monitor stays sharp through a big change (from
/// 34 to 50 dB of the text's PSNR on the first frame of a new 2560x1440 picture against
/// 33 Mbit/s). A cable carries it; more would add little (51 dB at 100), and CBR fills
/// every frame up to its share, while the headset's low-latency decoder says it takes
/// up to 70 Mbit/s.
pub const USB_LARGE: u32 = 60;

/// `mbps` in [`BITRATES`].
pub fn bitrate_in_range(mbps: u32) -> u32 {
    mbps.clamp(*BITRATES.start(), *BITRATES.end())
}

/// What holds for this run only, from the command line: never written.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub bitrate: Option<u32>,
    pub codec: Option<Codec>,
    pub software: Option<bool>,
    /// Monitors from `-m`: the set until the person changes it in the headset.
    pub screens: Option<SavedScreens>,
}

/// A file's version and the lines this version does not know.
#[derive(Debug, Clone, Default, PartialEq)]
struct Kept {
    version: u32,
    unknown: Vec<String>,
    /// Headsets with a mistyped `access` line: their line stays as it was, among the
    /// unknown ones, for the user to put right, and they have no rights meanwhile,
    /// whatever other line they have.
    mistyped: std::collections::BTreeSet<String>,
    /// The well-formed `access` lines of those headsets, written back as they were.
    beside_mistyped: BTreeMap<String, Access>,
}

impl Kept {
    /// The head of the file: the version (never lower than one that wrote it) and the
    /// unknown lines.
    fn head(&self) -> String {
        let mut text = format!("version {}\n", self.version.max(VERSION));
        for line in &self.unknown {
            text.push_str(line);
            text.push('\n');
        }
        text
    }
}

/// When a paired headset last had a session, and its app then.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seen {
    /// Seconds since 1970.
    pub at: u64,
    /// The headset app's version (`0.2.0-dev+p6`), as its hello said.
    pub app: String,
}

/// The file `headsets.txt`: `seen KEY SECONDS [APP]`, a line for each headset, KEY its
/// whole key in hex. Lines it cannot read are left out.
fn parse_seen_headsets(text: &str) -> BTreeMap<String, Seen> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "seen" {
                return None;
            }
            let key = words.next()?;
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let at = words.next()?.parse().ok()?;
            let app = words.next().unwrap_or_default().to_owned();
            Some((key.to_ascii_lowercase(), Seen { at, app }))
        })
        .collect()
}

fn seen_headsets_text(seen: &BTreeMap<String, Seen>) -> String {
    seen.iter()
        .map(|(key, s)| {
            let app: String = s.app.chars().filter(|c| !c.is_whitespace()).collect();
            if app.is_empty() {
                format!("seen {key} {}\n", s.at)
            } else {
                format!("seen {key} {} {app}\n", s.at)
            }
        })
        .collect()
}

/// The writes waiting for the writer thread, the latest of each file.
enum Job {
    Write(PathBuf, String),
    /// Says when everything before it is on disk.
    Sync(Sender<()>),
    Stop,
}

/// Writes `text` to `path` so that it survives a crash or a power cut: a whole new
/// file or the old one, never half of either.
fn write_durably(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    prostir_proto::secure::write_atomic(path, text.as_bytes())?;
    // The rename itself is kept only once the directory is on disk.
    std::fs::File::open(dir)?.sync_all()
}

fn spawn_writer() -> Option<(Sender<Job>, Receiver<()>)> {
    let (tx, jobs) = mpsc::channel::<Job>();
    let (done_tx, done) = mpsc::channel();
    std::thread::Builder::new()
        .name("store-writer".into())
        .spawn(move || {
            let mut stop = false;
            while !stop {
                let Ok(first) = jobs.recv() else { break };
                // The latest text of each file.
                let mut latest: BTreeMap<PathBuf, String> = BTreeMap::new();
                let mut synced = Vec::new();
                for job in std::iter::once(first).chain(jobs.try_iter()) {
                    match job {
                        Job::Write(path, text) => {
                            latest.insert(path, text);
                        }
                        Job::Sync(done) => synced.push(done),
                        Job::Stop => stop = true,
                    }
                }
                for (path, text) in latest {
                    if let Err(e) = write_durably(&path, &text) {
                        warn!("cannot save {}: {e}", path.display());
                    }
                }
                for done in synced {
                    let _ = done.send(());
                }
            }
            let _ = done_tx.send(());
        })
        .inspect_err(|e| warn!("no thread to save settings in: {e}"))
        .ok()?;
    Some((tx, done))
}

/// `~/.config/prostir`, where `questmon` was before the rename.
pub fn config_dir() -> Option<PathBuf> {
    let base = crate::platform::config_home()?;
    let dir = base.join("prostir");
    let old = base.join("questmon");
    if !dir.exists() && old.exists() {
        let _ = std::fs::rename(&old, &dir);
    }
    Some(dir)
}

/// Prostir never ran for this user (nothing of it in `dir`, `~/.config/prostir`): not
/// its settings, its screens, the settings of 0.1, its key or a paired headset.
pub fn first_start(dir: &Path) -> bool {
    [
        "settings.txt",
        "screens.txt",
        "monitors.txt",
        "identity",
        "paired_headsets",
    ]
    .iter()
    .all(|name| !dir.join(name).exists())
}

/// The settings and the set of screens, owned by the main loop.
pub struct Store {
    /// `~/.config/prostir`; none: nothing is saved (no home directory).
    dir: Option<PathBuf>,
    settings: Settings,
    settings_kept: Kept,
    screens: SavedScreens,
    screens_kept: Kept,
    /// The next number to give, and the end of the block reserved on disk.
    next: u32,
    reserved: u32,
    run: Run,
    settings_changed: bool,
    screens_changed: bool,
    /// When each paired headset last had a session (`headsets.txt`), by its key in
    /// hex.
    seen: BTreeMap<String, Seen>,
    seen_changed: bool,
    writer: Option<(Sender<Job>, Receiver<()>)>,
}

impl Store {
    /// The store of `dir` (moving the settings of 0.1 over), with what holds for this
    /// run only.
    pub fn open(dir: Option<PathBuf>, run: Run) -> Self {
        let mut store = Self {
            dir,
            settings: Settings::default(),
            settings_kept: Kept::default(),
            screens: SavedScreens::default(),
            screens_kept: Kept::default(),
            next: FIRST_ID,
            reserved: FIRST_ID,
            run,
            settings_changed: false,
            screens_changed: false,
            seen: BTreeMap::new(),
            seen_changed: false,
            writer: None,
        };
        store.load();
        store.writer = store.dir.as_ref().and_then(|_| spawn_writer());
        store
    }

    /// A number for a new screen, never given before: from the block reserved on disk,
    /// reserving the next block (and waiting until that is written) when this one is
    /// used up.
    pub fn new_id(&mut self) -> ScreenId {
        if self.next >= self.reserved {
            self.reserved = self.next + ID_BLOCK;
            self.screens_changed = true;
            self.flush_and_wait();
        }
        let id = ScreenId::saved(self.next);
        self.next += 1;
        id
    }

    /// Writes what changed and waits until it is on disk.
    fn flush_and_wait(&mut self) {
        self.flush();
        if let Some((tx, _)) = &self.writer {
            let (done_tx, done) = mpsc::channel();
            if tx.send(Job::Sync(done_tx)).is_ok() && done.recv_timeout(CLOSE_WAIT).is_err() {
                warn!(
                    "the screens' numbers not saved within {} s",
                    CLOSE_WAIT.as_secs()
                );
            }
        }
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(name))
    }

    fn load(&mut self) {
        let Some(dir) = self.dir.clone() else { return };
        // A write cut short leaves its temporary file; the real one is whole.
        // 0.1 wrote `monitors.txt` the same way.
        for name in [
            "settings.txt",
            "screens.txt",
            "headsets.txt",
            "monitors.txt",
        ] {
            prostir_proto::secure::remove_leftovers(&dir.join(name));
        }
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        if let Some(text) = read("headsets.txt") {
            self.seen = parse_seen_headsets(&text);
        }
        let (settings, screens) = (read("settings.txt"), read("screens.txt"));
        if settings.is_none() && screens.is_none() {
            // 0.1's counter switch had a file of its own, written only when the person
            // used it, and 0.1 counted until turned off: so on was no answer to the
            // question, and only an off stays (no, and not asked again), also where
            // 0.1 kept no monitors.
            let count = if read("count").is_some_and(|t| t.trim() == "off") {
                Counting::No
            } else {
                Counting::Unasked
            };
            // A first start: nothing to move over.
            let Some(old) = read("monitors.txt") else {
                self.settings.count = count;
                self.settings_changed = count == Counting::No;
                return;
            };
            (self.settings, self.screens) = migrate(&old);
            self.settings.count = count;
            info!("settings moved over from monitors.txt");
            self.settings_changed = true;
            self.screens_changed = !self.screens.specs.is_empty();
            self.start_counter(None);
            return;
        }
        if let Some(text) = settings {
            (self.settings, self.settings_kept) = parse_settings(&text);
            // The answer is in settings.txt now: 0.1's notice of the counter goes, and
            // its switch too, but for an off, kept so that a downgrade to 0.1 (which
            // counts until turned off) does not count again.
            let _ = std::fs::remove_file(dir.join("count-notice"));
            if read("count").is_some_and(|t| t.trim() != "off") {
                let _ = std::fs::remove_file(dir.join("count"));
            }
        }
        let mut next = None;
        if let Some(text) = screens {
            let (set, kept, counter) = parse_screens(&text);
            next = counter;
            if set.specs.is_empty() && counter.is_none() {
                // Nothing in it could be read: kept aside, so the next write does not
                // wipe out what went wrong.
                warn!("screens.txt has no monitor in it; kept as screens.txt.bad");
                let _ = std::fs::copy(dir.join("screens.txt"), dir.join("screens.txt.bad"));
            } else {
                (self.screens, self.screens_kept) = (set, kept);
            }
        }
        self.start_counter(next);
    }

    /// The counter goes on above every number saved and every number the file says may
    /// have been given out (`saved`), and never below [`FIRST_ID`].
    fn start_counter(&mut self, saved: Option<u32>) {
        let above = self.screens.highest().map_or(0, |n| n + 1);
        self.next = saved.unwrap_or(0).max(above).max(FIRST_ID);
        self.reserved = self.next;
    }

    /// The settings that hold now: as saved, but for what the command line says for
    /// this run.
    pub fn wanted(&self) -> Settings {
        Settings {
            bitrate: bitrate_in_range(self.run.bitrate.unwrap_or(self.settings.bitrate)),
            bitrate_chosen: self.settings.bitrate_chosen || self.run.bitrate.is_some(),
            codec: self.run.codec.unwrap_or(self.settings.codec),
            software: self.run.software.unwrap_or(self.settings.software),
            ..self.settings.clone()
        }
    }

    /// Changes settings the command line has no say in.
    pub fn change(&mut self, change: impl FnOnce(&mut Settings)) {
        let before = self.settings.clone();
        change(&mut self.settings);
        self.settings_changed |= self.settings != before;
    }

    /// The input right of the headset with key `key`, as the person at the PC chose
    /// it: kept in its `access` line. `None` for a headset whose line is mistyped,
    /// which stays as it was for the person to put right; else whether it changed.
    pub fn set_input(&mut self, key: &[u8; 32], on: bool) -> Option<bool> {
        if self.settings_kept.mistyped.contains(&hex(key)) {
            return None;
        }
        let mut changed = false;
        self.change(|s| changed = s.set_input(key, on));
        Some(changed)
    }

    /// Video quality chosen while running: from now on it holds, over the command line.
    pub fn set_bitrate(&mut self, mbps: u32) {
        self.run.bitrate = None;
        self.change(|s| {
            s.bitrate = mbps;
            s.bitrate_chosen = true;
        });
    }

    /// How to encode, chosen in the headset menu: it holds from now on, over the
    /// command line.
    pub fn set_encoding(&mut self, bitrate: u32, sharp: bool, codec: Codec, software: bool) {
        self.run.bitrate = None;
        self.run.codec = None;
        self.run.software = None;
        self.change(|s| {
            // Chosen when it changes: a headset that sends what it shows changes
            // nothing.
            if bitrate != s.bitrate {
                s.bitrate = bitrate;
                s.bitrate_chosen = true;
            }
            s.choose_clear_text(sharp);
            s.codec = codec;
            s.software = software;
        });
    }

    /// Starts this run with `monitors` from the command line, if any (see
    /// [`startup_set`]).
    pub fn start_with(&mut self, monitors: &[MonitorSpec]) {
        match startup_set(&self.screens, monitors) {
            Some(set) if !monitors.is_empty() => self.run.screens = Some(set),
            // The first one: kept once it changes.
            Some(set) => self.screens = set,
            None => {}
        }
    }

    /// The set of monitors this run: the saved one, or the one from the command line.
    pub fn screens(&self) -> &SavedScreens {
        self.run.screens.as_ref().unwrap_or(&self.screens)
    }

    /// The set as it is now. `chosen`: the person changed which monitors there are,
    /// so it is saved even when the run started with monitors from the command line;
    /// otherwise a change to a set from the command line stays in this run.
    pub fn set_screens(&mut self, set: SavedScreens, chosen: bool) {
        if chosen {
            self.run.screens = None;
        }
        match &mut self.run.screens {
            Some(run) => *run = set,
            None => {
                self.screens_changed |= self.screens != set;
                self.screens = set;
            }
        }
    }

    /// Headset `key` has a session now, or its session ended: seen now, with its app
    /// (`app`, from its hello; none keeps the one known).
    pub fn headset_seen(&mut self, key: &[u8; 32], at: u64, app: Option<&str>) {
        let seen = self.seen.entry(hex(key)).or_default();
        seen.at = at;
        if let Some(app) = app.filter(|a| !a.is_empty()) {
            app.clone_into(&mut seen.app);
        }
        self.seen_changed = true;
    }

    /// When headset `key` last had a session, and its app then, if known.
    pub fn last_seen(&self, key: &[u8; 32]) -> Option<&Seen> {
        self.seen.get(&hex(key))
    }

    /// Forgets when the headsets not in `paired` were seen (they were forgotten).
    pub fn keep_seen_of(&mut self, paired: &[[u8; 32]]) {
        let keep: Vec<String> = paired.iter().map(hex).collect();
        let before = self.seen.len();
        self.seen.retain(|key, _| keep.contains(key));
        self.seen_changed |= self.seen.len() != before;
    }

    /// Hands what changed to the writer thread.
    pub fn flush(&mut self) {
        let mut jobs = Vec::new();
        if std::mem::take(&mut self.seen_changed)
            && let Some(path) = self.path("headsets.txt")
        {
            jobs.push((path, seen_headsets_text(&self.seen)));
        }
        if std::mem::take(&mut self.settings_changed)
            && let Some(path) = self.path("settings.txt")
        {
            jobs.push((path, settings_text(&self.settings, &self.settings_kept)));
        }
        if std::mem::take(&mut self.screens_changed)
            && let Some(path) = self.path("screens.txt")
        {
            let text = screens_text(&self.screens, &self.screens_kept, self.reserved);
            jobs.push((path, text));
        }
        for (path, text) in jobs {
            match &self.writer {
                Some((tx, _)) => {
                    let _ = tx.send(Job::Write(path, text));
                }
                // Without the thread, here and now.
                None => {
                    if let Err(e) = write_durably(&path, &text) {
                        warn!("cannot save {}: {e}", path.display());
                    }
                }
            }
        }
    }

    /// Writes what is left and waits for it (at most [`CLOSE_WAIT`]).
    pub fn close(mut self) {
        self.flush();
        if let Some((tx, done)) = self.writer.take() {
            let _ = tx.send(Job::Stop);
            if done.recv_timeout(CLOSE_WAIT).is_err() {
                warn!("settings not saved within {} s", CLOSE_WAIT.as_secs());
            }
        }
    }
}

#[cfg(test)]
mod tests;
