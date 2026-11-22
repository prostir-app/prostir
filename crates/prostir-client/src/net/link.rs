//! One connection to a PC: the encrypted link, the versions and, before trust,
//! pairing as the user agreed (in the `net-dial` thread that made it); then a trusted
//! PC's session in a thread of its own, `link-<pc>`: its state checked part by part,
//! its streams handed to the decoders, its sound and the microphone through
//! [`super::Media`]. Plain `std` and the protocol: it builds and is tested on any
//! system.

#![expect(
    clippy::disallowed_methods,
    reason = "the network thread is part of the shell: its sockets, files and clock are its own"
)]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{info, warn};
use prostir_proto::handshake::{self, Answer};
use prostir_proto::link::tcp::{NoiseTcp, Plain};
use prostir_proto::link::{Closer, Link, PeerInfo, RecordReceiver};
use prostir_proto::packet::Assembler;
use prostir_proto::secure::{self, Pairing, confirmation, tags_match};
use prostir_proto::state::{self, DisplayId, Gen, PcState};
use prostir_proto::stats::{Gaps, Period};
use prostir_proto::{ClientMsg, HostMsg, PcGoodbye};

use super::discovery::{self, read_addr, try_connect};
use super::{Current, Disconnect, Feedback, Inbox, NetEvent, Shared};
use crate::connecting::Ended;
use crate::frames::{Frame, FrameQueue, KeyframeWait, Pushed};
use crate::ids::PcId;
use crate::lang::Lang;
use crate::session::{Dest, StepId};
use crate::ui::OfferWhy;

/// While the user types a code, the connection is looked at this often.
const LOOK_EVERY: Duration = Duration::from_millis(500);
/// How long the user has to read and type a pairing code.
const CODE_WITHIN: Duration = Duration::from_secs(300);
/// How often the session logs what it received.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// Nothing at all from the PC for this long (it pings every second): the session is
/// over, and the search starts at once. Not shorter: after a radio stall of a second or
/// two, TCP's resends back off, and what was held up can take a few seconds more to
/// arrive on a link that is fine. A dead Wi-Fi could be found sooner by dialing the PC
/// anew alongside the silent session after a shorter quiet, keeping whichever answers
/// (later, measured with `silence_detect_ms`).
pub const SILENCE: Duration = Duration::from_secs(5);
/// Wrong codes in a row before the headset stops and asks again.
const PAIR_ATTEMPTS: u32 = 3;

/// A dial the core asked for, with what the network thread knew then.
pub(super) struct Dial {
    pub step: StepId,
    pub to: Dest,
    /// The key of the PC the user agreed to pair with.
    pub pair: Option<[u8; 32]>,
    pub pc_shows_code: bool,
    pub host_file: Option<PathBuf>,
    pub last_host_file: PathBuf,
    /// The keys of the trusted PCs.
    pub trusted: Vec<[u8; 32]>,
    /// The addresses of the connections already open: never dialed twice (a second
    /// connection from this headset would replace the first on the PC).
    pub busy: Vec<SocketAddr>,
    pub shared: Arc<Shared>,
}

/// Reaches the PC at the dial's destination: a session for a trusted one, pairing for
/// the one the user agreed to, an offer to the user otherwise.
pub(super) fn dial(d: Dial) {
    let addr = match &d.to {
        Dest::Usb => Some(SocketAddr::new(
            Ipv4Addr::LOCALHOST.into(),
            prostir_proto::TCP_PORT,
        )),
        Dest::Addr(a) => Some(*a),
        Dest::HostFile => d.host_file.as_deref().and_then(read_addr),
        Dest::LastHost => read_addr(&d.last_host_file),
    };
    match addr.filter(|a| !d.busy.contains(a)).and_then(try_connect) {
        Some(stream) => connection(d, stream),
        None => d.shared.tell(NetEvent::Missed { step: d.step }),
    }
}

