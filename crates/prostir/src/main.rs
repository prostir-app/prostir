// Every unsafe block says why it is sound.
#![deny(clippy::undocumented_unsafe_blocks)]
mod api;
mod brain;
mod cli;
mod control;
mod ctl;
mod cursor;
mod deskside;
mod displays;
mod door;
mod encoder;
mod errors;
mod events;
mod find;
mod i18n;
mod input;
mod layout;
mod listen;
mod local;
mod login;
mod mcp;
mod outbox;
mod pacing;
mod pairing;
mod platform;
mod publish;
mod requests;
mod router;
mod screen;
mod session;
mod sessions;
mod store;
mod usage;
mod wake;
mod worker;

use std::collections::{BTreeMap, VecDeque};
use std::io::ErrorKind;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use log::{error, info, warn};
use prostir_proto::request::{Asked, Command};
use prostir_proto::state::ErrorCode;
use prostir_proto::{ClientMsg, Codec, HostMsg, PcGoodbye};

use brain::{Admit, LastEnded, Phase};
use cli::{Args, Cli, CodecArg, Sub};
use control::Control;
use displays::Displays;
use events::{Accepted, Event, EventSink, SessionId};
use platform::MonitorSpec;
use platform::sound::SoundEvent;
use platform::{TrayCommand, TrayStatus, tray};
use screen::ScreenId;
use session::{Closing, Session, Standing, TrayLine};
use store::Store;

/// Keeps the set and its rows as they are now (`chosen`: the headset changed which
/// monitors there are).
pub fn keep_screens(store: &mut Store, displays: &Displays, chosen: bool) {
    if let Some(set) = displays.saved_screens() {
        store.set_screens(set, chosen);
    }
}

/// While a Quest on USB waits for its `adb reverse`, or a worker puts things back, how
/// often the program looks whether it may leave.
const LOOK_AGAIN: Duration = Duration::from_secs(2);

/// Taken off for this long (awake: it may be put on again soon), the headset counts
/// as gone.
const TAKEN_OFF_GRACE: Duration = Duration::from_secs(5);
/// Asleep for this long (it said `Asleep`: its power button), the headset counts as
/// gone: about a second, so a tap that wakes it again at once keeps the session.
const ASLEEP_GRACE: Duration = Duration::from_secs(1);

/// Events handled one after the other before the next pass of the control loop, at
/// most for this long: what a pass costs never adds up behind a stream of small
/// messages (the microphone sends a hundred a second), and the clocks still run.
const TAKE_AT_ONCE: Duration = Duration::from_millis(50);
/// A pass of the control loop that takes this long is written in the log: the headset
/// waits for it.
const BUSY: Duration = Duration::from_millis(150);

/// The control loop: the one owner of the PC's state (settings, the monitors, the
/// sessions), acting on the events every other thread sends and on its own deadlines.
struct Brain {
    /// How long the monitors stay after the last headset leaves.
    linger: Duration,
    /// Make the middle monitor the desktop's primary one.
    set_primary: bool,
    store: Store,
    control: Control,
    security: Arc<pairing::Security>,
    /// For the sessions' readers.
    events: EventSink,
    displays: Option<Displays>,
    /// The sessions, by the headset's key: one each, at most `brain::max_headsets()`.
    sessions: BTreeMap<[u8; 32], Session>,
    /// Which headsets' sessions have the jobs only one can have (see `brain::roles`).
    roles: brain::Roles<[u8; 32]>,
    /// The order the headsets with a session first connected in.
    arrivals: brain::Arrivals<[u8; 32]>,
    /// Sessions that said goodbye, until their headset closes or their moment ends.
    closing: Vec<Closing>,
    phase: Phase,
    last_id: SessionId,
    /// The output that has the mouse, as last looked at.
    focus: Option<ScreenId>,
    quit: bool,
    /// The local API: where the state, events and replies go out.
    api: api::Outlet,
    /// The state as last sent out, and its revision.
    shown: Option<Arc<api::State>>,
    revision: u32,
    /// Replies of the local API that go out after this pass's state.
    answers: api::Answers,
    /// Whether each headset is worn, as last told on the local API.
    worn_told: BTreeMap<[u8; 32], bool>,
    /// The pairing window, the headsets nearby, the settings window, and when the
    /// program leaves (see `door`).
    door: door::Door,
    /// The connections being set up, and accepting them.
    listening: listen::Listening,
    /// The settings window typed a code a headset shows: answered once the pairing
    /// attempt ends.
    code_reply: Option<api::Reply>,
    /// Screenshots asked for on the local API, waiting for the desktop's worker.
    shots: local::Shots,
    /// The person asked Prostir to quit (the tray, the local API), not a signal.
    quit_asked: bool,
    /// Connections whose session opens once the desktop's worker connected.
    waiting_desk: Vec<Accepted>,
    /// The desktop did not respond for too long: the program ends with an error.
    desk_gave_up: bool,
    /// Why the desktop cannot be used and what to do about it (see
    /// [`platform::Unusable`]), in the PC's language, from the last try to connect that
    /// failed so, until one succeeds.
    desk_unusable: Option<String>,
    /// Desktop workers let go, until each is gone (see `deskside.rs`).
    retiring: Vec<worker::Retired>,
    /// The PC's Wi-Fi, as found when a headset last connected over the network.
    wifi: Option<api::Wifi>,
}

