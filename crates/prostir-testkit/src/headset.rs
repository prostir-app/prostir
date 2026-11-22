use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use prostir_proto::discovery::FindSecret;
use prostir_proto::handshake::{self, Answer};
use prostir_proto::link::tcp::{NoiseTcp, Plain, SocketOptions};
use prostir_proto::link::{Closer, Link, PeerInfo, RecordReceiver, RecordSender};
use prostir_proto::packet::{self, Assembler, FRAGMENT, lane};
use prostir_proto::request::{Asked, Command, Outcome, Request};
use prostir_proto::secure::{Identity, Pairing, TrustStore, confirmation, tags_match};
use prostir_proto::state::{
    Decoder, Display, DisplayId, DisplayState, Encoding, Gen, HeadsetCaps, HeadsetState, PcState,
    Prefs, Presence, ReqId, UpFormat, UpKind, UpState, VolumeAsk, caps, permission,
};
use prostir_proto::stats::{Gaps, Pings};
use prostir_proto::volume::HeadsetVolume;
use prostir_proto::{ClientMsg, Codec, HostMsg};

use crate::nal;

/// The headset reports a shown frame at most this often per stream, like the Quest app.
const ACK_EVERY: Duration = Duration::from_millis(100);
/// The PC answers within this, and takes what is sent within this.
const REPLY: Duration = Duration::from_secs(10);
/// In a session, nothing at all from the PC for this long ends it, as in the Quest app.
const SILENCE: Duration = Duration::from_secs(5);
/// Pairing waits for a person typing the code on the PC.
const PAIRING: Duration = Duration::from_secs(300);
/// The headset pings the PC this often in a session, like the Quest app.
const PING_EVERY: Duration = Duration::from_secs(1);
/// After a goodbye, how long the PC has to close the connection.
const GOODBYE_WAIT: Duration = Duration::from_secs(1);
/// The fake headset's volume when it connects, percent.
pub const FIRST_VOLUME: u8 = 40;

/// The Quest's volume has 16 steps: a level becomes the nearest one.
pub fn quest_step(level: u8) -> u8 {
    let step = (f32::from(level) * 15.0 / 100.0).round();
    (step * 100.0 / 15.0).round() as u8
}

/// A headset's long-term key, the PCs it trusts, and their find secrets (what its
/// discovery queries for each are made with), kept in a directory.
pub struct Keys {
    pub identity: Identity,
    pub trusted: TrustStore,
    /// Each PC's find secret, by the PC's key (`find_secrets`: `key secret` in hex,
    /// a line each).
    pub find_secrets: BTreeMap<[u8; 32], FindSecret>,
    dir: std::path::PathBuf,
}

