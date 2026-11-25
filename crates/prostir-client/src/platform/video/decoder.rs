//! Hardware decoding with MediaCodec straight into the compositor's surface.
//!
//! The codec runs in its asynchronous mode: from a thread of its own it says when an
//! input buffer is free and when a picture is decoded, and the network wakes the
//! decoder thread for each frame. A picture goes to the compositor the moment it is
//! decoded, a frame goes into the codec the moment both it and a buffer are there, and
//! a decoder with nothing to do sleeps instead of looking every few milliseconds.
//!
//! When it fails it follows the rules of `crate::video`: a passing failure makes the
//! codec again after a pause, asking the PC for a keyframe, and every third in a row
//! ends the thread asking for a new surface ([`End::NewSurface`]); a lasting one ends
//! it with the reason ([`End::Failed`]). What the shell watches of it is its
//! [`Status`]: since when it fails without a picture.

#![expect(
    clippy::disallowed_methods,
    reason = "the decoder threads are part of the shell"
)]

use std::collections::{HashMap, VecDeque};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;
use std::time::Instant;

use anyhow::{Context, Result};
use log::{error, info, warn};
use ndk::media::media_codec::{
    ActionCode, AsyncNotifyCallback, BufferInfo, MediaCodec, MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;
use ndk::media_error::MediaError;
use ndk::native_window::NativeWindow;
use prostir_proto::state::{DisplayId, Gen};
use prostir_proto::stats::{Histogram, Period};
use prostir_proto::{ClientMsg, Codec};

use crate::frames::{Frame, Frames, KeyframeWait, Taken};
use crate::net::Feedback;
use crate::video::{self, Fault, Why};

/// The stream a decoder decodes: its display and generation (see `StreamStart`).
#[derive(Debug, Clone, Copy)]
pub struct Stream {
    pub id: DisplayId,
    pub generation: Gen,
}

/// One stream for a decoder thread: what it decodes and where the pictures go.
pub struct DecoderJob {
    pub stream: Stream,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// The compositor's surface the pictures are decoded into.
    pub window: NativeWindow,
    pub frames: Frames,
    pub feedback: Feedback,
    /// Set when the monitor goes: the decoder stops before its surface does. Whoever
    /// sets it unparks the decoder thread too (see [`spawn`]).
    pub stop: Arc<AtomicBool>,
    /// What the shell watches of it.
    pub status: Arc<Status>,
    /// Passing failures in a row so far: kept when the thread is made again with a new
    /// surface, so the pauses go on growing.
    pub failures: u32,
}

/// What the shell watches of a decoder thread: since when it fails without a picture,
/// and a failure asked for on purpose (see `SurfaceVideo::inject`). Atomics only:
/// looked at every pass, nothing made.
pub struct Status {
    epoch: Instant,
    /// Nanoseconds from `epoch` to the first failure with no picture since, plus one;
    /// 0 while pictures come.
    failing: AtomicU64,
    /// A [`Fault`] to cause (see [`Status::fault_code`]); 0: none.
    fault: AtomicU8,
}

impl Status {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            failing: AtomicU64::new(0),
            fault: AtomicU8::new(0),
        }
    }

    /// Since when it fails without a picture (`None`: pictures come).
    pub fn failing_since(&self) -> Option<Instant> {
        match self.failing.load(Ordering::Relaxed) {
            0 => None,
            n => Some(self.epoch + Duration::from_nanos(n - 1)),
        }
    }

    /// It failed at `now`: counts from the first failure since the last picture.
    fn failed(&self, now: Instant) {
        let n = u64::try_from(now.saturating_duration_since(self.epoch).as_nanos())
            .unwrap_or(u64::MAX - 1)
            + 1;
        let _ = self
            .failing
            .compare_exchange(0, n, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// A picture came out.
    fn picture(&self) {
        self.failing.store(0, Ordering::Relaxed);
    }

    /// Asks the decoder thread to fail as `fault` says (its next look, once).
    pub fn cause(&self, fault: Fault) {
        self.fault.store(Self::fault_code(fault), Ordering::Relaxed);
    }

    fn fault_code(fault: Fault) -> u8 {
        match fault {
            Fault::CorruptFrame => 1,
            Fault::DecoderError => 2,
            Fault::SurfaceLost => 3,
        }
    }

    /// The fault asked for, once.
    fn take_fault(&self) -> Option<Fault> {
        match self.fault.swap(0, Ordering::Relaxed) {
            1 => Some(Fault::CorruptFrame),
            2 => Some(Fault::DecoderError),
            3 => Some(Fault::SurfaceLost),
            _ => None,
        }
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

/// A reader of the pictures of one stream that reads the PC's time code out of each
/// (`prostir_proto::timecode`) as it comes, for the selftest against a real PC; its
/// window, which the codec decodes into instead of the compositor's.
#[cfg(all(feature = "selftest", not(feature = "store")))]
fn time_code_reader(
    width: u32,
    height: u32,
) -> Result<(ndk::media::image_reader::ImageReader, NativeWindow)> {
    use ndk::media::image_reader::{AcquireResult, ImageFormat, ImageReader};
    let (w, h) = (i32::try_from(width)?, i32::try_from(height)?);
    let mut reader = ImageReader::new(w, h, ImageFormat::YUV_420_888, 3)
        .context("an image reader for the time code")?;
    reader
        .set_image_listener(Box::new(|reader: &ImageReader| {
            let at_us = monotonic_us();
            let Ok(AcquireResult::Image(image)) = reader.acquire_latest_image() else {
                return;
            };
            let (Ok(luma), Ok(stride)) = (image.plane_data(0), image.plane_row_stride(0)) else {
                return;
            };
            let (Ok(w), Ok(h)) = (image.width(), image.height()) else {
                return;
            };
            let size = |v: i32| usize::try_from(v).unwrap_or(0);
            let (stride, w, h) = (size(stride), size(w), size(h));
            // Upside down, the code's middle row is 8 rows from the bottom: the rows
            // from 16 above that read as its first ones.
            let from = h.saturating_sub(prostir_proto::timecode::CELL + 1) * stride;
            let luma = match luma.get(from..) {
                Some(rows) if crate::selftest::rows_up() => rows,
                _ => luma,
            };
            if let Some(pc_ms) = prostir_proto::timecode::read(luma, stride, w, h) {
                crate::selftest::time_code(pc_ms, at_us);
            }
        }))
        .context("the image reader's listener")?;
    let window = reader.window().context("the image reader's window")?;
    info!("decoder: reading the time code of a {width}x{height} stream");
    Ok((reader, window))
}

/// The headset's `CLOCK_MONOTONIC` in microseconds (as the selftest's clock offset to
/// the PC is measured).
#[cfg(all(feature = "selftest", not(feature = "store")))]
pub fn monotonic_us() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a timespec to fill, valid through the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut t) };
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000
}

