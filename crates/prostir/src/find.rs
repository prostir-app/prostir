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
//!   `platform::sockets::filter_queries`). A query with this PC's tag is always looked
//!   at and answered (once for each nonce: a flood of them needs the secret, and replays
//!   are answered once). Of the rest, at most [`READ_PER_SOURCE`] a second from one
//!   address and [`READ_PER_SECOND`] in all are looked at, and [`ANSWERS_PER_SECOND`]
//!   answered; the rest are dropped. Nothing is allocated for a datagram. An answer is
//!   always smaller than the query, so a forged sender address gets less than was sent:
//!   no amplification.
//! - **Changing state.** Nothing a query says changes anything kept: who is trusted,
//!   the find secrets and the pairing window change only through the person at the PC
//!   or an encrypted session. A stranger's query that looks for a PC to pair with (it
//!   gives a name) only brings the notice of a new headset nearby (at most once a
//!   minute) and the local API's `nearby` list.
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
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{debug, info, warn};
use prostir_proto::discovery::{
    Answer, FindSecret, MAX_ANSWER, MAX_HEADSET_NAME, MAX_QUERY, NONCE, QueryRef, flags,
};

use crate::events::{Event, EventSink};
use crate::pairing::Security;

/// Answers sent in a second, at most, whoever asks.
const ANSWERS_PER_SECOND: usize = 20;
/// Queries without this PC's tag looked at in a second, at most: the rest are dropped.
const READ_PER_SECOND: usize = 200;
/// The same, from one address.
const READ_PER_SOURCE: u16 = 20;
/// The addresses counted for [`READ_PER_SOURCE`] at once (a fixed table: made-up
/// addresses only push each other out, and the limit in all still holds).
const SOURCES_KEPT: usize = 32;
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

    /// `current`, and the different ones among `stale`, the newest first (the first
    /// [`STALE_KEPT`]).
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

/// The name a headset gave in its query, kept in place (no allocation for a datagram).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HeadsetName {
    bytes: [u8; MAX_HEADSET_NAME],
    len: u8,
}

impl HeadsetName {
    /// `name`, cut at a character's edge to [`MAX_HEADSET_NAME`] bytes.
    pub fn new(name: &str) -> Self {
        let mut end = name.len().min(MAX_HEADSET_NAME);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0; MAX_HEADSET_NAME];
        bytes[..end].copy_from_slice(&name.as_bytes()[..end]);
        Self {
            bytes,
            len: end as u8,
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

impl std::fmt::Debug for HeadsetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}

/// A headset that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asker {
    pub from: SocketAddr,
    /// Its query carries this PC's tag: one of its own headsets (it has the PC's find
    /// secret).
    pub ours: bool,
    /// The name it gave (only while it looks for a PC to pair with).
    pub name: HeadsetName,
    /// It got an answer (not a repeat of a query answered lately).
    pub answered: bool,
}

/// What came from one address lately, for [`READ_PER_SOURCE`].
#[derive(Clone, Copy)]
struct Source {
    ip: IpAddr,
    since: Instant,
    count: u16,
}

/// What the PC answers with: its port and name, and the limits on how much it reads
/// and answers. Its memory is made once.
pub struct Responder {
    port: u16,
    name: String,
    /// When the last answers without a tag went, at most [`ANSWERS_PER_SECOND`].
    sent: VecDeque<Instant>,
    /// When the last queries without a tag were looked at, [`READ_PER_SECOND`] at most.
    read: VecDeque<Instant>,
    /// The addresses that sent them lately.
    sources: Vec<Source>,
    /// The nonces answered lately, and when (see [`NONCES_KEPT_FOR`]).
    nonces: VecDeque<([u8; NONCE], Instant)>,
    /// The answer being sent.
    out: Vec<u8>,
}

