//! PC sound to the headset: a virtual output device ("Prostir — шолом") that is the
//! default while the headset is connected; whatever plays into it is captured as raw
//! PCM and sent over the (encrypted) connection. And back: the headset's microphone
//! as the PC's default input ([`MicIn`]). The level of that output device is the
//! shared volume ([`SinkVolume`]), and one `pactl subscribe` tells when it moves and
//! when a program records from the microphone ([`watch`]).

use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use log::{info, warn};
use prostir_proto::{AUDIO_CHANNELS, AUDIO_RATE, MIC_CHANNELS};

use crate::events::EventSink;
use crate::platform::process::spawn_tied;
use crate::platform::pulse::{self, MIC, Pactl, SINK, TIMEOUT, pactl, state_in};
use crate::platform::run;
use crate::platform::sound::{OutputDevice, Route};

pub use crate::platform::pulse::SinkVolume;
pub use crate::platform::sound_events::Watcher;

/// Follows the sound system while Prostir's devices exist (the headset's output device
/// and the headset microphone): their changes go to `events`.
pub fn watch(events: EventSink) -> Option<Watcher> {
    Watcher::start(Pactl::headset(), events)
}

/// Finds out, off the control loop, whether the PC can share its volume: `pw-cli`
/// makes the device, `pactl` reads and sets it.
pub fn prepare() {
    pulse::prepare(&["pw-cli"]);
}

/// A PipeWire node created with `pw-cli`; destroyed on drop. WirePlumber keeps
/// nothing of it (`state.restore-props=false`): the headset owns the level of its
/// sound device, which a remembered volume or mute would only fight, and a node of a
/// test leaves nothing behind.
///
/// It is destroyed as the node it was made as (its number and serial), never by its
/// name: one let go late (the sound system was slow) leaves alone the node made
/// after it under the same name.
struct VirtualNode {
    name: &'static str,
    /// Its number, which PipeWire gives to another node once this one is gone.
    id: u32,
    /// Its serial, which PipeWire never gives again (none from a PipeWire too old to
    /// have one).
    serial: Option<u64>,
}

