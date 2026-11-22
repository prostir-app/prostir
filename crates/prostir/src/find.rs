//! Answering the headsets that ask for this PC (see `prostir_proto::discovery`). The
//! PC sends nothing on its own: no beacon, so nothing of it wakes while nobody asks.
//!
//! The PC answers every query while it has no paired headset or while its pairing
//! window is open, and otherwise only a query with its own tag. The same rules hold in
//! the full program (the `find` thread here), in the light phase it starts with under
//! systemd and in the sockets' watcher (`crate::wake`): [`Responder`] is all of them.
//!
//! # What a stranger on the network gets (the threat model)
//!
//! Someone on the network (or anywhere a datagram reaches the PC from, a VPN too) may
//! send anything to the discovery port, from any address, as often as they like.
//!
//! - **Whether this PC is there.** A paired PC with its pairing window closed answers
//!   only a query carrying a tag made with its find secret, which it gives only to the
//!   headsets it trusts, inside the encrypted session. Its public key (which anyone who
//!   connects over TCP sees) does not make a tag. So a stranger's query gets silence,
//!   as from an address where nothing runs. Not hidden: the TCP port itself (a port
//!   scan finds it), and everything while the PC answers everyone (unpaired, or its
//!   pairing window open: then being found is the point).
//! - **Following a headset.** Each query has a new random nonce, so its tags change
//!   every time, and it carries at least eight of them, random ones among them, in a
//!   random order: two queries of one headset look like two of different ones, and
//!   the PC learns nothing of the other PCs a headset knows. The headset's name goes
//!   out only while it looks for a PC to pair with. Not hidden: that a headset asks,
//!   and from which address.
//! - **Replay.** A query is not bound to a time: one captured and sent again within
//!   [`NONCES_KEPT_FOR`] gets nothing (each nonce is answered once), but later it gets
//!   the same PC's answer again, which says that PC is still there (and nothing more:
//!   the answer is a hint where to connect, trust comes from the handshake).
//! - **Flooding.** The kernel drops what is not a whole query (a socket filter, see
//!   `platform::sockets::filter_queries`); of what gets through, at most
//!   [`READ_PER_SECOND`] datagrams a second are looked at and [`ANSWERS_PER_SECOND`]
//!   answered, the rest dropped unread, and nothing is allocated for one. An answer is
//!   always smaller than the query, so a forged sender address gets less than was sent:
//!   no amplification.
//! - **Changing state.** Nothing a query says changes anything kept: who is trusted,
//!   the find secrets and the pairing window change only through the person at the PC
//!   or an encrypted session. A stranger's query only brings the notice of a new
//!   headset nearby (at most once a minute) and the local API's `nearby` list.
//!
//! # The find secret after forgetting a headset
//!
//! Forgetting a headset makes a new find secret: the forgotten one can no longer make a
//! tag. A trusted headset that was off then still asks with the old secret, so the old
//! one stays valid for it (see `pairing::FindSecrets`) until its next session gives it
//! the new one, or until it is forgotten too. A forgotten headset gains nothing from
//! that: its tags may get an answer while the old secret lives, but the PC no longer
//! trusts its key in the handshake.

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{debug, info, warn};
use prostir_proto::discovery::{Answer, FindSecret, MAX_ANSWER, MAX_QUERY, NONCE, QueryRef, flags};

use crate::events::{Event, EventSink};
use crate::pairing::Security;

/// Answers sent in a second, at most, whoever asks.
const ANSWERS_PER_SECOND: usize = 20;
/// Datagrams looked at in a second, at most: the rest are dropped unread.
const READ_PER_SECOND: usize = 200;
/// How long a nonce answered is remembered: the same query again (a headset sends one
/// as a broadcast and straight to the PC, or someone sends a captured one again) gets
/// no second answer within it.
pub const NONCES_KEPT_FOR: Duration = Duration::from_secs(10);
/// The most nonces remembered (a second's answers, ten times over).
const NONCES_KEPT: usize = ANSWERS_PER_SECOND * 10;
/// The most old find secrets that stay valid besides the current one (see the module):
/// each is one more forgetting while some trusted headset was off. Beyond them, such a
/// headset finds the PC by its last address.
pub const STALE_KEPT: usize = 4;

