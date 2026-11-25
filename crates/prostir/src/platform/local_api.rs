//! The local API on D-Bus (see `crate::api` and `LOCAL-API.md`): the service on the
//! session bus, and the client `prostir ctl` and `prostir` use. The same on
//! every Linux desktop and with the fake one (the end-to-end tests run it on a
//! private bus). Only programs of the same user reach the session bus, so the same
//! trust as the tray, which shows the same things.
//!
//! The service runs on zbus's own thread; its methods only hand a request to the
//! control loop and wait (asleep) for the reply. One more thread, `local-api`,
//! takes what the control loop sends out (states, events, replies) in order: a
//! reply never goes out before the `StateChanged` with what it changed, and
//! `StateChanged` goes out at most ten times a second. Each of them sleeps until
//! something comes, so an idle PC still does not wake.

use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use zbus::blocking::Connection;

use crate::api::{
    self, Answer, ApiError, BUS_NAME, CtlReply, ERROR_PREFIX, Failure, INTERFACE, Note,
    OBJECT_PATH, Outlet, Reply, Request, STATE_VERSION, STATES_PER_SECOND,
};
use crate::ctl::{ClientError, CtlClient, Watched};
use crate::events::{Event, EventSink};

/// The state as `GetState` gives it: its revision and its JSON.
type Shown = Arc<Mutex<(u32, String)>>;

/// What the service's parts share: the way to the control loop and back, the state
/// `GetState` gives, the connection (none while the bus is away), and whether
/// Prostir is quitting.
struct Shared {
    events: EventSink,
    outlet: Outlet,
    shown: Shown,
    bus: Mutex<Option<Connection>>,
    quitting: AtomicBool,
    /// Requests handed to the control loop and not answered yet.
    asking: Mutex<usize>,
    answered: Condvar,
}

impl Shared {
    fn bus(&self) -> Option<Connection> {
        self.bus.lock().unwrap().clone()
    }
}

/// The service while it runs.
pub struct Service {
    shared: Arc<Shared>,
}

impl Service {
    /// Leaves the bus once every reply given so far went out (`quit` is answered
    /// too), a second at most; requests from now on are told Prostir is quitting.
    pub fn finish(self) {
        let shared = &self.shared;
        shared.quitting.store(true, Ordering::Release);
        let (tx, rx) = std::sync::mpsc::channel();
        shared.outlet.send(Note::Answer(Box::new(move |_| {
            let _ = tx.send(());
        })));
        let until = Instant::now() + Duration::from_secs(1);
        let _ = rx.recv_timeout(Duration::from_secs(1));
        // Each request still answering holds the connection until its reply is
        // written; then nothing else does, and it closes.
        let mut asking = shared.asking.lock().unwrap();
        while *asking > 0 {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            asking = shared.answered.wait_timeout(asking, left).unwrap().0;
        }
        drop(asking);
        let Some(bus) = shared.bus.lock().unwrap().take() else {
            return;
        };
        let (done, closed) = std::sync::mpsc::channel();
        let left = until.saturating_duration_since(Instant::now());
        let shutdown = std::thread::Builder::new()
            .name("local-api-end".into())
            .spawn(move || {
                bus.graceful_shutdown();
                let _ = done.send(());
            });
        if shutdown.is_ok() {
            let _ = closed.recv_timeout(left.max(Duration::from_millis(100)));
        }
    }
}

