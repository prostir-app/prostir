//! The sockets a headset reaches the PC through: the TCP socket connections come to,
//! and the UDP socket for discovery queries. They come from outside in systemd's way
//! (`LISTEN_FDS`, see [`take`]) when systemd's `prostir.socket` or the sockets'
//! watcher holds them, so the full program can come and go while they stay; otherwise
//! the program opens them itself ([`open`]). The same on every Linux desktop and with
//! the fake one. On Windows (step 11c) the watcher passes inherited sockets in
//! `PROSTIR_SOCKETS`, and [`take`] reads them there.

use std::io;
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use prostir_proto::discovery::{MAX_QUERY, MIN_QUERY, QUERY_MAGIC};

/// The first descriptor passed in systemd's way.
const FIRST: RawFd = 3;

/// The sockets of the PC.
pub struct Sockets {
    /// Where connections come.
    pub tcp: TcpListener,
    /// Where discovery queries come; none with `--no-discovery`.
    pub udp: Option<UdpSocket>,
    /// A connection accepted already (by the light phase): set up before the rest.
    pub first: Option<std::net::TcpStream>,
}

impl Sockets {
    /// The TCP port connections come to.
    pub fn port(&self) -> u16 {
        self.tcp.local_addr().map_or(0, |a| a.port())
    }
}

/// The sockets passed in systemd's way, if they were: `LISTEN_FDS` descriptors from 3
/// on, for this process (`LISTEN_PID`). Which is which comes from its type (a stream
/// is the TCP socket, a datagram socket the discovery one); names are not needed. The
/// variables are removed either way, so programs started later do not take them.
///
/// Called before any thread starts: it changes the environment.
pub fn take() -> Result<Option<Sockets>> {
    let pid = std::env::var("LISTEN_PID").ok();
    let count = std::env::var("LISTEN_FDS").ok();
    // SAFETY: called at the start of `prostir serve`, before any thread exists, so
    // nothing reads the environment meanwhile.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    let (Some(pid), Some(count)) = (pid, count) else {
        return Ok(None);
    };
    if pid.trim().parse::<u32>().ok() != Some(std::process::id()) {
        // For another process (a parent that did not clear them).
        return Ok(None);
    }
    let count: RawFd = count.trim().parse().context("LISTEN_FDS is no number")?;
    let (mut tcp, mut udp) = (None, None);
    for fd in FIRST..FIRST + count {
        // SAFETY: fcntl and getsockopt on a descriptor number; a closed or foreign
        // one only fails.
        let kind = unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            let mut kind: libc::c_int = 0;
            let mut len = size_of::<libc::c_int>() as libc::socklen_t;
            let ok = libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut len,
            ) == 0;
            ok.then_some(kind)
        };
        match kind {
            Some(libc::SOCK_STREAM) if tcp.is_none() => {
                // SAFETY: a socket passed to this process, owned from here on.
                tcp = Some(unsafe { TcpListener::from_raw_fd(fd) });
            }
            Some(libc::SOCK_DGRAM) if udp.is_none() => {
                // SAFETY: a socket passed to this process, owned from here on.
                udp = Some(unsafe { UdpSocket::from_raw_fd(fd) });
            }
            // A second one of a kind: which is the one meant is anyone's guess, and
            // answering on one while the other waits would start this again and again.
            Some(libc::SOCK_STREAM | libc::SOCK_DGRAM) => bail!(
                "{count} sockets passed, more than one TCP or UDP socket among them: \
                 prostir.socket listens on one of each (ListenStream, ListenDatagram)"
            ),
            _ => log::warn!("passed descriptor {fd} is not a socket Prostir uses: left alone"),
        }
    }
    let Some(tcp) = tcp else {
        bail!("{count} socket(s) passed, but no TCP socket among them");
    };
    // Whoever made them, they block: the threads that read them sleep in the kernel.
    tcp.set_nonblocking(false)?;
    if let Some(udp) = &udp {
        udp.set_nonblocking(false)?;
        // On a socket that systemd holds, the filter stays after this process ends:
        // what is no query never wakes it again.
        filter_queries(udp)?;
    }
    Ok(Some(Sockets {
        tcp,
        udp,
        first: None,
    }))
}

