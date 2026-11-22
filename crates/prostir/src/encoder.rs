//! Frames from the platform (a PipeWire node, a capture) → hardware encoder → encoded
//! access units, via GStreamer.
//!
//! Each output gets its own encoder, picked for where its frames live
//! ([`crate::platform::FrameSource::memory`]) so they stay on the graphics card
//! (`choice.rs`), from the encoders this PC was found to have (`probe.rs`, kept in the
//! cache directory), sharing one CUDA context or Direct3D 11 device per card (`gpu.rs`;
//! VA-API encoders keep a display each). An encoder
//! that does not open goes on to the next one for that output only: `Displays` hears
//! of it as [`EncoderEvent::Unopened`] and learns the encoder's session limit from it.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use log::{debug, error, info, warn};
use prostir_proto::Codec;

mod choice;
mod gpu;
mod probe;

pub use choice::{Backend, Choice, Kind, Wanted, codec_label, note};
pub use gpu::GpuDevices;
#[cfg(not(feature = "fake-desktop"))]
pub use probe::file_identity;
pub use probe::{Next, catalog, probe_timeout, settle, start_probing};

use crate::platform::AdapterId;

impl Backend {
    /// The GStreamer elements it encodes `codec` with; none for a codec it cannot
    /// encode (AV1: no encoder of Prostir makes it yet). The encoder is the last.
    fn elements(self, codec: Codec) -> Option<&'static [&'static str]> {
        Some(match (self, codec) {
            (Self::Va, Codec::H265) => &["vapostproc", "vah265enc"],
            (Self::Va, Codec::H264) => &["vapostproc", "vah264enc"],
            (Self::Nvenc, Codec::H265) => &["videoconvert", "nvh265enc"],
            (Self::Nvenc, Codec::H264) => &["videoconvert", "nvh264enc"],
            (Self::X264, Codec::H264) => &["videoconvert", "x264enc"],
            // Windows (step 11c), as the prototype ran them: the card's own encoders
            // take the capture's textures as they are, Media Foundation in NV12.
            (Self::NvencD3d11, Codec::H265) => &["nvd3d11h265enc"],
            (Self::NvencD3d11, Codec::H264) => &["nvd3d11h264enc"],
            (Self::Amf, Codec::H265) => &["amfh265enc"],
            (Self::Amf, Codec::H264) => &["amfh264enc"],
            (Self::Qsv, Codec::H265) => &["qsvh265enc"],
            (Self::Qsv, Codec::H264) => &["qsvh264enc"],
            (Self::MediaFoundation, Codec::H265) => &["d3d11convert", "mfh265enc"],
            (Self::MediaFoundation, Codec::H264) => &["d3d11convert", "mfh264enc"],
            #[cfg(feature = "fake-desktop")]
            (Self::Fake, Codec::H264) => &["videoconvert", "x264enc"],
            #[cfg(feature = "fake-desktop")]
            (Self::Fake, Codec::H265) => return None,
            (_, Codec::Av1) | (Self::X264, Codec::H265) => return None,
        })
    }
}

/// Keyframes come when they are needed, not every so many frames: TCP loses nothing,
/// a headset that lost frames asks for one, and one more follows when the screen
/// goes still (see the router). A periodic keyframe was a burst twenty times a
/// frame's size every ten seconds of motion on each monitor. x264 takes this as no
/// limit, NVENC takes `-1`; VA-API's limit is 1024 frames.
const X264_GOP: u32 = 1 << 30;
const VA_GOP: u32 = 1024;

/// What NVENC takes from the CPU's converter: the desktop's own BGRx or BGRA where
/// its version takes them (it turns them into YUV on the graphics card; the frames
/// then pass the converter as they are), else NV12, converted on the CPU. The caps
/// after the encoder ask for 4:2:0 profiles, which the headset decodes. Not tried on
/// an NVIDIA card yet.
const NVENC_IN: &str = "video/x-raw,format={ (string)BGRx, (string)BGRA, (string)NV12 }";

/// NVENC tuned for the lowest latency, where its version has the settings (the
/// older `nvh264enc` and the one from GStreamer 1.22 differ): the low-latency preset
/// and tuning, and no look-ahead. Its VBV keeps the encoder's own size: one of a few
/// frames would crush the keyframes. Each setting only where the encoder has it; the
/// probe opens the encoder tuned so, and an encoder that does not open is passed
/// over as before. Not tried on an NVIDIA card yet: the picture (text, keyframes)
/// needs a look there.
pub(crate) fn tune(pipeline: &gst::Pipeline, cfg: &EncoderConfig) {
    if cfg.backend != Backend::Nvenc {
        return;
    }
    let Some(enc) = pipeline.by_name("enc") else {
        return;
    };
    // The newer encoder: the fastest tuning with a middle preset.
    if set_enum(&enc, "tune", "ultra-low-latency") {
        set_enum(&enc, "preset", "p4");
    } else {
        set_enum(&enc, "preset", "low-latency-hq");
    }
    set_number(&enc, "rc-lookahead", 0);
}

/// Sets enum property `name` of `element` to its value `nick`, when it has both;
/// whether it did.
fn set_enum(element: &gst::Element, name: &str, nick: &str) -> bool {
    let Some(pspec) = element.find_property(name) else {
        return false;
    };
    let Some(spec) = pspec.downcast_ref::<gst::glib::ParamSpecEnum>() else {
        return false;
    };
    let Some(value) = spec.enum_class().to_value_by_nick(nick) else {
        return false;
    };
    element.set_property_from_value(name, &value);
    debug!("{}: {name}={nick}", element.name());
    true
}

/// Sets number property `name` of `element` to `value`, when it has it and takes it.
fn set_number(element: &gst::Element, name: &str, value: u32) {
    let Some(pspec) = element.find_property(name) else {
        return;
    };
    let v = if let Some(p) = pspec.downcast_ref::<gst::glib::ParamSpecUInt>() {
        (p.minimum()..=p.maximum())
            .contains(&value)
            .then(|| value.to_value())
    } else if let Some(p) = pspec.downcast_ref::<gst::glib::ParamSpecInt>() {
        i32::try_from(value)
            .ok()
            .filter(|v| (p.minimum()..=p.maximum()).contains(v))
            .map(|v| v.to_value())
    } else {
        None
    };
    if let Some(v) = v {
        element.set_property_from_value(name, &v);
        debug!("{}: {name}={value}", element.name());
    }
}

/// The parser after the encoder of `codec`.
fn parser(codec: Codec) -> &'static str {
    match codec {
        Codec::H265 => "h265parse",
        Codec::H264 => "h264parse",
        Codec::Av1 => "av1parse",
    }
}

/// What this PC can encode with: each codec, and whether on the graphics card, as the
/// encoders' probes found (see `probe.rs`).
pub fn available() -> Vec<(Codec, bool)> {
    catalog().available()
}

/// The encoder this PC encodes with when nothing is known of the frames: VA-API, then
/// NVENC, then x264 on the CPU, in the preferred codec first, as their probes found
/// them. `software` forces x264. The error names what to install. Each output then
/// gets its own encoder for where its frames are (`Catalog::rank`).
pub fn pick(prefer: Codec, software: bool) -> Result<(Backend, Codec)> {
    catalog().pick(prefer, software)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub backend: Backend,
    /// Slower VA-API preset: clearer small text at the same bitrate for about 2 ms more
    /// encoding per frame.
    pub sharp: bool,
}

impl EncoderConfig {
    /// VA-API `target-usage`: 4 is clearer, 6 is faster.
    pub fn target_usage(&self) -> u32 {
        if self.sharp { 4 } else { 6 }
    }
}

