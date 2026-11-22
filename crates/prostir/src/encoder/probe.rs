//! What this PC's encoders can do: each candidate encodes a few frames in a tiny
//! pipeline of the same elements and settings a stream uses, since an element being
//! there does not mean its driver encodes (a VA driver without H.265, an NVIDIA plugin
//! without the NVIDIA driver). Probes run side by side on a thread of their own, started
//! with the program ([`start_probing`]), each held to [`PROBE_TIMEOUT`] from its own
//! start: one that hangs counts as failed and holds nothing up (its thread is left
//! behind). An encoder is waited for only when one is needed ([`catalog`]).
//!
//! What they found is kept in the user's cache directory (`prostir/encoders.txt`) with
//! a fingerprint of GStreamer, the graphics hardware and its drivers, and probed again
//! when that changes. Only what lasts is kept as it is: an encoder that works, and one
//! whose elements are not installed. A probe that failed or did not end, and an
//! encoder that did not open, are kept as `retry`: probed again at the next start, in
//! the background, while what works is used at once.
//!
//! Session limits are learned on the way, never guessed (NVENC ran 10 at once on driver
//! 621): see [`Catalog::did_not_open`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use log::{info, warn};
use prostir_proto::Codec;

use super::choice::{Backend, CodecCaps, EncoderCaps, Kind, Ranked, Wanted, rank};
use super::{EncoderConfig, Input, NotOpened, encoding};
use crate::platform::{AdapterId, Memory};
use crate::screen::ScreenId;

/// How long one probe may take, from its own start.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// [`PROBE_TIMEOUT`] in this run, or in a build for the end-to-end tests what
/// `PROSTIR_TEST_PROBE_SECONDS` says: a probe that hangs held so long that waiting for
/// it shows whatever else takes a busy machine.
pub fn probe_timeout() -> Duration {
    #[cfg(feature = "fake-desktop")]
    if let Some(s) = std::env::var("PROSTIR_TEST_PROBE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return Duration::from_secs(s);
    }
    PROBE_TIMEOUT
}

/// An encoder that did not open for an output is tried for it again after this long
/// (or at once for another output).
pub const FORGET_FAILURE: Duration = Duration::from_secs(600);

/// The probe's picture: small, but not below the smallest size any encoder takes
/// (NVENC H.265 wants more than 128 pixels across).
const PROBE_SIZE: (u32, u32) = (256, 144);

/// The codecs a probe tries.
const CODECS: [Codec; 2] = [Codec::H265, Codec::H264];

/// An encoder of this build to probe: Linux has VA-API, NVENC and x264.
fn candidates() -> Vec<(Backend, Option<AdapterId>)> {
    #[cfg_attr(
        not(feature = "fake-desktop"),
        expect(unused_mut, reason = "only the tests add one")
    )]
    let mut all = vec![
        (Backend::Va, None),
        (Backend::Nvenc, None),
        (Backend::X264, None),
    ];
    #[cfg(feature = "fake-desktop")]
    if fake::mode().is_some() {
        all.insert(0, (Backend::Fake, None));
    }
    all
}

/// An encoder in a codec, as probed.
type Asked = (Backend, Option<AdapterId>, Codec);

/// Every encoder and codec this build probes.
fn everything() -> Vec<Asked> {
    candidates()
        .into_iter()
        .flat_map(|(b, a)| {
            CODECS
                .into_iter()
                .filter(move |c| b.elements(*c).is_some())
                .map(move |c| (b, a, c))
        })
        .collect()
}

/// What became of probing one encoder in one codec.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Probed {
    Works {
        max_w: u32,
        max_h: u32,
    },
    /// An element is not installed: kept (installing it changes the fingerprint).
    Missing(String),
    /// It did not encode, or did not end in time: probed again at the next start.
    Failed(String),
}

/// What an output does once its encoder did not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Tries the same encoder once more: it may have been short of sessions for a
    /// moment.
    Again,
    /// Goes on with the next encoder.
    Another,
}

