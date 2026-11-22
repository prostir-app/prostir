//! `adb reverse` for a headset on USB, so it reaches the PC through 127.0.0.1, with
//! nothing running while no headset is plugged in.
//!
//! - The kernel says when a USB device comes or goes (its uevents, over netlink: a
//!   thread sleeps on the socket). Where udev runs, its events are read instead, with a
//!   socket filter that lets only USB devices' in, so a laptop's sensors and the rest
//!   never wake the thread (the sockets' watcher sleeps with it). Only Meta's devices
//!   (vendor 2833) start anything; whether one is plugged in is read from
//!   `/sys/bus/usb/devices`.
//! - While one is, `adb track-devices` runs: the adb server tells it every change of a
//!   device's state, so a headset whose owner allows USB debugging minutes later (from
//!   `unauthorized` to `device`) gets its `adb reverse` then, with nothing asked again
//!   and again. It ends when the adb server ends (another tool restarted it, say): it is
//!   started again, and the reverse set up again on the new server.
//! - An `adb reverse` that fails is tried again after 5, 10, 20… up to 60 s while the
//!   device stays ready.
//! - A program that leaves when idle waits for a headset without its `adb reverse`
//!   only for a minute after it came or its state changed (its owner allows USB
//!   debugging then, or not today): then it may leave, and the retries rest until the
//!   next event of a device.
//! - Without the kernel's events (no netlink), `/sys` is looked at every 10 s instead.
//!   Without adb, nothing runs at all.

use std::collections::BTreeSet;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use log::{info, warn};

use super::system::run;

/// Meta's USB vendor id (the Quest's), as the kernel writes it.
const META: &str = "2833";

/// A Quest is plugged in and has no `adb reverse` yet (USB debugging not allowed yet,
/// say), and came or changed its state at most [`PATIENCE`] ago: the program it
/// started waits for it rather than leave.
static WAITING: AtomicBool = AtomicBool::new(false);

/// How long a Quest plugged in without its `adb reverse` is waited for, after it came
/// or its state changed: its owner allows USB debugging in that time, or the program
/// may leave (the next event of the device has it wait again).
const PATIENCE: Duration = Duration::from_secs(60);

/// Whether a Quest on USB waits for its `adb reverse` now.
pub fn usb_waiting() -> bool {
    WAITING.load(Ordering::Relaxed)
}

/// The kernel's own uevents (udev passes them on in group 2, which needs udev).
const KERNEL_EVENTS: u32 = 1;

/// Without the kernel's events, how often `/sys` is looked at.
const POLL_WITHOUT_EVENTS: Duration = Duration::from_secs(10);

/// The longest wait before a failed `adb reverse` is tried again, and before a
/// `track-devices` that ended is started again.
const LONGEST_RETRY: Duration = Duration::from_secs(60);
const LONGEST_RESTART: Duration = Duration::from_secs(30);

/// What the adb thread hears.
enum Note {
    /// A USB device came or went.
    Usb,
    /// The devices `track-devices` number `n` lists now: serial and state.
    Devices(u64, Vec<(String, String)>),
    /// `track-devices` number `n` ended.
    TrackEnded(u64),
    /// Stop (tests only).
    #[cfg(test)]
    Stop,
}

/// Whether a uevent is about a USB device that matters: one of Meta's plugged in or
/// ready (a driver bound, its configuration changed), or any that went (to look again
/// whether a Meta one is left). Its interfaces' events say nothing more.
fn usb_event_matters(event: &[u8]) -> bool {
    let (mut action, mut subsystem, mut devtype, mut product) = (None, None, None, None);
    // udev's: a header of its own, then KEY=value fields where it says. The kernel's:
    // a header (`add@/devices/…`) first, then KEY=value fields.
    let (fields, header) = match udev_properties(event) {
        Some(properties) => (properties, 0),
        None => (event, 1),
    };
    for field in fields.split(|b| *b == 0).skip(header) {
        if let Some(v) = field.strip_prefix(b"ACTION=") {
            action = Some(v);
        } else if let Some(v) = field.strip_prefix(b"SUBSYSTEM=") {
            subsystem = Some(v);
        } else if let Some(v) = field.strip_prefix(b"DEVTYPE=") {
            devtype = Some(v);
        } else if let Some(v) = field.strip_prefix(b"PRODUCT=") {
            product = Some(v);
        }
    }
    // PRODUCT is vendor/product/version in hex without leading zeros: 2833/186/ffff.
    let meta = product.is_some_and(|p| p.split(|b| *b == b'/').next() == Some(META.as_bytes()));
    subsystem == Some(b"usb")
        && devtype == Some(b"usb_device")
        && match action {
            Some(b"add" | b"bind" | b"change") => meta,
            Some(b"remove") => true,
            _ => false,
        }
}

