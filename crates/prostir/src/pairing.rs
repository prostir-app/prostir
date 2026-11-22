//! Encrypted connections and pairing on the PC side.

use crate::i18n::t;
use std::io;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use prostir_proto::discovery::{FindSecret, new_secret};
use prostir_proto::handshake::{self, First};
use prostir_proto::link::tcp::NoiseTcp;
use prostir_proto::link::{Closer, Link, PeerInfo};
use prostir_proto::packet::{self, Assembler, FRAGMENT, lane};
use prostir_proto::secure::{self, Identity, Pairing, TrustStore, confirmation, tags_match};
use prostir_proto::state::Part;
use prostir_proto::{ClientMsg, HostMsg, PROTOCOL_VERSION};

use crate::platform::dialogs::{ask_code, notify};
use crate::platform::{NoCode, tray};

/// Wrong codes allowed per connection before giving up.
const MAX_ATTEMPTS: u32 = 3;
/// Wrong codes allowed in a row (across connections) before pairing slows down.
const FREE_FAILURES: u32 = 3;
/// A code made on the PC stays the same for this long, however many attempts use it.
const CODE_LIFETIME: Duration = Duration::from_secs(300);
/// Encryption, hello and opening the session must be done within this time, however
/// slowly bytes trickle.
const SETUP_TIME: Duration = Duration::from_secs(10);
/// Time to read and type a code.
const PAIRING_TIME: Duration = Duration::from_secs(300);
/// Before the session, each message from the headset comes within this.
const READ_TIME: Duration = Duration::from_secs(5);

/// Wrong pairing codes across all connections. After a few, every attempt waits
/// longer (30 s, 1 min, 2 min… up to 15 min), so guessing the 6-digit code from the
/// network would take years instead of minutes.
struct Limiter {
    failures: u32,
    not_before: Option<Instant>,
    warned: bool,
}

static LIMITER: Mutex<Limiter> = Mutex::new(Limiter {
    failures: 0,
    not_before: None,
    warned: false,
});

impl Limiter {
    /// How long until the next attempt is allowed.
    fn wait(&self) -> Option<Duration> {
        self.not_before
            .and_then(|t| t.checked_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }

    /// An attempt starts: it counts as a wrong code until it succeeds, so one that
    /// never ends (a connection dropped half way) is not free.
    fn begin(&mut self) {
        self.failures += 1;
        self.pause();
    }

    /// The attempt did not succeed: the pause (if any) runs from now.
    fn failed(&mut self) {
        self.pause();
    }

    /// The attempt that began showed nothing (the PC has no dialog for the code): it
    /// does not count.
    fn not_attempted(&mut self) {
        self.failures = self.failures.saturating_sub(1);
        // Begun, it was not paused: a pause it set is its own.
        self.not_before = None;
    }

    fn pause(&mut self) {
        if self.failures > FREE_FAILURES {
            let step = (self.failures - FREE_FAILURES - 1).min(5);
            let wait = Duration::from_secs((30u64 << step).min(900));
            self.not_before = Some(Instant::now() + wait);
            if !self.warned {
                self.warned = true;
                notify(
                    t("Неправильні коди сполучення", "Wrong pairing codes"),
                    t(
                        "Хтось кілька разів ввів неправильний код. Нові спроби тепер чекають.",
                        "Someone typed wrong codes several times. New attempts now have to wait.",
                    ),
                );
            }
        }
    }

    fn succeeded(&mut self) {
        self.failures = 0;
        self.not_before = None;
        self.warned = false;
    }
}

/// The connection whose code the tray shows now, if one does (by its number), and
/// the code.
static CODE_SHOWN_BY: Mutex<Option<(u64, String)>> = Mutex::new(None);
/// Told when the code shown comes or goes (the local API shows it too), and how each
/// pairing ends.
static ON_CODE: std::sync::OnceLock<crate::events::EventSink> = std::sync::OnceLock::new();

/// From now on, `events` hears when the PC begins or stops showing a pairing code, and
/// how each pairing ends.
pub fn tell_about_codes(events: crate::events::EventSink) {
    let _ = ON_CODE.set(events);
}

fn tell(event: crate::events::Event) {
    if let Some(events) = ON_CODE.get() {
        events.send(event);
    }
}

/// The settings window is on the bus: a code a headset shows is typed there, not in
/// a dialog.
static WINDOW_TYPES: AtomicBool = AtomicBool::new(false);

/// A headset waiting for its code to be typed in the settings window: its connection's
/// number, its name, and where the code goes.
static CODE_FOR: Mutex<Option<(u64, String, std::sync::mpsc::Sender<String>)>> = Mutex::new(None);

/// Whether the settings window is on the bus (codes are typed there). Once it leaves,
/// a headset waiting for its code there is told no code was typed.
pub fn window_types_codes(on: bool) {
    WINDOW_TYPES.store(on, Ordering::Release);
    if !on && CODE_FOR.lock().unwrap().take().is_some() {
        code_changed();
    }
}

/// The name of the headset waiting for its code to be typed in the settings window.
pub fn code_for() -> Option<String> {
    CODE_FOR
        .lock()
        .unwrap()
        .as_ref()
        .map(|(_, name, _)| name.clone())
}

/// The code a headset shows, typed in the settings window: `false` when no headset
/// waits for one.
pub fn type_code(code: String) -> bool {
    CODE_FOR
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(_, _, to)| to.send(code).is_ok())
}