/// Frames that wait this long without a free input buffer: the codec is stuck, and
/// one of them is dropped (and a keyframe asked for).
const STALLED: Duration = Duration::from_secs(1);
/// Frames wait and the codec has handed out no input buffer for this long, without
/// saying it failed: it is made again, as after an error.
const NO_INPUT: Duration = Duration::from_secs(3);
/// The longest a decoder sleeps with nothing to do. Whatever gives it work wakes it at
/// once (a frame, the codec, `stop`): this is only a safety net.
const IDLE: Duration = Duration::from_secs(1);
/// Passing codec errors are logged at most this often.
const TRANSIENT_LOG_EVERY: Duration = Duration::from_secs(10);
/// Room for the codec's buffer numbers, made once (more only if a codec has more).
const BUFFERS: usize = 32;

/// How a decoder thread ended: its job back (to make the decoder again with), and why.
pub struct Stopped {
    pub job: DecoderJob,
    pub end: End,
}

/// Why a decoder thread ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The stream ended, or `stop` was set.
    Ended,
    /// Its passing failures want a new surface (see `video::new_surface_due`): the
    /// thread is made again with it, with its job and failures so far.
    NewSurface,
    /// It cannot decode this stream.
    Failed(Why),
}

/// How a codec failed.
enum Fail {
    /// For a moment: made again after a pause.
    Passing(anyhow::Error),
    /// The surface counts as broken: made again with a new one.
    Surface(anyhow::Error),
    /// For this stream: not tried again.
    Lasting(Why, anyhow::Error),
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Self {
        Fail::Passing(e)
    }
}

