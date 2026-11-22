//! The network thread (step 7 note, 4.5 and 6): it does what the headset's core asks
//! ([`NetCmd`], from [`crate::session`]) and tells what happened ([`NetEvent`]); whom to
//! reach and when is never its own choice. It keeps the files of the trusted PCs
//! ([`TrustStore`], and each one's find secret and last address, see
//! [`discovery::Known`]: read and written here only) and says the list of them in
//! [`NetEvent::Paired`].
//!
//! Each connection has its threads: `net-dial` connects and, before trust, pairs as the
//! user agreed; a trusted PC's session runs in `link-<pc>` with its writer
//! `net-writer` (see [`link`]). Queries for PCs go from `net-find` (see
//! [`discovery`]). The PC's sound and the microphone go through [`Media`]
//! (AAudio on the headset, a fake in tests), so this code builds and is tested on any
//! system, against the testkit's fake PC and the PC app with its fake desktop.

#![expect(
    clippy::disallowed_methods,
    reason = "the network thread is part of the shell: its sockets, files and clock are its own"
)]

pub mod discovery;
mod link;
#[cfg(test)]
mod tests;

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{info, warn};
use prostir_proto::link::{Closer, RecordSender};
use prostir_proto::request::{Asked, Command, Outcome};
use prostir_proto::secure::{self, Identity, TrustStore};
use prostir_proto::state::{DisplayId, ErrorCode, Gen, HeadsetState, ReqId};
use prostir_proto::{ClientMsg, Codec, HeadsetGoodbye, HostMsg};

use crate::connecting::Ended;
use crate::frames::Frames;
use crate::ids::PcId;
use crate::lang::Lang;
use crate::outbox::{Lanes, Outbox};
pub use crate::session::{Disconnect, FoundHost};
use crate::session::{NetCmd, PairedPc, StepId};
use crate::ui::OfferWhy;

/// After a goodbye, how long the PC has to read it and close the connection before
/// this side does.
const GOODBYE_WAIT: Duration = Duration::from_secs(1);
/// The app closing: how long the connections' threads get to say goodbye and end, and
/// then, closed, to end at all.
const QUIT_WITHIN: Duration = Duration::from_millis(700);
const CLOSE_WITHIN: Duration = Duration::from_millis(200);

/// Messages to the PC (see [`crate::outbox`]). Sending only queues them.
#[derive(Clone)]
pub struct Feedback {
    outbox: Arc<Outbox>,
}