/// The pairing code the PC shows now, if any.
pub fn code_shown() -> Option<String> {
    CODE_SHOWN_BY
        .lock()
        .unwrap()
        .as_ref()
        .map(|(_, code)| code.clone())
}

fn code_changed() {
    if let Some(events) = ON_CODE.get() {
        events.send(crate::events::Event::Pairing);
    }
}
/// The number of the next connection set up.
static NEXT_CONNECTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The tray's pairing code as one connection sees it: shown by it, and hidden when it
/// ends, but only if it is still the one that showed it (another connection ending
/// leaves the code of the one pairing).
struct TrayCode<'a> {
    tray: Option<&'a tray::Handle>,
    connection: u64,
}

impl<'a> TrayCode<'a> {
    fn new(tray: Option<&'a tray::Handle>) -> Self {
        Self {
            tray,
            connection: NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn show(&self, code: String) {
        *CODE_SHOWN_BY.lock().unwrap() = Some((self.connection, code.clone()));
        tray::set_pairing(self.tray, Some(code));
        code_changed();
    }

    /// The attempt ended: a headset waiting for its code to be typed in the settings
    /// window waits no more, if it was this connection's.
    fn code_done(&self) {
        let mut waiting = CODE_FOR.lock().unwrap();
        if waiting
            .as_ref()
            .is_some_and(|(by, _, _)| *by == self.connection)
        {
            *waiting = None;
            drop(waiting);
            code_changed();
        }
    }

    fn hide(&self) {
        self.code_done();
        let mut shown_by = CODE_SHOWN_BY.lock().unwrap();
        if shown_by
            .as_ref()
            .is_some_and(|(by, _)| *by == self.connection)
        {
            *shown_by = None;
            drop(shown_by);
            tray::set_pairing(self.tray, None);
            code_changed();
        }
    }

    #[cfg(test)]
    fn shows(&self) -> bool {
        CODE_SHOWN_BY
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(by, _)| *by == self.connection)
    }
}

impl Drop for TrayCode<'_> {
    fn drop(&mut self) {
        self.hide();
    }
}

/// Held while a headset pairs: one pairing at a time, so the wrong codes counted
/// before each attempt hold for every connection (several at once would each pass
/// the check before any had failed).
static PAIRING: Mutex<()> = Mutex::new(());

/// How long a headset that asks to pair waits for the pairing of another connection
/// to end before it is told to wait: a headset that hung up and dials again (the
/// other way round) finds its earlier attempt ending as it comes.
const PAIRING_HANDOVER: Duration = Duration::from_secs(1);

/// How often a code typed in the settings window is waited for before the connection
/// is looked at again: a headset that hung up meanwhile ends the attempt.
const LOOK_AT_THE_CONNECTION: Duration = Duration::from_millis(100);

/// The pairing taken for this connection, once no other connection pairs (within
/// [`PAIRING_HANDOVER`]); `None` while another one still does.
fn take_pairing() -> Option<std::sync::MutexGuard<'static, ()>> {
    let until = Instant::now() + PAIRING_HANDOVER;
    loop {
        match PAIRING.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(TryLockError::WouldBlock) => return None,
        }
    }
}

/// The code shown on the PC: one per pairing window, not one per attempt.
static PC_CODE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// Closes a connection that is not through some stage in time: per-read timeouts
/// alone let a peer that sends a byte every few seconds hold the PC forever.
struct Deadline {
    state: Arc<(Mutex<Instant>, std::sync::Condvar)>,
    done: Arc<AtomicBool>,
}

