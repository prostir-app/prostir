//! A fake PC: serves one trusted headset at a time like the PC app does (encryption,
//! hello, `Snapshot`, its find secret, parts, pings), answers the queries of the
//! headsets it trusts (on the UDP port of the same number as its TCP port), streams
//! monitors from clips encoded beforehand, answers requests, and breaks things on
//! purpose (a frame the decoder cannot take, a stream started over, the connection
//! dropped). No GStreamer, no desktop.
//!
//! One fake for three places: the headset's selftest (in the headset's own process),
//! tests of the headset's network code on Linux, and scenarios of the headset's core
//! (there by its messages alone).

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use prostir_proto::discovery::{Answer, FindSecret, MAX_ANSWER, MAX_QUERY, QueryRef, flags};
use prostir_proto::handshake::{self, First};
use prostir_proto::link::tcp::{NoiseTcp, Plain};
use prostir_proto::link::{Closer, Link, RecordReceiver, RecordSender};
use prostir_proto::packet::{self, FRAGMENT, lane};
use prostir_proto::request::Outcome;
use prostir_proto::secure::{Identity, TrustStore};
use prostir_proto::state::{
    Access, Display, DisplayId, DisplayKind, DisplayState, Displays, Encoding, ErrorCode, Gen,
    HeadsetCaps, HeadsetState, HostCaps, Limits, MicDevice, Mode, PcInfo, PcState, Sound, Wanted,
};
use prostir_proto::{ClientMsg, Codec, HostMsg, PROTOCOL_VERSION};

use crate::nal;

/// Nothing at all from the headset for this long ends the session (it pings every
/// second), as on the PC.
const SILENCE: Duration = Duration::from_secs(5);
/// The PC pings the headset this often.
const PING_EVERY: Duration = Duration::from_secs(1);
/// A stream that fell this many frames behind its clock skips ahead instead of
/// sending them all at once.
const MAX_BEHIND: u32 = 5;
/// After a keyframe asked for went out, further asks for the same stream are ignored
/// this long, as the PC app's router does.
const KEYFRAME_ASKS_APART: Duration = Duration::from_millis(300);

/// An encoded clip: access units of one codec and size, played in a loop.
pub struct Clip {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    data: Vec<u8>,
    units: Vec<Range<usize>>,
}

impl Clip {
    /// The clip in `data` (an Annex-B byte stream), which must start with a keyframe
    /// a decoder can start from.
    pub fn new(codec: Codec, width: u32, height: u32, fps: u32, data: Vec<u8>) -> Result<Self> {
        let units = nal::access_units(&data, codec);
        let Some(first) = units.first() else {
            bail!("no pictures in the clip");
        };
        if !nal::starts_decoder(&data[first.clone()], codec) {
            bail!("the clip does not start with a keyframe a decoder can start from");
        }
        if fps == 0 || width == 0 || height == 0 {
            bail!("a clip of no size or speed");
        }
        Ok(Self {
            codec,
            width,
            height,
            fps,
            data,
            units,
        })
    }

    /// A clip named as `scripts/selftest-clips.sh` names them:
    /// `<codec>-<width>x<height>-<fps>.<codec>`, e.g. `h264-1920x1080-60.h264`.
    pub fn named(name: &str, data: Vec<u8>) -> Result<Self> {
        let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
        let mut parts = stem.split('-');
        let (Some(codec), Some(size), Some(fps), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            bail!("{name}: not <codec>-<width>x<height>-<fps>");
        };
        let codec = match codec {
            "h264" => Codec::H264,
            "h265" => Codec::H265,
            other => bail!("{name}: no codec {other}"),
        };
        let (w, h) = size
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .ok_or_else(|| anyhow!("{name}: no size"))?;
        let fps = fps
            .parse()
            .with_context(|| format!("{name}: no frame rate"))?;
        Self::new(codec, w, h, fps, data).with_context(|| name.to_owned())
    }

    /// Pictures in one loop of the clip.
    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Pictures in one loop a decoder can start from.
    pub fn keyframes(&self) -> usize {
        (0..self.len()).filter(|&i| self.is_key(i)).count()
    }

    fn unit(&self, i: usize) -> &[u8] {
        &self.data[self.units[i % self.units.len()].clone()]
    }

    fn is_key(&self, i: usize) -> bool {
        nal::starts_decoder(self.unit(i), self.codec)
    }
}

/// What the fake PC is and serves.
pub struct PcConfig {
    /// The fake PC's key, made there on first use.
    pub identity: PathBuf,
    /// The headsets it serves (their keys); any other is told to pair, and closed.
    pub trusted: Vec<[u8; 32]>,
    /// What its monitors show; [`PcConfig::monitors`] of them are picked among those
    /// the headset decodes.
    pub clips: Vec<Clip>,
    /// How many monitors stream at most.
    pub monitors: usize,
    /// Its name, as `PcInfo` says it.
    pub name: String,
    /// What the headsets it trusts make their tags with: given in each session, as
    /// the PC app does.
    pub find_secret: FindSecret,
}

