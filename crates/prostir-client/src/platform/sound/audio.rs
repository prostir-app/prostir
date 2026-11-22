//! Plays PC sound received over the connection through AAudio (low-latency mode).

#![expect(
    clippy::disallowed_methods,
    reason = "the audio thread is part of the shell"
)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use log::{info, warn};
use ndk_sys as ffi;
use prostir_proto::stats::Period;
use prostir_proto::{AUDIO_CHANNELS, AUDIO_RATE};

use crate::backoff::Backoff;

/// How often the player logs underruns and dropped slices.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// A sound output that does not open is tried again after this, then twice as long
/// each time up to [`OPEN_AGAIN_MAX`].
const OPEN_AGAIN_FIRST: Duration = Duration::from_millis(300);
const OPEN_AGAIN_MAX: Duration = Duration::from_secs(5);

/// Most audio kept waiting before older slices are dropped (10 ms each), so sound
/// never drifts behind the picture after a Wi-Fi hiccup.
const MAX_QUEUED: usize = 6;
const FRAME_BYTES: usize = 2 * AUDIO_CHANNELS as usize;

/// Left and right gains (as `f32` bits) for placing the sound towards a monitor;
/// 1 and 1 leave it as it is. One per start of the app: the frame loop sets them,
/// the audio thread of each session plays with them.
#[derive(Debug)]
pub struct Gains([AtomicU32; 2]);

impl Default for Gains {
    fn default() -> Self {
        Self([
            AtomicU32::new(1.0f32.to_bits()),
            AtomicU32::new(1.0f32.to_bits()),
        ])
    }
}

impl Gains {
    /// Places the sound at `angle` radians right of where the user looks (0: straight
    /// ahead, ±π: behind), or plain stereo with `None`. Equal-power balance, a little
    /// quieter from behind; changes are ramped in the audio thread.
    pub fn place(&self, angle: Option<f32>) {
        let (left, right) = match angle {
            None => (1.0, 1.0),
            Some(a) => {
                // Towards the side, keeping some of the other ear (never hard-panned).
                let pan = (a.sin() * 0.8).clamp(-1.0, 1.0);
                let behind = if a.cos() < 0.0 { 0.8 } else { 1.0 };
                let l = ((1.0 - pan) / 2.0).sqrt() * std::f32::consts::SQRT_2 * behind;
                let r = ((1.0 + pan) / 2.0).sqrt() * std::f32::consts::SQRT_2 * behind;
                (l.min(1.0), r.min(1.0))
            }
        };
        self.0[0].store(f32::to_bits(left), Ordering::Relaxed);
        self.0[1].store(f32::to_bits(right), Ordering::Relaxed);
    }

    fn get(&self) -> [f32; 2] {
        [0, 1].map(|i| f32::from_bits(self.0[i].load(Ordering::Relaxed)))
    }
}