/// An encoder that did not open for an output, this run.
#[derive(Debug, Clone)]
struct Failure {
    kind: Kind,
    codec: Codec,
    output: ScreenId,
    why: String,
    at: Instant,
}

/// The encoders this PC has, as probed, and what was learned of them since.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// The ones that encode at least one codec, in the order of [`candidates`], with
    /// the session limits kept from earlier runs.
    pub encoders: Vec<EncoderCaps>,
    /// Encoders and codecs whose elements are not installed, and why.
    pub missing: Vec<(Backend, Codec, String)>,
    /// Encoders and codecs to probe again at the next start, and why.
    pub retry: Vec<(Backend, Option<AdapterId>, Codec, String)>,
    /// Encoders that did not open for an output this run.
    failures: Vec<Failure>,
    /// The sessions each encoder had open the first time this run one more did not
    /// open for lack of them.
    short: BTreeMap<Kind, u32>,
    /// Session limits that hold this run: learned now, or kept and seen again.
    limits: BTreeMap<Kind, u32>,
    /// The fingerprint it was probed with.
    key: String,
    /// Where it is kept; none where there is no cache directory.
    path: Option<PathBuf>,
}

impl Catalog {
    /// What a probe of `asked` found, in place of what was known of it.
    fn add(&mut self, (backend, adapter, codec): Asked, probed: Probed) {
        for e in self
            .encoders
            .iter_mut()
            .filter(|e| e.kind() == (backend, adapter))
        {
            e.codecs.retain(|c| c.codec != codec);
        }
        self.missing
            .retain(|(b, c, _)| (*b, *c) != (backend, codec));
        self.retry
            .retain(|(b, a, c, _)| (*b, *a, *c) != (backend, adapter, codec));
        match probed {
            Probed::Works { max_w, max_h } => {
                let caps = CodecCaps {
                    codec,
                    max_w,
                    max_h,
                };
                match self
                    .encoders
                    .iter_mut()
                    .find(|e| e.kind() == (backend, adapter))
                {
                    Some(e) => {
                        e.codecs.push(caps);
                        // Sharpest first, as probed.
                        e.codecs
                            .sort_by_key(|c| CODECS.iter().position(|x| *x == c.codec));
                    }
                    None => self.encoders.push(EncoderCaps {
                        backend,
                        adapter,
                        codecs: vec![caps],
                        sessions: None,
                    }),
                }
            }
            Probed::Missing(why) => self.missing.push((backend, codec, why)),
            Probed::Failed(why) => self.retry.push((backend, adapter, codec, why)),
        }
        let place = |b: Backend| candidates().iter().position(|(c, _)| *c == b);
        self.encoders.sort_by_key(|e| place(e.backend));
    }

    /// What to probe: every encoder and codec of this build not known yet (a new
    /// candidate), and those to try again.
    fn to_probe(&self) -> Vec<Asked> {
        everything()
            .into_iter()
            .filter(|&(b, a, c)| {
                let works = self
                    .encoders
                    .iter()
                    .any(|e| e.kind() == (b, a) && e.codec(c).is_some());
                let missing = self.missing.iter().any(|(mb, mc, _)| (*mb, *mc) == (b, c));
                let again = self
                    .retry
                    .iter()
                    .any(|(rb, ra, rc, _)| (*rb, *ra, *rc) == (b, a, c));
                again || !(works || missing)
            })
            .collect()
    }