impl Feedback {
    /// Starts the writer thread for one session. It ends (and closes the connection,
    /// which ends the session) when a write fails or times out, or once the outbox is
    /// closed.
    /// The thread is the network thread's to wait for when the app closes.
    fn start(
        mut sender: Box<dyn RecordSender>,
        closer: Closer,
        shared: &Shared,
    ) -> io::Result<Self> {
        let outbox = Arc::new(Outbox::default());
        let queued = outbox.clone();
        let media = shared.media.clone();
        let writer = std::thread::Builder::new()
            .name("net-writer".into())
            .spawn(move || {
                media.raise_priority();
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
                    match lanes.send(pick, &mut *sender) {
                        // The microphone's slices are filled again (see `up_buffer`).
                        Ok(Some(sent)) => queued.sent(sent),
                        Ok(None) => {}
                        Err(e) => {
                            warn!("sending to the PC failed: {e}");
                            return;
                        }
                    }
                }
                // After the goodbye nothing more goes out; the PC closes the connection
                // once it read it, and what it sends meanwhile is still read, so a reset
                // cannot throw the goodbye away: until the reader sees the end. If the PC
                // does not close, this side does.
                if queued.said_goodbye() {
                    let _ = sender.finish();
                    queued.wait_closed(GOODBYE_WAIT);
                }
            })?;
        shared.note(Inbox::Thread(writer));
        Ok(Self { outbox })
    }

    /// Ends the session on purpose, telling the PC why.
    fn goodbye(&self, reason: HeadsetGoodbye) {
        self.outbox.say_goodbye(reason);
    }

    /// Queues a message. While the network is stuck, microphone sound and frame
    /// acknowledgements beyond about 200 ms of them are dropped, the oldest first;
    /// nothing else is. An error means the session is over.
    pub fn send(&self, msg: ClientMsg) -> io::Result<()> {
        self.outbox.push(msg)
    }

    /// An empty buffer for a slice of the microphone of `bytes`: one of the slices sent
    /// before, so recording needs no new memory for each.
    pub fn up_buffer(&self, bytes: usize) -> Vec<u8> {
        self.outbox.up_buffer(bytes)
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

/// The PC's sound and the headset's microphone for one session (step 7 note, 4.1):
/// AAudio on the headset, a fake in tests.
pub trait Media: Send {
    /// A slice of the PC's sound: playing starts with the first one.
    fn play(&mut self, pcm: Vec<u8>);
    /// A slice played, back to be received into again (none: none played yet).
    fn played(&mut self) -> Option<Vec<u8>>;
    /// The PC's sound no longer comes to the headset: playing stops.
    fn silence(&mut self);
    /// Records the microphone to the PC through `up` while `on` (a PC program records
    /// from it, and the user's switch is on).
    fn record(&mut self, on: bool, up: &Feedback);
    /// The user's microphone switch is on.
    fn mic_allowed(&self) -> bool;
}

/// Makes each session's [`Media`], and raises the network's threads' priority where
/// the system lets it.
pub trait MediaSource: Send + Sync {
    fn open(&self) -> Box<dyn Media>;
    /// The calling thread reads or writes the network: a busy moment elsewhere should
    /// delay it less.
    fn raise_priority(&self) {}
}

/// What happened, for the core (through the app).
pub enum NetEvent {
    /// The connection of step `step` is open (TCP); whose it is shows later.
    Dialed { step: StepId, addr: SocketAddr },
    /// Nothing answered at step `step` (or the wait for answers to its query is over).
    Missed { step: StepId },
    /// A PC answered a query.
    Heard(FoundHost),
    /// The PC of step `step` is trusted (or was just paired with): its session opens.
    Reached {
        step: StepId,
        pc: PcId,
        addr: SocketAddr,
    },
    /// A PC this headset does not trust (or that forgot it): ask the user whether to
    /// pair. Its connection closes.
    PairOffer {
        key: [u8; 32],
        addr: SocketAddr,
        why: OfferWhy,
    },
    /// Pairing with a new PC: show `code` (the PC asks for it), or, with `None`, ask
    /// for the code the PC shows.
    Pairing {
        step: StepId,
        key: [u8; 32],
        addr: SocketAddr,
        code: Option<String>,
    },
    /// The codes did not match; a new attempt follows.
    PairingFailed,
    /// The PC speaks another protocol version: `pc_newer` says which side to update.
    Incompatible { pc_newer: bool, pc_app: String },
    /// The PC said why it cannot serve the headset; shown in the menu.
    HostError(String),
    /// The trusted PCs: at the start and after each change.
    Paired(Vec<PairedPc>),
    /// Forgetting PC `pc` failed (its file could not be written): it is still trusted,
    /// and still connected if it was.
    ForgetFailed { pc: PcId, why: String },
    /// The session with PC `pc` is open; the handle sends requests (add monitor…) to
    /// it. `opened_with`: the headset's state as `Open` carried it.
    Connected {
        pc: PcId,
        addr: SocketAddr,
        feedback: Feedback,
        /// The PC's interface language.
        language: Option<Lang>,
        opened_with: HeadsetState,
    },
    /// A part of PC `pc`'s state, checked: the snapshot's parts one by one, then each
    /// change.
    Part(PcId, HostMsg),
    /// The answer to request `req`.
    Reply {
        pc: PcId,
        req: ReqId,
        outcome: Outcome,
    },
    /// Something went wrong on the PC that nobody asked for.
    Problem {
        pc: PcId,
        error: ErrorCode,
        detail: String,
    },
    /// The PC (its tray, `prostir ctl`) asks for something: answered once.
    Command {
        pc: PcId,
        req: ReqId,
        asked: Asked<Command>,
    },
    /// A desktop notification of the PC.
    Notice(PcId, String),
    /// A stream starts (or starts over in a new generation).
    StreamStart {
        pc: PcId,
        id: DisplayId,
        generation: Gen,
        width: u32,
        height: u32,
        codec: Codec,
        frames: Frames,
        feedback: Feedback,
    },
    /// The stream stopped (the display may still be there: only `Displays` says).
    StreamEnd { pc: PcId, id: DisplayId },
    /// A stream larger than this headset decodes came (from an older PC): display
    /// `name` (as the user reads it) shows nothing.
    TooBig {
        pc: PcId,
        name: String,
        size: (u32, u32),
        codec: Codec,
    },
    /// The connection of step `step` (of PC `pc`, if it got that far) ended after
    /// `lasted`, as `ended` says.
    Ended {
        step: StepId,
        pc: Option<PcId>,
        why: Disconnect,
        ended: Ended,
        lasted: Duration,
    },
}

/// The files and names the network thread works with.
#[derive(Debug, Clone)]
pub struct Config {
    /// This headset's long-term key.
    pub identity_file: PathBuf,
    /// The PCs paired with.
    pub trust_file: PathBuf,
    /// The user's `IP` or `IP:PORT`, if they wrote one.
    pub host_file: Option<PathBuf>,
    /// Each trusted PC's find secret, as its sessions gave it.
    pub find_secrets_file: PathBuf,
    /// Each trusted PC's last address on the network.
    pub last_hosts_file: PathBuf,
    /// The one last address earlier builds kept: read once into `last_hosts_file`, then
    /// removed.
    pub old_last_host_file: Option<PathBuf>,
    /// The PCs' discovery port, where queries go (as a broadcast, and to the addresses
    /// known); none: no query goes but to an exact address (the selftest's fake PC).
    /// Only the headset asks the network by itself: on other systems (the tests) this
    /// is set by hand, so nothing reaches a real PC on the machine.
    pub discovery_port: Option<u16>,
    /// Queries also go as a broadcast (the headset; never in tests).
    pub broadcast: bool,
    /// This headset's name for pairing (its model, as the system names it).
    pub name: String,
    /// The code the headset shows when it pairs: a new random one each time, but in
    /// tests (against a PC whose person types a code given beforehand).
    pub pairing_code: fn() -> String,
}

impl Config {
    /// The files in `dir`, as the headset app keeps them (`identity`, `paired_hosts`,
    /// `find_secrets`, `last_hosts`), `host.txt` in `outside` if given, and the
    /// headset's `name`. Queries go to the network on the headset only (see
    /// [`Config::discovery_port`]).
    pub fn in_dir(dir: &std::path::Path, outside: Option<&std::path::Path>, name: &str) -> Self {
        let headset = cfg!(target_os = "android");
        Self {
            identity_file: dir.join("identity"),
            trust_file: dir.join("paired_hosts"),
            host_file: outside.map(|o| o.join("host.txt")),
            find_secrets_file: dir.join("find_secrets"),
            last_hosts_file: dir.join("last_hosts"),
            old_last_host_file: Some(dir.join("last_host")),
            discovery_port: headset.then_some(prostir_proto::DISCOVERY_PORT),
            broadcast: headset,
            name: name.to_owned(),
            pairing_code: secure::pairing_code,
        }
    }
}

/// The running network thread, for the app.
pub struct Net {
    inbox: Sender<Inbox>,
    thread: Option<JoinHandle<()>>,
}

impl Net {
    /// Starts the network thread: it tells the trusted PCs at once, then waits for
    /// commands. `headset`: what the first `Open` says.
    pub fn start(
        config: Config,
        events: Sender<NetEvent>,
        media: Arc<dyn MediaSource>,
        headset: HeadsetState,
    ) -> io::Result<Self> {
        let identity = Identity::load_or_create(&config.identity_file)
            .map_err(|e| io::Error::other(format!("the headset key: {e}")))?;
        info!("headset key {}", secure::fingerprint(&identity.public));
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            identity,
            name: config.name.clone(),
            pairing_code: config.pairing_code,
            headset: Mutex::new(headset),
            media_to: Mutex::new(None),
            media,
            inbox: Mutex::new(tx.clone()),
            events,
            linked: Mutex::default(),
            trusted: Mutex::default(),
            quitting: std::sync::atomic::AtomicBool::new(false),
        });
        let thread = std::thread::Builder::new()
            .name("net".into())
            .spawn(move || run(&config, &shared, &rx))?;
        Ok(Self {
            inbox: tx,
            thread: Some(thread),
        })
    }

    /// Does `cmd` (it only queues it).
    pub fn send(&self, cmd: NetCmd) {
        let _ = self.inbox.send(Inbox::Cmd(cmd));
    }

    /// Whether the thread ended (it ends only on [`NetCmd::Quit`], or a panic).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Ends every session (with a goodbye) and the thread, waiting for it up to
    /// `within`: Android may start the app again in the same process, and a session
    /// left behind would keep the PC busy. Whether it ended in time.
    pub fn stop(&mut self, within: Duration) -> bool {
        let _ = self.inbox.send(Inbox::Cmd(NetCmd::Quit));
        let Some(thread) = self.thread.take() else {
            return true;
        };
        let until = Instant::now() + within;
        while !thread.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !thread.is_finished() {
            warn!(
                "the network thread did not stop within {} ms",
                within.as_millis()
            );
            return false;
        }
        if thread.join().is_err() {
            warn!("the network thread panicked");
        }
        true
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop(Duration::from_secs(1));
        }
    }
}

