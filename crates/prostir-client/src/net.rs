//! Finds the PC host, keeps the TCP connection alive and fans frames out to decoders.

#![expect(
    clippy::disallowed_methods,
    reason = "the network thread is part of the shell: its sockets, files and clock are its own"
)]

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{info, warn};
use prostir_proto::handshake::{self, Answer};
use prostir_proto::link::tcp::{NoiseTcp, Plain};
use prostir_proto::link::{Closer, Link, PeerInfo, RecordReceiver, RecordSender};
use prostir_proto::packet::Assembler;
use prostir_proto::request::{Asked, Command, Outcome};
use prostir_proto::secure::{self, Identity, Pairing, TrustStore, confirmation, tags_match};
use prostir_proto::state::{self, DisplayId, ErrorCode, Gen, HeadsetState, PcState, ReqId};
use prostir_proto::stats::{Gaps, Period};
use prostir_proto::{ClientMsg, Codec, HeadsetGoodbye, HostMsg, PcGoodbye};

use crate::audio::{Gains, Player};
use crate::frames::{Frame, FrameQueue, Frames, KeyframeWait, Pushed};
use crate::ids::PcId;
use crate::lang::{Lang, Texts};
use crate::mic::{Mic, Recorder};
use crate::outbox::{Lanes, Outbox};
use crate::ui::OfferWhy;

/// After a goodbye, how long the PC has to read it and close the connection before
/// this side does.
const GOODBYE_WAIT: Duration = Duration::from_secs(1);

/// While the user types a code, the connection is looked at this often.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// How often the session logs what it received.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// Nothing at all from the PC for this long (it pings every second): the session is
/// over.
const SILENCE: Duration = Duration::from_secs(5);

/// Messages to the PC (see [`crate::outbox`]). Sending only queues them.
#[derive(Clone)]
pub struct Feedback {
    outbox: Arc<Outbox>,
}

impl Feedback {
    /// Starts the writer thread for one session. It ends (and closes the connection,
    /// which ends the session) when a write fails or times out, or once the outbox is
    /// closed.
    fn start(mut sender: Box<dyn RecordSender>, closer: Closer) -> std::io::Result<Self> {
        let outbox = Arc::new(Outbox::default());
        let queued = outbox.clone();
        std::thread::Builder::new()
            .name("net-writer".into())
            .spawn(move || {
                crate::xrutil::raise_thread_priority(crate::xrutil::Priority::Video);
                // However this thread stops, the session ends with it.
                struct Close(Closer, Arc<Outbox>);
                impl Drop for Close {
                    fn drop(&mut self) {
                        self.1.close();
                        self.0.close();
                    }
                }
                let _close = Close(closer, queued.clone());
                let mut lanes = Lanes::default();
                while let Some(pick) = queued.next(&mut lanes) {
                    if let Err(e) = lanes.send(pick, &mut *sender) {
                        warn!("sending to the PC failed: {e}");
                        return;
                    }
                }
                // After the goodbye nothing more goes out; the PC closes the connection
                // once it read it, and what it sends meanwhile is still read, so a reset
                // cannot throw the goodbye away. If it does not close, this side does.
                if queued.said_goodbye() {
                    let _ = sender.finish();
                    std::thread::sleep(GOODBYE_WAIT);
                }
            })?;
        Ok(Self { outbox })
    }

    /// Ends the session on purpose, telling the PC why.
    fn goodbye(&self, reason: HeadsetGoodbye) {
        self.outbox.say_goodbye(reason);
    }

    /// Queues a message. While the network is stuck, microphone sound and frame
    /// acknowledgements beyond about 200 ms of them are dropped; nothing else is. An
    /// error means the session is over.
    pub fn send(&self, msg: ClientMsg) -> std::io::Result<()> {
        self.outbox.push(msg)
    }

    /// The session is over: the writer stops.
    fn close(&self) {
        self.outbox.close();
    }

    /// The session is open: pings start.
    fn start_pings(&self) {
        self.outbox.start_pings();
    }
}

/// A PC announcing itself on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundHost {
    pub name: String,
    pub addr: SocketAddr,
}

