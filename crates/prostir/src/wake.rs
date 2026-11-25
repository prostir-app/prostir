//! Asleep until a headset comes: nothing of the full program runs while no headset
//! needs it, and the sockets wait with whoever holds them.
//!
//! - With systemd, `prostir.socket` holds them and starts `prostir.service` when
//!   something comes. The program then sorts first ([`sort`], the light phase): it
//!   answers discovery queries, and starts the rest (GStreamer, the tray, the local
//!   API) only for a connection that says something within a few seconds, a query
//!   from one of its own headsets, a press of the button of the notice of a new
//!   headset, or a start by hand (nothing waiting). Otherwise it stays light, still
//!   answering, until nothing came for [`SORT_LINGER`], then leaves: a stranger's
//!   queries wake nothing heavy, and a stream of them starts the service once a
//!   minute at most, far from systemd's limit on starts.
//! - Without systemd, the sockets' watcher ([`watch`], `prostir serve` itself) holds
//!   them: it sleeps in the kernel on the sockets, its signals and a notice, answers
//!   queries the same way, and starts the full program as a child process with the
//!   sockets passed in systemd's way. While the child runs, the watcher does not
//!   touch the sockets; when it ends, it goes back to sleep. A child that crashed (or
//!   could not be started) is started again only for something new, after a pause of
//!   2, 4, 8... up to 60 s.
//!
//! The full program leaves once nothing needs it (see `crate::door`): with code 0
//! back to sleep, with [`QUIT_ALL`] when the person asked Prostir to quit (the
//! watcher then quits too).

use std::net::TcpStream;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use log::{debug, info, warn};

use crate::door;
use crate::find::{self, Responder};
use crate::i18n::t;
use crate::platform::dialogs::{self, Notice};
use crate::platform::signals;
use crate::platform::sockets::{self, Sockets};

/// The full program's exit code when the person asked Prostir to quit: the watcher
/// quits too, instead of waiting for the headset.
pub const QUIT_ALL: i32 = 10;

/// The longest pause before a full program that crashed is started again.
const LONGEST_PAUSE: Duration = Duration::from_secs(60);
/// A full program that ran this long before it crashed counts as having worked: the
/// pauses start from the shortest again.
const RAN_WELL: Duration = Duration::from_secs(60);
/// How long the watcher waits for the full program to quit before it ends it.
const QUIT_WAIT: Duration = Duration::from_secs(10);
/// How long the light phase stays after the last thing that came, answering.
pub const SORT_LINGER: Duration = Duration::from_secs(60);
/// How long a connection has to say something before the light phase lets it go.
const FIRST_WORDS: Duration = Duration::from_secs(3);

/// [`SORT_LINGER`] in this run, or in a build for the end-to-end tests what
/// `PROSTIR_TEST_QUIET_SECONDS` says.
fn sort_linger() -> Duration {
    #[cfg(feature = "fake-desktop")]
    if let Some(s) = std::env::var("PROSTIR_TEST_QUIET_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return Duration::from_secs(s);
    }
    SORT_LINGER
}

/// Why the full program starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A connection is waiting.
    Connection,
    /// One of the PC's own headsets asked for it: it connects in a moment.
    Headset,
    /// Started by hand (`prostir`, `systemctl start`): nothing was waiting.
    Started,
    /// `prostir` asked the watcher for the settings window (SIGUSR1).
    Window,
    /// The button of the notice of a new headset: the pairing window opens.
    Pair,
}

/// The notice of a new headset nearby, with its button (see `crate::door`).
pub fn new_headset_notice() -> Option<Notice> {
    dialogs::notice(
        t("pc-wake-new-headset-nearby"),
        t("pc-wake-pair-it"),
        t("pc-wake-pair"),
        t("pc-wake-to-pair-it-choose-pair-a"),
    )
}

