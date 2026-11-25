//! A stream's frames on their way from the network to its decoder, and the rules for
//! skipping frames until a keyframe. The network thread and the decoders use it on
//! the headset; it builds and is tested on any system.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
#[cfg(feature = "preview")]
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::Thread;
use std::time::{Duration, Instant};

/// One frame of a stream, as it came from the network.
pub struct Frame {
    /// When the frame arrived from the network.
    pub received: Instant,
    /// Its number in its generation.
    pub seq: u32,
    pub pts_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Once its decoder takes frames, the oldest one waiting older than this when another
/// comes means the decoder is behind: the network drops frames until the next
/// keyframe and asks the PC for one, so the picture is never much more than this
/// behind.
pub const MAX_AGE: Duration = Duration::from_millis(50);
/// The decoder counts as started and caught up (so [`MAX_AGE`] holds) once it took a
/// frame and no more than this many wait: the frames that pile up while it starts are
/// not a reason to drop.
pub const MAX_WAITING: usize = 3;
/// Room in each stream's queue, made once: past it frames are dropped whatever their
/// age.
pub const QUEUE_ROOM: usize = 16;

/// The queue between the network and one decoder. The network side holds it in an
/// `Arc` and pushes; the decoder takes through [`Frames`].
pub struct FrameQueue {
    queued: Mutex<Queued>,
    /// What came for it, for the numbers shown while developing (see [`Received`]).
    #[cfg(feature = "preview")]
    received: Arc<Received>,
}

/// What came of a stream so far: atomics the network side counts in and anyone may
/// read, never a lock in the way of the frames. The preview build's only.
#[cfg(feature = "preview")]
#[derive(Debug, Default)]
pub struct Received {
    /// The bytes of every frame that came.
    bytes: AtomicU64,
    /// The bytes of the last keyframe; 0 before one came.
    keyframe: AtomicU32,
}

#[cfg(feature = "preview")]
impl Received {
    /// The bytes of every frame so far, and of the last keyframe (`None` before one).
    pub fn get(&self) -> (u64, Option<u32>) {
        let key = self.keyframe.load(Ordering::Relaxed);
        (self.bytes.load(Ordering::Relaxed), (key > 0).then_some(key))
    }

    fn count(&self, frame: &Frame) {
        let len = frame.data.len();
        self.bytes.fetch_add(len as u64, Ordering::Relaxed);
        if frame.keyframe {
            let len = u32::try_from(len).unwrap_or(u32::MAX).max(1);
            self.keyframe.store(len, Ordering::Relaxed);
        }
    }
}

struct Queued {
    frames: VecDeque<Frame>,
    /// The decoder took a frame with no more than [`MAX_WAITING`] left: from now on a
    /// frame waiting longer than [`MAX_AGE`] means it is behind.
    caught_up: bool,
    /// The network side is gone: no frame comes any more.
    closed: bool,
    /// The decoder thread, once it started (a decoder made again for the stream puts
    /// its own).
    decoder: Option<Thread>,
}

/// What [`FrameQueue::push`] did with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    Queued,
    /// Not queued: the decoder is behind. Frames are skipped until the next keyframe.
    Behind,
}

/// What [`Frames::take`] found.
pub enum Taken {
    Frame(Frame),
    /// Nothing waits now.
    Nothing,
    /// Nothing waits, and nothing will come: the stream ended.
    Ended,
}

impl Default for FrameQueue {
    fn default() -> Self {
        Self {
            queued: Mutex::new(Queued {
                frames: VecDeque::with_capacity(QUEUE_ROOM),
                caught_up: false,
                closed: false,
                decoder: None,
            }),
            #[cfg(feature = "preview")]
            received: Arc::default(),
        }
    }
}

