//! A frame sent again shortly after a new picture, for an encoder that keeps a frame
//! until the next one comes (VA-API, see [`super::Backend::keeps_a_frame`]).
//!
//! On a still screen KWin sends nothing; PipeWire's source repeats the last frame
//! every 2 s. VA-API encodes a frame on the card and gives it out only when the next
//! one comes in (its picture is not ready when it looks, and it does not look again
//! until then), so the last picture of a change (a typed letter) waited up to 2 s.
//! Now, 30 ms after a new picture with nothing after it, the same buffer goes into the
//! encoder once more: it gives out the one it kept. While it still keeps the new one
//! (right after it opened, or when the picture was slow to encode), the same goes in
//! again 30 ms later, three times at most. Once per change: a repeat (the source's
//! keepalive, or one sent here) is not followed by another, and a new picture within
//! the 30 ms takes the place of the one before (frames that keep coming push each
//! other out). A keyframe asked for makes the next frame count as new (on a still
//! screen it is the keepalive).
//!
//! The buffer goes in at the sink pad of the queue after the source, from a thread of
//! its own: a pad's stream lock serializes it with the source's frames, and the queue
//! takes it without waiting (it drops old frames when full).

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

/// How long after a new picture with nothing after it it is sent again: past the gap
/// between frames of a moving screen (16 ms at 60 Hz), well under what a person sees
/// as late.
pub const AFTER: Duration = Duration::from_millis(30);

/// At most this many times for one picture.
const SENDS: u8 = 3;

thread_local! {
    /// This thread is sending a frame again (its own frame is no new picture).
    static SENDING: Cell<bool> = const { Cell::new(false) };
}

/// A new picture to send again.
struct Armed {
    buffer: gst::Buffer,
    came: Instant,
    due: Instant,
    sent: u8,
    /// Frames into the encoder when it was first sent again: the new picture is out
    /// once as many came out.
    entered: u64,
}

#[derive(Default)]
struct State {
    armed: Option<Armed>,
    /// Counts the frames that came, to see that none came meanwhile.
    seq: u64,
    /// The memory of the last frame (its address; 0: none): the same again is a repeat.
    last_memory: usize,
    /// The next frame counts as new whatever its memory (a keyframe was asked for).
    next_is_new: bool,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    /// Frames into the encoder and out of it, where counted (see [`Nudger::start`]).
    counted: bool,
    entered: AtomicU64,
    left: AtomicU64,
}

impl Shared {
    /// Whether the encoder still keeps a frame that entered before `entered` frames
    /// had (where counted).
    fn keeps(&self, entered: u64) -> bool {
        self.counted && self.left.load(Ordering::Relaxed) < entered
    }
}

/// Tells the frames sent again what goes on, from any thread.
#[derive(Clone)]
pub struct Handle(Arc<Shared>);

impl Handle {
    /// The next frame is sent again too, even a repeat: the keyframe asked for comes
    /// with it, and the encoder would keep it.
    pub fn next_is_new(&self) {
        self.0.state.lock().unwrap().next_is_new = true;
    }

    /// The source starts over: whatever comes first is a new picture.
    pub fn forget(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.armed = None;
        state.last_memory = 0;
    }

    /// A frame came out of the encoder.
    pub fn left(&self) {
        self.0.left.fetch_add(1, Ordering::Relaxed);
    }
}

/// Sends a new picture into the queue again after [`AFTER`]; stops when dropped.
pub struct Nudger {
    handle: Handle,
    thread: Option<JoinHandle<()>>,
}