impl Deadline {
    fn new(closer: &Closer, after: Duration) -> Result<Self> {
        let state = Arc::new((
            Mutex::new(Instant::now() + after),
            std::sync::Condvar::new(),
        ));
        let done = Arc::new(AtomicBool::new(false));
        let watched = closer.clone();
        let (st, dn) = (state.clone(), done.clone());
        std::thread::Builder::new()
            .name("deadline".into())
            .spawn(move || {
                let (lock, cv) = &*st;
                let mut at = lock.lock().unwrap();
                while !dn.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if now >= *at {
                        warn!("connection setup took too long: closing it");
                        watched.close();
                        return;
                    }
                    let left = *at - now;
                    at = cv.wait_timeout(at, left).unwrap().0;
                }
            })?;
        Ok(Self { state, done })
    }

    fn extend(&self, after: Duration) {
        *self.state.0.lock().unwrap() = Instant::now() + after;
        self.state.1.notify_all();
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        self.state.1.notify_all();
    }
}

pub fn config_dir() -> PathBuf {
    crate::platform::config_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prostir")
}

/// The PC's key and the headsets it trusts, shared by the connections being set up
/// (each on a thread of its own) and the control loop, which forgets them.
pub struct Security {
    pub identity: Identity,
    pub trusted: Mutex<TrustStore>,
    /// What the trusted headsets' discovery tags for this PC are made with (see
    /// `prostir_proto::discovery` and `crate::find`): given to them in the session,
    /// made anew when the PC forgets a headset.
    pub find: Mutex<FindSecrets>,
    /// Where the find secret is kept.
    find_file: PathBuf,
}

/// The PC's find secret, and the older ones still owed: a trusted headset that was not
/// in a session when the secret changed asks with the one it has, which stays valid for
/// it until its next session gives it the current one, or it is forgotten (step 7 note,
/// 8.1, question 35).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindSecrets {
    pub current: FindSecret,
    /// The secret each such headset (by its key) still has, and when it stopped being
    /// the current one (a number that grows with each new secret).
    pub owed: std::collections::BTreeMap<[u8; 32], (FindSecret, u64)>,
}

impl FindSecrets {
    /// What a query may be answered by: the current secret and those still owed, the
    /// newest [`crate::find::STALE_KEPT`] of them (a headset that has an older one
    /// reaches the PC by its last address). Nothing new is made.
    pub fn valid(&self) -> crate::find::Secrets {
        let mut newest = [[0u8; 32]; crate::find::STALE_KEPT];
        let mut n = 0;
        while n < newest.len() {
            let taken = &newest[..n];
            let next = self
                .owed
                .values()
                .filter(|(s, _)| *s != self.current && !taken.contains(s))
                .max_by_key(|(_, made)| *made);
            let Some((secret, _)) = next else { break };
            newest[n] = *secret;
            n += 1;
        }
        crate::find::Secrets::with_stale(self.current, &newest[..n])
    }

    /// The current secret changes to `new`: each headset of `off` (trusted, not in a
    /// session, so it does not get `new` now) keeps the one it has valid.
    fn renew(&mut self, new: FindSecret, off: impl IntoIterator<Item = [u8; 32]>) {
        let made = self.owed.values().map(|(_, m)| m + 1).max().unwrap_or(1);
        let old = std::mem::replace(&mut self.current, new);
        for key in off {
            self.owed.entry(key).or_insert((old, made));
        }
    }
}

/// The file of the find secret, in `dir`.
pub fn find_secret_file(dir: &std::path::Path) -> PathBuf {
    dir.join("find_secret")
}

/// The file of the find secrets still owed, next to `secret_file`: a line
/// `<headset key hex> <secret hex> <when>` for each headset that has an older one (when:
/// a number that grows with each new secret). A file of its own, so `find_secret`
/// stays as earlier builds read it.
pub fn owed_file(secret_file: &std::path::Path) -> PathBuf {
    secret_file.with_file_name("find_secret_owed")
}

/// The find secret kept in `path`, made on first use. A file that is there but does
/// not read is an error, as for the PC's key: a new one would leave every headset
/// unable to find the PC without a word.
pub fn load_find_secret(path: &std::path::Path) -> Result<FindSecret> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let secret = secure::from_hex::<32>(text.trim());
            secret.ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is no find secret (move it away to make a new one; headsets get it at \
                     their next session)",
                    path.display()
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => save_find_secret(path, new_secret()),
        Err(e) => Err(anyhow::anyhow!("cannot read {}: {e}", path.display())),
    }
}

