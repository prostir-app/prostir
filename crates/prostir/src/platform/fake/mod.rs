//! A desktop that exists only inside this process, for the end-to-end tests
//! (`crates/prostir/tests/e2e`): built instead of the real platform with the `fake-desktop`
//! feature, never in a release. The core runs as it is (monitors, router, encoders,
//! the session loop, pairing) without KDE, Hyprland, PipeWire or a tray.
//!
//! What it does differently from a real PC:
//! - Monitors are pictures made here and streamed to the encoder like KWin streams
//!   its outputs (see `screen.rs`): the current picture when the encoder starts, then
//!   only when the picture changes (`PROSTIR_FAKE_FPS` times a second; 0, the
//!   default, is a still screen). `PROSTIR_FAKE_PICTURE=noise` makes the picture
//!   random pixels instead of grey bands, so frames are as big as on a busy screen.
//!   `PROSTIR_FAKE_SLOW_MONITORS_MS` makes each monitor take that long to make, and
//!   `PROSTIR_FAKE_BROKEN_MONITORS=2,3` makes those monitors (by number, from 1)
//!   impossible to make. `PROSTIR_FAKE_SLOW_CONNECT_MS` makes connecting to the
//!   desktop take that long.
//! - Sound is silence, 10 ms at a time, each slice starting with the time it was
//!   made (see [`audio`]), so a fake headset can tell how late it arrives; the
//!   microphone goes nowhere. The sound system says its changes as `pactl subscribe`
//!   lines through a pipe, so the real watcher of the sound system follows it.
//! - No windows, no mouse, no screenshots (its desktop has no [`crate::platform::Windows`]
//!   and no [`crate::platform::Pointer`]), no notifications, no USB, and no outside
//!   programs are run. The tray is a file a test writes menu
//!   items to (see [`tray`]).
//! - Another program can resize a monitor, as sharing it in Discord does on KDE: a
//!   test writes `resize Fake-2 320x240` to `$XDG_RUNTIME_DIR/fake-desktop` (see
//!   `desktop.rs`).
//! - The network is the real one: the connection to the headset is set up as on any
//!   Linux PC (`platform/net.rs`), so the tests measure what a user gets.
//! - It runs only when asked to with `PROSTIR_FAKE_DESKTOP=1`, and its settings live
//!   in `$XDG_CONFIG_HOME` only, which must be set: a fake build started by mistake
//!   never falls back to the user's own `~/.config`.
//! - The person at the PC types the pairing code in `PROSTIR_FAKE_TYPED_CODE`, if
//!   set, taking `PROSTIR_FAKE_TYPED_CODE_DELAY_MS` to do it; otherwise there is no
//!   dialog to type it in.
//!
//! It provides what the core uses of the platform contract (see `platform/mod.rs`);
//! helpers only the Linux platform calls itself (`audio::restore_after_crash`) are
//! left out.

mod desktop;
mod screen;

use std::net::Ipv4Addr;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub use desktop::connect;

/// The fake desktop was asked for: `PROSTIR_FAKE_DESKTOP=1`, as the tests set it.
fn asked_for() -> bool {
    std::env::var_os("PROSTIR_FAKE_DESKTOP").is_some_and(|v| v == "1")
}