pub enum EncoderEvent {
    /// Negotiated stream size; emitted before the first frame and on every change.
    Size {
        width: u32,
        height: u32,
    },
    Frame {
        pts_us: u64,
        keyframe: bool,
        data: Vec<u8>,
        /// How it went through the encoder (`None` when it could not be paired with
        /// its way in).
        timing: Option<FrameTiming>,
    },
    Failed(String),
    /// Its source lost what it read (the desktop's stream of the output is gone, or
    /// it ended): not the encoder's doing, and no other encoder would fare better.
    SourceLost(String),
    /// The encoder itself failed before its first frame: it did not open (no more
    /// sessions, a size or format its driver does not take). Another encoder may.
    Unopened(NotOpened),
}

/// How one frame went through its encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    /// From entering the converter to leaving the parser, microseconds.
    pub encode_us: u32,
    /// The picture of the frame before it, sent again (a keepalive of the source on a
    /// still screen): its memory is the previous frame's.
    pub repeat: bool,
}

/// Why an encoder did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOpened {
    pub why: String,
    /// It said it had no more sessions, or no room for one more (see
    /// [`short_of_sessions`]).
    pub short: bool,
    /// It was opening again after a pause (every stream does at once then).
    pub resumed: bool,
}

/// The source of the frames failed before the encoder started: its stream, not the
/// encoder (KWin's PipeWire node is gone, or WirePlumber links it to nothing).
/// Another encoder on the same source fails the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFailed(pub String);

impl std::fmt::Display for SourceFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SourceFailed {}

/// How long a failed start waits for its source to say why, on the bus: the state
/// change and the source's error come from two threads.
const SOURCE_SAYS: std::time::Duration = std::time::Duration::from_millis(500);

/// A source's failure while the encoder starts: kept for [`Encoder::start`] to return
/// rather than told as the encoder's (which would end the session); and whether the
/// source failed at all, for [`Encoder`]'s drop.
#[derive(Default)]
struct SourceGate {
    /// Started (failures are told from then on), and the failure kept before.
    state: Mutex<(bool, Option<String>)>,
    came: std::sync::Condvar,
    failed: AtomicBool,
}

/// What encoders say when they have no session left: GStreamer's "no space left" and
/// "busy" resource errors (the fake encoder of the tests says the first), NVENC's out
/// of memory and client key errors (a consumer card's session cap), and VA-API's
/// allocation, limit and busy statuses.
const SHORT_OF_SESSIONS: &[&str] = &[
    "NV_ENC_ERR_OUT_OF_MEMORY",
    "NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY",
    "NV_ENC_ERR_ENCODER_BUSY",
    "VA_STATUS_ERROR_ALLOCATION_FAILED",
    "VA_STATUS_ERROR_MAX_NUM_EXCEEDED",
    "VA_STATUS_ERROR_HW_BUSY",
];

/// Whether an error (`error`, with its debug text `debug`) says the encoder has no
/// session left, rather than anything else (a format, a size, a broken driver).
pub fn short_of_sessions(error: Option<&gst::glib::Error>, text: &str) -> bool {
    error.is_some_and(|e| {
        e.matches(gst::ResourceError::NoSpaceLeft) || e.matches(gst::ResourceError::Busy)
    }) || SHORT_OF_SESSIONS.iter().any(|s| text.contains(s))
}

pub struct Encoder {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    stop: Arc<AtomicBool>,
    paused: Arc<Pause>,
    /// The source sends nothing after a pause until the screen changes (PipeWire from
    /// KWin): it is started over on resuming. A capture pushing its own frames
    /// (`appsrc`) repeats the last one by itself.
    restart_on_resume: bool,
    /// It takes frames straight from GPU buffers (dmabuf), so a capture hands it those.
    takes_dmabuf: bool,
    bus_thread: Option<JoinHandle<()>>,
    /// Whether its source failed, and what it said while starting.
    gate: Arc<SourceGate>,
    kind: Kind,
    codec: Codec,
    /// It has encoded a frame since it last started.
    encoded: Arc<AtomicBool>,
    /// It was resumed after a pause.
    resumed: Arc<AtomicBool>,
    /// What to say of the encoder this output got, when it is not the best for it.
    note: Option<String>,
    /// Its frames go through the CPU on their way to it.
    copies: bool,
    /// The graphics devices it took, let go after the pipeline stopped (the fields
    /// drop after `Drop::drop` and in this order).
    devices: gpu::Lease,
    /// A fake hardware encoder that opened, counted until dropped.
    #[cfg(feature = "fake-desktop")]
    fake_open: bool,
}

/// Where the frames come from: the start of the pipeline, made by the platform (a
/// PipeWire node on KDE, frames pushed by a capture on Hyprland).
#[derive(Debug, Clone)]
pub struct Input {
    /// GStreamer source elements. Frames the platform pushes itself go into an `appsrc`
    /// named `src` (see [`Encoder::appsrc`]).
    pub source: String,
    /// What it is, for logs: `node 42`, `Prostir-1`.
    pub label: String,
    /// The frame rate the encoder is told when the source says none (KWin's PipeWire
    /// stream says `0/1`: a frame whenever the screen changes). Frames still come only
    /// when they come; see [`tell_frame_rate`].
    pub frame_rate: Option<gst::Fraction>,
    /// The source is live, though it may not say so when the encoder first asks
    /// (`pipewiresrc`); see [`tell_live`].
    pub live: bool,
}

fn pipeline_description(input: &Input, cfg: &EncoderConfig) -> Result<String> {
    Ok(format!(
        "{} ! appsink name=sink sync=false max-buffers=8",
        encoding(input, cfg)?
    ))
}

/// The pipeline from the source to the encoded frames, without the sink.
fn encoding(input: &Input, cfg: &EncoderConfig) -> Result<String> {
    let source = &input.source;
    let (encoder, parser, caps) = match (cfg.codec, cfg.backend) {
        (Codec::Av1, _) | (Codec::H265, Backend::X264) => {
            bail!("no encoder for {:?} on {:?}", cfg.codec, cfg.backend)
        }
        (_, Backend::NvencD3d11 | Backend::Amf | Backend::Qsv | Backend::MediaFoundation) => {
            bail!("{} is for Windows (step 11c)", cfg.backend.label())
        }
        // x264 underneath; one more than its sessions lets no frame through, and its
        // encoder says it did not open (see `Encoder::start`).
        #[cfg(feature = "fake-desktop")]
        (Codec::H265, Backend::Fake) => bail!("the fake encoder makes H.264 only"),
        #[cfg(feature = "fake-desktop")]
        (Codec::H264, Backend::Fake) if probe::fake::refuses() && !probe::fake::opens() => {
            bail!("no more encoder sessions (NV_ENC_ERR_OUT_OF_MEMORY)")
        }
        #[cfg(feature = "fake-desktop")]
        (Codec::H264, Backend::Fake) => (
            format!(
                "videoconvert name=enc_in n-threads=0 ! video/x-raw,format=I420 ! valve drop={} ! x264enc name=enc tune=zerolatency speed-preset=superfast \
                 bitrate={} key-int-max={X264_GOP} bframes=0",
                !probe::fake::opens(),
                cfg.bitrate_kbps
            ),
            "h264parse config-interval=-1",
            "video/x-h264,stream-format=byte-stream,alignment=au,profile=high",
        ),
        (Codec::H264, Backend::X264) => (
            format!(
                "videoconvert name=enc_in n-threads=0 ! video/x-raw,format=I420 ! x264enc name=enc tune=zerolatency speed-preset=superfast \
                 bitrate={} key-int-max={X264_GOP} bframes=0",
                cfg.bitrate_kbps
            ),
            "h264parse config-interval=-1",
            "video/x-h264,stream-format=byte-stream,alignment=au,profile=high",
        ),
        (Codec::H264, Backend::Nvenc) => (
            format!(
                "videoconvert name=enc_in n-threads=0 ! {NVENC_IN} ! nvh264enc name=enc rc-mode=cbr \
                 bitrate={} gop-size=-1 bframes=0 zerolatency=true",
                cfg.bitrate_kbps
            ),
            "h264parse config-interval=-1",
            "video/x-h264,stream-format=byte-stream,alignment=au,profile=high",
        ),
        (Codec::H265, Backend::Nvenc) => (
            format!(
                "videoconvert name=enc_in n-threads=0 ! {NVENC_IN} ! nvh265enc name=enc rc-mode=cbr \
                 bitrate={} gop-size=-1 bframes=0 zerolatency=true",
                cfg.bitrate_kbps
            ),
            "h265parse config-interval=-1",
            "video/x-h265,stream-format=byte-stream,alignment=au,profile=main",
        ),
        (Codec::H264, Backend::Va) => (
            format!(
                "vapostproc name=enc_in ! video/x-raw(memory:VAMemory),format=NV12 ! vah264enc name=enc rate-control=cbr bitrate={} \
                 key-int-max={VA_GOP} b-frames=0 ref-frames=1 target-usage={}",
                cfg.bitrate_kbps,
                cfg.target_usage()
            ),
            "h264parse config-interval=-1",
            "video/x-h264,stream-format=byte-stream,alignment=au",
        ),
        (Codec::H265, Backend::Va) => (
            format!(
                "vapostproc name=enc_in ! video/x-raw(memory:VAMemory),format=NV12 ! vah265enc name=enc rate-control=cbr bitrate={} \
                 key-int-max={VA_GOP} b-frames=0 ref-frames=1 target-usage={}",
                cfg.bitrate_kbps,
                cfg.target_usage()
            ),
            "h265parse config-interval=-1",
            "video/x-h265,stream-format=byte-stream,alignment=au",
        ),
    };
    // In the test mode of the time code, the frames go through memory, where the code
    // is drawn into them (see `draw_time_code`).
    let stamp = if time_code() {
        " ! videoconvert ! video/x-raw,format=I420 ! identity name=timecode"
    } else {
        ""
    };
    let queue = queue_name(input);
    Ok(format!(
        "{source} ! queue name={queue} max-size-buffers=2 leaky=downstream{stamp} ! {encoder} \
         ! {parser} ! {caps}"
    ))
}