pub enum NetEvent {
    /// Connected; the handle sends requests (add monitor…) to the host. `pc`: which PC
    /// it is, by its key from the handshake. `opened_with`: the headset's state as
    /// `Open` carried it.
    Connected {
        pc: PcId,
        addr: SocketAddr,
        name: Option<String>,
        feedback: Feedback,
        /// The PC's interface language.
        language: Option<Lang>,
        opened_with: HeadsetState,
    },
    /// PCs currently seen on the network (sent when the list changes).
    Hosts(Vec<FoundHost>),
    /// A part of the PC's state (see `prostir_proto::state::PcState`), checked: the
    /// snapshot's parts one by one, then each change.
    Part(HostMsg),
    /// The answer to request `req`.
    Reply {
        req: ReqId,
        outcome: Outcome,
    },
    /// Something went wrong on the PC that nobody asked for.
    Problem {
        error: ErrorCode,
        detail: String,
    },
    /// The PC said why it cannot serve the headset (before the session); shown in the
    /// menu.
    HostError(String),
    /// A stream starts (or starts over in a new generation).
    StreamStart {
        id: DisplayId,
        generation: Gen,
        width: u32,
        height: u32,
        codec: Codec,
        frames: Frames,
        feedback: Feedback,
    },
    /// The stream stopped (the display may still be there: only `Displays` says).
    StreamEnd {
        id: DisplayId,
    },
    /// A stream larger than this headset decodes came (from an older PC): display
    /// `name` (as the user reads it) shows nothing.
    TooBig {
        name: String,
        size: (u32, u32),
        codec: Codec,
    },
    /// Pairing with a new PC: show `code` (the PC asks for it), or, with `None`,
    /// ask for the code the PC shows.
    Pairing {
        host: String,
        code: Option<String>,
    },
    /// The codes did not match; a new attempt follows.
    PairingFailed,
    /// The PC (its tray, later `prostir ctl`) asks for something: answered once.
    Command {
        req: ReqId,
        asked: Asked<Command>,
    },
    /// A desktop notification of the PC.
    Notice(String),
    /// A PC announces itself on the network but does not accept the connection
    /// (a firewall on the PC); `None` once connecting works again.
    Unreachable(Option<String>),
    /// An unknown PC: ask the user whether to pair with it (`failed`: after several
    /// wrong codes).
    PairOffer {
        host: String,
        why: OfferWhy,
    },
    Paired,
    Disconnected {
        why: Disconnect,
    },
    /// The PC speaks another protocol version: `pc_newer` says which side to update.
    Incompatible {
        pc_newer: bool,
        pc_app: String,
    },
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disconnect {
    /// The PC ended the session and said why.
    Goodbye(PcGoodbye),
    /// Nothing came from the PC for a while: Wi-Fi gone, or the PC asleep.
    Silence,
    /// The connection closed (or never opened a session), without a word.
    Closed,
}

/// Where to look for the PC, and this headset's keys.
pub struct Discovery {
    /// This headset's long-term key.
    pub identity_file: PathBuf,
    /// PCs paired with.
    pub trust_file: PathBuf,
    /// User-provided `IP` or `IP:PORT`, tried first.
    pub host_file: Option<PathBuf>,
    /// Address of the last host that worked over the network, tried before discovery.
    pub last_host_file: PathBuf,
    /// The selftest's fake PC: connect to this address and nothing else (no USB, no
    /// `host.txt`, no last host, no listening for PCs, no probing the network).
    pub only: Option<SocketAddr>,
    /// This headset's name for pairing (its model, as the system names it).
    pub name: String,
}

/// What sessions play the PC's sound and record the microphone with: made once for
/// each start of the app and shared with the frame loop.
pub struct Sound {
    /// Where the sound comes from (the frame loop places it).
    pub gains: Arc<Gains>,
    pub mic: Arc<Mic>,
}

/// This headset, as its sessions need it.
struct Headset {
    identity: Identity,
    /// What the PC calls it when they pair.
    name: String,
    sound: Sound,
}

/// Lets the menu choose which PC to use and take part in pairing.
#[derive(Clone, Default)]
pub struct Steering(Arc<(Mutex<Steer>, Condvar)>);

#[derive(Default)]
struct Steer {
    /// Picked in the menu (or typed in); tried before anything else.
    choice: Option<SocketAddr>,
    /// "Change PC": do not reconnect on our own until the user picks one.
    picking: bool,
    current: Option<Current>,
    /// Pairing the other way round: the PC shows the code, the headset asks for it.
    pc_shows_code: bool,
    /// Code typed in the menu while pairing.
    typed_code: Option<String>,
    /// The headset is off the head: stay away from the PC, so it can give its
    /// desktop back to the laptop.
    taken_off: bool,
    /// A PC this headset is not paired with, waiting for the user to say whether to
    /// pair. Nothing connects on its own meanwhile.
    offer: Option<(PeerKey, SocketAddr)>,
    /// The user agreed to pair with this PC (until when).
    approved: Option<(PeerKey, Instant)>,
    /// The trust file was changed by the menu (a PC forgotten): read it again.
    trust_changed: bool,
    /// The app is closing: the network thread ends, and so does its session.
    quit: bool,
    /// What the headset says about itself when it opens a session (`Open`).
    headset: HeadsetState,
    /// The menu's language, for what this thread names (a PC over USB).
    texts: Option<Texts>,
}

type PeerKey = [u8; 32];

/// The connection in use: which PC, and how to drop it.
struct Current {
    addr: SocketAddr,
    closer: Closer,
    /// Once the session is open: messages to the PC, for a goodbye.
    feedback: Option<Feedback>,
}

impl Current {
    /// Leaves the PC: with a goodbye once the session is open, else by closing.
    fn leave(&self, reason: HeadsetGoodbye) {
        match &self.feedback {
            Some(f) => f.goodbye(reason),
            None => self.closer.close(),
        }
    }
}

/// How long a "Pair" answer holds for reconnecting and typing the code.
const APPROVAL: Duration = Duration::from_secs(600);
/// Wrong codes in a row before the headset stops and asks again.
const PAIR_ATTEMPTS: u32 = 3;

impl Steering {
    fn lock(&self) -> std::sync::MutexGuard<'_, Steer> {
        self.0.0.lock().unwrap()
    }

    /// Connects to `addr`, dropping the current connection if it is another PC.
    pub fn connect_to(&self, addr: SocketAddr) {
        let mut s = self.lock();
        s.choice = Some(addr);
        s.picking = false;
        if let Some(cur) = &s.current
            && cur.addr != addr
        {
            cur.leave(HeadsetGoodbye::OtherPc);
        }
    }

    /// Disconnects and waits for the user to pick a PC in the menu.
    pub fn change_host(&self) {
        self.leave(HeadsetGoodbye::OtherPc);
    }

    /// The PC in use was forgotten in the menu: leaves it, telling it so, and does not
    /// come back on its own.
    pub fn forget_current(&self) {
        self.leave(HeadsetGoodbye::Forgotten);
    }

    fn leave(&self, reason: HeadsetGoodbye) {
        let mut s = self.lock();
        s.choice = None;
        s.picking = true;
        if let Some(cur) = s.current.take() {
            cur.leave(reason);
        }
        self.0.1.notify_all();
    }

    /// The session with the PC at `addr` is open: leaving it from now on says goodbye.
    fn opened(&self, addr: SocketAddr, feedback: &Feedback) {
        if let Some(cur) = self.lock().current.as_mut().filter(|c| c.addr == addr) {
            cur.feedback = Some(feedback.clone());
        }
    }