impl VirtualNode {
    fn create(name: &'static str, description: &str, class: &str) -> Result<Self> {
        destroy_node(name); // left over from a crash
        let props = node_props(name, description, class);
        let out = run(
            Command::new("pw-cli").args(["create-node", "adapter", &props]),
            TIMEOUT,
        )
        .context("pw-cli is needed for sound (PipeWire)")?;
        if !out.status.success() {
            bail!("pw-cli: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        // The node shows up asynchronously.
        for _ in 0..20 {
            if let Some(node) = node_info(name) {
                return Ok(Self {
                    name,
                    id: node.id,
                    serial: node.serial,
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("PipeWire did not create {name}")
    }

    /// Whether the node with its number is still this one.
    fn is_there(&self) -> bool {
        node_info(&self.id.to_string()).is_some_and(|n| match self.serial {
            Some(serial) => n.serial == Some(serial),
            None => n.name.as_deref() == Some(self.name),
        })
    }
}

/// What `pw-cli create-node adapter` makes a node of.
fn node_props(name: &str, description: &str, class: &str) -> String {
    format!(
        "{{ factory.name=support.null-audio-sink node.name={name} \
         node.description=\"{description}\" media.class={class} \
         audio.position=[ FL FR ] audio.rate={AUDIO_RATE} object.linger=true \
         state.restore-props=false }}"
    )
}

impl Drop for VirtualNode {
    fn drop(&mut self) {
        if self.is_there() {
            let _ = run(
                Command::new("pw-cli").args(["destroy", &self.id.to_string()]),
                TIMEOUT,
            );
        }
    }
}

/// A PipeWire object as `pw-cli info` tells it.
#[derive(Debug, PartialEq, Eq)]
struct NodeInfo {
    id: u32,
    serial: Option<u64>,
    name: Option<String>,
}

/// The object `target` (a name or a number), if there is one.
fn node_info(target: &str) -> Option<NodeInfo> {
    let out = run(Command::new("pw-cli").args(["info", target]), TIMEOUT).ok()?;
    parse_info(&String::from_utf8_lossy(&out.stdout))
}

/// What `pw-cli info` printed: `id: 52`, then `object.serial = "57940"` and
/// `node.name = "…"` among the properties (a `*` before those that changed).
fn parse_info(text: &str) -> Option<NodeInfo> {
    let id = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("id: ")?.trim().parse().ok())?;
    let property = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.trim().trim_start_matches('*').split_once(" = ")?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
        })
    };
    Some(NodeInfo {
        id,
        serial: property("object.serial").and_then(|s| s.parse().ok()),
        name: property("node.name"),
    })
}

fn node_id(name: &str) -> Option<u32> {
    node_info(name).map(|n| n.id)
}

fn destroy_node(name: &str) {
    if let Some(id) = node_id(name) {
        let _ = run(
            Command::new("pw-cli").args(["destroy", &id.to_string()]),
            TIMEOUT,
        );
    }
}

/// Removes the sound devices a previous run (killed, crashed) left behind. The sink
/// lingers by design, and while it is gone PipeWire falls back to a real output.
pub fn restore_after_crash() {
    for name in [SINK, MIC, MIC_FEED] {
        destroy_node(name);
    }
}

/// Sound going to the headset while this exists.
pub struct AudioOut {
    pipeline: gst::Pipeline,
    /// The capture, and the device it is to capture (the sink's monitor).
    capture: gst::Element,
    monitor: String,
    /// The capture stopped (an error, the end of its stream) or moved to another
    /// device: the sound no longer reaches the headset.
    failed: Arc<AtomicBool>,
    previous_default: Option<String>,
    // Dropped last: the device disappears after capture stops.
    _sink: VirtualNode,
}

impl AudioOut {
    /// `on_sound` gets each slice of PC sound (raw PCM) as it is captured.
    pub fn start(on_sound: impl Fn(Vec<u8>) + Send + Sync + 'static) -> Result<Self> {
        Self::start_as(
            SINK,
            crate::i18n::t("Prostir — шолом", "Prostir headset"),
            true,
            on_sound,
        )
    }

    /// The sound device `name`; with `make_default`, the PC's default output (and the
    /// old default comes back when it goes).
    fn start_as(
        name: &'static str,
        description: &str,
        make_default: bool,
        on_sound: impl Fn(Vec<u8>) + Send + Sync + 'static,
    ) -> Result<Self> {
        let sink = VirtualNode::create(name, description, "Audio/Sink")?;
        let previous_default = make_default
            .then(|| pactl(&["get-default-sink"]))
            .flatten()
            .filter(|s| s != name);
        let monitor = format!("{name}.monitor");
        // Low latency: 10 ms slices, at most 40 ms buffered on the PC.
        let desc = format!(
            "pulsesrc name=capture device={monitor} buffer-time=40000 latency-time=10000 \
             ! audio/x-raw,format=S16LE,rate={AUDIO_RATE},channels={AUDIO_CHANNELS} \
             ! appsink name=sink sync=false max-buffers=8 drop=true"
        );
        let pipeline = gst::parse::launch(&desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let capture = pipeline
            .by_name("capture")
            .ok_or_else(|| anyhow!("pulsesrc missing"))?;
        let failed = Arc::new(AtomicBool::new(false));
        // The device removed (the sound system restarted, or someone removed it), the
        // capture says nothing of it: it moves to another device, the PC's speakers.
        let moved = failed.clone();
        let ours = monitor.clone();
        capture.connect_notify(Some("current-device"), move |capture, _| {
            let now = capture.property::<Option<String>>("current-device");
            if now.as_deref().is_some_and(|d| !d.is_empty() && d != ours)
                && !moved.swap(true, Ordering::Relaxed)
            {
                warn!("PC sound: the capture moved to {now:?}: {ours} is gone");
            }
        });
        // Nothing else reads the bus: its messages are looked at as they are posted,
        // and none waits there.
        let stopped = failed.clone();
        pipeline
            .bus()
            .ok_or_else(|| anyhow!("pipeline without bus"))?
            .set_sync_handler(move |_, msg| {
                let why = match msg.view() {
                    gst::MessageView::Error(e) => Some(e.error().to_string()),
                    gst::MessageView::Eos(_) => Some("the capture ended".to_owned()),
                    _ => None,
                };
                if let Some(why) = why
                    && !stopped.swap(true, Ordering::Relaxed)
                {
                    warn!("PC sound to the headset stopped: {why}");
                }
                gst::BusSyncReply::Drop
            });
        let appsink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| anyhow!("appsink missing"))?;
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(buffer) = sample.buffer()
                        && let Ok(map) = buffer.map_readable()
                    {
                        on_sound(map.as_slice().to_vec());
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing)?;
        // Like plugging in headphones: sound follows the headset.
        if make_default && pactl(&["set-default-sink", name]).is_none() {
            warn!("cannot make {name} the default output");
        }
        info!("PC sound goes to the headset");
        Ok(Self {
            pipeline,
            capture,
            monitor,
            failed,
            previous_default,
            _sink: sink,
        })
    }

    /// Whether the sound still reaches the headset: the capture runs, from the
    /// headset's device. Looked at now and then: when the sound system goes away
    /// nothing else tells (see `worker/sound.rs`).
    pub fn alive(&self) -> bool {
        if self.failed.load(Ordering::Relaxed) {
            return false;
        }
        // Not known yet right after the start: as good as ours.
        let now = self.capture.property::<Option<String>>("current-device");
        now.is_none_or(|d| d.is_empty() || d == self.monitor)
    }
}

impl AudioOut {
    /// Where the sound plays: only in the headset, its device being the default.
    pub fn route(&self) -> Route {
        Route::HeadsetOnly
    }

    /// The device the sound is captured from (its monitor, before its volume).
    pub fn device(&self) -> OutputDevice {
        OutputDevice(SINK.into())
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        // Give the sound back to the PC, unless the user picked something else meanwhile.
        if let Some(prev) = &self.previous_default
            && pactl(&["get-default-sink"]).as_deref() == Some(SINK)
        {
            let _ = pactl(&["set-default-sink", prev]);
        }
        let _ = self.pipeline.set_state(gst::State::Null);
        info!("PC sound back on the PC");
    }
}

const MIC_FEED: &str = "prostir_mic_feed";

/// The longest the headset's sound waits in the feed stream (`pacat`'s latency).
const FEED_LATENCY_MS: u32 = 20;
/// The pipe to `pacat`: one page, about 40 ms of sound. Writes of up to this size go
/// through whole or not at all.
const FEED_PIPE: usize = 4096;
/// At most this much waits in the pipe, three pieces of 10 ms (a burst that a Wi-Fi
/// card saving power held back comes whole): when more waits, the PC records slower
/// than the headset sends, and new sound is dropped instead of piling up. (A pipe
/// holds a page at least, so the limit is looked at on each write; a piece into an
/// empty pipe always goes, whatever its size.)
const FEED_QUEUED: usize = 3 * 960;

/// The headset microphone as a PC microphone ("Prostir — мікрофон шолома"), the
/// default input while the headset streams. A `pw-loopback` pairs an internal input
/// with the microphone apps record from; while an app records, a `pacat` plays the
/// headset's sound into that input.
///
/// No sound comes out late: each recording gets a new `pacat`, so nothing from an
/// earlier one comes out and nothing waits in between, and during a recording at most
/// ~60 ms waits on the PC. The internal input is always at full volume and never
/// muted: WirePlumber does not remember its settings (it used to, keyed by its name,
/// so a mute once set silenced the microphone on every later run), and they are set
/// again at the start of each recording.
pub struct MicIn {
    /// None once closed.
    loopback: Mutex<Option<Child>>,
    mic: String,
    feed_sink: String,
    /// The way in while an app records; none in between.
    feed: Mutex<Option<Feed>>,
    previous_default: Mutex<Option<String>>,
}

impl MicIn {
    pub fn start() -> Result<Self> {
        Self::start_as(MIC, MIC_FEED, true)
    }

    /// `make_default`: the microphone becomes the PC's default input (and the old
    /// default comes back when it goes).
    fn start_as(mic: &str, feed: &str, make_default: bool) -> Result<Self> {
        // Needed for each recording; missing, the microphone waits and tries later.
        run(Command::new("pacat").arg("--version"), TIMEOUT)
            .context("pacat is needed for the microphone")?;
        let mut loopback = spawn_tied(pw_loopback(mic, feed))
            .context("pw-loopback is needed for the microphone")?;
        let ready = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            source_state(mic).is_some()
        });
        if !ready {
            let _ = loopback.kill();
            let _ = loopback.wait();
            bail!("the virtual microphone did not appear");
        }
        let previous_default = if make_default {
            let prev = pactl(&["get-default-source"]).filter(|s| s != mic);
            if pactl(&["set-default-source", mic]).is_none() {
                warn!("cannot make {mic} the default microphone");
            }
            prev
        } else {
            None
        };
        info!("headset microphone is the PC's microphone");
        Ok(Self {
            loopback: Mutex::new(Some(loopback)),
            mic: mic.to_owned(),
            feed_sink: feed.to_owned(),
            feed: Mutex::new(None),
            previous_default: Mutex::new(previous_default),
        })
    }