/// The queue right after the source, whose thread converts and encodes: named after
/// the output (`q-node-42`, `q-Prostir-1`), so the system's list of threads tells
/// whose it is.
fn queue_name(input: &Input) -> String {
    format!("q-{}", input.label)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Tells everything after `pad` that its frames come at `rate` when their caps say
/// no rate, or a rate of 0 (KWin's PipeWire stream), by rewriting the caps as they
/// pass. An encoder without a frame rate budgets badly (VA-API spends about twice the
/// bitrate on a busy screen, x264 holds each frame back until the next comes).
///
/// Not an element after the source (`capssetter`): that one answers the source's
/// question of which formats the encoder takes with "any", so `pipewiresrc` offers
/// PipeWire no format, WirePlumber links it to nothing ("target not found") and its
/// start hangs. The caps query passes a probe untouched.
pub fn tell_frame_rate(pad: &gst::Pad, rate: gst::Fraction) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        let Some(gst::PadProbeData::Event(event)) = &mut info.data else {
            return gst::PadProbeReturn::Ok;
        };
        let told = match event.view() {
            gst::EventView::Caps(c) => with_frame_rate(c.caps(), rate),
            _ => None,
        };
        if let Some(caps) = told {
            *event = gst::event::Caps::builder(&caps)
                .seqnum(event.seqnum())
                .build();
        }
        gst::PadProbeReturn::Ok
    });
}

/// Answers the question whether the source is live with yes, as it passes `pad`
/// (the latency query, answered upstream). VA-API asks once, when it first
/// configures, and keeps up to four frames back for a source that is not live;
/// `pipewiresrc` 1.6 says it is live only once its stream is up, about 30 ms after
/// VA-API asked. So VA-API kept each picture of a still screen until the next frame
/// came: PipeWire's keepalive, 2 s later (4 s, two kept). Told live, it gives each
/// out at once (measured with a frame every 2 s from a PipeWire node: 75 to 100 ms
/// from the converter to the parser, the card idle between frames, against 4 s).
pub fn tell_live(pad: &gst::Pad) {
    pad.add_probe(
        gst::PadProbeType::QUERY_UPSTREAM | gst::PadProbeType::PULL,
        |_, info| {
            if let Some(query) = info.query_mut()
                && let gst::QueryViewMut::Latency(q) = query.view_mut()
            {
                let (_, min, max) = q.result();
                q.set(true, min, max);
            }
            gst::PadProbeReturn::Ok
        },
    );
}

/// `caps` with frame rate `rate` where they say none or 0; `None` when every
/// structure says a rate already.
fn with_frame_rate(caps: &gst::CapsRef, rate: gst::Fraction) -> Option<gst::Caps> {
    let unsaid = |s: &gst::StructureRef| {
        s.get::<gst::Fraction>("framerate")
            .map_or(true, |r| r.numer() == 0)
    };
    if !caps.iter().any(unsaid) {
        return None;
    }
    let mut told = caps.to_owned();
    for s in told.make_mut().iter_mut() {
        if unsaid(s) {
            s.set("framerate", rate);
        }
    }
    Some(told)
}

/// The test mode for measuring the latency from the PC to the eye (`prostir serve
/// --time-code`, never on by itself): every monitor's frames get the time they were
/// captured drawn into their top left corner as a code (`prostir_proto::timecode`),
/// which the headset reads back from what it shows. The frames then go through
/// memory (on KDE PipeWire hands them over so); with GPU buffers on Hyprland, use
/// `--software`.
static TIME_CODE: AtomicBool = AtomicBool::new(false);

/// Turns the time code's test mode on, for every encoder started from now on.
pub fn draw_time_codes() {
    warn!("test mode: a time code is drawn into every monitor (--time-code)");
    TIME_CODE.store(true, Ordering::Relaxed);
}

fn time_code() -> bool {
    TIME_CODE.load(Ordering::Relaxed)
}

