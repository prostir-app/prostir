//! `--selftest`: the app runs a fixed scenario against a fake PC in its own process
//! and writes what it measured, as `key=value` lines (see `scripts/selftest-headset.sh`).
//! In every build but the store's (feature `selftest`, on by default).
//!
//! What is here builds and is tested on any system: the options, the numbers and the
//! verdict. Running it on the headset is in `device` (Android only).

#![expect(
    clippy::disallowed_methods,
    reason = "the selftest is part of the shell: it measures time and reads its clips"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use prostir_proto::stats::Histogram;

#[cfg(target_os = "android")]
pub mod device;

/// How a selftest ended, as the process would exit with it (`exit=` in the report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Exit {
    Pass = 0,
    /// A check failed (`fail=` says which).
    Fail = 1,
    /// It could not run: no OpenXR session in time, no clips, a missing extension, an
    /// option this build does not have.
    NoStart = 2,
    /// It did not finish in time.
    Timeout = 3,
}

impl Exit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// What the selftest is asked to do (the intent extra `selftest`, `key=value` pairs
/// separated by commas, spaces, tabs or semicolons).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// How long the calm part, where frame times are measured, lasts.
    pub seconds: u32,
    /// End without leaving the process, so the next start runs in the same one.
    pub relaunch: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            seconds: 30,
            relaunch: false,
        }
    }
}

impl Options {
    /// The options in `text`; an error says what cannot be done (an option that comes
    /// with a later step of this build is refused rather than ignored, so a run never
    /// measures something else than it says).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut options = Self::default();
        for pair in text
            .split([',', ' ', '\t', ';', '\n'])
            .filter(|p| !p.trim().is_empty())
        {
            let (key, value) = pair
                .trim()
                .split_once('=')
                .ok_or_else(|| format!("{pair}: not key=value"))?;
            match (key, value) {
                ("seconds", v) => {
                    options.seconds = v
                        .parse()
                        .ok()
                        .filter(|s| (1..=7200).contains(s))
                        .ok_or_else(|| format!("seconds={v}: 1 to 7200"))?;
                }
                ("relaunch", "1") => options.relaunch = true,
                ("relaunch", "0") | ("caps", "full") | ("perf", "default") | ("pc", "fake") => {}
                ("caps", v) => return Err(format!("caps={v} is not in this build yet")),
                ("perf", v) => return Err(format!("perf={v} is not in this build yet")),
                ("pc", v) => return Err(format!("pc={v} is not in this build yet")),
                _ => return Err(format!("{key}={value}: unknown")),
            }
        }
        Ok(options)
    }
}

/// Numbers of one kind, kept in a buffer made once: adding one allocates nothing until
/// the buffer is full, so measuring a frame does not change what is measured.
#[derive(Debug, Clone, Default)]
pub struct Samples(Vec<f64>);

impl Samples {
    pub fn with_capacity(n: usize) -> Self {
        Self(Vec::with_capacity(n))
    }

    /// Keeps `v` while there is room (the rest is dropped, not reallocated for).
    pub fn add(&mut self, v: f64) {
        if self.0.len() < self.0.capacity() {
            self.0.push(v);
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value below which a share `p` (0 to 1) of them lie (nearest rank).
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.0.is_empty() {
            return None;
        }
        let mut sorted = self.0.clone();
        sorted.sort_by(f64::total_cmp);
        let rank = (p * sorted.len() as f64).ceil() as usize;
        Some(sorted[rank.clamp(1, sorted.len()) - 1])
    }

    pub fn max(&self) -> Option<f64> {
        self.0.iter().copied().reduce(f64::max)
    }

    pub fn mean(&self) -> Option<f64> {
        (!self.0.is_empty()).then(|| self.0.iter().sum::<f64>() / self.0.len() as f64)
    }
}

/// The report: `key=value` lines in the order they were first set.
#[derive(Debug, Clone, Default)]
pub struct Report {
    lines: Vec<(String, String)>,
}

impl Report {
    /// Sets `key` (replacing what it said before, in its place).
    pub fn set(&mut self, key: &str, value: impl std::fmt::Display) {
        let value = value.to_string().replace(['\n', '\r'], " ");
        match self.lines.iter_mut().find(|(k, _)| k == key) {
            Some(line) => line.1 = value,
            None => self.lines.push((key.to_owned(), value)),
        }
    }