/// Offers the API on the session bus: requests go to the control loop as events,
/// and what the control loop sends to `outlet`'s other end (`notes`) goes out. `None`
/// (logged) without a session bus, or when another program has the name. A bus
/// that goes away later (dbus-daemon restarted) is taken again once it is back, with
/// pauses of 1 s growing to a minute between tries.
pub fn serve(events: EventSink, outlet: Outlet, notes: Receiver<Note>) -> Option<Service> {
    // The first state, there before the name is: `GetState` always has one.
    let first = if let Ok(Note::State(revision, state)) = notes.try_recv() {
        (revision, state.to_json())
    } else {
        (0, String::new())
    };
    let shared = Arc::new(Shared {
        events,
        outlet,
        shown: Arc::new(Mutex::new(first)),
        bus: Mutex::new(None),
        quitting: AtomicBool::new(false),
        asking: Mutex::new(0),
        answered: Condvar::new(),
    });
    let gone = match offer(&shared) {
        Ok(gone) => gone,
        Err(e) => {
            warn!("no local API on D-Bus ({BUS_NAME}): {e}");
            return None;
        }
    };
    let out = shared.clone();
    let started = std::thread::Builder::new()
        .name("local-api".into())
        .spawn(move || send_out(&out, &notes));
    if let Err(e) = started {
        warn!("no local API on D-Bus: {e}");
        return None;
    }
    if let Some(gone) = gone {
        let watched = shared.clone();
        let watching = std::thread::Builder::new()
            .name("local-api-bus".into())
            .spawn(move || keep_on_the_bus(&watched, gone));
        if let Err(e) = watching {
            warn!("the local API will not come back after the session bus restarts: {e}");
        }
    }
    info!("local API on D-Bus as {BUS_NAME}");
    Some(Service { shared })
}

/// Takes the name and serves the object on a new connection to the session bus:
/// the socket to watch for the bus going away, where there is one.
fn offer(shared: &Arc<Shared>) -> zbus::Result<Option<UnixStream>> {
    let (builder, socket) = session()?;
    let bus = builder
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            Object {
                shared: shared.clone(),
            },
        )?
        .build()?;
    follow_the_window(&bus, shared.events.clone());
    *shared.bus.lock().unwrap() = Some(bus);
    Ok(socket)
}

/// Tells the control loop whether the settings window is on the bus
/// (`Event::Window`), now and each time it comes or goes, on a thread of its own that
/// sleeps until the bus says so. It ends with the connection (the bus went away: the
/// window counts as gone until the next connection says otherwise).
fn follow_the_window(bus: &Connection, events: EventSink) {
    use zbus::blocking::fdo::DBusProxy;
    let name = match zbus::names::BusName::try_from(api::SETTINGS_NAME) {
        Ok(name) => name.to_owned(),
        Err(e) => {
            warn!("{e}");
            return;
        }
    };
    let bus = bus.clone();
    let spawned = std::thread::Builder::new()
        .name("local-api-window".into())
        .spawn(move || {
            let proxy = match DBusProxy::new(&bus) {
                Ok(proxy) => proxy,
                Err(e) => return warn!("the settings window cannot be followed: {e}"),
            };
            // Subscribed first, so a change between the look and the subscription is
            // not missed.
            let changes = proxy.receive_name_owner_changed_with_args(&[(0, api::SETTINGS_NAME)]);
            let changes = match changes {
                Ok(changes) => changes,
                Err(e) => return warn!("the settings window cannot be followed: {e}"),
            };
            if proxy.name_has_owner(name).unwrap_or(false) {
                events.send(Event::Window(true));
            }
            for change in changes {
                let Ok(args) = change.args() else { continue };
                if !events.send(Event::Window(args.new_owner().is_some())) {
                    return;
                }
            }
            events.send(Event::Window(false));
        });
    if let Err(e) = spawned {
        warn!("the settings window cannot be followed: {e}");
    }
}

/// Sleeps until the session bus closes `socket`, then takes the name again once the
/// bus is back, and so on (while Prostir runs, which this thread does not outlive).
fn keep_on_the_bus(shared: &Arc<Shared>, mut socket: UnixStream) {
    loop {
        // Asleep in the kernel until the other end hangs up: no wakeups meanwhile.
        let mut fd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLRDHUP,
            revents: 0,
        };
        // SAFETY: one valid pollfd, for a socket this thread owns.
        let woke = unsafe { libc::poll(&raw mut fd, 1, -1) };
        if woke < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if shared.quitting.load(Ordering::Acquire) {
            return;
        }
        warn!("the session bus went away: offering the local API again once it is back");
        *shared.bus.lock().unwrap() = None;
        let mut pause = Duration::from_secs(1);
        socket = loop {
            std::thread::sleep(pause);
            if shared.quitting.load(Ordering::Acquire) {
                return;
            }
            match offer(shared) {
                Ok(Some(socket)) => break socket,
                Ok(None) => return,
                Err(e) => debug!("the session bus is not back yet: {e}"),
            }
            pause = (pause * 2).min(Duration::from_secs(60));
        };
        info!("local API on D-Bus as {BUS_NAME} again");
    }
}