/// 32 bytes from 64 hex digits.
fn from_hex32(s: &str) -> Option<[u8; 32]> {
    let mut out = [0; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    (s.len() == 64).then_some(out)
}

impl Keys {
    /// The keys in `dir` (`identity`, `paired_hosts`, `find_secrets`), made on first
    /// use.
    pub fn load(dir: &Path) -> Result<Self> {
        let identity = Identity::load_or_create(&dir.join("identity"))
            .with_context(|| format!("headset key in {}", dir.display()))?;
        let find_secrets = std::fs::read_to_string(dir.join("find_secrets"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (key, secret) = l.split_once(' ')?;
                Some((from_hex32(key)?, from_hex32(secret.trim())?))
            })
            .collect();
        Ok(Self {
            identity,
            trusted: TrustStore::load(&dir.join("paired_hosts")),
            find_secrets,
            dir: dir.to_owned(),
        })
    }

    /// Keeps `secret` as the find secret of the PC whose key is `pc`.
    pub fn learn_find_secret(&mut self, pc: [u8; 32], secret: FindSecret) -> Result<()> {
        use prostir_proto::secure::{to_hex, write_atomic};
        use std::fmt::Write as _;
        self.find_secrets.insert(pc, secret);
        let text = self
            .find_secrets
            .iter()
            .fold(String::new(), |mut text, (k, s)| {
                let _ = writeln!(text, "{} {}", to_hex(k), to_hex(s));
                text
            });
        write_atomic(&self.dir.join("find_secrets"), text.as_bytes())?;
        Ok(())
    }
}

/// Why there is no session.
#[derive(Debug)]
pub enum ConnectError {
    /// The PC does not know this headset, and no pairing code was given.
    NotPaired,
    /// The PC tried pairing and gave up, or the codes did not match.
    PairingFailed(String),
    /// The PC speaks another protocol version.
    Incompatible {
        protocol: u32,
        app: String,
    },
    /// The PC cannot open the session, and said why.
    Refused(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPaired => write!(f, "the PC does not know this headset"),
            Self::Refused(why) => write!(f, "the PC cannot open the session: {why}"),
            Self::PairingFailed(why) => write!(f, "pairing failed: {why}"),
            Self::Incompatible { protocol, app } => {
                write!(f, "the PC speaks protocol v{protocol} (app {app})")
            }
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<std::io::Error> for ConnectError {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.into())
    }
}

impl From<anyhow::Error> for ConnectError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

/// One stream as the headset sees it: a monitor, a window panel or a display of the PC.
#[derive(Debug, Clone)]
pub struct Stream {
    /// The display's name, as `Displays` had it when the stream started.
    pub name: String,
    pub generation: Gen,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    /// `StreamStart`s for this id in this session (it starts over with a new encoder).
    pub starts: u32,
    /// Since the last start.
    pub frames: u64,
    pub keyframes: u64,
    pub bytes: u64,
    /// Bytes of the keyframes among them, and of the biggest one.
    pub keyframe_bytes: u64,
    pub keyframe_max: u64,
    pub first_is_key: Option<bool>,
    /// Keyframes a decoder could not start from (no parameter sets or no IDR).
    pub bad_keyframes: u64,
    /// The number of the last frame; frames of a generation count from 0.
    pub last_seq: Option<u32>,
}

impl Stream {
    /// A decoder would show a picture: a good keyframe came since the last start.
    pub fn has_picture(&self) -> bool {
        self.keyframes > self.bad_keyframes
    }
}

/// A message from the PC, in short, in the order it came: for tests of the order of
/// messages within a lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
    /// `Displays` with these displays.
    Displays(Vec<DisplayId>),
    Reply(ReqId),
    StreamStart(DisplayId, Gen),
    StreamEnd(DisplayId, Gen),
    /// The first frame after a start.
    FirstFrame(DisplayId, Gen),
    Other(&'static str),
}

/// What the PC told the headset in this session.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// The PC's state: the snapshot, then every part since.
    pub pc: PcState,
    /// Streams running now, by display.
    pub streams: BTreeMap<DisplayId, Stream>,
    /// Streams the PC ended, in order.
    pub ended: Vec<DisplayId>,
    /// Displays that left `Displays` in this session.
    pub removed: Vec<DisplayId>,
    /// Replies to requests, by number.
    pub replies: BTreeMap<ReqId, Outcome>,
    /// What went wrong, as the PC said: failed replies, problems, errors (the detail).
    pub errors: Vec<String>,
    pub notices: Vec<String>,
    /// How many times `Encoding` came, to tell an answer from what it said before.
    pub encoding_count: u32,
    /// Bytes of PC sound received.
    pub audio_bytes: u64,
    /// How late each slice of sound arrived, in order: from when the fake desktop made
    /// it (the time it starts with) to when it was read here. Only a PC with the fake
    /// desktop stamps its sound so; from any other PC these numbers mean nothing.
    pub sound_delays: Vec<Duration>,
    /// Pauses between slices of sound arriving.
    pub sound_gaps: Gaps,
    /// Pings from the PC (each answered at once).
    pub pings: u32,
    /// The PC ended the session on purpose, with this reason code.
    pub goodbye: Option<u8>,
    /// Round trips of this headset's pings, as their pongs came.
    pub round_trips: Vec<Duration>,
    pub messages: u64,
    /// The PC's find secret, once it sent it (right after the snapshot).
    pub find_secret: Option<FindSecret>,
    /// The PC closed the connection.
    pub closed: bool,
    /// Why reading stopped: `UnexpectedEof` when the PC closed the connection,
    /// `TimedOut` after silence.
    pub end: Option<std::io::ErrorKind>,
    /// Slices of sound that came in while a frame was half way in: they went between
    /// its packets.
    pub sound_between_fragments: u32,
    /// What came, in order.
    pub arrivals: Vec<Arrival>,
    /// Broken rules: a stream started for a display no `Displays` of this session listed, a
    /// frame of a generation that never started, a frame number out of order.
    pub broken: Vec<String>,
    /// Commands from the PC, in order (the reader answers them, see
    /// [`FakeHeadset::stop_answering`]).
    pub commands: Vec<Asked<Command>>,
    /// The PC's requests to take its volume, in order (the reader answers them).
    pub volume_asks: Vec<VolumeAsk>,
    /// Every state each display had, in order, from the snapshot on: a new entry
    /// when its state changes.
    pub states: Vec<(DisplayId, DisplayState)>,
    /// The displays of the last `Displays`.
    listed: Vec<DisplayId>,
    /// The name of every display listed in this session, removed since or not.
    named: BTreeMap<DisplayId, String>,
}

