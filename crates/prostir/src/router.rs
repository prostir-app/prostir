//! Delivery of what the encoders and the sound capture make to every headset with a
//! session: each session's connection (its outbox) gets the streams on its own terms.
//!
//! A stream is encoded once and fans out. What is shared belongs to the stream (its
//! size and codec, when a keyframe was last asked for); what each headset has of it
//! belongs to the session (whether the stream was announced to it and in which
//! generation, the frame numbers, whether it waits for a keyframe, how long frames took
//! to show). So one headset that drops frames, is taken off, or cannot decode a stream
//! changes nothing for the others: the encoder is asked for a keyframe that all get,
//! and a stream too big for one headset still goes to the rest.
//!
//! The mouse pointer, where the desktop tells it apart from the pictures, fans out
//! here too (see `cursor.rs`): each headset hears where it is across the picture of a
//! display it was told about, in a look it has.
//!
//! Which streams get every frame, from where the headsets look, is kept here too
//! (`priority.rs`): the router sets each encoder's frame gate to it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use prostir_proto::state::{self, Gen};
use prostir_proto::stats::{Histogram, Period};
use prostir_proto::{Codec, HostMsg};

use crate::cursor::{self, At, Pointer, Told};
use crate::encoder::{EncoderEvent, FrameTiming, Gate, Level};
use crate::events::SessionId;
use crate::outbox::Outbox;
use crate::platform::{CursorEvent, OutputKey};
use crate::screen::ScreenId;

mod priority;

use priority::Priority;

/// How often the router logs what each stream sent (see [`Router::report`]).
const REPORT_EVERY: Duration = Duration::from_secs(10);

/// Slices of sound that are nothing but zeros, in a row, after which no more go out:
/// 200 ms of silence. The first slice with sound goes at once, so nothing waits for
/// it; the headset plays a gap as silence.
const SILENT_SLICES: u32 = 20;
/// Slices of silence sent right before the sound that ends a gap: the headset's
/// player starts from an empty buffer, and these fill it, so the sound does not
/// start with a click or run dry at once.
const PRIME_SLICES: u32 = 2;

/// Whether the sound is worth sending: not after [`SILENT_SLICES`] of silence.
#[derive(Debug, Default)]
struct Quiet {
    /// Slices of silence in a row.
    zeros: u32,
    /// Slices not sent since the last report, being silence.
    held: u32,
}

/// What goes out of a slice of sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// The slice, as it is.
    Send,
    /// This many slices of silence first, then the slice: sound after a gap.
    AfterSilence(u32),
    /// Nothing: silence after silence.
    Hold,
}

impl Quiet {
    /// What goes out of `pcm`.
    fn pass(&mut self, pcm: &[u8]) -> Pass {
        // No early exit: the whole slice is looked at in wide steps, about 100 ns.
        if pcm.iter().fold(0, |a, b| a | b) != 0 {
            let gap = self.zeros > SILENT_SLICES;
            self.zeros = 0;
            return if gap {
                Pass::AfterSilence(PRIME_SLICES)
            } else {
                Pass::Send
            };
        }
        self.zeros = self.zeros.saturating_add(1);
        if self.zeros <= SILENT_SLICES {
            Pass::Send
        } else {
            self.held += 1;
            Pass::Hold
        }
    }
}

/// A keyframe is asked of an encoder at most this often, whichever headset waits.
const KEYFRAME_EVERY: Duration = Duration::from_millis(300);
/// A headset that keeps losing frames while others get the same stream asks for a
/// keyframe less and less often, down to once in this (each one costs the others).
const KEYFRAME_SLOWEST: Duration = Duration::from_secs(2);
/// Without asking for this long, such a headset counts as keeping up again.
const KEYFRAME_RECOVERED: Duration = Duration::from_secs(4);

/// When one headset asks for keyframes of a stream others get too: every 300 ms at
/// first, then twice as long each time it asks again, up to 2 s, and every 300 ms
/// again once it went 4 s without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyAsks {
    every: Duration,
    last: Option<Instant>,
}

impl Default for KeyAsks {
    fn default() -> Self {
        Self {
            every: KEYFRAME_EVERY,
            last: None,
        }
    }
}

impl KeyAsks {
    /// Whether this headset asks for a keyframe at `now`: `shared` when other headsets
    /// get the stream too (alone, it asks whenever the stream may), `waiting` when its
    /// keyframe asked before still waits to go out to it.
    fn due(&mut self, now: Instant, shared: bool, waiting: bool) -> bool {
        if !shared {
            *self = Self::default();
            return true;
        }
        if waiting {
            return false;
        }
        match self.last {
            Some(t) if now.duration_since(t) >= KEYFRAME_RECOVERED => {
                self.every = KEYFRAME_EVERY;
            }
            Some(t) if now.duration_since(t) < self.every => return false,
            Some(_) => self.every = (self.every * 2).min(KEYFRAME_SLOWEST),
            None => {}
        }
        self.last = Some(now);
        true
    }
}

/// The generation of the next stream start: one counter for every stream of this run
/// of the app, so a frame or a request of an older start is never taken for a new one,
/// across sessions too.
static NEXT_GEN: AtomicU32 = AtomicU32::new(1);

fn next_gen() -> Gen {
    Gen(NEXT_GEN.fetch_add(1, Ordering::Relaxed))
}

/// What one stream's encoder made over one window of the router's report (see
/// [`Router::report`]), whoever got it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// How long the window was.
    pub window_ms: u32,
    /// Frames encoded, and of them those that repeat the picture before (a source's
    /// keepalive on a still screen: encoded for nothing new).
    pub frames: u32,
    pub repeats: u32,
    /// Their bytes.
    pub bytes: u64,
    /// Keyframes among them, and the largest.
    pub keyframes: u32,
    pub keyframe_max: u32,
    /// Keyframes asked for early, to sharpen a still picture after a big change (see
    /// [`Router::still_keyframes`]).
    pub sharpened: u32,
    /// From a frame entering the converter to leaving the parser, microseconds.
    pub encode_us_avg: u32,
    pub encode_us_max: u32,
    /// What its frame gate let through, once the encoder runs (see `encoder/gate.rs`).
    pub gate: Option<GateStats>,
}

/// What a stream's frame gate let through over one window of the router's report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateStats {
    /// Its level at the end of the window.
    pub level: Level,
    /// The monitor's full rate, and what the level lets through, mHz.
    pub rate_mhz: u32,
    pub fps_mhz: u32,
    /// Frames that waited for their turn at the gate, and frames not encoded since
    /// nothing changed.
    pub held: u32,
    pub unchanged: u32,
}

impl GateStats {
    /// `half rate 30/s, 12 frames held by rate, 3 skipped for no change`.
    pub fn summary(&self) -> String {
        let fps = f64::from(self.fps_mhz) / 1e3;
        let level = match self.level {
            Level::Full => format!("full rate {fps:.0}/s"),
            Level::Side => format!("half rate {fps:.0}/s"),
            Level::Glance => format!("Focus {fps:.0}/s"),
        };
        format!(
            "{level}, {} frames held by rate, {} skipped for no change",
            self.held, self.unchanged
        )
    }
}

impl StreamStats {
    /// `encoded 600 frames (60.0/s, 0.5/s a still picture again), 24.1 Mbit/s,
    /// keyframes 1 (1 to sharpen a still picture), max 750 KB; encoder avg 4.3 max
    /// 9.1 ms`.
    pub fn summary(&self) -> String {
        let secs = (f64::from(self.window_ms) / 1e3).max(1e-3);
        format!(
            "encoded {} frames ({:.1}/s, {:.1}/s a still picture again), {:.1} Mbit/s, \
             keyframes {} ({} to sharpen a still picture), max {:.0} KB; encoder avg {:.1} \
             max {:.1} ms",
            self.frames,
            f64::from(self.frames) / secs,
            f64::from(self.repeats) / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            self.keyframes,
            self.sharpened,
            f64::from(self.keyframe_max) / 1024.0,
            f64::from(self.encode_us_avg) / 1e3,
            f64::from(self.encode_us_max) / 1e3,
        ) + &self
            .gate
            .map_or_else(String::new, |g| format!("; gate: {}", g.summary()))
    }