/// A connection to the session bus, its socket connected here (and that socket,
/// to watch): zbus connecting by itself does it on a thread of its pool, which stays
/// and wakes twice a second for good, and an idle PC must not wake at all. Only a
/// bus address that is no unix socket is left to zbus (said once).
fn session() -> zbus::Result<(
    zbus::blocking::connection::Builder<'static>,
    Option<UnixStream>,
)> {
    use zbus::address::transport::{Transport, UnixSocket};
    let address = zbus::Address::session()?;
    if let Transport::Unix(unix) = address.transport() {
        let stream = match unix.path() {
            UnixSocket::File(path) => Some(UnixStream::connect(path)?),
            UnixSocket::Abstract(name) => {
                use std::os::linux::net::SocketAddrExt;
                use std::os::unix::ffi::OsStrExt;
                let at = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
                Some(UnixStream::connect_addr(&at)?)
            }
            _ => None,
        };
        if let Some(stream) = stream {
            let watch = stream.try_clone().ok();
            let builder = zbus::blocking::connection::Builder::async_io_unix_stream(stream);
            return Ok((builder, watch));
        }
    }
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        info!(
            "the session bus is not at a unix socket: zbus connects to it, and a thread of \
             its own then wakes twice a second while idle"
        );
    });
    Ok((zbus::blocking::connection::Builder::session()?, None))
}

/// The shortest time between two `StateChanged`.
const STATE_GAP: Duration = Duration::from_millis(1000 / STATES_PER_SECOND as u64);

/// Sends out what the control loop gives, in order, until it is gone.
fn send_out(shared: &Shared, notes: &Receiver<Note>) {
    let shown = &shared.shown;
    // A state not sent yet (it came too soon after the last one), what came after it
    // (held so the order stays), and when the last state went.
    let mut waiting: Option<u32> = None;
    let mut held: VecDeque<Note> = VecDeque::new();
    let mut last: Option<Instant> = None;
    let emit = |revision: u32, last: &mut Option<Instant>| {
        let json = shown.lock().unwrap().1.clone();
        signal(shared, "StateChanged", &(revision, json));
        *last = Some(Instant::now());
    };
    let deliver = |note: Note| match note {
        Note::Event(event) => headset_signal(shared, &event),
        Note::Answer(deliver) => deliver(shown.lock().unwrap().0),
        Note::State(..) => unreachable!("states are never held"),
    };
    loop {
        let note = match (waiting, last) {
            (Some(_), Some(at)) => {
                let left = (at + STATE_GAP).saturating_duration_since(Instant::now());
                match notes.recv_timeout(left) {
                    Ok(note) => Some(note),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            _ => match notes.recv() {
                Ok(note) => Some(note),
                Err(_) => return,
            },
        };
        let due = last.is_none_or(|at| at.elapsed() >= STATE_GAP);
        match note {
            // The gap is over: the state, then what waited behind it.
            None => {
                if let Some(revision) = waiting.take() {
                    emit(revision, &mut last);
                }
                held.drain(..).for_each(deliver);
            }
            Some(Note::State(revision, state)) => {
                *shown.lock().unwrap() = (revision, state.to_json());
                if due {
                    waiting = None;
                    emit(revision, &mut last);
                    held.drain(..).for_each(deliver);
                } else {
                    waiting = Some(revision);
                }
            }
            // Behind a state still waiting: it waits too, so a reply never goes before
            // the state with its change, and states stay ten a second at most.
            Some(note) if waiting.is_some() => held.push_back(note),
            Some(note) => deliver(note),
        }
    }
}

fn signal<B>(shared: &Shared, name: &str, body: &B)
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    // While the bus is away, nobody hears it.
    let Some(bus) = shared.bus() else {
        return;
    };
    if let Err(e) = bus.emit_signal(None::<()>, OBJECT_PATH, INTERFACE, name, body) {
        warn!("D-Bus signal {name}: {e}");
    }
}

fn headset_signal(bus: &Shared, event: &api::Event) {
    match event {
        api::Event::HeadsetConnected { headset, name, via } => signal(
            bus,
            "HeadsetConnected",
            &(headset.as_str(), name.as_str(), via.name()),
        ),
        api::Event::HeadsetDisconnected { headset, reason } => signal(
            bus,
            "HeadsetDisconnected",
            &(headset.as_str(), reason.name()),
        ),
        api::Event::HeadsetWorn { headset, worn } => {
            signal(bus, "HeadsetWorn", &(headset.as_str(), *worn));
        }
        // Only for reading what a newer Prostir sends; never sent.
        api::Event::Other => {}
    }
}

/// The D-Bus errors, one for each [`ApiError`]: `io.github.prostir_app.Prostir1.Error.*`.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.prostir_app.Prostir1.Error")]
enum BusError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Unsupported(String),
    NotAllowed(String),
    UnknownDisplay(String),
    Limit(String),
    BadSize(String),
    LastDisplay(String),
    NoWindow(String),
    Desktop(String),
    Encoder(String),
    Capture(String),
    Sound(String),
    NotOnDisplays(String),
    NoSuchProfile(String),
    BadArgument(String),
    NotConnected(String),
    TimedOut(String),
    Disconnected(String),
    Other(String),
}

