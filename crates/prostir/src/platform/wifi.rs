//! The PC's own Wi-Fi, for diagnostics: whether the PC reaches its network over Wi-Fi
//! (its default route goes through a wireless interface), on which channel, and
//! whether the card's power saving is on. With power saving the access point holds
//! what comes for the PC (the headset's acknowledgements, pings, microphone, requests
//! for keyframes) until the card wakes, which shows as pauses in the picture. Prostir
//! only says so; it changes nothing of the network.
//!
//! Read from the kernel (`/proc/net/route`, `/sys/class/net`, and nl80211 over generic
//! netlink, as `iw` asks it), with no program started. The fake desktop of the tests
//! reads `PROSTIR_FAKE_WIFI` (`wlan0,on`) instead of the PC's.

/// The PC's Wi-Fi link to its network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiLink {
    /// `wlan0`.
    pub interface: String,
    /// Its power saving, when the card says.
    pub power_save: Option<bool>,
    /// The channel's frequency and width in MHz, when known.
    pub freq_mhz: Option<u32>,
    pub width_mhz: Option<u32>,
}

/// The Wi-Fi the PC reaches its network over; `None` when it is on a cable (or has no
/// network).
#[cfg(feature = "fake-desktop")]
pub fn look() -> Option<WifiLink> {
    let text = std::env::var("PROSTIR_FAKE_WIFI").ok()?;
    let (interface, save) = text.split_once(',')?;
    Some(WifiLink {
        interface: interface.to_owned(),
        power_save: Some(save == "on"),
        freq_mhz: Some(5180),
        width_mhz: Some(80),
    })
}

/// The Wi-Fi the PC reaches its network over; `None` when it is on a cable (or has no
/// network).
#[cfg(all(target_os = "linux", not(feature = "fake-desktop")))]
pub fn look() -> Option<WifiLink> {
    let route = std::fs::read_to_string("/proc/net/route").ok()?;
    let interface = default_route(&route)?;
    let sys = std::path::Path::new("/sys/class/net").join(&interface);
    if !sys.join("wireless").exists() && !sys.join("phy80211").exists() {
        return None;
    }
    let index: u32 = std::fs::read_to_string(sys.join("ifindex"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let asked = nl80211::ask(index);
    if let Err(e) = &asked {
        log::debug!("Wi-Fi of {interface}: nl80211 did not answer: {e}");
    }
    let asked = asked.unwrap_or_default();
    Some(WifiLink {
        interface,
        power_save: asked.power_save,
        freq_mhz: asked.freq_mhz,
        width_mhz: asked.width_mhz,
    })
}

/// The interface of the default route in `/proc/net/route` (the one with the lowest
/// metric).
#[cfg_attr(
    feature = "fake-desktop",
    allow(
        dead_code,
        reason = "the fake desktop reads its Wi-Fi from the environment"
    )
)]
fn default_route(table: &str) -> Option<String> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (iface, dest, flags, metric) = (f.first()?, f.get(1)?, f.get(3)?, f.get(6)?);
            let up = u32::from_str_radix(flags, 16).ok()? & 1 != 0;
            (*dest == "00000000" && up).then(|| (metric.parse::<u32>().unwrap_or(0), *iface))
        })
        .min()
        .map(|(_, iface)| iface.to_owned())
}

/// Attributes of a netlink message, as (type, payload), from `bytes` (after the
/// headers).
#[cfg_attr(
    feature = "fake-desktop",
    allow(
        dead_code,
        reason = "the fake desktop reads its Wi-Fi from the environment"
    )
)]
fn attributes(mut bytes: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while bytes.len() >= 4 {
        let len = usize::from(u16::from_ne_bytes([bytes[0], bytes[1]]));
        // Without the "nested" and "byte order" flags.
        let kind = u16::from_ne_bytes([bytes[2], bytes[3]]) & 0x3fff;
        if len < 4 || len > bytes.len() {
            break;
        }
        out.push((kind, &bytes[4..len]));
        bytes = &bytes[len.next_multiple_of(4).min(bytes.len())..];
    }
    out
}