/// Opens the sockets here: TCP on `port`, and UDP on `discovery` (none: no discovery).
/// Port 0 lets the system pick (the tests).
pub fn open(port: u16, discovery: Option<u16>) -> Result<Sockets> {
    let tcp = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
        .with_context(|| format!("cannot listen on port {port}"))?;
    let udp = match discovery {
        None => None,
        Some(port) => {
            let udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))
                .with_context(|| format!("cannot listen for discovery on UDP port {port}"))?;
            filter_queries(&udp)?;
            Some(udp)
        }
    };
    Ok(Sockets {
        tcp,
        udp,
        first: None,
    })
}

/// Lets only discovery queries into `socket`: datagrams of [`MIN_QUERY`] to
/// [`MAX_QUERY`] bytes that start with `PRSQ`. The kernel drops the rest before
/// anyone wakes: beacons of older PCs around (they went to the same port), short
/// queries, noise.
pub fn filter_queries(socket: &UdpSocket) -> io::Result<()> {
    // A filter on a UDP socket sees the datagram from its UDP header (8 bytes) on.
    const HEADER: u32 = 8;
    let min = HEADER + MIN_QUERY as u32;
    let max = HEADER + MAX_QUERY as u32;
    let magic = u32::from_be_bytes(*QUERY_MAGIC);
    let op = |code: u32, jt: u8, jf: u8, k: u32| libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    };
    use libc::{
        BPF_ABS, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JMP, BPF_K, BPF_LD, BPF_LEN, BPF_RET, BPF_W,
    };
    let program = [
        // The length: at least the smallest query, at most the biggest.
        op(BPF_LD | BPF_W | BPF_LEN, 0, 0, 0),
        op(BPF_JMP | BPF_JGE | BPF_K, 0, 4, min),
        op(BPF_JMP | BPF_JGT | BPF_K, 3, 0, max),
        // The first four bytes after the header: the magic.
        op(BPF_LD | BPF_W | BPF_ABS, 0, 0, HEADER),
        op(BPF_JMP | BPF_JEQ | BPF_K, 0, 1, magic),
        op(BPF_RET | BPF_K, 0, 0, u32::MAX),
        op(BPF_RET | BPF_K, 0, 0, 0),
    ];
    let prog = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_ptr().cast_mut(),
    };
    // SAFETY: a valid socket and a filter program that lives through the call (the
    // kernel copies it).
    let failed = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ATTACH_FILTER,
            (&raw const prog).cast(),
            size_of::<libc::sock_fprog>() as libc::socklen_t,
        ) != 0
    };
    if failed {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sleeps until one of `fds` can be read, or `timeout` passes (`None`: no time
/// limit): which ones can.
pub fn readable(fds: &[RawFd], timeout: Option<Duration>) -> io::Result<Vec<bool>> {
    let mut polled: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let ms = timeout.map_or(-1, |t| {
        // Rounded up: a wait cut short would only come back to wait again.
        t.as_micros().div_ceil(1000).min(i32::MAX as u128) as libc::c_int
    });
    loop {
        // SAFETY: a valid array of pollfd of the length given.
        let n = unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, ms) };
        if n >= 0 {
            break;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
        // A signal: its pipe is among the descriptors, look again.
    }
    Ok(polled.iter().map(|p| p.revents != 0).collect())
}

/// Whether the peer of `socket` closed its end, or the connection broke: looked at
/// without waiting and without reading what it sent.
pub fn hung_up(socket: &impl AsRawFd) -> bool {
    let mut polled = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one valid pollfd, and no wait.
    let n = unsafe { libc::poll(&raw mut polled, 1, 0) };
    n > 0 && polled.revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR) != 0
}

