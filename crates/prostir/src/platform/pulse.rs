//! The sound system through `pactl` (PulseAudio's tool, on PipeWire's PulseAudio
//! server): the headset's output device read and set for the shared volume, and the
//! [`SoundSystem`] the watcher of `sound_events.rs` follows. The same on every Linux
//! desktop; the fake desktop uses it too when a test puts a stand-in `pactl` on its
//! `PATH` (`PROSTIR_FAKE_PACTL=1`).
//!
//! The control loop never waits for `pactl`: reads happen on the watcher's thread,
//! writes on a thread of their own ("sound-device"), each `pactl` with a time limit,
//! and what the control loop asks ([`SinkVolume::level`]) is the last reading.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::Duration;

use log::{info, warn};

use super::process::{run, spawn_tied};
use super::sound_events::{End, Indices, Reading, SoundSystem, Subscription};

/// Prostir's output device for the headset (made in `linux/audio.rs`).
pub const SINK: &str = "prostir_headset";
/// The headset microphone, as programs record from it.
pub const MIC: &str = "prostir_mic";

/// `pactl` answers at once while PipeWire is well.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// `pactl` with its output in English, as it is parsed.
pub fn pactl_command() -> Command {
    let mut cmd = Command::new("pactl");
    cmd.env("LC_ALL", "C");
    cmd
}

/// What `pactl args` printed, if it did what it was asked.
pub fn pactl(args: &[&str]) -> Option<String> {
    let out = run(pactl_command().args(args), TIMEOUT).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Whether this PC can share its volume with the headset: 0 not known yet, 1 yes, 2
/// no (see [`prepare`]).
static SHARES: AtomicU8 = AtomicU8::new(0);

/// Finds out once, on a thread of its own, whether the PC can share its volume: the
/// programs in `needs` are there (`pw-cli`, which makes the device), and `pactl` is new
/// enough to read a device's level (`get-sink-volume`, PulseAudio 14). Until it knows,
/// the volume is not shared.
pub fn prepare(needs: &'static [&'static str]) {
    let _ = std::thread::Builder::new()
        .name("sound-check".into())
        .spawn(move || {
            let tools = needs.iter().all(|p| {
                run(Command::new(p).arg("--version"), TIMEOUT).is_ok_and(|o| o.status.success())
            });
            let version = pactl(&["--version"]).and_then(|v| pactl_major(&v));
            let yes = tools && version.is_some_and(|v| v >= 14);
            if !yes {
                info!("volume: not shared with the headset (pactl {version:?}, tools {tools})");
            }
            SHARES.store(if yes { 1 } else { 2 }, Ordering::SeqCst);
        });
}

/// The major version in `pactl --version`: `pactl 17.0-134-g77d25`.
fn pactl_major(text: &str) -> Option<u32> {
    let word = text.lines().next()?.strip_prefix("pactl ")?;
    word.split(['.', '-', ' ']).next()?.parse().ok()
}

/// The loudest channel of `pactl get-sink-volume`, in percent:
/// `Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: ...`.
pub fn parse_volume(text: &str) -> Option<u32> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("Volume:"))?;
    line.split_whitespace()
        .filter_map(|w| w.strip_suffix('%')?.parse::<u32>().ok())
        .max()
}

/// `Mute: yes` or `Mute: no`.
pub fn parse_mute(text: &str) -> Option<bool> {
    match text.trim().strip_prefix("Mute:")?.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// The number of the device named `name` in `pactl list short sinks` (or sources).
pub fn index_in(list: &str, name: &str) -> Option<u32> {
    list.lines().find_map(|l| {
        let mut cols = l.split('\t');
        let index = cols.next()?.trim().parse().ok()?;
        (cols.next() == Some(name)).then_some(index)
    })
}

/// The state of source `name` in `list` (`pactl list short sources`): its last column,
/// `RUNNING`, `IDLE` or `SUSPENDED`.
pub fn state_in<'a>(list: &'a str, name: &str) -> Option<&'a str> {
    list.lines().find_map(|l| {
        let cols: Vec<&str> = l.split('\t').collect();
        (cols.get(1) == Some(&name)).then(|| cols.last().unwrap_or(&"").trim())
    })
}

/// A write asked for: its number, and the level.
struct Write {
    n: u64,
    level: u8,
    muted: bool,
}

/// The headset's output device as the PC reads and sets it: one read or write at a
/// time (a level is never read between the two halves of a write, the volume and the
/// mute), and the writes counted, so a reading from before the PC's last write is
/// known for old news.
pub struct Device {
    sink: String,
    one: Mutex<()>,
    /// Writes asked for (by the control loop), and done (by the writer, under `one`).
    asked: AtomicU64,
    done: AtomicU64,
    /// The last reading, and the last level asked to be written.
    last: Mutex<Option<(u8, bool)>>,
    wanted: Mutex<Option<(u8, bool)>>,
    /// To the writer's thread, once there is one.
    writer: Mutex<Option<mpsc::Sender<Write>>>,
}