    /// `VA-API H265 H264`, for the log.
    fn summary(&self) -> String {
        if self.encoders.is_empty() {
            return "none".into();
        }
        self.encoders
            .iter()
            .map(|e| {
                let codecs: Vec<String> =
                    e.codecs.iter().map(|c| format!("{:?}", c.codec)).collect();
                let limit = e
                    .sessions
                    .map_or_else(String::new, |n| format!(" (at most {n} at once)"));
                format!("{} {}{limit}", e.backend.label(), codecs.join(" "))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Why each encoder that does not work does not, for the log.
    fn log_missing(&self) {
        for (backend, codec, why) in &self.missing {
            info!("{} {codec:?}: {why}", backend.label());
        }
        for (backend, _, codec, why) in &self.retry {
            info!(
                "{} {codec:?}: {why} (probed again at the next start)",
                backend.label()
            );
        }
    }

    /// Each codec this PC encodes, and whether on the graphics card (for the headset's
    /// menu): hardware first, as before.
    pub fn available(&self) -> Vec<(Codec, bool)> {
        let mut choices = Vec::new();
        for codec in [Codec::H265, Codec::H264, Codec::Av1] {
            for hardware in [true, false] {
                if self
                    .encoders
                    .iter()
                    .any(|e| e.backend.hardware() == hardware && e.codec(codec).is_some())
                {
                    choices.push((codec, hardware));
                }
            }
        }
        choices
    }

    /// The encoder and codec this PC encodes `prefer` with when nothing else is known
    /// (where the frames are, which output): the first hardware one in
    /// [`Backend::ORDER`] with the codecs in their order, else x264; only x264 with
    /// `software`.
    pub fn pick(&self, prefer: Codec, software: bool) -> Result<(Backend, Codec)> {
        let mut codecs = vec![prefer];
        codecs.extend(
            [Codec::H265, Codec::H264]
                .into_iter()
                .filter(|c| *c != prefer),
        );
        rank(
            &self.encoders,
            Memory::System,
            &codecs,
            software,
            &|_| 0,
            &[],
        )
        .choices
        .first()
        .map(|c| (c.backend, c.codec))
        .ok_or_else(no_encoder)
    }

    /// The encoders for `output`, whose frames are in `memory` (see [`rank`]): with the
    /// session limits that hold this run, past the encoders that did not open for it in
    /// the last [`FORGET_FAILURE`].
    pub fn rank(
        &self,
        memory: Memory,
        wanted: Wanted,
        output: ScreenId,
        running: &dyn Fn(Kind) -> u32,
    ) -> Ranked {
        let encoders: Vec<EncoderCaps> = self
            .encoders
            .iter()
            .map(|e| EncoderCaps {
                sessions: self.limits.get(&e.kind()).copied(),
                ..e.clone()
            })
            .collect();
        let failed: Vec<(Kind, Codec, String)> = self
            .failures
            .iter()
            .filter(|f| f.output == output && f.at.elapsed() < FORGET_FAILURE)
            .map(|f| (f.kind, f.codec, f.why.clone()))
            .collect();
        let codecs = wanted.codecs();
        rank(
            &encoders,
            memory,
            &codecs,
            wanted.software,
            running,
            &failed,
        )
    }

    /// Encoder `kind` did not open in `codec` for `output` (as `failure` says: whether
    /// it said it had no more sessions, and whether it was opening again after a
    /// pause), with `open` sessions of it held by the other outputs.
    ///
    /// A session limit is learned only of an encoder that has one (NVENC, AMF, Quick
    /// Sync; not VA-API, whose drivers count none), only when it was short of sessions
    /// while others ran, and only the second time this run, or the first time where a
    /// limit was kept from earlier runs. The limit is the least number seen open then,
    /// and rises when more are seen open later ([`Catalog::seen_open`]). One learned
    /// while encoders opened again after a pause holds for this run only.
    ///
    /// Any other failure is the output's: the encoder is passed over for it (not for
    /// the others) for [`FORGET_FAILURE`], and probed again at the next start.
    pub fn did_not_open(
        &mut self,
        kind: Kind,
        codec: Codec,
        output: ScreenId,
        failure: &NotOpened,
        open: u32,
    ) -> Next {
        let label = kind.0.label();
        let why = failure.why.as_str();
        if failure.short && open > 0 && kind.0.counts_sessions() {
            let kept = self
                .encoders
                .iter()
                .find(|e| e.kind() == kind)
                .and_then(|e| e.sessions);
            let learned = match (self.short.remove(&kind), kept) {
                (Some(before), _) => before.min(open),
                (None, Some(_)) => open,
                (None, None) => {
                    info!("{label} did not open with {open} open: tried once more");
                    self.short.insert(kind, open);
                    return Next::Again;
                }
            };
            let max = learned.max(1);
            warn!("{label} opens at most {max} sessions at once");
            self.limits.insert(kind, max);
            if !failure.resumed
                && let Some(e) = self.encoders.iter_mut().find(|e| e.kind() == kind)
                && e.sessions != Some(max)
            {
                e.sessions = Some(max);
                self.save();
            }
            return Next::Another;
        }
        warn!("{label} {codec:?} did not start: {why}; not used for this output for now");
        self.failures.retain(|f| f.at.elapsed() < FORGET_FAILURE);
        self.failures.push(Failure {
            kind,
            codec,
            output,
            why: why.to_owned(),
            at: Instant::now(),
        });
        let marked = self
            .retry
            .iter()
            .any(|(b, a, c, _)| (*b, *a, *c) == (kind.0, kind.1, codec));
        if !marked {
            self.retry.push((kind.0, kind.1, codec, why.to_owned()));
            self.save();
        }
        Next::Another
    }

    /// What failed on `output` counts no more: every encoder is tried for it again (a
    /// new session, or the person asked for it to be tried again).
    pub fn forget_output(&mut self, output: ScreenId) {
        self.failures.retain(|f| f.output != output);
    }

    /// `open` sessions of encoder `kind` opened at once: a limit lower than that
    /// rises to it.
    pub fn seen_open(&mut self, kind: Kind, open: u32) {
        if let Some(limit) = self.limits.get_mut(&kind)
            && open > *limit
        {
            *limit = open;
        }
        if let Some(e) = self.encoders.iter_mut().find(|e| e.kind() == kind)
            && e.sessions.is_some_and(|n| open > n)
        {
            info!(
                "{} has {open} sessions open, more than before",
                kind.0.label()
            );
            e.sessions = Some(open);
            self.save();
        }
    }

    /// The cache file's text.
    fn text(&self) -> String {
        let mut out = format!(
            "# The video encoders Prostir found on this PC, and how many sessions each opens at\n\
             # once. Probed again when GStreamer or the graphics hardware or a driver changes,\n\
             # and `retry` ones at every start; safe to delete.\n\
             key {}\n",
            self.key
        );
        let line = |why: &str| why.replace(['\n', '\r'], " ");
        for e in &self.encoders {
            let codecs: Vec<String> = e
                .codecs
                .iter()
                .map(|c| format!("{} {}x{}", codec_name(c.codec), c.max_w, c.max_h))
                .collect();
            let _ = writeln!(
                out,
                "encoder {} {} {}",
                e.backend.name(),
                adapter_text(e.adapter),
                codecs.join(" ")
            );
            if let Some(n) = e.sessions {
                let _ = writeln!(
                    out,
                    "sessions {} {} {n}",
                    e.backend.name(),
                    adapter_text(e.adapter)
                );
            }
        }
        for (backend, codec, why) in &self.missing {
            let _ = writeln!(
                out,
                "missing {} {} {}",
                backend.name(),
                codec_name(*codec),
                line(why)
            );
        }
        for (backend, adapter, codec, why) in &self.retry {
            let _ = writeln!(
                out,
                "retry {} {} {} {}",
                backend.name(),
                adapter_text(*adapter),
                codec_name(*codec),
                line(why)
            );
        }
        out
    }

    /// The cache file's catalog, when it was made with fingerprint `key`.
    fn parse(text: &str, key: &str) -> Option<Self> {
        let mut catalog = Self {
            key: key.to_owned(),
            ..Self::default()
        };
        let mut keyed = false;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace();
            match words.next()? {
                "key" => {
                    keyed = words.next()? == key;
                    if !keyed {
                        return None;
                    }
                }
                "encoder" => {
                    let backend = Backend::named(words.next()?)?;
                    let adapter = parse_adapter(words.next()?)?;
                    let mut codecs = Vec::new();
                    while let Some(codec) = words.next() {
                        let (w, h) = words.next()?.split_once('x')?;
                        codecs.push(CodecCaps {
                            codec: parse_codec(codec)?,
                            max_w: w.parse().ok()?,
                            max_h: h.parse().ok()?,
                        });
                    }
                    catalog.encoders.push(EncoderCaps {
                        backend,
                        adapter,
                        codecs,
                        sessions: None,
                    });
                }
                "sessions" => {
                    let kind = (
                        Backend::named(words.next()?)?,
                        parse_adapter(words.next()?)?,
                    );
                    let n: u32 = words.next()?.parse().ok()?;
                    catalog
                        .encoders
                        .iter_mut()
                        .find(|e| e.kind() == kind)?
                        .sessions = Some(n.max(1));
                }
                "missing" => {
                    let backend = Backend::named(words.next()?)?;
                    let codec = parse_codec(words.next()?)?;
                    catalog
                        .missing
                        .push((backend, codec, words.collect::<Vec<_>>().join(" ")));
                }
                "retry" => {
                    let backend = Backend::named(words.next()?)?;
                    let adapter = parse_adapter(words.next()?)?;
                    let codec = parse_codec(words.next()?)?;
                    let why = words.collect::<Vec<_>>().join(" ");
                    catalog.retry.push((backend, adapter, codec, why));
                }
                _ => return None,
            }
        }
        keyed.then_some(catalog)
    }

    /// Writes the cache file (whole, through a file next to it, so a crash or another
    /// start writing at the same time never leaves half of one).
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
                std::fs::write(&tmp, self.text())?;
                std::fs::rename(&tmp, path)
            });
        if let Err(e) = written {
            warn!(
                "cannot keep what the encoders can do in {}: {e}",
                path.display()
            );
        }
    }
}