impl Brain {
    /// Waits for events and deadlines until asked to quit (or, with sockets from
    /// outside, until nothing needs it), then ends every session with a goodbye and
    /// puts everything back. Whether the program is to end at once with an error (the
    /// desktop did not respond), or without waiting for anything more (the desktop's
    /// worker did not finish putting everything back): its exit code then; and
    /// whether the person asked Prostir to quit.
    fn run(mut self, events: &Receiver<Event>) -> (Option<i32>, bool) {
        loop {
            let next = brain::earliest([
                self.phase.due(),
                self.closing.iter().map(|c| c.until).min(),
                self.sessions.values().filter_map(Session::volume_at).min(),
                // A headset off the head counts as gone at its time, not at the next
                // housekeeping after it.
                self.sessions.values().filter_map(gone_at).min(),
                self.desk_due(),
                // A still screen's keyframe on time: the one that sharpens a coarse
                // picture is due 0.65 s after it, not at the next housekeeping.
                self.displays
                    .as_ref()
                    .filter(|_| !self.sessions.is_empty())
                    .and_then(Displays::still_due),
                // A monitor the gaze left gets fewer frames 2 s later, on time.
                self.displays
                    .as_ref()
                    .filter(|_| !self.sessions.is_empty())
                    .and_then(|d| d.router.priority_due()),
                self.door.due(),
                // Looked at again now and then, by a program that leaves when idle,
                // while a Quest on USB waits for its `adb reverse` or, with nothing
                // else to do, a worker puts things back (only then).
                (self.door.leaves()
                    && (platform::usb_waiting()
                        || (matches!(self.phase, Phase::Idle) && self.workers_busy())))
                .then(|| Instant::now() + LOOK_AGAIN),
            ]);
            let first = match next {
                // Nothing to do until something happens: no wakeups at all.
                None => events.recv().ok(),
                Some(at) => events
                    .recv_timeout(at.saturating_duration_since(Instant::now()))
                    .ok(),
            };
            if let Some(event) = first {
                self.handle(event);
                // Every event waiting is taken before the next pass, for a short while
                // at most.
                let until = Instant::now() + TAKE_AT_ONCE;
                while !self.quit && Instant::now() < until {
                    match events.try_recv() {
                        Ok(event) => self.handle(event),
                        Err(_) => break,
                    }
                }
            }
            if self.quit {
                break;
            }
            self.pass();
            if self.leaves_now() {
                // Nothing may slip in as it leaves: a connection accepted meanwhile,
                // or an event already sent, keeps it.
                self.listening.stop();
                // In this order: a connection set up meanwhile sends its event before
                // it stops counting as busy, so a look at the events after it misses
                // nothing.
                let busy = self.listening.busy();
                let event = events.try_recv();
                if event.is_err() && !busy {
                    info!("nothing needs Prostir now: leaving until a headset comes");
                    break;
                }
                if let Err(e) = self.listening.resume() {
                    error!("cannot accept connections again: {e}");
                    self.quit = true;
                }
                if let Ok(event) = event {
                    self.handle(event);
                }
                if self.quit {
                    break;
                }
                self.pass();
            }
        }
        let asked = self.quit_asked;
        (self.shut_down(events), asked)
    }

    /// Whether the desktop's worker or the sound thread still has work: a session
    /// waiting for the desktop, a worker putting things back, the sound going back.
    fn workers_busy(&self) -> bool {
        !self.waiting_desk.is_empty()
            || !self.retiring.is_empty()
            || !worker::wait_gone(Duration::ZERO)
            || !worker::sound::wait_gone(Duration::ZERO)
    }

    /// Whether the program leaves now, its sockets having come from outside (see
    /// `door`).
    fn leaves_now(&mut self) -> bool {
        let now = Instant::now();
        let busy = !self.sessions.is_empty()
            || !matches!(self.phase, Phase::Idle)
            || !self.closing.is_empty()
            || self.listening.busy()
            || self.code_reply.is_some()
            || pairing::code_shown().is_some()
            || self.workers_busy()
            // A Quest on USB waits for its `adb reverse` (USB debugging not allowed yet,
            // say): the program it starts must not leave before it connects.
            || platform::usb_waiting();
        self.door.leave_at(now, busy).is_some_and(|at| now >= at)
    }