impl Nudger {
    /// Watches the frames that come into `queue` (the sink pad of the queue after the
    /// source), sending each new picture again `after` it came. `encoder`: the pad
    /// frames enter the encoder by, where they are counted (with [`Handle::left`]) to
    /// see whether it still keeps the new picture; without it, each is sent once.
    /// `label` names its thread.
    pub fn start(
        queue: &gst::Pad,
        encoder: Option<&gst::Pad>,
        label: &str,
        after: Duration,
    ) -> std::io::Result<Self> {
        let handle = Handle(Arc::new(Shared {
            counted: encoder.is_some(),
            ..Shared::default()
        }));
        if let Some(pad) = encoder {
            let shared = handle.0.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                shared.entered.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
        }
        let shared = handle.0.clone();
        queue.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if SENDING.get() {
                return gst::PadProbeReturn::Ok;
            }
            let Some(buffer) = info.buffer() else {
                return gst::PadProbeReturn::Ok;
            };
            let memory = if buffer.n_memory() > 0 {
                buffer.peek_memory(0).as_ptr() as usize
            } else {
                0
            };
            let now = Instant::now();
            let mut state = shared.state.lock().unwrap();
            state.seq += 1;
            let new = memory == 0 || memory != state.last_memory || state.next_is_new;
            state.last_memory = memory;
            state.next_is_new = false;
            state.armed = new.then(|| Armed {
                buffer: buffer.to_owned(),
                came: now,
                due: now + after,
                sent: 0,
                entered: 0,
            });
            drop(state);
            shared.changed.notify_all();
            gst::PadProbeReturn::Ok
        });
        let shared = handle.0.clone();
        let queue = queue.clone();
        let thread = std::thread::Builder::new()
            .name(format!("again-{label}"))
            .spawn(move || run(&shared, &queue, after))?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
}

/// Asleep while nothing is armed; sends the armed picture again once its time came
/// with no frame after it.
fn run(shared: &Shared, queue: &gst::Pad, after: Duration) {
    let mut state = shared.state.lock().unwrap();
    loop {
        state = shared
            .changed
            .wait_while(state, |s| s.armed.is_none() && !s.stop)
            .unwrap();
        if state.stop {
            return;
        }
        let seq = state.seq;
        let Some(due) = state.armed.as_ref().map(|a| a.due) else {
            continue;
        };
        let left = due.saturating_duration_since(Instant::now());
        if !left.is_zero() {
            state = shared
                .changed
                .wait_timeout_while(state, left, |s| s.seq == seq && !s.stop)
                .unwrap()
                .0;
            // Stopped, another frame came (it takes the place of this one), or not
            // yet the time.
            if state.stop || state.seq != seq || Instant::now() < due {
                continue;
            }
        }
        let Some(armed) = state.armed.as_mut() else {
            continue;
        };
        // Sent before, and the encoder gave the new picture out.
        if armed.sent > 0 && !shared.keeps(armed.entered) {
            state.armed = None;
            continue;
        }
        let again = again(&armed.buffer, armed.came.elapsed());
        if armed.sent == 0 {
            armed.entered = shared.entered.load(Ordering::Relaxed);
        }
        armed.sent += 1;
        if armed.sent >= SENDS || !shared.counted {
            state.armed = None;
        } else {
            armed.due = Instant::now() + after;
        }
        drop(state);
        SENDING.set(true);
        // Flushing (stopping, starting over): nothing to do.
        let _ = queue.chain(again);
        SENDING.set(false);
        state = shared.state.lock().unwrap();
    }
}

/// `buffer` again, as if taken `later`: the same memory, a later time.
fn again(buffer: &gst::Buffer, later: Duration) -> gst::Buffer {
    let mut again = buffer.copy();
    let b = again.make_mut();
    if let Some(pts) = buffer.pts() {
        b.set_pts(pts + gst::ClockTime::from_nseconds(later.as_nanos() as u64));
    }
    b.set_dts(gst::ClockTime::NONE);
    b.set_duration(gst::ClockTime::NONE);
    b.unset_flags(gst::BufferFlags::DISCONT);
    again
}

impl Drop for Nudger {
    fn drop(&mut self) {
        {
            let mut state = self.handle.0.state.lock().unwrap();
            state.stop = true;
            state.armed = None;
        }
        self.handle.0.changed.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests;