fn no_encoder() -> anyhow::Error {
    anyhow!(
        "{}",
        crate::i18n::t(
            "На ПК немає відеокодувальника. Intel чи AMD: встановіть VA-плагін GStreamer \
             (gst-plugin-va або gst-plugins-bad) і VA-драйвер. NVIDIA: драйвер NVIDIA і \
             nvcodec з gst-plugins-bad. Будь-яка відеокарта: gst-plugins-ugly (x264, \
             кодування процесором).",
            "No video encoder on the PC. Intel or AMD: install the GStreamer VA plugin \
             (gst-plugin-va or gst-plugins-bad) and a VA driver. NVIDIA: the NVIDIA driver \
             and nvcodec from gst-plugins-bad. Any graphics: gst-plugins-ugly (x264, \
             encoding on the CPU).",
        )
    )
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::H265 => "h265",
        Codec::Av1 => "av1",
    }
}

fn parse_codec(name: &str) -> Option<Codec> {
    [Codec::H264, Codec::H265, Codec::Av1]
        .into_iter()
        .find(|c| codec_name(*c) == name)
}

fn adapter_text(adapter: Option<AdapterId>) -> String {
    adapter.map_or_else(|| "-".into(), |a| format!("{:x}", a.0))
}

fn parse_adapter(text: &str) -> Option<Option<AdapterId>> {
    if text == "-" {
        return Some(None);
    }
    u64::from_str_radix(text, 16)
        .ok()
        .map(|a| Some(AdapterId(a)))
}