/// A `u32` or `u16` attribute's value.
#[cfg_attr(
    feature = "fake-desktop",
    allow(
        dead_code,
        reason = "the fake desktop reads its Wi-Fi from the environment"
    )
)]
fn number(payload: &[u8]) -> Option<u32> {
    match *payload {
        [a, b, c, d, ..] => Some(u32::from_ne_bytes([a, b, c, d])),
        [a, b] => Some(u32::from(u16::from_ne_bytes([a, b]))),
        _ => None,
    }
}

/// An nl80211 channel width as MHz.
#[cfg_attr(
    feature = "fake-desktop",
    allow(
        dead_code,
        reason = "the fake desktop reads its Wi-Fi from the environment"
    )
)]
fn width_mhz(width: u32) -> Option<u32> {
    Some(match width {
        0 | 1 => 20,
        2 => 40,
        3 => 80,
        4 | 5 => 160,
        6 => 5,
        7 => 10,
        13 => 320,
        _ => return None,
    })
}

#[cfg(all(target_os = "linux", not(feature = "fake-desktop")))]
mod nl80211 {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use super::{attributes, number, width_mhz};

    const GENL_ID_CTRL: u16 = 16;
    const CTRL_CMD_GETFAMILY: u8 = 3;
    const CTRL_ATTR_FAMILY_ID: u16 = 1;
    const CTRL_ATTR_FAMILY_NAME: u16 = 2;
    const CMD_GET_INTERFACE: u8 = 5;
    const CMD_GET_POWER_SAVE: u8 = 62;
    const ATTR_IFINDEX: u16 = 3;
    const ATTR_WIPHY_FREQ: u16 = 38;
    const ATTR_PS_STATE: u16 = 93;
    const ATTR_CHANNEL_WIDTH: u16 = 159;
    const NLMSG_ERROR: u16 = 2;
    const NLM_F_REQUEST: u16 = 1;

    /// What the card said.
    #[derive(Debug, Default)]
    pub struct Asked {
        pub power_save: Option<bool>,
        pub freq_mhz: Option<u32>,
        pub width_mhz: Option<u32>,
    }

    /// Asks nl80211 about interface `index`.
    pub fn ask(index: u32) -> io::Result<Asked> {
        let socket = Socket::open()?;
        let family = socket.request(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 1, |m| {
            attr(m, CTRL_ATTR_FAMILY_NAME, b"nl80211\0");
        })?;
        let family = attributes(&family)
            .into_iter()
            .find(|(k, _)| *k == CTRL_ATTR_FAMILY_ID)
            .and_then(|(_, v)| number(v))
            .and_then(|v| u16::try_from(v).ok())
            .ok_or_else(|| io::Error::other("no nl80211"))?;
        let index = index.to_ne_bytes();
        let power = socket.request(family, CMD_GET_POWER_SAVE, 0, |m| {
            attr(m, ATTR_IFINDEX, &index);
        })?;
        let mut asked = Asked {
            power_save: attributes(&power)
                .into_iter()
                .find(|(k, _)| *k == ATTR_PS_STATE)
                .and_then(|(_, v)| number(v))
                .map(|v| v != 0),
            ..Asked::default()
        };
        if let Ok(link) = socket.request(family, CMD_GET_INTERFACE, 0, |m| {
            attr(m, ATTR_IFINDEX, &index);
        }) {
            for (kind, value) in attributes(&link) {
                match kind {
                    ATTR_WIPHY_FREQ => asked.freq_mhz = number(value),
                    ATTR_CHANNEL_WIDTH => asked.width_mhz = number(value).and_then(width_mhz),
                    _ => {}
                }
            }
        }
        Ok(asked)
    }

    /// Appends attribute `kind` with `value` to message `m`.
    fn attr(m: &mut Vec<u8>, kind: u16, value: &[u8]) {
        let len = u16::try_from(4 + value.len()).unwrap_or(u16::MAX);
        m.extend_from_slice(&len.to_ne_bytes());
        m.extend_from_slice(&kind.to_ne_bytes());
        m.extend_from_slice(value);
        m.resize(m.len().next_multiple_of(4), 0);
    }

    struct Socket(OwnedFd);