impl Device {
    pub fn new(sink: &str) -> Arc<Self> {
        Arc::new(Self {
            sink: sink.into(),
            one: Mutex::new(()),
            asked: AtomicU64::new(0),
            done: AtomicU64::new(0),
            last: Mutex::new(None),
            wanted: Mutex::new(None),
            writer: Mutex::new(None),
        })
    }

    /// Prostir's own device for the headset.
    pub fn headset() -> Arc<Self> {
        static HEADSET: OnceLock<Arc<Device>> = OnceLock::new();
        HEADSET.get_or_init(|| Self::new(SINK)).clone()
    }

    /// Asks for the device at `level`, `muted` or not: written on the writer's thread,
    /// the newest asked for first (those it overtakes are dropped).
    fn set(self: &Arc<Self>, level: u8, muted: bool) {
        let n = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        *self.wanted.lock().unwrap() = Some((level.min(100), muted));
        let mut writer = self.writer.lock().unwrap();
        let mut write = Write { n, level, muted };
        if let Some(to) = writer.as_ref() {
            match to.send(write) {
                Ok(()) => return,
                // Its thread is gone: a new one.
                Err(mpsc::SendError(w)) => write = w,
            }
        }
        let (tx, rx) = mpsc::channel();
        let device = Arc::downgrade(self);
        let started = std::thread::Builder::new()
            .name("sound-device".into())
            .spawn(move || write_on(&device, &rx));
        match started {
            Ok(_) => {
                let _ = tx.send(write);
                *writer = Some(tx);
            }
            Err(e) => {
                warn!("volume: no thread to set the headset's device: {e}");
                self.done.store(n, Ordering::SeqCst);
            }
        }
    }

    /// The device now, read with `pactl`; `None` if it did not answer.
    fn read(&self) -> Option<Reading> {
        let _one = self.one.lock().unwrap();
        let level = parse_volume(&pactl(&["get-sink-volume", &self.sink])?)?;
        let muted = parse_mute(&pactl(&["get-sink-mute", &self.sink])?)?;
        let level = level.min(100) as u8;
        *self.last.lock().unwrap() = Some((level, muted));
        Some(Reading {
            level,
            muted,
            writes: self.done.load(Ordering::SeqCst),
        })
    }

    fn outdated(&self, writes: u64) -> bool {
        writes < self.asked.load(Ordering::SeqCst)
    }
}

/// The writer's thread: sleeps until a write is asked for, does the newest, and ends
/// with the device.
fn write_on(device: &Weak<Device>, asked: &mpsc::Receiver<Write>) {
    while let Ok(mut write) = asked.recv() {
        while let Ok(newer) = asked.try_recv() {
            write = newer;
        }
        let Some(device) = device.upgrade() else {
            return;
        };
        let _one = device.one.lock().unwrap();
        let volume = format!("{}%", write.level.min(100));
        let mute = if write.muted { "1" } else { "0" };
        let set = pactl(&["set-sink-volume", &device.sink, &volume]).is_some()
            && pactl(&["set-sink-mute", &device.sink, mute]).is_some();
        // Done even if it failed: what is read from now on is the device as it is.
        device.done.store(write.n, Ordering::SeqCst);
        if set {
            info!(
                "volume: the headset's device at {}{}",
                write.level,
                if write.muted { " muted" } else { "" }
            );
        } else {
            warn!("volume: the headset's device could not be set");
        }
    }
}

/// The volume of the sound device for the headset ("Prostir: headset"), as the PC's
/// slider and keys show it: set on a thread of its own, read by the watcher, never by
/// the control loop. The capture reads the device's monitor, before its volume and
/// mute, so a level written here does not change what the headset gets.
pub struct SinkVolume(Arc<Device>);

impl SinkVolume {
    /// Where the PC can share its volume (see [`prepare`]).
    pub fn open() -> Option<Self> {
        (SHARES.load(Ordering::SeqCst) == 1).then(|| Self(Device::headset()))
    }

    /// A volume for `device` (the harness's own).
    #[cfg(all(test, not(feature = "fake-desktop")))]
    pub fn of(device: Arc<Device>) -> Self {
        Self(device)
    }

    /// Puts the device at `level` percent, `muted` or not (soon, not by the return).
    pub fn set(&self, level: u8, muted: bool) {
        self.0.set(level, muted);
    }

    /// Where the device was when last read; before any reading, where the PC last
    /// asked to put it (so a new main headset is asked for the PC's level meanwhile).
    pub fn level(&self) -> Option<(u8, bool)> {
        let last = *self.0.last.lock().unwrap();
        last.or_else(|| *self.0.wanted.lock().unwrap())
    }

    /// Whether a reading of the device taken after `writes` writes were done is old:
    /// the PC asked for another since, and that write's own echo comes after it.
    pub fn outdated(&self, writes: u64) -> bool {
        self.0.outdated(writes)
    }
}