    /// One event.
    fn handle(&mut self, event: Event) {
        match event {
            Event::Accepted(accepted) => self.accepted(*accepted),
            Event::Headset(id, came, msg) => {
                let Some(key) = self.key_of(id) else {
                    // From a session that said goodbye or gave way: read and dropped.
                    return;
                };
                if let ClientMsg::Goodbye { reason } = msg {
                    info!("{}", session::headset_left(reason));
                    let why = match prostir_proto::HeadsetGoodbye::from_code(reason) {
                        prostir_proto::HeadsetGoodbye::Quit => api::Reason::Quit,
                        prostir_proto::HeadsetGoodbye::Forgotten => api::Reason::Forgotten,
                        _ => api::Reason::Other,
                    };
                    return self.end_session(key, None, why, Ok(()));
                }
                let (Some(session), Some(displays)) =
                    (self.sessions.get_mut(&key), self.displays.as_mut())
                else {
                    return;
                };
                if session.handle(msg, came, displays, &mut self.store) {
                    self.headsets_changed();
                }
            }
            Event::SessionEnded(id, e) => {
                if let Some(i) = self.closing.iter().position(|c| c.id == id) {
                    // The headset read the goodbye and closed.
                    return self.closing.swap_remove(i).done();
                }
                let Some(key) = self.key_of(id) else {
                    return;
                };
                let why = match e.kind() {
                    ErrorKind::TimedOut => api::Reason::Silence,
                    _ => api::Reason::Other,
                };
                let result = match e.kind() {
                    ErrorKind::UnexpectedEof => Ok(()),
                    ErrorKind::TimedOut => Err(anyhow::anyhow!(
                        "silence: nothing from the headset for {} s",
                        session::SILENCE.as_secs()
                    )),
                    _ => Err(e.into()),
                };
                self.end_session(key, None, why, result);
            }
            Event::Sound(sound) => self.sound(sound),
            Event::Tray(command) => self.tray(command),
            Event::Ctl(request, reply) => self.ctl(request, reply),
            Event::Pairing => {
                // The headset whose code the settings window typed went: its answer.
                if pairing::code_for().is_none() {
                    self.code_tried();
                }
            }
            Event::Paired { name } => {
                info!("a headset paired: the pairing window closes");
                self.door.paired(name);
                self.code_tried();
            }
            Event::WrongCode => {
                self.door.wrong_code();
                self.code_tried();
            }
            Event::Asked(asker) => {
                if self.door.asked(&asker, Instant::now())
                    && !self.door.notice_waits()
                    && door::notice_due(&platform::notice_mark(), std::time::SystemTime::now())
                {
                    self.notice_new_headset();
                }
            }
            Event::Notice(true) => {
                self.door.open_pairing(Instant::now(), door::pairing_time());
                info!("pairing a new headset: from the notice");
            }
            Event::Notice(false) => self.door.notice_up(false, Instant::now()),
            Event::Window(on) => {
                info!(
                    "the settings window {} the session bus",
                    if on { "is on" } else { "left" }
                );
                self.door.window(on);
                pairing::window_types_codes(on);
            }
            Event::Desk(generation, done) => self.desk(generation, done),
            Event::SoundDevices(generation, done) => self.sound_devices(generation, done),
            Event::Input(news) => self.input_news(news),
            Event::StillPicture(id) => {
                if let Some(d) = &mut self.displays {
                    d.fresh_picture(id);
                }
            }
            Event::Quit => self.quit = true,
        }
    }

    /// The input from the headset was not allowed or does not open: said to the
    /// headsets that may use it.
    fn input_news(&self, news: input::News) {
        let (error, detail) = match news {
            input::News::Refused => (ErrorCode::NOT_ALLOWED, i18n::t("pc-input-not-allowed")),
            input::News::Unavailable => (ErrorCode::DESKTOP, i18n::t("pc-input-unavailable")),
        };
        for s in self.sessions.values() {
            if s.may(prostir_proto::state::perms::INPUT) {
                s.send(HostMsg::Problem {
                    error,
                    detail: detail.into(),
                    display: None,
                });
            }
        }
    }

    /// What the sound system said: the device's level goes to the session that shares
    /// the PC's volume; a program recording or not, to the microphone.
    fn sound(&mut self, event: SoundEvent) {
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        match event {
            SoundEvent::Volume {
                level,
                muted,
                at,
                writes,
                new_device,
            } => {
                for s in self.sessions.values_mut() {
                    s.device_volume(level, muted, at, writes, new_device);
                }
            }
            SoundEvent::Recording(on) => {
                displays.set_recording(on);
                // The microphone may be free to go to the main headset now.
                self.headsets_changed();
            }
        }
    }

    /// What changed since the last pass goes out: to the headsets, the desktop, the
    /// tray and the files. Live, the clocks' work too.
    fn pass(&mut self) {
        let now = Instant::now();
        // Sessions that said goodbye and whose headset did not close in time.
        let (over, waiting) = std::mem::take(&mut self.closing)
            .into_iter()
            .partition(|c| c.until <= now);
        self.closing = waiting;
        over.into_iter().for_each(Closing::done);
        if self.phase.expire(now) {
            info!("the headset did not come back: removing virtual monitors");
            self.drop_displays();
        }
        self.door.expire(now);
        self.desk_answers_due(now);
        self.end_taken_off(now);
        if self.sessions.is_empty() {
            if let Some(d) = self.displays.as_mut() {
                d.push();
            }
        } else {
            self.live_pass(now);
        }
        self.look_at_desk(now);
        self.show();
        self.store.flush();
        self.publish_api();
    }

    /// A pass with sessions: the headsets told what changed (the replies after it), the
    /// state wanted of the desktop handed to its worker (which can take it a while: the
    /// headsets know already); the clocks' work.
    fn live_pass(&mut self, now: Instant) {
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        let started = Instant::now();
        for s in self.sessions.values_mut() {
            s.volume_due();
            s.publish(displays, &self.store, self.focus);
        }
        displays.still_keyframes();
        displays.router.priorities();
        displays.push();
        self.store.flush();
        let busy = started.elapsed();
        if busy >= BUSY {
            info!(
                "the control loop was busy for {} ms telling the headset",
                busy.as_millis()
            );
        }
        // Commands for the headset go to the main one, else the first.
        let mut waiting = std::mem::take(&mut self.control.for_headset);
        if let Some(s) = self.lead_mut() {
            s.commands_due(&mut waiting);
        }
        self.control.for_headset = waiting;
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        let Phase::Live(clocks) = &mut self.phase else {
            return;
        };
        if clocks.focus(now) {
            self.focus = displays.active_output();
            // The active monitor (the mouse or the focused window) gets every frame.
            displays.router.set_active(self.focus);
            displays.look_at_focus();
            for s in self.sessions.values_mut() {
                s.look_at_sound(displays);
            }
        }
        if !clocks.housekeeping(now) {
            return;
        }
        displays.tick();
        let healthy = displays.healthy();
        // The microphone may be free to go to the main headset now.
        self.headsets_changed();
        self.show();
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        if !healthy && !self.sessions.is_empty() {
            // Why no monitor could start, where none could; else one that stopped.
            let why = displays
                .why_none_started()
                .unwrap_or_else(|| "a virtual monitor or encoder stopped on the PC".into());
            warn!("{why}");
            displays.router.send_all(&HostMsg::Problem {
                error: ErrorCode::ENCODER,
                detail: prostir_proto::state::clip(&why, prostir_proto::state::MAX_DETAIL),
                display: None,
            });
            let keys: Vec<[u8; 32]> = self.sessions.keys().copied().collect();
            self.end_sessions(keys, PcGoodbye::Failed, api::Reason::Other, || {
                Err(anyhow::anyhow!("virtual monitor or encoder stopped"))
            });
        }
    }