impl View {
    fn apply(&mut self, arrived: &Arrived) {
        self.messages += 1;
        let msg = arrived.msg.clone();
        let msg = match self.pc.apply(msg) {
            Ok(None) => {
                self.part(&arrived.msg);
                self.round_trips.extend(arrived.round_trip);
                return;
            }
            Ok(Some(msg)) => msg,
            Err(e) => {
                self.broken.push(format!("{e}: {:?}", arrived.msg));
                return;
            }
        };
        match msg {
            HostMsg::StreamStart {
                id,
                generation,
                w,
                h,
                codec,
            } => {
                self.arrivals.push(Arrival::StreamStart(id, generation));
                // Lanes are not ordered with each other: a start the PC queued while
                // the display was listed can come after the `Displays` that removed it
                // (its `StreamEnd` follows in the same lane). Only a display never
                // listed breaks the rule.
                let name = match self.named.get(&id) {
                    Some(name) => name.clone(),
                    None => {
                        self.broken
                            .push(format!("{id} started before it was listed"));
                        format!("{id}")
                    }
                };
                let starts = self.streams.get(&id).map_or(0, |s| s.starts) + 1;
                self.streams.insert(
                    id,
                    Stream {
                        name,
                        generation,
                        width: w,
                        height: h,
                        codec,
                        starts,
                        frames: 0,
                        keyframes: 0,
                        bytes: 0,
                        keyframe_bytes: 0,
                        keyframe_max: 0,
                        first_is_key: None,
                        bad_keyframes: 0,
                        last_seq: None,
                    },
                );
            }
            HostMsg::Frame {
                id,
                generation,
                seq,
                keyframe,
                data,
                ..
            } => {
                let Some(s) = self
                    .streams
                    .get_mut(&id)
                    .filter(|s| s.generation == generation)
                else {
                    self.broken.push(format!(
                        "a frame of {id} {generation:?}, which never started"
                    ));
                    return;
                };
                if s.last_seq.is_none() {
                    self.arrivals.push(Arrival::FirstFrame(id, generation));
                }
                // Numbers only go up; the first to come may be above 0 only where a
                // newer keyframe replaced the first on the PC: then it is that keyframe.
                if s.last_seq.is_some_and(|last| seq <= last)
                    || (s.last_seq.is_none() && seq != 0 && !keyframe)
                {
                    self.broken
                        .push(format!("frame {seq} of {id} out of order"));
                }
                s.last_seq = Some(seq);
                s.first_is_key.get_or_insert(keyframe);
                s.frames += 1;
                s.bytes += data.len() as u64;
                if keyframe {
                    s.keyframes += 1;
                    s.keyframe_bytes += data.len() as u64;
                    s.keyframe_max = s.keyframe_max.max(data.len() as u64);
                    if !nal::starts_decoder(&data, s.codec) {
                        s.bad_keyframes += 1;
                    }
                }
            }
            HostMsg::StreamEnd { id, generation } => {
                self.arrivals.push(Arrival::StreamEnd(id, generation));
                if self
                    .streams
                    .get(&id)
                    .is_some_and(|s| s.generation == generation)
                {
                    self.streams.remove(&id);
                }
                self.ended.push(id);
            }
            HostMsg::Reply { req, outcome } => {
                self.arrivals.push(Arrival::Reply(req));
                if let Outcome::Failed { detail, .. } = &outcome {
                    self.errors.push(detail.clone());
                }
                self.replies.insert(req, outcome);
            }
            HostMsg::Problem { detail, .. } => self.errors.push(detail),
            HostMsg::Command { asked, .. } => self.commands.push(asked),
            HostMsg::VolumeAsk { asked } => self.volume_asks.push(asked),
            HostMsg::Audio { pcm } => {
                self.audio_bytes += pcm.len() as u64;
                self.sound_between_fragments += u32::from(arrived.between_fragments);
                if let Some(made) = pcm.first_chunk::<8>() {
                    let made = u64::from_le_bytes(*made);
                    let late = arrived.monotonic_ns.saturating_sub(made);
                    self.sound_delays.push(Duration::from_nanos(late));
                }
                self.sound_gaps.event(arrived.at);
            }
            HostMsg::Error { text } => self.errors.push(text),
            HostMsg::Notice { text } => self.notices.push(text),
            HostMsg::FindSecret { secret } => self.find_secret = Some(secret),
            HostMsg::Ping { .. } => self.pings += 1,
            HostMsg::Goodbye { reason } => self.goodbye = Some(reason),
            other => self.arrivals.push(Arrival::Other(other_name(&other))),
        }
        self.round_trips.extend(arrived.round_trip);
    }

    /// The names of `displays`, and their states that changed since they were last
    /// seen.
    fn note_displays(&mut self, displays: &prostir_proto::state::Displays) {
        for d in &displays.displays {
            self.named.insert(d.id, d.name.clone());
            let last = self.states.iter().rev().find(|(id, _)| *id == d.id);
            if last.is_none_or(|(_, s)| *s != d.state) {
                self.states.push((d.id, d.state.clone()));
            }
        }
    }

    /// A part of the state came (already in `pc`).
    fn part(&mut self, msg: &HostMsg) {
        match msg {
            HostMsg::Displays { part } => {
                self.note_displays(part);
                let ids: Vec<DisplayId> = part.ids().collect();
                self.arrivals.push(Arrival::Displays(ids.clone()));
                let listed: Vec<DisplayId> = self.listed.clone();
                for id in listed.iter().filter(|id| !ids.contains(id)) {
                    self.removed.push(*id);
                }
                self.listed = ids;
            }
            HostMsg::Encoding { .. } => {
                self.encoding_count += 1;
                self.arrivals.push(Arrival::Other("Encoding"));
            }
            other => self.arrivals.push(Arrival::Other(other_name(other))),
        }
    }

    /// The displays the PC has, as it listed them.
    pub fn displays(&self) -> &[Display] {
        self.pc.displays.as_ref().map_or(&[], |d| &d.displays)
    }

    pub fn display(&self, id: DisplayId) -> Option<&Display> {
        self.displays().iter().find(|d| d.id == id)
    }

    /// The virtual monitors, in the order of the rows.
    pub fn order(&self) -> Vec<DisplayId> {
        self.displays()
            .iter()
            .filter(|d| d.is_virtual())
            .map(|d| d.id)
            .collect()
    }

    pub fn primary(&self) -> Option<DisplayId> {
        self.pc.displays.as_ref()?.primary
    }

    pub fn encoding(&self) -> Option<&Encoding> {
        self.pc.encoding.as_ref()
    }

    /// Every virtual monitor streams and shows a picture.
    pub fn all_monitors_show(&self) -> bool {
        let order = self.order();
        !order.is_empty()
            && order
                .iter()
                .all(|id| self.streams.get(id).is_some_and(Stream::has_picture))
    }
}

/// A message's name, for [`Arrival::Other`].
fn other_name(msg: &HostMsg) -> &'static str {
    match msg {
        HostMsg::Displays { .. } => "Displays",
        HostMsg::Encoding { .. } => "Encoding",
        HostMsg::Sound { .. } => "Sound",
        HostMsg::Limits { .. } => "Limits",
        HostMsg::Access { .. } => "Access",
        HostMsg::PcInfo { .. } => "PcInfo",
        HostMsg::HostCaps { .. } => "HostCaps",
        HostMsg::Focus { .. } => "Focus",
        HostMsg::SoundFrom { .. } => "SoundFrom",
        HostMsg::Snapshot { .. } => "Snapshot",
        HostMsg::Command { .. } => "Command",
        HostMsg::Problem { .. } => "Problem",
        _ => "other",
    }
}

