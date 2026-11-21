//! A stream's frames on their way from the network to its decoder, and the rules for
//! skipping frames until a keyframe. The network thread and the decoders use it on
//! the headset; it builds and is tested on any system.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
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
    /// The decoder thread, once it started.
    decoder: OnceLock<Thread>,
}

struct Queued {
    frames: VecDeque<Frame>,
    /// The decoder took a frame with no more than [`MAX_WAITING`] left: from now on a
    /// frame waiting longer than [`MAX_AGE`] means it is behind.
    caught_up: bool,
    /// The network side is gone: no frame comes any more.
    closed: bool,
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
            }),
            decoder: OnceLock::new(),
        }
    }
}

impl FrameQueue {
    fn lock(&self) -> MutexGuard<'_, Queued> {
        self.queued.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(&self) {
        if let Some(t) = self.decoder.get() {
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
        {
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
        }
        self.wake();
        Pushed::Queued
    }

    /// No frame comes any more: the decoder ends once it took those waiting.
    pub fn close(&self) {
        self.lock().closed = true;
        self.wake();
    }
}

/// The decoder's end of a [`FrameQueue`].
pub struct Frames(Arc<FrameQueue>);

impl Frames {
    /// The thread to wake when a frame comes or the stream ends (the decoder's own).
    pub fn wake(&self, decoder: Thread) {
        let _ = self.0.decoder.set(decoder);
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
/// coming.
#[derive(Debug, Default)]
pub struct KeyframeWait {
    waiting: bool,
    asked: Option<Instant>,
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
        due
    }

    /// A frame came at `now`: whether it can be decoded, and whether to ask the PC for
    /// a keyframe now.
    pub fn passes(&mut self, keyframe: bool, now: Instant) -> Verdict {
        if keyframe {
            self.waiting = false;
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
