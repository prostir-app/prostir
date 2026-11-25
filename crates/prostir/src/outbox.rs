//! What goes to the headset, packet by packet, most important first: control messages,
//! then where the mouse pointer is (only the newest), then sound, then video, the streams taking turns a packet each. A big keyframe goes
//! out in packets of [`FRAGMENT`] bytes, so sound and the answers of the menu wait at
//! most one packet behind it, not the whole frame.
//!
//! A video frame that waited too long is dropped instead of being sent late (and the
//! frames after it, up to the next keyframe, since the decoder cannot use them), so a
//! slow moment on Wi-Fi costs a few frames rather than seconds of added latency. A
//! keyframe makes the older frames of its stream useless, so it drops them.
//!
//! Who does what:
//! - The encoders, the sound capture and the session loop queue messages here
//!   ([`Outbox::control`], [`Outbox::cursor`], [`Outbox::audio`], [`Outbox::video`]),
//!   never waiting.
//! - The one writer thread takes the next packet to send with [`Outbox::next`]. A
//!   message it started moves out of the queues into its [`Lanes`], so its packets are
//!   cut and encrypted without holding the queues' lock. A message started is always
//!   finished: there is no cancelling half way.
//!
//! It also counts what it does (see [`OutboxStats`]): how long frames and sound wait,
//! what was sent, and what was dropped and why. The router logs that now and then.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::warn;
use prostir_proto::link::RecordSender;
use prostir_proto::packet::{Encoded, FRAGMENT, Outgoing, lane};
use prostir_proto::state::DisplayId;
use prostir_proto::stats::{Histogram, Pings, per_minute};
use prostir_proto::{HostMsg, PcGoodbye};

/// Sound queued at most: 8 slices of 10 ms. Older slices make room for newer ones.
const MAX_AUDIO: usize = 8;
/// Video frames older than this are not worth sending any more.
const MAX_AGE: Duration = Duration::from_millis(120);
/// Memory guard for queued video: beyond it only keyframes are taken.
const MAX_VIDEO_BYTES: usize = 16 << 20;
/// Control messages are never dropped; this much of them waiting means the headset
/// reads nothing any more, and the session ends.
const MAX_CONTROL_BYTES: usize = 4 << 20;
/// A ping goes to the headset this often: it shows the round trip, and keeps the
/// connection from falling silent (which ends a session after a few seconds).
pub const PING_EVERY: Duration = Duration::from_secs(1);

struct Frame {
    queued: Instant,
    keyframe: bool,
    msg: Encoded,
}

/// What waits in a stream's lane: frames, and the start and end of the stream, which
/// go in the same lane so they never overtake a frame of the old size that is being
/// sent already. A stream holds its lane from its start to its end (`last`).
enum Item {
    Announce { msg: Encoded, last: bool },
    Frame(Frame),
}

#[derive(Default)]
struct StreamQueue {
    items: VecDeque<Item>,
    /// It lost a frame: its next frames are useless until a keyframe.
    broken: bool,
}

/// Frames of one stream the outbox did not send, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drops {
    /// Waited longer than [`MAX_AGE`].
    pub late: u32,
    /// Came after a lost frame, before the next keyframe (the decoder cannot use them).
    pub after_loss: u32,
    /// Not queued: too much video waits already.
    pub full: u32,
    /// Still waiting when their stream started over or ended.
    pub restart: u32,
    /// Still waiting when a keyframe of their stream came, which replaces them.
    pub replaced: u32,
}

impl Drops {
    pub fn total(&self) -> u32 {
        self.late + self.after_loss + self.full + self.restart + self.replaced
    }
}

/// One stream's frames since the stats were last taken.
#[derive(Debug, Clone, Default)]
pub struct VideoStats {
    /// From queued to its first packet going out. The socket keeps little unsent data
    /// (`TCP_NOTSENT_LOWAT`), so a frame started is on the network soon after.
    pub wait: Histogram,
    /// Frames sent, keyframes among them, and their bytes.
    pub frames: u32,
    pub bytes: u64,
    pub keyframes: u32,
    pub keyframe_bytes: u64,
    pub keyframe_max: usize,
    pub dropped: Drops,
}