/// Starts the program `exe` with `args`, passing it `sockets` in systemd's way (the
/// TCP socket as descriptor 3, the UDP one as 4, `LISTEN_FDS`, `LISTEN_PID`): what the
/// sockets' watcher does to start the full program. The environment is this
/// process's. Its process id.
pub fn start_with(
    exe: &std::path::Path,
    args: &[String],
    sockets: &Sockets,
) -> io::Result<libc::pid_t> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let cstring =
        |b: &[u8]| CString::new(b).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
    let path = cstring(exe.as_os_str().as_bytes())?;
    let mut argv = vec![path.clone()];
    for a in args {
        argv.push(cstring(a.as_bytes())?);
    }
    let fds: Vec<RawFd> = std::iter::once(sockets.tcp.as_raw_fd())
        .chain(sockets.udp.as_ref().map(AsRawFd::as_raw_fd))
        .collect();
    let mut env = Vec::new();
    for (k, v) in std::env::vars_os() {
        if k.as_bytes().starts_with(b"LISTEN_") {
            continue;
        }
        let mut entry = k.as_bytes().to_vec();
        entry.push(b'=');
        entry.extend_from_slice(v.as_bytes());
        if let Ok(c) = CString::new(entry) {
            env.push(c);
        }
    }
    env.push(cstring(format!("LISTEN_FDS={}", fds.len()).as_bytes())?);
    // Its number is written in after the fork, where it is known: room for it here, in
    // zeros that end the string wherever the digits stop.
    let mut pid_entry = b"LISTEN_PID=".to_vec();
    let digits_at = pid_entry.len();
    pid_entry.resize(digits_at + 24, 0);
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let pid_slot = pid_entry.as_mut_ptr();
    let mut env_ptrs: Vec<*const libc::c_char> = env.iter().map(|e| e.as_ptr()).collect();
    env_ptrs.push(pid_slot.cast_const().cast());
    env_ptrs.push(std::ptr::null());
    // SAFETY: after the fork the child only calls async-signal-safe functions (getpid,
    // fcntl, dup2, sigprocmask, execve, _exit) and writes into memory prepared above;
    // it allocates nothing. The parent goes on as before.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid > 0 {
            return Ok(pid);
        }
        // The child: its own number into the environment.
        let mut n = libc::getpid() as u32;
        let mut digits = [0u8; 12];
        let mut len = 0;
        loop {
            digits[len] = b'0' + (n % 10) as u8;
            len += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for i in 0..len {
            *pid_slot.add(digits_at + i) = digits[len - 1 - i];
        }
        // The sockets out of the way of 3 and 4 first, then into place (without
        // close-on-exec, which dup2 leaves off).
        let mut high = [0; 2];
        for (i, &fd) in fds.iter().enumerate() {
            high[i] = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10);
            if high[i] < 0 {
                libc::_exit(126);
            }
        }
        for (i, &fd) in high.iter().take(fds.len()).enumerate() {
            if libc::dup2(fd, FIRST + i as RawFd) < 0 {
                libc::_exit(126);
            }
        }
        // SIGUSR1 (the settings window, passed on by the watcher) waits blocked until
        // the program has its handler: before that it would end it.
        let mut held: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut held);
        libc::sigaddset(&raw mut held, libc::SIGUSR1);
        libc::sigprocmask(libc::SIG_BLOCK, &raw const held, std::ptr::null_mut());
        libc::execve(path.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr());
        libc::_exit(127);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn received(socket: &UdpSocket) -> Option<usize> {
        let mut buf = [0u8; 2048];
        socket.recv(&mut buf).ok()
    }

    /// Only whole queries get through the filter: beacons, short, long or other
    /// datagrams are dropped by the kernel.
    #[test]
    fn only_queries_get_through() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        filter_queries(&socket).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let to = socket.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let query = |len: usize| {
            let mut q = QUERY_MAGIC.to_vec();
            q.resize(len, 0);
            q
        };
        // An older PC's beacon: `PROSTIR1`, its port and name.
        let beacon = b"PROSTIR1\xd6\xbadesk".to_vec();
        let mut long_beacon = beacon.clone();
        long_beacon.resize(200, 0);
        let mut other = b"PRSA".to_vec();
        other.resize(200, 0);
        for dropped in [
            beacon,
            long_beacon,
            other,
            query(MIN_QUERY - 1),
            query(MAX_QUERY + 1),
        ] {
            sender.send_to(&dropped, to).unwrap();
        }
        for kept in [MIN_QUERY, MAX_QUERY] {
            sender.send_to(&query(kept), to).unwrap();
        }
        assert_eq!(received(&socket), Some(MIN_QUERY));
        assert_eq!(received(&socket), Some(MAX_QUERY));
        assert_eq!(received(&socket), None, "nothing else got through");
    }

    /// Readable descriptors are told; nothing to read waits out its time.
    #[test]
    fn waiting_for_what_can_be_read() {
        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let started = Instant::now();
        let ready = readable(
            &[a.as_raw_fd(), b.as_raw_fd()],
            Some(Duration::from_millis(50)),
        )
        .unwrap();
        assert_eq!(ready, [false, false]);
        assert!(started.elapsed() >= Duration::from_millis(50));
        a.send_to(b"x", b.local_addr().unwrap()).unwrap();
        let ready = readable(&[a.as_raw_fd(), b.as_raw_fd()], None).unwrap();
        assert_eq!(ready, [false, true]);
    }
}