/// Draws the time each frame was captured (the clock's time of its timestamp, which
/// is `CLOCK_MONOTONIC`, in ms) into the frames that pass `pad` (I420).
fn draw_time_code(pad: &gst::Pad) {
    pad.add_probe(gst::PadProbeType::BUFFER, |pad, info| {
        let Some(video) = pad
            .current_caps()
            .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
        else {
            return gst::PadProbeReturn::Ok;
        };
        let base = pad
            .parent_element()
            .and_then(|e| e.base_time())
            .map_or(0, gst::ClockTime::nseconds);
        let Some(buffer) = info.buffer_mut() else {
            return gst::PadProbeReturn::Ok;
        };
        let ns = match buffer.pts() {
            Some(pts) => base + pts.nseconds(),
            None => monotonic_ns(),
        };
        let ms = (ns / 1_000_000) as u32;
        if let Ok(mut frame) =
            gst_video::VideoFrameRef::from_buffer_ref_writable(buffer.make_mut(), &video)
        {
            use gst_video::prelude::*;
            let (w, h) = (frame.width() as usize, frame.height() as usize);
            let stride = frame.plane_stride()[0] as usize;
            if let Ok(luma) = frame.plane_data_mut(0) {
                prostir_proto::timecode::draw(luma, stride, w, h, ms);
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// `CLOCK_MONOTONIC` now, in nanoseconds.
fn monotonic_ns() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a timespec to fill, valid through the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

impl Encoder {
    /// An encoder with `cfg` on card `adapter` (none: the one its driver picks), taking
    /// its graphics device from `gpu`. `on_event` returns `true` when the receiver needs
    /// a keyframe (e.g. it had to drop frames).
    pub fn start(
        input: &Input,
        cfg: &EncoderConfig,
        adapter: Option<AdapterId>,
        gpu: &GpuDevices,
        on_event: impl Fn(EncoderEvent) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        let desc = pipeline_description(input, cfg)?;
        let node = input.label.clone();
        debug!("pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)
            .context("failed to build GStreamer pipeline (are gst-plugins-bad with VA and gst-plugin-pipewire installed?)")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| anyhow!("appsink missing"))?;
        tune(&pipeline, cfg);
        // The source's last pad, before the queue.
        if let Some(pad) = pipeline
            .by_name(&queue_name(input))
            .and_then(|q| q.static_pad("sink"))
            .and_then(|p| p.peer())
        {
            if let Some(rate) = input.frame_rate {
                tell_frame_rate(&pad, rate);
            }
            if input.live {
                tell_live(&pad);
            }
        }
        if let Some(pad) = pipeline
            .by_name("timecode")
            .and_then(|e| e.static_pad("src"))
        {
            draw_time_code(&pad);
        }
        let devices = gpu.share_with(&pipeline);

        let on_event = Arc::new(on_event);
        // Nothing encoded yet: an error of the encoder now means it did not open.
        let encoded = Arc::new(AtomicBool::new(false));
        let resumed = Arc::new(AtomicBool::new(false));
        let last_size = Mutex::new((0u32, 0u32));
        let probe = Arc::new(Mutex::new(Probe::default()));
        // Frames enter the encoder in order and (no B-frames) leave in order, so a FIFO
        // of arrival times pairs them up despite the encoder rewriting timestamps.
        if let Some(pad) = pipeline
            .by_name("enc_in")
            .and_then(|e| e.static_pad("sink"))
        {
            // What the frames really come as (GPU buffers or memory, format, layout,
            // rate): said once for each change.
            let node = node.clone();
            pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                if let Some(event) = info.event()
                    && let gst::EventView::Caps(c) = event.view()
                {
                    info!("{node}: frames come as {}", c.caps());
                }
                gst::PadProbeReturn::Ok
            });
            let probe = probe.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                // The memory the picture lies in: a source's keepalive sends the memory
                // of the frame before again, new pictures come in other memory.
                let memory = info
                    .buffer()
                    .filter(|b| b.n_memory() > 0)
                    .map_or(0, |b| b.peek_memory(0).as_ptr() as usize);
                probe.lock().unwrap().entered(memory);
                gst::PadProbeReturn::Ok
            });
        }
        {
            let on_event = on_event.clone();
            let node = node.clone();
            let encoded = encoded.clone();
            // An encoder a test asked to break: once it sent a frame, it fails as a
            // driver can in the middle of a stream.
            #[cfg(feature = "fake-desktop")]
            let breaker = pipeline.by_name("enc").map(|e| e.downgrade());
            #[cfg(feature = "fake-desktop")]
            let fake = cfg.backend == Backend::Fake;
            #[cfg(feature = "fake-desktop")]
            let broke = AtomicBool::new(false);
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        encoded.store(true, Ordering::Relaxed);
                        if let Some(info) =
                            sample.caps().and_then(|c| c.structure(0)).and_then(|s| {
                                Some((
                                    s.get::<i32>("width").ok()? as u32,
                                    s.get::<i32>("height").ok()? as u32,
                                ))
                            })
                        {
                            let mut last = last_size.lock().unwrap();
                            if *last != info {
                                *last = info;
                                if on_event(EncoderEvent::Size {
                                    width: info.0,
                                    height: info.1,
                                }) {
                                    request_keyframe(sink);
                                }
                            }
                        }
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
                        let pts_us = buffer.pts().map_or(0, gst::ClockTime::useconds);
                        let timing = probe.lock().unwrap().left(&node);
                        if on_event(EncoderEvent::Frame {
                            pts_us,
                            keyframe,
                            data: map.as_slice().to_vec(),
                            timing,
                        }) {
                            request_keyframe(sink);
                        }
                        #[cfg(feature = "fake-desktop")]
                        if let Some(enc) = breaker.as_ref().and_then(gst::glib::WeakRef::upgrade)
                            && !broke.load(Ordering::Relaxed)
                            && crate::platform::encoder_breaks(&node, fake)
                        {
                            broke.store(true, Ordering::Relaxed);
                            gst::element_error!(
                                enc,
                                gst::StreamError::Encode,
                                ("broken on purpose (break-encoder)")
                            );
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        let stop = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(SourceGate::default());
        let bus = pipeline
            .bus()
            .ok_or_else(|| anyhow!("pipeline without bus"))?;
        let bus_thread = {
            let stop = stop.clone();
            let encoded = encoded.clone();
            let resumed = resumed.clone();
            let gate = gate.clone();
            std::thread::Builder::new()
                .name(format!("gst-bus-{node}"))
                .spawn(move || {
                    // Sleeps until a message comes: dropping the encoder posts one.
                    while !stop.load(Ordering::Relaxed) {
                        let Some(msg) = bus.timed_pop(gst::ClockTime::NONE) else {
                            // The bus is flushing while the pipeline restarts: no message
                            // can come, so not a loop on the spot.
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            continue;
                        };
                        match msg.view() {
                            gst::MessageView::Error(e) => {
                                let text = format!("{} ({:?})", e.error(), e.debug());
                                // The source (an element taking nothing in) lost its
                                // resource (its stream, a file): not the encoder's
                                // doing. Not a stream error (not negotiated, the
                                // data flow stopped), which may well be the encoder's.
                                let by_source = e.error().is::<gst::ResourceError>()
                                    && msg
                                        .src()
                                        .and_then(|s| s.downcast_ref::<gst::Element>())
                                        .is_some_and(|e| e.sink_pads().is_empty());
                                if by_source {
                                    gate.failed.store(true, Ordering::Relaxed);
                                    let mut state = gate.state.lock().unwrap();
                                    if !state.0 {
                                        warn!("encoder for {node}: its source failed: {text}");
                                        state.1 = Some(text);
                                        gate.came.notify_all();
                                        break;
                                    }
                                    // After the start, once the stream flowed: its stream
                                    // is gone. Before its first frame it may as well be
                                    // the encoder that the source could not agree with
                                    // (a format, a modifier): that goes on as a failure.
                                    drop(state);
                                    if encoded.load(Ordering::Relaxed) {
                                        warn!("encoder for {node}: its source is gone: {text}");
                                        on_event(EncoderEvent::SourceLost(text));
                                        break;
                                    }
                                }
                                // The encoder (or its converter) before its first frame:
                                // it did not open, and another encoder may.
                                let by_encoder = msg
                                    .src()
                                    .is_some_and(|s| matches!(s.name().as_str(), "enc" | "enc_in"));
                                if by_encoder && !encoded.load(Ordering::Relaxed) {
                                    warn!("encoder for {node} did not open: {text}");
                                    let error = e.error();
                                    on_event(EncoderEvent::Unopened(NotOpened {
                                        why: error.to_string(),
                                        short: short_of_sessions(Some(&error), &text),
                                        resumed: resumed.load(Ordering::Relaxed),
                                    }));
                                } else {
                                    error!("encoder for {node}: {text}");
                                    on_event(EncoderEvent::Failed(text));
                                }
                                break;
                            }
                            gst::MessageView::Warning(w) => {
                                warn!("encoder for {node}: {}", w.error());
                            }
                            // Only a source ends the stream (the desktop's stream of
                            // the output ended); before the first frame, as above.
                            gst::MessageView::Eos(_) => {
                                let why = "stream ended".to_owned();
                                on_event(if encoded.load(Ordering::Relaxed) {
                                    EncoderEvent::SourceLost(why)
                                } else {
                                    EncoderEvent::Failed(why)
                                });
                                break;
                            }
                            _ => {}
                        }
                    }
                })?
        };

        let restart_on_resume = pipeline.by_name("src").is_none();
        // Built first, so a failed start below stops the bus thread and the pipeline
        // through Drop instead of leaking them.
        #[cfg(feature = "fake-desktop")]
        let fake_open = cfg.backend == Backend::Fake && probe::fake::opens();
        let encoder = Self {
            pipeline,
            sink,
            stop,
            paused: Arc::default(),
            restart_on_resume,
            takes_dmabuf: cfg.backend.takes_dmabuf(),
            bus_thread: Some(bus_thread),
            gate,
            kind: (cfg.backend, adapter),
            codec: cfg.codec,
            encoded,
            resumed,
            note: None,
            copies: false,
            devices,
            #[cfg(feature = "fake-desktop")]
            fake_open,
        };
        #[cfg(feature = "fake-desktop")]
        if fake_open {
            probe::fake::OPEN.fetch_add(1, Ordering::Relaxed);
        }
        let started = encoder.pipeline.set_state(gst::State::Playing);
        // A source that failed is its own failure, at once (the encoder is dropped
        // without waiting for it to stop: see `Drop`).
        let failed = {
            let mut state = encoder.gate.state.lock().unwrap();
            if started.is_err() && state.1.is_none() {
                state = encoder
                    .gate
                    .came
                    .wait_timeout_while(state, SOURCE_SAYS, |s| s.1.is_none())
                    .unwrap()
                    .0;
            }
            state.0 = true;
            state.1.take()
        };
        if let Some(why) = failed {
            return Err(SourceFailed(why).into());
        }
        started.context("cannot start encoder pipeline")?;
        // One more fake hardware encoder than it opens says so, as a real one does once
        // it has the first frame's format.
        #[cfg(feature = "fake-desktop")]
        if cfg.backend == Backend::Fake
            && !fake_open
            && let Some(enc) = encoder.pipeline.by_name("enc")
        {
            gst::element_error!(
                enc,
                gst::ResourceError::NoSpaceLeft,
                ("no more encoder sessions")
            );
        }
        Ok(encoder)
    }

    /// The encoder it runs, to count sessions by.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The codec it makes.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether it has encoded a frame (so it opened).
    pub fn opened(&self) -> bool {
        self.encoded.load(Ordering::Relaxed)
    }

    /// Whether it is paused now (the headset is off).
    pub fn is_paused(&self) -> bool {
        self.paused.is_on()
    }

    /// What to say of the encoder this output got (see [`note`]).
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Whether its frames go through the CPU on their way to it.
    pub fn copies(&self) -> bool {
        self.copies
    }

    /// What it was picked as for its output, and what to say of that.
    pub fn set_choice(&mut self, choice: &Choice, note: Option<String>) {
        self.copies = choice.copies();
        self.note = note;
    }

    /// The `appsrc` to push frames into, when the input has one.
    #[cfg_attr(
        feature = "fake-desktop",
        expect(dead_code, reason = "the fake desktop pushes no frames")
    )]
    pub fn appsrc(&self) -> Option<gst_app::AppSrc> {
        self.pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
    }

    /// Whether it takes frames straight from GPU buffers (dmabuf).
    #[cfg_attr(
        feature = "fake-desktop",
        expect(dead_code, reason = "the fake desktop pushes no frames")
    )]
    pub fn takes_dmabuf(&self) -> bool {
        self.takes_dmabuf
    }

    /// Set while the encoder is paused, so a capture can rest too.
    #[cfg_attr(
        feature = "fake-desktop",
        expect(dead_code, reason = "the fake desktop pushes no frames")
    )]
    pub fn paused(&self) -> Arc<Pause> {
        self.paused.clone()
    }

    /// Changes the target bitrate while running.
    pub fn set_bitrate(&self, kbps: u32) {
        if let Some(enc) = self.pipeline.by_name("enc") {
            enc.set_property("bitrate", kbps);
        }
    }

    /// Switches the VA-API speed/quality preset on the fly (no effect on x264).
    pub fn set_target_usage(&self, value: u32) {
        if let Some(enc) = self.pipeline.by_name("enc")
            && enc.find_property("target-usage").is_some()
        {
            enc.set_property("target-usage", value);
            self.force_keyframe();
        }
    }

    /// Stops capturing and encoding (headset not worn) or resumes with a fresh keyframe.
    pub fn set_paused(&self, paused: bool) {
        let state = if paused {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        self.paused.set(paused);
        if !paused {
            self.resumed.store(true, Ordering::Relaxed);
        }
        if !paused && self.restart_on_resume {
            // Resumed as it was, an idle screen would send no frame at all (the picture
            // in the headset stays frozen, or a headset that just connected gets no
            // monitors): connected again, the source sends the current picture at once.
            let _ = self.pipeline.set_state(gst::State::Null);
            // It opens again with the first frame.
            self.encoded.store(false, Ordering::Relaxed);
        }
        let _ = self.pipeline.set_state(state);
        if !paused {
            self.force_keyframe();
        }
    }

    /// Asks the encoder to emit an IDR frame (with parameter sets) as soon as possible.
    pub fn force_keyframe(&self) {
        request_keyframe(&self.sink);
    }

    /// A handle that asks it for a keyframe from any thread, while it runs.
    pub fn keyframer(&self) -> Keyframer {
        Keyframer {
            sink: self.sink.clone(),
            pause: self.paused.clone(),
        }
    }
}