impl VideoStats {
    /// `sent 300 frames (30.0/s), 12.3 Mbit/s; waited p50 … ms; keyframes 2 (12/min),
    /// avg 180 KB, max 210 KB; dropped 0 late, …` for stats taken over `window`.
    pub fn summary(&self, window: Duration) -> String {
        let secs = window.as_secs_f64().max(1e-3);
        let kb = |b: f64| b / 1024.0;
        let d = &self.dropped;
        format!(
            "sent {} frames ({:.1}/s), {:.1} Mbit/s; waited in outbox {}; keyframes {} \
             ({:.0}/min), avg {:.0} KB, max {:.0} KB; dropped {} late, {} after a loss, \
             {} queue full, {} at restart, {} for a newer keyframe",
            self.frames,
            f64::from(self.frames) / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            self.wait.summary_ms(),
            self.keyframes,
            per_minute(self.keyframes, window),
            kb(self.keyframe_bytes as f64 / f64::from(self.keyframes.max(1))),
            kb(self.keyframe_max as f64),
            d.late,
            d.after_loss,
            d.full,
            d.restart,
            d.replaced,
        )
    }
}

/// What the outbox did since the stats were last taken.
#[derive(Debug, Clone, Default)]
pub struct OutboxStats {
    pub video: BTreeMap<DisplayId, VideoStats>,
    /// Sound: from queued to going out (it waits behind at most one packet of video).
    pub audio_wait: Histogram,
    /// Sound slices dropped to make room for newer ones.
    pub audio_dropped: u32,
    /// From a ping going out to its pong arriving.
    pub round_trip: Histogram,
}

#[derive(Default)]
struct State {
    control: VecDeque<Encoded>,
    control_bytes: usize,
    /// Where the pointer is, the newest only (`CursorAt`): sent once no control
    /// message waits, so never before a look queued ahead of it.
    cursor: Option<Encoded>,
    /// Sound slices with when each was queued.
    audio: VecDeque<(Instant, Encoded)>,
    streams: BTreeMap<DisplayId, StreamQueue>,
    /// The lane each stream sends in, from its first message to its end.
    stream_lanes: BTreeMap<DisplayId, u8>,
    video_bytes: usize,
    /// Streams that lost a frame since the router last looked: they need a keyframe.
    dropped: BTreeSet<DisplayId>,
    /// Raised with `dropped`, shared with the outbox.
    lost: Arc<AtomicBool>,
    /// Bytes queued since the writer last asked: what the streams need.
    queued: usize,
    /// The round trip of the latest pong, until the writer takes it.
    round_trip: Option<Duration>,
    stats: OutboxStats,
    pings: Pings,
    /// When the next ping is due (from the writer's first look on).
    next_ping: Option<Instant>,
    /// A goodbye is queued: control messages go out up to it, nothing else does.
    leaving: bool,
    /// Frames dropped while the lock was held, freed once it is not (a keyframe's
    /// memory goes back to the system, which takes a moment).
    garbage: Vec<Encoded>,
    closed: bool,
}

pub struct Outbox {
    state: Mutex<State>,
    ready: Condvar,
    /// Raised with `State::dropped` (the same flag): the router's every frame looks at
    /// this, not at the queues behind the lock.
    lost: Arc<AtomicBool>,
}

impl Default for Outbox {
    fn default() -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        Self {
            state: Mutex::new(State {
                lost: lost.clone(),
                ..State::default()
            }),
            ready: Condvar::new(),
            lost,
        }
    }
}

/// The lane the writer sends a packet in next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Control,
    Sound,
    Video(DisplayId),
}

/// The messages the writer has started: at most one per lane.
#[derive(Default)]
pub struct Lanes {
    control: Option<Outgoing>,
    sound: Option<Outgoing>,
    video: BTreeMap<DisplayId, Outgoing>,
    /// The stream that sent the last video packet: the next one goes to the stream
    /// after it.
    last_stream: Option<DisplayId>,
}

impl Lanes {
    /// Sends the next packet in `pick`'s lane.
    pub fn send(&mut self, pick: Pick, tx: &mut dyn RecordSender) -> io::Result<()> {
        match pick {
            Pick::Control => send_next(&mut self.control, tx),
            Pick::Sound => send_next(&mut self.sound, tx),
            Pick::Video(id) => {
                self.last_stream = Some(id);
                if let Some(o) = self.video.get_mut(&id)
                    && o.send_next(tx, FRAGMENT)?
                {
                    self.video.remove(&id);
                }
                Ok(())
            }
        }
    }
}

/// Sends the next packet of `out`; it is gone once its last packet went.
fn send_next(out: &mut Option<Outgoing>, tx: &mut dyn RecordSender) -> io::Result<()> {
    if let Some(o) = out
        && o.send_next(tx, FRAGMENT)?
    {
        *out = None;
    }
    Ok(())
}

/// Lets go of the queues, then frees what was dropped meanwhile.
fn unlock(mut s: MutexGuard<'_, State>) {
    let garbage = std::mem::take(&mut s.garbage);
    drop(s);
    drop(garbage);
}