/// The find secrets a headset's tag may be made with: the current one, and older ones
/// still valid for headsets that have not got the current one yet. Plain data, copied
/// for each query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Secrets {
    pub current: FindSecret,
    stale: [FindSecret; STALE_KEPT],
    stale_len: usize,
}

impl Secrets {
    /// Only `current`.
    pub fn new(current: FindSecret) -> Self {
        Self {
            current,
            stale: [[0; 32]; STALE_KEPT],
            stale_len: 0,
        }
    }

    /// `current`, and the different ones among `stale` (the first [`STALE_KEPT`]).
    pub fn with_stale<'a>(
        current: FindSecret,
        stale: impl IntoIterator<Item = &'a FindSecret>,
    ) -> Self {
        let mut s = Self::new(current);
        for old in stale {
            if s.stale_len == STALE_KEPT {
                break;
            }
            if *old != current && !s.stale().contains(old) {
                s.stale[s.stale_len] = *old;
                s.stale_len += 1;
            }
        }
        s
    }

    /// The old secrets still valid.
    pub fn stale(&self) -> &[FindSecret] {
        &self.stale[..self.stale_len]
    }

    /// The secret `query` looks for this PC with, if it does.
    fn found_by(&self, query: &QueryRef<'_>) -> Option<&FindSecret> {
        std::iter::once(&self.current)
            .chain(self.stale())
            .find(|s| query.looks_for(s))
    }
}

/// Who may get an answer now, and the find secrets the PC's own headsets make their
/// tags with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open {
    /// The PC trusts at least one headset.
    pub paired: bool,
    /// The pairing window is open.
    pub pairing: bool,
    pub secrets: Secrets,
}

impl Open {
    /// Whether the PC answers queries without its tag.
    pub fn answers_all(self) -> bool {
        !self.paired || self.pairing
    }
}

/// A headset that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asker {
    pub from: SocketAddr,
    /// Its query carries this PC's tag: one of its own headsets (it has the PC's find
    /// secret).
    pub ours: bool,
    /// The name it gave (only while it looks for a PC to pair with).
    pub name: String,
    /// It got an answer.
    pub answered: bool,
}

/// What the PC answers with: its port and name, and the limits on how much it reads
/// and answers. Its memory is made once.
pub struct Responder {
    port: u16,
    name: String,
    /// When the last answers went, at most [`ANSWERS_PER_SECOND`] of them.
    sent: VecDeque<Instant>,
    /// When the last datagrams were looked at, at most [`READ_PER_SECOND`] of them.
    read: VecDeque<Instant>,
    /// The nonces answered lately, and when (see [`NONCES_KEPT_FOR`]).
    nonces: VecDeque<([u8; NONCE], Instant)>,
    /// The answer being sent.
    out: Vec<u8>,
}

/// Drops what in `times` is a second old at `now`; whether there is room for one more
/// (`most` in a second), which is then counted.
fn room(times: &mut VecDeque<Instant>, most: usize, now: Instant) -> bool {
    while times
        .front()
        .is_some_and(|t| now.saturating_duration_since(*t) >= Duration::from_secs(1))
    {
        times.pop_front();
    }
    if times.len() >= most {
        return false;
    }
    times.push_back(now);
    true
}

impl Responder {
    pub fn new(port: u16, name: String) -> Self {
        Self {
            port,
            name,
            sent: VecDeque::with_capacity(ANSWERS_PER_SECOND),
            read: VecDeque::with_capacity(READ_PER_SECOND),
            nonces: VecDeque::with_capacity(NONCES_KEPT),
            out: Vec::with_capacity(MAX_ANSWER),
        }
    }