/// Whether an encoder is paused, which a capture feeding it sleeps on.
#[derive(Debug, Default)]
pub struct Pause {
    on: Mutex<bool>,
    changed: std::sync::Condvar,
    /// A keyframe was asked of a still screen: a capture sends its last picture again
    /// at once, instead of at its next keepalive.
    again: AtomicBool,
}

impl Pause {
    /// Whether the capture is to send its last picture again now; it is asked once.
    #[cfg_attr(
        feature = "fake-desktop",
        expect(dead_code, reason = "the fake desktop pushes no frames")
    )]
    pub fn take_again(&self) -> bool {
        self.again.swap(false, Ordering::Relaxed)
    }

    pub fn is_on(&self) -> bool {
        *self.on.lock().unwrap()
    }

    fn set(&self, on: bool) {
        *self.on.lock().unwrap() = on;
        self.changed.notify_all();
    }

    /// Wakes whoever waits (a capture that is to stop).
    #[cfg_attr(
        all(feature = "fake-desktop", not(test)),
        expect(dead_code, reason = "the fake desktop pushes no frames")
    )]
    pub fn wake(&self) {
        let _on = self.on.lock().unwrap();
        self.changed.notify_all();
    }

    /// Sleeps while paused, until resumed or woken with `stop` set.
    #[cfg_attr(
        all(feature = "fake-desktop", not(test)),
        expect(dead_code, reason = "the fake desktop pushes no frames")
    )]
    pub fn wait(&self, stop: &AtomicBool) {
        let mut on = self.on.lock().unwrap();
        while *on && !stop.load(Ordering::Relaxed) {
            on = self.changed.wait(on).unwrap();
        }
    }
}

/// Asks one encoder for keyframes, from any thread (see [`Encoder::keyframer`]): a
/// headset's request does not wait for whoever owns the encoder. Asking an encoder
/// that stopped does nothing.
#[derive(Clone)]
pub struct Keyframer {
    sink: gst_app::AppSink,
    pause: Arc<Pause>,
}

impl Keyframer {
    pub fn ask(&self) {
        request_keyframe(&self.sink);
    }

    /// The source is to send its picture again at once (a keyframe asked of a still
    /// screen comes with the next frame): a capture pushing its own frames does so.
    pub fn picture_again(&self) {
        self.pause.again.store(true, Ordering::Relaxed);
    }
}