/// Where per-user settings go: `$XDG_CONFIG_HOME` and nowhere else, and only when the
/// fake desktop was asked for.
pub fn config_home() -> Option<PathBuf> {
    if !asked_for() {
        return None;
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// One fake PC per settings directory; refuses to start unless asked for and given
/// one, so neither a test nor a fake build started by mistake touches the real settings.
pub fn single_instance(role: &str) -> Result<std::fs::File> {
    anyhow::ensure!(
        asked_for(),
        "this build has a fake desktop for tests; it runs only with PROSTIR_FAKE_DESKTOP=1"
    );
    let dir = config_home().context(
        "the fake desktop needs XDG_CONFIG_HOME set to a directory of its own (a test's)",
    )?;
    std::fs::create_dir_all(&dir)?;
    let file = super::lock::take(&dir.join("prostir-fake.lock"), role)
        .with_context(|| format!("a fake Prostir already runs with {}", dir.display()))?;
    log::warn!("fake desktop (a test build): settings in {}", dir.display());
    Ok(file)
}

/// The fake sockets' watcher with these settings, if one runs: its process id.
pub fn watcher() -> Option<u32> {
    super::lock::holder(&config_home()?.join("prostir-fake.lock"), "watcher")
}

/// The fake desktop needs nothing of a session.
pub fn import_session_environment() {}

/// Nothing outlives a fake run.
pub fn clean_up_after_crash() {}

pub fn give_memory_back() {}

/// Calls of the desktop that hang the next time they are made, and for how long (a
/// test writes `hang make 20` to `$XDG_RUNTIME_DIR/fake-desktop`, see `desktop.rs`).
static HANGS: std::sync::Mutex<Vec<(String, std::time::Duration)>> =
    std::sync::Mutex::new(Vec::new());

/// The next call at `point` (`make`, `place`, `encoder-stop`) hangs for `long`.
fn hang_next(point: &str, long: std::time::Duration) {
    HANGS.lock().unwrap().push((point.to_owned(), long));
}

/// A call at `point`: it hangs, if a test said so.
pub fn hang(point: &str) {
    let long = {
        let mut hangs = HANGS.lock().unwrap();
        let i = hangs.iter().position(|(p, _)| p == point);
        i.map(|i| hangs.remove(i).1)
    };
    if let Some(long) = long {
        log::info!("fake desktop: {point} hangs for {} s", long.as_secs());
        std::thread::sleep(long);
        log::info!("fake desktop: {point} is back");
    }
}

/// Where caches go: `$XDG_CACHE_HOME` and nowhere else, and only when the fake desktop
/// was asked for (as [`config_home`]).
pub fn cache_home() -> Option<PathBuf> {
    if !asked_for() {
        return None;
    }
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// What the fake hardware encoder is told to do (`PROSTIR_FAKE_ENCODER`): another
/// setting is another PC, probed anew.
pub fn encoder_drivers() -> Vec<String> {
    vec![format!(
        "fake encoder {}",
        std::env::var("PROSTIR_FAKE_ENCODER").unwrap_or_default()
    )]
}

pub fn hostname() -> String {
    "fake-pc".into()
}

pub fn broadcast_addresses() -> Vec<Ipv4Addr> {
    Vec::new()
}

/// Calls `on_text` with what a test writes to `$XDG_RUNTIME_DIR/NAME`, on a thread
/// that sleeps in the kernel until a file there is written (inotify), and removes the
/// file once read. A test makes the file under another name and moves it there, so it
/// is never read half written. `on_text` returns `false` to stop. Whether the thread
/// started.
fn watch_file(name: &'static str, on_text: impl Fn(String) -> bool + Send + 'static) -> bool {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return false;
    };
    let path = dir.join(name);
    let Ok(dir_c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a plain system call; the descriptor it returns is owned right below.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        log::warn!("fake desktop: no inotify for {name}");
        return false;
    }
    // SAFETY: `fd` is a new descriptor nothing else owns.
    let inotify = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: a valid descriptor and a NUL-terminated path.
    let watch = unsafe {
        libc::inotify_add_watch(
            inotify.as_raw_fd(),
            dir_c.as_ptr(),
            libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO,
        )
    };
    if watch < 0 {
        log::warn!("fake desktop: cannot watch {}", dir.display());
        return false;
    }
    // Moved aside before it is read, so a command written meanwhile is a file of its
    // own, not removed unread with this one.
    let taken = dir.join(format!("{name}.taken"));
    let take = move |path: &std::path::Path| {
        std::fs::rename(path, &taken).ok()?;
        let text = std::fs::read_to_string(&taken).ok();
        let _ = std::fs::remove_file(&taken);
        text
    };
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // Written before the watch began.
            if let Some(text) = take(&path)
                && !on_text(text)
            {
                return;
            }
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: reads into a buffer of the length given.
                let n =
                    unsafe { libc::read(inotify.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                // Each event: wd (4 bytes), mask (4), cookie (4), len (4), then a name
                // of `len` bytes padded with NULs.
                let mut named = false;
                let mut at = 0;
                let n = n as usize;
                while at + 16 <= n {
                    let len =
                        u32::from_ne_bytes(buf[at + 12..at + 16].try_into().unwrap()) as usize;
                    let raw = &buf[at + 16..(at + 16 + len).min(n)];
                    let event_name = raw.split(|b| *b == 0).next().unwrap_or_default();
                    named |= event_name == name.as_bytes();
                    at += 16 + len;
                }
                if named
                    && let Some(text) = take(&path)
                    && !on_text(text)
                {
                    return;
                }
            }
        })
        .is_ok()
}

/// No headset is ever plugged in by USB.
pub fn keep_adb_reverse(_port: u16) {}

/// No headset waits on USB.
pub fn usb_waiting() -> bool {
    false
}

/// In the test's runtime directory, shared by the watcher and its child as on a real
/// PC.
pub fn notice_mark() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(config_home)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prostir-notice")
}