/// What decides what the encoders do: GStreamer's version, the plugin files of every
/// candidate, and the graphics hardware and drivers as the platform describes them. A
/// short hash of it, so a change anywhere makes the cache stale.
fn fingerprint() -> String {
    let mut parts = vec![gst::version_string().to_string()];
    let mut plugins: Vec<String> = candidates()
        .into_iter()
        .flat_map(|(backend, _)| CODECS.iter().filter_map(move |c| backend.elements(*c)))
        .flatten()
        .filter_map(|e| gst::ElementFactory::find(e)?.plugin()?.filename())
        .map(|f| file_identity(&f))
        .collect();
    plugins.sort();
    plugins.dedup();
    parts.extend(plugins);
    parts.extend(crate::platform::encoder_drivers());
    format!("{:016x}", fnv1a(parts.join("\n").as_bytes()))
}

/// A file's path, size and modification time: what changes when it is replaced.
pub fn file_identity(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let modified = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{} {size} {modified}", path.display())
}

/// 64-bit FNV-1a: stable across Rust versions, unlike the standard hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The largest picture the element `name` takes on its sink, from its pad template.
fn max_size(name: &str) -> Option<(u32, u32)> {
    let factory = gst::ElementFactory::find(name)?;
    let template = factory
        .static_pad_templates()
        .into_iter()
        .find(|t| t.direction() == gst::PadDirection::Sink)?;
    let caps = template.caps();
    let s = caps.structure(0)?;
    let max = |field: &str| -> Option<u32> {
        match s.value(field).ok()? {
            v if v.get::<gst::IntRange<i32>>().is_ok() => {
                u32::try_from(v.get::<gst::IntRange<i32>>().ok()?.max()).ok()
            }
            v => u32::try_from(v.get::<i32>().ok()?).ok(),
        }
    };
    Some((max("width")?, max("height")?))
}