fn request_keyframe(sink: &gst_app::AppSink) {
    let event = gst_video::UpstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
    if let Some(pad) = sink.static_pad("sink") {
        pad.push_event(event);
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the bus thread, which then sees `stop`.
        if let Some(bus) = self.pipeline.bus() {
            let _ = bus.post(gst::message::Application::new(gst::Structure::new_empty(
                "prostir-stop",
            )));
        }
        // A PipeWire source whose stream failed waits up to 30 s for a format that
        // never comes before it stops (gst-plugin-pipewire 1.6, `wait_negotiated`):
        // stopped on a thread of its own, holding nobody up; the graphics devices go
        // to it once it runs, and after the pipeline stopped.
        let stopper = self.gate.failed.load(Ordering::Relaxed).then(|| {
            let pipeline = self.pipeline.clone();
            let (give, take) = std::sync::mpsc::channel::<gpu::Lease>();
            std::thread::Builder::new()
                .name("gst-stop".into())
                .spawn(move || {
                    let _ = pipeline.set_state(gst::State::Null);
                    drop(take.recv());
                })
                .map(|_| give)
        });
        match stopper {
            Some(Ok(give)) => {
                let _ = give.send(std::mem::take(&mut self.devices));
            }
            // No thread: stopped here, the devices after it (the fields drop later).
            _ => {
                let _ = self.pipeline.set_state(gst::State::Null);
            }
        }
        if let Some(t) = self.bus_thread.take() {
            let _ = t.join();
        }
        #[cfg(feature = "fake-desktop")]
        if self.fake_open {
            probe::fake::OPEN.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Time each frame spends in conversion + encoding, and gaps between captured frames.
#[derive(Default)]
struct Probe {
    /// The frames inside: when each came, and whether it repeats the picture before.
    inside: std::collections::VecDeque<(Instant, bool)>,
    /// The memory of the last frame that came (its address, 0: none).
    last_memory: usize,
    last_entered: Option<Instant>,
    max_gap_ms: f64,
    frames: u32,
    sum_ms: f64,
    max_ms: f64,
    since: Option<Instant>,
}

impl Probe {
    /// A frame in `memory` (its address; 0 when unknown) came.
    fn entered(&mut self, memory: usize) {
        let now = Instant::now();
        let repeat = memory != 0 && memory == self.last_memory;
        self.last_memory = memory;
        if let Some(prev) = self.last_entered.replace(now) {
            self.max_gap_ms = self.max_gap_ms.max((now - prev).as_secs_f64() * 1e3);
        }
        if self.inside.len() > 64 {
            self.inside.clear();
        }
        self.inside.push_back((now, repeat));
    }

    /// The next frame came out: how it went (`None` if it cannot be paired).
    fn left(&mut self, node: &str) -> Option<FrameTiming> {
        let (t0, repeat) = self.inside.pop_front()?;
        let took = t0.elapsed();
        let ms = took.as_secs_f64() * 1e3;
        self.frames += 1;
        self.sum_ms += ms;
        self.max_ms = self.max_ms.max(ms);
        if ms > 30.0 {
            debug!(
                "{node}: frame spent {ms:.0} ms in the encoder, {} still inside",
                self.inside.len()
            );
        }
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed().as_secs() >= 5 {
            debug!(
                "{node}: {} frames, encoder avg {:.1} max {:.1} ms, longest gap between captured frames {:.0} ms",
                self.frames,
                self.sum_ms / f64::from(self.frames),
                self.max_ms,
                self.max_gap_ms
            );
            let inside = std::mem::take(&mut self.inside);
            let (last_entered, last_memory) = (self.last_entered, self.last_memory);
            *self = Self {
                inside,
                last_memory,
                last_entered,
                ..Self::default()
            };
        }
        Some(FrameTiming {
            encode_us: u32::try_from(took.as_micros()).unwrap_or(u32::MAX),
            repeat,
        })
    }
}

/// Decodes one keyframe of a monitor's stream into a PNG in the Pictures folder (a
/// screenshot of just that monitor where the desktop's tool cannot take one output).
pub fn keyframe_to_png(data: &[u8], codec: Codec) -> Result<std::path::PathBuf> {
    // A new file, made empty for this picture: only one with the picture in it counts.
    let path = crate::platform::screenshot_path()?;
    match decode_to(data, codec, &path) {
        Ok(()) if crate::platform::has_picture(&path) => {
            info!("screenshot saved to {path:?}");
            Ok(path)
        }
        done => {
            let _ = std::fs::remove_file(&path);
            Err(done
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("the frame was decoded into nothing")))
        }
    }
}

/// Decodes the keyframe `data` into a PNG at `path`.
fn decode_to(data: &[u8], codec: Codec, path: &std::path::Path) -> Result<()> {
    let (caps, parse) = match codec {
        Codec::H264 => ("video/x-h264", "h264parse"),
        Codec::H265 => ("video/x-h265", "h265parse"),
        Codec::Av1 => ("video/x-av1", "av1parse"),
    };
    let pipeline = gst::parse::launch(&format!(
        "appsrc name=src caps={caps},stream-format=byte-stream,alignment=au ! {parse} ! \
         decodebin ! videoconvert ! pngenc snapshot=true ! filesink name=sink"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
    let sink = pipeline.by_name("sink").context("no filesink")?;
    sink.set_property("location", path.to_string_lossy().as_ref());
    let src = pipeline
        .by_name("src")
        .context("no appsrc")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| anyhow::anyhow!("not an appsrc"))?;
    pipeline.set_state(gst::State::Playing)?;
    let pushed = src
        .push_buffer(gst::Buffer::from_slice(data.to_vec()))
        .map_err(|e| anyhow::anyhow!("{e:?}"))
        .and_then(|_| src.end_of_stream().map_err(|e| anyhow::anyhow!("{e:?}")));
    let bus = pipeline.bus().context("no bus")?;
    let ended = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(5),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);
    pushed?;
    match ended.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(()),
        Some(gst::MessageView::Error(e)) => bail!("{}", e.error()),
        _ => bail!("the frame was not decoded in time"),
    }
}

#[cfg(test)]
mod pause {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::Pause;

    /// A capture sleeps on its encoder's pause until it is resumed, or woken to stop,
    /// and not at all while it is not paused. What is seen is whether a sleeper ended,
    /// never how long it took: a busy machine starts a thread late.
    #[test]
    fn a_pause_is_slept_on_until_resumed_or_woken() {
        let pause = Arc::new(Pause::default());
        let stop = Arc::new(AtomicBool::new(false));
        let sleeper = |pause: &Arc<Pause>, stop: &Arc<AtomicBool>| {
            let (pause, stop) = (pause.clone(), stop.clone());
            std::thread::spawn(move || pause.wait(&stop))
        };
        let ends = |sleeper: std::thread::JoinHandle<()>| {
            let until = Instant::now() + Duration::from_secs(10);
            while !sleeper.is_finished() {
                if Instant::now() > until {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            sleeper.join().is_ok()
        };
        assert!(ends(sleeper(&pause, &stop)), "not paused: no sleep");
        pause.set(true);
        let resumed = sleeper(&pause, &stop);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!resumed.is_finished(), "paused: it sleeps");
        pause.set(false);
        assert!(ends(resumed), "resumed: it wakes");
        pause.set(true);
        let stopped = sleeper(&pause, &stop);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!stopped.is_finished(), "paused: it sleeps");
        stop.store(true, Ordering::Relaxed);
        pause.wake();
        assert!(ends(stopped), "woken to stop");
        assert!(pause.is_on(), "still paused, only woken");
    }
}

#[cfg(test)]
mod tests {
    use gstreamer as gst;
    use gstreamer::prelude::*;

    /// A source that fails as the encoder starts (a file that is not there here;
    /// KWin's node gone on KDE): the start says so at once as the source's failure,
    /// and the encoder is not told it failed (which would end the session).
    #[test]
    fn a_failed_source_is_the_starts_own_failure() {
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        use super::{
            Backend, Encoder, EncoderConfig, EncoderEvent, GpuDevices, Input, SourceFailed,
        };
        gst::init().unwrap();
        if gst::ElementFactory::find("x264enc").is_none() {
            eprintln!("skipped: no x264enc");
            return;
        }
        let input = Input {
            source: "filesrc location=/nonexistent/prostir-test ! \
                     rawvideoparse width=64 height=64 format=i420"
                .into(),
            label: "test".into(),
            frame_rate: None,
            live: false,
        };
        let cfg = EncoderConfig {
            codec: prostir_proto::Codec::H264,
            bitrate_kbps: 1000,
            backend: Backend::X264,
            sharp: false,
        };
        let told = Arc::new(Mutex::new(0));
        let kept = told.clone();
        let started = Instant::now();
        let result = Encoder::start(&input, &cfg, None, &GpuDevices::default(), move |e| {
            if matches!(
                e,
                EncoderEvent::Failed(_) | EncoderEvent::SourceLost(_) | EncoderEvent::Unopened(_)
            ) {
                *kept.lock().unwrap() += 1;
            }
            false
        });
        let Err(e) = result else {
            panic!("started on a file that is not there")
        };
        assert!(e.downcast_ref::<SourceFailed>().is_some(), "{e:#}");
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(*told.lock().unwrap(), 0, "not told as the encoder's");
    }

    /// Told live, the answer to the latency query that passes the pad says live,
    /// whatever the source said, with its latencies; without, as the source says.
    #[test]
    fn a_source_told_live_is_live_to_the_encoder() {
        gst::init().unwrap();
        let asked = |live: bool| {
            let pipeline =
                gst::parse::launch("videotestsrc ! identity name=told ! queue name=q ! fakesink")
                    .unwrap()
                    .downcast::<gst::Pipeline>()
                    .unwrap();
            if live {
                let pad = pipeline.by_name("told").unwrap().static_pad("src").unwrap();
                super::tell_live(&pad);
            }
            pipeline.set_state(gst::State::Paused).unwrap();
            let _ = pipeline.state(gst::ClockTime::from_seconds(5));
            let mut q = gst::query::Latency::new();
            let sink = pipeline.by_name("q").unwrap().static_pad("sink").unwrap();
            assert!(sink.peer_query(&mut q));
            pipeline.set_state(gst::State::Null).unwrap();
            q.result().0
        };
        assert!(!asked(false), "videotestsrc is not live");
        assert!(asked(true));
    }

    /// Caps that say no frame rate, or 0, get the one told; a rate said stays.
    #[test]
    fn a_frame_rate_is_told_where_none_is_said() {
        gst::init().unwrap();
        let rate = gst::Fraction::new(60, 1);
        let told = |caps: &str| {
            super::with_frame_rate(&caps.parse::<gst::Caps>().unwrap(), rate).map(|c| c.to_string())
        };
        assert_eq!(
            told("video/x-raw,format=BGRx,framerate=0/1").as_deref(),
            Some("video/x-raw, format=(string)BGRx, framerate=(fraction)60/1")
        );
        assert_eq!(
            told("video/x-raw(memory:DMABuf),format=DMA_DRM").as_deref(),
            Some("video/x-raw(memory:DMABuf), format=(string)DMA_DRM, framerate=(fraction)60/1")
        );
        assert_eq!(told("video/x-raw,framerate=30/1"), None);
    }

    /// Frames from a source that says no frame rate reach what comes after the pad
    /// with the rate told, and the source is still asked for what comes after (not
    /// "any").
    #[test]
    fn the_frames_after_the_pad_come_at_the_rate_told() {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "videotestsrc name=source num-buffers=1 ! \
             video/x-raw,format=I420,width=64,height=64,framerate=0/1 ! \
             identity name=told ! queue ! video/x-raw,format=I420 ! appsink name=out",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let told = pipeline.by_name("told").unwrap();
        super::tell_frame_rate(&told.static_pad("src").unwrap(), gst::Fraction::new(60, 1));
        let asked = told.static_pad("src").unwrap().peer_query_caps(None);
        assert!(!asked.is_any());
        let out = pipeline
            .by_name("out")
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let sample = out.pull_sample().unwrap();
        pipeline.set_state(gst::State::Null).unwrap();
        let rate = sample
            .caps()
            .and_then(|c| c.structure(0))
            .and_then(|s| s.get::<gst::Fraction>("framerate").ok());
        assert_eq!(rate, Some(gst::Fraction::new(60, 1)));
    }

    /// A frame that passes the time code's pad has the time it was captured drawn
    /// into it, on the PC's monotonic clock, as the headset reads it back.
    #[test]
    fn the_time_code_is_drawn_into_a_frame() {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "videotestsrc num-buffers=1 is-live=true pattern=black ! \
             video/x-raw,format=I420,width=704,height=32 ! identity name=timecode ! \
             appsink name=out",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let pad = pipeline
            .by_name("timecode")
            .unwrap()
            .static_pad("src")
            .unwrap();
        super::draw_time_code(&pad);
        let out = pipeline
            .by_name("out")
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let sample = out.pull_sample().unwrap();
        let now_ms = super::monotonic_ns() / 1_000_000;
        pipeline.set_state(gst::State::Null).unwrap();
        let map = sample.buffer().unwrap().map_readable().unwrap();
        // I420 of 704 pixels across: the luma rows are 704 bytes.
        let code = prostir_proto::timecode::read(map.as_slice(), 704, 704, 32)
            .expect("a time code in the frame");
        let late = (now_ms as u32).wrapping_sub(code);
        assert!(late < 2000, "captured {late} ms ago");
    }
}

#[cfg(test)]
mod live {
    /// A keyframe from the graphics card's encoder becomes a PNG (run by hand:
    /// `cargo test -- --ignored keyframe`; needs the VA plugin and a GPU).
    #[test]
    #[ignore = "by hand: needs the VA plugin and a graphics card"]
    fn keyframe_becomes_png() {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "videotestsrc num-buffers=1 ! video/x-raw,width=640,height=360 ! vapostproc ! \
             vah265enc ! h265parse config-interval=-1 ! \
             video/x-h265,stream-format=byte-stream,alignment=au ! appsink name=out",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let out = pipeline
            .by_name("out")
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let sample = out.pull_sample().unwrap();
        let data = sample.buffer().unwrap().map_readable().unwrap().to_vec();
        pipeline.set_state(gst::State::Null).unwrap();
        let path = super::keyframe_to_png(&data, prostir_proto::Codec::H265).unwrap();
        let png = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        eprintln!("decoded {} bytes into a {} byte PNG", data.len(), png.len());
    }

    /// Encoder latency with several streams on VA-API, each pipeline with a VA display
    /// of its own or all sharing one ([`super::GpuDevices`]). Run by hand on a quiet PC:
    /// `cargo test -p prostir -- --ignored va_display_sharing --nocapture`
    /// (`PROSTIR_MEASURE_SECS`, `PROSTIR_MEASURE_SIZE=2560x1440`,
    /// `PROSTIR_MEASURE_STREAMS=4,6`); needs VA-API.
    ///
    /// Frames come as GPU buffers made by a producer with a display of its own (as KWin
    /// makes them in its own process), pushed into an `appsrc` (as on Hyprland), and go
    /// through the same pipeline a stream uses. The time is from the frame entering the
    /// converter to its access unit leaving the parser.
    // GPU buffers as the real desktops hand them over (`gstreamer-allocators`).
    #[cfg(feature = "desktop")]
    #[test]
    #[ignore = "by hand: a measurement on a graphics card"]
    fn va_display_sharing() {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        use gstreamer as gst;
        use gstreamer::prelude::*;
        use gstreamer_app as gst_app;

        use super::{Backend, EncoderConfig, GpuDevices, Input, pipeline_description};

        gst::init().unwrap();
        let secs: u64 = std::env::var("PROSTIR_MEASURE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        let size = std::env::var("PROSTIR_MEASURE_SIZE").unwrap_or_else(|_| "2560x1440".into());
        let (w, h) = size.split_once('x').unwrap();
        let percentile = |v: &mut Vec<f64>, p: f64| {
            v.sort_by(f64::total_cmp);
            v.get(((v.len() as f64 - 1.0) * p).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        // Shared and not in turn, so a change of the PC's load falls on both.
        let counts: Vec<usize> = std::env::var("PROSTIR_MEASURE_STREAMS")
            .unwrap_or_else(|_| "4,6".into())
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        for n in counts {
            for shared in [false, true, false, true] {
                let gpu = GpuDevices::sharing_va();
                let times: Arc<Mutex<Vec<f64>>> = Arc::default();
                let mut running = Vec::new();
                for i in 0..n {
                    let producer = gst::parse::launch(&format!(
                        "videotestsrc is-live=true pattern=ball ! \
                         video/x-raw,format=BGRx,width=320,height=180,framerate=60/1 ! \
                         vapostproc ! video/x-raw(memory:DMABuf),width={w},height={h} ! \
                         appsink name=out sync=false max-buffers=2 drop=true"
                    ))
                    .unwrap()
                    .downcast::<gst::Pipeline>()
                    .unwrap();
                    let input = Input {
                        source: "appsrc name=src is-live=true format=time do-timestamp=true \
                                 max-buffers=2 leaky-type=downstream"
                            .into(),
                        label: format!("measure {i}"),
                        frame_rate: None,
                        live: false,
                    };
                    let cfg = EncoderConfig {
                        codec: prostir_proto::Codec::H265,
                        bitrate_kbps: 20_000,
                        backend: Backend::Va,
                        sharp: false,
                    };
                    let encoder = gst::parse::launch(&pipeline_description(&input, &cfg).unwrap())
                        .unwrap()
                        .downcast::<gst::Pipeline>()
                        .unwrap();
                    let lease = shared.then(|| gpu.share_with(&encoder));
                    let src = encoder
                        .by_name("src")
                        .unwrap()
                        .downcast::<gst_app::AppSrc>()
                        .unwrap();
                    let out = producer
                        .by_name("out")
                        .unwrap()
                        .downcast::<gst_app::AppSink>()
                        .unwrap();
                    // GPU buffers go only where frames carry their layout (video meta),
                    // which an appsink does not say it reads.
                    out.static_pad("sink").unwrap().add_probe(
                        gst::PadProbeType::QUERY_DOWNSTREAM,
                        |_, info| {
                            if let Some(gst::PadProbeData::Query(query)) = &mut info.data
                                && let gst::QueryViewMut::Allocation(a) = query.view_mut()
                            {
                                a.add_allocation_meta::<gstreamer_video::VideoMeta>(None);
                            }
                            gst::PadProbeReturn::Ok
                        },
                    );
                    // Handed on as plain GPU buffers (the file of each, as PipeWire hands
                    // KWin's over), not as the producer's VA surfaces: one per buffer
                    // of its pool, made once, so the encoder imports each once.
                    let allocator = gstreamer_allocators::DmaBufAllocator::new();
                    let mut plain: std::collections::HashMap<i32, gst::Buffer> =
                        std::collections::HashMap::new();
                    out.set_callbacks(
                        gst_app::AppSinkCallbacks::builder()
                            .new_sample(move |sink| {
                                use gstreamer_allocators::prelude::*;
                                use std::os::fd::FromRawFd;
                                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                                if src.caps().is_none() {
                                    src.set_caps(sample.caps().map(ToOwned::to_owned).as_ref());
                                }
                                let buffer = sample.buffer().unwrap();
                                let memory = buffer.peek_memory(0);
                                let dmabuf = memory
                                    .downcast_memory_ref::<gstreamer_allocators::DmaBufMemory>()
                                    .unwrap();
                                let wrapped = plain.entry(dmabuf.fd()).or_insert_with(|| {
                                    // SAFETY: a copy of a file the producer's buffer holds,
                                    // owned by the memory made of it from here on.
                                    let fd = unsafe {
                                        std::os::fd::OwnedFd::from_raw_fd(libc::dup(dmabuf.fd()))
                                    };
                                    // SAFETY: `fd` is a dmabuf of at least this size.
                                    let memory =
                                        unsafe { allocator.alloc_dmabuf(fd, memory.maxsize()) }
                                            .unwrap();
                                    let mut copy = gst::Buffer::new();
                                    let meta = buffer.meta::<gstreamer_video::VideoMeta>().unwrap();
                                    {
                                        let copy = copy.get_mut().unwrap();
                                        copy.append_memory(memory);
                                        gstreamer_video::VideoMeta::add_full(
                                            copy,
                                            meta.video_frame_flags(),
                                            meta.format(),
                                            meta.width(),
                                            meta.height(),
                                            meta.offset(),
                                            meta.stride(),
                                        )
                                        .unwrap();
                                    }
                                    copy
                                });
                                let _ = src.push_buffer(wrapped.copy());
                                Ok(gst::FlowSuccess::Ok)
                            })
                            .build(),
                    );
                    let inside: Arc<Mutex<VecDeque<Instant>>> = Arc::default();
                    {
                        let inside = inside.clone();
                        let pad = encoder
                            .by_name("enc_in")
                            .unwrap()
                            .static_pad("sink")
                            .unwrap();
                        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                            inside.lock().unwrap().push_back(Instant::now());
                            gst::PadProbeReturn::Ok
                        });
                    }
                    let sink = encoder
                        .by_name("sink")
                        .unwrap()
                        .downcast::<gst_app::AppSink>()
                        .unwrap();
                    let times = times.clone();
                    sink.set_callbacks(
                        gst_app::AppSinkCallbacks::builder()
                            .new_sample(move |sink| {
                                let _ = sink.pull_sample();
                                let t0 = inside.lock().unwrap().pop_front();
                                if let Some(t0) = t0 {
                                    times.lock().unwrap().push(t0.elapsed().as_secs_f64() * 1e3);
                                }
                                Ok(gst::FlowSuccess::Ok)
                            })
                            .build(),
                    );
                    encoder.set_state(gst::State::Playing).unwrap();
                    producer.set_state(gst::State::Playing).unwrap();
                    running.push((producer, encoder, lease));
                }
                // The first second is the encoders opening.
                std::thread::sleep(Duration::from_secs(1));
                times.lock().unwrap().clear();
                std::thread::sleep(Duration::from_secs(secs));
                let mut t = std::mem::take(&mut *times.lock().unwrap());
                for (producer, encoder, _) in &running {
                    let _ = producer.set_state(gst::State::Null);
                    let _ = encoder.set_state(gst::State::Null);
                }
                let fps = t.len() as f64 / secs as f64 / n as f64;
                let mean = t.iter().sum::<f64>() / t.len().max(1) as f64;
                eprintln!(
                    "{n} streams {size}, {}: {fps:.1} frames/s each, mean {mean:.2} p50 {:.2} \
                     p95 {:.2} p99 {:.2} max {:.2} ms",
                    if shared {
                        "one shared VA display"
                    } else {
                        "a VA display each"
                    },
                    percentile(&mut t, 0.5),
                    percentile(&mut t, 0.95),
                    percentile(&mut t, 0.99),
                    percentile(&mut t, 1.0),
                );
                drop(running);
            }
        }
    }
}