impl From<MediaError> for Fail {
    fn from(e: MediaError) -> Self {
        Fail::Passing(e.into())
    }
}

/// Decodes the job's frames into its window until the stream ends or `stop` is set.
/// Whoever sets `stop` then unparks the returned thread (`handle.thread().unpark()`),
/// so it stops at once rather than after up to [`IDLE`].
pub fn spawn(job: DecoderJob) -> JoinHandle<Stopped> {
    let id = job.stream.id.0;
    std::thread::Builder::new()
        .name(format!("decoder-{id}"))
        .spawn(move || {
            let mut job = job;
            crate::platform::android::raise_thread_priority(
                crate::platform::android::Priority::Video,
            );
            let end = run(&mut job);
            match end {
                End::Ended => info!("decoder {id} stopped"),
                End::NewSurface => warn!("decoder {id}: made again with a new surface"),
                End::Failed(why) => error!("decoder {id}: cannot decode this stream ({why:?})"),
            }
            Stopped { job, end }
        })
        .expect("spawn decoder thread")
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the codec said from its own thread that the decoder thread has not acted on
/// yet. Its queues are made once, with room for [`BUFFERS`].
struct Told {
    /// Input buffers free to fill, by number.
    inputs: VecDeque<usize>,
    /// Decoded pictures: the output buffer's number and the frame's time stamp.
    outputs: VecDeque<(usize, u64)>,
    /// The codec failed and has to be made again, in words.
    failed: Option<String>,
}

impl Told {
    fn new() -> Self {
        Self {
            inputs: VecDeque::with_capacity(BUFFERS),
            outputs: VecDeque::with_capacity(BUFFERS),
            failed: None,
        }
    }
}

/// The decoder thread's side of its stream, kept across codecs made again.
struct Decoding<'a> {
    frames: &'a Frames,
    stop: &'a AtomicBool,
    status: &'a Status,
    /// The next frame is spoiled on purpose (see [`Fault::CorruptFrame`]).
    spoil: bool,
    gate: KeyframeGate,
    timing: Timing,
    /// Passing failures in a row (a picture starts it over).
    failures: u32,
    /// Input buffers free to fill, taken from [`Told`].
    free: VecDeque<usize>,
    /// Pictures to hand to the compositor, taken from [`Told`].
    ready: VecDeque<(usize, u64)>,
    /// Since when frames wait without a free input buffer (the last frame dropped
    /// for it starts it over).
    stalled: Option<Instant>,
    /// Since when frames wait and the codec handed out no input buffer at all.
    no_input: Option<Instant>,
}