impl From<Failure> for BusError {
    fn from(f: Failure) -> Self {
        let text = f.text;
        match f.error {
            ApiError::Unsupported => Self::Unsupported(text),
            ApiError::NotAllowed => Self::NotAllowed(text),
            ApiError::UnknownDisplay => Self::UnknownDisplay(text),
            ApiError::Limit => Self::Limit(text),
            ApiError::BadSize => Self::BadSize(text),
            ApiError::LastDisplay => Self::LastDisplay(text),
            ApiError::NoWindow => Self::NoWindow(text),
            ApiError::Desktop => Self::Desktop(text),
            ApiError::Encoder => Self::Encoder(text),
            ApiError::Capture => Self::Capture(text),
            ApiError::Sound => Self::Sound(text),
            ApiError::NotOnDisplays => Self::NotOnDisplays(text),
            ApiError::NoSuchProfile => Self::NoSuchProfile(text),
            ApiError::BadArgument => Self::BadArgument(text),
            ApiError::NotConnected => Self::NotConnected(text),
            ApiError::TimedOut => Self::TimedOut(text),
            ApiError::Disconnected => Self::Disconnected(text),
            ApiError::Other => Self::Other(text),
        }
    }
}

/// The object on the bus.
struct Object {
    shared: Arc<Shared>,
}

/// One more request waits for its answer while it lives.
struct Asking<'a>(&'a Shared);

impl<'a> Asking<'a> {
    fn new(shared: &'a Shared) -> Self {
        *shared.asking.lock().unwrap() += 1;
        Self(shared)
    }
}

impl Drop for Asking<'_> {
    fn drop(&mut self) {
        *self.0.asking.lock().unwrap() -= 1;
        self.0.answered.notify_all();
    }
}

/// Requests longer than this are refused unread.
const MAX_REQUEST: usize = 64 * 1024;

fn quitting() -> BusError {
    Failure::new(
        ApiError::Other,
        crate::i18n::t("pc-local-api-prostir-is-quitting"),
    )
    .into()
}

impl Object {
    /// Hands `request` to the control loop and waits for what came of it (asleep).
    async fn ask(&self, request: Request) -> Result<Answer, BusError> {
        let shared = &self.shared;
        if shared.quitting.load(Ordering::Acquire) {
            return Err(quitting());
        }
        let _asking = Asking::new(shared);
        let (tx, rx) = async_channel::bounded(1);
        let outlet = shared.outlet.clone();
        let reply = Reply::new(move |reply: CtlReply| {
            outlet.send(Note::Answer(Box::new(move |revision| {
                let _ = tx.try_send(reply.answer(revision));
            })));
        });
        if !shared.events.send(Event::Ctl(request, reply)) {
            return Err(quitting());
        }
        let answer = rx.recv().await.map_err(|_| quitting())?;
        match answer.failure() {
            Some(failure) => Err(failure.into()),
            None => Ok(answer),
        }
    }