/// The events udev passes on (after its rules ran), in their own format.
const UDEV_EVENTS: u32 = 2;
/// The start of udev's header, and its magic (big-endian at offset 8).
const UDEV_PREFIX: &[u8; 8] = b"libudev\0";
const UDEV_MAGIC: u32 = 0xfeed_cafe;

/// MurmurHash2 with seed 0, as udev hashes the subsystem and device type into the
/// header of each event, so a socket filter can pick events by them.
const fn murmur2(text: &[u8]) -> u32 {
    const M: u32 = 0x5bd1_e995;
    let mut h = text.len() as u32;
    let mut i = 0;
    while i + 4 <= text.len() {
        let mut k = u32::from_le_bytes([text[i], text[i + 1], text[i + 2], text[i + 3]]);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
        i += 4;
    }
    let rest = text.len() - i;
    if rest == 3 {
        h ^= (text[i + 2] as u32) << 16;
    }
    if rest >= 2 {
        h ^= (text[i + 1] as u32) << 8;
    }
    if rest >= 1 {
        h ^= text[i] as u32;
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

/// The KEY=value fields of an event in udev's format; `None` for any other.
fn udev_properties(event: &[u8]) -> Option<&[u8]> {
    if !event.starts_with(UDEV_PREFIX) {
        return None;
    }
    let at = |o: usize| -> Option<usize> {
        Some(u32::from_ne_bytes(event.get(o..o + 4)?.try_into().ok()?) as usize)
    };
    let (off, len) = (at(16)?, at(20)?);
    event.get(off..off.checked_add(len)?)
}

/// Lets only udev's events of USB devices (whole devices, not their interfaces) into
/// `socket`: the subsystem's and device type's hashes in udev's header, as libudev
/// filters. Nothing else wakes the thread that reads it (a laptop's sensors send
/// events every few seconds).
fn filter_usb_devices(socket: &OwnedFd) -> std::io::Result<()> {
    let op = |code: u32, jt: u8, jf: u8, k: u32| libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    };
    use libc::{BPF_ABS, BPF_JEQ, BPF_JMP, BPF_K, BPF_LD, BPF_RET, BPF_W};
    let program = [
        op(BPF_LD | BPF_W | BPF_ABS, 0, 0, 8),
        op(BPF_JMP | BPF_JEQ | BPF_K, 0, 5, UDEV_MAGIC),
        op(BPF_LD | BPF_W | BPF_ABS, 0, 0, 24),
        op(BPF_JMP | BPF_JEQ | BPF_K, 0, 3, murmur2(b"usb")),
        op(BPF_LD | BPF_W | BPF_ABS, 0, 0, 28),
        op(BPF_JMP | BPF_JEQ | BPF_K, 0, 1, murmur2(b"usb_device")),
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
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Sends [`Note::Usb`] on a thread of its own each time a USB device that matters
/// comes or goes: udev's events of USB devices where udev runs (filtered in the
/// kernel), else every event of the kernel. An error when the events cannot be read.
fn follow_usb(notes: Sender<Note>) -> std::io::Result<()> {
    // SAFETY: a plain socket call; the descriptor is owned right below.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a new descriptor nothing else owns.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    // udev runs where its control socket is: its events, filtered, wake the thread only
    // for USB devices.
    let udev = Path::new("/run/udev/control").exists() && filter_usb_devices(&socket).is_ok();
    // SAFETY: an all-zero sockaddr_nl is valid; its family and group are set below.
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    addr.nl_groups = if udev { UDEV_EVENTS } else { KERNEL_EVENTS };
    // SAFETY: binds the socket to an address of the size given.
    let bound = unsafe {
        libc::bind(
            socket.as_raw_fd(),
            (&raw const addr).cast(),
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if bound != 0 {
        return Err(std::io::Error::last_os_error());
    }
    std::thread::Builder::new()
        .name("usb".into())
        .spawn(move || {
            let mut buf = vec![0u8; 8192];
            loop {
                // SAFETY: receives into a buffer of the length given.
                let n = unsafe {
                    libc::recv(socket.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0)
                };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    match e.raw_os_error() {
                        Some(libc::EINTR) => continue,
                        // Events were lost while the queue was full: one may matter.
                        Some(libc::ENOBUFS) => {}
                        _ => {
                            warn!("USB devices are no longer followed: {e}");
                            return;
                        }
                    }
                } else if !usb_event_matters(&buf[..n as usize]) {
                    continue;
                }
                if notes.send(Note::Usb).is_err() {
                    return;
                }
            }
        })?;
    Ok(())
}

/// Whether one of Meta's devices is plugged in, as `/sys` lists them (`root`).
fn meta_plugged_in(root: &Path) -> bool {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .any(|d| std::fs::read_to_string(d.path().join("idVendor")).is_ok_and(|v| v.trim() == META))
}

/// Reads what `adb track-devices` writes: each change a list, four hex digits of
/// length then `serial\tstate` lines. Calls `on_list` with each list, and returns at
/// its end.
fn read_lists(mut out: impl Read, mut on_list: impl FnMut(Vec<(String, String)>) -> bool) {
    let mut len = [0u8; 4];
    loop {
        if out.read_exact(&mut len).is_err() {
            return;
        }
        let Some(n) = std::str::from_utf8(&len)
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
        else {
            return;
        };
        let mut text = vec![0u8; n];
        if out.read_exact(&mut text).is_err() {
            return;
        }
        let list = String::from_utf8_lossy(&text)
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(serial, state)| (serial.to_owned(), state.trim().to_owned()))
            .collect();
        if !on_list(list) {
            return;
        }
    }
}

/// Where to find adb and the devices, and the port to reverse.
struct Follow {
    adb: PathBuf,
    /// `/sys/bus/usb/devices`, or a test's.
    devices: PathBuf,
    spec: String,
    /// Whether the kernel's events come (otherwise `/sys` is looked at now and then).
    events: bool,
    /// How long after a headset came, or changed its state, it is waited for.
    patience: Duration,
    /// Where it says whether one is waited for now ([`WAITING`], or a test's).
    waiting: &'static AtomicBool,
}

/// `adb track-devices` running, number `n`.
struct Tracker {
    n: u64,
    child: Child,
}

impl Drop for Tracker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The time to wait after `wait`, doubled up to `longest`.
fn longer(wait: Duration, longest: Duration) -> Duration {
    (wait * 2).min(longest)
}

impl Follow {
    /// Starts `adb track-devices`, its lists read on a thread of its own; an error of
    /// kind `NotFound` when adb is not installed.
    fn track(&self, n: u64, notes: &Sender<Note>) -> std::io::Result<Tracker> {
        let mut child = Command::new(&self.adb)
            .arg("track-devices")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let out = child.stdout.take().expect("piped above");
        let notes = notes.clone();
        let tracker = Tracker { n, child };
        std::thread::Builder::new()
            .name("adb-devices".into())
            .spawn(move || {
                read_lists(out, |list| notes.send(Note::Devices(n, list)).is_ok());
                let _ = notes.send(Note::TrackEnded(n));
            })?;
        Ok(tracker)
    }

    /// Sets `adb reverse` up for `serial`.
    fn reverse(&self, serial: &str) -> bool {
        run(
            Command::new(&self.adb).args(["-s", serial, "reverse", &self.spec, &self.spec]),
            Duration::from_secs(5),
        )
        .is_ok_and(|out| out.status.success())
    }

    /// Follows the headsets on USB until told to stop (never, but in tests).
    fn run(&self, notes: &Receiver<Note>, sender: &Sender<Note>) {
        let mut tracker: Option<Tracker> = None;
        let mut last = 0;
        // Ready devices (`device`) from the last list, and those reversed.
        let mut ready: Vec<String> = Vec::new();
        let mut reversed: BTreeSet<String> = BTreeSet::new();
        let mut retry: Option<(Instant, Duration)> = None;
        let mut restart: Option<(Instant, Duration)> = None;
        // Until when a headset that came, or changed its state, is waited for; the
        // last list of devices, to see a change.
        let mut patient_until: Option<Instant> = None;
        let mut was_plugged = false;
        let mut last_list: Vec<(String, String)> = Vec::new();
        loop {
            let now = Instant::now();
            let plugged = meta_plugged_in(&self.devices);
            if plugged && !was_plugged {
                patient_until = Some(now + self.patience);
            }
            was_plugged = plugged;
            if !plugged && tracker.is_some() {
                info!("no headset on USB any more: adb is left alone");
                (tracker, retry, restart) = (None, None, None);
                ready.clear();
                reversed.clear();
            }
            let may_start = restart.is_none_or(|(at, _)| now >= at);
            if plugged && tracker.is_none() && may_start {
                last += 1;
                match self.track(last, sender) {
                    Ok(t) => tracker = Some(t),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        info!("a headset is on USB, but adb is not installed: no adb reverse");
                        self.waiting.store(false, Ordering::Relaxed);
                        return;
                    }
                    Err(e) => {
                        warn!("cannot follow the devices adb sees: {e}");
                        let wait = restart
                            .map_or(Duration::from_secs(1), |(_, w)| longer(w, LONGEST_RESTART));
                        restart = Some((now + wait, wait));
                    }
                }
            }
            if tracker.is_some() && retry.is_none_or(|(at, _)| now >= at) {
                let mut failed = false;
                let waiting: Vec<String> = ready
                    .iter()
                    .filter(|s| !reversed.contains(*s))
                    .cloned()
                    .collect();
                for serial in &waiting {
                    if self.reverse(serial) {
                        info!("USB headset {serial}: adb reverse {} active", self.spec);
                        reversed.insert(serial.clone());
                    } else {
                        failed = true;
                    }
                }
                retry = failed.then(|| {
                    let wait =
                        retry.map_or(Duration::from_secs(5), |(_, w)| longer(w, LONGEST_RETRY));
                    (Instant::now() + wait, wait)
                });
            }
            let patient = patient_until.is_some_and(|until| Instant::now() < until);
            self.waiting
                .store(plugged && reversed.is_empty() && patient, Ordering::Relaxed);
            let deadline = [
                // The retries rest once nothing is waited for, until the next event.
                retry.filter(|_| patient).map(|(at, _)| at),
                restart.filter(|_| plugged).map(|(at, _)| at),
                patient_until.filter(|_| patient && plugged),
                (!self.events).then(|| Instant::now() + POLL_WITHOUT_EVENTS),
            ]
            .into_iter()
            .flatten()
            .min();
            let note = match deadline {
                None => notes.recv().ok(),
                Some(at) => notes
                    .recv_timeout(at.saturating_duration_since(Instant::now()))
                    .ok(),
            };
            match note {
                Some(Note::Devices(n, list)) if tracker.as_ref().is_some_and(|t| t.n == n) => {
                    if list != last_list {
                        patient_until = Some(Instant::now() + self.patience);
                    }
                    ready.clear();
                    ready.extend(
                        list.iter()
                            .filter(|(serial, state)| state == "device" && !serial.contains(':'))
                            .map(|(serial, _)| serial.clone()),
                    );
                    last_list = list;
                    // Gone or no longer ready: set up again when it is back.
                    reversed.retain(|s| ready.contains(s));
                    retry = None;
                    // The server answers: a later end waits from the start again.
                    restart = None;
                }
                Some(Note::TrackEnded(n)) if tracker.as_ref().is_some_and(|t| t.n == n) => {
                    // The adb server ended (restarted by another tool, say): its
                    // reverses went with it.
                    info!("the adb server ended: following the devices again");
                    tracker = None;
                    ready.clear();
                    reversed.clear();
                    let wait =
                        restart.map_or(Duration::from_secs(1), |(_, w)| longer(w, LONGEST_RESTART));
                    restart = Some((Instant::now() + wait, wait));
                }
                // A device came, went or changed: waited for again.
                Some(Note::Usb) => patient_until = Some(Instant::now() + self.patience),
                #[cfg(test)]
                Some(Note::Stop) => return,
                // A timer, or news of an earlier tracker.
                _ => {}
            }
        }
    }
}

/// Keeps `adb reverse tcp:PORT` set up for every Meta headset on USB, on a thread of
/// its own (see the top of this file).
pub fn keep_adb_reverse(port: u16) {
    let (sender, notes) = mpsc::channel();
    // Before anything else: a headset plugged in from now on is not missed.
    let events = match follow_usb(sender.clone()) {
        Ok(()) => true,
        Err(e) => {
            warn!(
                "the kernel's USB events cannot be read ({e}): looking for a headset every {} s",
                POLL_WITHOUT_EVENTS.as_secs()
            );
            false
        }
    };
    let follow = Follow {
        adb: "adb".into(),
        devices: "/sys/bus/usb/devices".into(),
        spec: format!("tcp:{port}"),
        events,
        patience: PATIENCE,
        waiting: &WAITING,
    };
    let spawned = std::thread::Builder::new()
        .name("adb-reverse".into())
        .spawn(move || follow.run(&notes, &sender));
    if let Err(e) = spawned {
        warn!("no adb reverse for a headset on USB: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(fields: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in fields {
            out.extend_from_slice(f.as_bytes());
            out.push(0);
        }
        out
    }

    /// udev's hash of the subsystem, as its events carry it (`hwmon` as seen in a
    /// real event of this PC's sensors).
    #[test]
    fn subsystems_hash_as_udev_hashes_them() {
        assert_eq!(murmur2(b"hwmon"), 0x22df_0017);
        assert_eq!(murmur2(b"usb"), 0x0577_c5e5);
        assert_eq!(murmur2(b"usb_device"), 0x27f8_f50c);
        assert_eq!(murmur2(b""), 0);
    }

    /// An event in udev's format: its fields where its header says.
    #[test]
    fn udev_events_are_read_by_their_header() {
        let fields = event(&[
            "ACTION=add",
            "SUBSYSTEM=usb",
            "DEVTYPE=usb_device",
            "PRODUCT=2833/186/ffff",
        ]);
        let mut udev = UDEV_PREFIX.to_vec();
        udev.extend_from_slice(&UDEV_MAGIC.to_be_bytes());
        for n in [40u32, 40, fields.len() as u32] {
            udev.extend_from_slice(&n.to_ne_bytes());
        }
        udev.extend_from_slice(&murmur2(b"usb").to_be_bytes());
        udev.extend_from_slice(&murmur2(b"usb_device").to_be_bytes());
        udev.extend_from_slice(&[0; 8]);
        assert_eq!(udev.len(), 40);
        udev.extend_from_slice(&fields);
        assert!(usb_event_matters(&udev));
        // Cut short: nothing read.
        assert!(!usb_event_matters(&udev[..45]));
    }

    #[test]
    fn only_a_meta_device_coming_or_any_going_matters() {
        let quest = event(&[
            "add@/devices/pci0000:00/0000:00:14.0/usb1/1-2",
            "ACTION=add",
            "DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2",
            "SUBSYSTEM=usb",
            "DEVNAME=bus/usb/001/005",
            "DEVTYPE=usb_device",
            "PRODUCT=2833/186/ffff",
            "SEQNUM=4510",
        ]);
        assert!(usb_event_matters(&quest));
        let bound = event(&[
            "bind@/x",
            "ACTION=bind",
            "SUBSYSTEM=usb",
            "DEVTYPE=usb_device",
            "PRODUCT=2833/183/1",
        ]);
        assert!(usb_event_matters(&bound));
        let mouse = event(&[
            "add@/x",
            "ACTION=add",
            "SUBSYSTEM=usb",
            "DEVTYPE=usb_device",
            "PRODUCT=46d/c52b/1211",
        ]);
        assert!(!usb_event_matters(&mouse), "not Meta's");
        let removed = event(&[
            "remove@/x",
            "ACTION=remove",
            "SUBSYSTEM=usb",
            "DEVTYPE=usb_device",
            "PRODUCT=46d/c52b/1211",
        ]);
        assert!(usb_event_matters(&removed), "whatever went: look again");
        let interface = event(&[
            "add@/x/1-2:1.0",
            "ACTION=add",
            "SUBSYSTEM=usb",
            "DEVTYPE=usb_interface",
            "PRODUCT=2833/186/ffff",
        ]);
        assert!(!usb_event_matters(&interface));
        let block = event(&["add@/x", "ACTION=add", "SUBSYSTEM=block", "DEVTYPE=disk"]);
        assert!(!usb_event_matters(&block));
        assert!(!usb_event_matters(b"add@/devices/usb1/1-2"));
        assert!(!usb_event_matters(&[0xff, 0, 0xfe]));
    }

    #[test]
    fn the_lists_of_track_devices_are_read() {
        let one = "R3Q\tunauthorized\n";
        let two = "R3Q\tdevice\n192.168.1.5:5555\tdevice\n";
        let text = format!("0000{:04x}{one}{:04x}{two}", one.len(), two.len());
        let mut lists = Vec::new();
        read_lists(text.as_bytes(), |l| {
            lists.push(l);
            true
        });
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            lists,
            [
                vec![],
                vec![pair("R3Q", "unauthorized")],
                vec![pair("R3Q", "device"), pair("192.168.1.5:5555", "device")],
            ]
        );
        // A cut-off list ends the reading.
        let mut n = 0;
        read_lists(&b"0010R3Q"[..], |_| {
            n += 1;
            true
        });
        assert_eq!(n, 0);
    }

    /// A fake adb: `track-devices` says what `track-N` holds (N counting the starts)
    /// and stays as long as the file says, `reverse` is written to `reversed`.
    fn fake_adb(dir: &Path) -> PathBuf {
        let adb = dir.join("adb");
        let script = format!(
            r#"#!/bin/sh
D={dir}
if [ "$1" = track-devices ]; then
    n=$(($(cat $D/starts 2>/dev/null || echo 0) + 1)); echo $n > $D/starts
    exec sh $D/track-$n
fi
if [ "$3" = reverse ]; then echo "$2" >> $D/reversed; exit 0; fi
exit 1
"#,
            dir = dir.display()
        );
        crate::platform::process::write_program(&adb, &script);
        adb
    }

    /// A list as `track-devices` prints it, for a shell `printf`.
    fn list(lines: &str) -> String {
        // The text goes in the format, where printf reads `\t` and `\n`.
        format!(
            "printf '%04x' {}; printf '{}'",
            lines.len(),
            lines.replace('\t', "\\t").replace('\n', "\\n")
        )
    }

    fn wait_for(what: &str, until: Duration, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < until, "{what} within {until:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A Meta headset not allowed yet, then allowed: reversed once it is ready, with
    /// nothing asked meanwhile. The adb server restarted by another tool: reversed
    /// again on the new one. Unplugged: adb is left alone, no `track-devices` runs.
    #[test]
    fn a_headset_is_reversed_when_ready_and_again_after_an_adb_restart() {
        let dir = std::env::temp_dir().join(format!("prostir-adb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let usb = dir.join("usb");
        std::fs::create_dir_all(usb.join("1-2")).unwrap();
        std::fs::write(usb.join("1-2/idVendor"), "2833\n").unwrap();
        // The first server: unauthorized, allowed a moment later, then it ends.
        std::fs::write(
            dir.join("track-1"),
            format!(
                "{}; sleep 0.3; {}; sleep 0.5\n",
                list("R3Q\tunauthorized\n"),
                list("R3Q\tdevice\n")
            ),
        )
        .unwrap();
        // The second one lists it ready at once and stays.
        std::fs::write(
            dir.join("track-2"),
            format!("{}; exec sleep 30\n", list("R3Q\tdevice\n")),
        )
        .unwrap();
        static WAITING: AtomicBool = AtomicBool::new(false);
        let waiting = || WAITING.load(Ordering::Relaxed);
        let follow = Follow {
            adb: fake_adb(&dir),
            devices: usb.clone(),
            spec: "tcp:47830".into(),
            events: true,
            patience: PATIENCE,
            waiting: &WAITING,
        };
        let (sender, notes) = mpsc::channel();
        let stopper = sender.clone();
        let reversed = || {
            std::fs::read_to_string(dir.join("reversed"))
                .unwrap_or_default()
                .lines()
                .count()
        };
        let starts = || {
            std::fs::read_to_string(dir.join("starts"))
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0)
        };
        /// Stops the follower however the test ends, so a failure does not hang it.
        struct Stop(Sender<Note>);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = self.0.send(Note::Stop);
            }
        }
        std::thread::scope(|s| {
            s.spawn(move || follow.run(&notes, &sender));
            let _stop = Stop(stopper.clone());
            // Plugged in, not allowed yet: it waits (the program it started stays).
            wait_for("waiting for the reverse", Duration::from_secs(5), waiting);
            wait_for("the reverse once ready", Duration::from_secs(5), || {
                reversed() == 1
            });
            wait_for("done waiting", Duration::from_secs(5), || !waiting());
            assert_eq!(starts(), 1);
            wait_for(
                "the reverse on the new server",
                Duration::from_secs(5),
                || reversed() == 2,
            );
            assert_eq!(starts(), 2);
            // Unplugged: the tracker goes, and nothing starts again.
            std::fs::remove_dir_all(usb.join("1-2")).unwrap();
            stopper.send(Note::Usb).unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            assert_eq!(starts(), 2);
            assert_eq!(reversed(), 2);
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A Meta headset whose owner does not allow USB debugging: waited for a while
    /// after it came, then the program may leave; a change of its state has it waited
    /// for again, as long.
    #[test]
    fn a_headset_not_allowed_is_waited_for_a_while_only() {
        let dir = std::env::temp_dir().join(format!("prostir-adb-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let usb = dir.join("usb");
        std::fs::create_dir_all(usb.join("1-2")).unwrap();
        std::fs::write(usb.join("1-2/idVendor"), "2833\n").unwrap();
        std::fs::write(
            dir.join("track-1"),
            format!(
                "{}; sleep 1.5; {}; exec sleep 30\n",
                list("R3Q\tunauthorized\n"),
                list("R3Q\toffline\n")
            ),
        )
        .unwrap();
        static WAITING: AtomicBool = AtomicBool::new(false);
        let waiting = || WAITING.load(Ordering::Relaxed);
        let follow = Follow {
            adb: fake_adb(&dir),
            devices: usb,
            spec: "tcp:47830".into(),
            events: true,
            patience: Duration::from_millis(400),
            waiting: &WAITING,
        };
        let (sender, notes) = mpsc::channel();
        let stopper = sender.clone();
        /// Stops the follower however the test ends, so a failure does not hang it.
        struct Stop(Sender<Note>);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = self.0.send(Note::Stop);
            }
        }
        std::thread::scope(|s| {
            s.spawn(move || follow.run(&notes, &sender));
            let _stop = Stop(stopper);
            wait_for("waiting once plugged in", Duration::from_secs(5), waiting);
            wait_for("waiting no more", Duration::from_secs(5), || !waiting());
            wait_for(
                "waiting again after a change",
                Duration::from_secs(5),
                waiting,
            );
            wait_for("waiting no more again", Duration::from_secs(5), || {
                !waiting()
            });
        });
        assert!(!dir.join("reversed").exists(), "nothing reversed");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