/// Something the fake PC saw or did, when.
#[derive(Debug, Clone)]
pub struct Event {
    pub at: Instant,
    pub what: Happened,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Happened {
    /// A headset connected and the encrypted handshake is done.
    Connected,
    /// A headset it trusts opened the session, saying this about itself.
    Opened {
        caps: Option<HeadsetCaps>,
    },
    /// A headset it does not trust asked for a session: told to pair, closed.
    NotTrusted,
    /// A query came: with its tag (`ours`), and whether it was answered.
    Asked {
        ours: bool,
        answered: bool,
    },
    /// The snapshot went out, listing these monitors.
    Snapshot {
        monitors: Vec<DisplayId>,
    },
    StreamStarted {
        id: DisplayId,
        generation: Gen,
        codec: Codec,
        width: u32,
        height: u32,
    },
    /// The headset showed frame `seq` (it says so about every 100 ms per stream).
    Shown {
        id: DisplayId,
        generation: Gen,
        seq: u32,
    },
    KeyframeAsked {
        id: DisplayId,
        generation: Gen,
    },
    /// An ask for a keyframe came within 300 ms of the last one answered, and was
    /// ignored (as the PC app does).
    KeyframeIgnored {
        id: DisplayId,
        generation: Gen,
    },
    /// A keyframe went out, as asked.
    KeyframeSent {
        id: DisplayId,
        generation: Gen,
        seq: u32,
    },
    /// A frame too big for the headset's decoder went out instead of frame `seq`.
    BrokenSent {
        id: DisplayId,
        generation: Gen,
        seq: u32,
    },
    /// Worn or not, as the headset says; streams pause while it is not worn.
    Presence {
        worn: bool,
    },
    /// The headset left, saying why (its goodbye code).
    Goodbye {
        reason: u8,
    },
    /// The session ended (either side).
    Closed,
}

/// What a test asks of the session.
enum Cmd {
    /// From the reader: answer the headset's ping.
    Pong(u32),
    Keyframe(DisplayId, Gen),
    Reply(prostir_proto::state::ReqId),
    Presence(bool),
    /// From the reader: the connection is over.
    Ended,
    Broken(DisplayId),
    Restart(DisplayId),
    RestartWithoutKeyframe(DisplayId),
    Goodbye(u8),
}

/// The session in progress, for [`FakePc`]'s methods.
struct Current {
    cmds: Sender<Cmd>,
    closer: Closer,
}

struct Shared {
    events: Sender<Event>,
    current: Mutex<Option<Current>>,
    stop: AtomicBool,
    /// The last generation of any stream: they never repeat, not even across
    /// sessions, so what a headset reports of one is never taken for another's.
    generation: AtomicU32,
    /// Frames sent, by display, over every session.
    frames: Mutex<std::collections::BTreeMap<DisplayId, u64>>,
}

impl Shared {
    fn tell(&self, what: Happened) {
        let _ = self.events.send(Event {
            at: Instant::now(),
            what,
        });
    }

    fn command(&self, cmd: Cmd) -> bool {
        lock(&self.current)
            .as_ref()
            .is_some_and(|c| c.cmds.send(cmd).is_ok())
    }