    /// What the headset is now (its preferences, worn or not, its microphone): what the
    /// next `Open` says.
    pub fn set_headset(&self, state: HeadsetState) {
        self.lock().headset = state;
    }

    fn headset(&self) -> HeadsetState {
        self.lock().headset.clone()
    }

    /// The menu's language changed.
    pub fn set_texts(&self, texts: Texts) {
        self.lock().texts = Some(texts);
    }

    fn texts(&self) -> Option<Texts> {
        self.lock().texts.clone()
    }

    /// A PC was forgotten in the menu: the next connection reads the trust file again.
    pub fn trust_changed(&self) {
        self.lock().trust_changed = true;
    }

    /// Put on or taken off; connecting waits until the headset is worn.
    pub fn set_worn(&self, worn: bool) {
        self.lock().taken_off = !worn;
        self.0.1.notify_all();
    }

    fn wait_until_worn(&self) {
        let mut s = self.lock();
        if s.taken_off && !s.quit {
            info!("headset not worn: not connecting");
        }
        while s.taken_off && !s.quit {
            s = self.0.1.wait(s).unwrap();
        }
    }

    /// The app is closing. Android may start it again in the same process, so the
    /// network thread must not outlive it: its session would keep the PC busy (the PC
    /// serves one headset at a time) and the new start could not connect.
    pub fn shut_down(&self) {
        let mut s = self.lock();
        s.quit = true;
        if let Some(cur) = &s.current {
            cur.leave(HeadsetGoodbye::Quit);
        }
        self.0.1.notify_all();
    }

    fn quitting(&self) -> bool {
        self.lock().quit
    }

    /// Waits `d`, or less once the app is closing.
    fn rest(&self, d: Duration) {
        let until = Instant::now() + d;
        let mut s = self.lock();
        while !s.quit {
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                return;
            };
            s = self.0.1.wait_timeout(s, left).unwrap().0;
        }
    }

    /// The user's answer to "Pair with this PC?".
    pub fn answer_offer(&self, pair: bool) {
        let mut s = self.lock();
        if let Some((key, addr)) = s.offer.take() {
            if pair {
                s.approved = Some((key, Instant::now() + APPROVAL));
                s.choice = Some(addr);
                s.picking = false;
            } else {
                // Not this PC: wait for the user to pick one.
                s.approved = None;
                s.choice = None;
                s.picking = true;
            }
        }
        self.0.1.notify_all();
    }

    fn approved(&self, key: &PeerKey) -> bool {
        self.lock()
            .approved
            .is_some_and(|(k, until)| &k == key && Instant::now() < until)
    }

    /// Asks the user about an unknown PC; the connection is closed meanwhile.
    fn offer(&self, key: PeerKey, addr: SocketAddr) {
        let mut s = self.lock();
        s.offer = Some((key, addr));
        s.approved = None;
    }

    /// While the user has not answered about an unknown PC, nothing connects.
    fn wait_for_answer(&self) {
        let mut s = self.lock();
        while s.offer.is_some() && !s.taken_off && !s.quit {
            s = self.0.1.wait(s).unwrap();
        }
    }

    /// Swaps which side shows the pairing code and restarts pairing.
    pub fn switch_pairing_side(&self) {
        let mut s = self.lock();
        s.pc_shows_code = !s.pc_shows_code;
        s.typed_code = None;
        if let Some(cur) = &s.current {
            cur.closer.close();
        }
        self.0.1.notify_all();
    }

    /// The code the user typed for the PC's code.
    pub fn submit_code(&self, code: String) {
        self.lock().typed_code = Some(code);
        self.0.1.notify_all();
    }

    fn pc_shows_code(&self) -> bool {
        self.lock().pc_shows_code
    }

    /// Waits for a code typed for the PC at `addr`; `None` if the connection was
    /// dropped meanwhile. `alive` looks at the connection for a moment (at most
    /// [`LOOK_EVERY`]) and says whether the PC is still there.
    fn wait_code(&self, addr: SocketAddr, mut alive: impl FnMut() -> bool) -> Option<String> {
        self.lock().typed_code = None;
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            {
                let mut s = self.lock();
                if let Some(code) = s.typed_code.take() {
                    return Some(code);
                }
                // Dropped by "change PC" or "the other way round".
                if s.current.as_ref().is_none_or(|c| c.addr != addr) {
                    return None;
                }
            }
            // Looked at without holding the lock (the menu takes it every frame).
            if !alive() || Instant::now() >= deadline {
                return None;
            }
        }
    }
}