    /// Sets `key` to `ms` milliseconds with one decimal, or `none`.
    pub fn set_ms(&mut self, key: &str, ms: Option<f64>) {
        match ms {
            Some(ms) => self.set(key, format!("{ms:.1}")),
            None => self.set(key, "none"),
        }
    }

    /// Sets `key` to a share of the display period with three decimals, or `none`.
    pub fn set_share(&mut self, key: &str, share: Option<f64>) {
        match share {
            Some(share) => self.set(key, format!("{share:.3}")),
            None => self.set(key, "none"),
        }
    }

    /// Sets each of `keys` to `none`, in this order, so a report that ends early lists
    /// the same keys in the same place.
    pub fn lay_out(&mut self, keys: &[&str]) {
        for key in keys {
            self.set(key, "none");
        }
    }

    pub fn text(&self) -> String {
        self.lines.iter().fold(String::new(), |mut s, (k, v)| {
            let _ = writeln!(s, "{k}={v}");
            s
        })
    }

    /// Percentiles and the largest of `samples` (milliseconds) as `key.p50` and so on.
    pub fn set_spread(&mut self, key: &str, samples: &Samples) {
        for (name, p) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
            self.set_ms(&format!("{key}.{name}"), samples.percentile(p));
        }
        self.set_ms(&format!("{key}.max"), samples.max());
    }
}

/// What the checks look at; `None`: not measured (which fails the checks that need it).
#[derive(Debug, Clone, Default)]
pub struct Measured {
    /// Each stream's first picture after it started, milliseconds.
    pub first_frames_ms: Vec<Option<f64>>,
    /// After the frame its decoder could not take: until the headset asked for a
    /// keyframe, and until a picture came out of the decoder again.
    pub keyframe_asked_ms: Option<f64>,
    pub decoder_recovered_ms: Option<f64>,
    pub reconnect_ms: Option<f64>,
    /// A frame's work without a menu redraw, as shares of its display period, at the
    /// 95th and 99th percentile.
    pub frame_share_p95: Option<f64>,
    pub frame_share_p99: Option<f64>,
}

/// The limits (step 7 note, 10.3 and 7): first pictures, asking for a keyframe and the
/// decoder's recovery within 1 s, reconnecting within 3 s, a frame's work p95 within
/// 0.15 and p99 within 0.3 of the display period. The menu's redraw is only recorded.
/// What failed, in words.
pub fn check(m: &Measured) -> Vec<String> {
    let mut fails = Vec::new();
    let mut within = |what: &str, value: Option<f64>, limit: f64| match value {
        Some(v) if v <= limit => {}
        Some(v) => fails.push(format!("{what} {v:.2} over {limit}")),
        None => fails.push(format!("{what} not measured")),
    };
    if m.first_frames_ms.is_empty() {
        within("first_frame_ms", None, 1000.0);
    }
    for (i, first) in m.first_frames_ms.iter().enumerate() {
        within(&format!("stream.{}.first_frame_ms", i + 1), *first, 1000.0);
    }
    within("keyframe_asked_ms", m.keyframe_asked_ms, 1000.0);
    within("decoder_recovered_ms", m.decoder_recovered_ms, 1000.0);
    within("reconnect_ms", m.reconnect_ms, 3000.0);
    within("frame_cpu_share.p95", m.frame_share_p95, 0.15);
    within("frame_cpu_share.p99", m.frame_share_p99, 0.3);
    fails
}

/// How many pictures of the watched stream are kept (5 s at 120 Hz and some).
const WATCHED: usize = 1024;

/// What the decoders tell the selftest while one runs: how long each frame took from
/// the network to the decoder and through it, how many each stream decoded, and when
/// pictures came out of them (the moment a decoder hands a picture to the compositor,
/// not the headset's `FrameShown`, which it sends at most every 100 ms per stream).
#[derive(Default)]
pub struct Probe {
    pub wait: Histogram,
    pub decode: Histogram,
    /// Uploads of images that change (the menu, buttons, notices), each from the
    /// pixels to the swapchain.
    pub upload: Histogram,
    /// The most frames seen waiting behind one a decoder took.
    pub queue_max: usize,
    /// Times the network dropped a stream's frames until its next keyframe because
    /// its decoder was behind.
    pub dropped_to_keyframe: u64,
    /// Frames decoded, by display number.
    pub decoded: BTreeMap<u32, u64>,
    /// When each stream's generation (display number, generation) gave its first
    /// picture.
    pub first: BTreeMap<(u32, u32), Instant>,
    /// The stream watched since a frame of it was broken on purpose, and its pictures
    /// since: number, when.
    watch: Option<(u32, u32)>,
    pub watched: Vec<(u32, Instant)>,
}

