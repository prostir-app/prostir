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

use jni::objects::{JString, JValue};
use jni::strings::JNIStr;
use jni::{jni_sig, jni_str};
use log::warn;

use crate::lang::{Known, Lang, Loader, Texts};
use crate::mic::with_app_context;

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
            if events.send(SystemEvent::Battery(battery())).is_err() {
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

/// What the headset is, as the system names it (`Build.MANUFACTURER` and
/// `Build.MODEL`, see [`crate::device::model_name`]); empty when it does not say.
pub fn model() -> String {
    let said = with_app_context(|env, _| {
        let mut field = |name: &JNIStr| -> jni::errors::Result<String> {
            let value = env
                .get_static_field(
                    jni_str!("android/os/Build"),
                    name,
                    jni_sig!("Ljava/lang/String;"),
                )?
                .l()?;
            if value.is_null() {
                return Ok(String::new());
            }
            env.cast_local::<JString<'_>>(value)?.try_to_string(env)
        };
        Ok((field(jni_str!("MANUFACTURER"))?, field(jni_str!("MODEL"))?))
    });
    let (manufacturer, model) = said.unwrap_or_default();
    crate::device::model_name(&manufacturer, &model)
}

/// The headset's own language (`Locale.getDefault()`, else the system property
/// `persist.sys.locale`); none when the system does not say.
pub fn language() -> Option<Lang> {
    let tag = with_app_context(|env, _| {
        let locale = env
            .call_static_method(
                jni_str!("java/util/Locale"),
                jni_str!("getDefault"),
                jni_sig!("()Ljava/util/Locale;"),
                &[],
            )?
            .l()?;
        let tag = env
            .call_method(
                &locale,
                jni_str!("toLanguageTag"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        if tag.is_null() {
            return Ok(String::new());
        }
        env.cast_local::<JString<'_>>(tag)?.try_to_string(env)
    })
    .filter(|t| !t.is_empty() && t != "und")
    .or_else(|| {
        let out = std::process::Command::new("getprop")
            .arg("persist.sys.locale")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    })?;
    let lang = Lang::from_tag(&tag);
    log::info!("system language {tag:?}: {lang:?}");
    lang
}

/// Whether the screen is on for the user (`PowerManager.isInteractive`): false once
/// the system goes to sleep (its power button, or taken off for long enough). One
/// call into Android: asked only while the app is put away, never in a drawn frame.
pub fn interactive() -> Option<bool> {
    with_app_context(|env, context| {
        let name = env.new_string("power")?;
        let manager = env
            .call_method(
                context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        env.call_method(&manager, jni_str!("isInteractive"), jni_sig!("()Z"), &[])?
            .z()
    })
}

/// The headset's battery: percent and whether it is charging (`BatteryManager`).
pub fn battery() -> Option<(u8, bool)> {
    with_app_context(|env, context| {
        let name = env.new_string("batterymanager")?;
        let manager = env
            .call_method(
                context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        let property = |env: &mut jni::Env<'_>, id| {
            env.call_method(
                &manager,
                jni_str!("getIntProperty"),
                jni_sig!("(I)I"),
                &[JValue::Int(id)],
            )?
            .i()
        };
        // BatteryManager.BATTERY_PROPERTY_CAPACITY
        let percent = property(env, 4)?;
        // BATTERY_PROPERTY_STATUS: 2 is charging (isCharging also says yes when full
        // and plugged in).
        let status = property(env, 6)?;
        let charging = status == 2;
        Ok((percent.clamp(0, 100) as u8, charging))
    })
}