    /// Whether its encoder took longer for a frame than a frame of its monitor lasts
    /// at its full rate, while getting every frame of a busy picture (more than a
    /// quarter of that rate): it cannot keep up with the monitor.
    pub fn behind(&self) -> bool {
        let Some(g) = self
            .gate
            .filter(|g| g.level == Level::Full && g.rate_mhz > 0)
        else {
            return false;
        };
        let secs = (f64::from(self.window_ms) / 1e3).max(1e-3);
        let busy = f64::from(self.frames) / secs > f64::from(g.rate_mhz) / 4e3;
        busy && u64::from(self.encode_us_avg) * u64::from(g.rate_mhz) > 1_000_000_000
    }
}

/// What a stream's encoder made since the last report.
#[derive(Debug, Default)]
struct Tally {
    since: Option<Instant>,
    frames: u32,
    repeats: u32,
    bytes: u64,
    keyframes: u32,
    keyframe_max: u32,
    sharpened: u32,
    timed: u32,
    encode_us: u64,
    encode_us_max: u32,
}

impl Tally {
    fn add(&mut self, keyframe: bool, bytes: usize, timing: Option<FrameTiming>) {
        self.since.get_or_insert_with(Instant::now);
        self.frames += 1;
        self.bytes += bytes as u64;
        if keyframe {
            self.keyframes += 1;
            self.keyframe_max = self.keyframe_max.max(bytes as u32);
        }
        if let Some(t) = timing {
            self.repeats += u32::from(t.repeat);
            self.timed += 1;
            self.encode_us += u64::from(t.encode_us);
            self.encode_us_max = self.encode_us_max.max(t.encode_us);
        }
    }

    /// What it counted up to `now`, and counts anew from there.
    fn take(&mut self, now: Instant) -> StreamStats {
        let t = std::mem::take(self);
        self.since = Some(now);
        StreamStats {
            window_ms: t
                .since
                .map_or(0, |s| now.duration_since(s).as_millis() as u32),
            frames: t.frames,
            repeats: t.repeats,
            bytes: t.bytes,
            keyframes: t.keyframes,
            keyframe_max: t.keyframe_max,
            sharpened: t.sharpened,
            encode_us_avg: (t.encode_us / u64::from(t.timed.max(1))) as u32,
            encode_us_max: t.encode_us_max,
            gate: None,
        }
    }
}

/// A stream as its encoder makes it, the same for every headset.
struct StreamSlot {
    /// The screen's name, for the log.
    name: String,
    /// Its output on the desktop, where the pointer is said to be.
    key: Option<OutputKey>,
    /// Its frames keep the pointer (a window panel): it counts as on none of the
    /// displays there.
    keeps_pointer: bool,
    size: Option<(u32, u32)>,
    /// What the encoder makes, announced with the stream.
    codec: Codec,
    /// When a keyframe was last asked for: asking again on every frame while waiting
    /// would make the encoder emit one huge keyframe after another.
    keyframe_asked: Option<Instant>,
    /// When the encoder started, for the log of how long its first picture took.
    since: Instant,
    /// When its last two frames came.
    last_frames: [Option<Instant>; 2],
    /// Asks its encoder for a keyframe, once the encoder runs.
    keyframer: Option<crate::encoder::Keyframer>,
    /// When a headset last asked for a keyframe of it.
    headset_asked: Option<Instant>,
    /// A headset asked again within [`HEADSET_KEYFRAMES`] of that: one keyframe is
    /// asked for once the time is up (every request within it counts as this one).
    headset_pending: bool,
    /// A headset waits for a keyframe the router could not ask for yet (within
    /// [`KEYFRAME_EVERY`] of the one before, or not yet that headset's turn): asked for
    /// with the next frame, or by the tick on a still screen (see
    /// [`Router::pending_keyframes`]).
    key_pending: bool,
    /// Where a frame's deliveries are listed, kept between frames so a frame
    /// allocates nothing for them (see [`Router::fan_out`]).
    outs: Vec<Out>,
    /// What its encoder made since the last report, and in the window before.
    tally: Tally,
    measured: Option<StreamStats>,
    /// When its picture last changed (a frame that was not a repeat of the one before).
    changed: Option<Instant>,
    /// A frame of a new picture went since the last keyframe, and no keyframe was
    /// asked for since the screen went still (see [`Router::still_keyframes`]).
    unsettled: bool,
    /// Its encoder's bitrate, kbit/s, as it last said (0: not yet).
    kbps: u32,
    /// Its encoder makes a keyframe of a still picture sharper than a big frame (see
    /// `encoder::Tuning::sharpens`): only then is a coarse picture sharpened early. One
    /// that caps its keyframes (VA-API's faster preset: 37 dB of a 2560x1440 picture
    /// against 38.6 of a big frame at 60 Mbit/s) would make it worse.
    sharpens: bool,
    /// A frame of a new picture since the last keyframe was big (a new file or tab:
    /// see [`big_frame`]), so it went out coarse within one frame's budget: a still
    /// picture after it is sharpened soon (see [`Router::still_keyframes`]).
    coarse: bool,
    /// The gate its frames go through, once its encoder runs: set to its level (see
    /// [`Router::priorities`]).
    gate: Option<Arc<Gate>>,
    /// Its encoder was said to fall behind its monitor's full rate.
    behind: bool,
}

/// A frame larger than this many frames of the stream's bitrate is big: the picture
/// changed a lot at once (another file, another tab), more than one frame's budget
/// holds, and went out coarse. Typing, scrolling and moving windows make frames within
/// about one budget.
const BIG_FRAMES: u64 = 4;
/// The frame rate the budget of a frame is reckoned at: a faster monitor's frames are
/// smaller, so fewer of its changes count as big.
const BUDGET_RATE: u64 = 60;

/// Whether a frame of `bytes` is big for a stream at `kbps` (see [`BIG_FRAMES`]).
fn big_frame(bytes: usize, kbps: u32) -> bool {
    kbps > 0 && bytes as u64 * 8 * BUDGET_RATE > BIG_FRAMES * u64::from(kbps) * 1000
}

/// Keyframes headsets ask for, of one stream, at most this often.
const HEADSET_KEYFRAMES: Duration = Duration::from_millis(300);

/// Two frames within this: the picture moves, so a keyframe asked for comes soon.
const FLOWING: Duration = Duration::from_millis(500);

impl StreamSlot {
    /// Whether a keyframe a headset asked for within 300 ms of the one before is to be
    /// asked for now; then it counts as asked.
    fn pending_due(&mut self, now: Instant) -> bool {
        let due = self.headset_pending
            && self
                .headset_asked
                .is_none_or(|t| now.duration_since(t) >= HEADSET_KEYFRAMES);
        if due {
            self.headset_pending = false;
            self.headset_asked = Some(now);
            debug!(
                "{}: a keyframe asked for within {} ms of the one before, asked for now",
                self.name,
                HEADSET_KEYFRAMES.as_millis()
            );
        }
        due
    }

    /// When it is to get the keyframe of a still screen (see
    /// [`Router::still_keyframes`]): `early` after a big change, else `after`.
    fn still_due(&self, after: Duration, early: Duration) -> Option<Instant> {
        let wait = if self.coarse { early } else { after };
        self.changed.filter(|_| self.unsettled).map(|t| t + wait)
    }

    /// Its picture moves: it sends frames now, and a keyframe asked of it comes with
    /// the next.
    fn flowing(&self, now: Instant) -> bool {
        self.last_frames
            .iter()
            .all(|t| t.is_some_and(|t| now.duration_since(t) < FLOWING))
    }
}