fn run(job: &mut DecoderJob) -> End {
    let DecoderJob {
        stream,
        codec,
        width,
        height,
        ref window,
        ref frames,
        ref feedback,
        ref stop,
        ref status,
        failures,
    } = *job;
    // The selftest against a real PC reads the time code out of one stream's pictures
    // (the PC's time of their capture): they go to a reader instead of the compositor,
    // so that monitor shows nothing.
    #[cfg(all(feature = "selftest", not(feature = "store")))]
    let reader = match crate::selftest::reads_time_code(stream.id.0)
        .then(|| time_code_reader(width, height))
        .transpose()
    {
        Ok(reader) => reader,
        Err(e) => {
            warn!("decoder {}: {e:#}", stream.id.0);
            None
        }
    };
    #[cfg(all(feature = "selftest", not(feature = "store")))]
    let reader_window = reader.as_ref().map(|(_, w)| w.clone());
    #[cfg(all(feature = "selftest", not(feature = "store")))]
    let window = reader_window.as_ref().unwrap_or(window);
    // From here on the network wakes this thread for each frame.
    frames.wake(std::thread::current());
    let mut gate = KeyframeGate::new(stream, feedback.clone());
    // The PC starts every stream with a keyframe and the network passes its frames
    // from one on: only a codec made again after a failure asks for one (also on a
    // still screen, where no frame would come to ask with).
    gate.expect_keyframe();
    if failures > 0 {
        gate.wait();
    }
    let mut d = Decoding {
        frames,
        stop,
        status,
        spoil: false,
        gate,
        timing: Timing {
            stream: Some(stream),
            feedback: Some(feedback.clone()),
            started: Some(Instant::now()),
            ..Default::default()
        },
        failures,
        free: VecDeque::with_capacity(BUFFERS),
        ready: VecDeque::with_capacity(BUFFERS),
        stalled: None,
        no_input: None,
    };
    // A passing failure (a lost surface, a bad frame), or a codec that does not start
    // for a moment, makes the codec again instead of freezing the monitor, as often as
    // it takes; every third in a row, the surface too. A lasting one ends the thread.
    let end = loop {
        let told = Arc::new(Mutex::new(Told::new()));
        let result = match start_decoder(codec, width, height, window, &told, d.failures) {
            Ok(decoder) => {
                d.free.clear();
                d.ready.clear();
                d.stalled = None;
                d.no_input = None;
                let result = d.pump(&decoder, &told);
                let _ = decoder.stop();
                drop(decoder);
                result
            }
            Err(e) => Err(e),
        };
        let now = Instant::now();
        let (e, surface) = match result {
            Ok(()) => break End::Ended,
            Err(Fail::Lasting(why, e)) => {
                error!("decoder {}: {e:#}", stream.id.0);
                status.failed(now);
                break End::Failed(why);
            }
            Err(Fail::Passing(e)) => (e, false),
            Err(Fail::Surface(e)) => (e, true),
        };
        status.failed(now);
        d.failures = d.failures.saturating_add(1);
        if surface || video::new_surface_due(d.failures) {
            warn!(
                "decoder {}: {e:#} ({} in a row); a new surface",
                stream.id.0, d.failures
            );
            break End::NewSurface;
        }
        let pause = video::retry_pause(d.failures);
        warn!(
            "decoder {}: {e:#}; made again in {} ms ({} in a row)",
            stream.id.0,
            pause.as_millis(),
            d.failures
        );
        // A pause, cut short when the monitor goes.
        let until = now + pause;
        while !d.stop.load(Ordering::Relaxed)
            && let Some(left) = until.checked_duration_since(Instant::now())
        {
            std::thread::park_timeout(left);
        }
        if d.stop.load(Ordering::Relaxed) {
            break End::Ended;
        }
        d.gate.wait();
    };
    job.failures = d.failures;
    end
}

impl Decoding<'_> {
    /// Feeds the codec and hands its pictures on until the stream ends or `stop` is
    /// set (`Ok`), or the codec fails.
    fn pump(&mut self, decoder: &MediaCodec, told: &Mutex<Told>) -> Result<(), Fail> {
        while !self.stop.load(Ordering::Relaxed) {
            match self.status.take_fault() {
                Some(Fault::CorruptFrame) => self.spoil = true,
                Some(Fault::DecoderError) => {
                    return Err(Fail::Passing(anyhow::anyhow!("a codec error, on purpose")));
                }
                Some(Fault::SurfaceLost) => {
                    return Err(Fail::Surface(anyhow::anyhow!(
                        "the surface lost, on purpose"
                    )));
                }
                None => {}
            }
            {
                let mut t = lock(told);
                if let Some(why) = t.failed.take() {
                    return Err(Fail::Passing(anyhow::anyhow!(why)));
                }
                if !t.inputs.is_empty() {
                    self.no_input = None;
                }
                self.free.extend(t.inputs.drain(..));
                self.ready.extend(t.outputs.drain(..));
            }
            // Pictures first: they are what the user waits for.
            while let Some((index, pts_us)) = self.ready.pop_front() {
                decoder.release_output_buffer_by_index(index, true)?;
                self.timing.decoded(pts_us);
                // A picture: whatever failed before is over.
                self.status.picture();
                self.failures = 0;
            }
            while let Some(&index) = self.free.front() {
                let frame = match self.frames.take() {
                    Taken::Frame(frame) => frame,
                    Taken::Nothing => break,
                    Taken::Ended => return Ok(()),
                };
                self.timing.taken(self.frames.waiting());
                if !self.gate.passes(&frame) {
                    continue;
                }
                self.free.pop_front();
                // The codec took one: whatever waits gets its full second again.
                self.stalled = None;
                let spoil = std::mem::take(&mut self.spoil);
                if !feed(decoder, index, &frame, spoil, &mut self.timing)? {
                    self.gate.wait();
                }
            }
            if self.frames.ended() {
                return Ok(());
            }
            let now = Instant::now();
            let wait = if self.free.is_empty() && self.frames.waiting() > 0 {
                // Frames wait, and the codec frees no buffer for them.
                let silent = *self.no_input.get_or_insert(now);
                if now.saturating_duration_since(silent) >= NO_INPUT {
                    return Err(Fail::Passing(anyhow::anyhow!(
                        "no input buffer from the codec for {} s",
                        NO_INPUT.as_secs()
                    )));
                }
                let since = *self.stalled.get_or_insert(now);
                let stuck = now.saturating_duration_since(since);
                if stuck >= STALLED {
                    self.stalled = None;
                    if let Taken::Frame(frame) = self.frames.take() {
                        warn!(
                            "decoder stalled, dropping a frame (keyframe: {})",
                            frame.keyframe
                        );
                        self.gate.wait();
                    }
                    continue;
                }
                STALLED - stuck
            } else {
                self.stalled = None;
                if !self.free.is_empty() {
                    self.no_input = None;
                }
                IDLE
            };
            self.timing.report();
            // An ask for a keyframe held back goes once its time comes, also when no
            // frame follows: the sleep ends for it.
            let wait = self.gate.held(now, wait);
            // Until a frame, the codec or `stop` wakes it. A wake that came meanwhile
            // is kept, so the thread does not sleep past it.
            std::thread::park_timeout(wait);
        }
        Ok(())
    }
}