/// What answers queries while the full program does not run: its port and name, the
/// files of the headsets it trusts and of its find secrets (read for each query that
/// reads as one: the full program pairs and forgets headsets), and the notice of a new
/// headset. It keeps nothing of who asked: a stream of queries from made-up addresses
/// costs it nothing but what [`Responder`] lets through. A query from one of the PC's
/// own headsets (by the current find secret, or an older one still owed to it) starts
/// the full program: this is how a sleeping PC wakes for its headset.
struct Sorter {
    responder: Responder,
    trust: PathBuf,
    /// The PC's find secret's file (the full program makes a new one when it forgets a
    /// headset), and the older ones still owed next to it.
    secret: PathBuf,
    /// Who may get an answer, as the files said lately: read again after
    /// [`OPEN_KEPT_FOR`], so a stream of queries does not read the files for each.
    open: Option<(Instant, find::Open)>,
    notice: Option<(Notice, Instant)>,
}

/// How long the watcher goes by what the files said (the full program, which changes
/// them, does not run meanwhile; when it ends, they are read again).
const OPEN_KEPT_FOR: Duration = Duration::from_secs(1);

impl Sorter {
    fn new(port: u16) -> Result<Self> {
        let dir = crate::pairing::config_dir();
        let secret = crate::pairing::find_secret_file(&dir);
        // Made here on the first start, as the full program would.
        crate::pairing::load_find_secret(&secret)?;
        Ok(Self {
            responder: Responder::new(port, crate::platform::hostname()),
            trust: dir.join("paired_headsets"),
            secret,
            open: None,
            notice: None,
        })
    }

    /// Who may get an answer now. An unreadable file closes the door rather than
    /// opening it: everyone answered would be the wrong way to fail.
    fn open(trust: &std::path::Path, secret: &std::path::Path) -> find::Open {
        let trusted = prostir_proto::secure::TrustStore::try_load(trust);
        let secrets = crate::pairing::load_find_secrets(secret);
        match (trusted, secrets) {
            (Ok(trusted), Ok(mut secrets)) => {
                secrets.owed.retain(|key, _| trusted.contains(key));
                find::Open {
                    paired: !trusted.is_empty(),
                    pairing: false,
                    secrets: secrets.valid(),
                }
            }
            (trusted, secrets) => {
                if let Err(e) = &trusted {
                    warn!("discovery: {e}");
                }
                if let Err(e) = &secrets {
                    warn!("discovery: {e:#}");
                }
                find::Open {
                    paired: true,
                    pairing: false,
                    secrets: find::Secrets::new(
                        secrets
                            .map_or_else(|_| prostir_proto::discovery::new_secret(), |s| s.current),
                    ),
                }
            }
        }
    }

    /// Answers the datagram waiting on `udp`: [`Wake::Headset`] when one of the PC's
    /// own headsets asked. A stranger it does not answer brings the notice, once a
    /// minute at most however many programs ask (see `door::notice_due`).
    fn query(&mut self, udp: &std::net::UdpSocket) -> Option<Wake> {
        let (trust, secret, kept) = (&self.trust, &self.secret, &mut self.open);
        let open = || {
            let now = Instant::now();
            match *kept {
                Some((at, open)) if now.saturating_duration_since(at) < OPEN_KEPT_FOR => open,
                _ => {
                    let open = Self::open(trust, secret);
                    *kept = Some((now, open));
                    open
                }
            }
        };
        let asker = match find::answer_one(udp, &mut self.responder, open) {
            Ok(Some(asker)) => asker,
            Ok(None) => return None,
            Err(e) => {
                warn!("discovery: {e}");
                return None;
            }
        };
        // Answered: a repeat of a query answered lately starts nothing.
        if asker.ours {
            if !asker.answered {
                return None;
            }
            info!("one of this PC's headsets asked for it at {}", asker.from);
            return Some(Wake::Headset);
        }
        let wants = door::wants_notice(&asker, false);
        if wants
            && self.notice.is_none()
            && door::notice_due(&crate::platform::notice_mark(), SystemTime::now())
        {
            info!(
                "a new headset at {} asked: telling the person at the PC",
                asker.from
            );
            self.notice = new_headset_notice().map(|n| (n, Instant::now() + door::NOTICE_TIME));
        }
        None
    }

    /// The notice's descriptor, while one is up.
    fn notice_fd(&self) -> Option<RawFd> {
        self.notice.as_ref().map(|(n, _)| n.fd())
    }