/// Whether `ip` may have another query looked at `now` (it is then counted).
fn source_room(sources: &mut Vec<Source>, ip: IpAddr, now: Instant) -> bool {
    let fresh = Source {
        ip,
        since: now,
        count: 1,
    };
    if let Some(s) = sources.iter_mut().find(|s| s.ip == ip) {
        if now.saturating_duration_since(s.since) >= Duration::from_secs(1) {
            *s = fresh;
            return true;
        }
        if s.count >= READ_PER_SOURCE {
            return false;
        }
        s.count += 1;
        return true;
    }
    if sources.len() < SOURCES_KEPT {
        sources.push(fresh);
    } else if let Some(oldest) = sources.iter_mut().min_by_key(|s| s.since) {
        *oldest = fresh;
    }
    true
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
            sources: Vec::with_capacity(SOURCES_KEPT),
            nonces: VecDeque::with_capacity(NONCES_KEPT),
            out: Vec::with_capacity(MAX_ANSWER),
        }
    }

    /// Looks at `datagram` from `from` at `now`, with `open` (asked for only once the
    /// datagram reads as a query): `None` when it is no query (too short or too long,
    /// broken), or one without this PC's tag beyond what is looked at (see the module);
    /// else who asked, and the answer to send back to `from` if it gets one.
    pub fn consider(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        open: impl FnOnce() -> Open,
        now: Instant,
    ) -> Option<(Asker, Option<&[u8]>)> {
        let query = QueryRef::read(datagram)?;
        let open = open();
        let found_by = open.secrets.found_by(&query).copied();
        let ours = found_by.is_some();
        // The PC's own headsets are never held back by anyone else's flood.
        if !ours
            && !(source_room(&mut self.sources, from.ip(), now)
                && room(&mut self.read, READ_PER_SECOND, now))
        {
            return None;
        }
        let again = self.seen(&query.nonce, now);
        let answered = (ours || open.answers_all())
            && !again
            && (ours || room(&mut self.sent, ANSWERS_PER_SECOND, now));
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
            name: HeadsetName::new(query.name),
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
        assert_eq!(asker.name.as_str(), "Quest 3S");
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

    /// At most 20 answers a second in all to queries without the PC's tag (the
    /// pairing window open), then again as the second moves on; its own headsets are
    /// answered all the same.
    #[test]
    fn answers_are_limited_to_twenty_a_second() {
        let mut r = responder();
        let start = Instant::now();
        let open = Open {
            pairing: true,
            ..PAIRED
        };
        let at = |i: u8| SocketAddr::from(([10, 0, 1, i], 4000));
        let answered = |r: &mut Responder, t: Instant| {
            // From addresses of their own: the limit for each address is not the one.
            let i = (t - start).as_millis() as u8;
            let bytes = Query::new([&OTHER_PC], "").encode();
            r.consider(&bytes, at(i), || open, t)
                .is_some_and(|(a, _)| a.answered)
        };
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
        let own = Query::new([&PC], "").encode();
        assert!(
            r.consider(&own, from(), || PAIRED, start + Duration::from_millis(1001))
                .is_some_and(|(a, _)| a.answered),
            "its own headset, beyond the limit"
        );
    }

    /// A flood: from one address, 20 queries a second are looked at; from many, 200
    /// in all. The PC's own headset gets through all the same.
    #[test]
    fn a_flood_is_dropped_but_not_the_pcs_own_headsets() {
        let mut r = responder();
        let now = Instant::now();
        let bytes = Query::new([&OTHER_PC], "").encode();
        let read = (0..100)
            .filter(|_| r.consider(&bytes, from(), || PAIRED, now).is_some())
            .count();
        assert_eq!(read, usize::from(READ_PER_SOURCE), "from one address");
        let many = (0..=255u8)
            .flat_map(|a| (0..4u8).map(move |b| SocketAddr::from(([10, 9, a, b], 4000))))
            .filter(|f| r.consider(&bytes, *f, || PAIRED, now).is_some())
            .count();
        assert_eq!(
            many,
            READ_PER_SECOND - usize::from(READ_PER_SOURCE),
            "in all"
        );
        let own = Query::new([&PC], "").encode();
        let (asker, answer) = r.consider(&own, from(), || PAIRED, now).expect("read");
        assert!(asker.ours && answer.is_some(), "its own headset");
        assert!(
            r.consider(&bytes, from(), || PAIRED, now + Duration::from_secs(1))
                .is_some()
        );
    }

    /// A name is kept in place, cut at a character's edge.
    #[test]
    fn a_headsets_name_is_kept_in_place() {
        assert_eq!(HeadsetName::new("Quest 3S").as_str(), "Quest 3S");
        let long = "я".repeat(40);
        assert_eq!(HeadsetName::new(&long).as_str().len(), MAX_HEADSET_NAME);
        assert_eq!(HeadsetName::new("").as_str(), "");
    }
}