/// Whether a codec that did not configure or start failed for a moment: the system
/// short of codecs (another stream's still going) or taking it back.
fn passing(e: MediaError) -> bool {
    matches!(
        e,
        MediaError::CodecErrorInsufficientResource | MediaError::CodecErrorReclaimed
    )
}

/// A codec for `codec` decoding `width`×`height` into `window`, told what happens in
/// `told`. Without a decoder for the codec, and (on its first try, `failures` 0) when
/// it refuses the size, the failure lasts.
fn start_decoder(
    codec: Codec,
    width: u32,
    height: u32,
    window: &NativeWindow,
    told: &Arc<Mutex<Told>>,
    failures: u32,
) -> Result<MediaCodec, Fail> {
    let mime = codec.mime();
    let Some(mut decoder) = MediaCodec::from_decoder_type(mime) else {
        return Err(Fail::Lasting(
            Why::NoDecoder,
            anyhow::anyhow!("no decoder for {mime}"),
        ));
    };
    // Before `configure`: a codec takes it only then.
    decoder
        .set_async_notify_callback(Some(callbacks(told, std::thread::current())))
        .context("asynchronous decoder")?;
    let mut format = MediaFormat::new();
    format.set_str("mime", mime);
    format.set_i32("width", width as i32);
    format.set_i32("height", height as i32);
    format.set_i32("max-input-size", (width * height).max(2 << 20) as i32);
    // Output each frame as soon as it is decoded instead of buffering for smooth playback.
    format.set_i32("low-latency", 1);
    format.set_i32("vendor.qti-ext-dec-low-latency.enable", 1);
    format.set_i32("vendor.qti-ext-dec-picture-order.enable", 1);
    format.set_i32("priority", 0);
    // Only for the selftest's A/B (`decoder_rate=max`): as fast as the codec can,
    // as some players ask of Qualcomm decoders, at the price of heat.
    #[cfg(all(feature = "selftest", not(feature = "store")))]
    if crate::selftest::decoder_rate_max() {
        format.set_i32("operating-rate", i32::from(i16::MAX));
    }
    // A codec that worked for this stream before and now refuses fails for a moment;
    // one that refuses it from the start, with a reason other than being short of
    // codecs, refuses its size.
    let refused = |e: MediaError, what: &str| {
        let lasting = failures == 0 && !passing(e);
        let e = anyhow::Error::from(e).context(format!("{what} {width}x{height}"));
        if lasting {
            Fail::Lasting(Why::BadSize, e)
        } else {
            Fail::Passing(e)
        }
    };
    decoder
        .configure(&format, Some(window), MediaCodecDirection::Decoder)
        .map_err(|e| refused(e, "configure decoder"))?;
    decoder.start().map_err(|e| refused(e, "start decoder"))?;
    info!(
        "decoder {} started for {width}x{height}",
        decoder.name().unwrap_or_default()
    );
    Ok(decoder)
}