/// Probes the local network, then goes on as [`dial`] with what answered.
pub(super) fn scan(d: Dial) {
    let found =
        discovery::scan_subnet().filter(|s| s.peer_addr().is_ok_and(|a| !d.busy.contains(&a)));
    match found {
        Some(stream) => connection(d, stream),
        None => d.shared.tell(NetEvent::Missed { step: d.step }),
    }
}

/// A connection as it goes: what the core hears when it ends.
struct Conn {
    d: Dial,
    addr: SocketAddr,
    current: Arc<Current>,
    began: Instant,
    ended: Ended,
    pc: Option<PcId>,
    /// The PC this connection took (see `Shared::claim`), let go when it ends.
    claimed: Option<PcId>,
    /// Its end was told.
    told: bool,
}

impl Drop for Conn {
    /// However its thread ends (a panic too), the core hears the connection ended, so
    /// the search never waits for it.
    fn drop(&mut self) {
        if !self.told {
            self.finish(&Err(io::Error::other("the connection's thread ended")));
        }
    }
}

impl Conn {
    /// The connection is over: the core and the network thread hear how.
    fn finish(&mut self, outcome: &io::Result<()>) {
        self.told = true;
        if let Some(pc) = self.claimed.take() {
            self.d.shared.release(pc);
        }
        let addr = self.addr;
        let why = match (outcome, self.ended.goodbye) {
            (_, Some(reason)) => Disconnect::Goodbye(reason),
            (Err(e), None) if self.ended.opened && e.kind() == io::ErrorKind::TimedOut => {
                Disconnect::Silence
            }
            _ => Disconnect::Closed,
        };
        match outcome {
            Ok(()) if self.ended.goodbye.is_some() => {
                info!("host {addr} ended the session: {why:?}");
            }
            Ok(()) => info!("host {addr} closed the connection"),
            Err(e) => warn!("connection to {addr} lost: {e}"),
        }
        // Remembered only once the PC proved it is a trusted one: a single connection
        // to anything else must not redirect the next start.
        if self.ended.opened
            && !addr.ip().is_loopback()
            && let Err(e) = discovery::keep_last(&self.d.last_host_file, addr)
        {
            warn!("cannot keep the last PC's address: {e}");
        }
        let shared = &self.d.shared;
        shared.note(Inbox::Gone { step: self.d.step });
        shared.tell(NetEvent::Ended {
            step: self.d.step,
            pc: self.pc,
            why,
            ended: std::mem::take(&mut self.ended),
            lasted: self.began.elapsed(),
        });
    }
}

/// The pieces of the encrypted link past the versions.
struct Wire {
    receiver: Box<dyn RecordReceiver>,
    packets: Assembler,
    /// Messages read while waiting for the user, not handled yet.
    pending: VecDeque<HostMsg>,
    feedback: Feedback,
    peer: PeerInfo,
}

impl Drop for Wire {
    /// However the connection ends, its writer ends too.
    fn drop(&mut self) {
        self.feedback.close();
    }
}

