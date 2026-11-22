//! Finding PCs by asking (step 7 note, 8.1; `prostir_proto::discovery`): the queries
//! and their answers, in a thread of their own (`net-find`), and what the network
//! thread keeps of each trusted PC for them ([`Known`]: its find secret and its last
//! address). Which query goes when, and what an answer leads to, is the core's
//! (`crate::session`); this only does it.
//!
//! # What this side holds against a stranger on the network (the threat model)
//!
//! - **Tracking the headset.** A query has a new random nonce each time, so its tags
//!   change each time; at least eight of them, random ones among them, in a random
//!   order: two queries look like two headsets', and how many PCs it knows does not
//!   show. Each query goes from a socket of its own, so its port does not tie it to
//!   the last one either. Its name goes out only while it looks for a PC to pair with.
//!   What cannot be hidden: that something asks, from its address.
//! - **The find secrets.** Each trusted PC's secret comes from that PC inside the
//!   encrypted session, and only from it (a secret is kept under the key of the PC whose
//!   session gave it); the file is the app's own (mode 0600). Forgetting a PC drops its
//!   secret and its address.
//! - **Forged answers.** An answer is a hint where to connect, nothing more: it changes
//!   nothing kept. Only an answer to the last query (its nonce) counts, at most
//!   [`ANSWERS_MAX`] of PCs that answer everyone (a trusted PC's always counts); whose
//!   it is shows by its tag, which only the PC that
//!   holds the secret can make. Someone who copies a real answer to another address
//!   gets a connection whose handshake shows a key the headset does not trust: it is
//!   offered to the user as a new PC, never connected to as the trusted one, and
//!   pairing needs the code. The last address is kept only once a trusted PC proved
//!   its key in the handshake and its session opened, never from an answer.
//! - **Waking PCs.** Nothing is asked during a session or while the headset is off the
//!   head (the core's), and a trusted PC's last address is dialed by TCP (which wakes a
//!   sleeping PC) at most every 10 s, only while it does not answer.

#![expect(
    clippy::disallowed_methods,
    reason = "the network thread is part of the shell: its sockets, files and clock are its own"
)]

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{info, warn};
use prostir_proto::discovery::{AnswerRef, FindSecret, MAX_ANSWER, Query, flags};
use prostir_proto::secure::{self, TrustStore};

use super::{Config, FoundHost, NetEvent, Shared, lock};
use crate::ids::PcId;
use crate::session::{AskTo, StepId};

/// How long a dial waits for the PC to take the connection.
pub const CONNECT_WITHIN: Duration = Duration::from_millis(400);
/// How long the answers to a query are waited for: they come within milliseconds.
pub const ANSWERS_WITHIN: Duration = Duration::from_millis(300);
/// The most answers of PCs that answer everyone taken from one query.
pub const ANSWERS_MAX: usize = 8;
/// The most answers with one trusted PC's tag taken from one query (a copy of its
/// answer from elsewhere, or several ways to it).
pub const ANSWERS_OF_ONE_PC: u8 = 4;
/// The interfaces' broadcast addresses and `host.txt` are looked up again after this.
const LOOK_AGAIN_AFTER: Duration = Duration::from_secs(10);
/// The first line of the files kept here.
const FORMAT: &str = "format 2";

/// What the network thread keeps of each trusted PC, by the PC's key: the find secret
/// its sessions gave, and the last address it had a session at (`find_secrets` and
/// `last_hosts`, a line `<PC key hex> <secret hex>` or `<PC key hex> <address:port>`
/// each after `format 2`). Read once at the start; written on each change.
pub(super) struct Known {
    secrets: BTreeMap<[u8; 32], FindSecret>,
    last: BTreeMap<[u8; 32], SocketAddr>,
    secrets_file: PathBuf,
    last_file: PathBuf,
}