    /// The notice's descriptor is readable: [`Wake::Pair`] for its button.
    fn notice_said(&mut self) -> Option<Wake> {
        let (notice, _) = self.notice.as_mut()?;
        match notice.check()? {
            true => {
                self.notice = None;
                Some(Wake::Pair)
            }
            false => {
                self.notice = None;
                None
            }
        }
    }

    /// At `now`: the notice given up on at its time. When that is.
    fn notice_until(&mut self, now: Instant) -> Option<Instant> {
        let until = self.notice.as_ref()?.1;
        if now >= until {
            self.notice = None;
            return None;
        }
        Some(until)
    }
}

/// What the light phase decided: why the full program starts, and the connection that
/// woke it, if one did (accepted already; it goes to the full program first).
pub struct Sorted {
    pub why: Wake,
    pub first: Option<TcpStream>,
}

/// A connection the light phase accepted: kept when it says something within
/// [`FIRST_WORDS`] (a headset starts its handshake at once), else let go.
fn says_something(stream: &TcpStream) -> bool {
    match sockets::readable(&[stream.as_raw_fd()], Some(FIRST_WORDS)) {
        Ok(ready) if ready[0] => {
            let mut byte = [0u8; 1];
            // Something to read, not the end: a peer that only closed says nothing.
            matches!(stream.peek(&mut byte), Ok(1))
        }
        _ => false,
    }
}

/// The light phase under systemd: answers what is waiting on `sockets`, and says
/// whether the full program is to start (and why). `None`: nothing came for it within
/// [`SORT_LINGER`] of the last thing, the program leaves.
pub fn sort(sockets: &Sockets) -> Result<Option<Sorted>> {
    let mut sorter = Sorter::new(sockets.port())?;
    let tcp = sockets.tcp.as_raw_fd();
    let linger = sort_linger();
    let mut first = true;
    let mut last = Instant::now();
    let started = |why| Ok(Some(Sorted { why, first: None }));
    loop {
        let now = Instant::now();
        let until = sorter
            .notice_until(now)
            .map_or(last + linger, |n| n.max(last + linger));
        let wait = if first {
            Duration::ZERO
        } else {
            until.saturating_duration_since(now)
        };
        let mut fds = vec![tcp];
        fds.extend(sockets.udp.as_ref().map(AsRawFd::as_raw_fd));
        fds.extend(sorter.notice_fd());
        let ready = sockets::readable(&fds, Some(wait))?;
        let at = |fd: RawFd| fds.iter().position(|f| *f == fd).is_some_and(|i| ready[i]);
        if !ready.contains(&true) {
            if first {
                return started(Wake::Started);
            }
            if Instant::now() >= until {
                return Ok(None);
            }
            continue;
        }
        first = false;
        last = Instant::now();
        if at(tcp) {
            match sockets.tcp.accept() {
                Ok((stream, _)) if says_something(&stream) => {
                    return Ok(Some(Sorted {
                        why: Wake::Connection,
                        first: Some(stream),
                    }));
                }
                Ok((_, from)) => debug!("a connection from {from} said nothing: let go"),
                Err(e) => warn!("accepting a connection failed: {e}"),
            }
        }
        if let Some(udp) = &sockets.udp
            && at(udp.as_raw_fd())
            && let Some(why) = sorter.query(udp)
        {
            return started(why);
        }
        if let Some(fd) = sorter.notice_fd()
            && at(fd)
            && let Some(why) = sorter.notice_said()
        {
            return started(why);
        }
    }
}

/// How to start the full program.
pub struct Start {
    pub exe: PathBuf,
    /// Its arguments (`serve` and this run's), without what says why it starts.
    pub args: Vec<String>,
}

impl Start {
    /// This program again, with this run's arguments: a wrapper's name for it (Nix,
    /// `PROSTIR_EXE`), else this very file, as found at the start (see
    /// [`prostir_window::launch::this_program`]).
    pub fn this_program() -> Result<Self> {
        let exe = prostir_window::launch::this_program()?;
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) != Some("serve") {
            args.insert(0, "serve".into());
        }
        // The watcher's own: it starts the program for the window once.
        args.retain(|a| a != "--open-settings");
        Ok(Self { exe, args })
    }

    fn args_for(&self, wake: Wake) -> Vec<String> {
        let mut args = self.args.clone();
        args.push("--woken".into());
        match wake {
            Wake::Pair => args.push("--pair".into()),
            Wake::Window => args.push("--open-settings".into()),
            Wake::Connection | Wake::Headset | Wake::Started => {}
        }
        args
    }
}