/// Starts the network thread; it ends soon after [`Steering::shut_down`].
pub fn spawn(
    events: Sender<NetEvent>,
    discovery: Discovery,
    steering: Steering,
    sound: Sound,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("net".into())
        .spawn(move || {
            // It reads the frames off the network.
            crate::xrutil::raise_thread_priority(crate::xrutil::Priority::Video);
            // The socket PCs announce themselves to. A network thread of an earlier
            // start that has not stopped yet may still hold it: tried again each round
            // until this one has it.
            let mut beacon = None;
            let mut beacon_told = false;
            let identity = match Identity::load_or_create(&discovery.identity_file) {
                Ok(id) => id,
                Err(e) => {
                    warn!("cannot create the headset key: {e}");
                    return;
                }
            };
            info!("headset key {}", secure::fingerprint(&identity.public));
            let me = Headset {
                identity,
                name: discovery.name.clone(),
                sound,
            };
            let mut trusted = TrustStore::load(&discovery.trust_file);
            let mut finder = Finder::default();
            let mut probing = Probing::default();
            loop {
                // Set when the last session broke off without a word after it had run
                // for a while: the PC is most likely still there, so the next attempt
                // goes at once instead of after the usual pause.
                let mut again_now = false;
                steering.wait_until_worn();
                steering.wait_for_answer();
                if steering.quitting() {
                    info!("network stopped");
                    return;
                }
                if std::mem::take(&mut steering.lock().trust_changed) {
                    trusted = TrustStore::load(&discovery.trust_file);
                }
                if beacon.is_none() && discovery.only.is_none() {
                    match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, prostir_proto::DISCOVERY_PORT)) {
                        Ok(socket) => {
                            if beacon_told {
                                info!("discovery socket open");
                            }
                            beacon = Some(socket);
                        }
                        Err(e) if !beacon_told => {
                            warn!("discovery socket: {e}; trying again each round");
                            beacon_told = true;
                        }
                        Err(_) => {}
                    }
                }
                if let Some(stream) = connect(
                    beacon.as_ref(),
                    &discovery,
                    &steering,
                    &mut finder,
                    &events,
                    &mut probing,
                ) {
                    if steering.quitting() {
                        info!("network stopped");
                        return;
                    }
                    let peer = stream.peer_addr().ok();
                    let name = peer.and_then(|p| finder.name_of(p));
                    steering.lock().current = current(&stream);
                    let mut ended = Ended::default();
                    let began = Instant::now();
                    let outcome = run_session(
                        stream,
                        &events,
                        name,
                        &steering,
                        &me,
                        &mut trusted,
                        &mut ended,
                    );
                    let why = match (&outcome, ended.goodbye) {
                        (_, Some(reason)) => Disconnect::Goodbye(reason),
                        (Err(e), None)
                            if ended.opened && e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            Disconnect::Silence
                        }
                        _ => Disconnect::Closed,
                    };
                    match outcome {
                        Ok(()) if ended.goodbye.is_some() => {
                            info!("host {peer:?} ended the session: {why:?}");
                        }
                        Ok(()) => info!("host {peer:?} closed the connection"),
                        Err(e) => warn!("connection to {peer:?} lost: {e}"),
                    }
                    // Remembered only once the PC proved it is a trusted one: a single
                    // connection to anything else must not redirect the next start.
                    if ended.opened
                        && let Some(p) = peer.filter(|p| !p.ip().is_loopback())
                    {
                        let _ = std::fs::write(&discovery.last_host_file, p.to_string());
                    }
                    steering.lock().current = None;
                    // A search after a session probes soon again.
                    if ended.opened {
                        probing = Probing::default();
                    }
                    if events.send(NetEvent::Disconnected { why }).is_err() || steering.quitting() {
                        info!("network stopped");
                        return;
                    }
                    // The PC said what is wrong (no encoder, say): the message stays
                    // up, and retrying every 0.3 s would only rebuild its monitors.
                    if ended
                        .refused
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
                    {
                        steering.rest(Duration::from_secs(10));
                    } else {
                        again_now = quick_retry(&ended, began.elapsed());
                    }
                }
                if !again_now {
                    steering.rest(RETRY_PAUSE);
                }
            }
        })
        .expect("spawn net thread")
}

/// Between attempts to connect.
const RETRY_PAUSE: Duration = Duration::from_millis(300);
/// A session must have run this long for the next attempt to go at once: one that
/// breaks right after it opens is retried only after [`RETRY_PAUSE`], so a PC that
/// keeps dropping sessions is not hammered.
const QUICK_RETRY_AFTER: Duration = Duration::from_secs(2);

/// Whether to connect again at once after a session that ended as `ended` says,
/// having lasted `lasted`: only one that opened, ran a while and broke off without
/// the PC saying why (a goodbye or a refusal keep their pauses).
fn quick_retry(ended: &Ended, lasted: Duration) -> bool {
    ended.opened
        && ended.goodbye.is_none()
        && ended.refused.is_none()
        && lasted >= QUICK_RETRY_AFTER
}

/// What steering needs of a connection that is being opened.
fn current(stream: &TcpStream) -> Option<Current> {
    let addr = stream.peer_addr().ok()?;
    let socket = stream.try_clone().ok()?;
    let closer = Closer::new(move || {
        let _ = socket.shutdown(Shutdown::Both);
    });
    Some(Current {
        addr,
        closer,
        feedback: None,
    })
}

/// The PC's name with its address, so a look-alike name from the network stands out.
fn offer_label(host: &str, addr: SocketAddr) -> String {
    let ip = addr.ip().to_string();
    if host == ip || addr.ip().is_loopback() {
        host.to_owned()
    } else {
        format!("{host} {ip}")
    }
}

/// A PC that announced itself refused the connection: most likely a firewall.
fn blocked(finder: &Finder, events: &Sender<NetEvent>, addr: SocketAddr) {
    if let Some(name) = finder.name_of(addr) {
        let _ = events.send(NetEvent::Unreachable(Some(name)));
    }
}

fn try_connect(addr: SocketAddr) -> Option<TcpStream> {
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok()
}

fn read_addr(path: &PathBuf) -> Option<SocketAddr> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_addr(text.trim())
}

/// `IP` or `IP:PORT`.
pub fn parse_addr(text: &str) -> Option<SocketAddr> {
    text.parse::<SocketAddr>().ok().or_else(|| {
        text.parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, prostir_proto::TCP_PORT))
    })
}

/// PCs heard on the network recently.
#[derive(Default)]
struct Finder {
    seen: HashMap<SocketAddr, (String, Instant)>,
    reported: Vec<FoundHost>,
}