    /// Asks and leaves out what the answer says.
    async fn done(&self, request: Request) -> Result<(), BusError> {
        self.ask(request).await.map(|_| ())
    }
}

#[zbus::interface(name = "io.github.prostir_app.Prostir1")]
impl Object {
    /// Any request, as JSON; the answer as JSON (a failure is a D-Bus error).
    async fn call(&self, request_json: String) -> Result<String, BusError> {
        if request_json.len() > MAX_REQUEST {
            return Err(Failure::new(ApiError::BadArgument, "a request over 64 KiB").into());
        }
        let request = Request::from_json(&request_json)?;
        Ok(self.ask(request).await?.to_json())
    }

    /// The whole state: its revision and its JSON.
    fn get_state(&self) -> (u32, String) {
        self.shared.shown.lock().unwrap().clone()
    }

    async fn recenter(&self) -> Result<(), BusError> {
        self.done(Request::Recenter).await
    }

    async fn line_up(&self) -> Result<(), BusError> {
        self.done(Request::LineUp).await
    }

    async fn show_mouse(&self) -> Result<(), BusError> {
        self.done(Request::ShowMouse).await
    }

    async fn reset_arrangement(&self) -> Result<(), BusError> {
        self.done(Request::ResetArrangement).await
    }

    async fn profile(&self, name: String) -> Result<(), BusError> {
        self.done(Request::Profile { name }).await
    }

    async fn rename_profile(&self, profile: String, name: String) -> Result<(), BusError> {
        self.done(Request::RenameProfile { profile, name }).await
    }

    async fn cinema(&self, on: bool) -> Result<(), BusError> {
        self.done(Request::Cinema { on }).await
    }

    async fn focus(&self, on: bool) -> Result<(), BusError> {
        self.done(Request::Focus { on }).await
    }

    async fn follow(&self, on: bool) -> Result<(), BusError> {
        self.done(Request::Follow { on }).await
    }

    async fn set_mic(&self, on: bool) -> Result<(), BusError> {
        self.done(Request::SetMic { on }).await
    }

    async fn no_limits(&self, on: bool) -> Result<(), BusError> {
        self.done(Request::NoLimits { on }).await
    }

    async fn preset(&self, preset: u8) -> Result<(), BusError> {
        self.done(Request::Preset { preset }).await
    }

    /// A virtual monitor: its id.
    async fn add_monitor(&self, w: u32, h: u32, scale_percent: u16) -> Result<u32, BusError> {
        let request = Request::AddMonitor {
            w,
            h,
            scale: scale_percent,
        };
        Ok(self.ask(request).await?.id.unwrap_or_default())
    }

    async fn remove_monitor(&self, id: u32) -> Result<(), BusError> {
        self.done(Request::RemoveMonitor { id }).await
    }

    async fn show_physical(&self, id: u32, shown: bool) -> Result<(), BusError> {
        self.done(Request::ShowPhysical { id, shown }).await
    }

    async fn set_quality(&self, mbit: u32) -> Result<(), BusError> {
        self.done(Request::SetQuality { mbit }).await
    }

    /// Opens pairing: the seconds it stays open.
    async fn pair_new_headset(&self) -> Result<u32, BusError> {
        Ok(self
            .ask(Request::PairNewHeadset)
            .await?
            .seconds_left
            .unwrap_or_default())
    }

    async fn stop_pairing(&self) -> Result<(), BusError> {
        self.done(Request::StopPairing).await
    }

    async fn forget_headset(&self, fingerprint_or_name: String) -> Result<(), BusError> {
        self.done(Request::ForgetHeadset {
            headset: fingerprint_or_name,
        })
        .await
    }