/// A message from the PC and when it was read off the connection.
struct Arrived {
    msg: HostMsg,
    at: Instant,
    /// `CLOCK_MONOTONIC` at the same moment, in nanoseconds.
    monotonic_ns: u64,
    /// For a pong: how long since its ping went out.
    round_trip: Option<Duration>,
    /// Another message was half way in when this one completed.
    between_fragments: bool,
}

/// What a test asks of the headset's threads, and what they found.
#[derive(Default)]
struct Shared {
    /// Neither reading nor sending until then, as when Wi-Fi freezes.
    stalled_until: Mutex<Option<Instant>>,
    /// Hang up as soon as a message is half way in.
    cut_mid_message: AtomicBool,
    /// Leave the PC's commands unanswered.
    silent: AtomicBool,
    /// Answer the PC's commands with this failure.
    refuse: Mutex<Option<prostir_proto::state::ErrorCode>>,
    /// The headset's volume, which the PC asks to change.
    volume: Mutex<Option<HeadsetVolume>>,
    /// Why reading stopped.
    end: Mutex<Option<std::io::ErrorKind>>,
    pings: Mutex<Pings>,
}

impl Shared {
    fn wait_while_stalled(&self) {
        loop {
            let until = *self.stalled_until.lock().unwrap();
            match until.map(|t| t.saturating_duration_since(Instant::now())) {
                Some(left) if !left.is_zero() => std::thread::sleep(left),
                _ => return,
            }
        }
    }
}

/// Keeps the receive buffer small, so a headset that reads slowly holds the PC back
/// at once instead of after megabytes.
struct SmallReceiveBuffer;

impl SocketOptions for SmallReceiveBuffer {
    fn prepare(&self, stream: &TcpStream) {
        use std::os::fd::AsRawFd;
        let size: libc::c_int = 64 * 1024;
        // SAFETY: a valid socket and a c_int option value that lives through the call.
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw const size).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    fn set_pace(&self, _stream: &TcpStream, _bytes_per_sec: u64) {}
}

/// `CLOCK_MONOTONIC` now, in nanoseconds (the clock the fake desktop stamps sound with).
fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes into the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// A connected fake headset. A reader thread takes everything the PC sends (so the
/// PC never waits on this side) and acknowledges shown frames like the Quest app;
/// [`FakeHeadset::recv`] and [`FakeHeadset::wait`] go through what it took.
pub struct FakeHeadset {
    closer: Closer,
    writer: Arc<Mutex<Box<dyn RecordSender>>>,
    shared: Arc<Shared>,
    incoming: Receiver<Arrived>,
    view: View,
    /// The number of the last request sent.
    last_req: u32,
    /// The PC's protocol version and language, from its welcome.
    pub version: u32,
    pub language: String,
    /// The PC's key, as the handshake showed it.
    pub pc_key: [u8; 32],
}

impl FakeHeadset {
    /// Waits up to `timeout` for the PC's find secret (sent right after the snapshot)
    /// and keeps it in `keys`, as the Quest app keeps it to ask for this PC.
    pub fn learn_find_secret(&mut self, keys: &mut Keys, timeout: Duration) -> Result<()> {
        self.wait(timeout, "the find secret", |v| v.find_secret.is_some())?;
        let secret = self.view.find_secret.expect("waited for it");
        keys.learn_find_secret(self.pc_key, secret)
    }

    /// What this fake headset says about itself in `Open`: it shows everything the
    /// Quest app shows and decodes what it decodes, worn.
    pub fn state() -> HeadsetState {
        HeadsetState {
            caps: Some(HeadsetCaps {
                features: vec![
                    caps::headset::PHYSICAL,
                    caps::headset::WINDOWS,
                    caps::headset::COMMANDS,
                    caps::headset::MIC,
                    caps::headset::VOLUME,
                ],
                decoders: vec![
                    Decoder {
                        codec: 0,
                        max_w: 4096,
                        max_h: 4096,
                    },
                    Decoder {
                        codec: 1,
                        max_w: 8192,
                        max_h: 4320,
                    },
                ],
                streams_max: 30,
                layers_max: 16,
                model: "prostir-testkit".into(),
            }),
            prefs: Some(Prefs::default()),
            presence: Some(Presence { worn: true }),
            battery: None,
            volume: Some(HeadsetVolume::new(FIRST_VOLUME, false).now()),
            up: std::iter::once((
                UpKind::Mic,
                UpState {
                    kind: UpKind::Mic,
                    switch: true,
                    permission: permission::GRANTED,
                },
            ))
            .collect(),
        }
    }

    /// Connects to the PC at `addr`, says hello and opens the session. A PC this
    /// headset does not know, or one that does not know this headset, is paired with
    /// the code `pairing` gives (shown by the headset, typed on the PC); without it a
    /// PC that does not know the headset is [`ConnectError::NotPaired`].
    pub fn connect(
        addr: SocketAddr,
        keys: &mut Keys,
        pairing: Option<&mut dyn FnMut() -> String>,
    ) -> Result<Self, ConnectError> {
        let app = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
        Self::open(addr, keys, pairing, None, &app, &Self::state())
    }

    /// As [`FakeHeadset::connect`] for a trusted headset whose app says it is `app` in
    /// its hello (another build, say).
    pub fn connect_as(addr: SocketAddr, keys: &mut Keys, app: &str) -> Result<Self, ConnectError> {
        Self::open(addr, keys, None, None, app, &Self::state())
    }