impl Finder {
    /// Listens for beacons for a moment and reports changes to the menu. Looks every
    /// 100 ms whether the app is closing, and stops then.
    fn listen(&mut self, beacon: &UdpSocket, events: &Sender<NetEvent>, steering: &Steering) {
        let until = Instant::now() + Duration::from_millis(1200);
        let mut buf = [0u8; 512];
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if steering.quitting() {
                return;
            }
            let slice = left.clamp(Duration::from_millis(1), Duration::from_millis(100));
            let _ = beacon.set_read_timeout(Some(slice));
            let (n, from) = match beacon.recv_from(&mut buf) {
                Ok(got) => got,
                // Nothing in this slice: the next one.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            };
            if let Some((port, name)) = prostir_proto::decode_beacon(&buf[..n]) {
                self.seen
                    .insert(SocketAddr::new(from.ip(), port), (name, Instant::now()));
            }
        }
        self.seen
            .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(6));
        let mut hosts: Vec<FoundHost> = self
            .seen
            .iter()
            .map(|(addr, (name, _))| FoundHost {
                name: name.clone(),
                addr: *addr,
            })
            .collect();
        hosts.sort_by(|a, b| a.name.cmp(&b.name).then(a.addr.cmp(&b.addr)));
        if hosts != self.reported {
            self.reported.clone_from(&hosts);
            let _ = events.send(NetEvent::Hosts(hosts));
        }
    }

    fn name_of(&self, addr: SocketAddr) -> Option<String> {
        self.seen.get(&addr).map(|(n, _)| n.clone())
    }
}

fn connect(
    beacon: Option<&UdpSocket>,
    discovery: &Discovery,
    steering: &Steering,
    finder: &mut Finder,
    events: &Sender<NetEvent>,
    probing: &mut Probing,
) -> Option<TcpStream> {
    if let Some(only) = discovery.only {
        return try_connect(only);
    }
    let (choice, picking) = {
        let s = steering.lock();
        (s.choice, s.picking)
    };
    // USB: the host keeps `adb reverse` active, so localhost reaches the PC. Not when
    // the user is picking a PC or picked one on the network: that choice comes first.
    let usb = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), prostir_proto::TCP_PORT);
    if !picking
        && choice.is_none_or(|c| c.ip().is_loopback())
        && let Some(stream) = try_connect(usb)
    {
        return Some(stream);
    }
    // Each attempt takes a moment: none starts once the app is closing.
    if steering.quitting() {
        return None;
    }
    if let Some(addr) = choice {
        if let Some(stream) = try_connect(addr) {
            return Some(stream);
        }
        blocked(finder, events, addr);
    }
    if let Some(beacon) = beacon {
        finder.listen(beacon, events, steering);
    }
    if picking || choice.is_some() || steering.quitting() {
        // The user is choosing (or their choice is unreachable): only listen.
        return None;
    }
    if let Some(stream) = discovery
        .host_file
        .as_ref()
        .and_then(read_addr)
        .and_then(try_connect)
    {
        return Some(stream);
    }
    if steering.quitting() {
        return None;
    }
    if let Some(stream) = read_addr(&discovery.last_host_file).and_then(try_connect) {
        return Some(stream);
    }
    if steering.quitting() {
        return None;
    }
    // Exactly one PC around: just use it. Several: wait for a pick in the menu.
    match finder.reported.as_slice() {
        [only] => {
            let addr = only.addr;
            let stream = try_connect(addr);
            if stream.is_none() {
                blocked(finder, events, addr);
            }
            return stream;
        }
        [] => {}
        _ => return None,
    }
    // Some networks drop broadcasts: now and then probe the local /24.
    if probing.due(Instant::now()) && !steering.quitting() {
        scan_subnet()
    } else {
        None
    }
}

/// The first probe of the local network for a PC that does not announce itself, and
/// how much longer each next one waits, up to [`PROBE_AT_MOST`].
const PROBE_FIRST: Duration = Duration::from_millis(7500);
const PROBE_AT_MOST: Duration = Duration::from_secs(60);

/// When to probe the local network next: at once when the search starts, then less
/// and less often while no PC is found (each probe starts 253 short connections).
#[derive(Default)]
struct Probing {
    /// When the next probe is due, and the wait after it; `None` before the first.
    next: Option<(Instant, Duration)>,
}

impl Probing {
    /// Whether to probe now; if so, the next one waits twice as long as the last.
    fn due(&mut self, now: Instant) -> bool {
        match self.next {
            Some((at, _)) if now < at => false,
            Some((_, wait)) => {
                self.next = Some((now + wait, (wait * 2).min(PROBE_AT_MOST)));
                true
            }
            None => {
                self.next = Some((now + PROBE_FIRST, PROBE_FIRST * 2));
                true
            }
        }
    }
}

/// The headset's own IPv4 address on the network the default route uses.
fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // UDP connect only picks a route; nothing is sent.
    socket
        .connect((Ipv4Addr::new(10, 255, 255, 254), 9))
        .or_else(|_| socket.connect((Ipv4Addr::new(192, 168, 255, 254), 9)))
        .ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

fn scan_subnet() -> Option<TcpStream> {
    let me = local_ipv4()?;
    let [a, b, c, own] = me.octets();
    let found = Mutex::new(None);
    std::thread::scope(|scope| {
        for d in (1..=254u8).filter(|&d| d != own) {
            let found = &found;
            scope.spawn(move || {
                let addr =
                    SocketAddr::new(Ipv4Addr::new(a, b, c, d).into(), prostir_proto::TCP_PORT);
                if let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) {
                    found.lock().unwrap().get_or_insert(stream);
                }
            });
        }
    });
    let stream = found.into_inner().unwrap()?;
    info!(
        "found host by scanning {a}.{b}.{c}.0/24: {:?}",
        stream.peer_addr()
    );
    Some(stream)
}

/// What pairing needs from the current connection.
struct Session<'a> {
    addr: SocketAddr,
    receiver: &'a mut dyn RecordReceiver,
    packets: &'a mut Assembler,
    /// Messages read while waiting for the user, not handled yet.
    pending: VecDeque<HostMsg>,
    feedback: &'a Feedback,
    events: &'a Sender<NetEvent>,
    steering: &'a Steering,
    identity: &'a Identity,
    /// What the PC calls this headset (`PairBegin`).
    name: &'a str,
    peer: &'a PeerInfo,
    host: &'a str,
}