/// Encodes a few frames of a test picture with `backend` in `codec`, through the same
/// elements and settings as a stream.
fn probe_one(backend: Backend, codec: Codec) -> Probed {
    #[cfg(feature = "fake-desktop")]
    if backend == Backend::Fake
        && let Some(made) = fake::probe()
    {
        return made;
    }
    let Some(elements) = backend.elements(codec) else {
        return Probed::Missing("no such codec".into());
    };
    if let Some(gone) = elements
        .iter()
        .chain([&super::parser(codec)])
        .find(|e| gst::ElementFactory::find(e).is_none())
    {
        return Probed::Missing(format!("{gone} is not installed"));
    }
    match run_probe(backend, codec) {
        Ok(()) => {
            let encoder = elements.last().copied().unwrap_or_default();
            let (max_w, max_h) = max_size(encoder).unwrap_or((u32::MAX, u32::MAX));
            Probed::Works { max_w, max_h }
        }
        Err(e) => Probed::Failed(format!("{e:#}")),
    }
}

fn run_probe(backend: Backend, codec: Codec) -> Result<()> {
    let (w, h) = PROBE_SIZE;
    let input = Input {
        source: format!(
            "videotestsrc num-buffers=3 ! video/x-raw,width={w},height={h},framerate=60/1"
        ),
        label: format!("probe {}", backend.name()),
        frame_rate: None,
        live: false,
    };
    let cfg = EncoderConfig {
        codec,
        bitrate_kbps: 2000,
        backend,
        sharp: false,
    };
    // A fakesink: an appsink would hold the end of the stream back until its frames are
    // taken.
    let description = format!(
        "{} ! fakesink name=sink sync=false",
        encoding(&input, &cfg)?
    );
    let pipeline = gst::parse::launch(&description)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("not a pipeline"))?;
    // Opened as a stream opens it.
    super::tune(&pipeline, &cfg);
    let frames = Arc::new(AtomicU32::new(0));
    if let Some(pad) = pipeline.by_name("sink").and_then(|s| s.static_pad("sink")) {
        let frames = frames.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            frames.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    }
    let bus = pipeline.bus().ok_or_else(|| anyhow!("no bus"))?;
    let result = pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| anyhow!("does not start"))
        .and_then(|_| {
            let ended = bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(PROBE_TIMEOUT.as_millis() as u64),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
            match ended.as_ref().map(|m| m.view()) {
                Some(gst::MessageView::Eos(_)) => Ok(()),
                Some(gst::MessageView::Error(e)) => bail!("{}", e.error()),
                _ => bail!("no end of the test frames"),
            }
        })
        .and_then(|()| {
            anyhow::ensure!(frames.load(Ordering::Relaxed) > 0, "no frame came out");
            Ok(())
        });
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Probes `asked` side by side, each on a thread of its own and held to
/// [`probe_timeout`] from when it started.
fn probe(asked: &[Asked]) -> Vec<(Asked, Probed)> {
    let limit = probe_timeout();
    let (tx, rx) = mpsc::channel();
    let mut deadlines = Vec::new();
    for (index, &(backend, _, codec)) in asked.iter().enumerate() {
        deadlines.push(Instant::now() + limit);
        let sender = tx.clone();
        let alive = Alive::new();
        let spawned = std::thread::Builder::new()
            .name(format!("probe-{}", backend.name()))
            .spawn(move || {
                let _alive = alive;
                let _ = sender.send((index, probe_one(backend, codec)));
            });
        if let Err(e) = spawned {
            let _ = tx.send((index, Probed::Failed(e.to_string())));
        }
    }
    drop(tx);
    let mut results: Vec<Option<Probed>> = vec![None; asked.len()];
    // Until every probe answered or ran out of time, the soonest of them first.
    while let Some(due) = (0..asked.len())
        .filter(|i| results[*i].is_none())
        .map(|i| deadlines[i])
        .min()
    {
        match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
            Ok((index, probed)) => results[index] = Some(probed),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                for (i, result) in results.iter_mut().enumerate() {
                    if result.is_none() && deadlines[i] <= Instant::now() {
                        let (backend, _, codec) = asked[i];
                        warn!(
                            "{} {codec:?}: the probe did not end in {limit:?}",
                            backend.label()
                        );
                        *result = Some(Probed::Failed(format!(
                            "did not answer in {} s",
                            limit.as_secs()
                        )));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    asked
        .iter()
        .copied()
        .zip(results)
        .map(|(a, r)| (a, r.unwrap_or_else(|| Probed::Failed("no answer".into()))))
        .collect()
}

/// The threads of the probes, and the one finding what the encoders can do, that did
/// not end yet (one that hangs is left behind and stays counted), and a wake-up for
/// each that ends.
struct Running {
    count: Mutex<usize>,
    ended: Condvar,
}

fn running() -> &'static Running {
    static RUNNING: OnceLock<Running> = OnceLock::new();
    RUNNING.get_or_init(|| Running {
        count: Mutex::new(0),
        ended: Condvar::new(),
    })
}

/// Counts a thread of the probes from before it is made until it ends, or until it
/// turns out it could not be made.
struct Alive;

impl Alive {
    fn new() -> Self {
        *running()
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        Self
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        let running = running();
        *running
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        running.ended.notify_all();
    }
}

/// Waits up to `wait` for the probes under way to end: whether none is left. One
/// still running is inside a driver (VA-API, NVIDIA) whose library is torn down when
/// the process exits, which must not happen under that thread.
pub fn settle(wait: Duration) -> bool {
    let running = running();
    let until = Instant::now() + wait;
    let mut count = running
        .count
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *count > 0 {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        count = running
            .ended
            .wait_timeout(count, left)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
    true
}

/// The catalog of this run, once found.
struct Shared {
    catalog: Mutex<Option<Catalog>>,
    found: Condvar,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        catalog: Mutex::new(None),
        found: Condvar::new(),
    })
}

/// Finds what the encoders can do, on a thread of its own, once: from the cache where
/// it is current, the `retry` ones probed again in the background meanwhile; else
/// probed. Called when the program starts (after GStreamer), so the first headset does
/// not wait for it; the threads end once done, and an idle PC sleeps as before.
pub fn start_probing() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let alive = Alive::new();
        let spawned = std::thread::Builder::new()
            .name("encoders".into())
            .spawn(move || {
                let _alive = alive;
                find();
            });
        if let Err(e) = spawned {
            warn!("no thread to probe the encoders on: {e}; probing here");
            find();
        }
    });
}