/// In the settings directory (the fake PC has no Pictures folder).
pub fn screenshot_dir() -> PathBuf {
    config_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("screenshots")
}

/// A new, empty file there, of a name no other file has (as on a real PC).
pub fn screenshot_path() -> Result<PathBuf> {
    let dir = screenshot_dir();
    std::fs::create_dir_all(&dir)?;
    for n in 1..100 {
        let path = dir.join(format!("screenshot-{n}.png"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("no free name for a screenshot in {dir:?}")
}

/// Whether `path` is a file with something in it.
pub fn has_picture(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Silence to the headset; the headset microphone goes nowhere; a sound system in
/// memory whose changes are written as `pactl subscribe` lines, so the real watcher
/// (`platform/sound_events.rs`) follows it.
///
/// Every slice of sound starts with the `CLOCK_MONOTONIC` time it was made, in
/// nanoseconds (`u64`, little-endian): a fake headset on the same machine reads how
/// long each slice took to arrive (see `prostir-testkit`). With
/// `PROSTIR_FAKE_SOUND=silence` the slices are all zeros, as an idle PC's are.
pub mod audio {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use prostir_proto::{AUDIO_CHANNELS, AUDIO_RATE};

    use crate::events::EventSink;
    use crate::platform::sound::{OutputDevice, Route};
    use crate::platform::sound_events::{self, End, Indices, Reading, SoundSystem, Subscription};

    pub use crate::platform::sound_events::Watcher;

    /// 10 ms of 16-bit sound, as the real capture sends it.
    const SLICE_BYTES: usize = (AUDIO_RATE / 100 * AUDIO_CHANNELS * 2) as usize;

    pub struct AudioOut {
        stop: Arc<AtomicBool>,
    }

    impl AudioOut {
        /// Silence to the headset; with `PROSTIR_FAKE_NO_SOUND=1` the sound device
        /// cannot be made.
        pub fn start(on_sound: impl Fn(Vec<u8>) + Send + Sync + 'static) -> Result<Self> {
            if std::env::var_os("PROSTIR_FAKE_NO_SOUND").is_some_and(|v| v == "1") {
                anyhow::bail!("the fake desktop makes no sound device (PROSTIR_FAKE_NO_SOUND)");
            }
            super::hang("sound-start");
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let stamped = std::env::var_os("PROSTIR_FAKE_SOUND").is_none_or(|v| v != "silence");
            std::thread::Builder::new()
                .name("fake-sound".into())
                .spawn(move || {
                    let mut next = Instant::now();
                    while !stopped.load(Ordering::Relaxed) {
                        let mut pcm = vec![0; SLICE_BYTES];
                        if stamped {
                            pcm[..8].copy_from_slice(&monotonic_ns().to_le_bytes());
                        }
                        on_sound(pcm);
                        next += Duration::from_millis(10);
                        std::thread::sleep(next.saturating_duration_since(Instant::now()));
                    }
                })?;
            let mut sys = system();
            let sink = sys.number();
            sys.sink = Some(sink);
            // A device made anew is at the sound system's level for one (WirePlumber
            // gives full volume), not the one before.
            sys.level = Some((NEW_DEVICE_LEVEL, false));
            log::info!("fake desktop: the headset's sound device made (#{sink})");
            // The sink, and its monitor with the same number.
            sys.say("new", "sink", sink);
            sys.say("new", "source", sink);
            Ok(Self { stop })
        }
    }

    /// `CLOCK_MONOTONIC` now, in nanoseconds.
    fn monotonic_ns() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime writes into the timespec it is given.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    impl Drop for AudioOut {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let mut sys = system();
            if let Some(sink) = sys.sink.take() {
                sys.level = None;
                log::info!("fake desktop: the headset's sound device removed (#{sink})");
                sys.say("remove", "source", sink);
                sys.say("remove", "sink", sink);
            }
        }
    }

    impl AudioOut {
        /// Only in the headset.
        pub fn route(&self) -> Route {
            Route::HeadsetOnly
        }

        pub fn device(&self) -> OutputDevice {
            OutputDevice("fake_headset".into())
        }
    }

    /// The level a sound device made anew has.
    const NEW_DEVICE_LEVEL: u8 = 100;

    /// The fake sound system, as `pactl` would show it: Prostir's devices with their
    /// numbers, the headset's device's level, whether a program records, and the
    /// subscription its changes are written to, a `pactl subscribe` line each.
    struct System {
        sink: Option<u32>,
        mic: Option<u32>,
        /// Where the headset's device is, once something set or moved it.
        level: Option<(u8, bool)>,
        /// How many times the PC set it.
        writes: u64,
        /// A program records from the headset's microphone (a test says so).
        recording: bool,
        /// The number the next object gets.
        next: u32,
        /// The subscription running now: its number, and where its lines go.
        events: Option<(u64, std::fs::File)>,
        subscriptions: u64,
    }

    static SYSTEM: Mutex<System> = Mutex::new(System {
        sink: None,
        mic: None,
        level: None,
        writes: 0,
        recording: false,
        next: 1,
        events: None,
        subscriptions: 0,
    });

    fn system() -> std::sync::MutexGuard<'static, System> {
        SYSTEM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    impl System {
        /// A new object's number.
        fn number(&mut self) -> u32 {
            self.next += 1;
            self.next
        }

        /// Says a change on the subscription, if one runs; a line it has no room for
        /// is lost, as the real one would block.
        fn say(&mut self, what: &str, on: &str, index: u32) {
            self.say_together(&[(what, on, index)]);
        }

        /// Says the changes of one event in one write, so they come together, as the
        /// sound system's lines for one change do. Without that, a busy machine could
        /// wake the PC between them, and it would look twice at one change.
        fn say_together(&mut self, changes: &[(&str, &str, u32)]) {
            use std::fmt::Write as _;
            use std::io::Write;
            let text = changes
                .iter()
                .fold(String::new(), |mut text, (what, on, index)| {
                    let _ = writeln!(text, "Event '{what}' on {on} #{index}");
                    text
                });
            if let Some((_, lines)) = self.events.as_mut()
                && let Err(e) = lines.write_all(text.as_bytes())
                && e.kind() != std::io::ErrorKind::WouldBlock
            {
                self.events = None;
            }
        }

        /// Asking the sound system connects as a client, which it says too.
        fn asked(&mut self) {
            let client = self.number();
            self.say("new", "client", client);
            self.say("remove", "client", client);
        }
    }

    /// The slider of the headset's device moved to `level` (a test's command).
    pub fn moved(level: u8, muted: bool) {
        let mut sys = system();
        sys.level = Some((level, muted));
        if let Some(sink) = sys.sink {
            sys.say("change", "sink", sink);
        }
    }

    /// A program starts (`on`) or stops recording from the headset's microphone (a
    /// test's command).
    pub fn record(on: bool) {
        let mut sys = system();
        sys.recording = on;
        let stream = sys.number();
        let mut changes = vec![(if on { "new" } else { "remove" }, "source-output", stream)];
        if let Some(mic) = sys.mic {
            changes.push(("change", "source", mic));
        }
        sys.say_together(&changes);
    }

    /// The subscription ends, as when `pactl subscribe` dies (a test's command).
    pub fn stop_events() {
        system().events = None;
    }

    /// The headset's microphone as the fake PC's: what comes is counted and said in
    /// the log now and then; a program records while a test says so (`record on`).
    pub struct MicIn {
        got: std::sync::atomic::AtomicUsize,
    }

    impl MicIn {
        pub fn start() -> Result<Self> {
            let mut sys = system();
            let mic = sys.number();
            sys.mic = Some(mic);
            sys.say("new", "source", mic);
            Ok(Self {
                got: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        pub fn push(&self, pcm: &[u8]) {
            let before = self.got.fetch_add(pcm.len(), Ordering::Relaxed);
            // About once a second of mono sound.
            if before / 96_000 != (before + pcm.len()) / 96_000 {
                log::info!(
                    "fake desktop: the microphone got {} bytes",
                    before + pcm.len()
                );
            }
        }

        /// Everything that comes is counted, recorded or not.
        pub fn set_recording(&self, _on: bool) {}
    }

    impl Drop for MicIn {
        fn drop(&mut self) {
            let mut sys = system();
            if let Some(mic) = sys.mic.take() {
                sys.say("remove", "source", mic);
            }
        }
    }

    /// With `PROSTIR_FAKE_PACTL=1` the sound system is `pactl`, as on Linux: whatever
    /// the test put first on `PATH` under that name (`crates/prostir/tests/e2e/fake-pactl`).
    fn real_pactl() -> bool {
        std::env::var_os("PROSTIR_FAKE_PACTL").is_some_and(|v| v == "1")
    }

    /// As on Linux, with `pactl`; nothing to find out in memory.
    pub fn prepare() {
        if real_pactl() {
            crate::platform::pulse::prepare(&[]);
        }
    }

    /// The fake sound device for the headset: its level is what a test sets (`volume
    /// 40`, `volume 40 muted`, `drag 0 100 1000`: the slider from 0 to 100 in a
    /// second), and what the PC sets goes into the log. With `pactl`, Linux's.
    pub enum SinkVolume {
        Memory,
        Pactl(crate::platform::pulse::SinkVolume),
    }

    impl SinkVolume {
        pub fn open() -> Option<Self> {
            if real_pactl() {
                return crate::platform::pulse::SinkVolume::open().map(Self::Pactl);
            }
            Some(Self::Memory)
        }

        /// As `pactl` does it: the volume, then the mute, each a change of its own.
        pub fn set(&self, level: u8, muted: bool) {
            if let Self::Pactl(v) = self {
                return v.set(level, muted);
            }
            let mut sys = system();
            let was = sys.level;
            sys.level = Some((level, muted));
            sys.writes += 1;
            log::info!(
                "fake desktop: volume set to {level}{}",
                if muted { " muted" } else { "" }
            );
            if let Some(sink) = sys.sink {
                if was.map(|(l, _)| l) != Some(level) {
                    sys.say("change", "sink", sink);
                }
                if was.map(|(_, m)| m) != Some(muted) {
                    sys.say("change", "sink", sink);
                }
            }
        }

        /// Where the device is now, if known.
        pub fn level(&self) -> Option<(u8, bool)> {
            match self {
                Self::Memory => system().level,
                Self::Pactl(v) => v.level(),
            }
        }

        pub fn outdated(&self, writes: u64) -> bool {
            match self {
                Self::Memory => writes < system().writes,
                Self::Pactl(v) => v.outdated(writes),
            }
        }
    }

    /// Follows the fake sound system with the same watcher as the real one (or the
    /// real one through `pactl`).
    pub fn watch(events: EventSink) -> Option<Watcher> {
        if real_pactl() {
            return Watcher::start(crate::platform::pulse::Pactl::headset(), events);
        }
        Watcher::start(Fake, events)
    }

    struct Fake;

    /// Ends subscription number `.0`, if it still runs.
    struct Ended(u64);

    impl End for Ended {
        fn end(&self) {
            let mut sys = system();
            if sys.events.as_ref().is_some_and(|(n, _)| *n == self.0) {
                sys.events = None;
            }
        }
    }

    impl SoundSystem for Fake {
        fn subscribe(&mut self) -> std::io::Result<Subscription> {
            use std::os::fd::AsRawFd;
            let (lines, into) = sound_events::pipe()?;
            // SAFETY: fcntl on a descriptor this function owns.
            unsafe {
                let flags = libc::fcntl(into.as_raw_fd(), libc::F_GETFL);
                libc::fcntl(into.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            let mut sys = system();
            sys.subscriptions += 1;
            let n = sys.subscriptions;
            sys.events = Some((n, into));
            log::info!("fake desktop: the sound system's events start ({n})");
            Ok(Subscription {
                lines,
                end: Arc::new(Ended(n)),
            })
        }

        fn indices(&mut self) -> Option<Indices> {
            let mut sys = system();
            sys.asked();
            Some(Indices {
                sink: sys.sink,
                mic: sys.mic,
            })
        }

        fn volume(&mut self) -> Option<Reading> {
            let mut sys = system();
            sys.asked();
            let (level, muted) = sys.level?;
            Some(Reading {
                level,
                muted,
                writes: sys.writes,
            })
        }

        /// As slow as asking the real sound system (`pactl` takes a process start), and
        /// said in the log, so a test sees when and how often it is asked.
        fn recording(&mut self) -> Option<bool> {
            std::thread::sleep(Duration::from_millis(20));
            let mut sys = system();
            sys.asked();
            log::info!("fake desktop: asked whether a program records");
            Some(sys.recording && sys.mic.is_some())
        }
    }
}

pub mod sound_source {
    use std::sync::Arc;

    use crate::platform::{OutputKey, WindowList};

    /// No app plays sound on the fake PC.
    pub struct Watcher;

    impl Watcher {
        pub fn start(_windows: Option<Arc<dyn WindowList>>) -> Self {
            Self
        }

        pub fn output(&self) -> Option<OutputKey> {
            None
        }
    }
}

pub mod notifications {
    pub struct Watcher;

    /// The fake PC shows no notifications.
    pub fn watch(_show: impl Fn(String) + Send + 'static) -> Option<Watcher> {
        None
    }
}

/// Starting with the session, in memory: what is set goes to the log (`fake autostart:
/// on`), for the tests to read, and nothing outside the process changes.
pub mod autostart {
    use std::sync::atomic::{AtomicBool, Ordering};

    static ON: AtomicBool = AtomicBool::new(false);

    pub fn available() -> bool {
        true
    }

    pub fn enabled() -> bool {
        ON.load(Ordering::Relaxed)
    }

    pub fn set(on: bool) {
        ON.store(on, Ordering::Relaxed);
        log::info!("fake autostart: {}", if on { "on" } else { "off" });
    }

    pub fn first_start() {
        log::info!("fake autostart: the first start");
        set(true);
    }

    pub fn quit_until_login() {}

    /// No XDG entry of 0.1 in memory to change.
    pub fn migrate() -> bool {
        false
    }

    pub fn start_service() -> bool {
        false
    }

    /// A test never starts anything in the real systemd manager of this user.
    pub fn start_in_user_manager(_args: &[&str]) -> bool {
        false
    }

    pub fn socket_waits() -> bool {
        false
    }
}

/// A tray without an icon: a test "clicks" its items by writing their names, one a
/// line, to `$XDG_RUNTIME_DIR/fake-tray` (`forget`, `recenter`, `settings`, `pair`,
/// `pair stop`, `autostart on`, `autostart off`, `quit`, `sharp on`, `sharp off`,
/// `quality 15`, `quality 50`; `activate`: a click on the icon itself), which is
/// read and removed once written.
pub mod tray {
    use crate::events::EventSink;
    use crate::platform::{TrayCommand, TrayStatus};

    #[derive(Clone)]
    pub struct Handle;

    pub fn spawn(events: EventSink) -> Option<Handle> {
        super::watch_file("fake-tray", move |text| {
            for item in text.lines() {
                let command = match item.trim() {
                    "forget" => TrayCommand::ForgetHeadsets,
                    "recenter" => TrayCommand::Headset(prostir_proto::request::Asked::Known(
                        prostir_proto::request::Command::Recenter,
                    )),
                    "settings" => TrayCommand::ShowSettings,
                    // A click on the icon itself.
                    "activate" => TrayCommand::clicked(),
                    "pair" => TrayCommand::PairNewHeadset,
                    "pair stop" => TrayCommand::StopPairing,
                    "quit" => TrayCommand::Quit,
                    "autostart on" => TrayCommand::SetAutostart(true),
                    "autostart off" => TrayCommand::SetAutostart(false),
                    "sharp on" => TrayCommand::SetSharp(true),
                    "sharp off" => TrayCommand::SetSharp(false),
                    "quality 15" => TrayCommand::SetBitrate(15),
                    "quality 50" => TrayCommand::SetBitrate(50),
                    other => {
                        log::warn!("fake tray: no item {other:?}");
                        continue;
                    }
                };
                log::info!("fake tray: {item}");
                if !events.tray(command) {
                    return false;
                }
            }
            true
        })
        .then_some(Handle)
    }

    /// What the tray would show, in the log for the tests.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the same signature as the real tray's, which keeps the status"
    )]
    pub fn set_status(_handle: Option<&Handle>, status: TrayStatus) {
        log::info!(
            "fake tray: {} other headset(s), microphone denied: {}, encoding: {}, desktop: {}",
            status.others,
            status.mic_denied,
            status.encoding.as_deref().unwrap_or("as it should"),
            if status.desktop_stuck {
                "not responding"
            } else {
                "fine"
            }
        );
    }

    pub fn set_battery(_handle: Option<&Handle>, _battery: Option<(u8, bool)>) {}

    pub fn set_settings(_handle: Option<&Handle>, _bitrate: u32, _sharp: bool, _counting: bool) {}

    pub fn set_pairing(_handle: Option<&Handle>, _code: Option<String>) {}
}

pub mod dialogs {
    use crate::platform::NoCode;

    /// The code the test's person at the PC types (`PROSTIR_FAKE_TYPED_CODE`), after
    /// thinking for `PROSTIR_FAKE_TYPED_CODE_DELAY_MS`.
    pub fn ask_code() -> Result<String, NoCode> {
        let code = std::env::var("PROSTIR_FAKE_TYPED_CODE").map_err(|_| NoCode::NoDialog)?;
        let delay: u64 = std::env::var("PROSTIR_FAKE_TYPED_CODE_DELAY_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(0);
        log::info!("fake desktop: the person at the PC types the code in {delay} ms");
        std::thread::sleep(std::time::Duration::from_millis(delay));
        Ok(code)
    }

    /// Into the log instead of onto a screen.
    pub fn notify(title: &str, body: &str) {
        log::info!("notification: {title}: {body}");
    }

    pub fn notify_and_wait(title: &str, body: &str) -> bool {
        notify(title, body);
        false
    }

    /// A notification with a button: in the log, and a test "presses" the button by
    /// writing `click` to `$XDG_RUNTIME_DIR/fake-notice`, or closes it with `close`.
    pub struct Notice {
        read: std::fs::File,
    }

    pub fn notice(title: &str, body: &str, button: &str, _plain: &str) -> Option<Notice> {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills the two descriptors, owned right below.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return None;
        }
        // SAFETY: new descriptors nothing else owns.
        let (read, write) = unsafe {
            (
                std::fs::File::from_raw_fd(fds[0]),
                std::fs::File::from_raw_fd(fds[1]),
            )
        };
        let write = std::sync::Mutex::new(Some(write));
        let watched = super::watch_file("fake-notice", move |text| {
            use std::io::Write;
            let mut write = write.lock().unwrap();
            match text.trim() {
                "click" => {
                    if let Some(w) = write.as_mut() {
                        let _ = w.write_all(b"pair\n");
                    }
                }
                "close" => {}
                other => {
                    log::warn!("fake notice: no {other:?}");
                    return true;
                }
            }
            // The notice is done: its end closes.
            *write = None;
            false
        });
        if !watched {
            return None;
        }
        log::info!("notification: {title}: {body} [{button}]");
        Some(Notice { read })
    }

    impl Notice {
        pub fn fd(&self) -> std::os::fd::RawFd {
            use std::os::fd::AsRawFd;
            self.read.as_raw_fd()
        }

        pub fn check(&mut self) -> Option<bool> {
            use std::io::Read;
            let mut buf = [0u8; 16];
            match self.read.read(&mut buf) {
                Ok(n) if n > 0 => Some(buf[..n].starts_with(b"pair")),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => None,
                _ => Some(false),
            }
        }
    }
}