/// The find secrets in `path` and the ones owed next to it (see [`owed_file`]). The
/// owed ones are read leniently: a line that does not read is left out (that headset
/// then finds the PC by its last address until its next session).
pub fn load_find_secrets(path: &std::path::Path) -> Result<FindSecrets> {
    let current = load_find_secret(path)?;
    let owed = match std::fs::read_to_string(owed_file(path)) {
        Ok(text) => text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let key = secure::from_hex::<32>(fields.next()?)?;
                let secret = secure::from_hex::<32>(fields.next()?)?;
                let made = fields.next().and_then(|m| m.parse().ok()).unwrap_or(0);
                Some((key, (secret, made)))
            })
            .collect(),
        Err(_) => std::collections::BTreeMap::new(),
    };
    Ok(FindSecrets { current, owed })
}

fn save_find_secret(path: &std::path::Path, secret: FindSecret) -> Result<FindSecret> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    secure::write_atomic(path, format!("{}\n", secure::to_hex(&secret)).as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(secret)
}

/// Keeps the secrets still owed next to `path` (no file while none is).
fn save_owed(path: &std::path::Path, secrets: &FindSecrets) -> Result<()> {
    use std::fmt::Write as _;
    let file = owed_file(path);
    if secrets.owed.is_empty() {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(anyhow::anyhow!("cannot remove {}: {e}", file.display()))
            }
            _ => Ok(()),
        };
    }
    let mut text = String::new();
    for (key, (secret, made)) in &secrets.owed {
        let _ = writeln!(
            text,
            "{} {} {made}",
            secure::to_hex(key),
            secure::to_hex(secret)
        );
    }
    secure::write_atomic(&file, text.as_bytes())
        .with_context(|| format!("cannot write {}", file.display()))
}

impl Security {
    pub fn load() -> Result<Self> {
        let dir = config_dir();
        let find_file = find_secret_file(&dir);
        // A write cut short leaves its temporary file; the real one is whole.
        for file in [
            dir.join("identity"),
            dir.join("paired_headsets"),
            find_file.clone(),
            owed_file(&find_file),
        ] {
            secure::remove_leftovers(&file);
        }
        let identity =
            Identity::load_or_create(&dir.join("identity")).context("cannot load the PC's key")?;
        info!("PC key {}", secure::fingerprint(&identity.public));
        // Unreadable, it would read as no headset at all: everyone answered.
        let trusted = TrustStore::try_load(&dir.join("paired_headsets"))
            .context("cannot read the paired headsets")?;
        let mut find = load_find_secrets(&find_file)?;
        // Owed only to headsets still trusted.
        find.owed.retain(|key, _| trusted.contains(key));
        Ok(Self {
            identity,
            trusted: Mutex::new(trusted),
            find: Mutex::new(find),
            find_file,
        })
    }

    /// A new find secret, after the PC forgot the headsets `forgotten`: queries made
    /// with the old one get no answer any more, but from the trusted headsets for whom
    /// `in_session` is false (they get the new one at their next session, and until
    /// then the one they have stays valid). The new one: the headsets in a session
    /// get it now ([`Security::gave_find_secret`]).
    pub fn new_find_secret(
        &self,
        forgotten: &[[u8; 32]],
        in_session: impl Fn(&[u8; 32]) -> bool,
    ) -> FindSecret {
        let trusted: Vec<[u8; 32]> = self
            .trusted
            .lock()
            .unwrap()
            .entries()
            .iter()
            .map(|(key, _)| *key)
            .filter(|key| !forgotten.contains(key))
            .collect();
        let secret = new_secret();
        let mut find = self.find.lock().unwrap();
        // Owed to trusted headsets only: one forgotten is owed nothing.
        find.owed.retain(|key, _| trusted.contains(key));
        find.renew(secret, trusted.into_iter().filter(|key| !in_session(key)));
        // The old secret kept for the headsets that were off first, then the new one:
        // cut short between the two, the files lose nothing a headset holds.
        if let Err(e) = save_owed(&self.find_file, &find) {
            warn!("{e:#}");
        }
        if let Err(e) = save_find_secret(&self.find_file, secret) {
            warn!("{e:#}");
        }
        info!(
            "a new find secret: a forgotten headset can no longer find this PC ({} old \
             one(s) still valid for headsets not connected)",
            find.valid().stale().len()
        );
        secret
    }

    /// Headset `key` got the current find secret in its session: the older one it had
    /// is valid no more.
    pub fn gave_find_secret(&self, key: &[u8; 32]) {
        let mut find = self.find.lock().unwrap();
        if find.owed.remove(key).is_some() {
            info!(
                "headset {} has the current find secret now",
                secure::fingerprint(key)
            );
            if let Err(e) = save_owed(&self.find_file, &find) {
                warn!("{e:#}");
            }
        }
    }