    /// As [`FakeHeadset::connect`] for a trusted headset that says `state` about
    /// itself in `Open` (fewer capabilities, say).
    pub fn connect_saying(
        addr: SocketAddr,
        keys: &mut Keys,
        state: &HeadsetState,
    ) -> Result<Self, ConnectError> {
        let app = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
        Self::open(addr, keys, None, None, &app, state)
    }

    /// As [`FakeHeadset::connect`] for a trusted headset that reads at most
    /// `bytes_per_sec` (a slow link), with a small receive buffer.
    pub fn connect_reading_at(
        addr: SocketAddr,
        keys: &mut Keys,
        bytes_per_sec: u64,
    ) -> Result<Self, ConnectError> {
        let app = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
        Self::open(addr, keys, None, Some(bytes_per_sec), &app, &Self::state())
    }

    fn open(
        addr: SocketAddr,
        keys: &mut Keys,
        mut pairing: Option<&mut dyn FnMut() -> String>,
        read_rate: Option<u64>,
        app: &str,
        state: &HeadsetState,
    ) -> Result<Self, ConnectError> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(REPLY))?;
        let options: Arc<dyn SocketOptions> = match read_rate {
            Some(_) => Arc::new(SmallReceiveBuffer),
            None => Arc::new(Plain),
        };
        let Link {
            mut sender,
            mut receiver,
            closer,
            peer,
        } = NoiseTcp::connect(stream, &keys.identity, options)?;
        let mut packets = HostMsg::assembler();
        let mut ends = Ends {
            rx: &mut *receiver,
            tx: &mut *sender,
            packets: &mut packets,
        };
        let (version, language, pc) = open(&mut ends, keys, &peer, addr, &mut pairing, app, state)?;
        receiver.set_idle_limit(Some(SILENCE));
        let writer = Arc::new(Mutex::new(sender));
        let shared = Arc::new(Shared::default());
        if let Some(v) = state.volume {
            *shared.volume.lock().unwrap() = Some(HeadsetVolume::new(v.level, v.muted));
        }
        let incoming = spawn_reader(
            receiver,
            packets,
            writer.clone(),
            shared.clone(),
            closer.clone(),
            read_rate,
        )?;
        spawn_pinger(writer.clone(), shared.clone())?;
        let listed = pc
            .displays
            .iter()
            .flat_map(prostir_proto::state::Displays::ids)
            .collect();
        Ok(Self {
            closer,
            writer,
            shared,
            incoming,
            view: {
                let mut view = View {
                    pc,
                    listed,
                    ..View::default()
                };
                if let Some(displays) = view.pc.displays.clone() {
                    view.note_displays(&displays);
                }
                view
            },
            last_req: 0,
            version,
            language,
            pc_key: peer.key,
        })
    }

    /// Sends request `asked`, numbered after the last one: its number.
    pub fn request(&mut self, asked: Request) -> Result<ReqId> {
        self.last_req += 1;
        let req = ReqId(self.last_req);
        self.send(&ClientMsg::request(req, asked))?;
        Ok(req)
    }

    /// Takes what the PC sends until it answers request `req`, for up to `timeout`.
    pub fn reply(&mut self, req: ReqId, timeout: Duration) -> Result<Outcome> {
        self.wait(timeout, &format!("the reply to request {}", req.0), |v| {
            v.replies.contains_key(&req)
        })?;
        Ok(self.view.replies[&req].clone())
    }

    /// Sends request `asked` and waits for its reply.
    pub fn ask(&mut self, asked: Request) -> Result<Outcome> {
        let req = self.request(asked)?;
        self.reply(req, REPLY)
    }

    /// Sends `msg` to the PC.
    pub fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        send(&mut **self.writer.lock().unwrap(), msg.clone())?;
        Ok(())
    }

    /// Goes to sleep as the Quest app does (the power button): not worn, then asleep.
    pub fn sleep(&mut self) -> Result<()> {
        self.send(&ClientMsg::Presence {
            part: Presence { worn: false },
        })?;
        self.send(&ClientMsg::Asleep)
    }

    /// What the PC said so far in this session.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// The next message from the PC, within `timeout`. `None`: nothing came in time.
    /// Once the PC closed the connection, an error.
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<HostMsg>> {
        match self.incoming.recv_timeout(timeout) {
            Ok(arrived) => {
                self.view.apply(&arrived);
                Ok(Some(arrived.msg))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                self.view.closed = true;
                self.view.end = *self.shared.end.lock().unwrap();
                bail!("the PC closed the connection")
            }
        }
    }

    /// Takes what the PC sends until `done` holds for the view, for up to `timeout`.
    /// The error says `what` was awaited and what the headset saw.
    pub fn wait(
        &mut self,
        timeout: Duration,
        what: &str,
        done: impl Fn(&View) -> bool,
    ) -> Result<()> {
        let until = Instant::now() + timeout;
        while !done(&self.view) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!(
                    "no {what} within {timeout:?}; the headset saw {:#?}",
                    self.view
                );
            }
            self.recv(left)
                .with_context(|| format!("waiting for {what}; saw {:#?}", self.view))?;
        }
        Ok(())
    }

    /// Takes what the PC sends for `time` (the session must stay open).
    pub fn take_for(&mut self, time: Duration) -> Result<()> {
        let until = Instant::now() + time;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            self.recv(left)?;
        }
    }

    /// Waits up to `timeout` for the PC to close the connection.
    pub fn wait_closed(&mut self, timeout: Duration) -> Result<()> {
        let until = Instant::now() + timeout;
        while !self.view.closed {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("the PC kept the connection open for {timeout:?}");
            }
            let _ = self.recv(left);
        }
        Ok(())
    }

    /// Counts start again from here: frames, sound, ended streams, errors and notices.
    /// What the headset knows stays: the streams, the PC's state.
    pub fn reset_counts(&mut self) {
        let v = &mut self.view;
        for s in v.streams.values_mut() {
            (s.frames, s.keyframes, s.bad_keyframes, s.bytes) = (0, 0, 0, 0);
            (s.keyframe_bytes, s.keyframe_max) = (0, 0);
        }
        v.ended.clear();
        v.removed.clear();
        v.arrivals.clear();
        v.errors.clear();
        v.notices.clear();
        v.pings = 0;
        v.round_trips.clear();
        v.audio_bytes = 0;
        v.sound_between_fragments = 0;
        v.sound_delays.clear();
        v.sound_gaps.restart();
        v.messages = 0;
    }

    /// Neither reads nor sends for `time` from now, as when its Wi-Fi freezes.
    pub fn stall(&self, time: Duration) {
        *self.shared.stalled_until.lock().unwrap() = Some(Instant::now() + time);
    }

    /// The volume buttons of the headset: its own change, told to the PC.
    pub fn press_volume(&mut self, level: u8) -> Result<()> {
        let changed = self
            .shared
            .volume
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|v| v.changed_here(level, false));
        if let Some(part) = changed {
            self.send(&ClientMsg::Volume { part })?;
        }
        Ok(())
    }

    /// The headset's volume now.
    pub fn volume(&self) -> Option<u8> {
        self.shared
            .volume
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| v.now().level)
    }

    /// Streams `slices` of 10 ms of silence as the microphone: its start, the slices,
    /// its end.
    pub fn send_mic(&mut self, slices: usize) -> Result<()> {
        let rate = prostir_proto::AUDIO_RATE;
        self.send(&ClientMsg::UpStart {
            up: 0,
            kind: UpKind::Mic,
            format: UpFormat::Pcm16 { rate, channels: 1 },
        })?;
        for _ in 0..slices {
            let data = vec![0; (rate / 100 * 2) as usize];
            self.send(&ClientMsg::Up { up: 0, data })?;
        }
        self.send(&ClientMsg::UpEnd { up: 0 })
    }

    /// Answers the PC's commands with `code` from now on (a profile it does not have,
    /// say).
    pub fn refuse_commands(&self, code: prostir_proto::state::ErrorCode) {
        *self.shared.refuse.lock().unwrap() = Some(code);
    }

    /// Leaves the PC's commands unanswered from now on, as a headset that hangs would.
    pub fn stop_answering(&self) {
        self.shared.silent.store(true, Ordering::Relaxed);
    }

    /// Hangs up as soon as the next message is half way in (a frame, mostly).
    pub fn cut_mid_message(&self) {
        self.shared.cut_mid_message.store(true, Ordering::Relaxed);
    }

    /// Sends `record` as it is, a broken packet say.
    pub fn send_record(&self, record: &[u8]) -> Result<()> {
        self.writer.lock().unwrap().send(&[record])?;
        Ok(())
    }

    /// Hangs up, as a headset that lost the network would.
    pub fn disconnect(self) {
        self.closer.close();
    }

    /// Leaves as the Quest app does: a goodbye saying `reason`, nothing after it, and
    /// the connection closed once the PC closed it (or after `GOODBYE_WAIT`).
    pub fn leave(mut self, reason: prostir_proto::HeadsetGoodbye) -> Result<()> {
        {
            let mut w = self.writer.lock().unwrap();
            send(
                &mut **w,
                ClientMsg::Goodbye {
                    reason: reason.code(),
                },
            )?;
            match w.finish() {
                // The PC closed the connection on the goodbye already.
                Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {}
                finished => finished?,
            }
        }
        self.wait_closed(GOODBYE_WAIT)
    }
}