    async fn show_settings(&self) -> Result<(), BusError> {
        self.done(Request::ShowSettings).await
    }

    async fn quit(&self) -> Result<(), BusError> {
        self.done(Request::Quit).await
    }

    /// The PC app's version.
    #[zbus(property)]
    fn version(&self) -> String {
        prostir_proto::app_version(env!("CARGO_PKG_VERSION"))
    }

    /// The `v` of the state's JSON.
    #[zbus(property)]
    fn state_version(&self) -> u32 {
        STATE_VERSION
    }

    #[zbus(signal)]
    async fn state_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        revision: u32,
        state_json: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn headset_connected(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        headset: &str,
        name: &str,
        via: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn headset_disconnected(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        headset: &str,
        reason: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn headset_worn(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        headset: &str,
        worn: bool,
    ) -> zbus::Result<()>;
}

/// The client: `prostir ctl` and `prostir` on the session bus.
pub struct Client {
    bus: Connection,
}

impl Client {
    /// The session bus, with a reply waited for `timeout` at most.
    pub fn connect(timeout: Duration) -> Result<Self, ClientError> {
        session()
            .map(|(b, _)| b.method_timeout(timeout))
            .and_then(zbus::blocking::connection::Builder::build)
            .map(|bus| Self { bus })
            .map_err(|e| ClientError::NoBus(e.to_string()))
    }

    fn method(&self, name: &str, body: &impl SerializeBody) -> Result<zbus::Message, ClientError> {
        body.call(&self.bus, name).map_err(client_error)
    }
}

/// A method call's body (a helper so `method` takes any tuple).
trait SerializeBody {
    fn call(&self, bus: &Connection, name: &str) -> zbus::Result<zbus::Message>;
}

impl<B: serde::Serialize + zbus::zvariant::DynamicType> SerializeBody for B {
    fn call(&self, bus: &Connection, name: &str) -> zbus::Result<zbus::Message> {
        bus.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), name, self)
    }
}

/// A bus error as a client understands it.
fn client_error(e: zbus::Error) -> ClientError {
    match e {
        zbus::Error::MethodError(name, text, _) => {
            let name = name.as_str();
            let text = text.unwrap_or_default();
            if let Some(error) = name.strip_prefix(ERROR_PREFIX) {
                return ClientError::Refused(Failure::new(ApiError::from_name(error), text));
            }
            match name {
                "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
                | "org.freedesktop.DBus.Error.UnknownObject" => ClientError::NotRunning,
                "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
                    ClientError::TimedOut
                }
                _ => ClientError::Broken(format!("{name}: {text}")),
            }
        }
        zbus::Error::InputOutput(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            ClientError::TimedOut
        }
        e => ClientError::Broken(e.to_string()),
    }
}

fn broken(e: impl std::fmt::Display) -> ClientError {
    ClientError::Broken(e.to_string())
}

/// The sockets' watcher of this user while it sleeps: its process id. One whose full
/// program runs is not asleep, also when that program is on another session bus than
/// this one (another session's): it is not quit from here.
fn sleeping_watcher() -> Option<u32> {
    let pid = crate::platform::watcher()?;
    let children =
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    children.trim().is_empty().then_some(pid)
}

impl CtlClient for Client {
    fn call(&self, request: &Request) -> Result<Answer, ClientError> {
        let msg = self.method("Call", &(request.to_json(),))?;
        let json: String = msg.body().deserialize().map_err(broken)?;
        serde_json::from_str(&json).map_err(broken)
    }

    fn state(&self) -> Result<(u32, String), ClientError> {
        let msg = self.method("GetState", &())?;
        msg.body().deserialize().map_err(broken)
    }