    /// The find secret now.
    pub fn find_secret(&self) -> FindSecret {
        self.find.lock().unwrap().current
    }

    /// What a query may be answered by now.
    pub fn find_secrets(&self) -> crate::find::Secrets {
        self.find.lock().unwrap().valid()
    }
}

/// A connection in packets: the link, and the headset's packets put together so far.
pub struct Connection {
    pub link: Link,
    pub packets: Assembler,
    /// The headset app's version, as its hello said (`0.2.0`, `0.2.0-dev+p6`).
    pub app: String,
}

impl Connection {
    fn recv(&mut self) -> io::Result<ClientMsg> {
        ClientMsg::recv(&mut *self.link.receiver, &mut self.packets)
    }

    /// Sends `msg` whole in the control lane (before the session's writer runs).
    pub fn send(&mut self, msg: HostMsg) -> io::Result<()> {
        packet::send_whole(
            &mut *self.link.sender,
            lane::CONTROL,
            msg.encode()?,
            FRAGMENT,
        )
    }

    fn set_idle_limit(&mut self, idle: Duration) {
        self.link.receiver.set_idle_limit(Some(idle));
    }
}

/// Encrypts the connection, checks the versions, pairs the headset if either side asks,
/// and returns the connection once the headset is trusted and asked to open the
/// session, with the parts of the headset's state its `Open` carried. The caller
/// answers `Snapshot` once it can serve the session (its monitors made, its writer
/// running), or an `Error` saying why it cannot.
pub fn accept(
    stream: TcpStream,
    security: &Security,
    tray: Option<&tray::Handle>,
) -> Result<(Connection, Vec<Part>)> {
    accept_within(stream, security, tray, SETUP_TIME)
}