impl Drop for FakeHeadset {
    fn drop(&mut self) {
        self.closer.close();
    }
}

/// Sends `msg` whole: up streams in the sound lane, everything else in the control
/// one.
fn send(tx: &mut dyn RecordSender, msg: ClientMsg) -> std::io::Result<()> {
    let lane = match msg {
        ClientMsg::UpStart { .. } | ClientMsg::Up { .. } | ClientMsg::UpEnd { .. } => lane::SOUND,
        _ => lane::CONTROL,
    };
    packet::send_whole(tx, lane, msg.encode()?, FRAGMENT)
}

/// The two halves of a connection before the session, and the PC's packets put
/// together so far.
struct Ends<'a> {
    rx: &'a mut dyn RecordReceiver,
    tx: &'a mut dyn RecordSender,
    packets: &'a mut Assembler,
}

impl Ends<'_> {
    fn send(&mut self, msg: ClientMsg) -> std::io::Result<()> {
        send(self.tx, msg)
    }

    fn recv(&mut self) -> std::io::Result<HostMsg> {
        HostMsg::recv(self.rx, self.packets)
    }
}

/// Says hello, pairs if asked to and possible, and opens the session with `state`: the
/// PC's protocol version, language and state (its snapshot, which must come first).
fn open(
    ends: &mut Ends<'_>,
    keys: &mut Keys,
    peer: &PeerInfo,
    addr: SocketAddr,
    pairing: &mut Option<&mut dyn FnMut() -> String>,
    app: &str,
    state: &HeadsetState,
) -> Result<(u32, String, PcState), ConnectError> {
    // Versions first, before pairing.
    handshake::send_hello(ends.tx, app)?;
    let (version, language) = match handshake::read_answer(ends.rx)? {
        Answer::Welcome { version, language } => (version, language),
        Answer::Incompatible { protocol, app } => {
            return Err(ConnectError::Incompatible { protocol, app });
        }
    };
    if !keys.trusted.contains(&peer.key)
        && let Some(code) = pairing.as_mut()
    {
        pair(ends, &keys.identity, peer, &code())?;
        keys.trusted.add(peer.key, &addr.to_string())?;
    }
    let open = state.open().map_err(|e| anyhow!("{e}"))?;
    ends.send(open.clone())?;
    // The PC is trusted from here: its snapshot may be as big as it needs.
    ends.packets.set_limits(HostMsg::limits(true));
    let mut refused = None;
    loop {
        let msg = match ends.recv() {
            Ok(msg) => msg,
            // Said why, then closed the connection.
            Err(e) => {
                return Err(match refused {
                    Some(why) => ConnectError::Refused(why),
                    None => e.into(),
                });
            }
        };
        match msg {
            HostMsg::Snapshot { parts } => {
                let pc = PcState::from_snapshot(&parts).map_err(|e| anyhow!("snapshot: {e}"))?;
                return Ok((version, language, pc));
            }
            HostMsg::Error { text } => refused = Some(text),
            HostMsg::PairingRequired => {
                let Some(code) = pairing.as_mut() else {
                    return Err(ConnectError::NotPaired);
                };
                ends.packets.set_limits(HostMsg::limits(false));
                pair(ends, &keys.identity, peer, &code())?;
                keys.trusted.add(peer.key, &addr.to_string())?;
                ends.send(open.clone())?;
                ends.packets.set_limits(HostMsg::limits(true));
            }
            other => return Err(anyhow!("unexpected {other:?} before the snapshot").into()),
        }
    }
}