impl FrameQueue {
    fn lock(&self) -> MutexGuard<'_, Queued> {
        self.queued.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes `decoder`, the thread taken from the queue under its lock (a handle,
    /// nothing made).
    fn wake(decoder: Option<Thread>) {
        if let Some(t) = decoder {
            t.unpark();
        }
    }

    /// The decoder's end of this queue.
    pub fn frames(self: &Arc<Self>) -> Frames {
        Frames(self.clone())
    }

    /// Nobody takes from it any more (the decoder's end is gone).
    pub fn abandoned(self: &Arc<Self>) -> bool {
        Arc::strong_count(self) == 1
    }

    /// Queues `frame` (which came at `now`) and wakes the decoder, unless the decoder
    /// is behind. A keyframe always goes in, in place of the frames still waiting:
    /// they came before it and are of no use once it is decoded.
    pub fn push(&self, frame: Frame, now: Instant) -> Pushed {
        #[cfg(feature = "preview")]
        self.received.count(&frame);
        let decoder = {
            let mut q = self.lock();
            if frame.keyframe {
                q.frames.clear();
            } else {
                let late = q.caught_up
                    && q.frames
                        .front()
                        .is_some_and(|f| now.saturating_duration_since(f.received) > MAX_AGE);
                if late || q.frames.len() >= QUEUE_ROOM {
                    return Pushed::Behind;
                }
            }
            q.frames.push_back(frame);
            q.decoder.clone()
        };
        Self::wake(decoder);
        Pushed::Queued
    }

    /// No frame comes any more: the decoder ends once it took those waiting.
    pub fn close(&self) {
        let decoder = {
            let mut q = self.lock();
            q.closed = true;
            q.decoder.clone()
        };
        Self::wake(decoder);
    }
}

/// The decoder's end of a [`FrameQueue`].
pub struct Frames(Arc<FrameQueue>);

impl Frames {
    /// The thread to wake when a frame comes or the stream ends (the decoder's own;
    /// a decoder made again for the stream puts its own in place of the last one).
    pub fn wake(&self, decoder: Thread) {
        self.0.lock().decoder = Some(decoder);
    }

    /// The next frame, if one waits.
    pub fn take(&self) -> Taken {
        let mut q = self.0.lock();
        match q.frames.pop_front() {
            Some(frame) => {
                if q.frames.len() <= MAX_WAITING {
                    q.caught_up = true;
                }
                Taken::Frame(frame)
            }
            None if q.closed => Taken::Ended,
            None => Taken::Nothing,
        }
    }

    /// What came of the stream so far (see [`Received`]), to read from elsewhere.
    #[cfg(feature = "preview")]
    pub fn received(&self) -> Arc<Received> {
        self.0.received.clone()
    }

    /// Frames received from the network that the decoder has not taken yet.
    pub fn waiting(&self) -> usize {
        self.0.lock().frames.len()
    }

    /// Nothing waits, and nothing will come.
    pub fn ended(&self) -> bool {
        let q = self.0.lock();
        q.closed && q.frames.is_empty()
    }
}

/// While still waiting for a keyframe, how long after asking the PC for one to ask
/// again: the PC ignores asks for 300 ms after one it answered, so one ask can come
/// to nothing.
pub const ASK_AGAIN: Duration = Duration::from_millis(400);

/// Skipping frames until a keyframe, and when to ask the PC for one: once when frames
/// are lost, and again every [`ASK_AGAIN`] while frames other than keyframes keep
/// coming. An ask held back (within [`ASK_AGAIN`] of the last) goes once its time
/// comes, also when no frame follows: its owner looks at [`KeyframeWait::due_at`] and
/// asks on [`KeyframeWait::due`].
#[derive(Debug, Default)]
pub struct KeyframeWait {
    waiting: bool,
    asked: Option<Instant>,
    /// An ask was held back since the last one.
    held: bool,
}

/// What [`KeyframeWait::passes`] says of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// The frame can be decoded.
    pub pass: bool,
    /// Ask the PC for a keyframe now.
    pub ask: bool,
}

impl KeyframeWait {
    /// A keyframe is on its way (a stream starts with one): skip frames until it
    /// comes, and ask only if other frames come first.
    pub fn expect(&mut self) {
        self.waiting = true;
    }

    /// Frames were lost at `now`: skip frames until a keyframe. Whether to ask the PC
    /// for one now.
    pub fn lost(&mut self, now: Instant) -> bool {
        self.waiting = true;
        self.ask(now)
    }

    fn ask(&mut self, now: Instant) -> bool {
        let due = self
            .asked
            .is_none_or(|t| now.saturating_duration_since(t) >= ASK_AGAIN);
        if due {
            self.asked = Some(now);
        }
        self.held = !due;
        due
    }

    /// When the ask held back is due, while it is (for a timer).
    pub fn due_at(&self) -> Option<Instant> {
        if !(self.waiting && self.held) {
            return None;
        }
        self.asked.map(|t| t + ASK_AGAIN)
    }