/// As [`accept`], with `setup` for everything up to `Open` (but pairing).
fn accept_within(
    stream: TcpStream,
    security: &Security,
    tray: Option<&tray::Handle>,
    setup: Duration,
) -> Result<(Connection, Vec<Part>)> {
    // A vanished headset does not hold the PC (the transport also gives up a write
    // after a few seconds, and the deadline closes a setup that drags on).
    stream.set_read_timeout(Some(READ_TIME))?;
    // Looked at while the person at the PC is asked for a code: a headset that hung
    // up meanwhile ends the attempt (and lets go of the pairing at once).
    let watched = stream.try_clone()?;
    let gone = || crate::platform::sockets::hung_up(&watched);
    // The deadline covers the Noise handshake too: a peer that sends a byte now and
    // then, each within the read timeout, must not hold the PC for hours. Until the
    // link exists, the deadline closes the socket itself.
    let socket = stream.try_clone()?;
    let closer = Closer::new(move || {
        let _ = socket.shutdown(std::net::Shutdown::Both);
    });
    let deadline = Deadline::new(&closer, setup)?;
    let mut link = NoiseTcp::accept(
        stream,
        &security.identity,
        Arc::new(crate::platform::TcpOptions),
    )?;
    // However this connection ends, the tray stops showing a code it showed.
    let tray_code = TrayCode::new(tray);
    let peer = link.peer.clone();
    // Versions first, before pairing: this exchange is the same in every version, so
    // a headset of any version learns which side to update.
    let first = handshake::read_first(&mut *link.receiver)?;
    let headset_app = match &first {
        First::Hello { app, .. } => peer_version(app),
        First::Other => String::new(),
    };
    match first {
        First::Hello {
            version, min, app, ..
        } if !prostir_proto::compatible(version, min) => {
            let app = peer_version(&app);
            let update = if min > PROTOCOL_VERSION {
                Update::Pc
            } else {
                Update::Headset
            };
            let first = incompatible(&mut link, update, &app)?;
            close_gently(&mut link);
            return Err(turned_away(
                first,
                format!("headset speaks protocol v{version} (app {app}), PC v{PROTOCOL_VERSION}"),
            ));
        }
        // Two development builds of different revisions: the same protocol version,
        // other messages.
        First::Hello { app, .. } if !prostir_proto::same_revision(&app) => {
            let app = peer_version(&app);
            let first = incompatible(&mut link, Update::Both, &app)?;
            close_gently(&mut link);
            return Err(turned_away(
                first,
                format!(
                    "headset build {app} reads other messages than this one ({})",
                    prostir_proto::app_version(env!("CARGO_PKG_VERSION"))
                ),
            ));
        }
        First::Hello { alone: true, .. } => {}
        // Packets follow the hello: one that shares its record leaves bytes they
        // cannot account for.
        First::Hello { alone: false, .. } => bail!("the headset's hello shares its record"),
        // Only headsets from before protocol 2 (0.1) start with anything else: they
        // could ask to pair before saying hello.
        First::Other => {
            let first = incompatible(&mut link, Update::Headset, "0.1.x")?;
            close_gently(&mut link);
            return Err(turned_away(
                first,
                format!("headset from before protocol {PROTOCOL_VERSION}: no hello first"),
            ));
        }
    }
    handshake::send_from_pc(
        &mut *link.sender,
        &HostMsg::Welcome {
            version: PROTOCOL_VERSION,
            language: crate::i18n::code().into(),
        },
    )?;
    // From here on, packets.
    let mut conn = Connection {
        link,
        packets: ClientMsg::assembler(),
        app: headset_app,
    };
    let mut attempts = 0;
    loop {
        match conn.recv()? {
            ClientMsg::Open { parts } if security.trusted.lock().unwrap().contains(&peer.key) => {
                conn.link.receiver.set_idle_limit(None);
                drop(deadline);
                return Ok((conn, parts));
            }
            ClientMsg::Open { .. } => {
                info!(
                    "unknown headset {}: pairing needed",
                    secure::fingerprint(&peer.key)
                );
                conn.send(HostMsg::PairingRequired)?;
            }
            ClientMsg::PairBegin {
                headset_shows,
                name,
            } => {
                attempts += 1;
                // From the network: shown in the window, the tray and the list of headsets.
                let name = crate::door::clean_name(&name);
                if attempts > MAX_ATTEMPTS {
                    bail!("too many wrong pairing codes");
                }
                let wait = LIMITER.lock().unwrap().wait();
                if let Some(wait) = wait {
                    let minutes = wait.as_secs().div_ceil(60);
                    refuse(
                        &mut conn,
                        &format!(
                            "{} {minutes} {}",
                            t(
                                "Забагато неправильних кодів. Спробуйте за",
                                "Too many wrong codes. Try again in",
                            ),
                            t("хв.", "min."),
                        ),
                    )?;
                    bail!(
                        "pairing paused after wrong codes ({} s left)",
                        wait.as_secs()
                    );
                }
                let Some(_pairing) = take_pairing() else {
                    info!("another headset is pairing: this one is told to wait");
                    refuse(
                        &mut conn,
                        t(
                            "Інший шолом саме сполучається з цим ПК. Спробуйте за хвилину.",
                            "Another headset is pairing with this PC right now. Try again in a minute.",
                        ),
                    )?;
                    // Its half of the exchange came with the request.
                    if headset_shows {
                        let _ = conn.recv();
                    }
                    continue;
                };
                LIMITER.lock().unwrap().begin();
                // The user needs time to read and type the code.
                conn.set_idle_limit(PAIRING_TIME);
                deadline.extend(PAIRING_TIME);
                // `None`: no attempt (the dialog was cancelled or there is none), which
                // is not a wrong code.
                let mut shown = true;
                let paired = match obtain_code(headset_shows, &name, &tray_code, &gone) {
                    Ok(code) => pair(&mut conn, &security.identity, &peer, &code).map(Some),
                    // Its pairing is let go of as this returns, for the headset dialling
                    // again (the other way round, say).
                    Err(NoCode::Gone) => {
                        LIMITER.lock().unwrap().failed();
                        bail!("the headset hung up while its code was asked for");
                    }
                    Err(NoCode::Cancelled) => refuse(
                        &mut conn,
                        t("На ПК код не введено", "No code was typed on the PC"),
                    )
                    .map(|_| None),
                    Err(NoCode::NoDialog) => {
                        shown = false;
                        refuse(
                            &mut conn,
                            t(
                                "На ПК немає вікна для коду. Виберіть «Навпаки: код на ноутбуці, ввести тут».",
                                "The PC has no dialog for the code. Choose \"The other way: code on the PC, type it here\".",
                            ),
                        )
                        .map(|_| None)
                    }
                };
                conn.set_idle_limit(READ_TIME);
                deadline.extend(setup);
                // The headset sent its half of the exchange with its request: with no
                // attempt made, it is read and dropped so the next request can follow.
                if headset_shows && matches!(paired, Ok(None)) {
                    let _ = conn.recv();
                }
                // Every attempt that did not succeed counted towards the slowdown (from
                // its start), once: a cancelled dialog or a broken exchange too, so a peer
                // cannot keep the PC showing dialogs and codes for free. One that showed
                // nothing (no dialog on the PC) does not count.
                let paired = match paired {
                    Ok(p) => p,
                    Err(e) => {
                        LIMITER.lock().unwrap().failed();
                        return Err(e);
                    }
                };
                match paired {
                    None if !shown => {
                        info!("pairing not attempted: no dialog for the code on the PC");
                        LIMITER.lock().unwrap().not_attempted();
                    }
                    None => {
                        info!("pairing not attempted: no code on the PC");
                        LIMITER.lock().unwrap().failed();
                    }
                    Some(true) => {
                        LIMITER.lock().unwrap().succeeded();
                        tell(crate::events::Event::Paired {
                            name: if name.is_empty() {
                                t("Шолом", "Headset").into()
                            } else {
                                name.clone()
                            },
                        });
                        *PC_CODE.lock().unwrap() = None;
                        tray_code.hide();
                        let label = if name.is_empty() {
                            "headset".into()
                        } else {
                            name
                        };
                        security.trusted.lock().unwrap().add(peer.key, &label)?;
                        info!("paired with {label} ({})", secure::fingerprint(&peer.key));
                        notify(
                            t("Шолом сполучено", "Headset paired"),
                            &format!(
                                "{label} {}",
                                t("тепер може підключатися.", "can connect now.")
                            ),
                        );
                    }
                    Some(false) => {
                        warn!("pairing failed: the codes did not match");
                        LIMITER.lock().unwrap().failed();
                        tell(crate::events::Event::WrongCode);
                    }
                }
                tray_code.code_done();
            }
            other => bail!("unexpected message before the session: {}", brief(&other)),
        }
    }
}