impl Probe {
    /// Keeps the pictures of display `id`'s generation `generation` from now on.
    pub fn watch(&mut self, id: u32, generation: u32) {
        self.watch = Some((id, generation));
        self.watched.clear();
        self.watched.reserve(WATCHED);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// On only while a selftest runs: the decoders check it before anything else.
static PROBING: AtomicBool = AtomicBool::new(false);
static PROBE: Mutex<Option<Arc<Mutex<Probe>>>> = Mutex::new(None);

/// Starts collecting from the decoders into a new probe.
pub fn probe_start() -> Arc<Mutex<Probe>> {
    let probe = Arc::new(Mutex::new(Probe::default()));
    *lock(&PROBE) = Some(probe.clone());
    PROBING.store(true, Ordering::Relaxed);
    probe
}

/// Stops collecting.
pub fn probe_stop() {
    PROBING.store(false, Ordering::Relaxed);
    *lock(&PROBE) = None;
}

/// A decoder put out picture `seq` of display `id`'s generation `generation`: `wait`
/// from the network to the decoder, `decode` through it. Nothing unless a selftest
/// runs.
pub fn decoded(id: u32, generation: u32, seq: u32, wait: Duration, decode: Duration) {
    if !PROBING.load(Ordering::Relaxed) {
        return;
    }
    let at = Instant::now();
    let probe = lock(&PROBE).clone();
    if let Some(probe) = probe {
        let mut p = lock(&probe);
        p.wait.add(wait);
        p.decode.add(decode);
        *p.decoded.entry(id).or_default() += 1;
        p.first.entry((id, generation)).or_insert(at);
        if p.watch == Some((id, generation)) && p.watched.len() < WATCHED {
            p.watched.push((seq, at));
        }
    }
}

/// A decoder took a frame from the network with `behind` more waiting. Nothing unless
/// a selftest runs.
pub fn taken(behind: usize) {
    if !PROBING.load(Ordering::Relaxed) {
        return;
    }
    let probe = lock(&PROBE).clone();
    if let Some(probe) = probe {
        let mut p = lock(&probe);
        p.queue_max = p.queue_max.max(behind);
    }
}

/// The network started dropping a stream's frames until its next keyframe. Nothing
/// unless a selftest runs.
pub fn dropped_to_keyframe() {
    if !PROBING.load(Ordering::Relaxed) {
        return;
    }
    let probe = lock(&PROBE).clone();
    if let Some(probe) = probe {
        lock(&probe).dropped_to_keyframe += 1;
    }
}

/// When an upload of an image starts, while a selftest runs.
pub fn upload_started() -> Option<Instant> {
    PROBING.load(Ordering::Relaxed).then(Instant::now)
}

/// The upload that started at `began` (see [`upload_started`]) is done.
pub fn uploaded(began: Option<Instant>) {
    let Some(began) = began else { return };
    let took = began.elapsed();
    let probe = lock(&PROBE).clone();
    if let Some(probe) = probe {
        lock(&probe).upload.add(took);
    }
}

/// Names of the threads this app starts (as the system shows them, at most 15 bytes):
/// one still alive when a new start begins is left over from the one before.
pub fn is_app_thread(name: &str) -> bool {
    matches!(
        name,
        "net" | "net-writer" | "audio" | "mic" | "fake-pc" | "fake-pc-read" | "persist" | "system"
    ) || name.starts_with("decoder-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_say_what_this_build_does() {
        assert_eq!(Options::parse("").unwrap(), Options::default());
        assert_eq!(
            Options::parse("seconds=10, relaunch=1").unwrap(),
            Options {
                seconds: 10,
                relaunch: true
            }
        );
        assert_eq!(
            Options::parse("seconds=5\tcaps=full perf=default")
                .unwrap()
                .seconds,
            5
        );
        for refused in [
            "caps=minimal",
            "perf=low",
            "pc=usb",
            "seconds=0",
            "speed=2",
            "x",
        ] {
            assert!(Options::parse(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn samples_give_percentiles_without_growing() {
        let mut s = Samples::with_capacity(100);
        for v in 1..=200 {
            s.add(f64::from(v));
        }
        assert_eq!(s.len(), 100);
        assert_eq!(s.percentile(0.5), Some(50.0));
        assert_eq!(s.percentile(0.95), Some(95.0));
        assert_eq!(s.percentile(1.0), Some(100.0));
        assert_eq!(s.max(), Some(100.0));
        assert_eq!(s.mean(), Some(50.5));
        assert_eq!(Samples::with_capacity(4).percentile(0.5), None);
    }

    #[test]
    fn the_report_keeps_its_order() {
        let mut r = Report::default();
        r.lay_out(&["selftest", "model", "connect_ms"]);
        r.set("model", "Quest\n3");
        r.set_ms("connect_ms", Some(412.04));
        r.set("selftest", 1);
        r.set_share("share", Some(0.12345));
        assert_eq!(
            r.text(),
            "selftest=1\nmodel=Quest 3\nconnect_ms=412.0\nshare=0.123\n"
        );
    }

    #[test]
    fn checks_name_what_failed() {
        let good = Measured {
            first_frames_ms: vec![Some(180.0), Some(205.0)],
            keyframe_asked_ms: Some(20.0),
            decoder_recovered_ms: Some(95.0),
            reconnect_ms: Some(1300.0),
            frame_share_p95: Some(0.12),
            frame_share_p99: Some(0.2),
        };
        assert!(check(&good).is_empty());
        let bad = Measured {
            first_frames_ms: vec![Some(180.0), None],
            reconnect_ms: Some(3500.0),
            ..good
        };
        assert_eq!(
            check(&bad),
            [
                "stream.2.first_frame_ms not measured",
                "reconnect_ms 3500.00 over 3000"
            ]
        );
        assert_eq!(check(&Measured::default()).len(), 6);
    }

    /// The decoders' pictures are kept for the selftest only while it runs, and only
    /// the watched stream's one by one.
    #[test]
    fn the_probe_listens_only_while_on() {
        let ms = Duration::from_millis(1);
        decoded(1, 1, 0, ms, ms);
        let probe = probe_start();
        decoded(1, 1, 0, ms, ms);
        taken(2);
        taken(1);
        dropped_to_keyframe();
        {
            let p = lock(&probe);
            assert_eq!((p.queue_max, p.dropped_to_keyframe), (2, 1));
        }
        lock(&probe).watch(2, 3);
        decoded(1, 1, 1, ms, ms);
        decoded(2, 3, 7, ms, ms);
        probe_stop();
        decoded(2, 3, 8, ms, ms);
        taken(5);
        dropped_to_keyframe();
        let p = lock(&probe);
        assert_eq!(
            (p.queue_max, p.dropped_to_keyframe),
            (2, 1),
            "nothing after the stop"
        );
        assert_eq!(p.decoded[&1], 2);
        assert_eq!(p.decoded[&2], 1);
        assert_eq!(p.first.len(), 2);
        assert_eq!(p.watched.iter().map(|w| w.0).collect::<Vec<_>>(), [7]);
    }

    /// The clips in `assets/selftest/` (made by `scripts/selftest-clips.sh`) play on the
    /// fake PC: named as it reads them, a few seconds each, one keyframe at the start,
    /// small together.
    #[test]
    fn the_shipped_clips_play() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/selftest");
        let mut total = 0;
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            let data = std::fs::read(&path).unwrap();
            total += data.len();
            let clip = prostir_testkit::pc::Clip::named(&name, data).unwrap();
            assert_eq!(clip.len() as u32, 2 * clip.fps, "{name}");
            assert_eq!(clip.keyframes(), 1, "{name}");
            names.push(name);
        }
        names.sort();
        assert_eq!(
            names,
            [
                "h264-1280x720-60.h264",
                "h264-1920x1080-60.h264",
                "h265-2560x1440-60.h265"
            ]
        );
        assert!(total < 1 << 20, "{total} bytes of clips");
    }

    #[test]
    fn app_threads_are_known_by_name() {
        assert!(is_app_thread("decoder-3"));
        assert!(is_app_thread("net-writer"));
        assert!(!is_app_thread("RenderThread"));
    }
}