    /// Whether the microphone is still there: its `pw-loopback` runs. PipeWire removes
    /// its devices when it ends, and it ends when the sound system goes away or one of
    /// its devices is removed, so it runs exactly while the microphone is there.
    /// Looked at now and then: nothing else tells (see `worker/sound.rs`).
    pub fn alive(&self) -> bool {
        self.loopback
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|l| matches!(l.try_wait(), Ok(None)))
    }

    /// Lets the microphone go, for every share of it (the default input back as it
    /// was, the loopback stopped); what is left to do when the last share goes is
    /// nothing. The sound thread closes one that broke, so the control loop's share
    /// runs nothing of this.
    pub fn close(&self) {
        let previous = self.previous_default.lock().unwrap().take();
        if let Some(prev) = previous
            && pactl(&["get-default-source"]).as_deref() == Some(self.mic.as_str())
        {
            let _ = pactl(&["set-default-source", &prev]);
        }
        drop(self.feed.lock().unwrap().take());
        let loopback = self.loopback.lock().unwrap().take();
        if let Some(mut loopback) = loopback {
            let _ = loopback.kill();
            let _ = loopback.wait();
            info!("microphone back on the PC");
        }
    }

    /// Plays a slice of the headset's microphone into the PC microphone, if an app
    /// records; dropped otherwise.
    pub fn push(&self, pcm: &[u8]) {
        let mut feed = self.feed.lock().unwrap();
        if let Some(f) = feed.as_mut()
            && !f.write(pcm)
        {
            warn!("headset microphone: pacat stopped");
            *feed = None;
        }
    }

    /// Whether some PC app records from the microphone, as the sound system said (see
    /// [`watch`]). A recording that begins gets a fresh way in; one that ends takes
    /// whatever still waits with it. Said again while it records, a way in that broke
    /// (`pacat` stopped) is made anew.
    ///
    /// The headset's sound keeps coming meanwhile ([`MicIn::push`], from another
    /// thread): the way in is made without holding it up, and only put in place then.
    pub fn set_recording(&self, on: bool) {
        if !on {
            let old = self.feed.lock().unwrap().take();
            drop(old);
            return;
        }
        if self.feed.lock().unwrap().is_some() {
            return;
        }
        // Full volume, not muted: whatever someone set on the internal input.
        let _ = pactl(&["set-sink-mute", &self.feed_sink, "0"]);
        let _ = pactl(&["set-sink-volume", &self.feed_sink, "100%"]);
        let new = Feed::start(&self.feed_sink)
            .inspect_err(|e| warn!("no headset microphone: {e:#}"))
            .ok();
        let mut feed = self.feed.lock().unwrap();
        if feed.is_none() {
            *feed = new;
        }
    }
}