/// What the codec calls from its own thread: each notes what happened in `told` and
/// wakes the decoder thread. Nothing here waits for long or allocates (but for an
/// error).
fn callbacks(told: &Arc<Mutex<Told>>, decoder: Thread) -> AsyncNotifyCallback {
    let (t, d) = (told.clone(), decoder.clone());
    let on_input = move |index: usize| {
        lock(&t).inputs.push_back(index);
        d.unpark();
    };
    let (t, d) = (told.clone(), decoder.clone());
    let on_output = move |index: usize, info: &BufferInfo| {
        lock(&t)
            .outputs
            .push_back((index, info.presentation_time_us() as u64));
        d.unpark();
    };
    let t = told.clone();
    let mut passing = (None::<Instant>, 0u32);
    let on_error = move |e: MediaError, action: ActionCode, detail: &CStr| {
        if action.is_transient() {
            let (logged, since) = &mut passing;
            *since += 1;
            let now = Instant::now();
            if logged.is_none_or(|l| now.saturating_duration_since(l) >= TRANSIENT_LOG_EVERY) {
                warn!("decoder: {e} ({detail:?}), going on ({since} such since the last line)");
                *logged = Some(now);
                *since = 0;
            }
            return;
        }
        lock(&t).failed = Some(format!("codec error: {e} ({detail:?})"));
        decoder.unpark();
    };
    AsyncNotifyCallback {
        on_input_available: Some(Box::new(on_input)),
        on_output_available: Some(Box::new(on_output)),
        on_format_changed: None,
        on_error: Some(Box::new(on_error)),
    }
}

/// After a frame is lost, the frames that follow it until the next keyframe would
/// decode into garbage: skip them and ask the PC for a keyframe (not on every frame).
struct KeyframeGate {
    stream: Stream,
    feedback: Feedback,
    wait: KeyframeWait,
}

impl KeyframeGate {
    fn new(stream: Stream, feedback: Feedback) -> Self {
        Self {
            stream,
            feedback,
            wait: KeyframeWait::default(),
        }
    }

    /// Skips frames until a keyframe that is on its way: asks for one only if a frame
    /// other than a keyframe comes first.
    fn expect_keyframe(&mut self) {
        self.wait.expect();
    }

    fn wait(&mut self) {
        if self.wait.lost(Instant::now()) {
            self.ask();
        }
    }

    /// Sends the ask held back if its time came at `now`: how long the thread may
    /// sleep (at most `sleep`) before the next one is due.
    fn held(&mut self, now: Instant, sleep: Duration) -> Duration {
        if self.wait.due(now) {
            self.ask();
        }
        self.wait
            .due_at()
            .map_or(sleep, |at| sleep.min(at.saturating_duration_since(now)))
    }

    fn ask(&self) {
        let _ = self.feedback.send(ClientMsg::RequestKeyframe {
            id: self.stream.id,
            generation: self.stream.generation,
        });
    }

    fn passes(&mut self, frame: &Frame) -> bool {
        let verdict = self.wait.passes(frame.keyframe, Instant::now());
        if verdict.ask {
            self.ask();
        }
        verdict.pass
    }
}

/// Hands a frame to the codec in input buffer `index`; `false` when it had to be
/// dropped.
/// With `spoil`, its bytes past the start are spoiled first (a fault on purpose).
fn feed(
    decoder: &MediaCodec,
    index: usize,
    frame: &Frame,
    spoil: bool,
    timing: &mut Timing,
) -> Result<bool> {
    let dst = decoder
        .input_buffer(index)
        .context("the codec gave no input buffer")?;
    let n = frame.data.len();
    if n > dst.len() {
        warn!(
            "frame of {n} bytes does not fit the {} byte input buffer",
            dst.len()
        );
        decoder.queue_input_buffer_by_index(index, 0, 0, frame.pts_us, 0)?;
        return Ok(false);
    }
    dst[..n].write_copy_of_slice(&frame.data);
    if spoil {
        // Past the headers of the first units: the picture data itself, every few
        // bytes turned over.
        for b in dst[n.min(64)..n].iter_mut().step_by(7) {
            // SAFETY: written just above (`write_copy_of_slice`), so initialised.
            let v = unsafe { b.assume_init_read() };
            b.write(!v);
        }
        warn!("decoder: a frame of {n} bytes spoiled on purpose");
    }
    decoder.queue_input_buffer_by_index(index, 0, n, frame.pts_us, 0)?;
    timing.queued(frame);
    Ok(true)
}