    /// Each headset off the head for long enough (see [`gone_at`]) counts as gone: its
    /// session ends; the others go on.
    fn end_taken_off(&mut self, now: Instant) {
        let taken_off: Vec<[u8; 32]> = self
            .sessions
            .iter()
            .filter(|(_, s)| gone_at(s).is_some_and(|t| t <= now))
            .map(|(key, _)| *key)
            .collect();
        if taken_off.is_empty() {
            return;
        }
        let asleep = taken_off
            .iter()
            .any(|k| self.sessions.get(k).is_some_and(Session::asleep));
        info!("headset {}", if asleep { "asleep" } else { "taken off" });
        self.end_sessions(
            taken_off,
            PcGoodbye::TakenOff,
            api::Reason::TakenOff,
            || Ok(()),
        );
    }

    /// Asked to quit: every session ends with a goodbye, which each headset gets a
    /// moment to read; the monitors go, the files are written. Whether to end at once,
    /// and with which code (see [`Brain::run`]).
    fn shut_down(mut self, events: &Receiver<Event>) -> Option<i32> {
        let keys: Vec<[u8; 32]> = self.sessions.keys().copied().collect();
        let goodbye = if self.desk_gave_up {
            PcGoodbye::Failed
        } else {
            PcGoodbye::Quit
        };
        self.end_sessions(keys, goodbye, api::Reason::Quit, || Ok(()));
        info!("quitting");
        // Whoever asked to quit hears it was done.
        self.publish_api();
        while let Some(until) = self.closing.iter().map(|c| c.until).max() {
            let left = until.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(Event::SessionEnded(id, _)) => {
                    if let Some(i) = self.closing.iter().position(|c| c.id == id) {
                        self.closing.swap_remove(i).done();
                    }
                }
                // What else comes is too late now.
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.closing.drain(..).for_each(Closing::done);
        if let Some(d) = self.displays.as_mut() {
            d.note_connected();
            keep_screens(&mut self.store, d, false);
        }
        drop(self.displays.take());
        self.store.close();
        if self.desk_gave_up {
            return Some(1);
        }
        // The desktop's worker puts the monitors and windows back, the sound thread the
        // sound.
        let until = Instant::now() + DESK_GONE;
        let sound_back = worker::sound::wait_gone(until.saturating_duration_since(Instant::now()));
        if !sound_back {
            warn!("the sound did not go back to the PC in time");
        }
        if !worker::wait_gone(until.saturating_duration_since(Instant::now())) {
            warn!("the desktop did not take the monitors back in time: quitting without them");
            return Some(0);
        }
        None
    }
}

/// When the headset of `session`, off the head, counts as gone: after the grace of
/// one taken off while awake, or the short one of one asleep (its power button).
fn gone_at(session: &Session) -> Option<Instant> {
    let grace = if session.asleep() {
        ASLEEP_GRACE
    } else {
        TAKEN_OFF_GRACE
    };
    session.taken_off_at().map(|t| t + grace)
}

/// How long quitting waits for the desktop's worker to put everything back: with the
/// headsets' moment to read the goodbye, within the time a quit signal gives
/// (`signals.rs`).
const DESK_GONE: Duration = Duration::from_secs(8);

/// Tells a headset why it gets no session, instead of a snapshot, and closes its
/// connection gently (on a thread of its own: that takes a moment).
fn turn_away(mut conn: pairing::Connection, text: String) {
    let spawned = std::thread::Builder::new()
        .name("turn-away".into())
        .spawn(move || {
            let _ = conn.send(HostMsg::Error { text });
            pairing::close_gently(&mut conn.link);
        });
    if let Err(e) = spawned {
        warn!("cannot tell the headset why: {e}");
    }
}

/// The screens of the saved set; the desktop's worker starts connecting.
fn create_displays(set_primary: bool, store: &mut Store, events: &EventSink) -> Displays {
    let set = store.screens().clone();
    let displays = Displays::create(&set, &store.wanted(), set_primary, events);
    keep_screens(store, &displays, false);
    displays
}

/// Seconds since 1970, now.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The log filter unless `RUST_LOG` says otherwise: Prostir's own lines from `level`
/// on, other crates' only from warnings (zbus logs every call of the local API at
/// info).
fn log_filter(level: &str) -> String {
    format!("warn,prostir={level}")
}

/// Starts the log with Prostir's own lines from `level` on (see [`log_filter`]), each
/// with its time to the millisecond: a session starts within a second, and its steps
/// are only told apart so.
fn init_log(level: &str) {
    let env = env_logger::Env::default().default_filter_or(log_filter(level));
    env_logger::Builder::from_env(env)
        .format_timestamp_millis()
        .init();
}

/// `Prostir 0.2.0` (with its message revision in a development build), as `--version`
/// prints it and the log starts with it.
fn version_line() -> String {
    format!(
        "Prostir {}",
        prostir_proto::app_version(env!("CARGO_PKG_VERSION"))
    )
}

/// `prostir ctl -v` and `prostir ctl --version`, which the commands of `ctl` take whole.
fn asks_version(cli: &Cli) -> bool {
    cli.version
        || matches!(&cli.command, Some(Sub::Ctl { args })
            if args.first().is_some_and(|a| a == "-v" || a == "--version"))
}

fn main() -> Result<()> {
    // How it was started, before anything changes the environment.
    let signs = login::Signs::take();
    // This program's file, found now: a package update may replace it while it runs.
    let _ = prostir_window::launch::this_program();
    let cli = Cli::read();
    if asks_version(&cli) {
        println!("{}", version_line());
        return Ok(());
    }
    let quiet = || init_log("warn");
    let code = match cli.command {
        None => {
            // An autostart entry of 0.1 (`Exec=prostir`) runs `prostir serve` from the
            // next login on.
            let moved = platform::autostart::migrate();
            match signs.login() {
                _ if cli.old_options => serve(cli.serve, Why::OldOptions, moved)?,
                // 0.1's autostart at this login: the background part, not the window.
                Some(by) => serve(cli.serve, Why::Login(by), moved)?,
                None => {
                    quiet();
                    open_window()
                }
            }
        }
        Some(Sub::Serve(args)) => {
            let moved = platform::autostart::migrate();
            serve(args, Why::Asked, moved)?
        }
        Some(Sub::Window { mock }) => match prostir_window::run(mock) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("prostir window: {e}");
                1
            }
        },
        Some(Sub::Ctl { args }) => {
            quiet();
            let client = platform::local_api::Client::connect(ctl::WAIT);
            let client = client.as_ref().map(|c| -> &dyn ctl::CtlClient { c });
            let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
            let mut io = ctl::Output {
                out: &mut out,
                err: &mut err,
            };
            ctl::run(&args, client.map_err(Clone::clone), &mut io)
        }
        Some(Sub::Mcp) => {
            quiet();
            mcp::run()
        }
    };
    std::process::exit(code);
}

