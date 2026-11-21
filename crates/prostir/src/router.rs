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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use prostir_proto::state::{self, Gen};
use prostir_proto::stats::{Histogram, Period};
use prostir_proto::{Codec, HostMsg};

use crate::encoder::{EncoderEvent, FrameTiming};
use crate::events::SessionId;
use crate::outbox::Outbox;
use crate::screen::ScreenId;

/// How often the router logs what each stream sent (see [`Router::report`]).
const REPORT_EVERY: Duration = Duration::from_secs(10);

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
    /// From a frame entering the converter to leaving the parser, microseconds.
    pub encode_us_avg: u32,
    pub encode_us_max: u32,
}

impl StreamStats {
    /// `encoded 600 frames (60.0/s, 0.5/s a still picture again), 24.1 Mbit/s,
    /// keyframes 1, max 750 KB; encoder avg 4.3 max 9.1 ms`.
    pub fn summary(&self) -> String {
        let secs = (f64::from(self.window_ms) / 1e3).max(1e-3);
        format!(
            "encoded {} frames ({:.1}/s, {:.1}/s a still picture again), {:.1} Mbit/s, \
             keyframes {}, max {:.0} KB; encoder avg {:.1} max {:.1} ms",
            self.frames,
            f64::from(self.frames) / secs,
            f64::from(self.repeats) / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            self.keyframes,
            f64::from(self.keyframe_max) / 1024.0,
            f64::from(self.encode_us_avg) / 1e3,
            f64::from(self.encode_us_max) / 1e3,
        )
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
            encode_us_avg: (t.encode_us / u64::from(t.timed.max(1))) as u32,
            encode_us_max: t.encode_us_max,
        }
    }
}

/// A stream as its encoder makes it, the same for every headset.
struct StreamSlot {
    /// The screen's name, for the log.
    name: String,
    /// A display of the PC: a broken stream is started again, not the end of all.
    physical: bool,
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
}