impl Known {
    /// What the files say, for the PCs `trusted` has only. The one last address that
    /// earlier builds kept (`last_host`, the PC's number and address, or the address
    /// alone) becomes its PC's, once.
    pub(super) fn load(config: &Config, trusted: &TrustStore) -> Self {
        let keys: Vec<[u8; 32]> = trusted.entries().iter().map(|(k, _)| *k).collect();
        let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
        let mut secrets: BTreeMap<[u8; 32], FindSecret> = lines(&read(&config.find_secrets_file))
            .filter_map(|(key, rest)| Some((key, secure::from_hex::<32>(rest)?)))
            .collect();
        let mut last: BTreeMap<[u8; 32], SocketAddr> = lines(&read(&config.last_hosts_file))
            .filter_map(|(key, rest)| Some((key, rest.parse().ok()?)))
            .collect();
        secrets.retain(|k, _| keys.contains(k));
        last.retain(|k, _| keys.contains(k));
        let mut known = Self {
            secrets,
            last,
            secrets_file: config.find_secrets_file.clone(),
            last_file: config.last_hosts_file.clone(),
        };
        if let Some(old) = &config.old_last_host_file
            && let Ok(text) = std::fs::read_to_string(old)
        {
            if let Some((key, addr)) = old_last(&text, &keys)
                && !known.last.contains_key(&key)
            {
                info!("the last PC's address {addr} kept as that PC's");
                known.last.insert(key, addr);
                known.save_last();
            }
            if let Err(e) = std::fs::remove_file(old) {
                warn!("cannot remove {}: {e}", old.display());
            }
        }
        known
    }

    /// The find secrets of the trusted PCs.
    fn secrets(&self) -> impl Iterator<Item = (&[u8; 32], &FindSecret)> {
        self.secrets.iter()
    }

    /// PC `key` gave `secret` in its session.
    pub(super) fn set_secret(&mut self, key: [u8; 32], secret: FindSecret) {
        if self.secrets.insert(key, secret) != Some(secret) {
            info!(
                "PC {} gave its find secret: it is asked for by it from now on",
                PcId::of(&key)
            );
            let text = text(self.secrets.iter().map(|(k, s)| (k, secure::to_hex(s))));
            if let Err(e) = secure::write_atomic(&self.secrets_file, text.as_bytes()) {
                warn!("cannot keep the PC's find secret: {e}");
            }
        }
    }

    /// PC `key` had its session opened at `addr` (a network address, not USB's).
    pub(super) fn seen(&mut self, key: [u8; 32], addr: SocketAddr) {
        if self.last.insert(key, addr) != Some(addr) {
            self.save_last();
        }
    }

    /// PC `key` was forgotten: its secret and address go.
    pub(super) fn forget(&mut self, key: &[u8; 32]) {
        if self.secrets.remove(key).is_some() {
            let text = text(self.secrets.iter().map(|(k, s)| (k, secure::to_hex(s))));
            if let Err(e) = secure::write_atomic(&self.secrets_file, text.as_bytes()) {
                warn!("cannot forget the PC's find secret: {e}");
            }
        }
        if self.last.remove(key).is_some() {
            self.save_last();
        }
    }

    /// The last address PC `pc` had a session at.
    pub(super) fn last_of(&self, pc: PcId) -> Option<SocketAddr> {
        self.last
            .iter()
            .find(|(k, _)| PcId::of(k) == pc)
            .map(|(_, a)| *a)
    }

    fn save_last(&self) {
        let text = text(self.last.iter().map(|(k, a)| (k, a.to_string())));
        if let Err(e) = secure::write_atomic(&self.last_file, text.as_bytes()) {
            warn!("cannot keep the PC's last address: {e}");
        }
    }
}

/// The lines of a file kept here, as (PC key, the rest): `format 2` and lines that do
/// not read are left out.
fn lines(text: &str) -> impl Iterator<Item = ([u8; 32], &str)> {
    text.lines().filter_map(|line| {
        let (key, rest) = line.trim().split_once(' ')?;
        Some((secure::from_hex::<32>(key)?, rest.trim()))
    })
}

/// A file kept here: `format 2`, then a line for each PC.
fn text<'a>(rows: impl Iterator<Item = (&'a [u8; 32], String)>) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{FORMAT}\n");
    for (key, value) in rows {
        let _ = writeln!(text, "{} {value}", secure::to_hex(key));
    }
    text
}