/// Applies the gains to interleaved stereo s16 `pcm`, ramping from `current`.
fn apply_gains(pcm: &mut [u8], current: &mut [f32; 2], gains: &Gains) {
    let target = gains.get();
    if *current == [1.0, 1.0] && target == [1.0, 1.0] {
        return;
    }
    let frames = pcm.len() / FRAME_BYTES;
    if frames == 0 || AUDIO_CHANNELS != 2 {
        return;
    }
    for (f, frame) in pcm.as_chunks_mut::<FRAME_BYTES>().0.iter_mut().enumerate() {
        let k = (f + 1) as f32 / frames as f32;
        for (ch, sample) in frame.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let g = current[ch] + (target[ch] - current[ch]) * k;
            let v = i16::from_le_bytes([sample[0], sample[1]]) as f32 * g;
            sample.copy_from_slice(&(v.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
    }
    *current = target;
}

/// Plays until dropped.
pub struct Player {
    tx: SyncSender<Vec<u8>>,
    /// Slices played (or dropped), back to be received into again.
    spare: Receiver<Vec<u8>>,
    /// Slices dropped because the audio thread was far behind (shared with it, which
    /// logs them).
    behind: Arc<AtomicU32>,
}

impl Player {
    /// Starts the audio thread, which places the sound with `gains`.
    pub fn spawn(gains: Arc<Gains>) -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        let (back, spare) = mpsc::sync_channel(32);
        let behind = Arc::new(AtomicU32::new(0));
        let counted = behind.clone();
        let _ = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                crate::platform::android::raise_thread_priority(
                    crate::platform::android::Priority::Audio,
                );
                // An AAudio stream dies when the audio route changes (headphones in or
                // out): open a new one and keep playing. One that does not open is
                // tried again, less often while it goes on failing, for as long as the
                // session sends sound: it never gives up for the rest of the session.
                let mut backoff = Backoff::new(OPEN_AGAIN_FIRST, OPEN_AGAIN_MAX);
                loop {
                    let pause = match Stream::open() {
                        Ok(stream) => match play(&stream, &rx, &back, &counted, &gains) {
                            Ended::Closed => return,
                            Ended::Failed => {
                                backoff.worked();
                                OPEN_AGAIN_FIRST
                            }
                        },
                        Err(e) => {
                            let pause = backoff.failed();
                            warn!(
                                "no sound: AAudio error {e}; trying again in {} ms",
                                pause.as_millis()
                            );
                            pause
                        }
                    };
                    if !pass_while_silent(pause, &rx, &back, &counted) {
                        return;
                    }
                }
            })
            .inspect_err(|e| warn!("no sound: {e}"));
        Self { tx, spare, behind }
    }

    /// A slice played, back to be received into again (see `Assembler::recycle`).
    pub fn played(&self) -> Option<Vec<u8>> {
        self.spare.try_recv().ok()
    }

    pub fn push(&self, pcm: Vec<u8>) {
        // Full means the player is far behind: drop rather than add latency.
        if let Err(mpsc::TrySendError::Full(_)) = self.tx.try_send(pcm) {
            self.behind.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// An AAudio output stream. Called through `ndk-sys` directly: `ndk` 0.9 reports a
/// successful write (the frame count) as an error.
struct Stream(*mut ffi::AAudioStream);

// SAFETY: AAudio streams may be used from any one thread at a time; this one is
// owned by the audio thread.
unsafe impl Send for Stream {}

impl Stream {
    fn open() -> Result<Self, i32> {
        // SAFETY: plain AAudio calls; the builder is deleted on every path.
        unsafe {
            let mut builder = std::ptr::null_mut();
            check(ffi::AAudio_createStreamBuilder(&raw mut builder))?;
            ffi::AAudioStreamBuilder_setDirection(builder, ffi::AAUDIO_DIRECTION_OUTPUT as i32);
            ffi::AAudioStreamBuilder_setFormat(builder, ffi::AAUDIO_FORMAT_PCM_I16);
            ffi::AAudioStreamBuilder_setChannelCount(builder, AUDIO_CHANNELS as i32);
            ffi::AAudioStreamBuilder_setSampleRate(builder, AUDIO_RATE as i32);
            ffi::AAudioStreamBuilder_setPerformanceMode(
                builder,
                ffi::AAUDIO_PERFORMANCE_MODE_LOW_LATENCY as i32,
            );
            ffi::AAudioStreamBuilder_setSharingMode(
                builder,
                ffi::AAUDIO_SHARING_MODE_SHARED as i32,
            );
            let mut stream = std::ptr::null_mut();
            let opened = ffi::AAudioStreamBuilder_openStream(builder, &raw mut stream);
            ffi::AAudioStreamBuilder_delete(builder);
            check(opened)?;
            // A few bursts of headroom absorb network jitter without much delay.
            let burst = ffi::AAudioStream_getFramesPerBurst(stream).max(96);
            ffi::AAudioStream_setBufferSizeInFrames(stream, burst * 4);
            let s = Self(stream);
            check(ffi::AAudioStream_requestStart(stream))?;
            info!(
                "sound: {} Hz, buffer {} frames",
                ffi::AAudioStream_getSampleRate(stream),
                burst * 4
            );
            Ok(s)
        }
    }

    /// How many times AAudio ran out of sound to play (or of room) since it opened;
    /// `None` when it cannot say (an error code).
    fn xruns(&self) -> Option<u32> {
        // SAFETY: the stream is open.
        u32::try_from(unsafe { ffi::AAudioStream_getXRunCount(self.0) }).ok()
    }

    /// Writes interleaved i16 frames, waiting up to 50 ms for room.
    fn write(&self, pcm: &[u8]) -> Result<(), i32> {
        let frames = (pcm.len() / FRAME_BYTES) as i32;
        // SAFETY: `pcm` holds `frames` whole frames; the stream is open.
        let n = unsafe { ffi::AAudioStream_write(self.0, pcm.as_ptr().cast(), frames, 50_000_000) };
        check(n)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream was opened by `Stream::open` and is closed once.
        unsafe {
            ffi::AAudioStream_requestStop(self.0);
            ffi::AAudioStream_close(self.0);
        }
    }
}

fn check(result: i32) -> Result<(), i32> {
    if result < 0 { Err(result) } else { Ok(()) }
}

/// While no stream plays, for `pause`: the slices that come go back to be received
/// into again (counted as dropped). `false` once the session is over.
fn pass_while_silent(
    pause: Duration,
    rx: &Receiver<Vec<u8>>,
    back: &SyncSender<Vec<u8>>,
    behind: &AtomicU32,
) -> bool {
    let until = Instant::now() + pause;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(slice) => {
                behind.fetch_add(1, Ordering::Relaxed);
                let _ = back.try_send(slice);
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
    true
}

/// Why playing stopped.
enum Ended {
    /// The session is over.
    Closed,
    /// The stream broke; another one may work.
    Failed,
}

/// Logs now and then how playing went: underruns (AAudio had nothing to play: a
/// click) and slices dropped, late ones here and ones the player was too far behind
/// to take (`behind`).
struct Report {
    period: Period,
    /// The underrun count at the start of the window.
    xruns: Option<u32>,
    late: u32,
    played: u32,
}

impl Report {
    fn tick(&mut self, stream: &Stream, behind: &AtomicU32) {
        let Some(window) = self.period.due(Instant::now()) else {
            return;
        };
        let xruns = stream.xruns();
        let behind = behind.swap(0, Ordering::Relaxed);
        // Quiet while nothing comes (headset taken off, sound switched off). A window
        // in which the sound stopped also counts the stop as an underrun.
        if self.played > 0 || self.late > 0 || behind > 0 {
            let underruns = match (xruns, self.xruns) {
                (Some(now), Some(before)) => now.saturating_sub(before).to_string(),
                _ => "unknown".into(),
            };
            info!(
                "sound: {} slices played in {:.0} s, {underruns} underruns, dropped {} late and {} behind",
                self.played,
                window.as_secs_f64(),
                self.late,
                behind
            );
        }
        self.xruns = xruns;
        self.late = 0;
        self.played = 0;
    }
}

fn play(
    stream: &Stream,
    rx: &Receiver<Vec<u8>>,
    back: &SyncSender<Vec<u8>>,
    behind: &AtomicU32,
    target: &Gains,
) -> Ended {
    // Made once: never more than this waits (the rest is dropped below).
    let mut queue: VecDeque<Vec<u8>> = VecDeque::with_capacity(MAX_QUEUED + 33);
    let mut gains = [1.0f32; 2];
    let mut report = Report {
        period: Period::new(REPORT_EVERY),
        xruns: stream.xruns(),
        late: 0,
        played: 0,
    };
    loop {
        report.tick(stream, behind);
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(pcm) => queue.push_back(pcm),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ended::Closed,
        }
        while let Ok(pcm) = rx.try_recv() {
            queue.push_back(pcm);
        }
        while queue.len() > MAX_QUEUED {
            if let Some(late) = queue.pop_front() {
                let _ = back.try_send(late);
            }
            report.late += 1;
        }
        while let Some(mut pcm) = queue.pop_front() {
            apply_gains(&mut pcm, &mut gains, target);
            let written = stream.write(&pcm);
            // Back to the network, to be received into again; dropped when it has
            // enough.
            let _ = back.try_send(pcm);
            if let Err(e) = written {
                warn!("sound output: AAudio error {e}");
                return Ended::Failed;
            }
            report.played += 1;
        }
    }
}
