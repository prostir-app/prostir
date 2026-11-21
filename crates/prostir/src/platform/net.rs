//! The connection to the headset: socket options, which depend on the operating
//! system and not on the desktop (the fake desktop of the tests uses them too).

use std::net::TcpStream;
#[cfg(target_os = "linux")]
use std::time::Duration;

use log::warn;
#[cfg(target_os = "linux")]
use prostir_proto::link::LinkInfo;
use prostir_proto::link::tcp::SocketOptions;

/// The options of a connection to the headset.
pub struct TcpOptions;

/// DSCP AF41 (34) in the old type-of-service byte: interactive video.
#[cfg(target_os = "linux")]
const AF41: u32 = 34 << 2;

#[cfg(target_os = "linux")]
fn set(stream: &TcpStream, level: libc::c_int, name: libc::c_int, value: u32) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid socket and a 4-byte option value that lives through the call.
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            level,
            name,
            (&raw const value).cast(),
            size_of::<u32>() as libc::socklen_t,
        ) == 0
    }
}

#[cfg(target_os = "linux")]
impl SocketOptions for TcpOptions {
    /// Notices a headset that vanished without closing the connection (switched off,
    /// out of Wi-Fi) within about 10 s, also while pairing, when the session's own
    /// silence check does not run yet; keeps little unsent data in the kernel.
    fn prepare(&self, stream: &TcpStream) {
        let ok = set(stream, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1)
            && set(stream, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 4)
            && set(stream, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 2)
            && set(stream, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3)
            // Unacknowledged data (frames) also gives up after 10 s.
            && set(stream, libc::IPPROTO_TCP, libc::TCP_USER_TIMEOUT, 10_000)
            // Linux would let unsent data grow to megabytes, and nothing can overtake
            // what is in the kernel: two packets of 16 KiB, so it never runs dry
            // between two wake-ups of the writer, and the rest waits in the outbox,
            // where sound goes first and late frames can still be dropped.
            && set(stream, libc::IPPROTO_TCP, libc::TCP_NOTSENT_LOWAT, 32 * 1024);
        if !ok {
            warn!(
                "cannot set up the connection's socket: {}",
                std::io::Error::last_os_error()
            );
        }
        // Marked as video (DSCP AF41): Wi-Fi with WMM sends it in the video access
        // category, with shorter waits for the air than ordinary traffic. A network
        // that ignores the mark loses nothing.
        let marked = match stream.local_addr() {
            Ok(std::net::SocketAddr::V4(_)) => set(stream, libc::IPPROTO_IP, libc::IP_TOS, AF41),
            Ok(std::net::SocketAddr::V6(_)) => {
                set(stream, libc::IPPROTO_IPV6, libc::IPV6_TCLASS, AF41)
            }
            Err(_) => false,
        };
        if !marked {
            log::debug!(
                "cannot mark the connection as video: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    /// TCP spreads its packets out to this rate itself (no queueing discipline
    /// needed).
    fn set_pace(&self, stream: &TcpStream, bytes_per_sec: u64) {
        let rate = match bytes_per_sec {
            0 => u32::MAX, // what the kernel reads as no limit
            n => u32::try_from(n).unwrap_or(u32::MAX - 1),
        };
        if !set(stream, libc::SOL_SOCKET, libc::SO_MAX_PACING_RATE, rate) {
            warn!(
                "cannot pace the connection: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    /// From `TCP_INFO`.
    fn link_info(&self, stream: &TcpStream) -> Option<LinkInfo> {
        use std::os::fd::AsRawFd;
        let mut info = TcpInfo::default();
        let mut len = size_of::<TcpInfo>() as libc::socklen_t;
        // SAFETY: a valid socket, and a buffer of `len` bytes the kernel fills (at
        // most that many; `len` says how many).
        let ok = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_INFO,
                (&raw mut info).cast(),
                &raw mut len,
            ) == 0
        };
        // An older kernel fills less: nothing to tell then.
        let us = |v: u32| Duration::from_micros(u64::from(v));
        (ok && len as usize >= size_of::<TcpInfo>()).then(|| LinkInfo {
            delivery_rate: info.delivery_rate,
            rtt: us(info.rtt),
            rtt_var: us(info.rtt_var),
            min_rtt: us(info.min_rtt),
        })
    }
}

/// The start of Linux's `struct tcp_info` (`linux/tcp.h`), up to the delivery rate.
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Default)]
struct TcpInfo {
    state: u8,
    ca_state: u8,
    retransmits: u8,
    probes: u8,
    backoff: u8,
    options: u8,
    wscale: u8,
    /// `tcpi_delivery_rate_app_limited` and `tcpi_fastopen_client_fail`.
    flags: u8,
    /// From `tcpi_rto` to `tcpi_rcv_ssthresh`.
    counters: [u32; 15],
    /// The smoothed round trip and its variation, microseconds.
    rtt: u32,
    rtt_var: u32,
    /// From `tcpi_snd_ssthresh` to `tcpi_total_retrans`.
    more_counters: [u32; 7],
    /// `tcpi_pacing_rate`, `tcpi_max_pacing_rate`, `tcpi_bytes_acked`,
    /// `tcpi_bytes_received`.
    totals: [u64; 4],
    /// `tcpi_segs_out`, `tcpi_segs_in`, `tcpi_notsent_bytes`.
    segments: [u32; 3],
    /// The shortest round trip seen, microseconds.
    min_rtt: u32,
    /// `tcpi_data_segs_in`, `tcpi_data_segs_out`.
    data_segments: [u32; 2],
    delivery_rate: u64,
}

#[cfg(target_os = "linux")]
const _: () = {
    assert!(std::mem::offset_of!(TcpInfo, rtt) == 68);
    assert!(std::mem::offset_of!(TcpInfo, min_rtt) == 148);
    assert!(std::mem::offset_of!(TcpInfo, delivery_rate) == 160);
};

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;

    use super::*;

    fn get(stream: &TcpStream, level: libc::c_int, name: libc::c_int) -> u32 {
        let mut value = 0u32;
        let mut len = size_of::<u32>() as libc::socklen_t;
        // SAFETY: a valid socket and a 4-byte buffer the kernel fills.
        let ok = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                level,
                name,
                (&raw mut value).cast(),
                &raw mut len,
            ) == 0
        };
        assert!(ok, "{}", std::io::Error::last_os_error());
        value
    }

    /// A connection to the headset goes out marked as video, over IPv4 and IPv6.
    #[test]
    fn the_connection_is_marked_as_video() {
        let v4 = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(v4.local_addr().unwrap()).unwrap();
        TcpOptions.prepare(&stream);
        assert_eq!(get(&stream, libc::IPPROTO_IP, libc::IP_TOS), AF41);
        // A PC without IPv6 has nothing to mark there.
        if let Ok(v6) = TcpListener::bind("[::1]:0") {
            let stream = TcpStream::connect(v6.local_addr().unwrap()).unwrap();
            TcpOptions.prepare(&stream);
            assert_eq!(get(&stream, libc::IPPROTO_IPV6, libc::IPV6_TCLASS), AF41);
        }
    }
}