/// The connection's first part, in the dialing thread: up to a trusted PC, whose
/// session then goes on in a thread of its own.
fn connection(d: Dial, stream: TcpStream) {
    let Ok(addr) = stream.peer_addr() else {
        return d.shared.tell(NetEvent::Missed { step: d.step });
    };
    let closer = match stream.try_clone() {
        Ok(socket) => Closer::new(move || {
            let _ = socket.shutdown(Shutdown::Both);
        }),
        Err(_) => return d.shared.tell(NetEvent::Missed { step: d.step }),
    };
    let current = Arc::new(Current::new(closer));
    d.shared.note(Inbox::Opened {
        step: d.step,
        addr,
        current: current.clone(),
    });
    d.shared.tell(NetEvent::Dialed { step: d.step, addr });
    let mut conn = Conn {
        d,
        addr,
        current,
        began: Instant::now(),
        ended: Ended::default(),
        pc: None,
        claimed: None,
        told: false,
    };
    match before_trust(&mut conn, stream) {
        Ok(Some((wire, language))) => {
            let pc = PcId::of(&wire.peer.key);
            conn.pc = Some(pc);
            let shared = conn.d.shared.clone();
            shared.note(Inbox::Linked {
                step: conn.d.step,
                pc,
            });
            shared.tell(NetEvent::Reached {
                step: conn.d.step,
                pc,
                addr,
            });
            // The rest in a thread of the PC's own (its name: the first bytes of its
            // number, as long as the system shows).
            let name = format!("link-{}", &pc.to_string()[..10]);
            let started = std::thread::Builder::new().name(name).spawn(move || {
                let mut wire = wire;
                let outcome = session(&mut conn, &mut wire, language);
                drop(wire);
                conn.finish(&outcome);
            });
            // Not started: the connection was moved into the closure and dropped
            // with it, which told its end.
            match started {
                Ok(t) => shared.note(Inbox::Thread(t)),
                Err(e) => warn!("cannot start the session's thread: {e}"),
            }
        }
        Ok(None) => conn.finish(&Ok(())),
        Err(e) => conn.finish(&Err(e)),
    }
}

/// The encrypted link and the versions, then pairing if the PC is not trusted and the
/// user agreed: the link, once the PC is trusted (`None`: it is not, or the versions
/// differ).
fn before_trust(conn: &mut Conn, stream: TcpStream) -> io::Result<Option<(Wire, Option<Lang>)>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let shared = conn.d.shared.clone();
    let Link {
        mut sender,
        mut receiver,
        closer: noise_closer,
        peer,
    } = NoiseTcp::connect(stream, &shared.identity, Arc::new(Plain))?;
    // One connection to a PC at a time, and none once the app closes: closed now,
    // before anything is asked of the PC.
    let pc = PcId::of(&peer.key);
    if shared.quitting() {
        noise_closer.close();
        return Ok(None);
    }
    if !shared.claim(pc) {
        info!(
            "PC {pc} at {} has a connection already: this one closes",
            conn.addr
        );
        noise_closer.close();
        return Ok(None);
    }
    conn.claimed = Some(pc);
    // Versions first, before pairing: this exchange is the same in every version, so
    // a PC of any version says which side to update.
    let app = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
    handshake::send_hello(&mut *sender, &app)?;
    let language = match handshake::read_answer(&mut *receiver)? {
        Answer::Welcome { language, .. } => Lang::from_tag(&language),
        Answer::Incompatible { protocol, app } => {
            shared.tell(NetEvent::Incompatible {
                pc_newer: prostir_proto::peer_is_newer(protocol, &app),
                pc_app: app,
            });
            conn.ended.incompatible = true;
            noise_closer.close();
            return Ok(None);
        }
    };
    // From here on, packets.
    let feedback = Feedback::start(sender, noise_closer, &shared)?;
    let mut wire = Wire {
        receiver,
        packets: HostMsg::assembler(),
        pending: VecDeque::new(),
        feedback,
        peer,
    };
    // A PC we have never paired with: pair only once the user said so in the menu,
    // and never on our own (a hostile PC on the network could keep guessing codes).
    if !conn.d.trusted.contains(&wire.peer.key) && !pair_if_approved(conn, &mut wire)? {
        return Ok(None);
    }
    Ok(Some((wire, language)))
}

/// Pairs if the user agreed to (`true` once paired); otherwise asks them and ends the
/// connection (the PC must not be held waiting for an answer).
fn pair_if_approved(conn: &mut Conn, wire: &mut Wire) -> io::Result<bool> {
    if conn.d.pair == Some(wire.peer.key) {
        return pair(conn, wire);
    }
    conn.d.shared.tell(NetEvent::PairOffer {
        key: wire.peer.key,
        addr: conn.addr,
        why: OfferWhy::New,
    });
    info!(
        "an unknown PC at {}: waiting for the user to approve pairing",
        conn.addr
    );
    Ok(false)
}