    /// Looks at `datagram` from `from` at `now`, with `open` (asked for only once the
    /// datagram reads as a query): `None` when it is no query (too short or too long,
    /// broken), or more came this second than are looked at; else who asked, and the
    /// answer to send back to `from` if it gets one.
    pub fn consider(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        open: impl FnOnce() -> Open,
        now: Instant,
    ) -> Option<(Asker, Option<&[u8]>)> {
        if !room(&mut self.read, READ_PER_SECOND, now) {
            return None;
        }
        let query = QueryRef::read(datagram)?;
        let open = open();
        let found_by = open.secrets.found_by(&query).copied();
        let ours = found_by.is_some();
        let again = self.seen(&query.nonce, now);
        let answered =
            (ours || open.answers_all()) && !again && room(&mut self.sent, ANSWERS_PER_SECOND, now);
        if answered {
            let mut bits = 0;
            if open.pairing {
                bits |= flags::PAIRING;
            }
            if open.answers_all() {
                bits |= flags::ANSWERS_ALL;
            }
            // The tag of the secret the headset asked with: it may hold an older one.
            let secret = found_by.unwrap_or(open.secrets.current);
            Answer::write_to(
                &query.nonce,
                &secret,
                self.port,
                bits,
                &self.name,
                &mut self.out,
            );
            debug_assert!(self.out.len() < datagram.len());
            if self.nonces.len() == NONCES_KEPT {
                self.nonces.pop_front();
            }
            self.nonces.push_back((query.nonce, now));
        }
        let asker = Asker {
            from,
            ours,
            name: query.name.to_owned(),
            answered,
        };
        Some((asker, answered.then_some(self.out.as_slice())))
    }

    /// Whether the query with `nonce` was answered within [`NONCES_KEPT_FOR`] of `now`.
    fn seen(&mut self, nonce: &[u8; NONCE], now: Instant) -> bool {
        while self
            .nonces
            .front()
            .is_some_and(|(_, t)| now.saturating_duration_since(*t) >= NONCES_KEPT_FOR)
        {
            self.nonces.pop_front();
        }
        self.nonces.iter().any(|(n, _)| n == nonce)
    }
}

/// Reads one datagram from `socket` (waiting for it unless the socket does not block)
/// and answers it: who asked, if it was a query.
pub fn answer_one(
    socket: &UdpSocket,
    responder: &mut Responder,
    open: impl FnOnce() -> Open,
) -> std::io::Result<Option<Asker>> {
    let mut buf = [0u8; MAX_QUERY + 1];
    let (n, from) = socket.recv_from(&mut buf)?;
    let Some((asker, answer)) = responder.consider(&buf[..n], from, open, Instant::now()) else {
        return Ok(None);
    };
    if let Some(answer) = answer
        && let Err(e) = socket.send_to(answer, from)
    {
        debug!("cannot answer {from}: {e}");
    }
    debug!(
        "discovery: {} headset at {from} asked{}",
        if asker.ours { "a paired" } else { "another" },
        if asker.answered { ", answered" } else { "" }
    );
    Ok(Some(asker))
}

/// Whether the pairing window is open, as the `find` thread reads it (the control loop
/// owns the window).
#[derive(Clone, Default)]
pub struct PairingOpen(Arc<AtomicBool>);