impl Session<'_> {
    /// The next message from the PC before the session.
    fn read(&mut self) -> std::io::Result<HostMsg> {
        match self.pending.pop_front() {
            Some(msg) => Ok(msg),
            None => HostMsg::recv(self.receiver, self.packets),
        }
    }

    /// Whether the PC is still there, looking at the connection for a moment: what
    /// arrives meanwhile is kept for [`Session::read`].
    fn alive(
        receiver: &mut dyn RecordReceiver,
        packets: &mut Assembler,
        pending: &mut VecDeque<HostMsg>,
    ) -> bool {
        receiver.set_idle_limit(Some(LOOK_EVERY));
        let alive = match receiver.recv() {
            Ok(record) => match packets.push(record).map_err(std::io::Error::from) {
                Ok(Some(m)) => match HostMsg::from_message(m) {
                    Ok(msg) => {
                        pending.extend(msg);
                        true
                    }
                    Err(_) => false,
                },
                Ok(None) => true,
                Err(_) => false,
            },
            Err(e) => e.kind() == std::io::ErrorKind::TimedOut,
        };
        receiver.set_idle_limit(Some(Duration::from_secs(300)));
        alive
    }

    /// Pairs if the user agreed to; otherwise asks them and closes the connection
    /// (the PC must not be held waiting for an answer).
    fn pair_if_approved(&mut self, trusted: &mut TrustStore) -> std::io::Result<()> {
        if self.steering.approved(&self.peer.key) {
            return self.pair(trusted);
        }
        let addr = self.addr;
        self.steering.offer(self.peer.key, addr);
        let _ = self.events.send(NetEvent::PairOffer {
            host: offer_label(self.host, addr),
            why: OfferWhy::New,
        });
        Err(std::io::Error::other(
            "waiting for the user to approve pairing",
        ))
    }

    /// Pairs with the PC: up to [`PAIR_ATTEMPTS`] codes, then asks the user again.
    fn pair(&mut self, trusted: &mut TrustStore) -> std::io::Result<()> {
        // The user needs time to read and type the code.
        self.receiver.set_idle_limit(Some(Duration::from_secs(300)));
        for _ in 0..PAIR_ATTEMPTS {
            let pc_shows = self.steering.pc_shows_code();
            // The PC makes its code only when asked, so ask first and then wait for
            // the user to type what it shows.
            self.feedback.send(ClientMsg::PairBegin {
                headset_shows: !pc_shows,
                name: self.name.to_owned(),
            })?;
            let code = if pc_shows {
                let _ = self.events.send(NetEvent::Pairing {
                    host: self.host.to_owned(),
                    code: None,
                });
                let Session {
                    addr,
                    receiver,
                    packets,
                    pending,
                    steering,
                    ..
                } = self;
                steering
                    .wait_code(*addr, || Self::alive(&mut **receiver, packets, pending))
                    .ok_or_else(|| std::io::Error::other("pairing interrupted"))?
            } else {
                let code = secure::pairing_code();
                let _ = self.events.send(NetEvent::Pairing {
                    host: self.host.to_owned(),
                    code: Some(format!("{} {}", &code[..3], &code[3..])),
                });
                code
            };
            let (pairing, msg) = Pairing::start(&code);
            self.feedback.send(ClientMsg::PairMsg { msg })?;
            let theirs = loop {
                match self.read()? {
                    HostMsg::PairMsg { msg } => break Some(msg),
                    // The PC gave up this attempt (e.g. its dialog was cancelled).
                    HostMsg::PairResult { .. } => break None,
                    // Not trusted yet: only a short line of plain text gets shown.
                    HostMsg::Error { text } => {
                        let text: String =
                            text.chars().filter(|c| !c.is_control()).take(160).collect();
                        let _ = self.events.send(NetEvent::HostError(text));
                    }
                    other => return Err(std::io::Error::other(format!("unexpected {other:?}"))),
                }
            };
            let Some(theirs) = theirs else {
                let _ = self.events.send(NetEvent::PairingFailed);
                continue;
            };
            let key = pairing.finish(&theirs)?;
            let own = &self.identity.public;
            self.feedback.send(ClientMsg::PairConfirm {
                tag: confirmation(&key, "headset", self.peer, own),
            })?;
            match self.read()? {
                HostMsg::PairResult { tag: Some(tag) }
                    if tags_match(&tag, &confirmation(&key, "pc", self.peer, own)) =>
                {
                    trusted.add(self.peer.key, self.host)?;
                    info!(
                        "paired with {} ({})",
                        self.host,
                        secure::fingerprint(&self.peer.key)
                    );
                    let _ = self.events.send(NetEvent::Paired);
                    self.receiver.set_idle_limit(Some(Duration::from_secs(10)));
                    return Ok(());
                }
                HostMsg::PairResult { .. } => {
                    warn!("pairing with {}: the codes did not match", self.host);
                    let _ = self.events.send(NetEvent::PairingFailed);
                }
                other => return Err(std::io::Error::other(format!("unexpected {other:?}"))),
            }
        }
        // Several wrong codes: stop and let the user decide whether to go on.
        let addr = self.addr;
        self.steering.offer(self.peer.key, addr);
        let _ = self.events.send(NetEvent::PairOffer {
            host: offer_label(self.host, addr),
            why: OfferWhy::WrongCodes,
        });
        Err(std::io::Error::other("pairing stopped after wrong codes"))
    }
}

struct StreamSlot {
    /// The generation its frames belong to.
    generation: Gen,
    /// Shared with the stream's decoder.
    queue: Arc<FrameQueue>,
    /// Frames are skipped until a keyframe: from the start, and after the decoder
    /// fell behind.
    keyframe: KeyframeWait,
}

impl Drop for StreamSlot {
    /// The stream is over here: its decoder ends once it took what waits.
    fn drop(&mut self) {
        self.queue.close();
    }
}