/// The next message from the PC before the session.
fn read(wire: &mut Wire) -> io::Result<HostMsg> {
    match wire.pending.pop_front() {
        Some(msg) => Ok(msg),
        None => HostMsg::recv(&mut *wire.receiver, &mut wire.packets),
    }
}

/// Whether the PC is still there, looking at the connection for a moment: what
/// arrives meanwhile is kept for [`read`].
fn alive(wire: &mut Wire) -> bool {
    wire.receiver.set_idle_limit(Some(LOOK_EVERY));
    let alive = match wire.receiver.recv() {
        Ok(record) => match wire.packets.push(record).map_err(io::Error::from) {
            Ok(Some(m)) => match HostMsg::from_message(m) {
                Ok(msg) => {
                    wire.pending.extend(msg);
                    true
                }
                Err(_) => false,
            },
            Ok(None) => true,
            Err(_) => false,
        },
        Err(e) => e.kind() == io::ErrorKind::TimedOut,
    };
    wire.receiver.set_idle_limit(Some(CODE_WITHIN));
    alive
}

/// Waits for the code the user types, looking at the connection meanwhile; `None` if
/// it was dropped, the PC left, or nobody typed in time.
fn wait_code(conn: &Conn, wire: &mut Wire) -> Option<String> {
    conn.current.forget_code();
    let deadline = Instant::now() + CODE_WITHIN;
    loop {
        match conn.current.code_within(Duration::from_millis(10)) {
            Ok(Some(code)) => return Some(code),
            Ok(None) => {}
            Err(()) => return None,
        }
        if !alive(wire) || Instant::now() >= deadline {
            return None;
        }
    }
}

/// What the PC is kept as when paired with, until its session says its own name: its
/// address.
fn pairing_name(addr: SocketAddr) -> String {
    addr.ip().to_string()
}