/// How often each decoder logs its timing.
const REPORT_EVERY: Duration = Duration::from_secs(10);

/// Where time goes per frame: network arrival → decoder input → decoded output, and
/// how many frames wait for the decoder.
struct Timing {
    stream: Option<Stream>,
    /// Reports shown frames so the host can measure the whole trip.
    feedback: Option<Feedback>,
    /// pts → (received, queued, the frame's number) for frames inside the decoder.
    in_flight: HashMap<u64, (Instant, Instant, u32)>,
    /// When a shown frame was last reported to the PC.
    last_ack: Option<Instant>,
    /// When the stream started here, until its first picture is out (for the log).
    started: Option<Instant>,
    /// Received → handed to the decoder.
    wait: Histogram,
    /// Handed to the decoder → decoded.
    decode: Histogram,
    /// Frames still waiting behind each one taken: (sum, max, taken).
    queue: (u64, usize, u32),
    report: Period,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            stream: None,
            feedback: None,
            in_flight: HashMap::new(),
            last_ack: None,
            started: None,
            wait: Histogram::default(),
            decode: Histogram::default(),
            queue: (0, 0, 0),
            report: Period::new(REPORT_EVERY),
        }
    }
}

impl Timing {
    /// A frame was taken from the network with `behind` more waiting.
    fn taken(&mut self, behind: usize) {
        let (sum, max, n) = &mut self.queue;
        *sum += behind as u64;
        *max = (*max).max(behind);
        *n += 1;
        #[cfg(all(feature = "selftest", not(feature = "store")))]
        crate::selftest::taken(behind);
    }

    fn queued(&mut self, frame: &Frame) {
        if self.in_flight.len() > 64 {
            self.in_flight.clear();
        }
        self.in_flight
            .insert(frame.pts_us, (frame.received, Instant::now(), frame.seq));
    }

    fn decoded(&mut self, pts_us: u64) {
        let Some((received, queued, seq)) = self.in_flight.remove(&pts_us) else {
            return;
        };
        if let (Some(started), Some(stream)) = (self.started.take(), self.stream) {
            info!(
                "stream {} {:?}: first picture {} ms after it started",
                stream.id.0,
                stream.generation,
                started.elapsed().as_millis()
            );
        }
        // Enough for the PC's latency figures: one frame every 100 ms, not every frame
        // as its own network packet.
        if let (Some(f), Some(stream)) = (&self.feedback, self.stream)
            && self
                .last_ack
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(100))
        {
            self.last_ack = Some(Instant::now());
            let _ = f.send(ClientMsg::FrameShown {
                id: stream.id,
                generation: stream.generation,
                seq,
            });
        }
        let (wait, decode) = (queued - received, queued.elapsed());
        self.wait.add(wait);
        self.decode.add(decode);
        #[cfg(all(feature = "selftest", not(feature = "store")))]
        if let Some(stream) = self.stream {
            crate::selftest::decoded(stream.id.0, stream.generation.0, seq, wait, decode);
        }
    }

    fn report(&mut self) {
        if self.report.due(Instant::now()).is_none() {
            return;
        }
        let (sum, max, taken) = std::mem::take(&mut self.queue);
        let wait = std::mem::take(&mut self.wait);
        let decode = std::mem::take(&mut self.decode);
        // Quiet while no frames come (headset taken off, say).
        if taken == 0 && decode.count() == 0 {
            return;
        }
        info!(
            "decoder {}: {} frames decoded; net → decoder {}; decode {}; waiting for the decoder avg {:.1} max {max}; inside the decoder {}",
            self.stream.map_or(0, |s| s.id.0),
            decode.count(),
            wait.summary_ms(),
            decode.summary_ms(),
            sum as f64 / f64::from(taken.max(1)),
            self.in_flight.len()
        );
    }
}
