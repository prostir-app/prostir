//! What the app asks the system. The `system` thread asks now and then, away from the
//! frame (a call into Java takes its time): for now the battery, once a minute and
//! every 15 s while the palm menu shows, read afresh when it appears. It also reads the
//! language files when the menu's language changes (see [`crate::lang`]). What does not
//! change (the headset's model, its language) is asked once at the start.

#![expect(
    clippy::disallowed_methods,
    reason = "the system thread is part of the shell"
)]

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::warn;

use crate::lang::{Known, Lang, Loader, Texts};

/// How often the battery is read, and while the palm menu shows it.
const BATTERY_EVERY: Duration = Duration::from_secs(60);
const BATTERY_EVERY_ON_PALM: Duration = Duration::from_secs(15);
/// How long the app waits for the thread when it closes.
const STOP_WITHIN: Duration = Duration::from_millis(300);

/// What the thread found.
pub enum SystemEvent {
    /// The battery: percent and whether it is charging; `None` when Android did not
    /// say.
    Battery(Option<(u8, bool)>),
    /// A language loaded, ready for the menu.
    Texts(Texts),
    /// The languages there are files for, listed again.
    Languages(Vec<Known>),
}

enum SystemCmd {
    /// The palm menu (which shows the battery) appeared or went.
    PalmMenu(bool),
    /// Load this language's files.
    Language(Lang),
    /// List the language files again.
    Languages,
}

/// The running thread, for one start of the app: it stops when this is dropped.
pub struct System {
    cmds: Option<Sender<SystemCmd>>,
    /// The thread is gone (it could not start, or it ended): said once in the log.
    gone: std::cell::Cell<bool>,
    events: Receiver<SystemEvent>,
    thread: Option<JoinHandle<()>>,
}

impl System {
    /// Starts the thread; the battery was just read (see [`battery`]), so its first
    /// reading comes a minute later. `loader` reads the language files from now on.
    pub fn start(loader: Loader) -> Self {
        let (cmds, from_app) = mpsc::channel();
        let (to_app, events) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("system".into())
            .spawn(move || run(&from_app, &to_app, loader))
            .inspect_err(|e| warn!("no system thread: {e}"))
            .ok();
        Self {
            cmds: Some(cmds),
            gone: std::cell::Cell::new(false),
            events,
            thread,
        }
    }

    /// The palm menu appeared (`true`) or went.
    pub fn palm_menu(&self, shown: bool) {
        if let Some(cmds) = &self.cmds {
            let _ = cmds.send(SystemCmd::PalmMenu(shown));
        }
    }

    /// Load the files of `lang`; [`SystemEvent::Texts`] brings them.
    pub fn load_language(&self, lang: Lang) {
        self.send(SystemCmd::Language(lang));
    }

    /// List the language files again if they changed (one may have been added);
    /// [`SystemEvent::Languages`] brings the list.
    pub fn list_languages(&self) {
        self.send(SystemCmd::Languages);
    }

    fn send(&self, cmd: SystemCmd) {
        let sent = self.cmds.as_ref().is_some_and(|c| c.send(cmd).is_ok());
        if !sent && !self.gone.replace(true) {
            warn!("no system thread: the menu keeps its language, the battery is not read");
        }
    }

    /// The next thing the thread found, if there is one; never waits.
    pub fn try_recv(&self) -> Option<SystemEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for System {
    fn drop(&mut self) {
        // Its channel closes, and the thread ends at once (unless in a call into Java).
        self.cmds = None;
        if let Some(thread) = self.thread.take() {
            join_within("system", thread, STOP_WITHIN);
        }
    }
}

fn run(cmds: &Receiver<SystemCmd>, events: &Sender<SystemEvent>, mut loader: Loader) {
    let mut palm = false;
    let mut read_at = Instant::now();
    loop {
        let every = if palm {
            BATTERY_EVERY_ON_PALM
        } else {
            BATTERY_EVERY
        };
        let wait = (read_at + every).saturating_duration_since(Instant::now());
        let read = match cmds.recv_timeout(wait) {
            Ok(SystemCmd::PalmMenu(shown)) => {
                let appeared = shown && !palm;
                palm = shown;
                appeared
            }
            Ok(SystemCmd::Language(lang)) => {
                if events.send(SystemEvent::Texts(loader.load(lang))).is_err() {
                    return;
                }
                false
            }
            Ok(SystemCmd::Languages) => {
                if let Some(known) = loader.languages_if_changed()
                    && events.send(SystemEvent::Languages(known)).is_err()
                {
                    return;
                }
                false
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if read {
            read_at = Instant::now();
            if events.send(SystemEvent::Battery(super::battery())).is_err() {
                return;
            }
        }
    }
}

/// Waits up to `limit` for `thread` to end. One that does not is left running, and the
/// log says so: a new start of the app in the same process does not wait for it.
pub fn join_within(name: &str, thread: JoinHandle<()>, limit: Duration) {
    let until = Instant::now() + limit;
    while !thread.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    if !thread.is_finished() {
        warn!(
            "the {name} thread did not stop within {} ms",
            limit.as_millis()
        );
    } else if thread.join().is_err() {
        warn!("the {name} thread panicked");
    }
}