/// What the network thread is told: the app's commands, and its connections' news.
enum Inbox {
    Cmd(NetCmd),
    /// A connection of step `step` is open: how to leave it.
    Opened {
        step: StepId,
        addr: SocketAddr,
        current: Arc<Current>,
    },
    /// The connection of step `step` is PC `pc`'s.
    Linked {
        step: StepId,
        pc: PcId,
    },
    /// The user paired with the PC of `key`, which calls itself `name`.
    Trusted {
        key: [u8; 32],
        name: String,
    },
    /// A connection's threads: kept to be waited for when the app closes.
    Thread(JoinHandle<()>),
    /// The connection of step `step` is over.
    Gone {
        step: StepId,
    },
    /// PC `key` gave its find secret in its session.
    Secret {
        key: [u8; 32],
        secret: prostir_proto::discovery::FindSecret,
    },
    /// PC `key`'s session opened at `addr` on the network.
    Seen {
        key: [u8; 32],
        addr: SocketAddr,
    },
}

/// What every connection shares with the network thread.
pub(crate) struct Shared {
    identity: Identity,
    /// This headset's name, as the PC shows it when they pair (and in a query while
    /// it looks for a PC to pair with).
    name: String,
    pairing_code: fn() -> String,
    /// What the next `Open` says.
    headset: Mutex<HeadsetState>,
    /// The session the sound and the microphone go with.
    media_to: Mutex<Option<PcId>>,
    media: Arc<dyn MediaSource>,
    inbox: Mutex<Sender<Inbox>>,
    events: Sender<NetEvent>,
    /// The PCs a connection has, by key: never a second one to the same PC (the PC
    /// would take it for the headset coming back, and drop the first).
    linked: Mutex<std::collections::BTreeSet<PcId>>,
    /// The keys of the trusted PCs, as the network thread's file has them now: a
    /// connection looks its PC up once it knows it (a PC forgotten while its dial was
    /// on the way is not trusted any more).
    trusted: Mutex<std::collections::BTreeSet<[u8; 32]>>,
    /// The app is closing: no connection starts a session any more.
    quitting: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn tell(&self, event: NetEvent) {
        let _ = self.events.send(event);
    }