/// The message's bytes, or why it cannot be sent (the PC's mistake: the session ends).
fn encode(msg: HostMsg) -> Option<Encoded> {
    msg.encode()
        .inspect_err(|e| warn!("a message could not be sent, ending the session: {e}"))
        .ok()
}

impl Outbox {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Queues a control message; `false` once the session is over. Starting or ending
    /// a stream goes in the stream's lane and drops its frames still waiting (they
    /// belong to the old size or codec).
    pub fn control(&self, msg: HostMsg) -> bool {
        let stream = match &msg {
            HostMsg::StreamStart { id, .. } => Some((*id, false)),
            HostMsg::StreamEnd { id, .. } => Some((*id, true)),
            _ => None,
        };
        let mut s = self.lock();
        if s.closed || s.leaving {
            return false;
        }
        let Some(msg) = encode(msg) else {
            self.close_locked(&mut s);
            return false;
        };
        s.queued += msg.len();
        match stream {
            Some((id, last)) => {
                let gone = s.drop_frames(id);
                if gone > 0 {
                    s.stats.video.entry(id).or_default().dropped.restart += gone;
                }
                let q = s.streams.entry(id).or_default();
                q.broken = false;
                q.items.push_back(Item::Announce { msg, last });
            }
            None => {
                s.control_bytes += msg.len();
                s.control.push_back(msg);
                if s.control_bytes > MAX_CONTROL_BYTES {
                    warn!(
                        "over {} MB of messages wait for the headset, which reads nothing: \
                         ending the session",
                        MAX_CONTROL_BYTES >> 20
                    );
                    self.close_locked(&mut s);
                    return false;
                }
            }
        }
        self.ready.notify_one();
        unlock(s);
        true
    }

    /// Queues where the mouse pointer is (`CursorAt`), in place of one still waiting:
    /// only the newest is worth sending. It goes once every control message queued
    /// before it went (a `CursorShape` it names among them), in one packet.
    pub fn cursor(&self, msg: HostMsg) {
        let mut s = self.lock();
        if s.closed || s.leaving {
            return;
        }
        let Some(msg) = encode(msg) else {
            self.close_locked(&mut s);
            return;
        };
        s.queued += msg.len();
        if let Some(old) = s.cursor.replace(msg) {
            s.garbage.push(old);
        }
        self.ready.notify_one();
        unlock(s);
    }

    /// Queues a slice of sound; the oldest goes when too many wait.
    pub fn audio(&self, msg: HostMsg) {
        let mut s = self.lock();
        if s.closed || s.leaving {
            return;
        }
        let Some(msg) = encode(msg) else {
            self.close_locked(&mut s);
            return;
        };
        if s.audio.len() >= MAX_AUDIO {
            s.audio.pop_front();
            s.stats.audio_dropped += 1;
        }
        s.queued += msg.len();
        s.audio.push_back((Instant::now(), msg));
        self.ready.notify_one();
    }

    /// Queues an encoded frame of stream `id`. `false`: not queued, and the stream
    /// must wait for a keyframe. A keyframe replaces the frames of its stream still
    /// waiting.
    pub fn video(&self, id: DisplayId, keyframe: bool, msg: HostMsg) -> bool {
        let mut s = self.lock();
        if s.closed || s.leaving {
            return false;
        }
        let Some(msg) = encode(msg) else {
            self.close_locked(&mut s);
            return false;
        };
        if keyframe {
            let gone = s.drop_frames(id);
            if gone > 0 {
                s.stats.video.entry(id).or_default().dropped.replaced += gone;
            }
        } else if s.streams.get(&id).is_some_and(|q| q.broken) {
            s.stats.video.entry(id).or_default().dropped.after_loss += 1;
            return false;
        } else if s.video_bytes + msg.len() > MAX_VIDEO_BYTES {
            s.stats.video.entry(id).or_default().dropped.full += 1;
            return false;
        }
        s.video_bytes += msg.len();
        s.queued += msg.len();
        let q = s.streams.entry(id).or_default();
        q.broken = false;
        q.items.push_back(Item::Frame(Frame {
            queued: Instant::now(),
            keyframe,
            msg,
        }));
        self.ready.notify_one();
        unlock(s);
        true
    }

    /// The headset answered ping `seq`.
    pub fn pong(&self, seq: u32) {
        let mut s = self.lock();
        if let Some(rtt) = s.pings.pong(seq, Instant::now()) {
            s.stats.round_trip.add(rtt);
            s.round_trip = Some(rtt);
        }
    }

    /// What happened since the last call.
    pub fn take_stats(&self) -> OutboxStats {
        std::mem::take(&mut self.lock().stats)
    }