impl Drop for MicIn {
    fn drop(&mut self) {
        self.close();
    }
}

/// One recording's way in: `pacat` plays what comes through a small pipe into the
/// loopback's internal input. Its stream waits for sound after a gap (before the
/// headset's microphone comes on, a Wi-Fi hiccup). GStreamer's `pulsesink`, used
/// before, asks for one that never waits: the sound system skips ahead over the gap,
/// and about as much of the sound that comes next was thrown away (up to half a
/// second at the start of a recording).
struct Feed {
    pacat: Child,
    pipe: std::process::ChildStdin,
    /// Bytes dropped because the pipe was full.
    dropped: usize,
}

impl Feed {
    fn start(sink: &str) -> Result<Self> {
        Self::spawn(pacat(sink)).context("pacat is needed for the microphone")
    }

    /// Starts `cmd`, which plays what comes on its standard input.
    fn spawn(mut cmd: Command) -> Result<Self> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut pacat = spawn_tied(cmd)?;
        let pipe = pacat.stdin.take().ok_or_else(|| anyhow!("no pipe"))?;
        let fd = pipe.as_raw_fd();
        // SAFETY: fcntl on a pipe this process owns.
        unsafe {
            libc::fcntl(fd, libc::F_SETPIPE_SZ, FEED_PIPE as libc::c_int);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Ok(Self {
            pacat,
            pipe,
            dropped: 0,
        })
    }

    /// `false` once `pacat` is gone.
    fn write(&mut self, pcm: &[u8]) -> bool {
        use std::io::Write;
        // While as much waits in the pipe as may, nothing is written, so no write error
        // would tell that `pacat` stopped (and one that stopped leaves its pipe full).
        if let Ok(Some(_)) = self.pacat.try_wait() {
            return false;
        }
        // Whole samples only, in pieces the pipe takes whole or not at all.
        for piece in pcm[..pcm.len() & !1].chunks(FEED_PIPE) {
            let mut queued: libc::c_int = 0;
            // SAFETY: FIONREAD into an int, on a pipe this process owns.
            let asked =
                unsafe { libc::ioctl(self.pipe.as_raw_fd(), libc::FIONREAD, &raw mut queued) };
            if asked == 0 && queued > 0 && queued as usize + piece.len() > FEED_QUEUED {
                self.dropped += piece.len();
                continue;
            }
            match self.pipe.write(piece) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.dropped += piece.len();
                }
                Err(_) => return false,
            }
        }
        true
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let _ = self.pacat.kill();
        let _ = self.pacat.wait();
        if self.dropped > 0 {
            let ms = self.dropped * 1000 / (AUDIO_RATE as usize * 2 * MIC_CHANNELS as usize);
            info!("headset microphone: {ms} ms dropped (the PC took it slower than it came)");
        }
    }
}