fn publish(catalog: Catalog) {
    let shared = shared();
    *shared
        .catalog
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(catalog);
    shared.found.notify_all();
}

fn find() {
    let key = fingerprint();
    let path = crate::platform::cache_home().map(|d| d.join("prostir").join("encoders.txt"));
    let cached = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| Catalog::parse(&text, &key));
    let started = Instant::now();
    let Some(mut catalog) = cached else {
        let mut catalog = Catalog {
            key,
            path,
            ..Catalog::default()
        };
        for (asked, probed) in probe(&everything()) {
            catalog.add(asked, probed);
        }
        info!(
            "video encoders probed in {} ms: {}",
            started.elapsed().as_millis(),
            catalog.summary()
        );
        catalog.log_missing();
        catalog.save();
        publish(catalog);
        return;
    };
    catalog.path = path;
    info!("video encoders as found before: {}", catalog.summary());
    let again = catalog.to_probe();
    publish(catalog);
    if again.is_empty() {
        return;
    }
    let names: Vec<String> = again
        .iter()
        .map(|(b, _, c)| format!("{} {c:?}", b.label()))
        .collect();
    info!("probing again in the background: {}", names.join(", "));
    let results = probe(&again);
    let mut catalog = self::catalog();
    for (asked, probed) in results {
        catalog.add(asked, probed);
    }
    info!(
        "video encoders probed again in {} ms: {}",
        started.elapsed().as_millis(),
        catalog.summary()
    );
    catalog.log_missing();
    catalog.save();
}