/// The sound system through `pactl`, for the headset's device and the microphone
/// named `mic`.
pub struct Pactl {
    device: Arc<Device>,
    mic: String,
}

impl Pactl {
    pub fn new(device: Arc<Device>, mic: &str) -> Self {
        Self {
            device,
            mic: mic.into(),
        }
    }

    /// Prostir's own devices.
    pub fn headset() -> Self {
        Self::new(Device::headset(), MIC)
    }
}

/// A running `pactl subscribe`: killed to end it.
struct Subscribed(Mutex<Option<Child>>);

impl End for Subscribed {
    fn end(&self) {
        let mut child = self.0.lock().unwrap();
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl SoundSystem for Pactl {
    fn subscribe(&mut self) -> std::io::Result<Subscription> {
        let mut cmd = pactl_command();
        cmd.arg("subscribe")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Ended by the watcher, or with Prostir.
        let mut child = spawn_tied(cmd)?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no output"))?;
        Ok(Subscription {
            lines: std::fs::File::from(std::os::fd::OwnedFd::from(out)),
            end: Arc::new(Subscribed(Mutex::new(Some(child)))),
        })
    }

    fn indices(&mut self) -> Option<Indices> {
        let sinks = pactl(&["list", "short", "sinks"])?;
        let sources = pactl(&["list", "short", "sources"])?;
        Some(Indices {
            sink: index_in(&sinks, &self.device.sink),
            mic: index_in(&sources, &self.mic),
        })
    }

    fn volume(&mut self) -> Option<Reading> {
        self.device.read()
    }

    fn recording(&mut self) -> Option<bool> {
        let list = pactl(&["list", "short", "sources"])?;
        Some(state_in(&list, &self.mic).is_some_and(|s| s == "RUNNING"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `pactl` says (with `LC_ALL=C`) about the headset's device.
    #[test]
    fn the_device_as_pactl_says_it() {
        let stereo = "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26869 /  41% \
                      / -23.23 dB\n        balance 0.02\n";
        assert_eq!(parse_volume(stereo), Some(41), "the loudest channel");
        assert_eq!(
            parse_volume("Volume: mono: 98304 / 150% / 10.57 dB\n"),
            Some(150)
        );
        assert_eq!(
            parse_volume("Volume: front-left: 0 /   0% / -inf dB"),
            Some(0)
        );
        assert_eq!(parse_volume("Гучність: 40%"), None);
        assert_eq!(parse_volume(""), None);
        assert_eq!(parse_mute("Mute: yes\n"), Some(true));
        assert_eq!(parse_mute("Mute: no"), Some(false));
        assert_eq!(parse_mute("Mute: maybe"), None);
        let sinks = "61\talsa_output.pci-0000_00_1f.3.HiFi__HDMI3__sink\tPipeWire\ts32le 2ch \
                     48000Hz\tSUSPENDED\n183693\tprostir_headset\tPipeWire\tfloat32le 2ch \
                     48000Hz\tRUNNING\n";
        assert_eq!(index_in(sinks, "prostir_headset"), Some(183693));
        assert_eq!(index_in(sinks, "prostir"), None);
        let sources = "183693\tprostir_headset.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n\
                       183776\tprostir_mic\tPipeWire\tfloat32le 1ch 48000Hz\tRUNNING\n";
        assert_eq!(index_in(sources, "prostir_mic"), Some(183776));
        assert_eq!(state_in(sources, "prostir_mic"), Some("RUNNING"));
        assert_eq!(state_in(sources, "prostir_headset.monitor"), Some("IDLE"));
        assert_eq!(state_in(sources, "prostir_headset"), None);
    }

    #[test]
    fn pactl_versions() {
        assert_eq!(
            pactl_major("pactl 17.0-134-g77d25\nCompiled with libpulse 17.0.0"),
            Some(17)
        );
        assert_eq!(pactl_major("pactl 13.99.1"), Some(13));
        assert_eq!(pactl_major("pactl 16.1"), Some(16));
        assert_eq!(pactl_major("pulseaudio 17"), None);
    }

    /// A stand-in for `pactl` that takes a while: the caller does not wait for a
    /// write, writes asked for together are done once (the newest), and readings from
    /// before it are old until it is done.
    #[test]
    fn writes_go_on_their_own_thread_newest_first() {
        let device = Device::new("prostir_no_such_sink_for_tests");
        let started = std::time::Instant::now();
        device.set(10, false);
        device.set(20, false);
        device.set(30, true);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "the caller waited"
        );
        assert!(device.outdated(0) && device.outdated(2));
        assert!(!device.outdated(3));
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while device.done.load(Ordering::SeqCst) < 3 {
            assert!(std::time::Instant::now() < until, "the write never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Dropped with its device, the writer's thread ends too: it lets go of the
        // device once it is through with the write it counted done.
        let weak = Arc::downgrade(&device);
        drop(device);
        while weak.upgrade().is_some() {
            assert!(
                std::time::Instant::now() < until,
                "the writer kept the device"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