/// `pw-loopback` making the microphone `mic` and its internal input `feed`.
fn pw_loopback(mic: &str, feed: &str) -> Command {
    let mut cmd = Command::new("pw-loopback");
    cmd.args([
        "-n",
        "prostir-mic",
        "--capture-props",
        // Never restored (see `MicIn`); the microphone itself keeps what the user sets.
        &format!(
            "media.class=Audio/Sink audio.position=[ MONO ] node.name={feed} \
             node.description=\"{}\" state.restore-props=false",
            crate::i18n::t("Prostir (службовий)", "Prostir (internal)")
        ),
        "--playback-props",
        &format!(
            "media.class=Audio/Source audio.position=[ MONO ] node.name={mic} \
             node.description=\"{}\"",
            crate::i18n::t("Prostir — мікрофон шолома", "Prostir headset microphone")
        ),
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd
}

/// `pacat` playing raw headset sound from its standard input into `sink`.
fn pacat(sink: &str) -> Command {
    let mut cmd = Command::new("pacat");
    cmd.args([
        "--playback",
        "--raw",
        &format!("--device={sink}"),
        "--format=s16le",
        &format!("--rate={AUDIO_RATE}"),
        &format!("--channels={MIC_CHANNELS}"),
        &format!("--latency-msec={FEED_LATENCY_MS}"),
        // Full volume, whatever the sound system remembers for "prostir" (a near-zero
        // volume once stuck to Prostir's stream and came back on every run).
        "--volume=65536",
        "--property=state.restore-props=false",
        "--client-name=prostir",
        "--stream-name=headset microphone",
    ]);
    cmd
}

/// `RUNNING`, `IDLE` or `SUSPENDED` for a source, from `pactl list short sources`.
fn source_state(name: &str) -> Option<String> {
    state_in(&pactl(&["list", "short", "sources"])?, name).map(str::to_owned)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod mic_harness {
    //! Measures the headset microphone path on this PC's PipeWire (with its PulseAudio
    //! server, and `parecord`): `cargo test -p prostir -- --ignored mic_harness
    //! --nocapture`. It makes its own devices (`prostir_test_mic`, gone when it
    //! ends), leaves Prostir's and the default input alone, and can run while Prostir
    //! does. It prints, for each case, how much sound was lost, old or late, and the
    //! latency the PC adds (with the recording app's 10 ms).
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::*;

    const BLOCK: usize = AUDIO_RATE as usize / 100;

    /// 10 ms of sound whose every sample is the block number (never 0).
    fn block(n: u32) -> Vec<u8> {
        let v = (n % 30000 + 1) as i16;
        (0..BLOCK)
            .flat_map(|i| if i % 2 == 0 { v } else { -v }.to_le_bytes())
            .collect()
    }

    /// Samples, by when they came.
    type Recorded = Vec<(Instant, Vec<i16>)>;

    /// An app recording from `device` (`parecord`, 10 ms fragments): what arrives
    /// and when.
    struct Reader {
        child: Child,
        got: Arc<Mutex<Recorded>>,
    }

    impl Reader {
        fn start(device: &str) -> Self {
            use std::io::Read;
            let mut child = Command::new("parecord")
                .args([
                    "-d",
                    device,
                    "--raw",
                    "--format=s16le",
                    "--rate=48000",
                    "--channels=1",
                    "--latency-msec=10",
                ])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut out = child.stdout.take().unwrap();
            let got = Arc::new(Mutex::new(Vec::new()));
            let store = got.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                // A sample split between two reads waits for its second byte.
                let mut have = 0;
                while let Ok(n) = out.read(&mut buf[have..]) {
                    if n == 0 {
                        break;
                    }
                    have += n;
                    let (samples, _) = buf[..have].as_chunks::<2>();
                    let pcm = samples
                        .iter()
                        .map(|&c| i16::from_le_bytes(c).saturating_abs())
                        .collect();
                    store.lock().unwrap().push((Instant::now(), pcm));
                    buf.copy_within(have & !1..have, 0);
                    have %= 2;
                }
            });
            Self { child, got }
        }

        fn stop(mut self) -> Recorded {
            let _ = self.child.kill();
            let _ = self.child.wait();
            std::mem::take(&mut self.got.lock().unwrap())
        }
    }

    /// The headset: pushes block after block every 10 ms.
    struct Headset {
        next: u32,
        pushed: Vec<Instant>,
    }

    impl Headset {
        fn talk(&mut self, mic: &MicIn, for_ms: u64) {
            let start = Instant::now();
            let first = self.next;
            while start.elapsed() < Duration::from_millis(for_ms) {
                let due = start + Duration::from_millis(10 * u64::from(self.next - first));
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                self.pushed.push(Instant::now());
                mic.push(&block(self.next));
                self.next += 1;
            }
        }
    }

    /// Whether the way in is open, and how much waits in its pipe.
    fn levels(mic: &MicIn) -> String {
        let feed = mic.feed.lock().unwrap();
        let Some(f) = feed.as_ref() else {
            return "no feed".into();
        };
        let mut queued: libc::c_int = 0;
        // SAFETY: FIONREAD into an int.
        unsafe { libc::ioctl(f.pipe.as_raw_fd(), libc::FIONREAD, &mut queued) };
        format!(
            "feed open, {} ms in the pipe, {} ms dropped",
            queued / 96,
            f.dropped / 96
        )
    }

    /// What a recording got of the blocks in `window`, pushed while it recorded.
    struct Got {
        /// Never came.
        lost_ms: usize,
        /// Of those, at the start of the window.
        lost_first_ms: usize,
        /// Came, but from before `earlier` (an earlier recording, or while nobody
        /// recorded).
        stale_ms: usize,
        /// Came more than 100 ms after it was pushed.
        late_ms: usize,
        median_ms: f64,
        max_ms: f64,
    }

    fn report(
        what: &str,
        got: &Recorded,
        hs: &Headset,
        earlier: u32,
        window: std::ops::Range<u32>,
    ) -> Got {
        // When the last sample of each block came.
        let mut arrived = std::collections::BTreeMap::new();
        for (at, pcm) in got {
            for &s in pcm.iter().filter(|&&s| s > 0) {
                arrived.insert(s as u32 - 1, *at);
            }
        }
        let mut lat: Vec<f64> = arrived
            .iter()
            .filter(|(n, _)| window.contains(*n))
            .map(|(n, at)| at.duration_since(hs.pushed[*n as usize]).as_secs_f64() * 1e3)
            .collect();
        lat.sort_by(f64::total_cmp);
        let pct = |p: f64| {
            lat.get(((lat.len().max(1) - 1) as f64 * p) as usize)
                .copied()
        };
        let r = Got {
            lost_ms: 10 * window.clone().filter(|n| !arrived.contains_key(n)).count(),
            lost_first_ms: 10
                * window
                    .clone()
                    .take_while(|n| !arrived.contains_key(n))
                    .count(),
            stale_ms: 10 * arrived.keys().filter(|&&n| n < earlier).count(),
            late_ms: 10 * lat.iter().filter(|&&l| l > 100.0).count(),
            median_ms: pct(0.5).unwrap_or(f64::NAN),
            max_ms: pct(1.0).unwrap_or(f64::NAN),
        };
        eprintln!(
            "{what}: {} ms sent, {} ms lost ({} ms at the start), {} ms from before, \
             {} ms later than 100 ms; latency ms min {:.0} median {:.0} p95 {:.0} max {:.0}",
            window.len() * 10,
            r.lost_ms,
            r.lost_first_ms,
            r.stale_ms,
            r.late_ms,
            pct(0.0).unwrap_or(f64::NAN),
            r.median_ms,
            pct(0.95).unwrap_or(f64::NAN),
            r.max_ms,
        );
        r
    }

    /// A recording that went as it should: everything came, nothing from before,
    /// nothing late.
    fn assert_clean(g: &Got) {
        assert!(g.lost_ms <= 20, "lost {} ms", g.lost_ms);
        assert_eq!(g.stale_ms, 0, "old sound came out");
        assert_eq!(g.late_ms, 0, "late sound came out");
        assert!(g.median_ms < 50.0, "latency {} ms", g.median_ms);
    }

    /// The whole path on this PC's PipeWire, with throwaway devices (it does not touch
    /// Prostir's own, nor the default input). A test app (`parecord`) records while a
    /// fake headset pushes 10 ms blocks numbered in their samples, so every block
    /// that comes out tells how late it is. The recording is looked at where the sound
    /// system's event would come (see `sound_harness` for those). Run by hand:
    /// `cargo test -p prostir -- --ignored mic_harness --nocapture`.
    #[test]
    #[ignore = "by hand: records through the real sound system"]
    fn mic_harness() {
        let _ = env_logger::try_init();
        // Fixed names, as in `sound_harness`.
        let (name, feed) = (
            "prostir_test_mic".to_owned(),
            "prostir_test_feed".to_owned(),
        );
        let mic = MicIn::start_as(&name, &feed, false).unwrap();
        let mut hs = Headset {
            next: 0,
            pushed: Vec::new(),
        };
        // What the sound system's event would say, where it would come.
        let tick = |mic: &MicIn| {
            let on = source_state(&mic.mic).is_some_and(|s| s == "RUNNING");
            mic.set_recording(on);
            eprintln!("   tick: recording {on}, {}", levels(mic));
        };

        // 1. The first recording: the PC notices it, the headset's microphone follows.
        let r = Reader::start(&name);
        std::thread::sleep(Duration::from_millis(250));
        tick(&mic);
        std::thread::sleep(Duration::from_millis(300));
        let from = hs.next;
        hs.talk(&mic, 3000);
        std::thread::sleep(Duration::from_millis(200));
        assert_clean(&report(
            "1 first recording",
            &r.stop(),
            &hs,
            from,
            from..hs.next - 10,
        ));

        // 2. Nobody records, yet sound keeps coming; then an app records while it
        // still comes.
        for _ in 0..3 {
            hs.talk(&mic, 1000);
            tick(&mic);
            assert!(mic.feed.lock().unwrap().is_none());
        }
        let r = Reader::start(&name);
        hs.talk(&mic, 250);
        tick(&mic);
        let from = hs.next;
        hs.talk(&mic, 2000);
        std::thread::sleep(Duration::from_millis(200));
        let what = "2 sound while nobody records, then a recording";
        assert_clean(&report(what, &r.stop(), &hs, from, from..hs.next - 10));
        tick(&mic);

        // 3. The on-demand cycle, three times: the app starts, the PC notices, the
        // headset follows; the app stops, the PC notices, the headset stops.
        for round in 0..3 {
            let r = Reader::start(&name);
            std::thread::sleep(Duration::from_millis(250));
            tick(&mic);
            std::thread::sleep(Duration::from_millis(300));
            let from = hs.next;
            hs.talk(&mic, 2000);
            let to = hs.next;
            let got = r.stop();
            hs.talk(&mic, 250);
            tick(&mic);
            hs.talk(&mic, 300);
            let what = format!("3.{round} on-demand cycle");
            assert_clean(&report(&what, &got, &hs, from, from..to - 10));
            std::thread::sleep(Duration::from_secs(2));
        }

        // 4. A gap (Wi-Fi) in the middle of a recording.
        let r = Reader::start(&name);
        std::thread::sleep(Duration::from_millis(250));
        tick(&mic);
        std::thread::sleep(Duration::from_millis(300));
        let start = hs.next;
        hs.talk(&mic, 1000);
        std::thread::sleep(Duration::from_millis(300));
        let from = hs.next;
        hs.talk(&mic, 1500);
        std::thread::sleep(Duration::from_millis(200));
        assert_clean(&report(
            "4 after a 300 ms gap",
            &r.stop(),
            &hs,
            start,
            from..hs.next - 10,
        ));
        tick(&mic);

        // 5. A burst: a second of sound held up on the way comes at once. At most a
        // few tens of ms of it may still come out; after it, all is as before.
        let r = Reader::start(&name);
        std::thread::sleep(Duration::from_millis(250));
        tick(&mic);
        std::thread::sleep(Duration::from_millis(300));
        let start = hs.next;
        hs.talk(&mic, 1000);
        std::thread::sleep(Duration::from_secs(1));
        let burst = hs.next;
        for _ in 0..100 {
            hs.pushed.push(Instant::now() - Duration::from_secs(1));
            mic.push(&block(hs.next));
            hs.next += 1;
        }
        let from = hs.next;
        hs.talk(&mic, 1500);
        std::thread::sleep(Duration::from_millis(200));
        let got = r.stop();
        let held = report("5 the burst itself", &got, &hs, start, burst..from);
        assert!(
            held.late_ms <= 70,
            "{} ms of the burst came out",
            held.late_ms
        );
        let after = report("5 after a burst", &got, &hs, start, from..hs.next - 10);
        assert!(after.lost_first_ms <= 70 && after.median_ms < 50.0 && after.late_ms == 0);
        tick(&mic);
        drop(mic);
    }
}

#[cfg(test)]
mod sound_harness {
    //! Follows this PC's PipeWire (through its PulseAudio server) as Prostir does, with
    //! throwaway devices of its own (`prostir_test_sink`, `prostir_test_mic`,
    //! gone when it ends): `cargo test -p prostir -- --ignored sound_harness
    //! --nocapture`. It leaves Prostir's devices, the defaults and every other volume
    //! alone, and can run while Prostir does. It prints how often the watcher woke while
    //! the sound was idle, how soon each change came as an event, and what is captured
    //! of a tone at the levels the PC writes (the same at each: the headset applies the
    //! level once).
    use std::sync::mpsc::Receiver;
    use std::time::Instant;

    use super::*;
    use crate::events::Event;
    use crate::platform::sound::SoundEvent;

    /// The watcher's thread in this process, and how often it went to sleep and woke.
    fn watcher_wakeups() -> Option<(u32, u64)> {
        let tasks = std::fs::read_dir("/proc/self/task").ok()?;
        tasks.flatten().find_map(|task| {
            let name = std::fs::read_to_string(task.path().join("comm")).ok()?;
            (name.trim() == "sound-events").then_some(())?;
            let tid = task.file_name().to_string_lossy().parse().ok()?;
            Some((tid, switches(&task.path().join("status"))?))
        })
    }

    fn switches(status: &std::path::Path) -> Option<u64> {
        std::fs::read_to_string(status)
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))?
            .trim()
            .parse()
            .ok()
    }

    /// The `pactl subscribe` the watcher's thread started.
    fn subscriber(tid: u32) -> Option<u32> {
        let children = std::fs::read_to_string(format!("/proc/self/task/{tid}/children")).ok()?;
        children.split_whitespace().find_map(|pid| {
            let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
            cmd.contains("subscribe").then(|| pid.parse().ok())?
        })
    }

    /// The next sound event within `within`, and how long it took.
    fn next(events: &Receiver<Event>, within: Duration) -> Option<(SoundEvent, Duration)> {
        let started = Instant::now();
        match events.recv_timeout(within) {
            Ok(Event::Sound(e)) => Some((e, started.elapsed())),
            _ => None,
        }
    }

    fn pactl_ok(args: &[&str]) {
        assert!(pactl(args).is_some(), "pactl {args:?}");
    }

    /// Plays a second of a 440 Hz tone at peak 8000 into `sink` and records its
    /// monitor as Prostir captures it: the loudest sample recorded.
    fn captured_peak(sink: &str) -> i16 {
        use std::io::{Read, Write};
        let raw = ["--raw", "--format=s16le", "--rate=48000", "--channels=2"];
        let mut rec = Command::new("parecord")
            // Small pieces: by default it holds two seconds before writing any.
            .args(["-d", &format!("{sink}.monitor"), "--latency-msec=10"])
            .args(raw)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = rec.stdout.take().unwrap();
        let got = std::thread::spawn(move || {
            let mut all = Vec::new();
            let _ = out.read_to_end(&mut all);
            all
        });
        let tone: Vec<u8> = (0..48_000)
            .flat_map(|i| {
                let v = (8000.0 * (2.0 * std::f64::consts::PI * 440.0 * f64::from(i) / 48e3).sin())
                    as i16;
                [v.to_le_bytes(), v.to_le_bytes()].concat()
            })
            .collect();
        let mut play = Command::new("pacat")
            .args(["--playback", &format!("--device={sink}"), "--volume=65536"])
            .args(raw)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        play.stdin.take().unwrap().write_all(&tone).unwrap();
        let _ = play.wait();
        std::thread::sleep(Duration::from_millis(200));
        // Asked to stop, it writes out what it holds (killed, it would not).
        // SAFETY: a signal to the child started above.
        unsafe { libc::kill(rec.id() as libc::pid_t, libc::SIGTERM) };
        let _ = rec.wait();
        let pcm = got.join().unwrap();
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b).saturating_abs())
            .max()
            .unwrap_or(0)
    }

    #[test]
    #[ignore = "by hand: plays through the real sound system"]
    fn sound_harness() {
        let _ = env_logger::try_init();
        // Fixed names: WirePlumber keeps the volume of the microphone programs record
        // from (as it should for Prostir's own), so a name of each run would leave an
        // entry each time. One harness at a time.
        let sink = "prostir_test_sink";
        let (mic_name, feed) = (
            "prostir_test_mic".to_owned(),
            "prostir_test_feed".to_owned(),
        );
        let (events, received) = crate::events::channel();
        // The watcher first, then the devices, as Prostir starts them.
        let followed = pulse::Device::new(sink);
        let watcher = Watcher::start(Pactl::new(followed.clone(), &mic_name), events).unwrap();
        let device = VirtualNode::create(sink, "Prostir test", "Audio/Sink").unwrap();
        let mic = MicIn::start_as(&mic_name, &feed, false).unwrap();
        let volume = SinkVolume::of(followed);
        // What the new devices are: their first events.
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if let Some((e, _)) = next(&received, Duration::from_millis(300)) {
                eprintln!("at the start: {e:?}");
            }
        }
        let (tid, _) = watcher_wakeups().expect("the watcher's thread");
        let pactl_pid = subscriber(tid).expect("pactl subscribe");

        // 1. Idle: how often the watcher and its pactl woke in 10 s.
        let child_status = std::path::PathBuf::from(format!("/proc/{pactl_pid}/status"));
        let (w0, c0) = (
            watcher_wakeups().unwrap().1,
            switches(&child_status).unwrap(),
        );
        std::thread::sleep(Duration::from_secs(10));
        let (w1, c1) = (
            watcher_wakeups().unwrap().1,
            switches(&child_status).unwrap(),
        );
        let stray: Vec<SoundEvent> = received
            .try_iter()
            .filter_map(|e| match e {
                Event::Sound(e) => Some(e),
                _ => None,
            })
            .collect();
        eprintln!(
            "idle 10 s: the watcher woke {} times, pactl subscribe {} times, events {stray:?}",
            w1 - w0,
            c1 - c0
        );

        // 2. The slider (another program sets the device): one event.
        pactl_ok(&["set-sink-volume", sink, "37%"]);
        let (e, took) = next(&received, Duration::from_secs(2)).expect("no event for the slider");
        eprintln!("the slider at 37 %: {e:?} after {took:?}");
        assert!(matches!(
            e,
            SoundEvent::Volume {
                level: 37,
                muted: false,
                ..
            }
        ));

        // 3. The PC writes the headset's level: its echo, once, not outdated.
        volume.set(55, true);
        let (e, took) = next(&received, Duration::from_secs(2)).expect("no echo");
        eprintln!("the PC wrote 55 muted: {e:?} after {took:?}");
        let SoundEvent::Volume {
            level,
            muted,
            writes,
            ..
        } = e
        else {
            panic!("not a volume: {e:?}");
        };
        assert_eq!((level, muted), (55, true));
        assert!(!volume.outdated(writes));
        let more = next(&received, Duration::from_millis(500));
        assert!(more.is_none(), "the write said twice: {more:?}");
        assert_eq!(volume.level(), Some((55, true)));
        pactl_ok(&["set-sink-mute", sink, "0"]);
        let _ = next(&received, Duration::from_secs(2));

        // 4. A program records from the microphone, then stops.
        let started = Instant::now();
        let mut rec = Command::new("parecord")
            .args(["-d", &mic_name, "--raw", "--latency-msec=10", "/dev/null"])
            .spawn()
            .unwrap();
        let (e, _) = next(&received, Duration::from_secs(3)).expect("no event for the recording");
        eprintln!("a recording: {e:?} after {:?}", started.elapsed());
        assert_eq!(e, SoundEvent::Recording(true));
        std::thread::sleep(Duration::from_millis(500));
        let stopped = Instant::now();
        let _ = rec.kill();
        let _ = rec.wait();
        let (e, _) = next(&received, Duration::from_secs(3)).expect("no event for its end");
        eprintln!("its end: {e:?} after {:?}", stopped.elapsed());
        assert_eq!(e, SoundEvent::Recording(false));

        // 5. pactl subscribe killed: it starts again, and what changed meanwhile comes.
        // SAFETY: a signal to the child this test found.
        unsafe { libc::kill(pactl_pid as libc::pid_t, libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(200));
        pactl_ok(&["set-sink-volume", sink, "23%"]);
        let killed = Instant::now();
        let (e, _) = next(&received, Duration::from_secs(5)).expect("no event after a restart");
        eprintln!("killed, moved to 23 %: {e:?} after {:?}", killed.elapsed());
        assert!(matches!(e, SoundEvent::Volume { level: 23, .. }));
        let again = subscriber(tid).expect("a new pactl subscribe");
        assert_ne!(again, pactl_pid);

        // 6. The level the PC writes does not change what the headset gets: the
        // capture reads the monitor, before the device's volume and mute.
        let peaks: Vec<(&str, &str, i16)> = [("100%", "0"), ("30%", "0"), ("100%", "1")]
            .into_iter()
            .map(|(level, mute)| {
                pactl_ok(&["set-sink-volume", sink, level]);
                pactl_ok(&["set-sink-mute", sink, mute]);
                (level, mute, captured_peak(sink))
            })
            .collect();
        eprintln!("a tone at peak 8000, captured: {peaks:?}");
        for (_, _, peak) in &peaks {
            assert!((7600..=8400).contains(peak), "captured at {peak}");
        }

        // Gone with the watcher: no pactl left.
        drop(watcher);
        let until = Instant::now() + Duration::from_secs(3);
        while std::fs::read_to_string(format!("/proc/{again}/status"))
            .is_ok_and(|s| !s.contains("State:\tZ"))
        {
            assert!(
                Instant::now() < until,
                "pactl subscribe outlived the watcher"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(mic);
        drop(device);
        assert!(node_id(sink).is_none(), "the test sink is gone");
    }
}