    fn note(&self, news: Inbox) {
        let _ = lock(&self.inbox).send(news);
    }

    fn headset(&self) -> HeadsetState {
        lock(&self.headset).clone()
    }

    /// Whether PC `pc`'s session has the sound and the microphone.
    fn has_media(&self, pc: PcId) -> bool {
        *lock(&self.media_to) == Some(pc)
    }

    /// Takes PC `pc` for a connection: `false` when another one has it.
    fn claim(&self, pc: PcId) -> bool {
        lock(&self.linked).insert(pc)
    }

    fn release(&self, pc: PcId) {
        lock(&self.linked).remove(&pc);
    }

    /// Whether the PC of `key` is trusted now.
    fn trusts(&self, key: &[u8; 32]) -> bool {
        lock(&self.trusted).contains(key)
    }

    /// The trusted PCs are those of `store` from now on.
    fn trusted_now(&self, store: &TrustStore) {
        *lock(&self.trusted) = store.entries().iter().map(|(k, _)| *k).collect();
    }

    fn quitting(&self) -> bool {
        self.quitting.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// A lock, also when a thread panicked holding it (the data is plain, fine to go on
/// with).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One connection, as the network thread leaves it: closed, or with a goodbye once its
/// session is open; and, while pairing, the code the user types.
pub(crate) struct Current {
    closer: Closer,
    state: Mutex<CurrentState>,
    typed: Condvar,
}

#[derive(Default)]
struct CurrentState {
    feedback: Option<Feedback>,
    /// The PC this connection took, once its key is known (before its session).
    pc: Option<PcId>,
    /// The code typed for the PC's code while pairing.
    code: Option<String>,
    /// Given up on (or left): pairing stops waiting.
    dropped: bool,
}

impl Current {
    fn new(closer: Closer) -> Self {
        Self {
            closer,
            state: Mutex::default(),
            typed: Condvar::new(),
        }
    }

    /// The session is open: leaving it from now on says goodbye.
    fn opened(&self, feedback: &Feedback) {
        lock(&self.state).feedback = Some(feedback.clone());
    }

    /// The connection took PC `pc` (see `Shared::claim`): leaving or forgetting that
    /// PC leaves it from now on.
    fn took(&self, pc: PcId) {
        lock(&self.state).pc = Some(pc);
    }

    /// Its session is open (it pairs no more).
    fn is_open(&self) -> bool {
        lock(&self.state).feedback.is_some()
    }

    /// Leaves the PC: with a goodbye once the session is open, else by closing.
    fn leave(&self, reason: HeadsetGoodbye) {
        let mut s = lock(&self.state);
        s.dropped = true;
        self.typed.notify_all();
        match &s.feedback {
            Some(f) => f.goodbye(reason),
            None => self.closer.close(),
        }
    }

    fn submit(&self, code: String) {
        lock(&self.state).code = Some(code);
        self.typed.notify_all();
    }

    /// Waits up to `d` for a typed code: the code, or `Err(())` once dropped.
    fn code_within(&self, d: Duration) -> Result<Option<String>, ()> {
        let s = lock(&self.state);
        let (mut s, _) = self
            .typed
            .wait_timeout_while(s, d, |s| s.code.is_none() && !s.dropped)
            .unwrap_or_else(PoisonError::into_inner);
        if s.dropped {
            return Err(());
        }
        Ok(s.code.take())
    }

    fn forget_code(&self) {
        lock(&self.state).code = None;
    }
}

/// A connection the network thread knows of.
struct Conn {
    step: StepId,
    addr: SocketAddr,
    pc: Option<PcId>,
    current: Arc<Current>,
}

impl Conn {
    /// Whether it is PC `pc`'s: as its session said, or as soon as the connection took
    /// it (its dial still on the way).
    fn is_of(&self, pc: PcId) -> bool {
        self.pc == Some(pc) || lock(&self.current.state).pc == Some(pc)
    }
}

/// The network thread: commands and news, in order, until the app closes.
fn run(config: &Config, shared: &Arc<Shared>, inbox: &Receiver<Inbox>) {
    shared.media.raise_priority();
    let mut trusted = TrustStore::load(&config.trust_file);
    shared.trusted_now(&trusted);
    shared.tell(NetEvent::Paired(paired(&trusted)));
    let mut conns: Vec<Conn> = Vec::new();
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    let known = Arc::new(Mutex::new(discovery::Known::load(config, &trusted)));
    let targets = discovery::Targets {
        port: config.discovery_port,
        broadcast: config.broadcast,
        broadcasts: discovery::interface_broadcasts,
        host_file: config.host_file.clone(),
    };
    let mut finder = discovery::Finder::start(targets, shared.clone(), known.clone(), &mut threads);
    let net = Here {
        config,
        shared,
        known: &known,
        finder: &finder,
    };
    loop {
        let news = match inbox.recv_timeout(Duration::from_secs(1)) {
            Ok(news) => news,
            Err(RecvTimeoutError::Timeout) => {
                threads.retain(|t| !t.is_finished());
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match news {
            Inbox::Cmd(NetCmd::Quit) => break,
            Inbox::Cmd(cmd) => net.command(cmd, &mut trusted, &conns, &mut threads),
            Inbox::Opened {
                step,
                addr,
                current,
            } => conns.push(Conn {
                step,
                addr,
                pc: None,
                current,
            }),
            Inbox::Linked { step, pc } => {
                if let Some(c) = conns.iter_mut().find(|c| c.step == step) {
                    c.pc = Some(pc);
                }
            }
            Inbox::Trusted { key, name } => {
                if let Err(e) = trusted.add(key, &name) {
                    warn!("cannot keep the PC paired with: {e}");
                }
                shared.trusted_now(&trusted);
                shared.tell(NetEvent::Paired(paired(&trusted)));
            }
            Inbox::Thread(t) => threads.push(t),
            Inbox::Gone { step } => conns.retain(|c| c.step != step),
            Inbox::Secret { key, secret } if trusted.contains(&key) => {
                lock(&known).set_secret(key, secret);
            }
            Inbox::Seen { key, addr } if trusted.contains(&key) => lock(&known).seen(key, addr),
            Inbox::Secret { .. } | Inbox::Seen { .. } => {}
        }
    }
    // Closing: every connection is left (sessions with a goodbye), and their threads
    // get a moment to end; what is still open then is closed.
    shared
        .quitting
        .store(true, std::sync::atomic::Ordering::Relaxed);
    finder.stop();
    for c in &conns {
        c.current.leave(HeadsetGoodbye::Quit);
    }
    let until = Instant::now() + QUIT_WITHIN;
    let mut closed = false;
    while threads.iter().any(|t| !t.is_finished()) && Instant::now() < until + CLOSE_WITHIN {
        // News of the connections ending (and their threads) still comes.
        while let Ok(news) = inbox.try_recv() {
            match news {
                Inbox::Thread(t) => threads.push(t),
                Inbox::Opened {
                    step,
                    addr,
                    current,
                } => {
                    current.leave(HeadsetGoodbye::Quit);
                    conns.push(Conn {
                        step,
                        addr,
                        pc: None,
                        current,
                    });
                }
                Inbox::Gone { step } => conns.retain(|c| c.step != step),
                _ => {}
            }
        }
        if !closed && Instant::now() >= until {
            closed = true;
            for c in &conns {
                c.current.closer.close();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let left = threads.iter().filter(|t| !t.is_finished()).count();
    if left > 0 {
        warn!("{left} connection threads did not stop in time");
    }
    info!("network stopped");
}

/// The trusted PCs, for the core.
fn paired(trusted: &TrustStore) -> Vec<PairedPc> {
    trusted
        .entries()
        .iter()
        .map(|(key, name)| PairedPc {
            pc: PcId::of(key),
            name: name.clone(),
        })
        .collect()
}

/// What a command of the core works with.
struct Here<'a> {
    config: &'a Config,
    shared: &'a Arc<Shared>,
    known: &'a Mutex<discovery::Known>,
    finder: &'a discovery::Finder,
}

impl Here<'_> {
    /// Does one command of the core.
    fn command(
        &self,
        cmd: NetCmd,
        trusted: &mut TrustStore,
        conns: &[Conn],
        threads: &mut Vec<JoinHandle<()>>,
    ) {
        let (config, shared) = (self.config, self.shared);
        match cmd {
            NetCmd::Dial {
                step,
                to,
                pair,
                pc_shows_code,
            } => {
                let last = match to {
                    crate::session::Dest::LastOf(pc) => lock(self.known).last_of(pc),
                    _ => None,
                };
                let dial = link::Dial {
                    step,
                    to,
                    last,
                    pair,
                    pc_shows_code,
                    host_file: config.host_file.clone(),
                    busy: conns.iter().map(|c| c.addr).collect(),
                    shared: shared.clone(),
                };
                spawn(threads, "net-dial", move || link::dial(dial));
            }
            NetCmd::Ask { step, to, pairing } => {
                self.finder
                    .ask(discovery::Ask { step, to, pairing }, shared);
            }
            // Before trust, or pairing again inside a trusted PC's link (the PC had
            // forgotten the headset): the connections whose session has not opened.
            NetCmd::Hangup => {
                for c in conns.iter().filter(|c| !c.current.is_open()) {
                    c.current.leave(HeadsetGoodbye::OtherPc);
                }
            }
            NetCmd::Leave(pc, why) => {
                for c in conns.iter().filter(|c| c.is_of(pc)) {
                    c.current.leave(why);
                }
            }
            NetCmd::SubmitCode(code) => {
                if let Some(c) = conns.iter().find(|c| !c.current.is_open()) {
                    c.current.submit(code);
                }
            }
            NetCmd::Forget(pc) => {
                let keys: Vec<[u8; 32]> = trusted
                    .entries()
                    .iter()
                    .map(|(k, _)| *k)
                    .filter(|k| PcId::of(k) == pc)
                    .collect();
                let (mut removed, mut failed) = (false, None);
                for key in &keys {
                    // Kept as it was when its file cannot be written.
                    match trusted.remove(key) {
                        Ok(()) => {
                            info!("forgot PC {pc}");
                            removed = true;
                        }
                        Err(e) => {
                            warn!("cannot forget PC {pc}: {e}");
                            failed = Some(e.to_string());
                        }
                    }
                }
                if removed || keys.is_empty() {
                    // First, so a connection that looks its PC up from now on finds it
                    // gone; one that took it already is left below.
                    shared.trusted_now(trusted);
                    shared.tell(NetEvent::Paired(paired(trusted)));
                }
                if let Some(why) = failed {
                    // Still trusted: its session goes on, and the menu says why.
                    shared.tell(NetEvent::ForgetFailed { pc, why });
                    return;
                }
                // Its find secret and last address: not asked for, nor dialed, again.
                let mut known = lock(self.known);
                for key in &keys {
                    known.forget(key);
                }
                drop(known);
                // Connected to it (by its key, not its name: two PCs may share one):
                // left, telling it so (after the new list, which the core hears first).
                for c in conns.iter().filter(|c| c.is_of(pc)) {
                    c.current.leave(HeadsetGoodbye::Forgotten);
                }
            }
            NetCmd::SetHeadset(state) => *lock(&shared.headset) = state,
            NetCmd::MediaTo(pc) => *lock(&shared.media_to) = pc,
            NetCmd::Quit => {}
        }
    }
}

/// Starts a thread of the network named `name`, kept to be waited for.
fn spawn(threads: &mut Vec<JoinHandle<()>>, name: &str, f: impl FnOnce() + Send + 'static) {
    match std::thread::Builder::new().name(name.into()).spawn(f) {
        Ok(t) => threads.push(t),
        Err(e) => warn!("cannot start the {name} thread: {e}"),
    }
}

/// Whether a socket error only means nothing came in time.
fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}