impl RouterInner {
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
    /// A stream that is no display of the PC broke: the monitors are made anew.
    pub failed: AtomicBool,
    /// Displays of the PC whose stream broke, to start again.
    broken: Mutex<Vec<ScreenId>>,
    /// Streams whose encoder did not open, with why: each goes on with the next
    /// encoder for it (see `Displays::tick`).
    unopened: Mutex<Vec<(ScreenId, crate::encoder::NotOpened)>>,
    /// Streams larger than every headset decodes, with their size and codec, for
    /// `Displays` to mark them failed.
    too_big: Mutex<Vec<TooBig>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RouterInner {
                streams: BTreeMap::new(),
                receivers: BTreeMap::new(),
                report: Period::new(REPORT_EVERY),
            }),
            failed: AtomicBool::new(false),
            broken: Mutex::new(Vec::new()),
            unopened: Mutex::new(Vec::new()),
            too_big: Mutex::new(Vec::new()),
            snapshot: Mutex::new(None),
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
        inner.receivers.insert(session, receiver);
        // What was found for the headsets before is looked at again with this one.
        *self.too_big.lock().unwrap() = inner.too_big_for_all();
    }

    /// The session ended: nothing more goes to its headset. A stream only it decoded
    /// is now larger than every headset left decodes, and found so.
    pub fn detach(&self, session: SessionId) {
        let mut inner = self.inner.lock().unwrap();
        inner.receivers.remove(&session);
        *self.too_big.lock().unwrap() = inner.too_big_for_all();
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
        if !worn {
            return Vec::new();
        }
        let now = Instant::now();
        r.streams
            .iter_mut()
            .filter(|(_, d)| !d.too_big)
            .filter_map(|(id, d)| {
                d.need_keyframe = true;
                d.fresh_since = Some(now);
                std::mem::take(&mut d.held).then_some(*id)
            })
            .collect()
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
        new.into_iter()
            .filter(|id| {
                r.streams.get_mut(id).is_some_and(|d| {
                    let held = std::mem::take(&mut d.held);
                    if held {
                        d.fresh_since = Some(now);
                    }
                    held
                })
            })
            .collect()
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
        let (window, encoded, lines) = {
            let mut inner = self.inner.lock().unwrap();
            let RouterInner {
                streams,
                receivers,
                report,
            } = &mut *inner;
            if receivers.is_empty() {
                return;
            }
            let now = Instant::now();
            let Some(window) = report.due(now) else {
                return;
            };
            // What each encoder made, whoever got it.
            let encoded: Vec<(String, StreamStats)> = streams
                .values_mut()
                .map(|s| {
                    let stats = s.tally.take(now);
                    s.measured = Some(stats);
                    (s.name.clone(), stats)
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
            (window, encoded, lines)
        };
        // Logged without the lock, which every encoded frame takes.
        for (name, stats) in encoded {
            if stats.frames > 0 {
                info!("{name}: {}", stats.summary());
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
            if stats.audio_wait.count() > 0 || stats.audio_dropped > 0 {
                info!(
                    "sound: sent {} slices, waited in outbox {}; dropped {}{whose}",
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

    /// Stream `id` (named `name`, a display of the PC with `physical`) starts, or
    /// starts over with a new encoder making `codec`: every headset gets it announced
    /// anew, and one that had it is told when it ends.
    pub fn add_stream(&self, id: ScreenId, codec: Codec, name: String, physical: bool) {
        // What an encoder before this one said is no news of the new one.
        self.unopened.lock().unwrap().retain(|(i, _)| *i != id);
        let mut inner = self.inner.lock().unwrap();
        let slot = StreamSlot {
            name,
            physical,
            size: None,
            codec,
            keyframe_asked: None,
            since: Instant::now(),
            last_frames: [None; 2],
            keyframer: None,
            headset_asked: None,
            headset_pending: false,
            outs: Vec::new(),
            tally: Tally::default(),
            measured: None,
            changed: None,
            unsettled: false,
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
    /// (see [`Router::force_keyframe`]).
    pub fn set_keyframer(&self, id: ScreenId, keyframer: crate::encoder::Keyframer) {
        if let Some(slot) = self.inner.lock().unwrap().streams.get_mut(&id) {
            slot.keyframer = Some(keyframer);
        }
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
                slot.headset_pending = true;
                return;
            }
            slot.headset_asked = Some(now);
            slot.headset_pending = false;
            slot.keyframer.clone()
        };
        if let Some(k) = keyframer {
            k.ask();
        }
    }

    /// Asks for the keyframes headsets asked for within 300 ms of the one before,
    /// once that time is up (see [`Router::keyframe_asked`]); how many.
    pub fn pending_keyframes(&self) -> usize {
        let now = Instant::now();
        let keyframers: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .streams
            .values_mut()
            .filter_map(|s| s.pending_due(now).then(|| s.keyframer.clone()))
            .collect();
        let n = keyframers.len();
        for k in keyframers.into_iter().flatten() {
            k.ask();
        }
        n
    }

    /// Asks for one keyframe of each stream whose screen went still `after` ago and
    /// that sent new pictures since its last keyframe: nobody sees it (nothing moves),
    /// and it clears whatever errors the pictures since then may have left in a
    /// decoder. Keyframes come no other way but this and on request (see
    /// `encoder::X264_GOP`). It comes with the next frame the source sends (its
    /// keepalive on a still screen).
    /// Returns how many it asked for.
    pub fn still_keyframes(&self, after: Duration) -> usize {
        let now = Instant::now();
        let keyframers: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .streams
            .values_mut()
            .filter(|s| s.unsettled && s.changed.is_some_and(|t| now.duration_since(t) >= after))
            .map(|s| {
                s.unsettled = false;
                debug!("{}: still, a keyframe", s.name);
                s.keyframer.clone()
            })
            .collect();
        let n = keyframers.len();
        for k in keyframers.into_iter().flatten() {
            k.ask();
        }
        n
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

    /// Displays of the PC whose stream broke since the last call.
    pub fn take_broken(&self) -> Vec<ScreenId> {
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
    }

    /// Forwards a slice of PC sound to every headset on the head; dropped for one
    /// whose network is backed up (a tiny gap in sound beats growing latency).
    pub fn send_audio(&self, pcm: Vec<u8>) {
        let inner = self.inner.lock().unwrap();
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
            EncoderEvent::Size { width, height } => {
                info!("{}: streaming {width}x{height}", slot.name);
                slot.size = Some((width, height));
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
                !too_big
            }
            EncoderEvent::Failed(reason) => {
                error!("{}: {reason}", slot.name);
                // A display of the PC is started again by `Displays::tick`; anything
                // else ends the sessions.
                if slot.physical {
                    self.broken.lock().unwrap().push(id);
                } else {
                    self.failed.store(true, Ordering::Relaxed);
                }
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
                Self::fan_out(slot, receivers, id, pts_us, keyframe, data) || pending
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
        key_now
            || (!want_key.is_empty()
                && Self::ask_keyframe(slot, receivers, id, &want_key, takers > 1))
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