/// Pairs with the PC: up to [`PAIR_ATTEMPTS`] codes, then asks the user again.
fn pair(conn: &mut Conn, wire: &mut Wire) -> io::Result<bool> {
    let shared = conn.d.shared.clone();
    let (step, addr, key) = (conn.d.step, conn.addr, wire.peer.key);
    // The user needs time to read and type the code.
    wire.receiver.set_idle_limit(Some(CODE_WITHIN));
    for _ in 0..PAIR_ATTEMPTS {
        let pc_shows = conn.d.pc_shows_code;
        // The PC makes its code only when asked, so ask first and then wait for the
        // user to type what it shows.
        wire.feedback.send(ClientMsg::PairBegin {
            headset_shows: !pc_shows,
            name: shared.name.clone(),
        })?;
        let code = if pc_shows {
            shared.tell(NetEvent::Pairing {
                step,
                key,
                addr,
                code: None,
            });
            wait_code(conn, wire).ok_or_else(|| io::Error::other("pairing interrupted"))?
        } else {
            let code = (shared.pairing_code)();
            shared.tell(NetEvent::Pairing {
                step,
                key,
                addr,
                code: Some(format!("{} {}", &code[..3], &code[3..])),
            });
            code
        };
        let (pairing, msg) = Pairing::start(&code);
        wire.feedback.send(ClientMsg::PairMsg { msg })?;
        let theirs = loop {
            match read(wire)? {
                HostMsg::PairMsg { msg } => break Some(msg),
                // The PC gave up this attempt (e.g. its dialog was cancelled).
                HostMsg::PairResult { .. } => break None,
                // Not trusted yet: only a short line of plain text gets shown.
                HostMsg::Error { text } => {
                    let text: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
                    shared.tell(NetEvent::HostError(text));
                }
                other => return Err(io::Error::other(format!("unexpected {other:?}"))),
            }
        };
        let Some(theirs) = theirs else {
            shared.tell(NetEvent::PairingFailed);
            continue;
        };
        let secret = pairing.finish(&theirs)?;
        let own = &shared.identity.public;
        wire.feedback.send(ClientMsg::PairConfirm {
            tag: confirmation(&secret, "headset", &wire.peer, own),
        })?;
        match read(wire)? {
            HostMsg::PairResult { tag: Some(tag) }
                if tags_match(&tag, &confirmation(&secret, "pc", &wire.peer, own)) =>
            {
                info!("paired with {addr} ({})", secure::fingerprint(&key));
                // Named by its address for now; by its own name once its session says
                // it (see `session`).
                shared.note(Inbox::Trusted {
                    key,
                    name: pairing_name(addr),
                });
                conn.d.trusted.push(key);
                conn.ended.paired = true;
                wire.receiver.set_idle_limit(Some(Duration::from_secs(10)));
                return Ok(true);
            }
            HostMsg::PairResult { .. } => {
                warn!("pairing with {addr}: the codes did not match");
                shared.tell(NetEvent::PairingFailed);
            }
            other => return Err(io::Error::other(format!("unexpected {other:?}"))),
        }
    }
    // Several wrong codes: stop and let the user decide whether to go on.
    shared.tell(NetEvent::PairOffer {
        key,
        addr,
        why: OfferWhy::WrongCodes,
    });
    Err(io::Error::other("pairing stopped after wrong codes"))
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

/// A trusted PC's session: `Open`, its snapshot, then everything it sends until the
/// connection ends.
fn session(conn: &mut Conn, wire: &mut Wire, language: Option<Lang>) -> io::Result<()> {
    let shared = conn.d.shared.clone();
    let addr = conn.addr;
    let pc = PcId::of(&wire.peer.key);
    let opened_with = shared.headset();
    let open = opened_with
        .open()
        .map_err(|e| io::Error::other(format!("Open: {e}")))?;
    wire.feedback.send(open.clone())?;
    // The PC is trusted from here: its snapshot may be as big as it needs.
    wire.packets.set_limits(HostMsg::limits(true));
    let snapshot = loop {
        match read(wire)? {
            HostMsg::Snapshot { parts } => break parts,
            HostMsg::PairingRequired => {
                wire.packets.set_limits(HostMsg::limits(false));
                if !pair_if_approved(conn, wire)? {
                    return Ok(());
                }
                wire.feedback.send(open.clone())?;
                wire.packets.set_limits(HostMsg::limits(true));
            }
            // The PC cannot open the session (no encoder, say), and closes the
            // connection after saying why.
            HostMsg::Error { text } => {
                conn.ended.refused = Some(Instant::now());
                shared.tell(NetEvent::HostError(text));
            }
            other => return Err(io::Error::other(format!("unexpected {other:?}"))),
        }
    };
    let mut state = PcState::from_snapshot(&snapshot).map_err(io::Error::from)?;
    conn.ended.opened = true;
    // Just paired: the PC is kept by the name it gives itself.
    if conn.ended.paired
        && let Some(info) = state.info.as_ref().filter(|i| !i.name.is_empty())
    {
        shared.note(Inbox::Trusted {
            key: wire.peer.key,
            name: info.name.clone(),
        });
    }
    // Both sides ping every second: a few seconds of silence is a dead link.
    wire.receiver.set_idle_limit(Some(SILENCE));
    let feedback = wire.feedback.clone();
    feedback.start_pings();
    conn.current.opened(&feedback);
    shared.tell(NetEvent::Connected {
        pc,
        addr,
        feedback: feedback.clone(),
        language,
        opened_with: opened_with.clone(),
    });
    // The snapshot's parts, as they would come one by one.
    for part in state.parts() {
        shared.tell(NetEvent::Part(pc, part));
    }
    let mut streams: HashMap<DisplayId, StreamSlot> = HashMap::new();
    let mut media = shared.media.open();
    // Recording only while a PC program records from the headset microphone, the
    // user's switch on the headset is on, and this session has the microphone.
    let mut recording = false;
    // Pauses between messages: with sound on, anything over ~10 ms is something
    // (a frame being written, Wi-Fi) holding the rest up.
    let mut gaps = Gaps::default();
    let mut media_bytes = 0u64;
    let mut report = Period::new(REPORT_EVERY);
    let caps = opened_with.caps.unwrap_or_default();
    let result = loop {
        let msg = match HostMsg::recv(&mut *wire.receiver, &mut wire.packets) {
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
            match state.apply(msg.clone()) {
                Ok(None) => {
                    if let HostMsg::Sound { part } = &msg
                        && !part.to_headset
                    {
                        media.silence();
                    }
                    shared.tell(NetEvent::Part(pc, msg));
                    None
                }
                Ok(Some(msg)) => Some(msg),
                Err(e) => break Err(e.into()),
            }
        };
        let record = state
            .up_wanted
            .get(&state::UpKind::Mic)
            .is_some_and(|u| u.on)
            && media.mic_allowed()
            && shared.has_media(pc);
        if record != recording {
            recording = record;
            media.record(record, &feedback);
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
                    shared.tell(NetEvent::StreamEnd { pc, id });
                }
                // The PC sends only what this headset decodes. One that does not
                // (an older PC) is that display's trouble, not the session's: it
                // shows nothing, and its frames go nowhere.
                if !caps.fits(codec, w, h) {
                    warn!(
                        "stream {}: {w}x{h} in {codec:?} is too large to decode",
                        id.0
                    );
                    let name = state
                        .displays
                        .as_ref()
                        .and_then(|d| d.get(id))
                        .map_or_else(String::new, |d| crate::ui::display_label(&d.name));
                    shared.tell(NetEvent::TooBig {
                        pc,
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
                shared.tell(NetEvent::StreamStart {
                    pc,
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
                    shared.tell(NetEvent::StreamEnd { pc, id });
                }
            }
            HostMsg::Reply { req, outcome } => {
                shared.tell(NetEvent::Reply { pc, req, outcome });
            }
            HostMsg::Problem { error, detail, .. } => {
                shared.tell(NetEvent::Problem { pc, error, detail });
            }
            HostMsg::Error { text } => {
                conn.ended.refused = Some(Instant::now());
                shared.tell(NetEvent::HostError(text));
            }
            HostMsg::Audio { pcm } => {
                // Only the session with the sound plays it; a slice played is
                // received into again.
                if shared.has_media(pc) {
                    media.play(pcm);
                } else {
                    wire.packets.recycle(HostMsg::AUDIO_TAG, pcm);
                }
                while let Some(played) = media.played() {
                    wire.packets.recycle(HostMsg::AUDIO_TAG, played);
                }
            }
            HostMsg::Command { req, asked } => {
                shared.tell(NetEvent::Command { pc, req, asked });
            }
            HostMsg::Notice { text } => shared.tell(NetEvent::Notice(pc, text)),
            // Answered at once, from here: the round trip is the network's.
            HostMsg::Ping { seq } => feedback.send(ClientMsg::Pong { seq })?,
            HostMsg::Pong { seq } => feedback.outbox.pong(seq),
            HostMsg::Goodbye { reason } => {
                let reason = PcGoodbye::from_code(reason);
                conn.ended.goodbye = Some(reason);
                // It no longer trusts this headset: ask the user before pairing again,
                // and connect to nothing meanwhile.
                if reason == PcGoodbye::Forgotten {
                    shared.tell(NetEvent::PairOffer {
                        key: wire.peer.key,
                        addr,
                        why: OfferWhy::Forgotten,
                    });
                }
                break Ok(());
            }
            // Only before the session: ignored in it, as parts already taken are.
            _ => {}
        }
    };
    if recording {
        media.record(false, &feedback);
    }
    for id in streams.into_keys() {
        shared.tell(NetEvent::StreamEnd { pc, id });
    }
    result
}