/// What earlier builds kept as the last PC (`<PcId> <address>`, or the address alone):
/// the trusted PC of `keys` it was (by its number, or the only one), and its address.
fn old_last(text: &str, keys: &[[u8; 32]]) -> Option<([u8; 32], SocketAddr)> {
    let mut fields = text.split_whitespace();
    let first = fields.next()?;
    let (pc, addr) = match fields.next() {
        Some(addr) => (PcId::parse(first), parse_addr(addr)?),
        None => (None, parse_addr(first)?),
    };
    let key = match pc {
        Some(pc) => keys.iter().find(|k| PcId::of(k) == pc)?,
        None => match keys {
            [one] => one,
            _ => return None,
        },
    };
    Some((*key, addr))
}

/// A connection to `addr`, if it takes one in time.
pub fn try_connect(addr: SocketAddr) -> Option<TcpStream> {
    TcpStream::connect_timeout(&addr, CONNECT_WITHIN).ok()
}

/// The address in the file at `path` (`IP` or `IP:PORT`).
pub fn read_addr(path: &Path) -> Option<SocketAddr> {
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

/// Whether `addr` is the PC over USB (`adb reverse` to `127.0.0.1:47830`): never kept
/// as a PC's last address.
pub fn is_usb(addr: SocketAddr) -> bool {
    addr.ip().is_loopback() && addr.port() == prostir_proto::TCP_PORT
}

/// A query the core asked for.
pub(super) struct Ask {
    pub step: StepId,
    pub to: AskTo,
    pub pairing: bool,
}

/// Where queries go, as the network thread's config says.
#[derive(Clone)]
pub(super) struct Targets {
    /// The PCs' discovery port, for the addresses known (none: no query goes to them,
    /// nor as a broadcast).
    pub port: Option<u16>,
    /// Also as a broadcast.
    pub broadcast: bool,
    /// The interfaces' broadcast addresses.
    pub broadcasts: fn(&mut Vec<Ipv4Addr>),
    pub host_file: Option<PathBuf>,
}

/// The thread that asks: it waits for the network thread's [`Ask`], sends the query
/// and tells the answers that come within [`ANSWERS_WITHIN`]. It ends once the network
/// thread lets go of it.
pub(super) struct Finder {
    asks: Option<Sender<Ask>>,
}

impl Finder {
    /// Starts the thread (kept in `threads` to be waited for).
    pub(super) fn start(
        targets: Targets,
        shared: Arc<Shared>,
        known: Arc<Mutex<Known>>,
        threads: &mut Vec<std::thread::JoinHandle<()>>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("net-find".into())
            .spawn(move || find(&rx, &targets, &shared, &known))
        {
            Ok(t) => threads.push(t),
            Err(e) => warn!("cannot start the net-find thread: {e}"),
        }
        Self { asks: Some(tx) }
    }

    /// Asks as the core said; without the thread, the wait is over at once.
    pub(super) fn ask(&self, ask: Ask, shared: &Shared) {
        let step = ask.step;
        if self.asks.as_ref().is_none_or(|tx| tx.send(ask).is_err()) {
            shared.tell(NetEvent::Missed { step });
        }
    }

    /// The app is closing: the thread ends after the wait in progress.
    pub(super) fn stop(&mut self) {
        self.asks = None;
    }
}

/// What the asking thread keeps from one query to the next: its memory, made once, and
/// what it looks up now and then (the interfaces' broadcast addresses, `host.txt`).
struct Asking {
    told: bool,
    query: Query,
    bytes: Vec<u8>,
    to: Vec<SocketAddr>,
    broadcasts: Vec<Ipv4Addr>,
    /// The address in `host.txt`.
    typed: Option<SocketAddr>,
    /// When the broadcast addresses and `host.txt` were looked up last.
    looked_at: Option<Instant>,
    /// The addresses that answered the query in progress.
    answered: Vec<SocketAddr>,
    /// Each trusted PC's key, find secret and last address.
    secrets: Vec<([u8; 32], FindSecret, Option<SocketAddr>)>,
    /// How many answers each trusted PC's tag had in the query in progress.
    per_pc: Vec<(PcId, u8)>,
}

fn find(asks: &Receiver<Ask>, targets: &Targets, shared: &Shared, known: &Mutex<Known>) {
    let mut a = Asking {
        told: false,
        query: Query::new([], ""),
        bytes: Vec::with_capacity(prostir_proto::discovery::MIN_QUERY),
        to: Vec::new(),
        broadcasts: Vec::new(),
        typed: None,
        looked_at: None,
        answered: Vec::with_capacity(ANSWERS_MAX),
        secrets: Vec::new(),
        per_pc: Vec::new(),
    };
    while let Ok(ask) = asks.recv() {
        // A socket of its own for each query: its port does not tie one query of this
        // headset to the next (on another network, say).
        if let Some(socket) = open(&mut a.told) {
            ask_once(&mut a, &socket, &ask, targets, shared, known);
        }
        shared.tell(NetEvent::Missed { step: ask.step });
    }
}

/// A socket a query goes from and its answers come to: a port of its own (an answer
/// goes where its query came from), broadcasts allowed.
fn open(told: &mut bool) -> Option<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).and_then(|s| {
        s.set_broadcast(true)?;
        Ok(s)
    });
    match socket {
        Ok(s) => Some(s),
        Err(e) => {
            if !*told {
                warn!("no socket to ask for PCs: {e}; trying again with each query");
                *told = true;
            }
            None
        }
    }
}