/// The full program running as the watcher's child.
struct Child {
    pid: libc::pid_t,
    started: Instant,
}

/// How a child ended, from `waitpid`'s status.
fn ended(status: libc::c_int) -> Option<i32> {
    libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
}

/// The pause before the next start after `fails` crashes in a row.
fn pause_after(fails: u32) -> Duration {
    let secs = 1u64 << fails.clamp(1, 6);
    Duration::from_secs(secs).min(LONGEST_PAUSE)
}

/// The sockets' watcher: holds `sockets` and starts the full program with `start`
/// when a headset comes, until asked to quit. `window`: start it at once for the
/// settings window (`prostir` when nothing ran). Its exit code.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the watcher owns the sockets and how to start until it returns"
)]
pub fn watch(sockets: Sockets, start: Start, window: bool) -> Result<i32> {
    let pipe = signals::install(
        &[
            signals::QUIT_SIGNALS.as_slice(),
            &[libc::SIGUSR1, libc::SIGCHLD],
        ]
        .concat(),
    )?;
    let mut sorter = Sorter::new(sockets.port())?;
    let tcp = sockets.tcp.as_raw_fd();
    let udp = sockets.udp.as_ref().map(AsRawFd::as_raw_fd);
    let mut child: Option<Child> = None;
    let mut fails = 0u32;
    let mut not_before: Option<Instant> = None;
    // Whether it started; one that cannot start waits as a crash does, so the
    // connection waiting does not start it again and again.
    let launch = |wake: Wake, child: &mut Option<Child>| {
        info!("starting Prostir: {wake:?}");
        match sockets::start_with(&start.exe, &start.args_for(wake), &sockets) {
            Ok(pid) => {
                *child = Some(Child {
                    pid,
                    started: Instant::now(),
                });
                true
            }
            Err(e) => {
                warn!("cannot start Prostir ({}): {e}", start.exe.display());
                false
            }
        }
    };
    if let Some(udp) = &sockets.udp {
        info!(
            "answering headsets that ask for this PC: discovery on UDP port {}",
            udp.local_addr()?.port()
        );
    }
    info!(
        "asleep until a headset comes: waiting for the headset on port {}",
        sockets.port()
    );
    let failed_to_start = |fails: &mut u32, not_before: &mut Option<Instant>| {
        *fails += 1;
        *not_before = Some(Instant::now() + pause_after(*fails));
    };
    if window && !launch(Wake::Window, &mut child) {
        failed_to_start(&mut fails, &mut not_before);
    }
    loop {
        // A child that ended (its SIGCHLD may have been one of several at once).
        if let Some(c) = &child {
            let mut status = 0;
            // SAFETY: waits without blocking for this process's own child.
            let pid = unsafe { libc::waitpid(c.pid, &raw mut status, libc::WNOHANG) };
            if pid == c.pid {
                let ran = c.started.elapsed();
                child = None;
                // The full program may have paired or forgotten headsets.
                sorter.open = None;
                match ended(status) {
                    Some(0) => {
                        fails = 0;
                        info!("Prostir went to sleep: waiting for the headset");
                    }
                    Some(QUIT_ALL) => {
                        info!("Prostir was asked to quit: the watcher quits too");
                        return Ok(0);
                    }
                    how => {
                        fails = if ran >= RAN_WELL { 1 } else { fails + 1 };
                        let pause = pause_after(fails);
                        warn!(
                            "Prostir ended ({}): started again for the next headset, in {} s at the soonest",
                            how.map_or_else(|| "killed".to_owned(), |c| format!("code {c}")),
                            pause.as_secs()
                        );
                        not_before = Some(Instant::now() + pause);
                    }
                }
            }
        }
        let now = Instant::now();
        let paused = not_before.filter(|t| *t > now);
        let mut fds = vec![pipe.as_raw_fd()];
        if child.is_none() {
            if paused.is_none() {
                fds.push(tcp);
            }
            fds.extend(udp);
        }
        fds.extend(sorter.notice_fd());
        let wait = crate::brain::earliest([paused, sorter.notice_until(now)])
            .map(|at| at.saturating_duration_since(Instant::now()));
        let ready = sockets::readable(&fds, wait)?;
        let at = |fd: RawFd| fds.iter().position(|f| *f == fd).is_some_and(|i| ready[i]);
        if at(pipe.as_raw_fd()) {
            match signals::wait_for_signal(&pipe) {
                Some(libc::SIGCHLD) => continue,
                Some(libc::SIGUSR1) => {
                    match &child {
                        // Starting, not on the bus yet: it opens the window once it
                        // takes the signal (blocked until then, see `start_with`).
                        Some(c) => {
                            // SAFETY: a signal to this process's own child.
                            unsafe { libc::kill(c.pid, libc::SIGUSR1) };
                        }
                        None => {
                            if !launch(Wake::Window, &mut child) {
                                failed_to_start(&mut fails, &mut not_before);
                            }
                        }
                    }
                    continue;
                }
                Some(signal) => {
                    info!("{} received: quitting", signals::name(signal));
                    if let Some(c) = child.take() {
                        stop(c.pid);
                    }
                    return Ok(0);
                }
                None => anyhow::bail!("reading signals failed"),
            }
        }
        if child.is_some() {
            if let Some(fd) = sorter.notice_fd()
                && at(fd)
                && sorter.notice_said() == Some(Wake::Pair)
            {
                // The program runs already: it opens pairing on the bus.
                open_pairing_on_the_bus();
            }
            continue;
        }
        let mut wake = None;
        if at(tcp) {
            wake = Some(Wake::Connection);
        } else if let (Some(fd), Some(socket)) = (udp, &sockets.udp)
            && at(fd)
        {
            wake = sorter.query(socket).filter(|_| paused.is_none());
        }
        if wake.is_none()
            && let Some(fd) = sorter.notice_fd()
            && at(fd)
        {
            wake = sorter.notice_said();
        }
        if let Some(wake) = wake
            && !launch(wake, &mut child)
        {
            failed_to_start(&mut fails, &mut not_before);
        }
    }
}