    /// Whether to ask the PC for a keyframe now for the ask held back (once).
    pub fn due(&mut self, now: Instant) -> bool {
        let due = self.due_at().is_some_and(|at| now >= at);
        if due {
            self.asked = Some(now);
            self.held = false;
        }
        due
    }

    /// A frame came at `now`: whether it can be decoded, and whether to ask the PC for
    /// a keyframe now.
    pub fn passes(&mut self, keyframe: bool, now: Instant) -> Verdict {
        if keyframe {
            self.waiting = false;
            self.held = false;
            return Verdict {
                pass: true,
                ask: false,
            };
        }
        let ask = self.waiting && self.ask(now);
        Verdict {
            pass: !self.waiting,
            ask,
        }
    }

    pub fn waiting(&self) -> bool {
        self.waiting
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn frame(seq: u32, keyframe: bool, received: Instant) -> Frame {
        Frame {
            received,
            seq,
            pts_us: u64::from(seq) * 16_667,
            keyframe,
            data: vec![seq as u8],
        }
    }

    fn seqs(frames: &Frames) -> Vec<u32> {
        let mut out = Vec::new();
        while let Taken::Frame(f) = frames.take() {
            out.push(f.seq);
        }
        out
    }

    #[test]
    fn frames_go_in_order_and_the_end_is_told() {
        let t = Instant::now();
        let queue = Arc::new(FrameQueue::default());
        let frames = queue.frames();
        assert!(!queue.abandoned());
        for seq in 0..3 {
            assert_eq!(queue.push(frame(seq, seq == 0, t), t), Pushed::Queued);
        }
        assert_eq!(frames.waiting(), 3);
        assert_eq!(seqs(&frames), [0, 1, 2]);
        assert!(matches!(frames.take(), Taken::Nothing));
        assert!(!frames.ended());
        queue.push(frame(3, false, t), t);
        queue.close();
        assert!(!frames.ended(), "one still waits");
        assert!(matches!(frames.take(), Taken::Frame(_)));
        assert!(frames.ended());
        assert!(matches!(frames.take(), Taken::Ended));
        drop(frames);
        assert!(queue.abandoned());
    }

    /// A decoder made again for the same stream (the last one failed) is the one a
    /// frame wakes, not the thread that ended.
    #[test]
    fn a_decoder_made_again_is_woken() {
        let queue = Arc::new(FrameQueue::default());
        let frames = queue.frames();
        let gone = std::thread::spawn(std::thread::current).join().unwrap();
        frames.wake(gone);
        frames.wake(std::thread::current());
        let t = Instant::now();
        queue.push(frame(0, true, t), t);
        std::thread::park_timeout(Duration::from_secs(2));
        assert!(t.elapsed() < Duration::from_secs(1), "woken at once");
    }

    /// While the decoder starts, frames pile up without being dropped; once it caught
    /// up, a frame waiting over 50 ms when the next comes means it is behind. A burst
    /// of frames that came together is not.
    #[test]
    fn behind_is_by_age_once_the_decoder_caught_up() {
        let t = Instant::now();
        let queue = Arc::new(FrameQueue::default());
        let frames = queue.frames();
        // Its decoder is still starting: old frames wait, up to the room there is.
        assert_eq!(queue.push(frame(0, true, t), t), Pushed::Queued);
        let late = t + ms(200);
        for seq in 1..QUEUE_ROOM as u32 {
            assert_eq!(queue.push(frame(seq, false, t), late), Pushed::Queued);
        }
        assert_eq!(
            queue.push(frame(16, false, late), late),
            Pushed::Behind,
            "full"
        );
        // It takes them; caught up once three or fewer wait.
        for _ in 0..QUEUE_ROOM - MAX_WAITING {
            assert!(matches!(frames.take(), Taken::Frame(_)));
        }
        assert_eq!(frames.waiting(), MAX_WAITING);
        // The oldest waiting came at `t`: 200 ms ago.
        assert_eq!(queue.push(frame(17, false, late), late), Pushed::Behind);
        assert_eq!(seqs(&frames).len(), MAX_WAITING);
        // A burst: eight frames at once, none old.
        let burst = t + ms(300);
        for seq in 20..28 {
            assert_eq!(queue.push(frame(seq, false, burst), burst), Pushed::Queued);
        }
        // 40 ms on, still in time; 60 ms on, behind.
        assert_eq!(
            queue.push(frame(28, false, burst), burst + ms(40)),
            Pushed::Queued
        );
        assert_eq!(
            queue.push(frame(29, false, burst), burst + ms(60)),
            Pushed::Behind
        );
    }

    /// Every frame's bytes are counted, and the last keyframe's size is kept, also
    /// for a frame the decoder is too far behind to take.
    #[cfg(feature = "preview")]
    #[test]
    fn what_came_is_counted() {
        let t = Instant::now();
        let queue = Arc::new(FrameQueue::default());
        let received = queue.frames().received();
        assert_eq!(received.get(), (0, None));
        let sized = |seq, keyframe, len| Frame {
            data: vec![0; len],
            ..frame(seq, keyframe, t)
        };
        queue.push(sized(0, true, 900), t);
        queue.push(sized(1, false, 50), t);
        assert_eq!(received.get(), (950, Some(900)));
        queue.push(sized(2, true, 700), t);
        assert_eq!(received.get(), (1650, Some(700)));
    }

    /// A keyframe never waits behind frames it makes useless.
    #[test]
    fn a_keyframe_takes_the_place_of_the_frames_waiting() {
        let t = Instant::now();
        let queue = Arc::new(FrameQueue::default());
        let frames = queue.frames();
        for seq in 0..4 {
            queue.push(frame(seq, seq == 0, t), t);
        }
        assert!(matches!(frames.take(), Taken::Frame(_)));
        // Long after: the decoder is behind, yet the keyframe goes in, alone.
        let later = t + ms(500);
        assert_eq!(queue.push(frame(9, true, later), later), Pushed::Queued);
        assert_eq!(seqs(&frames), [9]);
    }

    /// The PC asked once ignores the ask (it answered another just before): while
    /// frames other than keyframes keep coming, the headset asks again, not only once.
    #[test]
    fn a_keyframe_is_asked_for_again_while_none_comes() {
        let t = Instant::now();
        let mut wait = KeyframeWait::default();
        assert!(wait.lost(t), "asked at once");
        let mut asks = Vec::new();
        for k in 1..=60 {
            let at = t + ms(16 * k);
            let v = wait.passes(false, at);
            assert!(!v.pass);
            if v.ask {
                asks.push(16 * k);
            }
        }
        assert_eq!(asks, [400, 800], "every 400 ms, not only once");
        assert!(wait.passes(true, t + ms(1000)).pass);
        assert!(!wait.waiting());
        let v = wait.passes(false, t + ms(1016));
        assert_eq!((v.pass, v.ask), (true, false));
        // Lost again soon after an ask: no second ask within 400 ms of it.
        assert!(!wait.lost(t + ms(1100)));
        assert!(wait.passes(false, t + ms(1200)).ask);
    }

    /// Lost again within 400 ms of an ask: the ask held back goes once its time comes,
    /// also when no frame follows (a still screen sends none); a keyframe ends it.
    #[test]
    fn an_ask_held_back_goes_when_its_time_comes() {
        let t = Instant::now();
        let mut wait = KeyframeWait::default();
        assert!(wait.lost(t));
        assert!(wait.passes(true, t + ms(50)).pass);
        assert!(!wait.lost(t + ms(100)), "too soon after the last");
        assert_eq!(wait.due_at(), Some(t + ASK_AGAIN));
        assert!(!wait.due(t + ms(399)));
        assert!(wait.due(t + ms(400)), "its time came");
        assert!(!wait.due(t + ms(401)), "once");
        assert_eq!(wait.due_at(), None);
        // Held back, then the keyframe came: nothing left to ask.
        assert!(!wait.lost(t + ms(500)));
        assert!(wait.passes(true, t + ms(600)).pass);
        assert_eq!(wait.due_at(), None);
        assert!(!wait.due(t + ms(2000)));
        // Nothing held back: nothing due, however long.
        let mut wait = KeyframeWait::default();
        wait.expect();
        assert_eq!(wait.due_at(), None);
        assert!(!wait.due(t + ms(5000)));
    }

    /// A stream starts with its keyframe: nothing is asked unless another frame
    /// comes first.
    #[test]
    fn a_stream_start_asks_only_without_its_keyframe() {
        let t = Instant::now();
        let mut wait = KeyframeWait::default();
        wait.expect();
        assert_eq!(
            wait.passes(true, t),
            Verdict {
                pass: true,
                ask: false
            }
        );
        let mut wait = KeyframeWait::default();
        wait.expect();
        assert_eq!(
            wait.passes(false, t),
            Verdict {
                pass: false,
                ask: true
            }
        );
    }
}