/// Reads everything the PC sends on a thread of its own, acknowledging frames as
/// shown (at most every [`ACK_EVERY`] per stream). The channel closes with the
/// connection.
fn spawn_reader(
    mut receiver: Box<dyn RecordReceiver>,
    mut packets: Assembler,
    writer: Arc<Mutex<Box<dyn RecordSender>>>,
    shared: Arc<Shared>,
    closer: Closer,
    read_rate: Option<u64>,
) -> std::io::Result<Receiver<Arrived>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fake-headset".into())
        .spawn(move || {
            // However reading stops, the connection closes with it, as in the Quest app.
            struct Close(Closer);
            impl Drop for Close {
                fn drop(&mut self) {
                    self.0.close();
                }
            }
            let _close = Close(closer.clone());
            let mut acked: BTreeMap<DisplayId, Instant> = BTreeMap::new();
            // Bytes read but not yet waited for, when reading at a set rate.
            let mut unpaid = 0u64;
            loop {
                shared.wait_while_stalled();
                let record = match receiver.recv() {
                    Ok(record) => record,
                    Err(e) => {
                        *shared.end.lock().unwrap() = Some(e.kind());
                        return;
                    }
                };
                if let Some(rate) = read_rate {
                    unpaid += record.len() as u64;
                    let owed = Duration::from_secs_f64(unpaid as f64 / rate as f64);
                    if owed >= Duration::from_millis(2) {
                        std::thread::sleep(owed);
                        unpaid = 0;
                    }
                }
                let complete = match packets.push(record) {
                    Ok(complete) => complete,
                    Err(e) => {
                        *shared.end.lock().unwrap() = Some(std::io::Error::from(e).kind());
                        return;
                    }
                };
                let Some(m) = complete else {
                    // Half a message in: hang up now if asked to.
                    if shared.cut_mid_message.load(Ordering::Relaxed) {
                        *shared.end.lock().unwrap() = Some(std::io::ErrorKind::ConnectionAborted);
                        return;
                    }
                    continue;
                };
                let between_fragments = packets.assembling() > 0;
                let msg = match HostMsg::from_message(m) {
                    Ok(Some(msg)) => msg,
                    Ok(None) => continue,
                    Err(e) => {
                        *shared.end.lock().unwrap() = Some(e.kind());
                        return;
                    }
                };
                let (at, monotonic_ns) = (Instant::now(), monotonic_ns());
                let mut round_trip = None;
                match &msg {
                    // Answered at once, from here, as the Quest app does.
                    HostMsg::Ping { seq } => {
                        let pong = ClientMsg::Pong { seq: *seq };
                        let _ = send(&mut **writer.lock().unwrap(), pong);
                    }
                    HostMsg::Pong { seq } => {
                        round_trip = shared.pings.lock().unwrap().pong(*seq, at);
                    }
                    // Takes the PC's level, as the Quest does (to its nearest step), unless
                    // its own change crossed it.
                    HostMsg::VolumeAsk { asked } => {
                        let answer = shared
                            .volume
                            .lock()
                            .unwrap()
                            .as_mut()
                            .map(|v| v.asked(asked, |level, muted| (quest_step(level), muted)));
                        if let Some(part) = answer {
                            let _ = send(&mut **writer.lock().unwrap(), ClientMsg::Volume { part });
                        }
                    }
                    // Done, as far as a fake headset does anything; a kind it does not
                    // know is unsupported, as the Quest app answers it.
                    HostMsg::Command { req, asked } if !shared.silent.load(Ordering::Relaxed) => {
                        let refused = *shared.refuse.lock().unwrap();
                        let outcome = match asked {
                            Asked::Known(_) if let Some(code) = refused => {
                                Outcome::failed(code, "refused by the fake headset")
                            }
                            Asked::Known(_) => Outcome::Done,
                            Asked::Unknown { .. } => {
                                Outcome::failed(prostir_proto::state::ErrorCode::UNSUPPORTED, "")
                            }
                        };
                        // "No limits" changes its preferences, which it says first, as
                        // the Quest app does.
                        if let (Asked::Known(Command::NoLimits { on }), Outcome::Done) =
                            (asked, &outcome)
                        {
                            let part = Prefs {
                                no_limits: *on,
                                ..Prefs::default()
                            };
                            let _ = send(&mut **writer.lock().unwrap(), ClientMsg::Prefs { part });
                        }
                        let reply = ClientMsg::Reply { req: *req, outcome };
                        let _ = send(&mut **writer.lock().unwrap(), reply);
                    }
                    _ => {}
                }
                if let HostMsg::Frame {
                    id,
                    generation,
                    seq,
                    ..
                } = &msg
                    && acked.get(id).is_none_or(|t| t.elapsed() >= ACK_EVERY)
                {
                    acked.insert(*id, Instant::now());
                    let shown = ClientMsg::FrameShown {
                        id: *id,
                        generation: *generation,
                        seq: *seq,
                    };
                    let _ = send(&mut **writer.lock().unwrap(), shown);
                }
                let arrived = Arrived {
                    msg,
                    at,
                    monotonic_ns,
                    round_trip,
                    between_fragments,
                };
                if tx.send(arrived).is_err() {
                    return;
                }
            }
        })?;
    Ok(rx)
}