/// What one headset has of one stream.
struct Delivery {
    /// The current size and codec were announced to this headset.
    announced: bool,
    /// The headset has this stream (announced on this connection, perhaps by an
    /// encoder since replaced), so it is told when the stream ends.
    known: bool,
    /// The generation announced last, and the number of its next frame.
    generation: Gen,
    seq: u32,
    need_keyframe: bool,
    /// Frame number → when the encoded frame was queued for sending.
    sent: BTreeMap<u32, Instant>,
    /// Encoded on the PC → handed to the headset's compositor, as the headset reports
    /// back with `FrameShown` (for some frames only).
    shown: Histogram,
    /// Frames not queued because the stream waited for a keyframe.
    skipped: u32,
    /// Frames came that this headset did not get (its display not in its list yet, or
    /// the headset taken off), so it needs a fresh picture.
    held: bool,
    /// The picture is larger than this headset decodes in its codec: nothing of it
    /// goes to it.
    too_big: bool,
    /// When it asks for keyframes.
    key_asks: KeyAsks,
    /// Since when this headset waits for a fresh picture (it just came, or was put on
    /// again): until a keyframe is queued for it.
    fresh_since: Option<Instant>,
}

impl Default for Delivery {
    fn default() -> Self {
        Self {
            announced: false,
            known: false,
            generation: Gen(0),
            seq: 0,
            need_keyframe: true,
            sent: BTreeMap::new(),
            shown: Histogram::default(),
            skipped: 0,
            held: false,
            too_big: false,
            key_asks: KeyAsks::default(),
            fresh_since: None,
        }
    }
}

/// One session's headset, as the router sends to it.
struct Receiver {
    tx: Arc<Outbox>,
    /// What it decodes: no stream larger goes to it.
    headset: state::HeadsetCaps,
    /// On the head: frames and sound go to it only then.
    worn: bool,
    /// The displays this headset was told about in its session (`Displays`): a stream
    /// starts only once its display is among them, since the start goes in a lane of
    /// its own and must not overtake the list.
    published: BTreeSet<ScreenId>,
    /// What it has of each stream.
    streams: BTreeMap<ScreenId, Delivery>,
    /// What it was sent of the mouse pointer.
    cursor: Told,
}

impl Receiver {
    /// Whether the picture of `slot` is larger than this headset decodes.
    fn too_big(&self, slot: &StreamSlot) -> bool {
        slot.size
            .is_some_and(|(w, h)| !self.headset.fits(slot.codec, w, h))
    }
}

struct RouterInner {
    streams: BTreeMap<ScreenId, StreamSlot>,
    /// Every session's headset, by the session's number.
    receivers: BTreeMap<SessionId, Receiver>,
    report: Period,
    /// The PC's sound, held back while silent.
    quiet: Quiet,
    /// The mouse pointer as the desktop said it last.
    pointer: Pointer,
    /// The pointer goes to the headsets apart from the pictures: every one of them
    /// draws it (see [`cursor::apart_for`]).
    cursor_apart: bool,
    /// The desktop tells the pointer apart from the pictures (PC feature 16): without
    /// it, nothing of the pointer goes to any headset.
    cursor_source: bool,
    /// Where the headsets look, for how many frames each stream gets.
    priority: Priority,
}

impl RouterInner {
    /// Sets every stream's frame gate to its level now; said where it changed.
    fn set_levels(&self, now: Instant) {
        for (id, s) in &self.streams {
            let Some(gate) = &s.gate else {
                continue;
            };
            let (level, why) = self.priority.level(*id, now);
            if !gate.set_level(level) {
                continue;
            }
            let fps = f64::from(gate.fps_mhz()) / 1e3;
            let why = why.says();
            match level {
                Level::Full => info!("{}: every frame, {fps:.0} a second ({why})", s.name),
                Level::Side => info!("{}: half its frames, {fps:.0} a second ({why})", s.name),
                Level::Glance => info!("{}: {fps:.0} frames a second, Focus ({why})", s.name),
            }
        }
    }

    /// The streams larger than every headset decodes (none without a headset): their
    /// display stands failed, since nobody could show it.
    fn too_big_for_all(&self) -> Vec<TooBig> {
        if self.receivers.is_empty() {
            return Vec::new();
        }
        self.streams
            .iter()
            .filter(|(_, s)| self.receivers.values().all(|r| r.too_big(s)))
            .filter_map(|(id, s)| Some((*id, s.size?, s.codec)))
            .collect()
    }

    /// Whether the pointer goes apart from the pictures for the headsets there are:
    /// where the desktop tells it, and every one of them draws it.
    fn cursor_apart_now(&self) -> bool {
        self.cursor_source && cursor::apart_for(self.receivers.values().map(|r| &r.headset))
    }

    /// The headsets came or went: the pointer goes apart from the pictures, or back
    /// into them. Back into them, each headset told where it was hears that it is
    /// nowhere (the pictures have it now). Without a headset nothing changes: one
    /// that loses its connection for a moment and comes back starts nothing over.
    fn cursor_follow_headsets(&mut self) {
        if self.receivers.is_empty() {
            return;
        }
        let apart = self.cursor_apart_now();
        if apart == self.cursor_apart {
            return;
        }
        self.cursor_apart = apart;
        info!(
            "the mouse pointer {}",
            if apart {
                "goes apart from the pictures"
            } else {
                "goes into the pictures"
            }
        );
        if !apart {
            for r in self.receivers.values_mut() {
                if r.cursor.told() && r.cursor.news(At::NOWHERE) {
                    r.tx.cursor(At::NOWHERE.msg());
                }
            }
        }
    }

    /// Tells the headsets on the head where the pointer is now and how it looks, where
    /// that is news to them (`only`: to that one). Nothing while it is in the pictures.
    fn tell_cursor(&mut self, only: Option<SessionId>) {
        if !self.cursor_apart {
            return;
        }
        let RouterInner {
            streams,
            receivers,
            pointer,
            ..
        } = self;
        // The stream of the output it is on (not a window panel, whose frames keep the
        // pointer), its point across that picture, and the picture's size as streamed.
        let on = pointer.at().and_then(|(key, x, y, native)| {
            streams.iter().find_map(|(id, s)| {
                (s.key.as_ref() == Some(key) && !s.keeps_pointer)
                    .then(|| Some((*id, cursor::uv_of(x, y, native), s.size?)))
                    .flatten()
            })
        });
        let look = pointer.look(on.map(|(_, _, size)| size));
        let shape = look.map_or(0, |(id, _)| id);
        for (session, r) in receivers.iter_mut() {
            if only.is_some_and(|o| o != *session) || !r.worn {
                continue;
            }
            let at = match on {
                Some((id, (u, v), _))
                    if r.published.contains(&id)
                        && r.streams.get(&id).is_some_and(|d| !d.too_big) =>
                {
                    At {
                        display: Some(id.wire()),
                        u,
                        v,
                        shape,
                    }
                }
                _ => At {
                    shape,
                    ..At::NOWHERE
                },
            };
            if !r.cursor.news(at) {
                continue;
            }
            if r.cursor.needs(shape)
                && let Some((_, msg)) = look
            {
                r.tx.control(msg.clone());
            }
            r.tx.cursor(at.msg());
        }
    }
}

/// How a stream broke: why, and whether its source lost its stream (the desktop's)
/// rather than its encoder failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broke {
    pub why: String,
    pub by_source: bool,
}

/// A stream larger than every headset decodes: its display, size and codec.
pub type TooBig = (ScreenId, (u32, u32), Codec);

/// A frame to queue for one headset.
struct Out {
    session: SessionId,
    /// The stream's first frame for this headset: its start goes before it.
    start: Option<HostMsg>,
    /// The frame goes too (not when the headset waits for a keyframe).
    frame: bool,
}