/// Asks the full program to quit, and waits for it (ending it if it takes too long).
fn stop(pid: libc::pid_t) {
    // SAFETY: signals and waits for this process's own child.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
        let until = Instant::now() + QUIT_WAIT;
        let mut status = 0;
        while libc::waitpid(pid, &raw mut status, libc::WNOHANG) == 0 {
            if Instant::now() >= until {
                warn!("Prostir did not quit in time: ending it");
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &raw mut status, 0);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The notice's button while the full program runs: pairing opens through its local
/// API.
fn open_pairing_on_the_bus() {
    let opened = crate::platform::local_api::Client::connect(Duration::from_secs(5))
        .map_err(|e| format!("{e:?}"))
        .and_then(|client| {
            use crate::ctl::CtlClient;
            client
                .call(&prostir_api::Request::PairNewHeadset)
                .map_err(|e| format!("{e:?}"))
        });
    if let Err(e) = opened {
        warn!("cannot open pairing: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After crashes in a row the pauses double, up to a minute.
    #[test]
    fn pauses_after_crashes_grow_to_a_minute() {
        let secs: Vec<u64> = (1..=8).map(|n| pause_after(n).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 32, 60, 60, 60]);
    }

    /// The full program starts with this run's arguments and what says why.
    #[test]
    fn the_program_starts_with_why() {
        let start = Start {
            exe: "/usr/bin/prostir".into(),
            args: vec!["serve".into(), "--linger".into(), "5".into()],
        };
        assert_eq!(
            start.args_for(Wake::Pair),
            ["serve", "--linger", "5", "--woken", "--pair"]
        );
        assert_eq!(
            start.args_for(Wake::Window),
            ["serve", "--linger", "5", "--woken", "--open-settings"]
        );
        assert_eq!(
            start.args_for(Wake::Headset),
            ["serve", "--linger", "5", "--woken"]
        );
    }
}