    fn next_generation(&self) -> Gen {
        Gen(self.generation.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// A running fake PC on `127.0.0.1`, on a port of its own.
pub struct FakePc {
    addr: SocketAddr,
    key: [u8; 32],
    shared: Arc<Shared>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
    find: Option<JoinHandle<()>>,
}

/// A TCP port and the UDP port of the same number on `127.0.0.1`, both free.
fn ports() -> Result<(TcpListener, UdpSocket)> {
    let mut last = None;
    for _ in 0..32 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        match UdpSocket::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(udp) => return Ok((listener, udp)),
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow!("no free pair of ports: {last:?}"))
}

/// Answers the queries on `udp` as the PC app does: with its tag, or any while it
/// trusts nobody. Until stopped.
fn answer_queries(udp: &UdpSocket, config: &FindConfig, shared: &Shared) {
    let _ = udp.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buf = [0u8; MAX_QUERY + 1];
    let mut out = Vec::with_capacity(MAX_ANSWER);
    while !shared.stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = udp.recv_from(&mut buf) else {
            continue;
        };
        let Some(query) = QueryRef::read(&buf[..n]) else {
            continue;
        };
        let ours = query.looks_for(&config.secret);
        let answered = ours || !config.paired;
        if answered {
            let bits = if config.paired { 0 } else { flags::ANSWERS_ALL };
            Answer::write_to(
                &query.nonce,
                &config.secret,
                config.port,
                bits,
                &config.name,
                &mut out,
            );
            let _ = udp.send_to(&out, from);
        }
        shared.tell(Happened::Asked { ours, answered });
    }
}

/// What the fake PC answers queries with.
struct FindConfig {
    secret: FindSecret,
    paired: bool,
    port: u16,
    name: String,
}

impl FakePc {
    /// Starts listening on `127.0.0.1` on a free port (TCP for the sessions, UDP of the
    /// same number for the queries).
    pub fn start(config: PcConfig) -> Result<Self> {
        let identity = Identity::load_or_create(&config.identity)
            .with_context(|| format!("the fake PC's key in {}", config.identity.display()))?;
        let (listener, udp) = ports()?;
        let addr = listener.local_addr()?;
        let (tx, events) = mpsc::channel();
        let shared = Arc::new(Shared {
            events: tx,
            current: Mutex::new(None),
            stop: AtomicBool::new(false),
            generation: AtomicU32::new(0),
            frames: Mutex::default(),
        });
        let key = identity.public;
        let finding = FindConfig {
            secret: config.find_secret,
            paired: !config.trusted.is_empty(),
            port: addr.port(),
            name: config.name.clone(),
        };
        let answering = shared.clone();
        let find = std::thread::Builder::new()
            .name("fake-pc-find".into())
            .spawn(move || answer_queries(&udp, &finding, &answering))?;
        let served = shared.clone();
        let thread = std::thread::Builder::new()
            .name("fake-pc".into())
            .spawn(move || serve(&listener, &identity, &config, &served))?;
        Ok(Self {
            addr,
            key,
            shared,
            events,
            thread: Some(thread),
            find: Some(find),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The fake PC's key, for the headset to trust.
    pub fn key(&self) -> [u8; 32] {
        self.key
    }

    /// What happened since last asked, in order.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Frames sent for display `id` so far, over every session.
    pub fn frames_sent(&self, id: DisplayId) -> u64 {
        lock(&self.shared.frames).get(&id).copied().unwrap_or(0)
    }

    /// Instead of the next frame of display `id`, one bigger than its decoder takes:
    /// the headset loses it and asks for a keyframe. `false` without a session.
    pub fn send_broken_frame(&self, id: DisplayId) -> bool {
        self.shared.command(Cmd::Broken(id))
    }

    /// Starts the stream of display `id` over, in a new generation.
    pub fn restart_stream(&self, id: DisplayId) -> bool {
        self.shared.command(Cmd::Restart(id))
    }

    /// Starts the stream of display `id` over, in a new generation, but from the
    /// frame after its keyframe, as if a keyframe had just been asked for and sent:
    /// the headset gets nothing it can decode, and an ask for a keyframe within
    /// 300 ms is ignored, so it has to ask again to see a picture before the clip
    /// comes round to its keyframe.
    pub fn restart_stream_without_keyframe(&self, id: DisplayId) -> bool {
        self.shared.command(Cmd::RestartWithoutKeyframe(id))
    }

    /// Hangs up without a word, as a PC that lost the network would.
    pub fn drop_connection(&self) -> bool {
        match lock(&self.shared.current).as_ref() {
            Some(c) => {
                c.closer.close();
                true
            }
            None => false,
        }
    }

    /// Ends the session with a goodbye saying `reason` (a `PcGoodbye` code).
    pub fn goodbye(&self, reason: u8) -> bool {
        self.shared.command(Cmd::Goodbye(reason))
    }

    /// Stops: the session ends, the port closes, and its threads are gone on return.
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.drop_connection();
        // Wakes the listener.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(find) = self.find.take() {
            let _ = find.join();
        }
    }
}

impl Drop for FakePc {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// A lock, also when a thread panicked holding it (the data is counts and handles,
/// fine to go on with).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Makes the headset's and the fake PC's keys (if not there yet) and has the headset
/// trust the PC: the headset's `identity` and `paired_hosts` files in `headset_dir`,
/// the PC's key in `pc_identity`. The keys of the headset and of the PC.
pub fn trust_each_other(
    headset_dir: &Path,
    pc_identity: &Path,
    pc_label: &str,
) -> Result<([u8; 32], [u8; 32])> {
    std::fs::create_dir_all(headset_dir)?;
    let headset = Identity::load_or_create(&headset_dir.join("identity"))?;
    let pc = Identity::load_or_create(pc_identity)?;
    let mut trusted = TrustStore::load(&headset_dir.join("paired_hosts"));
    if !trusted.contains(&pc.public) {
        trusted.add(pc.public, pc_label)?;
    }
    Ok((headset.public, pc.public))
}

/// Takes headsets one after another until stopped.
fn serve(listener: &TcpListener, identity: &Identity, config: &PcConfig, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(stream) = stream else { continue };
        if let Err(e) = session(stream, identity, config, shared) {
            log::info!("fake PC: session ended: {e:#}");
        }
        *lock(&shared.current) = None;
        shared.tell(Happened::Closed);
    }
}

/// One monitor streaming a clip.
struct Streaming {
    id: DisplayId,
    lane: u8,
    clip: usize,
    generation: Gen,
    /// The next frame's number in its generation, and the clip's picture it shows.
    seq: u32,
    unit: usize,
    next_due: Instant,
    broken_next: bool,
    /// When a keyframe asked for last went out.
    answered: Option<Instant>,
}

fn session(
    stream: TcpStream,
    identity: &Identity,
    config: &PcConfig,
    shared: &Arc<Shared>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Link {
        mut sender,
        mut receiver,
        closer,
        peer,
    } = NoiseTcp::accept(stream, identity, Arc::new(Plain))?;
    // However the session ends, the connection closes: the reader stops with it.
    struct CloseOnExit(Closer);
    impl Drop for CloseOnExit {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let _close = CloseOnExit(closer.clone());
    shared.tell(Happened::Connected);
    let First::Hello { alone: true, .. } = handshake::read_first(&mut *receiver)? else {
        bail!("no hello of this protocol");
    };
    handshake::send_from_pc(
        &mut *sender,
        &HostMsg::Welcome {
            version: PROTOCOL_VERSION,
            language: "en".into(),
        },
    )?;
    let mut packets = ClientMsg::assembler();
    let parts = match ClientMsg::recv(&mut *receiver, &mut packets)? {
        ClientMsg::Open { parts } => parts,
        other => bail!("{other:?} before Open"),
    };
    if !config.trusted.contains(&peer.key) {
        shared.tell(Happened::NotTrusted);
        send(&mut *sender, lane::CONTROL, HostMsg::PairingRequired)?;
        let _ = sender.finish();
        std::thread::sleep(Duration::from_millis(200));
        return Ok(());
    }
    packets.set_limits(ClientMsg::limits(true));
    let headset = HeadsetState::from_open(&parts).map_err(|e| anyhow!("Open: {e}"))?;
    let caps = headset.caps.clone();
    shared.tell(Happened::Opened { caps: caps.clone() });
    let picked = pick_clips(&config.clips, caps.as_ref(), config.monitors);
    let pc = pc_state(config, &picked);
    send(&mut *sender, lane::CONTROL, pc.snapshot()?)?;
    send(
        &mut *sender,
        lane::CONTROL,
        HostMsg::FindSecret {
            secret: config.find_secret,
        },
    )?;
    shared.tell(Happened::Snapshot {
        monitors: pc.displays.iter().flat_map(Displays::ids).collect(),
    });
    receiver.set_idle_limit(Some(SILENCE));
    let (cmds, cmd_rx) = mpsc::channel();
    *lock(&shared.current) = Some(Current {
        cmds: cmds.clone(),
        closer: closer.clone(),
    });
    let reading = shared.clone();
    let reader = std::thread::Builder::new()
        .name("fake-pc-read".into())
        .spawn(move || read(&mut *receiver, &mut packets, &cmds, &reading))?;
    let worn = headset.presence.is_none_or(|p| p.worn);
    let result = stream_until_over(&mut *sender, &cmd_rx, config, &picked, worn, shared);
    closer.close();
    let _ = reader.join();
    result
}

/// Which clip each monitor shows: as many as `monitors`, the first clips the
/// headset decodes, each clip once while there are enough of them.
fn pick_clips(clips: &[Clip], caps: Option<&HeadsetCaps>, monitors: usize) -> Vec<usize> {
    let fits: Vec<usize> = (0..clips.len())
        .filter(|&i| {
            let c = &clips[i];
            caps.is_none_or(|caps| caps.fits(c.codec, c.width, c.height))
        })
        .collect();
    if fits.is_empty() {
        return Vec::new();
    }
    (0..monitors).map(|k| fits[k % fits.len()]).collect()
}

/// The PC's state: a monitor for each picked clip, numbered from 1.
fn pc_state(config: &PcConfig, picked: &[usize]) -> PcState {
    let displays = picked
        .iter()
        .enumerate()
        .map(|(k, &c)| {
            let clip = &config.clips[c];
            Display {
                id: DisplayId(k as u32 + 1),
                kind: DisplayKind::Virtual,
                name: format!("Selftest {}", k + 1),
                mode: Mode::new(clip.width, clip.height),
                desk: None,
                state: DisplayState::Running,
            }
        })
        .collect();
    let codec = picked
        .first()
        .map_or(Codec::H264, |&c| config.clips[c].codec);
    PcState {
        displays: Some(Displays {
            displays,
            primary: None,
        }),
        info: Some(PcInfo {
            name: config.name.clone(),
            app: prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
            language: "en".into(),
            os: 0,
            desktop: String::new(),
        }),
        caps: Some(HostCaps::default()),
        access: Some(Access::MAIN),
        limits: Some(Limits {
            virtual_max: 4,
            windows_max: 0,
            physical_max: 0,
            streams_max: 4,
            min: (640, 480),
            max: (3840, 2160),
            sizes: picked
                .iter()
                .map(|&c| Mode::new(config.clips[c].width, config.clips[c].height))
                .collect(),
            scales: vec![100],
        }),
        encoding: Some(Encoding {
            wanted: Wanted {
                bitrate: 20,
                codec: Some(codec),
                software: true,
                sharp: false,
            },
            using: Some((codec, false)),
            choices: vec![(codec, false)],
        }),
        sound: Some(Sound {
            to_headset: false,
            mic: MicDevice::Off,
        }),
        ..PcState::default()
    }
}

/// Sends `msg` whole in `lane`.
fn send(tx: &mut dyn RecordSender, lane: u8, msg: HostMsg) -> std::io::Result<()> {
    packet::send_whole(tx, lane, msg.encode()?, FRAGMENT)
}

/// Streams every picked clip at its frame rate and does what is asked, until the
/// session is over.
fn stream_until_over(
    tx: &mut dyn RecordSender,
    cmds: &Receiver<Cmd>,
    config: &PcConfig,
    picked: &[usize],
    mut worn: bool,
    shared: &Shared,
) -> Result<()> {
    let start = Instant::now();
    let mut streams: Vec<Streaming> = picked
        .iter()
        .enumerate()
        .map(|(k, &clip)| Streaming {
            id: DisplayId(k as u32 + 1),
            lane: lane::VIDEO + k as u8,
            clip,
            generation: shared.next_generation(),
            seq: 0,
            unit: 0,
            next_due: start,
            broken_next: false,
            answered: None,
        })
        .collect();
    for s in &streams {
        start_stream(tx, config, s, shared)?;
    }
    let mut ping_seq = 0u32;
    let mut next_ping = start + PING_EVERY;
    loop {
        let now = Instant::now();
        let due = streams
            .iter()
            .filter(|_| worn)
            .map(|s| s.next_due)
            .chain([next_ping])
            .min()
            .unwrap_or(next_ping);
        match cmds.recv_timeout(due.saturating_duration_since(now)) {
            Ok(Cmd::Pong(seq)) => send(tx, lane::CONTROL, HostMsg::Pong { seq })?,
            Ok(Cmd::Keyframe(id, g)) => {
                if let Some(s) = streams.iter_mut().find(|s| s.id == id && s.generation == g) {
                    let now = Instant::now();
                    if s.answered
                        .is_some_and(|t| now.saturating_duration_since(t) < KEYFRAME_ASKS_APART)
                    {
                        shared.tell(Happened::KeyframeIgnored { id, generation: g });
                    } else {
                        // The clip starts with its keyframe: back to it.
                        s.unit = 0;
                        s.answered = Some(now);
                    }
                }
            }
            Ok(Cmd::Reply(req)) => {
                let outcome = Outcome::failed(ErrorCode::UNSUPPORTED, "not on the fake PC");
                send(tx, lane::CONTROL, HostMsg::Reply { req, outcome })?;
            }
            Ok(Cmd::Presence(on)) => {
                worn = on;
                let now = Instant::now();
                for s in &mut streams {
                    s.next_due = now;
                }
            }
            Ok(Cmd::Broken(id)) => {
                if let Some(s) = streams.iter_mut().find(|s| s.id == id) {
                    s.broken_next = true;
                }
            }
            Ok(Cmd::Restart(id)) => {
                if let Some(s) = streams.iter_mut().find(|s| s.id == id) {
                    s.generation = shared.next_generation();
                    (s.seq, s.unit, s.broken_next, s.answered) = (0, 0, false, None);
                    s.next_due = Instant::now();
                    start_stream(tx, config, s, shared)?;
                }
            }
            Ok(Cmd::RestartWithoutKeyframe(id)) => {
                if let Some(s) = streams.iter_mut().find(|s| s.id == id) {
                    let now = Instant::now();
                    s.generation = shared.next_generation();
                    let after_key = 1 % config.clips[s.clip].len();
                    (s.seq, s.unit, s.broken_next) = (0, after_key, false);
                    s.answered = Some(now);
                    s.next_due = now;
                    start_stream(tx, config, s, shared)?;
                }
            }
            Ok(Cmd::Goodbye(reason)) => {
                send(tx, lane::CONTROL, HostMsg::Goodbye { reason })?;
                let _ = tx.finish();
                std::thread::sleep(Duration::from_millis(300));
                return Ok(());
            }
            Ok(Cmd::Ended) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if now >= next_ping {
            ping_seq += 1;
            send(tx, lane::CONTROL, HostMsg::Ping { seq: ping_seq })?;
            next_ping = now + PING_EVERY;
        }
        if !worn {
            continue;
        }
        for s in &mut streams {
            if s.next_due > now {
                continue;
            }
            send_frame(tx, config, s, shared)?;
            let period = Duration::from_secs(1) / config.clips[s.clip].fps;
            s.next_due += period;
            if now.saturating_duration_since(s.next_due) > period * MAX_BEHIND {
                s.next_due = now + period;
            }
        }
    }
}

fn start_stream(
    tx: &mut dyn RecordSender,
    config: &PcConfig,
    s: &Streaming,
    shared: &Shared,
) -> Result<()> {
    let clip = &config.clips[s.clip];
    send(
        tx,
        s.lane,
        HostMsg::StreamStart {
            id: s.id,
            generation: s.generation,
            w: clip.width,
            h: clip.height,
            codec: clip.codec,
        },
    )?;
    shared.tell(Happened::StreamStarted {
        id: s.id,
        generation: s.generation,
        codec: clip.codec,
        width: clip.width,
        height: clip.height,
    });
    Ok(())
}

fn send_frame(
    tx: &mut dyn RecordSender,
    config: &PcConfig,
    s: &mut Streaming,
    shared: &Shared,
) -> Result<()> {
    let clip = &config.clips[s.clip];
    let seq = s.seq;
    let pts_us = u64::from(seq) * 1_000_000 / u64::from(clip.fps);
    let (keyframe, data) = if std::mem::take(&mut s.broken_next) {
        // Four times what the headset sets its decoder up to take in one frame
        // (`max-input-size`: the picture's pixels, at least 2 MiB), so no decoder's
        // rounded-up input buffer holds it: the frame is lost, as a broken one is.
        let size = (clip.width as usize * clip.height as usize).max(2 << 20) * 4;
        shared.tell(Happened::BrokenSent {
            id: s.id,
            generation: s.generation,
            seq,
        });
        (false, vec![0; size])
    } else {
        let key = clip.is_key(s.unit);
        if key && s.unit == 0 && seq > 0 {
            shared.tell(Happened::KeyframeSent {
                id: s.id,
                generation: s.generation,
                seq,
            });
        }
        (key, clip.unit(s.unit).to_vec())
    };
    send(
        tx,
        s.lane,
        HostMsg::Frame {
            id: s.id,
            generation: s.generation,
            seq,
            keyframe,
            pts_us,
            data,
        },
    )?;
    *lock(&shared.frames).entry(s.id).or_default() += 1;
    s.seq += 1;
    s.unit = (s.unit + 1) % clip.len();
    Ok(())
}

/// Reads what the headset sends until the connection ends.
fn read(
    rx: &mut dyn RecordReceiver,
    packets: &mut packet::Assembler,
    cmds: &Sender<Cmd>,
    shared: &Shared,
) {
    loop {
        let Ok(msg) = ClientMsg::recv(rx, packets) else {
            let _ = cmds.send(Cmd::Ended);
            return;
        };
        match msg {
            ClientMsg::Ping { seq } => {
                let _ = cmds.send(Cmd::Pong(seq));
            }
            ClientMsg::FrameShown {
                id,
                generation,
                seq,
            } => shared.tell(Happened::Shown {
                id,
                generation,
                seq,
            }),
            ClientMsg::RequestKeyframe { id, generation } => {
                shared.tell(Happened::KeyframeAsked { id, generation });
                let _ = cmds.send(Cmd::Keyframe(id, generation));
            }
            ClientMsg::Request { req, .. } => {
                let _ = cmds.send(Cmd::Reply(req));
            }
            ClientMsg::Presence { part } => {
                shared.tell(Happened::Presence { worn: part.worn });
                let _ = cmds.send(Cmd::Presence(part.worn));
            }
            ClientMsg::Goodbye { reason } => {
                shared.tell(Happened::Goodbye { reason });
                let _ = cmds.send(Cmd::Ended);
                return;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FakeHeadset, Keys};

    /// A clip of `n` pictures, the first a keyframe: SPS, PPS, IDR, then P slices.
    pub(crate) fn clip(n: usize, codec: Codec) -> Clip {
        let mut data = vec![
            0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 0x88, 3,
        ];
        for k in 1..n {
            data.extend([0, 0, 1, 0x41, 0x9a, k as u8]);
        }
        Clip::new(codec, 1280, 720, 60, data).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("prostir-fake-pc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn started(dir: &Path, monitors: usize) -> (FakePc, Keys) {
        let (headset, _) = trust_each_other(&dir.join("headset"), &dir.join("pc"), "fake").unwrap();
        let pc = FakePc::start(PcConfig {
            identity: dir.join("pc"),
            trusted: vec![headset],
            clips: vec![clip(30, Codec::H264)],
            monitors,
            name: "Fake".into(),
            find_secret: [7; 32],
        })
        .unwrap();
        (pc, Keys::load(&dir.join("headset")).unwrap())
    }

    /// Its own headset's query (with the fake PC's tag) is answered, with the tag of
    /// the same secret; a stranger's gets nothing.
    #[test]
    fn its_headsets_find_it_by_asking() {
        let dir = temp_dir("asked");
        let (pc, _) = started(&dir, 1);
        let to = pc.addr();
        let own = prostir_proto::discovery::Query::new([&[7; 32]], "");
        let answer = crate::find::ask(to, &own, Duration::from_secs(2)).expect("answered");
        assert!(answer.from_pc(&[7; 32]));
        assert_eq!((answer.port, answer.name.as_str()), (to.port(), "Fake"));
        let stranger = prostir_proto::discovery::Query::new([&[8; 32]], "");
        assert!(crate::find::ask(to, &stranger, Duration::from_millis(300)).is_none());
        wait_for(&pc, |h| {
            *h == Happened::Asked {
                ours: true,
                answered: true,
            }
        });
        wait_for(&pc, |h| {
            *h == Happened::Asked {
                ours: false,
                answered: false,
            }
        });
        pc.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn wait_for(pc: &FakePc, what: impl Fn(&Happened) -> bool) -> Event {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let e = pc
                .events()
                .recv_timeout(left)
                .expect("the fake PC did not get there");
            if what(&e.what) {
                return e;
            }
        }
    }

    #[test]
    fn clips_are_named_by_codec_size_and_rate() {
        let c = clip(3, Codec::H264);
        let named = Clip::named("h264-1280x720-60.h264", c.data.clone()).unwrap();
        assert_eq!(
            (named.width, named.height, named.fps, named.len()),
            (1280, 720, 60, 3)
        );
        assert!(Clip::named("h264-1280x720.h264", c.data.clone()).is_err());
        assert!(Clip::named("vp9-1280x720-60.vp9", c.data).is_err());
        // A clip must start with a keyframe.
        assert!(Clip::new(Codec::H264, 8, 8, 60, vec![0, 0, 1, 0x41, 0x9a]).is_err());
    }

    #[test]
    fn monitors_get_clips_the_headset_decodes() {
        let clips = vec![
            Clip {
                codec: Codec::H265,
                ..clip(2, Codec::H264)
            },
            clip(2, Codec::H264),
        ];
        let no_h265 = HeadsetCaps {
            decoders: Vec::new(),
            ..FakeHeadset::state().caps.unwrap()
        };
        assert_eq!(pick_clips(&clips, None, 2), [0, 1]);
        assert_eq!(pick_clips(&clips, Some(&no_h265), 2), [1, 1]);
        assert_eq!(pick_clips(&clips, Some(&no_h265), 1), [1]);
        assert!(pick_clips(&clips[..1], Some(&no_h265), 2).is_empty());
    }

    /// The fake headset sees what the PC app would send it: a snapshot with the
    /// monitors, their streams, frames it acknowledges; the fake PC sends a keyframe
    /// when asked, starts a stream over, and takes the headset back after a drop.
    #[test]
    fn a_headset_gets_a_session_with_streams() {
        let dir = temp_dir("session");
        let (pc, mut keys) = started(&dir, 2);
        let mut headset = FakeHeadset::connect(pc.addr(), &mut keys, None).unwrap();
        assert_eq!(headset.pc_key, pc.key());
        assert_eq!(headset.view().order(), [DisplayId(1), DisplayId(2)]);
        headset
            .wait(
                Duration::from_secs(5),
                "pictures",
                crate::View::all_monitors_show,
            )
            .unwrap();
        wait_for(&pc, |h| matches!(h, Happened::Shown { .. }));
        assert!(pc.frames_sent(DisplayId(1)) > 0);

        let first = headset.view().streams[&DisplayId(2)].generation;
        assert!(pc.restart_stream(DisplayId(2)));
        let restarted = wait_for(&pc, |h| matches!(h, Happened::StreamStarted { .. }));
        let Happened::StreamStarted { generation, .. } = restarted.what else {
            unreachable!()
        };
        assert_ne!(generation, first);
        headset
            .wait(Duration::from_secs(5), "the new generation", |v| {
                v.streams[&DisplayId(2)].generation == generation
            })
            .unwrap();

        headset
            .send(&ClientMsg::RequestKeyframe {
                id: DisplayId(1),
                generation: headset.view().streams[&DisplayId(1)].generation,
            })
            .unwrap();
        wait_for(&pc, |h| matches!(h, Happened::KeyframeSent { .. }));
        assert!(pc.send_broken_frame(DisplayId(1)));
        wait_for(&pc, |h| matches!(h, Happened::BrokenSent { .. }));

        assert!(pc.drop_connection());
        headset.wait_closed(Duration::from_secs(5)).unwrap();
        wait_for(&pc, |h| *h == Happened::Closed);
        let again = FakeHeadset::connect(pc.addr(), &mut keys, None).unwrap();
        assert_eq!(again.view().order(), [DisplayId(1), DisplayId(2)]);
        drop(again);
        pc.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Like the PC app, the fake PC answers one ask for a keyframe and ignores the
    /// next ones for 300 ms; a stream started over without its keyframe counts as
    /// just answered.
    #[test]
    fn keyframe_asks_close_together_are_ignored() {
        let dir = temp_dir("asks");
        let (headset, _) = trust_each_other(&dir.join("headset"), &dir.join("pc"), "fake").unwrap();
        let pc = FakePc::start(PcConfig {
            identity: dir.join("pc"),
            trusted: vec![headset],
            // 5 s: no keyframe of its own comes round during the test.
            clips: vec![clip(300, Codec::H264)],
            monitors: 1,
            name: "Fake".into(),
            find_secret: [7; 32],
        })
        .unwrap();
        let mut keys = Keys::load(&dir.join("headset")).unwrap();
        let mut headset = FakeHeadset::connect(pc.addr(), &mut keys, None).unwrap();
        headset
            .wait(
                Duration::from_secs(5),
                "pictures",
                crate::View::all_monitors_show,
            )
            .unwrap();
        let id = DisplayId(1);
        let ask = |headset: &mut FakeHeadset| {
            let generation = headset.view().streams[&id].generation;
            headset
                .send(&ClientMsg::RequestKeyframe { id, generation })
                .unwrap();
        };
        ask(&mut headset);
        ask(&mut headset);
        // The keyframe goes out with the next frame, maybe after the second ask.
        let (mut ignored, mut sent) = (false, false);
        while !(ignored && sent) {
            match wait_for(&pc, |h| {
                matches!(
                    h,
                    Happened::KeyframeSent { .. } | Happened::KeyframeIgnored { .. }
                )
            })
            .what
            {
                Happened::KeyframeIgnored { .. } => ignored = true,
                _ => sent = true,
            }
        }

        assert!(pc.restart_stream_without_keyframe(id));
        let Happened::StreamStarted { generation, .. } =
            wait_for(&pc, |h| matches!(h, Happened::StreamStarted { .. })).what
        else {
            unreachable!()
        };
        headset
            .wait(Duration::from_secs(5), "the new generation", |v| {
                v.streams[&id].generation == generation
            })
            .unwrap();
        ask(&mut headset);
        wait_for(&pc, |h| matches!(h, Happened::KeyframeIgnored { .. }));
        std::thread::sleep(KEYFRAME_ASKS_APART);
        ask(&mut headset);
        let sent = wait_for(&pc, |h| {
            matches!(
                h,
                Happened::KeyframeSent { .. } | Happened::KeyframeIgnored { .. }
            )
        });
        assert!(
            matches!(sent.what, Happened::KeyframeSent { generation: g, .. } if g == generation),
            "{sent:?}"
        );
        drop(headset);
        pc.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A goodbye reaches the headset with its reason, and the connection closes; a
    /// stream's generation is new in the next session too.
    #[test]
    fn a_goodbye_ends_the_session() {
        let dir = temp_dir("goodbye");
        let (pc, mut keys) = started(&dir, 1);
        let mut headset = FakeHeadset::connect(pc.addr(), &mut keys, None).unwrap();
        headset
            .wait(Duration::from_secs(5), "a stream", |v| {
                !v.streams.is_empty()
            })
            .unwrap();
        let first = headset.view().streams[&DisplayId(1)].generation;
        assert!(pc.goodbye(prostir_proto::PcGoodbye::Quit.code()));
        headset.wait_closed(Duration::from_secs(5)).unwrap();
        assert_eq!(
            headset.view().goodbye,
            Some(prostir_proto::PcGoodbye::Quit.code())
        );
        let mut again = FakeHeadset::connect(pc.addr(), &mut keys, None).unwrap();
        again
            .wait(Duration::from_secs(5), "a stream", |v| {
                !v.streams.is_empty()
            })
            .unwrap();
        assert_ne!(again.view().streams[&DisplayId(1)].generation, first);
        drop(again);
        pc.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A headset the fake PC does not trust is told to pair.
    #[test]
    fn a_stranger_is_told_to_pair() {
        let dir = temp_dir("stranger");
        let (pc, _) = started(&dir, 1);
        let mut stranger = Keys::load(&dir.join("stranger")).unwrap();
        stranger.trusted.add(pc.key(), "fake").unwrap();
        let err = FakeHeadset::connect(pc.addr(), &mut stranger, None)
            .err()
            .unwrap();
        assert!(matches!(err, crate::ConnectError::NotPaired), "{err}");
        wait_for(&pc, |h| *h == Happened::NotTrusted);
        pc.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