    fn running(&self) -> bool {
        let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&self.bus) else {
            return false;
        };
        dbus.name_has_owner(BUS_NAME.try_into().expect("a valid name"))
            .unwrap_or(false)
    }

    fn asleep(&self) -> bool {
        sleeping_watcher().is_some() || crate::platform::autostart::socket_waits()
    }

    fn quit_asleep(&self) -> bool {
        if let Some(pid) = sleeping_watcher() {
            // SAFETY: a signal to a Prostir of this user, which quits on it (and ends
            // the full program it started, if one starts meanwhile).
            return unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0;
        }
        if crate::platform::autostart::socket_waits() {
            crate::platform::autostart::quit_until_login();
            return true;
        }
        false
    }

    fn port_taken(&self) -> bool {
        // The tests' fake PC never uses the real port, whatever runs on this machine.
        if cfg!(feature = "fake-desktop") {
            return false;
        }
        let port = (std::net::Ipv4Addr::UNSPECIFIED, prostir_proto::TCP_PORT);
        std::net::TcpListener::bind(port).is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse)
    }

    fn watch(&self, each: &mut dyn FnMut(Watched) -> bool) -> Result<(), ClientError> {
        use zbus::message::Type;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.bus).map_err(broken)?;
        let ours = zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(INTERFACE)
            .and_then(|b| b.path(OBJECT_PATH))
            .map_err(broken)?
            .build();
        let owner = zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")
            .and_then(|b| b.interface("org.freedesktop.DBus"))
            .and_then(|b| b.member("NameOwnerChanged"))
            .and_then(|b| b.arg(0, BUS_NAME))
            .map_err(broken)?
            .build();
        let messages = zbus::blocking::MessageIterator::from(&self.bus);
        dbus.add_match_rule(ours).map_err(broken)?;
        dbus.add_match_rule(owner).map_err(broken)?;
        // Who has the name: only its signals are Prostir's (any program may send a
        // signal of this interface).
        let mut prostir: Option<String> = dbus
            .get_name_owner(BUS_NAME.try_into().expect("a valid name"))
            .ok()
            .map(|o| o.to_string());
        for msg in messages {
            let msg = msg.map_err(broken)?;
            let header = msg.header();
            if header.message_type() != Type::Signal {
                continue;
            }
            let member = header.member().map(ToString::to_string).unwrap_or_default();
            let sender = header.sender().map(ToString::to_string);
            let body = msg.body();
            if member == "NameOwnerChanged" {
                let Ok((name, _, new)) = body.deserialize::<(String, String, String)>() else {
                    continue;
                };
                if name != BUS_NAME || sender.as_deref() != Some("org.freedesktop.DBus") {
                    continue;
                }
                let seen = if new.is_empty() {
                    prostir = None;
                    each(Watched::Vanished)
                } else {
                    prostir = Some(new);
                    // What it shows from the start: it may say nothing for a while.
                    each(Watched::Appeared)
                        && self.state().map_or(true, |(revision, json)| {
                            each(Watched::State(revision, json))
                        })
                };
                if !seen {
                    return Ok(());
                }
                continue;
            }
            if prostir.is_none() || sender != prostir {
                continue;
            }
            let seen = match member.as_str() {
                "StateChanged" => body
                    .deserialize::<(u32, String)>()
                    .ok()
                    .map(|(revision, json)| Watched::State(revision, json)),
                "HeadsetConnected" => body.deserialize::<(String, String, String)>().ok().map(
                    |(headset, name, via)| {
                        Watched::Event(api::Event::HeadsetConnected {
                            headset,
                            name,
                            via: match via.as_str() {
                                "usb" => api::Via::Usb,
                                "wifi" => api::Via::Wifi,
                                _ => api::Via::Other,
                            },
                        })
                    },
                ),
                "HeadsetDisconnected" => {
                    body.deserialize::<(String, String)>()
                        .ok()
                        .map(|(headset, reason)| {
                            Watched::Event(api::Event::HeadsetDisconnected {
                                headset,
                                reason: api::Reason::from_name(&reason),
                            })
                        })
                }
                "HeadsetWorn" => {
                    body.deserialize::<(String, bool)>()
                        .ok()
                        .map(|(headset, worn)| {
                            Watched::Event(api::Event::HeadsetWorn { headset, worn })
                        })
                }
                _ => None,
            };
            if let Some(seen) = seen
                && !each(seen)
            {
                return Ok(());
            }
        }
        Ok(())
    }
}