/// `prostir` (the menu entry): the settings window, from the running Prostir, which is
/// woken or started first when it is not running. Its exit code.
fn open_window() -> i32 {
    // Prostir runs (not asleep in its watcher): its window, and nothing is started.
    if let Some(pid) = platform::serving() {
        return open_the_running_one(Some(pid), true);
    }
    match platform::local_api::Client::connect(ctl::WAIT) {
        Ok(client) => ctl::open(&client, &local::wake_for_window),
        Err(e) => {
            eprintln!("prostir: no session bus ({e:?})");
            ctl::NOT_RUNNING
        }
    }
}

/// Where the program's sockets came from, which says how it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// Opened here (`--stay`): it runs until asked to quit.
    Own,
    /// From the sockets' watcher, its parent: it leaves when nothing needs it, and
    /// tells the watcher when the person asked Prostir to quit.
    Watcher,
    /// From systemd (`prostir.socket`): it leaves when nothing needs it, and stops the
    /// socket when the person asked Prostir to quit.
    Systemd,
}

/// What started `serve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    /// `prostir serve`.
    Asked,
    /// `serve`'s options without a command, as 0.1 took them (`prostir -m 2560x1440`).
    OldOptions,
    /// A bare `prostir` started with the session, as 0.1's autostart started it.
    Login(login::Login),
}

/// `prostir serve`: waits for the headset and serves it; `moved`: an autostart entry
/// of 0.1 was changed to `prostir serve` just now. Its exit code.
fn serve(args: Args, why: Why, moved: bool) -> Result<i32> {
    // Before any thread starts: it clears the variables that passed them.
    let passed = platform::sockets::take()?;
    init_log("info");
    if moved {
        info!("the autostart entry of 0.1 runs `prostir serve` from now on");
    }
    match why {
        Why::Asked => {}
        Why::OldOptions => warn!(
            "options without a command are 0.1's way: this runs as `prostir serve` with them; \
             write `prostir serve ...`, a later version takes them only so"
        ),
        Why::Login(by) => {
            info!(
                "a bare `prostir` started with the session ({by:?}), as 0.1's autostart \
                 starts it: this runs as `prostir serve`, without the window"
            );
            // Hyprland's config is the person's own: what to change is only said.
            if by == login::Login::Hyprland {
                platform::dialogs::notify(
                    i18n::t("pc-main-prostir-in-the-hyprland-config"),
                    i18n::t("pc-main-change-exec-once-prostir-to-exec"),
                );
            }
        }
    }
    let discovery = (!args.no_discovery).then_some(args.discovery_port);
    match passed {
        Some(mut sockets) => {
            if args.no_discovery {
                sockets.udp = None;
            }
            if args.woken {
                return full(&args, sockets, Origin::Watcher, None);
            }
            // systemd started it for what came: sorted first, light.
            match wake::sort(&sockets)? {
                Some(sorted) => {
                    platform::import_session_environment();
                    sockets.first = sorted.first;
                    full(&args, sockets, Origin::Systemd, Some(sorted.why))
                }
                None => {
                    info!("nothing for Prostir to do: back to sleep");
                    Ok(0)
                }
            }
        }
        None if args.stay => {
            let args = window_at_first_start(args);
            let instance = match platform::single_instance("serve") {
                Err(e) if platform::runs_already(&e) => {
                    return Ok(open_the_running_one(platform::held_by(&e), false));
                }
                taken => taken?,
            };
            the_one_prostir();
            let sockets = open_sockets(&args, discovery)?;
            full_with(&args, sockets, Origin::Own, None, Some(instance))
        }
        None => {
            let args = window_at_first_start(args);
            let instance = match platform::single_instance("watcher") {
                Err(e) if platform::runs_already(&e) => {
                    return Ok(open_the_running_one(platform::held_by(&e), false));
                }
                taken => taken?,
            };
            let sockets = match open_sockets(&args, discovery) {
                // systemd's socket waits for the headset: Prostir is there, asleep.
                Err(_) if platform::autostart::socket_waits() => {
                    drop(instance);
                    return Ok(open_the_running_one(None, false));
                }
                opened => opened?,
            };
            let _instance = instance;
            the_one_prostir();
            if !args.no_adb {
                platform::keep_adb_reverse(sockets.port());
            }
            let code = wake::watch(sockets, wake::Start::this_program()?, args.open_settings);
            // Nothing listens on the port any more.
            platform::drop_adb_reverse();
            code
        }
    }
}