/// The catalog of this run, locked.
pub struct CatalogGuard(MutexGuard<'static, Option<Catalog>>);

impl std::ops::Deref for CatalogGuard {
    type Target = Catalog;
    fn deref(&self) -> &Catalog {
        self.0.as_ref().expect("found before the guard is given")
    }
}

impl std::ops::DerefMut for CatalogGuard {
    fn deref_mut(&mut self) -> &mut Catalog {
        self.0.as_mut().expect("found before the guard is given")
    }
}

/// The catalog of this run, waiting for it to be found if it is not yet (it is
/// started with the program; this starts it where it was not).
pub fn catalog() -> CatalogGuard {
    start_probing();
    let shared = shared();
    let mut guard = shared
        .catalog
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while guard.is_none() {
        guard = shared
            .found
            .wait(guard)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    CatalogGuard(guard)
}

/// The fake hardware encoder of the end-to-end tests, as `PROSTIR_FAKE_ENCODER` says:
/// `works` (any number of sessions), `sessions=N` (one more does not open once it
/// started), `refuses=N` (one more does not start at all), `broken` (its probe fails),
/// `hangs` (its probe never ends), `slow` (works, its probe takes a second).
#[cfg(feature = "fake-desktop")]
pub mod fake {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::Probed;

    /// Fake hardware encoders open now.
    pub static OPEN: AtomicU32 = AtomicU32::new(0);

    pub fn mode() -> Option<String> {
        std::env::var("PROSTIR_FAKE_ENCODER")
            .ok()
            .filter(|m| !m.is_empty())
    }

    /// The sessions it opens at once; none: any number.
    pub fn sessions() -> Option<u32> {
        let mode = mode()?;
        mode.strip_prefix("sessions=")
            .or_else(|| mode.strip_prefix("refuses="))?
            .parse()
            .ok()
    }

    /// One more than it opens does not start at all (rather than starting and then
    /// saying it did not open).
    pub fn refuses() -> bool {
        mode().is_some_and(|m| m.starts_with("refuses="))
    }

    /// Its probe's result, where the mode makes one up; none: probed as x264.
    pub(super) fn probe() -> Option<Probed> {
        match mode()?.as_str() {
            "broken" => Some(Probed::Failed("broken on purpose".into())),
            "slow" => {
                std::thread::sleep(std::time::Duration::from_secs(1));
                None
            }
            "hangs" => {
                std::thread::sleep(std::time::Duration::from_secs(600));
                Some(Probed::Failed("hung".into()))
            }
            _ => None,
        }
    }

    /// Whether one more would open now.
    pub fn opens() -> bool {
        sessions().is_none_or(|max| OPEN.load(Ordering::Relaxed) < max)
    }
}

#[cfg(test)]
mod tests;