/// How a session went, for what to do next.
#[derive(Default)]
struct Ended {
    /// The PC is trusted and the session started.
    opened: bool,
    /// When the PC last sent an error: right before it hung up means it cannot
    /// serve the headset right now.
    refused: Option<Instant>,
    /// The PC ended the session on purpose, for this reason.
    goodbye: Option<PcGoodbye>,
}

fn run_session(
    stream: TcpStream,
    events: &Sender<NetEvent>,
    name: Option<String>,
    steering: &Steering,
    me: &Headset,
    trusted: &mut TrustStore,
    ended: &mut Ended,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let addr = stream.peer_addr()?;
    let identity = &me.identity;
    let Link {
        mut sender,
        mut receiver,
        closer,
        peer,
    } = NoiseTcp::connect(stream, identity, Arc::new(Plain))?;
    // However the session ends, the connection closes: nothing stays blocked on it.
    struct CloseOnExit(Closer);
    impl Drop for CloseOnExit {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let _close = CloseOnExit(closer.clone());
    let host = name.clone().unwrap_or_else(|| match steering.texts() {
        Some(texts) if addr.ip().is_loopback() => texts.t("pc-usb").to_owned(),
        _ => addr.ip().to_string(),
    });
    // Versions first, before pairing: this exchange is the same in every version, so
    // a PC of any version says which side to update.
    let app = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
    handshake::send_hello(&mut *sender, &app)?;
    let language = match handshake::read_answer(&mut *receiver)? {
        Answer::Welcome { language, .. } => Lang::from_tag(&language),
        Answer::Incompatible { protocol, app } => {
            let _ = events.send(NetEvent::Incompatible {
                pc_newer: prostir_proto::peer_is_newer(protocol, &app),
                pc_app: app,
            });
            // Nothing changes until one side is updated: try again only now and then.
            steering.rest(Duration::from_secs(10));
            return Ok(());
        }
    };
    // From here on, packets.
    let feedback = Feedback::start(sender, closer)?;
    // Nor does its writer outlive it.
    struct StopOnExit(Feedback);
    impl Drop for StopOnExit {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let _stop = StopOnExit(feedback.clone());
    let mut packets = HostMsg::assembler();
    let opened = {
        let mut session = Session {
            addr,
            receiver: &mut *receiver,
            packets: &mut packets,
            pending: VecDeque::new(),
            feedback: &feedback,
            events,
            steering,
            identity,
            name: &me.name,
            peer: &peer,
            host: &host,
        };
        // A PC we have never paired with: pair only once the user said so in the menu,
        // and never on our own (a hostile PC on the network could keep guessing codes).
        if !trusted.contains(&peer.key) {
            session.pair_if_approved(trusted)?;
        }
        let opened_with = steering.headset();
        let open = opened_with
            .open()
            .map_err(|e| std::io::Error::other(format!("Open: {e}")))?;
        feedback.send(open.clone())?;
        // The PC is trusted from here: its snapshot may be as big as it needs.
        session.packets.set_limits(HostMsg::limits(true));
        let snapshot = loop {
            match session.read()? {
                HostMsg::Snapshot { parts } => break parts,
                HostMsg::PairingRequired => {
                    session.packets.set_limits(HostMsg::limits(false));
                    session.pair_if_approved(trusted)?;
                    feedback.send(open.clone())?;
                    session.packets.set_limits(HostMsg::limits(true));
                }
                // The PC cannot open the session (no encoder, say), and closes the
                // connection after saying why.
                HostMsg::Error { text } => {
                    ended.refused = Some(Instant::now());
                    let _ = events.send(NetEvent::HostError(text));
                }
                other => return Err(std::io::Error::other(format!("unexpected {other:?}"))),
            }
        };
        let state = PcState::from_snapshot(&snapshot).map_err(std::io::Error::from)?;
        ended.opened = true;
        // Both sides ping every second: a few seconds of silence is a dead link.
        receiver.set_idle_limit(Some(SILENCE));
        feedback.start_pings();
        steering.opened(addr, &feedback);
        let _ = events.send(NetEvent::Connected {
            pc: PcId::of(&peer.key),
            addr,
            name,
            feedback: feedback.clone(),
            language,
            opened_with: opened_with.clone(),
        });
        // The snapshot's parts, as they would come one by one.
        for part in state.parts() {
            tell_part(events, &part);
        }
        (state, opened_with)
    };
    let (mut pc, opened_with) = opened;

    let mut streams: HashMap<DisplayId, StreamSlot> = HashMap::new();
    // Started on the first sound slice, stopped with the session.
    let mut player: Option<Player> = None;
    // Runs only while a PC app records from the headset microphone and the user's
    // microphone switch on the headset is on.
    let mut recorder: Option<Recorder> = None;
    let sound = &me.sound;
    // Pauses between messages: with sound on, anything over ~10 ms is something
    // (a frame being written, Wi-Fi) holding the rest up.
    let mut gaps = Gaps::default();
    let mut media_bytes = 0u64;
    let mut report = Period::new(REPORT_EVERY);
    let caps = opened_with.caps.unwrap_or_default();
    let result = loop {
        let msg = match HostMsg::recv(&mut *receiver, &mut packets) {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        let now = Instant::now();
        gaps.event(now);
        media_bytes += match &msg {
            HostMsg::Frame { data, .. } => data.len(),
            HostMsg::Audio { pcm } => pcm.len(),
            _ => 0,
        } as u64;
        if let Some(window) = report.due(now) {
            info!(
                "received {} messages, {:.1} Mbit/s of video and sound; pauses {}; \
                 round trip {}; microphone queue max {}",
                gaps.events(),
                media_bytes as f64 * 8.0 / window.as_secs_f64() / 1e6,
                gaps.summary(window),
                feedback.outbox.take_round_trips().summary_ms(),
                feedback.outbox.take_lossy_max()
            );
            gaps.restart();
            media_bytes = 0;
        }
        // A part of the PC's state: kept, checked, and told to the app. Pictures and
        // sound, most of what comes, are never parts: they go on as they are, not
        // copied.
        let msg = if matches!(msg, HostMsg::Frame { .. } | HostMsg::Audio { .. }) {
            Some(msg)
        } else {
            match pc.apply(msg.clone()) {
                Ok(None) => {
                    tell_part(events, &msg);
                    if let HostMsg::Sound { part } = &msg
                        && !part.to_headset
                    {
                        player = None;
                    }
                    None
                }
                Ok(Some(msg)) => Some(msg),
                Err(e) => break Err(e.into()),
            }
        };
        // The microphone streams while a PC program records from it (`UpWanted`) and
        // the user's switch on the headset is on.
        let record =
            pc.up_wanted.get(&state::UpKind::Mic).is_some_and(|u| u.on) && sound.mic.allowed();
        if record != recorder.is_some() {
            recorder = record.then(|| Recorder::start(sound.mic.clone(), feedback.clone()));
        }
        let Some(msg) = msg else {
            continue;
        };
        match msg {
            HostMsg::StreamStart {
                id,
                generation,
                w,
                h,
                codec,
            } => {
                info!("stream {} {generation:?}: {w}x{h} {codec:?}", id.0);
                if streams.remove(&id).is_some() {
                    let _ = events.send(NetEvent::StreamEnd { id });
                }
                // The PC sends only what this headset decodes. One that does not
                // (an older PC) is that display's trouble, not the session's: it
                // shows nothing, and its frames go nowhere.
                if !caps.fits(codec, w, h) {
                    warn!(
                        "stream {}: {w}x{h} in {codec:?} is too large to decode",
                        id.0
                    );
                    let name = pc
                        .displays
                        .as_ref()
                        .and_then(|d| d.get(id))
                        .map_or_else(String::new, |d| crate::ui::display_label(&d.name));
                    let _ = events.send(NetEvent::TooBig {
                        name,
                        size: (w, h),
                        codec,
                    });
                    continue;
                }
                let queue = Arc::new(FrameQueue::default());
                let mut keyframe = KeyframeWait::default();
                // The PC starts every stream with one.
                keyframe.expect();
                streams.insert(
                    id,
                    StreamSlot {
                        generation,
                        queue: queue.clone(),
                        keyframe,
                    },
                );
                let _ = events.send(NetEvent::StreamStart {
                    id,
                    generation,
                    width: w,
                    height: h,
                    codec,
                    frames: queue.frames(),
                    feedback: feedback.clone(),
                });
            }
            HostMsg::Frame {
                id,
                generation,
                seq,
                pts_us,
                keyframe,
                data,
            } => {
                // A frame of another start of the stream is of no use.
                let Some(slot) = streams.get_mut(&id).filter(|s| s.generation == generation) else {
                    continue;
                };
                // Asked again while frames come and no keyframe: the PC may have
                // ignored the last ask (it answers one every 300 ms at most).
                let verdict = slot.keyframe.passes(keyframe, now);
                if verdict.ask {
                    feedback.send(ClientMsg::RequestKeyframe { id, generation })?;
                }
                // Its decoder is gone (the monitor was closed here): nobody takes it.
                if !verdict.pass || slot.queue.abandoned() {
                    continue;
                }
                let frame = Frame {
                    received: now,
                    seq,
                    pts_us,
                    keyframe,
                    data,
                };
                if slot.queue.push(frame, now) == Pushed::Behind {
                    warn!("decoder {} is behind, dropping until next keyframe", id.0);
                    if slot.keyframe.lost(now) {
                        feedback.send(ClientMsg::RequestKeyframe { id, generation })?;
                    }
                    #[cfg(all(feature = "selftest", not(feature = "store")))]
                    crate::selftest::dropped_to_keyframe();
                }
            }
            HostMsg::StreamEnd { id, generation } => {
                if streams.get(&id).is_some_and(|s| s.generation == generation) {
                    streams.remove(&id);
                    let _ = events.send(NetEvent::StreamEnd { id });
                }
            }
            HostMsg::Reply { req, outcome } => {
                let _ = events.send(NetEvent::Reply { req, outcome });
            }
            HostMsg::Problem { error, detail, .. } => {
                let _ = events.send(NetEvent::Problem { error, detail });
            }
            HostMsg::Error { text } => {
                ended.refused = Some(Instant::now());
                let _ = events.send(NetEvent::HostError(text));
            }
            HostMsg::Audio { pcm } => {
                player
                    .get_or_insert_with(|| Player::spawn(sound.gains.clone()))
                    .push(pcm);
            }
            HostMsg::Command { req, asked } => {
                let _ = events.send(NetEvent::Command { req, asked });
            }
            HostMsg::Notice { text } => {
                let _ = events.send(NetEvent::Notice(text));
            }
            // Answered at once, from here: the round trip is the network's.
            HostMsg::Ping { seq } => feedback.send(ClientMsg::Pong { seq })?,
            HostMsg::Pong { seq } => feedback.outbox.pong(seq),
            HostMsg::Goodbye { reason } => {
                let reason = PcGoodbye::from_code(reason);
                ended.goodbye = Some(reason);
                // It no longer trusts this headset: ask the user before pairing again,
                // and connect to nothing meanwhile.
                if reason == PcGoodbye::Forgotten {
                    steering.offer(peer.key, addr);
                    let _ = events.send(NetEvent::PairOffer {
                        host: offer_label(&host, addr),
                        why: OfferWhy::Forgotten,
                    });
                }
                break Ok(());
            }
            // Only before the session: ignored in it, as parts already taken are.
            _ => {}
        }
    };
    for id in streams.into_keys() {
        let _ = events.send(NetEvent::StreamEnd { id });
    }
    result
}

/// Tells the app about a part of the PC's state.
fn tell_part(events: &Sender<NetEvent>, msg: &HostMsg) {
    let _ = events.send(NetEvent::Part(msg.clone()));
}