/// Delivers encoder output and sound to every headset with a session.
pub struct Router {
    /// A screenshot waiting for the next keyframe of stream `id` (see `screenshot`).
    pub snapshot: Mutex<Option<(ScreenId, std::sync::mpsc::SyncSender<Vec<u8>>)>>,
    inner: Mutex<RouterInner>,
    /// Streams whose encoder broke, with why: each is started again by itself (see
    /// `Worker::check_closed`), the others go on.
    broken: Mutex<Vec<(ScreenId, Broke)>>,
    /// Streams whose encoder did not open, with why: each goes on with the next
    /// encoder for it (see `Displays::tick`).
    unopened: Mutex<Vec<(ScreenId, crate::encoder::NotOpened)>>,
    /// Streams larger than every headset decodes, with their size and codec, for
    /// `Displays` to mark them failed.
    too_big: Mutex<Vec<TooBig>>,
    /// The control loop, told when a keyframe is asked of a still screen (its source
    /// then has to send a picture: see [`Router::keyframe_asked`]).
    events: Mutex<Option<crate::events::EventSink>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RouterInner {
                streams: BTreeMap::new(),
                receivers: BTreeMap::new(),
                report: Period::new(REPORT_EVERY),
                quiet: Quiet::default(),
                pointer: Pointer::default(),
                cursor_apart: false,
                cursor_source: false,
                priority: Priority::default(),
            }),
            broken: Mutex::new(Vec::new()),
            unopened: Mutex::new(Vec::new()),
            too_big: Mutex::new(Vec::new()),
            snapshot: Mutex::new(None),
            events: Mutex::new(None),
        }
    }

    /// The headset of session `session`, which decodes what `headset` says and is on
    /// the head or not (`worn`), gets the streams through `tx`, each only as large as
    /// it decodes. One larger never starts there (no `StreamStart`): its `Displays`
    /// says it failed instead (see `Displays::displays_part`), and one larger than
    /// every headset decodes [`crate::displays::Displays`] marks failed for all, so
    /// one display too big is not the end of anyone's session.
    pub fn attach(
        &self,
        session: SessionId,
        tx: Arc<Outbox>,
        headset: state::HeadsetCaps,
        worn: bool,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if inner.receivers.is_empty() {
            inner.report = Period::new(REPORT_EVERY);
            let now = Instant::now();
            for s in inner.streams.values_mut() {
                s.tally.take(now);
            }
        }
        let mut receiver = Receiver {
            tx,
            headset,
            worn,
            published: BTreeSet::new(),
            streams: BTreeMap::new(),
            cursor: Told::default(),
        };
        let now = Instant::now();
        for (id, s) in &inner.streams {
            let too_big = receiver.too_big(s);
            if let (true, Some((w, h))) = (too_big, s.size) {
                info!(
                    "{}: {w}x{h} in {:?} is larger than one headset decodes: not sent to it",
                    s.name, s.codec
                );
            }
            let delivery = Delivery {
                too_big,
                fresh_since: Some(now),
                ..Delivery::default()
            };
            receiver.streams.insert(*id, delivery);
        }
        let tells = receiver.headset.has(state::caps::headset::GAZE);
        inner.priority.attach(session, worn, tells);
        inner.set_levels(now);
        inner.receivers.insert(session, receiver);
        // What was found for the headsets before is looked at again with this one.
        *self.too_big.lock().unwrap() = inner.too_big_for_all();
        // Its snapshot is queued: the pointer as it is now comes right after it.
        inner.cursor_follow_headsets();
        inner.tell_cursor(Some(session));
    }

    /// The session ended: nothing more goes to its headset. A stream only it decoded
    /// is now larger than every headset left decodes, and found so.
    pub fn detach(&self, session: SessionId) {
        let mut inner = self.inner.lock().unwrap();
        inner.receivers.remove(&session);
        inner.priority.detach(session);
        inner.set_levels(Instant::now());
        *self.too_big.lock().unwrap() = inner.too_big_for_all();
        inner.cursor_follow_headsets();
    }

    /// The headset of `session` is on the head (`worn`) or not: frames and sound go to
    /// it only while it is. Put on again, it returns the streams that sent meanwhile,
    /// which it needs a fresh picture of.
    pub fn set_worn(&self, session: SessionId, worn: bool) -> Vec<ScreenId> {
        let mut inner = self.inner.lock().unwrap();
        let Some(r) = inner.receivers.get_mut(&session) else {
            return Vec::new();
        };
        if r.worn == worn {
            return Vec::new();
        }
        r.worn = worn;
        let now = Instant::now();
        inner.priority.set_worn(session, worn);
        inner.set_levels(now);
        if !worn {
            return Vec::new();
        }
        let Some(r) = inner.receivers.get_mut(&session) else {
            return Vec::new();
        };
        let fresh = r
            .streams
            .iter_mut()
            .filter(|(_, d)| !d.too_big)
            .filter_map(|(id, d)| {
                d.need_keyframe = true;
                d.fresh_since = Some(now);
                std::mem::take(&mut d.held).then_some(*id)
            })
            .collect();
        // Where the pointer is now: nothing of it went while it was off the head.
        inner.tell_cursor(Some(session));
        fresh
    }

    /// Whether stream `id` is still larger than every headset decodes (a stream found
    /// too big may have started over since, in another size or codec, or another
    /// headset that decodes it came).
    pub fn still_too_big(&self, id: ScreenId) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(slot) = inner.streams.get(&id) else {
            return false;
        };
        !inner.receivers.is_empty() && inner.receivers.values().all(|r| r.too_big(slot))
    }

    /// Whether a headset with a session decodes a picture of `size` in `codec`
    /// (without one, a display too big for the last ones waits for the next).
    pub fn fits(&self, codec: Codec, (w, h): (u32, u32)) -> bool {
        self.inner
            .lock()
            .unwrap()
            .receivers
            .values()
            .any(|r| r.headset.fits(codec, w, h))
    }

    /// How many headsets have a session.
    pub fn receivers(&self) -> usize {
        self.inner.lock().unwrap().receivers.len()
    }

    /// What the headset of every session decodes.
    pub fn headsets(&self) -> Vec<state::HeadsetCaps> {
        let inner = self.inner.lock().unwrap();
        inner
            .receivers
            .values()
            .map(|r| r.headset.clone())
            .collect()
    }

    /// Whether stream `id` sends frames now (its picture moves), so a keyframe asked of
    /// its encoder comes with the next one.
    pub fn flowing(&self, id: ScreenId) -> bool {
        self.inner
            .lock()
            .unwrap()
            .streams
            .get(&id)
            .is_some_and(|s| s.flowing(Instant::now()))
    }

    /// Whether stream `id` goes to a headset on the head already and sends frames now
    /// (its picture moves), so a keyframe asked of its encoder comes with the next
    /// one: starting it over would only make that headset miss a moment.
    pub fn live(&self, id: ScreenId) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.streams.get(&id).is_some_and(|s| {
            s.flowing(Instant::now())
                && inner
                    .receivers
                    .values()
                    .any(|r| r.worn && r.streams.get(&id).is_some_and(|d| d.announced))
        })
    }

    /// Streams a headset on the head has waited `after` or longer for a fresh picture
    /// of, while nothing at all came of them (a still picture: a keyframe asked for
    /// never comes); each then waits again from now. Their encoder has to start over
    /// to send its picture.
    pub fn starved(&self, after: Duration) -> Vec<ScreenId> {
        let mut inner = self.inner.lock().unwrap();
        let RouterInner {
            streams, receivers, ..
        } = &mut *inner;
        let now = Instant::now();
        let mut starved = Vec::new();
        for (id, slot) in streams.iter() {
            // One that never sent a frame is starting: its first is a keyframe.
            let Some(last) = slot.last_frames[0] else {
                continue;
            };
            let mut waits = false;
            for r in receivers.values_mut().filter(|r| r.worn) {
                let Some(d) = r.streams.get_mut(id) else {
                    continue;
                };
                if d.too_big || !r.published.contains(id) {
                    continue;
                }
                if d.fresh_since
                    .is_some_and(|t| last < t && now.duration_since(t) >= after)
                {
                    d.fresh_since = Some(now);
                    waits = true;
                }
            }
            if waits {
                starved.push(*id);
            }
        }
        starved
    }

    /// Streams found larger than every headset decodes since the last call.
    pub fn take_too_big(&self) -> Vec<TooBig> {
        std::mem::take(&mut *self.too_big.lock().unwrap())
    }

    /// The headset of `session` was told about these displays (`Displays` is queued):
    /// their streams may start for it. Returns the streams that may start only now
    /// and sent frames before: they need a fresh picture.
    pub fn publish(&self, session: SessionId, ids: &BTreeSet<ScreenId>) -> Vec<ScreenId> {
        let mut inner = self.inner.lock().unwrap();
        let Some(r) = inner.receivers.get_mut(&session) else {
            return Vec::new();
        };
        let new: Vec<ScreenId> = ids.difference(&r.published).copied().collect();
        r.published.clone_from(ids);
        let now = Instant::now();
        let fresh: Vec<ScreenId> = new
            .into_iter()
            .filter(|id| {
                r.streams.get_mut(id).is_some_and(|d| {
                    let held = std::mem::take(&mut d.held);
                    if held {
                        d.fresh_since = Some(now);
                    }
                    held
                })
            })
            .collect();
        // The pointer may be on a display it was just told about, or one it lost.
        inner.tell_cursor(Some(session));
        fresh
    }

    /// Whether the pictures leave the pointer out now: every headset with a session
    /// draws it itself.
    pub fn cursor_apart(&self) -> bool {
        self.inner.lock().unwrap().cursor_apart
    }

    /// What the desktop says of the pointer (from its own thread): each headset hears
    /// what is news to it, while the pointer is apart from the pictures.
    pub fn cursor(&self, event: CursorEvent) {
        let mut inner = self.inner.lock().unwrap();
        if inner.pointer.take(event) {
            inner.tell_cursor(None);
        }
    }

    /// Forgets the pointer: the desktop stops telling it, or is about to tell it anew.
    pub fn cursor_forget(&self) {
        self.inner.lock().unwrap().pointer.clear();
    }

    /// Whether the desktop tells the pointer apart from the pictures (PC feature 16):
    /// only then does it go apart, with the headsets that all draw it.
    pub fn set_cursor_source(&self, tells: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.cursor_source = tells;
        inner.cursor_follow_headsets();
    }

    /// Stream `id` is of output `key`, where the desktop says the pointer is; one
    /// whose frames keep the pointer (`keeps_pointer`: a window panel) counts as on
    /// none of the displays.
    pub fn set_stream_output(&self, id: ScreenId, key: OutputKey, keeps_pointer: bool) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(slot) = inner.streams.get_mut(&id) {
            slot.key = Some(key);
            slot.keeps_pointer = keeps_pointer;
            inner.tell_cursor(None);
        }
    }

    /// Logs, every [`REPORT_EVERY`] while a headset is connected, one line per stream
    /// and headset: what the outbox sent and dropped, how long frames waited there,
    /// keyframes, and encoded → shown in the headset. Then one line for sound and one
    /// for the round trip of the pings, for each headset. With more than one headset,
    /// each line names its session.
    pub fn report(&self) {
        struct Lines {
            session: SessionId,
            stats: crate::outbox::OutboxStats,
            streams: Vec<(ScreenId, String, Histogram, u32)>,
        }
        let (window, encoded, silent, lines) = {
            let mut inner = self.inner.lock().unwrap();
            let RouterInner {
                streams,
                receivers,
                report,
                quiet,
                ..
            } = &mut *inner;
            if receivers.is_empty() {
                return;
            }
            let now = Instant::now();
            let Some(window) = report.due(now) else {
                return;
            };
            let silent = std::mem::take(&mut quiet.held);
            // What each encoder made, whoever got it.
            let encoded: Vec<(String, StreamStats, Option<bool>)> = streams
                .values_mut()
                .map(|s| {
                    let mut stats = s.tally.take(now);
                    stats.gate = s.gate.as_ref().map(|g| {
                        let counts = g.take_counts();
                        GateStats {
                            level: g.level(),
                            rate_mhz: g.rate_mhz(),
                            fps_mhz: g.fps_mhz(),
                            held: counts.held,
                            unchanged: counts.unchanged,
                        }
                    });
                    s.measured = Some(stats);
                    // Said once when its encoder falls behind, once when it keeps up.
                    let news = (stats.frames > 0)
                        .then(|| stats.behind())
                        .filter(|b| *b != s.behind);
                    if let Some(b) = news {
                        s.behind = b;
                    }
                    (s.name.clone(), stats, news)
                })
                .collect();
            let lines: Vec<Lines> = receivers
                .iter_mut()
                .map(|(session, r)| Lines {
                    session: *session,
                    stats: r.tx.take_stats(),
                    streams: r
                        .streams
                        .iter_mut()
                        .map(|(id, d)| {
                            let name = streams.get(id).map_or_else(String::new, |s| s.name.clone());
                            let shown = std::mem::take(&mut d.shown);
                            let skipped = std::mem::take(&mut d.skipped);
                            (*id, name, shown, skipped)
                        })
                        .collect(),
                })
                .collect();
            (window, encoded, silent, lines)
        };
        // Logged without the lock, which every encoded frame takes.
        for (name, stats, behind) in encoded {
            if stats.frames > 0 {
                info!("{name}: {}", stats.summary());
            }
            let hz = stats.gate.map_or(0.0, |g| f64::from(g.rate_mhz) / 1e3);
            match behind {
                Some(true) => warn!(
                    "{name}: the encoder takes {:.1} ms a frame, longer than a frame of the \
                     monitor at {hz:.0} Hz lasts: it cannot keep up with the monitor's full \
                     rate",
                    f64::from(stats.encode_us_avg) / 1e3
                ),
                Some(false) => info!("{name}: the encoder keeps up with {hz:.0} Hz again"),
                None => {}
            }
        }
        let several = lines.len() > 1;
        for Lines {
            session,
            mut stats,
            streams,
        } in lines
        {
            let whose = if several {
                format!(" (session {session})")
            } else {
                String::new()
            };
            for (id, name, shown, skipped) in streams {
                let video = stats.video.remove(&id.wire()).unwrap_or_default();
                if video.frames == 0 && video.dropped.total() == 0 && skipped == 0 {
                    continue;
                }
                let shown = match shown.count() {
                    0 => "no reports".to_owned(),
                    n => format!("{} ({n} reports)", shown.summary_ms()),
                };
                info!(
                    "{name}: {}; {skipped} skipped waiting for a keyframe; encoded → shown in headset {shown}{whose}",
                    video.summary(window),
                );
            }
            if stats.audio_wait.count() > 0 || stats.audio_dropped > 0 || silent > 0 {
                info!(
                    "sound: sent {} slices, waited in outbox {}; dropped {}; {silent} of \
                     silence not sent{whose}",
                    stats.audio_wait.count(),
                    stats.audio_wait.summary_ms(),
                    stats.audio_dropped
                );
            }
            info!(
                "link: round trip {} ({} pings answered){whose}",
                stats.round_trip.summary_ms(),
                stats.round_trip.count()
            );
        }
    }

    /// Sends a control message to every headset with a session. Never blocks.
    pub fn send_all(&self, msg: &HostMsg) {
        let mut inner = self.inner.lock().unwrap();
        let mut over = Vec::new();
        for (session, r) in &inner.receivers {
            if !r.tx.control(msg.clone()) {
                over.push(*session);
            }
        }
        for session in over {
            inner.receivers.remove(&session);
        }
    }

    /// The size and codec stream `id` goes out in, once its encoder has said its size.
    pub fn stream_format(&self, id: ScreenId) -> Option<((u32, u32), Codec)> {
        let inner = self.inner.lock().unwrap();
        let slot = inner.streams.get(&id)?;
        Some((slot.size?, slot.codec))
    }

    /// What the encoder of stream `id` made in the last window of the report, once
    /// one ended.
    pub fn stream_stats(&self, id: ScreenId) -> Option<StreamStats> {
        self.inner.lock().unwrap().streams.get(&id)?.measured
    }

    /// The size stream `id` goes out at, once its encoder has said.
    pub fn stream_size(&self, id: ScreenId) -> Option<(u32, u32)> {
        self.inner.lock().unwrap().streams.get(&id)?.size
    }

    /// Whether `generation` is what stream `id` sends now to the headset of `session`
    /// (a request about an older one is for frames long gone).
    pub fn is_current(&self, session: SessionId, id: ScreenId, generation: Gen) -> bool {
        self.inner
            .lock()
            .unwrap()
            .receivers
            .get(&session)
            .and_then(|r| r.streams.get(&id))
            .is_some_and(|d| d.announced && d.generation == generation)
    }

    /// Stream `id` (named `name`) starts, or starts over with a new encoder making
    /// `codec`: every headset gets it announced anew, and one that had it is told when
    /// it ends.
    pub fn add_stream(&self, id: ScreenId, codec: Codec, name: String) {
        // What an encoder before this one said is no news of the new one.
        self.unopened.lock().unwrap().retain(|(i, _)| *i != id);
        self.broken.lock().unwrap().retain(|(i, _)| *i != id);
        let mut inner = self.inner.lock().unwrap();
        let slot = StreamSlot {
            name,
            key: None,
            keeps_pointer: false,
            size: None,
            codec,
            keyframe_asked: None,
            since: Instant::now(),
            last_frames: [None; 2],
            keyframer: None,
            headset_asked: None,
            headset_pending: false,
            key_pending: false,
            outs: Vec::new(),
            tally: Tally::default(),
            measured: None,
            changed: None,
            unsettled: false,
            kbps: 0,
            sharpens: false,
            coarse: false,
            gate: None,
            behind: false,
        };
        inner.streams.insert(id, slot);
        for r in inner.receivers.values_mut() {
            let (known, generation) = r
                .streams
                .get(&id)
                .map_or((false, Gen(0)), |d| (d.known, d.generation));
            let delivery = Delivery {
                known,
                generation,
                ..Delivery::default()
            };
            r.streams.insert(id, delivery);
        }
    }

    /// The encoder of stream `id` runs: keyframes are asked of it through `keyframer`
    /// (see [`Router::force_keyframe`]), and its frames go through its gate at the
    /// stream's level.
    pub fn set_keyframer(&self, id: ScreenId, keyframer: crate::encoder::Keyframer) {
        let gate = keyframer.gate().clone();
        if let Some(slot) = self.inner.lock().unwrap().streams.get_mut(&id) {
            slot.keyframer = Some(keyframer);
        }
        self.set_gate(id, gate);
    }

    /// The frames of stream `id` go through `gate`, set to the stream's level now.
    pub fn set_gate(&self, id: ScreenId, gate: Arc<Gate>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(slot) = inner.streams.get_mut(&id) {
            slot.gate = Some(gate);
            slot.behind = false;
            inner.set_levels(Instant::now());
        }
    }

    /// The headset of `session` looks at display `at` (none: at none of this PC's):
    /// a stream looked at gets every frame at once, one the gaze left keeps them for
    /// 2 s (see `priority.rs`).
    pub fn gaze(&self, session: SessionId, at: Option<ScreenId>) {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        inner.priority.gaze(session, at, now);
        inner.set_levels(now);
    }

    /// "Focus" is on in the headset of `session`, or not: where every headset on the
    /// head has it, the streams nobody looks at get 5 frames a second.
    pub fn set_focus_mode(&self, session: SessionId, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.priority.set_focus(session, on);
        inner.set_levels(Instant::now());
    }

    /// The PC's active window is on display `at` (none: on none of them): that one
    /// gets every frame.
    pub fn set_active(&self, at: Option<ScreenId>) {
        let mut inner = self.inner.lock().unwrap();
        inner.priority.set_active(at);
        inner.set_levels(Instant::now());
    }

    /// Sets every stream's gate to its level now: a stream the gaze left 2 s ago
    /// goes down (see [`Router::priority_due`]).
    pub fn priorities(&self) {
        self.inner.lock().unwrap().set_levels(Instant::now());
    }

    /// When a stream's level next goes down by itself: the control loop wakes then
    /// to call [`Router::priorities`].
    pub fn priority_due(&self) -> Option<Instant> {
        self.inner.lock().unwrap().priority.due(Instant::now())
    }

    /// Asks the encoder of stream `id` for a keyframe now, if it runs: from any thread,
    /// without waiting for the desktop's worker that owns the encoder.
    pub fn force_keyframe(&self, id: ScreenId) {
        let keyframer = self
            .inner
            .lock()
            .unwrap()
            .streams
            .get(&id)
            .and_then(|s| s.keyframer.clone());
        if let Some(k) = keyframer {
            k.ask();
        }
    }

    /// Where to say that a keyframe was asked of a still screen.
    pub fn send_events_to(&self, events: crate::events::EventSink) {
        *self.events.lock().unwrap() = Some(events);
    }

    /// A keyframe was asked of stream `id`, whose screen is still: on a still screen
    /// the source sends nothing until its keepalive (750 ms on KDE, 2 s on Hyprland),
    /// so it is asked for a picture now. A capture pushing its own frames (KWin's
    /// stream through the feed, Hyprland's capture) sends its last one again
    /// (`keyframer`); one that sends its picture only when it starts is started over
    /// by the control loop.
    fn picture_now(&self, id: ScreenId, keyframer: Option<&crate::encoder::Keyframer>) {
        if let Some(k) = keyframer {
            k.picture_again();
        }
        if let Some(events) = &*self.events.lock().unwrap() {
            events.send(crate::events::Event::StillPicture(id));
        }
    }

    /// A headset asks for a keyframe of stream `id` (from its reader, without the
    /// control loop): at most every 300 ms for each stream, whoever asks, or keyframes
    /// would follow one another. A request within the 300 ms is never lost (no
    /// keyframe comes on its own any more): it waits, together with any others, and
    /// one keyframe is asked for once the time is up, with the stream's next frame or
    /// the next tick (see [`Router::pending_keyframes`]), whichever comes first.
    pub fn keyframe_asked(&self, id: ScreenId) {
        let now = Instant::now();
        let keyframer = {
            let mut inner = self.inner.lock().unwrap();
            let Some(slot) = inner.streams.get_mut(&id) else {
                return;
            };
            if slot
                .headset_asked
                .is_some_and(|t| now.duration_since(t) < HEADSET_KEYFRAMES)
            {
                debug!(
                    "{}: a keyframe asked for: within {} ms of the one before, it waits",
                    slot.name,
                    HEADSET_KEYFRAMES.as_millis()
                );
                slot.headset_pending = true;
                return;
            }
            match slot.headset_asked {
                Some(t) => debug!(
                    "{}: a keyframe asked for: at once, {} ms after the one before",
                    slot.name,
                    now.duration_since(t).as_millis()
                ),
                None => debug!("{}: a keyframe asked for: at once, the first", slot.name),
            }
            slot.headset_asked = Some(now);
            slot.headset_pending = false;
            (slot.keyframer.clone(), !slot.flowing(now))
        };
        if let Some(k) = &keyframer.0 {
            k.ask();
        }
        if keyframer.1 {
            self.picture_now(id, keyframer.0.as_ref());
        }
    }

    /// Asks for the keyframes headsets asked for within 300 ms of the one before,
    /// once that time is up (see [`Router::keyframe_asked`]), and those the router
    /// could not ask for yet when a headset began to wait for one (see
    /// [`Router::fan_out`]); how many.
    pub fn pending_keyframes(&self) -> usize {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let RouterInner {
            streams, receivers, ..
        } = &mut *inner;
        let keyframers: Vec<_> = streams
            .iter_mut()
            .filter_map(|(id, s)| {
                let headset = s.pending_due(now);
                let router = Self::router_pending_due(s, receivers, *id, now);
                (headset || router).then(|| (*id, s.keyframer.clone(), !s.flowing(now)))
            })
            .collect();
        drop(inner);
        let n = keyframers.len();
        for (id, k, still) in keyframers {
            if let Some(k) = &k {
                k.ask();
            }
            if still {
                self.picture_now(id, k.as_ref());
            }
        }
        n
    }

    /// Asks for one keyframe of each stream whose screen went still `after` ago and
    /// that sent new pictures since its last keyframe: nobody sees it (nothing moves),
    /// and it clears whatever errors the pictures since then may have left in a
    /// decoder. Keyframes come no other way but this and on request (see
    /// `encoder::X264_GOP`). It comes with the next frame the source sends (its
    /// keepalive on a still screen).
    ///
    /// After a big frame (see [`big_frame`]) the picture on the screen is coarse, and
    /// would stay so until then: the keyframe is asked for `early` instead, which
    /// sharpens it (once a pause: then it is settled), and the source is asked for its
    /// picture at once (a capture that pushes its own frames, KWin's stream through
    /// the feed among them). Small frames (typing, scrolling)
    /// wait `after` as before.
    /// Returns how many it asked for.
    pub fn still_keyframes(&self, after: Duration, early: Duration) -> usize {
        let now = Instant::now();
        let keyframers: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .streams
            .values_mut()
            .filter(|s| s.still_due(after, early).is_some_and(|t| now >= t))
            .map(|s| {
                s.unsettled = false;
                let sharpen = std::mem::take(&mut s.coarse);
                if sharpen {
                    s.tally.sharpened += 1;
                    debug!(
                        "{}: still after a big change, a keyframe to sharpen it",
                        s.name
                    );
                } else {
                    debug!("{}: still, a keyframe", s.name);
                }
                (s.keyframer.clone(), sharpen)
            })
            .collect();
        let n = keyframers.len();
        for (k, sharpen) in keyframers {
            if let Some(k) = k {
                k.ask();
                if sharpen {
                    k.picture_again();
                }
            }
        }
        n
    }

    /// When [`Router::still_keyframes`] next has a keyframe to ask for, with the same
    /// `after` and `early`: the control loop wakes then, so a coarse picture is
    /// sharpened on time, not at its next housekeeping.
    pub fn still_due(&self, after: Duration, early: Duration) -> Option<Instant> {
        self.inner
            .lock()
            .unwrap()
            .streams
            .values()
            .filter_map(|s| s.still_due(after, early))
            .min()
    }

    /// The encoder of stream `id` encodes at `kbps` from now on, and sharpens a still
    /// picture with a keyframe or not (see `StreamSlot::sharpens`).
    pub fn set_tuning(&self, id: ScreenId, kbps: u32, sharpens: bool) {
        if let Some(slot) = self.inner.lock().unwrap().streams.get_mut(&id) {
            slot.kbps = kbps;
            slot.sharpens = sharpens;
            slot.coarse &= sharpens;
        }
    }

    /// Asks every running encoder for a keyframe.
    pub fn force_keyframes(&self) {
        let keyframers: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .streams
            .values()
            .filter_map(|s| s.keyframer.clone())
            .collect();
        for k in keyframers {
            k.ask();
        }
    }

    /// Streams whose encoder broke since the last call, with why.
    pub fn take_broken(&self) -> Vec<(ScreenId, Broke)> {
        std::mem::take(&mut *self.broken.lock().unwrap())
    }

    /// Streams whose encoder did not open since the last call, with why.
    pub fn take_unopened(&self) -> Vec<(ScreenId, crate::encoder::NotOpened)> {
        std::mem::take(&mut *self.unopened.lock().unwrap())
    }

    /// Stream `id` stops: every headset that had it hears so.
    pub fn remove_stream(&self, id: ScreenId) {
        let mut inner = self.inner.lock().unwrap();
        if inner.streams.remove(&id).is_none() {
            return;
        }
        let mut over = Vec::new();
        for (session, r) in &mut inner.receivers {
            let Some(d) = r.streams.remove(&id) else {
                continue;
            };
            let end = HostMsg::StreamEnd {
                id: id.wire(),
                generation: d.generation,
            };
            if d.known && !r.tx.control(end) {
                over.push(*session);
            }
        }
        for session in over {
            inner.receivers.remove(&session);
        }
        // The pointer may have been on it.
        inner.tell_cursor(None);
    }

    /// Forwards a slice of PC sound to every headset on the head; dropped for one
    /// whose network is backed up (a tiny gap in sound beats growing latency). After
    /// 200 ms of silence nothing goes until there is sound again.
    pub fn send_audio(&self, pcm: Vec<u8>) {
        let mut inner = self.inner.lock().unwrap();
        let primed = match inner.quiet.pass(&pcm) {
            Pass::Hold => return,
            Pass::Send => 0,
            Pass::AfterSilence(n) => n,
        };
        for r in inner.receivers.values().filter(|r| r.worn) {
            for _ in 0..primed {
                r.tx.audio(HostMsg::Audio {
                    pcm: vec![0; pcm.len()],
                });
            }
        }
        let mut worn = inner.receivers.values().filter(|r| r.worn).peekable();
        let mut pcm = Some(pcm);
        while let Some(r) = worn.next() {
            let pcm = if worn.peek().is_some() {
                pcm.clone()
            } else {
                pcm.take()
            };
            if let Some(pcm) = pcm {
                r.tx.audio(HostMsg::Audio { pcm });
            }
        }
    }

    /// The headset of `session` showed frame `seq` of `generation` of stream `id`.
    pub fn frame_shown(&self, session: SessionId, id: ScreenId, generation: Gen, seq: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(d) = inner
            .receivers
            .get_mut(&session)
            .and_then(|r| r.streams.get_mut(&id))
        else {
            return;
        };
        if d.generation != generation {
            return;
        }
        match d.sent.remove(&seq) {
            Some(sent) => d.shown.add(sent.elapsed()),
            None => debug!("shown frame {seq} of {id} is unknown"),
        }
        // The headset reports only some frames: older ones are done too.
        d.sent = d.sent.split_off(&seq);
    }

    /// What the encoder of stream `id` says. Returns `true` when it should produce a
    /// keyframe.
    pub fn on_event(&self, id: ScreenId, event: EncoderEvent) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let RouterInner {
            streams, receivers, ..
        } = &mut *inner;
        // Streams whose queued frames were too late to send: they need a keyframe.
        for r in receivers.values_mut() {
            for lost in r.tx.take_dropped() {
                if let Some(d) = r.streams.get_mut(&ScreenId::from_wire(lost)) {
                    d.need_keyframe = true;
                }
            }
        }
        let Some(slot) = streams.get_mut(&id) else {
            return false;
        };
        match event {
            EncoderEvent::Size {
                width,
                height,
                kbps,
                sharpens,
            } => {
                info!(
                    "{}: streaming {width}x{height} at {} Mbit/s",
                    slot.name,
                    kbps / 1000
                );
                slot.size = Some((width, height));
                slot.kbps = kbps;
                slot.sharpens = sharpens;
                let mut fit_one = false;
                for r in receivers.values_mut() {
                    let too_big = r.too_big(slot);
                    let d = r.streams.entry(id).or_default();
                    d.announced = false;
                    d.need_keyframe = true;
                    d.too_big = too_big;
                    fit_one |= !too_big;
                }
                let too_big = !receivers.is_empty() && !fit_one;
                if too_big {
                    warn!(
                        "{}: {width}x{height} in {:?} is larger than the headset decodes: not sent",
                        slot.name, slot.codec
                    );
                    self.too_big
                        .lock()
                        .unwrap()
                        .push((id, (width, height), slot.codec));
                } else if receivers.values().any(|r| r.too_big(slot)) {
                    info!(
                        "{}: {width}x{height} in {:?} is larger than one headset decodes: not sent to it",
                        slot.name, slot.codec
                    );
                }
                // The pointer on it can be told across its picture now.
                inner.tell_cursor(None);
                !too_big
            }
            // Not the end of anything either: the desktop's worker starts this stream
            // again by itself, after a pause (see `Worker::check_closed`), and the
            // others go on.
            EncoderEvent::Failed(why) => {
                error!("{}: {why}", slot.name);
                let broke = Broke {
                    why,
                    by_source: false,
                };
                self.broken.lock().unwrap().push((id, broke));
                false
            }
            EncoderEvent::SourceLost(why) => {
                warn!("{}: its source is gone: {why}", slot.name);
                let broke = Broke {
                    why,
                    by_source: true,
                };
                self.broken.lock().unwrap().push((id, broke));
                false
            }
            // Not the end of anything: `Displays` starts the next encoder for it.
            EncoderEvent::Unopened(reason) => {
                self.unopened.lock().unwrap().push((id, reason));
                false
            }
            EncoderEvent::Frame {
                pts_us,
                keyframe,
                data,
                timing,
            } => {
                slot.tally.add(keyframe, data.len(), timing);
                if !timing.is_some_and(|t| t.repeat) {
                    slot.changed = Some(Instant::now());
                    slot.unsettled = !keyframe;
                } else if keyframe {
                    slot.unsettled = false;
                }
                // A keyframe is sharp; a big frame after it is not.
                slot.coarse =
                    !keyframe && slot.sharpens && (slot.coarse || big_frame(data.len(), slot.kbps));
                if keyframe {
                    let mut wanted = self.snapshot.lock().unwrap();
                    if wanted.as_ref().is_some_and(|(want, _)| *want == id)
                        && let Some((_, tx)) = wanted.take()
                    {
                        let _ = tx.try_send(data.clone());
                    }
                }
                // A keyframe a headset asked for meanwhile, due now: with this frame
                // (each request is answered, none dropped), and with the still
                // screen's one apart from it.
                // A keyframe going out now answers what waited.
                if keyframe {
                    slot.headset_pending = false;
                }
                let pending = slot.pending_due(Instant::now());
                let ask = Self::fan_out(slot, receivers, id, pts_us, keyframe, data) || pending;
                // The keyframe comes with the next frame: on a still screen, now.
                if ask && !slot.flowing(Instant::now()) {
                    self.picture_now(id, slot.keyframer.as_ref());
                }
                ask
            }
        }
    }

    /// A frame of stream `id` to every headset that takes it now; whether to ask the
    /// encoder for a keyframe.
    fn fan_out(
        slot: &mut StreamSlot,
        receivers: &mut BTreeMap<SessionId, Receiver>,
        id: ScreenId,
        pts_us: u64,
        keyframe: bool,
        data: Vec<u8>,
    ) -> bool {
        slot.last_frames = [Some(Instant::now()), slot.last_frames[0]];
        // Asked at once (a stream without its size yet), or at most every 300 ms for
        // the headsets that wait for one (`want_key`).
        let mut key_now = false;
        let mut want_key: Vec<SessionId> = Vec::new();
        let mut outs = std::mem::take(&mut slot.outs);
        // Headsets the stream goes to now.
        let mut takers = 0;
        for (session, r) in receivers.iter_mut() {
            let d = r.streams.entry(id).or_default();
            // Larger than this headset decodes: nothing of it goes there.
            if d.too_big {
                continue;
            }
            // Taken off, or its display not in the list it has yet: nothing of it
            // goes, and it gets a fresh picture once it is worn and listed.
            if !r.worn || !r.published.contains(&id) {
                d.need_keyframe = true;
                d.held = true;
                d.skipped += u32::from(r.worn);
                continue;
            }
            takers += 1;
            let start = if d.announced {
                None
            } else {
                let Some((w, h)) = slot.size else {
                    key_now = true;
                    continue;
                };
                d.generation = next_gen();
                d.seq = 0;
                d.sent.clear();
                Some(HostMsg::StreamStart {
                    id: id.wire(),
                    generation: d.generation,
                    w,
                    h,
                    codec: slot.codec,
                })
            };
            // Waiting for a keyframe: this frame cannot start its pictures.
            let frame = keyframe || !d.need_keyframe;
            if !frame {
                d.skipped += 1;
                want_key.push(*session);
            }
            if start.is_some() || frame {
                outs.push(Out {
                    session: *session,
                    start,
                    frame,
                });
            }
        }
        let mut data = Some(data);
        let mut frames = outs.iter().filter(|o| o.frame).count();
        let mut over = Vec::new();
        for out in outs.drain(..) {
            let Some(r) = receivers.get_mut(&out.session) else {
                continue;
            };
            let d = r.streams.entry(id).or_default();
            if let Some(start) = out.start {
                if !r.tx.control(start) {
                    over.push(out.session);
                    frames -= usize::from(out.frame);
                    continue;
                }
                let (w, h) = slot.size.unwrap_or_default();
                info!(
                    "{}: stream {w}x{h} starts in the headset ({:?}), {} ms after its encoder",
                    slot.name,
                    d.generation,
                    slot.since.elapsed().as_millis()
                );
                d.announced = true;
                d.known = true;
            }
            if !out.frame {
                continue;
            }
            // The last one takes the frame as it is, the others a copy.
            frames -= 1;
            let bytes = if frames == 0 {
                data.take()
            } else {
                data.clone()
            };
            let seq = d.seq;
            let msg = HostMsg::Frame {
                id: id.wire(),
                generation: d.generation,
                seq,
                pts_us,
                keyframe,
                data: bytes.unwrap_or_default(),
            };
            if r.tx.video(id.wire(), keyframe, msg) {
                d.need_keyframe = false;
                if keyframe {
                    d.fresh_since = None;
                }
                d.seq += 1;
                if d.sent.len() > 256 {
                    d.sent.clear();
                }
                d.sent.insert(seq, Instant::now());
            } else {
                // Too much waiting already: drop until the next keyframe so the
                // decoder never sees a broken reference chain.
                d.need_keyframe = true;
                want_key.push(out.session);
            }
        }
        // Empty again, its room kept for the next frame.
        slot.outs = outs;
        for session in over {
            receivers.remove(&session);
        }
        if keyframe && want_key.is_empty() {
            slot.keyframe_asked = None;
        }
        // Not asked now, it waits: on a still screen no frame comes to ask again.
        let ask =
            !want_key.is_empty() && Self::ask_keyframe(slot, receivers, id, &want_key, takers > 1);
        slot.key_pending = !want_key.is_empty() && !ask;
        key_now || ask
    }

    /// Whether the keyframe the router could not ask for of stream `id` (see
    /// `key_pending`) is to be asked for now, as [`Router::fan_out`] would with the
    /// next frame: for the headsets that still wait for one, once the time since the
    /// last one asked is up, and each as often as it may ask.
    fn router_pending_due(
        slot: &mut StreamSlot,
        receivers: &mut BTreeMap<SessionId, Receiver>,
        id: ScreenId,
        now: Instant,
    ) -> bool {
        if !slot.key_pending
            || slot
                .keyframe_asked
                .is_some_and(|t| now.duration_since(t) < KEYFRAME_EVERY)
        {
            return false;
        }
        let takes = |r: &Receiver| r.worn && r.published.contains(&id);
        let shared = receivers.values().filter(|r| takes(r)).count() > 1;
        let (mut waiting, mut ask) = (false, false);
        for r in receivers.values_mut().filter(|r| takes(r)) {
            let queued = shared && r.tx.keyframe_waiting(id.wire());
            if let Some(d) = r
                .streams
                .get_mut(&id)
                .filter(|d| !d.too_big && d.need_keyframe)
            {
                waiting = true;
                ask |= d.key_asks.due(now, shared, queued);
            }
        }
        // Nobody waits any more (a keyframe came, the headset went): nothing to ask.
        slot.key_pending = waiting && !ask;
        if ask {
            slot.keyframe_asked = Some(now);
            debug!("{}: a keyframe that waited is asked for now", slot.name);
        }
        ask
    }

    /// Whether the headsets of `sessions`, which wait for a keyframe of stream `id`,
    /// ask the encoder for one now: at most every 300 ms for the stream, and while
    /// other headsets get it too (`shared`), each at most as often as its
    /// [`KeyAsks`] lets it and not while its last keyframe still waits to go to it.
    /// So one headset that keeps losing frames does not make keyframes for all.
    fn ask_keyframe(
        slot: &mut StreamSlot,
        receivers: &mut BTreeMap<SessionId, Receiver>,
        id: ScreenId,
        sessions: &[SessionId],
        shared: bool,
    ) -> bool {
        let now = Instant::now();
        if slot
            .keyframe_asked
            .is_some_and(|t| now.duration_since(t) < KEYFRAME_EVERY)
        {
            return false;
        }
        let mut ask = false;
        for session in sessions {
            let Some(r) = receivers.get_mut(session) else {
                continue;
            };
            let waiting = shared && r.tx.keyframe_waiting(id.wire());
            if let Some(d) = r.streams.get_mut(&id) {
                ask |= d.key_asks.due(now, shared, waiting);
            }
        }
        if ask {
            slot.keyframe_asked = Some(now);
        }
        ask
    }
}

#[cfg(test)]
mod tests;