/// Where a query of `ask` goes, in `a.to` (see [`Targets`]).
fn targets_of(a: &mut Asking, ask: &Ask, targets: &Targets, known: &Known) {
    a.to.clear();
    let also = match ask.to {
        AskTo::Exactly(to) => return a.to.push(to),
        AskTo::Around { also } => also,
    };
    let Some(port) = targets.port else {
        return;
    };
    let now = Instant::now();
    if a.looked_at
        .is_none_or(|t| now.saturating_duration_since(t) >= LOOK_AGAIN_AFTER)
    {
        a.looked_at = Some(now);
        a.typed = targets.host_file.as_deref().and_then(read_addr);
        if targets.broadcast {
            let before = a.broadcasts.len();
            a.broadcasts.clear();
            (targets.broadcasts)(&mut a.broadcasts);
            if a.broadcasts.len() != before {
                log::debug!("queries go as broadcasts to {:?}", a.broadcasts);
            }
        }
    }
    if targets.broadcast {
        a.to.push(SocketAddr::from((Ipv4Addr::BROADCAST, port)));
        a.to.extend(a.broadcasts.iter().map(|ip| SocketAddr::from((*ip, port))));
    }
    let direct = known.last.values().copied().chain(a.typed).chain(also);
    for addr in direct {
        let to = SocketAddr::new(addr.ip(), port);
        if !a.to.contains(&to) {
            a.to.push(to);
        }
    }
}

/// Sends one query (see [`Targets`]) and tells the answers that come in time.
fn ask_once(
    a: &mut Asking,
    socket: &UdpSocket,
    ask: &Ask,
    targets: &Targets,
    shared: &Shared,
    known: &Mutex<Known>,
) {
    {
        let known = lock(known);
        a.secrets.clear();
        a.secrets.extend(
            known
                .secrets()
                .map(|(k, s)| (*k, *s, known.last.get(k).copied())),
        );
        targets_of(a, ask, targets, &known);
    }
    let name = if ask.pairing {
        shared.name.as_str()
    } else {
        ""
    };
    a.query.renew(a.secrets.iter().map(|(_, s, _)| s), name);
    a.query.encode_into(&mut a.bytes);
    for to in &a.to {
        if let Err(e) = socket.send_to(&a.bytes, to) {
            log::debug!("query to {to}: {e}");
        }
    }
    a.answered.clear();
    a.per_pc.clear();
    // Answers of PCs that answer everyone, counted: a flood of made-up ones cannot keep
    // a trusted PC's out.
    let mut others = 0;
    let until = Instant::now() + ANSWERS_WITHIN;
    let mut buf = [0u8; MAX_ANSWER + 1];
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        let _ = socket.set_read_timeout(Some(left.max(Duration::from_millis(1))));
        let (n, from) = match socket.recv_from(&mut buf) {
            Ok(got) => got,
            Err(e) if super::timed_out(&e) => break,
            Err(e) => {
                log::debug!("waiting for answers: {e}");
                break;
            }
        };
        // Only an answer to this query counts.
        let Some(answer) = AnswerRef::read(&buf[..n]).filter(|x| x.nonce == a.query.nonce) else {
            continue;
        };
        let addr = SocketAddr::new(from.ip(), answer.port);
        if a.answered.contains(&addr) {
            continue;
        }
        let trusted = a.secrets.iter().find(|(_, s, _)| answer.from_pc(s));
        let pc = trusted.map(|(k, _, _)| PcId::of(k));
        let at_last = trusted.is_some_and(|(_, _, last)| *last == Some(addr));
        match pc {
            None if others >= ANSWERS_MAX => continue,
            None => others += 1,
            Some(pc) => match a.per_pc.iter_mut().find(|(p, _)| *p == pc) {
                Some((_, n)) if *n >= ANSWERS_OF_ONE_PC => continue,
                Some((_, n)) => *n += 1,
                None => a.per_pc.push((pc, 1)),
            },
        }
        a.answered.push(addr);
        shared.tell(NetEvent::Heard(FoundHost {
            name: answer.name.to_owned(),
            addr,
            pc,
            pairing: answer.flags & flags::PAIRING != 0,
            heard: Instant::now(),
            at_last,
            label: String::new(),
            ip: String::new(),
        }));
    }
}