    impl Socket {
        fn open() -> io::Result<Self> {
            // SAFETY: plain socket calls; the descriptor is owned from here on.
            let fd = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                    libc::NETLINK_GENERIC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a descriptor just made, owned by nobody else.
            let socket = Self(unsafe { OwnedFd::from_raw_fd(fd) });
            let timeout = libc::timeval {
                tv_sec: 1,
                tv_usec: 0,
            };
            // SAFETY: a valid socket and a timeval that lives through the call.
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    (&raw const timeout).cast(),
                    size_of::<libc::timeval>() as libc::socklen_t,
                );
            }
            Ok(socket)
        }

        /// Sends one generic netlink request and returns the attributes of its answer.
        fn request(
            &self,
            family: u16,
            cmd: u8,
            version: u8,
            add: impl FnOnce(&mut Vec<u8>),
        ) -> io::Result<Vec<u8>> {
            let mut m = vec![0u8; 20];
            m[4..6].copy_from_slice(&family.to_ne_bytes());
            m[6..8].copy_from_slice(&NLM_F_REQUEST.to_ne_bytes());
            m[8..12].copy_from_slice(&1u32.to_ne_bytes());
            m[16] = cmd;
            m[17] = version;
            add(&mut m);
            let len = u32::try_from(m.len()).unwrap_or(u32::MAX);
            m[..4].copy_from_slice(&len.to_ne_bytes());
            // SAFETY: an all-zero sockaddr_nl is the kernel's address.
            let mut to: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            to.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            // SAFETY: a valid socket, a buffer of `m.len()` bytes and an address of its
            // own size.
            let sent = unsafe {
                libc::sendto(
                    self.0.as_raw_fd(),
                    m.as_ptr().cast(),
                    m.len(),
                    0,
                    (&raw const to).cast(),
                    size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if sent < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut answer = vec![0u8; 8192];
            // SAFETY: a valid socket and a buffer of its own length.
            let got = unsafe {
                libc::recv(
                    self.0.as_raw_fd(),
                    answer.as_mut_ptr().cast(),
                    answer.len(),
                    0,
                )
            };
            let got = usize::try_from(got).map_err(|_| io::Error::last_os_error())?;
            if got < 20 {
                return Err(io::Error::other("a short answer"));
            }
            let len = (u32::from_ne_bytes([answer[0], answer[1], answer[2], answer[3]]) as usize)
                .min(got);
            let kind = u16::from_ne_bytes([answer[4], answer[5]]);
            if kind == NLMSG_ERROR {
                let code = i32::from_ne_bytes([answer[16], answer[17], answer[18], answer[19]]);
                return Err(io::Error::from_raw_os_error(-code));
            }
            if len < 20 {
                return Err(io::Error::other("a broken answer"));
            }
            answer.truncate(len);
            answer.drain(..20);
            Ok(answer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_route_with_the_lowest_metric() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                     wlan0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
                     enp3s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert_eq!(default_route(table).as_deref(), Some("enp3s0"));
        let wifi_only = table.lines().take(3).collect::<Vec<_>>().join("\n");
        assert_eq!(default_route(&wifi_only).as_deref(), Some("wlan0"));
        assert_eq!(default_route(table.lines().next().unwrap()), None);
    }

    #[test]
    fn netlink_attributes() {
        // A u32 (8 bytes), a 3-byte string padded to 8, and a u16 padded to 8.
        let mut bytes = vec![8, 0, 93, 0, 1, 0, 0, 0];
        bytes.extend([7, 0, 4, 0, b'a', b'b', 0, 0]);
        bytes.extend([6, 0, 1, 0, 0x1c, 0, 0, 0]);
        let attrs = attributes(&bytes);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0], (93, &[1, 0, 0, 0][..]));
        assert_eq!(number(attrs[0].1), Some(1));
        assert_eq!(attrs[1], (4, &b"ab\0"[..]));
        assert_eq!(number(attrs[2].1), Some(28));
        // A broken length stops reading.
        assert_eq!(attributes(&[200, 0, 1, 0]).len(), 0);
        assert_eq!(width_mhz(3), Some(80));
        assert_eq!(width_mhz(99), None);
    }

    /// This PC's Wi-Fi, as the session's log line says it: `cargo test -p prostir --
    /// --ignored wifi_of_this_pc --nocapture`.
    #[test]
    #[ignore = "by hand: reads this PC's network"]
    fn wifi_of_this_pc() {
        eprintln!("{:?}", look());
    }
}