/// Pings the PC every [`PING_EVERY`] (but not while stalled) until the connection
/// closes.
fn spawn_pinger(
    writer: Arc<Mutex<Box<dyn RecordSender>>>,
    shared: Arc<Shared>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("fake-headset-ping".into())
        .spawn(move || {
            loop {
                std::thread::sleep(PING_EVERY);
                shared.wait_while_stalled();
                let seq = shared.pings.lock().unwrap().ping(Instant::now());
                if send(&mut **writer.lock().unwrap(), ClientMsg::Ping { seq }).is_err() {
                    return;
                }
            }
        })?;
    Ok(())
}

/// Pairs with the code this headset shows (the person at the PC types it), waiting
/// for the PC's answers as long as a person may take.
fn pair(
    ends: &mut Ends<'_>,
    identity: &Identity,
    peer: &PeerInfo,
    code: &str,
) -> Result<(), ConnectError> {
    ends.rx.set_idle_limit(Some(PAIRING));
    let paired = exchange_codes(ends, identity, peer, code);
    ends.rx.set_idle_limit(Some(REPLY));
    paired
}

fn exchange_codes(
    ends: &mut Ends<'_>,
    identity: &Identity,
    peer: &PeerInfo,
    code: &str,
) -> Result<(), ConnectError> {
    ends.send(ClientMsg::PairBegin {
        headset_shows: true,
        name: "prostir-testkit".into(),
    })?;
    let (pairing, msg) = Pairing::start(code);
    ends.send(ClientMsg::PairMsg { msg })?;
    let mut said = String::new();
    let theirs = loop {
        match ends.recv()? {
            HostMsg::PairMsg { msg } => break msg,
            HostMsg::PairResult { .. } => {
                return Err(ConnectError::PairingFailed(format!(
                    "the PC gave up: {said}"
                )));
            }
            HostMsg::Error { text } => said = text,
            other => return Err(anyhow!("unexpected {other:?} while pairing").into()),
        }
    };
    let key = pairing.finish(&theirs)?;
    ends.send(ClientMsg::PairConfirm {
        tag: confirmation(&key, "headset", peer, &identity.public),
    })?;
    match ends.recv()? {
        HostMsg::PairResult { tag: Some(tag) }
            if tags_match(&tag, &confirmation(&key, "pc", peer, &identity.public)) =>
        {
            Ok(())
        }
        HostMsg::PairResult { .. } => Err(ConnectError::PairingFailed(
            "the codes did not match".into(),
        )),
        other => Err(anyhow!("unexpected {other:?} after pairing").into()),
    }
}

#[cfg(test)]
mod tests {
    use prostir_proto::state::{DisplayKind, Displays, Mode};

    use super::*;

    fn arrived(msg: HostMsg) -> Arrived {
        Arrived {
            msg,
            at: Instant::now(),
            monotonic_ns: 0,
            round_trip: None,
            between_fragments: false,
        }
    }

    fn listing(ids: &[u32]) -> HostMsg {
        let displays = ids
            .iter()
            .map(|&id| Display {
                id: DisplayId(id),
                kind: DisplayKind::Virtual,
                name: format!("Prostir-{id}"),
                mode: Mode::new(640, 360),
                desk: None,
                state: DisplayState::Running,
            })
            .collect();
        HostMsg::Displays {
            part: Displays {
                displays,
                primary: None,
            },
        }
    }

    fn start(id: u32, generation: u32) -> HostMsg {
        HostMsg::StreamStart {
            id: DisplayId(id),
            generation: Gen(generation),
            w: 640,
            h: 360,
            codec: Codec::H264,
        }
    }

    /// A start queued while its display was listed may come after the `Displays` that
    /// removed it (lanes are not ordered with each other); one for a display never
    /// listed is the PC's bug.
    #[test]
    fn only_a_display_never_listed_breaks_a_start() {
        let mut view = View::default();
        for msg in [listing(&[0, 5]), listing(&[0]), start(5, 1)] {
            view.apply(&arrived(msg));
        }
        assert!(view.broken.is_empty(), "{:?}", view.broken);
        assert_eq!(view.streams[&DisplayId(5)].name, "Prostir-5");
        view.apply(&arrived(start(7, 2)));
        assert_eq!(view.broken, ["display 7 started before it was listed"]);
    }
}