/// The interfaces' IPv4 broadcast addresses, in `out`: `255.255.255.255` leaves only
/// by the default route, and Android sometimes sends it on one interface alone.
#[cfg(target_os = "android")]
pub fn interface_broadcasts(out: &mut Vec<Ipv4Addr>) {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, freed once below.
    if unsafe { libc::getifaddrs(&raw mut list) } != 0 {
        return;
    }
    let mut cur = list;
    // SAFETY: `cur` is null or a node of `list`, alive until it is freed below.
    while let Some(ifa) = unsafe { cur.as_ref() } {
        let flags = ifa.ifa_flags;
        let up = flags & libc::IFF_UP as u32 != 0
            && flags & libc::IFF_BROADCAST as u32 != 0
            && flags & libc::IFF_LOOPBACK as u32 == 0;
        // The broadcast address, while IFF_BROADCAST is set.
        let addr = ifa.ifa_ifu;
        // SAFETY: a non-null address the list owns.
        let inet = !addr.is_null() && i32::from(unsafe { (*addr).sa_family }) == libc::AF_INET;
        if up && inet {
            // SAFETY: an AF_INET address is a `sockaddr_in`.
            let sin = unsafe { &*addr.cast::<libc::sockaddr_in>() };
            let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: the list getifaddrs made, freed once; nothing of it is used after.
    unsafe { libc::freeifaddrs(list) };
}

/// No interfaces to look at on this system: `255.255.255.255` only.
#[cfg(not(target_os = "android"))]
pub fn interface_broadcasts(_: &mut Vec<Ipv4Addr>) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last PC as earlier builds kept it: by its number, or the address alone when
    /// one PC is trusted; anything else is nobody's.
    #[test]
    fn the_last_pc_of_earlier_builds_is_its_pcs() {
        let (a, b) = ([0xab; 32], [0xcd; 32]);
        let addr: SocketAddr = "192.168.1.5:47830".parse().unwrap();
        let pc = PcId::of(&a);
        assert_eq!(
            old_last(&format!("{pc} {addr}\n"), &[b, a]),
            Some((a, addr))
        );
        assert_eq!(old_last("192.168.1.5\n", &[a]), Some((a, addr)));
        assert_eq!(old_last("192.168.1.5\n", &[a, b]), None, "whose?");
        assert_eq!(old_last(&format!("{pc} {addr}"), &[b]), None, "not trusted");
        assert_eq!(old_last("", &[a]), None);
        assert_eq!(old_last("garbage", &[a]), None);
    }

    /// The files read back with `format 2`, and without (as the testkit writes them);
    /// a line that does not read is left out.
    #[test]
    fn the_files_read_leniently() {
        let key = [7; 32];
        let written = text([(&key, "192.168.1.5:47830".to_owned())].into_iter());
        assert!(written.starts_with("format 2\n"));
        let read: Vec<_> = lines(&written).collect();
        assert_eq!(read, [(key, "192.168.1.5:47830")]);
        let bare = format!("{} 1.2.3.4:5\nnonsense\n", secure::to_hex(&key));
        assert_eq!(lines(&bare).count(), 1);
    }
}