impl PairingOpen {
    pub fn set(&self, open: bool) {
        self.0.store(open, Ordering::Release);
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Answers the queries on `socket` on a thread of its own, asleep in the kernel until
/// one comes, for as long as the program runs. A headset that is not one of this PC's
/// goes to the control loop (`Event::Asked`), at most once a second for each address.
pub fn spawn(
    socket: UdpSocket,
    mut responder: Responder,
    security: Arc<Security>,
    pairing: PairingOpen,
    events: EventSink,
) -> Result<()> {
    info!(
        "answering headsets that ask for this PC: discovery on UDP port {}",
        socket.local_addr()?.port()
    );
    std::thread::Builder::new()
        .name("find".into())
        .spawn(move || {
            // The addresses told to the control loop lately, and when.
            let mut told: VecDeque<(SocketAddr, Instant)> = VecDeque::with_capacity(16);
            loop {
                let open = || Open {
                    paired: !security.trusted.lock().unwrap().is_empty(),
                    pairing: pairing.get(),
                    secrets: security.find_secrets(),
                };

                let asker = match answer_one(&socket, &mut responder, open) {
                    Ok(Some(asker)) => asker,
                    Ok(None) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        warn!("discovery stopped: {e}");
                        return;
                    }
                };
                if asker.ours {
                    continue;
                }
                let now = Instant::now();
                told.retain(|(_, at)| now.saturating_duration_since(*at) < Duration::from_secs(1));
                if told.iter().any(|(from, _)| *from == asker.from) || told.len() >= 16 {
                    continue;
                }
                told.push_back((asker.from, now));
                if !events.send(Event::Asked(asker)) {
                    return;
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostir_proto::discovery::{MIN_QUERY, Query};

    const PC: [u8; 32] = [3; 32];
    const OTHER_PC: [u8; 32] = [4; 32];
    const OLD: [u8; 32] = [5; 32];

    fn from() -> SocketAddr {
        "192.168.1.20:40000".parse().unwrap()
    }

    fn responder() -> Responder {
        Responder::new(47830, "desk".into())
    }

    const PAIRED: Open = Open {
        paired: true,
        pairing: false,
        secrets: Secrets {
            current: PC,
            stale: [[0; 32]; STALE_KEPT],
            stale_len: 0,
        },
    };

    fn ask(r: &mut Responder, q: &Query, open: Open, now: Instant) -> (Asker, Option<Answer>) {
        let bytes = q.encode();
        let (asker, back) = r.consider(&bytes, from(), || open, now).expect("a query");
        if let Some(b) = &back {
            assert!(
                b.len() < bytes.len(),
                "the answer is smaller than the query"
            );
        }
        let back = back.map(|b| Answer::decode(b).unwrap());
        (asker, back)
    }

    /// Paired: only a query with this PC's tag gets an answer, and the answer says
    /// which PC it is.
    #[test]
    fn a_paired_pc_answers_its_own_headsets_only() {
        let mut r = responder();
        let now = Instant::now();
        let own = Query::new([&OTHER_PC, &PC], "");
        let (asker, answer) = ask(&mut r, &own, PAIRED, now);
        assert!(asker.ours && asker.answered);
        let answer = answer.unwrap();
        assert!(answer.from_pc(&PC));
        assert_eq!(
            (answer.port, answer.flags, answer.name.as_str()),
            (47830, 0, "desk")
        );
        let stranger = Query::new([&OTHER_PC], "Quest 3S");
        let (asker, answer) = ask(&mut r, &stranger, PAIRED, now);
        assert!(!asker.ours && !asker.answered);
        assert_eq!(asker.name, "Quest 3S");
        assert!(answer.is_none());
        let none = Query::new([], "");
        assert!(ask(&mut r, &none, PAIRED, now).1.is_none());
    }

    /// Without a paired headset, or while the pairing window is open, everyone gets
    /// an answer, which says so.
    #[test]
    fn unpaired_or_pairing_answers_everyone() {
        let mut r = responder();
        let now = Instant::now();
        let unpaired = Open {
            paired: false,
            ..PAIRED
        };
        let a = ask(&mut r, &Query::new([&OTHER_PC], ""), unpaired, now)
            .1
            .unwrap();
        assert_eq!(a.flags, flags::ANSWERS_ALL);
        let pairing = Open {
            pairing: true,
            ..PAIRED
        };
        let a = ask(&mut r, &Query::new([&OTHER_PC], ""), pairing, now)
            .1
            .unwrap();
        assert_eq!(a.flags, flags::ANSWERS_ALL | flags::PAIRING);
        // The window closed: silence again.
        assert!(
            ask(&mut r, &Query::new([&OTHER_PC], ""), PAIRED, now)
                .1
                .is_none()
        );
    }

    /// A headset that was off when the PC forgot another one asks with the old secret:
    /// it is answered with the old secret's tag (the one it can check), while the old
    /// secret is still valid; once it is not, silence.
    #[test]
    fn an_old_secret_still_valid_is_answered_with_its_own_tag() {
        let mut r = responder();
        let now = Instant::now();
        let open = Open {
            secrets: Secrets::with_stale(PC, [&OLD, &PC, &OLD]),
            ..PAIRED
        };
        assert_eq!(
            open.secrets.stale(),
            [OLD],
            "each old secret once, never the current"
        );
        let (asker, answer) = ask(&mut r, &Query::new([&OLD], ""), open, now);
        assert!(asker.ours);
        let answer = answer.unwrap();
        assert!(answer.from_pc(&OLD) && !answer.from_pc(&PC));
        let (asker, answer) = ask(&mut r, &Query::new([&OLD], ""), PAIRED, now);
        assert!(!asker.ours && answer.is_none());
        // Never more old secrets than kept.
        let many: Vec<[u8; 32]> = (10..30u8).map(|i| [i; 32]).collect();
        assert_eq!(Secrets::with_stale(PC, &many).stale().len(), STALE_KEPT);
    }

    /// A short query or anything else is not read at all, nor asks who may be answered.
    #[test]
    fn what_is_no_query_is_not_answered() {
        let mut r = responder();
        let now = Instant::now();
        let bytes = Query::new([&PC], "").encode();
        let never = || -> Open { panic!("asked for who may be answered") };
        assert!(
            r.consider(&bytes[..MIN_QUERY - 1], from(), never, now)
                .is_none()
        );
        let mut beacon = b"PROSTIR1\xd6\xbadesk".to_vec();
        beacon.resize(MIN_QUERY, 0);
        assert!(r.consider(&beacon, from(), never, now).is_none());
        assert!(r.consider(&[0; 200], from(), never, now).is_none());
    }

    /// The same query again (its broadcast and its direct copy, or one captured and
    /// sent again) is answered once, until its nonce is forgotten.
    #[test]
    fn a_query_is_answered_once() {
        let mut r = responder();
        let now = Instant::now();
        let own = Query::new([&PC], "");
        assert!(ask(&mut r, &own, PAIRED, now).1.is_some());
        let (asker, answer) = ask(&mut r, &own, PAIRED, now + Duration::from_millis(5));
        assert!(asker.ours && !asker.answered && answer.is_none());
        assert!(ask(&mut r, &own, PAIRED, now + NONCES_KEPT_FOR).1.is_some());
    }

    /// At most 20 answers a second in all, then again as the second moves on.
    #[test]
    fn answers_are_limited_to_twenty_a_second() {
        let mut r = responder();
        let start = Instant::now();
        let answered =
            |r: &mut Responder, at| ask(r, &Query::new([&PC], ""), PAIRED, at).1.is_some();
        for i in 0..20 {
            assert!(
                answered(&mut r, start + Duration::from_millis(i * 10)),
                "{i}"
            );
        }
        assert!(!answered(&mut r, start + Duration::from_millis(500)));
        assert!(!answered(&mut r, start + Duration::from_millis(999)));
        // The first one is a second old now: room for one.
        assert!(answered(&mut r, start + Duration::from_millis(1000)));
        assert!(!answered(&mut r, start + Duration::from_millis(1001)));
        assert!(answered(&mut r, start + Duration::from_millis(2500)));
    }

    /// A flood: beyond 200 datagrams a second nothing is even read.
    #[test]
    fn a_flood_is_dropped_unread() {
        let mut r = responder();
        let now = Instant::now();
        let bytes = Query::new([&OTHER_PC], "").encode();
        let read = (0..300)
            .filter(|_| r.consider(&bytes, from(), || PAIRED, now).is_some())
            .count();
        assert_eq!(read, READ_PER_SECOND);
        assert!(
            r.consider(&bytes, from(), || PAIRED, now + Duration::from_secs(1))
                .is_some()
        );
    }
}
