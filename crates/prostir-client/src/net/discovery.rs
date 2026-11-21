//! Finding a PC on the network until step 7.9 replaces it with queries: listening for
//! the PCs' beacons, the addresses typed or kept, and a probe of the local `/24`. Which
//! of them and when is the core's (`crate::session`); this only does them.

#![expect(
    clippy::disallowed_methods,
    reason = "the network thread is part of the shell: its sockets, files and clock are its own"
)]

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{info, warn};

use super::{FoundHost, NetEvent, Shared, lock};

/// How long a dial waits for the PC to take the connection.
pub const CONNECT_WITHIN: Duration = Duration::from_millis(400);
/// How long one listening for beacons lasts.
const LISTEN_FOR: Duration = Duration::from_millis(1200);
/// A PC not heard for this long is no longer listed.
const HEARD_WITHIN: Duration = Duration::from_secs(6);

/// The socket PCs announce themselves to and what was heard on it, kept between
/// listenings (bound on the first one: a network thread of an earlier start that has
/// not stopped yet may still hold it, so it is tried again each time until this one
/// has it).
#[derive(Default)]
pub(super) struct Beacon {
    socket: Option<Arc<UdpSocket>>,
    told: bool,
    heard: Arc<Mutex<Finder>>,
    quit: Arc<AtomicBool>,
}

impl Beacon {
    /// What a listening needs, binding the socket if it is not yet; `None` without it.
    pub(super) fn listener(&mut self) -> Option<Listener> {
        if self.socket.is_none() {
            match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, prostir_proto::DISCOVERY_PORT)) {
                Ok(socket) => {
                    if self.told {
                        info!("discovery socket open");
                    }
                    self.socket = Some(Arc::new(socket));
                }
                Err(e) if !self.told => {
                    warn!("discovery socket: {e}; trying again each round");
                    self.told = true;
                }
                Err(_) => {}
            }
        }
        Some(Listener {
            socket: self.socket.clone()?,
            heard: self.heard.clone(),
            quit: self.quit.clone(),
        })
    }

    /// The app is closing: a listening in progress stops.
    pub(super) fn quit(&self) {
        self.quit.store(true, Ordering::Relaxed);
    }
}

/// One listening for beacons.
pub(super) struct Listener {
    socket: Arc<UdpSocket>,
    heard: Arc<Mutex<Finder>>,
    quit: Arc<AtomicBool>,
}

impl Listener {
    /// Listens for a moment and tells the PCs heard when the list changed. Looks every
    /// 100 ms whether the app is closing, and stops then.
    pub(super) fn run(&self, shared: &Shared) {
        let until = Instant::now() + LISTEN_FOR;
        let mut buf = [0u8; 512];
        let mut heard = Vec::new();
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if self.quit.load(Ordering::Relaxed) {
                return;
            }
            let slice = left.clamp(Duration::from_millis(1), Duration::from_millis(100));
            let _ = self.socket.set_read_timeout(Some(slice));
            let (n, from) = match self.socket.recv_from(&mut buf) {
                Ok(got) => got,
                // Nothing in this slice: the next one.
                Err(e) if super::timed_out(&e) => continue,
                Err(_) => break,
            };
            if let Some((port, name)) = prostir_proto::decode_beacon(&buf[..n]) {
                heard.push((SocketAddr::new(from.ip(), port), name));
            }
        }
        let changed = lock(&self.heard).heard(heard, Instant::now());
        if let Some(hosts) = changed {
            shared.tell(NetEvent::Hosts(hosts));
        }
    }
}

/// PCs heard on the network recently.
#[derive(Default)]
struct Finder {
    seen: HashMap<SocketAddr, (String, Instant)>,
    reported: Vec<FoundHost>,
}

impl Finder {
    /// Takes what one listening heard at `now`: the list, if it changed.
    fn heard(&mut self, heard: Vec<(SocketAddr, String)>, now: Instant) -> Option<Vec<FoundHost>> {
        for (addr, name) in heard {
            self.seen.insert(addr, (name, now));
        }
        self.seen
            .retain(|_, (_, at)| now.saturating_duration_since(*at) < HEARD_WITHIN);
        let mut hosts: Vec<FoundHost> = self
            .seen
            .iter()
            .map(|(addr, (name, _))| FoundHost {
                name: name.clone(),
                addr: *addr,
            })
            .collect();
        hosts.sort_by(|a, b| a.name.cmp(&b.name).then(a.addr.cmp(&b.addr)));
        (hosts != self.reported).then(|| {
            self.reported.clone_from(&hosts);
            hosts
        })
    }
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

/// Remembers `addr` as the last PC that worked over the network (the next start tries
/// it before searching).
pub fn keep_last(path: &Path, addr: SocketAddr) -> io::Result<()> {
    std::fs::write(path, addr.to_string())
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

/// Probes the local `/24` for a PC that does not announce itself (until step 7.9).
pub fn scan_subnet() -> Option<TcpStream> {
    let me = local_ipv4()?;
    let [a, b, c, own] = me.octets();
    let found = Mutex::new(None);
    std::thread::scope(|scope| {
        for d in (1..=254u8).filter(|&d| d != own) {
            let found = &found;
            scope.spawn(move || {
                let addr =
                    SocketAddr::new(Ipv4Addr::new(a, b, c, d).into(), prostir_proto::TCP_PORT);
                if let Some(stream) = try_connect(addr) {
                    lock(found).get_or_insert(stream);
                }
            });
        }
    });
    let stream = found.into_inner().ok().flatten()?;
    info!(
        "found host by scanning {a}.{b}.{c}.0/24: {:?}",
        stream.peer_addr()
    );
    Some(stream)
}