    /// How many bytes of messages were queued since the last call (frames dropped
    /// later count too: they are what the streams need).
    pub fn take_queued(&self) -> usize {
        std::mem::take(&mut self.lock().queued)
    }

    /// The round trip of the latest pong, if one came since the last call.
    pub fn take_round_trip(&self) -> Option<Duration> {
        self.lock().round_trip.take()
    }

    /// Whether a keyframe of stream `id` still waits to be sent.
    pub fn keyframe_waiting(&self, id: DisplayId) -> bool {
        self.lock().streams.get(&id).is_some_and(|q| {
            q.items
                .iter()
                .any(|i| matches!(i, Item::Frame(f) if f.keyframe))
        })
    }

    /// Streams that lost frames since the last call (empty at once, without the
    /// lock, while none did).
    pub fn take_dropped(&self) -> BTreeSet<DisplayId> {
        if !self.lost.swap(false, Ordering::Relaxed) {
            return BTreeSet::new();
        }
        std::mem::take(&mut self.lock().dropped)
    }

    /// Ends the session on purpose: the control messages waiting go out, then a
    /// goodbye saying `reason`, and nothing else (sound and video are dropped).
    pub fn say_goodbye(&self, reason: PcGoodbye) {
        let mut s = self.lock();
        if s.closed || s.leaving {
            return;
        }
        s.audio.clear();
        s.cursor = None;
        s.streams.clear();
        s.stream_lanes.clear();
        s.video_bytes = 0;
        if let Some(goodbye) = encode(HostMsg::Goodbye {
            reason: reason.code(),
        }) {
            s.control_bytes += goodbye.len();
            s.control.push_back(goodbye);
        }
        s.leaving = true;
        self.ready.notify_all();
    }

    /// The writer sent the goodbye, or will once it sent what it started.
    pub fn said_goodbye(&self) -> bool {
        self.lock().leaving
    }

    /// The session is over: nothing more is taken or sent.
    pub fn close(&self) {
        let mut s = self.lock();
        self.close_locked(&mut s);
    }

    fn close_locked(&self, s: &mut State) {
        s.closed = true;
        s.control.clear();
        s.cursor = None;
        s.audio.clear();
        s.streams.clear();
        s.stream_lanes.clear();
        s.video_bytes = 0;
        self.ready.notify_all();
    }

    /// The lane to send the next packet in, waiting until there is one; `None` once
    /// closed. A message taken from the queues moves into `lanes`.
    pub fn next(&self, lanes: &mut Lanes) -> Option<Pick> {
        let mut s = self.lock();
        loop {
            if s.closed {
                return None;
            }
            if s.leaving {
                // Up to the goodbye, then done.
                return s.pick_control(lanes);
            }
            let now = Instant::now();
            let ping_at = *s.next_ping.get_or_insert(now + PING_EVERY);
            if now >= ping_at {
                s.next_ping = Some(now + PING_EVERY);
                let seq = s.pings.ping(now);
                if let Some(ping) = encode(HostMsg::Ping { seq }) {
                    s.control_bytes += ping.len();
                    s.control.push_back(ping);
                }
            }
            if let Some(pick) = s.pick(lanes) {
                unlock(s);
                return Some(pick);
            }
            let wait = ping_at.saturating_duration_since(now);
            s = self.ready.wait_timeout(s, wait).unwrap().0;
        }
    }

    /// As [`Outbox::next`], without waiting.
    #[cfg(test)]
    fn try_next(&self, lanes: &mut Lanes) -> Option<Pick> {
        let mut s = self.lock();
        match (s.closed, s.leaving) {
            (true, _) => None,
            (false, true) => s.pick_control(lanes),
            (false, false) => s.pick(lanes),
        }
    }

    /// Everything waiting, sent and read back as the headset would: for tests.
    #[cfg(test)]
    pub fn sent(&self) -> Vec<HostMsg> {
        let (mut lanes, mut capture) = (Lanes::default(), tests::Capture::default());
        while let Some(pick) = self.try_next(&mut lanes) {
            lanes.send(pick, &mut capture).unwrap();
        }
        capture.messages()
    }
}

impl State {
    /// Drops the frames of stream `id` still waiting (not one being sent); how many.
    fn drop_frames(&mut self, id: DisplayId) -> u32 {
        let Some(q) = self.streams.get_mut(&id) else {
            return 0;
        };
        let mut gone = 0;
        let mut i = 0;
        while i < q.items.len() {
            if !matches!(q.items[i], Item::Frame(_)) {
                i += 1;
            } else if let Some(Item::Frame(f)) = q.items.remove(i) {
                self.video_bytes -= f.msg.len();
                self.garbage.push(f.msg);
                gone += 1;
            }
        }
        gone
    }