/// The settings window opens at the very first start (the menu, a terminal): Prostir
/// shows it is there. A start at login is never the first, which turns autostart on,
/// so it stays silent.
fn window_at_first_start(mut args: Args) -> Args {
    if !args.open_settings && store::config_dir().is_some_and(|d| store::first_start(&d)) {
        info!("the first start: the settings window opens");
        args.open_settings = true;
    }
    args
}

/// Whether the first start of this version turns starting with the session on: yes on
/// a new install, and where it is not set from here (Hyprland's config, which the
/// notification of [`platform::autostart::first_start`] is about). After an update
/// from 0.1 (`ran_before`), which kept no choice of its own, as 0.1 had it: on when
/// its autostart entry or systemd's socket is there (`enabled`), else off for good.
fn autostart_at_first_start(ran_before: bool, available: bool, enabled: bool) -> bool {
    !(ran_before && available && !enabled)
}

/// The service started again while it runs (`prostir serve` by hand, or an autostart
/// line while the menu started it): the running one (`pid`, where known) opens its
/// settings window, or brings it forward, as `prostir` does, and this one ends. Its
/// exit code. Nothing is started: a Prostir that runs but is not on this session bus
/// is only named, also in a notification from the menu (`menu`), where no terminal
/// shows what went wrong.
fn open_the_running_one(pid: Option<u32>, menu: bool) -> i32 {
    use ctl::CtlClient as _;
    info!("Prostir runs already: its settings window opens");
    let client = match platform::local_api::Client::connect(ctl::WAIT) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("prostir: no session bus ({e:?})");
            return ctl::NOT_RUNNING;
        }
    };
    // One that runs, not asleep in its watcher, may be starting: a moment for it to
    // come on the bus.
    if pid.is_some() && platform::watcher().is_none() {
        let until = Instant::now() + ON_THE_BUS_SOON;
        while !client.running() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let code = ctl::open(&client, &|| local::wake_running(pid));
    if menu && code == ctl::NOT_RUNNING && !client.running() {
        platform::dialogs::notify("Prostir", &local::not_on_this_bus(pid));
    }
    code
}

/// How long a Prostir that runs, but is not on the session bus yet, is waited for.
const ON_THE_BUS_SOON: Duration = Duration::from_secs(2);

/// This program took the lock of the one Prostir of the user: what it starts from now
/// on (the window, the full program again) may start Prostir.
fn the_one_prostir() {
    // SAFETY: `serve` starts no thread before it takes the lock, and this runs right
    // after, so nothing reads or writes the environment meanwhile.
    unsafe { std::env::remove_var(local::STARTED_TO_OPEN) };
}

/// Opens the sockets here; when systemd's socket holds the port, says so.
fn open_sockets(args: &Args, discovery: Option<u16>) -> Result<platform::sockets::Sockets> {
    platform::sockets::open(args.port, discovery).map_err(|e| {
        if platform::autostart::socket_waits() {
            e.context(
                "Prostir already waits for the headset through systemd (prostir.socket): \
                 `prostir` opens its window, `systemctl --user stop prostir.socket` stops it",
            )
        } else {
            // Another program (another user's Prostir, say): there is no tray to say
            // so, a notification does.
            let taken = e.chain().any(|c| {
                c.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == ErrorKind::AddrInUse)
            });
            if taken {
                platform::dialogs::notify_and_wait(
                    i18n::t("pc-main-prostir-is-not-running"),
                    &format!(
                        "{} {} {}",
                        i18n::t("pc-main-port"),
                        args.port,
                        i18n::t("pc-main-is-taken-by-another-program")
                    ),
                );
            }
            e
        }
    })
}

/// The full program, with sockets from outside.
fn full(
    args: &Args,
    sockets: platform::sockets::Sockets,
    origin: Origin,
    why: Option<wake::Wake>,
) -> Result<i32> {
    // Under the watcher, the watcher holds the one instance.
    let instance = match origin {
        Origin::Watcher => None,
        _ => Some(platform::single_instance("serve")?),
    };
    full_with(args, sockets, origin, why, instance)
}