/// The code for this attempt: typed on the PC (the headset shows it), or generated
/// here and shown in the tray and a notification (the headset asks for it). While it
/// is typed, `gone` says whether the headset hung up, which ends the wait.
fn obtain_code(
    headset_shows: bool,
    name: &str,
    tray: &TrayCode<'_>,
    gone: &dyn Fn() -> bool,
) -> Result<String, NoCode> {
    if headset_shows {
        if !WINDOW_TYPES.load(Ordering::Acquire) {
            return ask_code(gone);
        }
        // The settings window is open: the code is typed there.
        let (to, code) = std::sync::mpsc::channel();
        let name = if name.is_empty() {
            t("Шолом", "Headset").to_owned()
        } else {
            name.to_owned()
        };
        *CODE_FOR.lock().unwrap() = Some((tray.connection, name, to));
        code_changed();
        // The window shows the headset waiting until the attempt ends (`code_done`).
        let until = Instant::now() + PAIRING_TIME;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match code.recv_timeout(left.min(LOOK_AT_THE_CONNECTION)) {
                Ok(code) => return Ok(code),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(NoCode::Cancelled);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if gone() => {
                    return Err(NoCode::Gone);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if left.is_zero() => {
                    return Err(NoCode::Cancelled);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    let mut current = PC_CODE.lock().unwrap();
    let (code, new) = match &*current {
        Some((code, made)) if made.elapsed() < CODE_LIFETIME => (code.clone(), false),
        _ => {
            let code = secure::pairing_code();
            *current = Some((code.clone(), Instant::now()));
            (code, true)
        }
    };
    drop(current);
    let shown = format!("{} {}", &code[..3], &code[3..]);
    // Never into the log: whoever can read the journal could pair with it.
    info!("pairing code shown in the tray");
    tray.show(shown.clone());
    // Once per code: a peer asking again and again must not flood the desktop.
    if new {
        notify(
            t("Сполучення шолома", "Pairing a headset"),
            &format!(
                "{} {shown}",
                t("Введіть у шоломі код", "Type this code in the headset:")
            ),
        );
    }
    Ok(code)
}

/// A headset turned away for its version again: a 0.1 headset that is not paired yet
/// tries every few hundred milliseconds for minutes, and it was logged and shown once.
#[derive(Debug)]
pub struct TurnedAwayAgain(String);

impl std::fmt::Display for TurnedAwayAgain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TurnedAwayAgain {}

fn turned_away(first: bool, why: String) -> anyhow::Error {
    if first {
        anyhow::anyhow!(why)
    } else {
        TurnedAwayAgain(why).into()
    }
}

/// Lets the headset read what was sent before the connection closes. Closing with
/// unread data from the peer resets the connection, which can throw away what was
/// sent (over `adb reverse` it does): so the sending half closes first, and what the
/// headset still sends is read and dropped for a moment.
pub fn close_gently(link: &mut Link) {
    let _ = link.sender.finish();
    let until = Instant::now() + Duration::from_millis(500);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if left.is_zero() {
            break;
        }
        link.receiver.set_idle_limit(Some(left));
        if link.receiver.recv().is_err() {
            break;
        }
    }
}

/// Which side has to be updated for a headset and a PC to talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Update {
    Pc,
    Headset,
    /// Development builds of different revisions (see `prostir_proto::PROTO_REVISION`).
    Both,
}

/// The headset speaks another protocol: tell both the headset and the person at the PC
/// which side needs the update. `false` when this was logged and shown a moment ago.
fn incompatible(link: &mut Link, update: Update, headset_app: &str) -> Result<bool> {
    let ours = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
    let headset_app = if headset_app.is_empty() {
        t("старіша", "older")
    } else {
        headset_app
    };
    let (title, body) = match update {
        Update::Pc => (
            t("Оновіть Prostir на ПК", "Update Prostir on the PC"),
            format!(
                "{} {headset_app}, {} {ours}. prostir.ishawyha.dev",
                t("Застосунок у шоломі новіший:", "The headset app is newer:"),
                t("а на ПК", "the PC has")
            ),
        ),
        Update::Headset => (
            t("Оновіть Prostir у шоломі", "Update Prostir in the headset"),
            format!(
                "{} {headset_app}, {} {ours}.",
                t("Застосунок у шоломі старіший:", "The headset app is older:"),
                t("а на ПК", "the PC has")
            ),
        ),
        Update::Both => (
            t(
                "Збірки шолома й ПК з різних версій гілки",
                "The headset and PC builds are from different versions of the branch",
            ),
            format!(
                "{} {headset_app}, {} {ours}. {}",
                t("У шоломі", "The headset has"),
                t("а на ПК", "the PC has"),
                t("Оновіть обидві.", "Update both.")
            ),
        ),
    };
    // Headsets from before `Incompatible` existed still show this text.
    let error = HostMsg::Error {
        text: format!("{title}. {body}"),
    };
    handshake::send_from_pc(&mut *link.sender, &error)?;
    let answer = HostMsg::Incompatible {
        protocol: PROTOCOL_VERSION,
        app: ours,
    };
    handshake::send_from_pc(&mut *link.sender, &answer)?;
    // Once per headset version, and at most once in ten minutes whatever versions a
    // peer claims: not on every reconnection attempt.
    static TOLD: Mutex<(Vec<String>, Option<Instant>)> = Mutex::new((Vec::new(), None));
    let mut told = TOLD.lock().unwrap();
    let quiet = told
        .1
        .is_some_and(|t| t.elapsed() < Duration::from_secs(600));
    let first = !quiet && !told.0.iter().any(|t| t == headset_app);
    if first {
        told.0.push(headset_app.to_owned());
        told.1 = Some(Instant::now());
        warn!("{title}: {body}");
        notify(title, &body);
    } else {
        debug!("{title}: {body}");
    }
    Ok(first)
}

/// The app version a peer that has not proved anything yet says it has, as it may go
/// into the log or a notification: version-like characters only, and short.
fn peer_version(app: &str) -> String {
    app.chars()
        .filter(|c| c.is_ascii_alphanumeric() || ".+-".contains(*c))
        .take(24)
        .collect()
}

fn refuse(conn: &mut Connection, why: &str) -> Result<bool> {
    conn.send(HostMsg::Error { text: why.into() })?;
    conn.send(HostMsg::PairResult { tag: None })?;
    Ok(false)
}

/// A short description of `msg` for the log (a message from a peer that is not
/// trusted must not become a huge log line).
fn brief(msg: &ClientMsg) -> String {
    let full = format!("{msg:?}");
    let mut short: String = full.chars().take(80).collect();
    if full.chars().count() > 80 {
        short.push('…');
    }
    short
}

/// One pairing attempt with `code`; `Ok(false)` when the codes did not match.
fn pair(conn: &mut Connection, identity: &Identity, peer: &PeerInfo, code: &str) -> Result<bool> {
    let (pairing, msg) = Pairing::start(code);
    conn.send(HostMsg::PairMsg { msg })?;
    let theirs = match conn.recv()? {
        ClientMsg::PairMsg { msg } => msg,
        other => bail!(
            "expected the headset's pairing message, got {}",
            brief(&other)
        ),
    };
    let key = pairing.finish(&theirs)?;
    let tag = match conn.recv()? {
        ClientMsg::PairConfirm { tag } => tag,
        other => bail!("expected a pairing confirmation, got {}", brief(&other)),
    };
    let own = &identity.public;
    let ok = tags_match(&tag, &confirmation(&key, "headset", peer, own));
    conn.send(HostMsg::PairResult {
        tag: ok.then(|| confirmation(&key, "pc", peer, own)),
    })?;
    Ok(ok)
}

#[cfg(test)]
mod tests;