    /// The next lane to send in, taking a message into `lanes` when its lane is free:
    /// control, then the pointer, then sound, then the streams in turn.
    fn pick(&mut self, lanes: &mut Lanes) -> Option<Pick> {
        if let Some(pick) = self.pick_control(lanes) {
            return Some(pick);
        }
        // The control lane is free and nothing waits for it: the pointer, whole.
        if let Some(msg) = self.cursor.take() {
            lanes.control = Some(Outgoing::new(lane::CONTROL, msg));
            return Some(Pick::Control);
        }
        if lanes.sound.is_none()
            && let Some((queued, msg)) = self.audio.pop_front()
        {
            self.stats.audio_wait.add(queued.elapsed());
            lanes.sound = Some(Outgoing::new(lane::SOUND, msg));
        }
        if lanes.sound.is_some() {
            return Some(Pick::Sound);
        }
        // The streams after the last one served first, then the rest.
        use std::ops::Bound::{Excluded, Included, Unbounded};
        let last = lanes.last_stream;
        let after = last.map_or(Unbounded, Excluded);
        let up_to_last = last.map(|last| (Unbounded, Included(last)));
        for (mut from, to) in std::iter::once((after, Unbounded)).chain(up_to_last) {
            while let Some(&id) = self.streams.range((from, to)).next().map(|(id, _)| id) {
                if lanes.video.contains_key(&id) || self.start_video(id, lanes) {
                    return Some(Pick::Video(id));
                }
                from = Excluded(id);
            }
        }
        None
    }

    /// The control lane, if it has something to send.
    fn pick_control(&mut self, lanes: &mut Lanes) -> Option<Pick> {
        if lanes.control.is_none()
            && let Some(msg) = self.control.pop_front()
        {
            self.control_bytes -= msg.len();
            lanes.control = Some(Outgoing::new(lane::CONTROL, msg));
        }
        lanes.control.is_some().then_some(Pick::Control)
    }

    /// The lane of stream `id`: the one it has, or a free one (taken by no other stream
    /// and by no message still going out); `None` while every lane is busy.
    fn stream_lane(&mut self, id: DisplayId, lanes: &Lanes) -> Option<u8> {
        if let Some(&l) = self.stream_lanes.get(&id) {
            return Some(l);
        }
        let free = lane::STREAMS.clone().find(|l| {
            !self.stream_lanes.values().any(|t| t == l)
                && !lanes.video.values().any(|o| o.lane() == *l)
        })?;
        self.stream_lanes.insert(id, free);
        Some(free)
    }

    /// Moves the next message of stream `id` worth sending into its lane; `false`
    /// if there is none.
    fn start_video(&mut self, id: DisplayId, lanes: &mut Lanes) -> bool {
        if self.streams.get(&id).is_none_or(|q| q.items.is_empty()) {
            return false;
        }
        let Some(lane) = self.stream_lane(id, lanes) else {
            return false;
        };
        let Some(q) = self.streams.get_mut(&id) else {
            return false;
        };
        while let Some(item) = q.items.pop_front() {
            let f = match item {
                Item::Announce { msg, last } => {
                    lanes.video.insert(id, Outgoing::new(lane, msg));
                    // Its end: the lane is free for another stream once that is out.
                    if last && q.items.is_empty() {
                        self.streams.remove(&id);
                        self.stream_lanes.remove(&id);
                    }
                    return true;
                }
                Item::Frame(f) => f,
            };
            self.video_bytes -= f.msg.len();
            let waited = f.queued.elapsed();
            let stats = self.stats.video.entry(id).or_default();
            if !f.keyframe && (q.broken || waited > MAX_AGE) {
                if q.broken {
                    stats.dropped.after_loss += 1;
                } else {
                    stats.dropped.late += 1;
                    q.broken = true;
                    self.dropped.insert(id);
                    self.lost.store(true, Ordering::Relaxed);
                }
                self.garbage.push(f.msg);
                continue;
            }
            q.broken = false;
            stats.wait.add(waited);
            stats.frames += 1;
            stats.bytes += f.msg.len() as u64;
            if f.keyframe {
                stats.keyframes += 1;
                stats.keyframe_bytes += f.msg.len() as u64;
                stats.keyframe_max = stats.keyframe_max.max(f.msg.len());
            }
            lanes.video.insert(id, Outgoing::new(lane, f.msg));
            return true;
        }
        false
    }
}

#[cfg(test)]
pub(crate) mod tests;