/// The full program: waits for the headset and serves it until asked to quit, or
/// (with sockets from outside) until nothing needs it. Its exit code.
fn full_with(
    args: &Args,
    sockets: platform::sockets::Sockets,
    origin: Origin,
    why: Option<wake::Wake>,
    _instance: Option<platform::Lock>,
) -> Result<i32> {
    let (events, received) = events::channel();
    // A signal from here on quits the usual way, putting everything back.
    {
        let (quit, window) = (events.clone(), events.clone());
        platform::quit_on_signals(
            move |_| {
                quit.send(Event::Quit);
            },
            move || {
                window.tray(TrayCommand::ShowSettings);
            },
        );
    }
    if let Some(why) = why {
        info!("started: {why:?}");
    }
    if args.time_code {
        encoder::draw_time_codes();
    }
    // Whether the PC can share its volume, found out meanwhile.
    platform::audio::prepare();
    // The command line holds for this run only.
    let run = store::Run {
        bitrate: args.bitrate,
        codec: args.codec.map(|c| match c {
            CodecArg::H264 => Codec::H264,
            CodecArg::H265 => Codec::H265,
        }),
        software: args.software.then_some(true),
        screens: None,
    };
    // Prostir ran for this user before (0.1, which kept no choice about autostart),
    // looked at before the store writes anything.
    let ran_before = store::config_dir().is_some_and(|d| !store::first_start(&d));
    let mut store = Store::open(store::config_dir(), run);
    store.start_with(&args.monitors);
    // Starting with the session is on by default: the first start turns it on, and
    // the choice is kept, so turned off it stays off through every update.
    if store.wanted().autostart.is_none() {
        let on = autostart_at_first_start(
            ran_before,
            platform::autostart::available(),
            platform::autostart::enabled(),
        );
        if on {
            platform::autostart::first_start();
        } else {
            info!("an update from 0.1, which did not start with the session: autostart stays off");
        }
        store.change(|s| s.autostart = Some(on));
    }
    // Monitors from a run that crashed: without the headset nobody sees their windows.
    platform::clean_up_after_crash();

    let port = sockets.port();
    info!("{} waiting for the headset on port {port}", version_line());
    // Under the watcher, the watcher keeps `adb reverse` (it runs all along).
    if !args.no_adb && origin != Origin::Watcher {
        platform::keep_adb_reverse(port);
    }

    let security = Arc::new(pairing::Security::load()?);
    let pairing_open = find::PairingOpen::default();
    if let Some(udp) = sockets.udp {
        let responder = find::Responder::new(port, platform::hostname());
        find::spawn(
            udp,
            responder,
            security.clone(),
            pairing_open.clone(),
            events.clone(),
        )?;
    }
    let tray = tray::spawn(events.clone());
    // The local API: requests come in as events, what the control loop says goes out.
    let (outlet, notes) = api::channel();
    pairing::tell_about_codes(events.clone());
    let listening = listen::spawn(
        sockets.tcp,
        sockets.first,
        security.clone(),
        tray.clone(),
        events.clone(),
    )?;
    let mut control = Control {
        tray,
        shown: None,
        shown_settings: None,
        shown_battery: None,
        for_headset: VecDeque::new(),
    };
    control.show(None, &store, &TrayLine::default(), 0, false, false);
    let now = Instant::now();
    let mut door = door::Door::new(pairing_open, origin != Origin::Own, now);
    if args.pair || why == Some(wake::Wake::Pair) {
        door.open_pairing(now, door::pairing_time());
        info!("pairing a new headset: from the notice");
    }
    let mut brain = Brain {
        linger: Duration::from_secs(args.linger),
        set_primary: !args.keep_primary,
        store,
        control,
        security,
        events,
        displays: None,
        sessions: BTreeMap::new(),
        roles: brain::Roles::NONE,
        arrivals: brain::Arrivals::default(),
        closing: Vec::new(),
        phase: Phase::Idle,
        last_id: 0,
        focus: None,
        quit: false,
        api: outlet,
        shown: None,
        revision: 0,
        answers: api::Answers::default(),
        worn_told: BTreeMap::new(),
        door,
        listening,
        code_reply: None,
        shots: local::Shots::default(),
        quit_asked: false,
        waiting_desk: Vec::new(),
        desk_gave_up: false,
        desk_unusable: None,
        retiring: Vec::new(),
        wifi: None,
    };

    // The local API has a state to give from the moment it is on the bus.
    brain.publish_api();
    let api = platform::local_api::serve(brain.events.clone(), brain.api.clone(), notes);
    // On the bus as early as the state can be: GStreamer's start (a scan of its
    // plugins, the first time) comes after.
    gstreamer::init().context("GStreamer init")?;
    // GStreamer's streaming threads come from GLib's pool, which would keep up to eight
    // of them after a pipeline stops, each waking to end 15 s later: an idle PC would
    // wake once more long after the encoders' probe or a session. They end half a
    // second after their last task instead; a pipeline starts new ones.
    gstreamer::glib::ThreadPool::set_max_unused_threads(Some(0));
    // What the encoders can do, found on a thread of its own before a headset needs it.
    encoder::start_probing();
    if args.open_settings
        && let Err(e) = local::open_settings()
    {
        warn!("{e}");
    }
    let (end_now, quit_asked) = brain.run(&received);
    if let Some(api) = api {
        api.finish();
    }
    // Nothing listens on the port any more, but where systemd's socket keeps waiting
    // for the headset (it wakes this program again); under the watcher, the watcher
    // keeps the reverse.
    if origin == Origin::Own || (origin == Origin::Systemd && quit_asked) {
        platform::drop_adb_reverse();
    }
    let code = match origin {
        Origin::Watcher if quit_asked => wake::QUIT_ALL,
        Origin::Systemd if quit_asked => {
            // Asked to quit: nothing waits for the headset until the next login.
            platform::autostart::quit_until_login();
            0
        }
        _ => 0,
    };
    if let Some(end) = end_now {
        // What a run that ends this way leaves behind (Hyprland's outputs, the sound
        // devices), as after a crash.
        platform::clean_up_after_crash();
        // An error stays one; otherwise as the run asked (the watcher quits too).
        platform::end_now(if end == 0 { code } else { end });
    }
    // A probe of the encoders still under way (quitting right after the start) is
    // inside a driver whose library the exit tears down: the process crashed so on
    // the way out. Each probe ends within its time limit; one that hangs is left
    // behind, and then the process ends without the libraries' exit handlers.
    if !encoder::settle(encoder::probe_timeout()) {
        warn!("a probe of the encoders did not end: quitting without waiting for it");
        platform::end_now(code);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use log::{Level, Log, Metadata};

    use super::{Cli, Sub};

    fn read(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::read_from(std::iter::once("prostir").chain(args.iter().copied()))
    }

    /// A new install starts with the session; an update from 0.1 as 0.1 did (its entry
    /// or socket there, or not); Hyprland's config is not set from here.
    #[test]
    fn autostart_at_the_first_start_of_0_2() {
        use super::autostart_at_first_start as on;
        // (ran before, available here, on now)
        assert!(on(false, true, false), "a new install");
        assert!(on(false, false, false), "a new install on Hyprland");
        assert!(on(true, true, true), "0.1 started with the session");
        assert!(!on(true, true, false), "0.1 did not: it stays off");
        assert!(
            on(true, false, false),
            "Hyprland: its config, and the notification"
        );
    }

    /// `-v` and `--version` from every way in; `-m` stays the monitor.
    #[test]
    fn the_version_from_every_way_in() {
        let asks = |args: &[&str]| {
            let cli = read(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            super::asks_version(&cli)
        };
        for args in [
            &["-v"][..],
            &["--version"],
            &["serve", "-v"],
            &["serve", "--version"],
            &["window", "--version"],
            &["ctl", "--version"],
            &["ctl", "-v"],
        ] {
            assert!(asks(args), "{args:?}");
        }
        for args in [
            &[][..],
            &["serve"],
            &["ctl", "status"],
            &["serve", "-m", "1920x1080"],
        ] {
            assert!(!asks(args), "{args:?}");
        }
        assert!(super::version_line().starts_with("Prostir 0."));
    }

    /// 0.1's `prostir -m 2560x1440` is `prostir serve -m 2560x1440`; a bare `prostir`
    /// and the commands are as they are; the help shows the options under `serve` only.
    #[test]
    fn serve_options_without_a_command_mean_serve() {
        let cli = read(&["-m", "2560x1440", "--software", "--linger", "5"]).unwrap();
        assert!(cli.old_options && cli.command.is_none());
        assert_eq!(cli.serve.monitors.len(), 1);
        assert!(cli.serve.software);
        assert_eq!(cli.serve.linger, 5);
        for args in [
            &[][..],
            &["-v"],
            &["serve", "-m", "2560x1440"],
            &["ctl", "status"],
        ] {
            assert!(!read(args).unwrap().old_options, "{args:?}");
        }
        // HEVC as people read it, h265 as before.
        for codec in ["hevc", "h265"] {
            let cli = read(&["serve", "--codec", codec]).unwrap();
            assert!(
                matches!(&cli.command, Some(Sub::Serve(a)) if matches!(a.codec, Some(super::CodecArg::H265))),
                "{codec}"
            );
        }
        let serve = read(&["serve", "--port", "1"]).unwrap();
        assert!(matches!(serve.command, Some(Sub::Serve(a)) if a.port == 1));
        assert!(
            read(&["-m", "2560x1440", "serve"]).is_err(),
            "one or the other"
        );
        let help = super::cli::command_line().render_long_help().to_string();
        assert!(!help.contains("--monitor"), "{help}");
        let mut line = super::cli::command_line();
        let serve_help = line
            .find_subcommand_mut("serve")
            .unwrap()
            .render_long_help();
        assert!(serve_help.to_string().contains("--monitor"));
    }

    /// Prostir's own lines at info, a dependency's only from warnings: zbus logs
    /// every call of the local API at info.
    #[test]
    fn dependencies_log_only_warnings() {
        let logger = env_logger::Builder::new()
            .parse_filters(&super::log_filter("info"))
            .build();
        let on = |target: &str, level: Level| {
            logger.enabled(&Metadata::builder().target(target).level(level).build())
        };
        assert!(on("prostir::session", Level::Info));
        assert!(on("prostir", Level::Info));
        // The window's and the translations' crates: their own names, Prostir's lines.
        assert!(on("prostir_window::run", Level::Info));
        assert!(on("prostir_lang", Level::Info));
        assert!(!on("prostir::session", Level::Debug));
        assert!(!on("zbus::object_server", Level::Info));
        assert!(!on("tracing::span", Level::Info));
        assert!(on("zbus::connection", Level::Warn));
    }
}
