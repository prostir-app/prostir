//! The local API in the control loop: each request of `prostir ctl`, the settings
//! window or a script carried out (see `crate::api` for what there is), the state
//! they see built from what the control loop owns, and what happens to the headsets
//! told as events.
//!
//! A request that changes the PC's state is answered in the same pass, after the
//! state that shows the change went out; a command for the headset is answered when
//! the headset answers. The person at the PC asks as the owner (every right); a
//! command for the headset goes to the main headset only: a guest is someone else's
//! headset, so with only guests connected it is refused (`NotAllowed`).

use std::sync::Arc;
use std::time::Instant;

use log::{info, warn};
use prostir_proto::Codec;
use prostir_proto::request::{Asked, Command, Request as HeadsetRequest};
use prostir_proto::state::{self as wire, DisplayId, HeadsetCaps, Limits, Mode, caps};

use crate::api::{self, ApiError, CodecChoice, CtlReply, Failure, Reply, Request};
use crate::i18n::t;
use crate::platform::{self, MonitorSpec};
use crate::screen::ScreenId;
use crate::worker::JobId;
use crate::{Brain, brain, errors, pairing, publish, requests};

/// The command for the headset a request is, if it is one.
fn command_of(request: &Request) -> Option<Command> {
    Some(match request {
        Request::Recenter => Command::Recenter,
        Request::LineUp => Command::LineUp,
        Request::ShowMouse => Command::ShowMouse,
        Request::ResetArrangement => Command::ResetArrangement,
        Request::Profile { name } => Command::Profile { name: name.clone() },
        Request::Cinema { on } => Command::Cinema { on: *on },
        Request::Focus { on } => Command::FocusMode { on: *on },
        Request::Follow { on } => Command::Follow { on: *on },
        Request::NoLimits { on } => Command::NoLimits { on: *on },
        Request::Distance { steps } => Command::Distance { steps: *steps },
        Request::Preset { preset } => Command::Preset { preset: *preset },
        _ => return None,
    })
}

/// Whether `command` can go to the headset as it is: its arguments within what the
/// protocol carries (a profile's name of 64 bytes at most), else why not.
pub fn fits_the_wire(command: &Command) -> Result<(), Failure> {
    if let Command::Profile { name } = command
        && name.len() > wire::MAX_DISPLAY_NAME
    {
        return Err(Failure::new(
            ApiError::BadArgument,
            format!(
                "{} {}",
                t(
                    "Назва профілю задовга, байтів щонайбільше:",
                    "A profile's name is too long, bytes at most:"
                ),
                wire::MAX_DISPLAY_NAME
            ),
        ));
    }
    let msg = prostir_proto::HostMsg::Command {
        req: wire::ReqId(0),
        asked: Asked::Known(command.clone()),
    };
    msg.encode().map(drop).map_err(|e| {
        Failure::new(
            ApiError::BadArgument,
            format!(
                "{}: {e}",
                t("Команда не годиться", "The command does not fit")
            ),
        )
    })
}

/// What the PC itself can show: its own displays and window panels, any codec.
fn pc_caps() -> HeadsetCaps {
    HeadsetCaps {
        features: vec![caps::headset::PHYSICAL, caps::headset::WINDOWS],
        ..HeadsetCaps::default()
    }
}

/// What the PC allows any headset, for a request without one.
fn pc_limits(codec: Codec) -> Limits {
    publish::limits(wire::Prefs::default(), &pc_caps(), codec)
}

fn done() -> CtlReply {
    CtlReply::Done
}

fn unsupported(text: &str) -> CtlReply {
    CtlReply::failed(ApiError::Unsupported, text)
}

/// The failure for `e` (a `UserError` somewhere in its chain names it).
/// The failure for `e`: its name, and the text of the `UserError` in it (in the PC's
/// language); what lay under it (a tool's output, say) goes to the log only.
fn failed(e: &anyhow::Error) -> CtlReply {
    warn!("local API: {e:#}");
    let text = e.downcast_ref::<errors::UserError>().map_or_else(
        || t("Не вдалося", "It did not work").to_owned(),
        ToString::to_string,
    );
    CtlReply::failed(api::of_code(errors::code_of(e)), text)
}

/// A request as the log says it: its JSON, what it asks and with what (`sound_to_headset`
/// with `"on":false`), but a pairing code, which stays out of the log.
fn logged(request: &Request) -> String {
    let Ok(mut json) = serde_json::to_value(request) else {
        return String::new();
    };
    if let Some(code) = json.get_mut("code") {
        *code = "…".into();
    }
    json.to_string()
}

/// The PC's name, as headsets list it: looked up once.
fn hostname() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(platform::hostname)
}

/// Screenshots asked for on the local API, waiting for the desktop's worker: each by
/// the worker's generation and its job (job numbers start again with each worker), with
/// its reply and until when it waits.
#[derive(Default)]
pub struct Shots(Vec<(u64, JobId, Reply, Instant)>);

impl Shots {
    pub fn push(&mut self, generation: u64, job: JobId, reply: Reply, until: Instant) {
        self.0.push((generation, job, reply, until));
    }

    /// The reply waiting for job `job` of worker `generation`, taken.
    pub fn take(&mut self, generation: u64, job: JobId) -> Option<Reply> {
        let i = self
            .0
            .iter()
            .position(|(g, j, ..)| *g == generation && *j == job)?;
        Some(self.0.remove(i).2)
    }

    /// When the first one has waited long enough.
    pub fn due(&self) -> Option<Instant> {
        self.0.iter().map(|(.., until)| *until).min()
    }

    /// Those that waited until `now` (all of them with `all`), taken.
    pub fn over(&mut self, now: Instant, all: bool) -> Vec<(u64, JobId, Reply)> {
        let (over, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.0)
            .into_iter()
            .partition(|(.., until)| all || *until <= now);
        self.0 = still;
        over.into_iter().map(|(g, j, r, _)| (g, j, r)).collect()
    }
}

impl Brain {
    /// One request of the local API.
    pub(crate) fn ctl(&mut self, request: Request, reply: Reply) {
        info!("local API: {}", logged(&request));
        if let Some(command) = command_of(&request) {
            if self.sessions.is_empty() {
                let text = t("Шолом не підключено", "No headset is connected");
                return self.answer(reply, CtlReply::failed(ApiError::NotConnected, text));
            }
            if self.roles.main.is_none() {
                let text = t(
                    "Команди з ПК виконує лише головний шолом, а підключено гостя",
                    "Commands from the PC go to the main headset; a guest is connected",
                );
                return self.answer(reply, CtlReply::failed(ApiError::NotAllowed, text));
            }
            if let Err(why) = fits_the_wire(&command) {
                return self.answer(reply, CtlReply::Failed(why));
            }
            // Answered by the headset (see `Session::commands_due`).
            self.control
                .for_headset
                .push_back((Asked::Known(command), Some(reply)));
            return;
        }
        if let Request::PairingCode { code } = request {
            return self.code_typed(&code, reply);
        }
        if let Request::Screenshot { id } = request {
            return self.screenshot(id, reply);
        }
        let result = self.carry_out(request);
        if let Some(d) = self.displays.as_mut() {
            d.apply(&self.store.wanted());
        }
        self.answer(reply, result);
    }

    /// The code a headset shows, typed in the settings window: it goes to the
    /// connection waiting for it, and the answer comes once that attempt ends.
    /// Starting with the session on or off (the tray, `ctl`, the settings window): set,
    /// and the choice kept, so no later start or update turns it on again.
    pub(crate) fn set_autostart(&mut self, on: bool) {
        info!(
            "starting with the session: {}",
            if on { "on" } else { "off" }
        );
        platform::autostart::set(on);
        self.store.change(|s| s.autostart = Some(on));
    }

    /// The answer to "Count this PC among users?" (the settings window, the tray,
    /// `ctl`), kept; `unasked` puts the question back.
    pub(crate) fn set_counting(&mut self, count: prostir_api::Counting) {
        info!("counting this PC among users: {count:?}");
        self.store.change(|s| s.count = count);
    }

    fn code_typed(&mut self, code: &str, reply: Reply) {
        let digits = code
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>();
        if digits.len() != 6 || code.chars().any(|c| !c.is_ascii_digit() && c != ' ') {
            let text = t("Код: шість цифр", "The code: six digits");
            return self.answer(reply, CtlReply::failed(ApiError::BadArgument, text));
        }
        if self.code_reply.is_some() || !pairing::type_code(digits) {
            let text = t(
                "Жоден шолом не чекає на код",
                "No headset is waiting for a code",
            );
            return self.answer(reply, CtlReply::failed(ApiError::NotAllowed, text));
        }
        self.code_reply = Some(reply);
    }

    /// The attempt with the code the settings window typed ended (or its headset
    /// went): its answer, now the state shows how it ended.
    pub(crate) fn code_tried(&mut self) {
        if let Some(reply) = self.code_reply.take() {
            self.answer(reply, done());
        }
    }

    /// A picture of monitor `n`, as the headset's screenshot: the desktop's worker
    /// takes it, and the answer comes once it did (see [`Brain::shot_taken`]).
    fn screenshot(&mut self, n: u32, reply: Reply) {
        let Some(displays) = self.displays.as_mut() else {
            let text = t(
                "Знімок можна зробити, поки монітори є на стільниці",
                "A screenshot is taken while the monitors are on the desktop",
            );
            return self.answer(reply, unsupported(text));
        };
        if displays.desk_stuck() {
            let text = crate::session::desktop_not_responding();
            return self.answer(reply, CtlReply::failed(ApiError::Desktop, text));
        }
        match displays.screenshot(ScreenId::from_wire(DisplayId(n))) {
            Ok(job) => {
                let until = Instant::now() + crate::worker::answer_within();
                self.shots.push(displays.generation(), job, reply, until);
            }
            Err(e) => self.answer(reply, failed(&e)),
        }
    }

    /// The answer to job `job` of the desktop's worker `generation`, if a screenshot
    /// of the local API waits for it: whether one did.
    pub(crate) fn shot_taken(
        &mut self,
        generation: u64,
        job: JobId,
        result: &anyhow::Result<DisplayId>,
        answer: &crate::displays::JobAnswer,
    ) -> bool {
        let Some(reply) = self.shots.take(generation, job) else {
            return false;
        };
        let result = match (result, answer) {
            (Err(e), _) => failed(e),
            (Ok(_), crate::displays::JobAnswer::Saved { path, whole }) => CtlReply::Saved {
                path: path.to_string_lossy().into_owned(),
                whole: *whole,
            },
            (Ok(_), _) => done(),
        };
        self.answer(reply, result);
        true
    }

    /// Screenshots that waited for the desktop until `now` (all of them with `all`:
    /// the desktop does not respond) fail with `Desktop`; the worker is told to drop
    /// their jobs, which nobody waits for now.
    pub(crate) fn give_up_shots(&mut self, now: Instant, all: bool) {
        for (generation, job, reply) in self.shots.over(now, all) {
            let text = crate::session::desktop_not_responding();
            self.answer(reply, CtlReply::failed(ApiError::Desktop, text));
            if let Some(d) = self
                .displays
                .as_ref()
                .filter(|d| d.generation() == generation)
            {
                d.desk().cancel(job);
            }
        }
    }

    /// The monitors leave the desktop (their worker is let go): screenshots still
    /// waiting for it are answered now, as no answer comes any more.
    pub(crate) fn shots_without_monitors(&mut self) {
        for (_, _, reply) in self.shots.over(Instant::now(), true) {
            let text = t(
                "Монітори зникли зі стільниці раніше, ніж знімок",
                "The monitors left the desktop before the screenshot",
            );
            self.answer(reply, unsupported(text));
        }
    }

    /// Shows the notice of a new headset nearby; its button opens the pairing window
    /// (`Event::Notice`). A thread waits for it, asleep in the kernel.
    pub(crate) fn notice_new_headset(&mut self) {
        let Some(mut notice) = crate::wake::new_headset_notice() else {
            return;
        };
        info!("a new headset asked for this PC: telling the person at the PC");
        let now = Instant::now();
        self.door.notice_up(true, now);
        let until = now + crate::door::NOTICE_TIME;
        let events = self.events.clone();
        let spawned = std::thread::Builder::new()
            .name("notice".into())
            .spawn(move || {
                let mut pressed = false;
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    match platform::sockets::readable(&[notice.fd()], Some(left)) {
                        Ok(ready) if ready[0] => {
                            if let Some(p) = notice.check() {
                                pressed = p;
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                drop(notice);
                if pressed {
                    events.send(crate::events::Event::Notice(true));
                }
                events.send(crate::events::Event::Notice(false));
            });
        if let Err(e) = spawned {
            warn!("no notice of the new headset: {e}");
            self.door.notice_up(false, now);
        }
    }

    fn answer(&self, reply: Reply, result: CtlReply) {
        if let CtlReply::Failed(f) = &result {
            info!("local API: {f}");
        }
        self.answers.push(reply, result);
    }

    fn carry_out(&mut self, request: Request) -> CtlReply {
        let id = |n: u32| DisplayId(n);
        match request {
            Request::AddMonitor { w, h, scale } => {
                let mode = Mode { w, h, scale };
                if self.displays.is_some() {
                    self.pc_request(HeadsetRequest::AddDisplay { mode })
                } else {
                    self.add_saved(mode)
                }
            }
            Request::RemoveMonitor { id: n } => {
                if self.displays.is_some() {
                    self.pc_request(HeadsetRequest::RemoveDisplay { id: id(n) })
                } else {
                    self.remove_saved(ScreenId::saved(n))
                }
            }
            Request::RetryMonitor { id: n } => match self.displays.as_mut() {
                Some(d) => d
                    .retry(ScreenId::saved(n))
                    .map_or_else(|e| failed(&e), |()| done()),
                // Every monitor is made anew for the next headset anyway.
                None if self.store.screens().specs.contains_key(&ScreenId::saved(n)) => done(),
                None => failed(&anyhow::Error::new(errors::UserError::UnknownDisplay)),
            },
            Request::ShowPhysical { id: n, shown } => {
                if self.displays.is_some() {
                    self.pc_request(HeadsetRequest::ShowPhysical { id: id(n), shown })
                } else {
                    self.change_saved(ScreenId::saved(n), |set, key| {
                        let physical = set.physical.get_mut(&key)?;
                        physical.shown = shown;
                        Some(())
                    })
                }
            }
            Request::SetPrimary { id: n } => {
                if self.displays.is_some() {
                    self.pc_request(HeadsetRequest::SetPrimary { id: id(n) })
                } else {
                    self.change_saved(ScreenId::saved(n), |set, key| {
                        let known = set.specs.contains_key(&key) || set.physical.contains_key(&key);
                        known.then(|| set.primary = Some(key))
                    })
                }
            }
            Request::SetQuality { mbit } => {
                if !(5..=100).contains(&mbit) {
                    return CtlReply::failed(
                        ApiError::BadArgument,
                        t("Якість: від 5 до 100 Мбіт/с", "Quality: 5 to 100 Mbit/s"),
                    );
                }
                self.store.set_bitrate(mbit);
                done()
            }
            Request::SetSharp { on } => {
                self.store.change(|s| s.sharp = on);
                done()
            }
            Request::SetCodec { codec, cpu } => {
                let codec = match codec {
                    CodecChoice::H264 => Codec::H264,
                    CodecChoice::H265 | CodecChoice::Auto => Codec::H265,
                };
                self.set_codec(codec, cpu)
            }
            Request::SoundToHeadset { on } => {
                self.store.change(|s| s.audio = on);
                done()
            }
            Request::SetCounting { counting } => {
                self.set_counting(counting);
                done()
            }
            Request::Autostart { on } => {
                if !platform::autostart::available() {
                    return unsupported(t(
                        "Тут Prostir не вмикає автозапуск сам",
                        "Starting with the session is not set from here on this PC",
                    ));
                }
                self.set_autostart(on);
                done()
            }
            Request::WindowLook {
                theme,
                palette,
                language,
            } => {
                let words = crate::store::is_word(&palette)
                    && language.as_deref().is_none_or(crate::store::is_word);
                if !words || theme == api::ThemeMode::Other {
                    return CtlReply::failed(ApiError::BadArgument, "theme, palette or language");
                }
                self.store.change(|s| {
                    s.window = api::WindowLook {
                        theme,
                        palette,
                        language,
                    }
                });
                done()
            }
            Request::ForgetHeadset { headset } => self.forget(&headset),
            Request::ForgetAll => {
                self.tray(platform::TrayCommand::ForgetHeadsets);
                done()
            }
            Request::ShowSettings => match open_settings() {
                Ok(()) => done(),
                Err(f) => CtlReply::Failed(f),
            },
            Request::Quit => {
                // As the tray's "Quit": nothing waits for the headset afterwards either.
                self.quit = true;
                self.quit_asked = true;
                done()
            }
            Request::SetMic { .. } => unsupported(t(
                "Мікрофон шолома вмикають лише в шоломі",
                "The headset microphone is switched in the headset only",
            )),
            Request::PairNewHeadset => {
                info!("pairing a new headset: from the local API");
                CtlReply::Pairing(
                    self.door
                        .open_pairing(Instant::now(), crate::door::pairing_time()),
                )
            }
            Request::StopPairing => {
                self.door.stop_pairing();
                done()
            }
            // Handled in `ctl`: answered once the attempt ends, or once the desktop
            // took the picture.
            Request::PairingCode { .. } | Request::Screenshot { .. } => unsupported(""),
            Request::SetScale { id: n, scale } => self.rescale(ScreenId::saved(n), scale),
            Request::Place { id: n, x, y } => match self.displays.as_mut() {
                Some(d) => match d.move_to(ScreenId::saved(n), x, y) {
                    Ok(()) => {
                        crate::keep_screens(&mut self.store, d, false);
                        done()
                    }
                    Err(e) => failed(&e),
                },
                None => unsupported(t(
                    "Монітори розставляються, поки вони є на стільниці",
                    "Monitors are placed while they are on the desktop",
                )),
            },
            Request::Volume { level } if level > 100 => {
                CtlReply::failed(ApiError::BadArgument, "level: 0 to 100")
            }
            Request::Volume { level } => match self.lead_mut().map(|s| s.volume_from_pc(level)) {
                None => unsupported(t(
                    "Гучність шолома змінюється, поки він підключений",
                    "The headset's volume is changed while it is connected",
                )),
                Some(true) => done(),
                Some(false) => unsupported(t(
                    "Гучність цього шолома з ПК не змінюється: лише його кнопками",
                    "This headset's volume is not changed from the PC, only with its buttons",
                )),
            },
            // Handled above.
            Request::Recenter
            | Request::LineUp
            | Request::ShowMouse
            | Request::ResetArrangement
            | Request::Profile { .. }
            | Request::Cinema { .. }
            | Request::Focus { .. }
            | Request::Follow { .. }
            | Request::NoLimits { .. }
            | Request::Distance { .. }
            | Request::Preset { .. } => unsupported(""),
        }
    }

    /// A request on the monitors, as the headset's requests are carried out (they
    /// stay in step with each other): with every right, within the main headset's
    /// limits (else what any headset gets).
    fn pc_request(&mut self, request: HeadsetRequest) -> CtlReply {
        let Some(displays) = self.displays.as_mut() else {
            return CtlReply::failed(ApiError::Other, "no monitors");
        };
        let lead = self
            .roles
            .main
            .and_then(|key| self.sessions.get(&key))
            .or_else(|| self.sessions.values().min_by_key(|s| s.id));
        let (caps, limits) = match lead {
            Some(s) => (s.caps().clone(), s.limits(displays)),
            None => (pc_caps(), pc_limits(displays.codec())),
        };
        let asker = requests::Asker {
            caps: &caps,
            limits: &limits,
            access: wire::Access::MAIN,
        };
        match requests::handle_checked(request, displays, &mut self.store, &asker) {
            Ok(requests::Answer::Now(outcome)) => CtlReply::of_outcome(&outcome),
            // None of the requests the PC makes waits for the desktop.
            Ok(requests::Answer::Later(_)) => done(),
            Err(e) => failed(&e),
        }
    }

    /// Another scale for monitor `id`: made anew at it while there are monitors, else
    /// kept for the next headset.
    fn rescale(&mut self, id: ScreenId, scale: u16) -> CtlReply {
        if let Some(d) = self.displays.as_mut() {
            let result = d.rescale(id, scale);
            crate::keep_screens(&mut self.store, d, true);
            return result.map_or_else(|e| failed(&e), |()| done());
        }
        let limits = pc_limits(self.store.wanted().codec);
        if !limits.scales.contains(&scale) {
            let spec = self.store.screens().specs.get(&id).cloned();
            let (w, h) = spec.map_or((0, 0), |s| (s.width, s.height));
            return failed(&anyhow::Error::new(errors::UserError::BadSize {
                w,
                h,
                scale,
            }));
        }
        self.change_saved(id, |set, key| {
            let spec = set.specs.get_mut(&key)?;
            spec.scale = f64::from(scale) / 100.0;
            Some(())
        })
    }

    /// The codec, and whether on the CPU, for now and later.
    fn set_codec(&mut self, codec: Codec, cpu: bool) -> CtlReply {
        if self.displays.is_some() && !self.sessions.is_empty() {
            return self.pc_request(HeadsetRequest::SetEncoding {
                bitrate: None,
                codec: Some(codec),
                software: Some(cpu),
                sharp: None,
            });
        }
        let now = self.store.wanted();
        self.store.set_encoding(now.bitrate, now.sharp, codec, cpu);
        match self.displays.as_mut() {
            Some(d) => {
                d.apply(&self.store.wanted());
                d.encode_with(codec, cpu)
                    .map_or_else(|e| failed(&e), |()| done())
            }
            None => done(),
        }
    }

    /// A monitor added to the set kept for the next headset.
    fn add_saved(&mut self, mode: Mode) -> CtlReply {
        let limits = pc_limits(self.store.wanted().codec);
        let spec = MonitorSpec {
            width: mode.w,
            height: mode.h,
            scale: f64::from(mode.scale) / 100.0,
        };
        let fits = (limits.min.0..=limits.max.0).contains(&mode.w)
            && (limits.min.1..=limits.max.1).contains(&mode.h)
            && limits.scales.contains(&mode.scale)
            && spec.validate().is_ok();
        if !fits {
            let e = errors::UserError::BadSize {
                w: mode.w,
                h: mode.h,
                scale: mode.scale,
            };
            return CtlReply::failed(ApiError::BadSize, e.to_string());
        }
        let mut set = self.store.screens().clone();
        let max = usize::from(limits.virtual_max);
        if set.specs.len() >= max {
            let e = errors::UserError::Limit {
                max,
                of: errors::Counted::Monitors,
            };
            return CtlReply::failed(ApiError::Limit, e.to_string());
        }
        let id = self.store.new_id();
        set.specs.insert(id, spec);
        match set.rows.last_mut() {
            Some(row) => row.push(prostir_layout::desk::Entry::new(id)),
            None => set.rows.push(vec![prostir_layout::desk::Entry::new(id)]),
        }
        self.store.set_screens(set, true);
        CtlReply::Added(id.number())
    }

    /// A monitor taken out of the set kept for the next headset (never the last).
    fn remove_saved(&mut self, id: ScreenId) -> CtlReply {
        let mut set = self.store.screens().clone();
        if !set.specs.contains_key(&id) {
            let e = errors::UserError::UnknownDisplay;
            return CtlReply::failed(ApiError::UnknownDisplay, e.to_string());
        }
        if set.specs.len() == 1 {
            let e = errors::UserError::LastDisplay;
            return CtlReply::failed(ApiError::LastDisplay, e.to_string());
        }
        set.specs.remove(&id);
        for row in &mut set.rows {
            row.retain(|e| e.id != id);
        }
        set.rows.retain(|row| !row.is_empty());
        if set.primary == Some(id) {
            set.primary = None;
        }
        self.store.set_screens(set, true);
        done()
    }

    /// Changes the set kept for the next headset with `change`, which gives `None`
    /// when display `id` is not in it.
    fn change_saved(
        &mut self,
        id: ScreenId,
        change: impl FnOnce(&mut crate::store::SavedScreens, ScreenId) -> Option<()>,
    ) -> CtlReply {
        let mut set = self.store.screens().clone();
        if change(&mut set, id).is_none() {
            let e = errors::UserError::UnknownDisplay;
            return CtlReply::failed(ApiError::UnknownDisplay, e.to_string());
        }
        self.store.set_screens(set, false);
        done()
    }

    /// Forgets the paired headset `which` names (its fingerprint, its whole key or its
    /// name); its session ends.
    fn forget(&mut self, which: &str) -> CtlReply {
        let which = which.trim();
        let found: Vec<[u8; 32]> = {
            let trusted = self.security.trusted.lock().unwrap();
            let by_key: Vec<[u8; 32]> = trusted
                .entries()
                .iter()
                .filter(|(key, _)| {
                    prostir_proto::secure::fingerprint(key).eq_ignore_ascii_case(which)
                        || prostir_proto::secure::to_hex(key).eq_ignore_ascii_case(which)
                })
                .map(|(key, _)| *key)
                .collect();
            if by_key.is_empty() {
                trusted
                    .entries()
                    .iter()
                    .filter(|(_, name)| name == which)
                    .map(|(key, _)| *key)
                    .collect()
            } else {
                by_key
            }
        };
        let key = match found.as_slice() {
            [key] => *key,
            [] => {
                return CtlReply::failed(
                    ApiError::BadArgument,
                    format!(
                        "{} {which}",
                        t("Немає сполученого шолома", "No paired headset")
                    ),
                );
            }
            _ => {
                return CtlReply::failed(
                    ApiError::BadArgument,
                    t(
                        "Так названо кілька шоломів: назвіть ключем",
                        "Several headsets have this name: give its key",
                    ),
                );
            }
        };
        let removed = self.security.trusted.lock().unwrap().remove(&key);
        if let Err(e) = removed {
            warn!("cannot forget the headset: {e}");
            return CtlReply::failed(ApiError::Other, e.to_string());
        }
        info!(
            "forgot headset {}",
            prostir_proto::secure::fingerprint(&key)
        );
        self.keep_seen_of_paired();
        // It can no longer find the PC; the headsets still trusted get the new secret
        // now, in their sessions, or at their next.
        let secret = self.security.new_find_secret();
        for (other, session) in &self.sessions {
            if *other != key {
                session.send(prostir_proto::HostMsg::FindSecret { secret });
            }
        }
        self.end_session(
            key,
            Some(prostir_proto::PcGoodbye::Forgotten),
            api::Reason::Forgotten,
            Ok(()),
        );
        done()
    }

    /// When each headset was seen is kept for the paired ones only.
    pub(crate) fn keep_seen_of_paired(&mut self) {
        let paired: Vec<[u8; 32]> = {
            let trusted = self.security.trusted.lock().unwrap();
            trusted.entries().iter().map(|(key, _)| *key).collect()
        };
        self.store.keep_seen_of(&paired);
    }

    /// A headset's name as it was paired.
    pub(crate) fn headset_name(&self, key: &[u8; 32]) -> String {
        self.security
            .trusted
            .lock()
            .unwrap()
            .label(key)
            .unwrap_or_default()
            .to_owned()
    }

    /// A headset's session ended: said on the local API.
    pub(crate) fn headset_left(&mut self, key: &[u8; 32], why: api::Reason) {
        self.worn_told.remove(key);
        self.api
            .send(api::Note::Event(api::Event::HeadsetDisconnected {
                headset: prostir_proto::secure::fingerprint(key),
                reason: why,
            }));
    }

    /// End of a pass: a headset put on or taken off is told, the state goes out if it
    /// changed, then the replies waiting for it.
    pub(crate) fn publish_api(&mut self) {
        for (key, session) in &self.sessions {
            let worn = session.worn();
            if self
                .worn_told
                .insert(*key, worn)
                .is_some_and(|was| was != worn)
            {
                self.api.send(api::Note::Event(api::Event::HeadsetWorn {
                    headset: prostir_proto::secure::fingerprint(key),
                    worn,
                }));
            }
        }
        let state = self.api_state();
        if self.shown.as_deref() != Some(&state) {
            let state = Arc::new(state);
            self.revision += 1;
            self.api
                .send(api::Note::State(self.revision, state.clone()));
            self.shown = Some(state);
        }
        for (reply, result) in self.answers.take() {
            reply.send(result);
        }
    }

    /// Everything the local API shows, as it is now.
    fn api_state(&self) -> api::State {
        let wanted = self.store.wanted();
        let mut order: Vec<_> = self.sessions.iter().collect();
        order.sort_by_key(|(_, s)| s.id);
        let lead = self
            .roles
            .main
            .and_then(|key| self.sessions.get(&key))
            .or_else(|| order.first().map(|(_, s)| *s));
        let headsets = {
            let trusted = self.security.trusted.lock().unwrap();
            let mut keys: Vec<[u8; 32]> = order.iter().map(|(key, _)| **key).collect();
            keys.extend(
                trusted
                    .entries()
                    .iter()
                    .map(|(key, _)| *key)
                    .filter(|key| !self.sessions.contains_key(key)),
            );
            keys.iter()
                .map(|key| {
                    let access = wanted.access_of(key);
                    let session = self.sessions.get(key);
                    let seen = self.store.last_seen(key);
                    api::Headset {
                        key: prostir_proto::secure::fingerprint(key),
                        name: trusted.label(key).unwrap_or_default().to_owned(),
                        model: session.map(|s| s.caps().model.clone()).unwrap_or_default(),
                        app: seen.map(|s| s.app.clone()).unwrap_or_default(),
                        last_seen: seen.map(|s| s.at),
                        role: if access.is_main() {
                            api::Role::Main
                        } else {
                            api::Role::Guest
                        },
                        rights: crate::store::perm_words(access.perms),
                        session: session.map(crate::session::Session::api_session),
                    }
                })
                .collect()
        };
        let (displays, primary) = match &self.displays {
            Some(d) => {
                // As it is on the PC; what the leading headset cannot show, as it was
                // told, failed (by its own decoders and streams, never by a stand-in).
                let part = d.pc_view();
                let lead_part = lead.map(|s| d.displays_part(s.caps()));
                let now = Instant::now();
                let list = part
                    .displays
                    .iter()
                    .map(|w| {
                        let id = ScreenId::from_wire(w.id);
                        let output = d.output_of(id);
                        let failed_there = lead_part
                            .as_ref()
                            .and_then(|p| p.get(w.id))
                            .filter(|t| matches!(t.state, wire::DisplayState::Failed { .. }));
                        let mut w = w.clone();
                        if let Some(t) = failed_there {
                            w.state = t.state.clone();
                        }
                        // A monitor just added stands nowhere until the headset placed
                        // it: a map then shows it once, where it stays.
                        let placing = !d.desk_known(id, now);
                        if placing {
                            w.desk = None;
                        }
                        api::Display {
                            placing,
                            ..api_display(&w, output.unwrap_or_default())
                        }
                    })
                    .collect();
                (list, part.primary.map(|p| p.0))
            }
            None => self.saved_displays(),
        };
        let limits = match (&self.displays, lead) {
            (Some(d), Some(s)) => s.limits(d),
            (Some(d), None) => pc_limits(d.codec()),
            (None, _) => pc_limits(wanted.codec),
        };
        let mut sizes: Vec<[u32; 2]> = Vec::new();
        for m in &limits.sizes {
            if !sizes.contains(&[m.w, m.h]) {
                sizes.push([m.w, m.h]);
            }
        }
        let mic_session = self
            .roles
            .mic
            .and_then(|key| self.sessions.get(&key))
            .filter(|s| s.mic_on());
        let mic = self
            .displays
            .as_ref()
            .map(|d| d.sound_part(mic_session.is_some()).mic);
        let (mic, mic_failure) = match mic {
            Some(wire::MicDevice::Ready) => (api::MicState::Ready, None),
            Some(wire::MicDevice::Failed { error, detail }) => (
                api::MicState::Failed,
                Some(api::Problem {
                    code: api::of_code(error),
                    text: detail,
                }),
            ),
            _ => (api::MicState::Off, None),
        };
        let mut problems = Vec::new();
        if self
            .sessions
            .values()
            .any(|s| s.api_session().mic_allowed == Some(false) && s.mic_on())
        {
            problems.push(api::Problem {
                code: ApiError::NotAllowed,
                text: t(
                    "Система шолома не дозволяє записувати мікрофон",
                    "The headset's system does not let it record its microphone",
                )
                .into(),
            });
        }
        if let Some(f) = &mic_failure {
            problems.push(f.clone());
        }
        if self
            .displays
            .as_ref()
            .is_some_and(crate::displays::Displays::desk_stuck)
        {
            problems.push(api::Problem {
                code: ApiError::Desktop,
                text: crate::session::desktop_not_responding(),
            });
        }
        let codec = |c: Codec| match c {
            Codec::H264 => api::Codec::H264,
            Codec::H265 => api::Codec::H265,
            Codec::Av1 => api::Codec::Av1,
        };
        // How each display that streams encodes, in the order of `displays`.
        let mut outputs: Vec<api::OutputEncoding> = self
            .displays
            .as_ref()
            .map_or_else(Vec::new, crate::displays::Displays::encoding_report)
            .into_iter()
            .map(|o| api::OutputEncoding {
                display: o.id.wire().0,
                encoder: o.encoder.to_owned(),
                codec: codec(o.codec),
                hardware: o.hardware,
                copies: o.copies,
                note: o.note,
                stats: o.stats.map(|s| api::StreamStats {
                    window_ms: s.window_ms,
                    frames: s.frames,
                    repeats: s.repeats,
                    bytes: s.bytes,
                    keyframes: s.keyframes,
                    keyframe_max: s.keyframe_max,
                    encode_us_avg: s.encode_us_avg,
                    encode_us_max: s.encode_us_max,
                }),
            })
            .collect();
        outputs.sort_by_key(|o| displays.iter().position(|d| d.id == o.display));
        api::State {
            v: api::STATE_VERSION,
            pc: api::Pc {
                name: hostname().to_owned(),
                app: prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
                desktop: self
                    .displays
                    .as_ref()
                    .map(|d| d.desktop_name().to_owned())
                    .unwrap_or_default(),
                phase: match self.phase {
                    brain::Phase::Idle => api::Phase::Idle,
                    brain::Phase::Live(_) => api::Phase::Live,
                    brain::Phase::Lingering { .. } => api::Phase::Lingering,
                },
            },
            window: wanted.window.clone(),
            autostart: platform::autostart::available().then(platform::autostart::enabled),
            headsets,
            connecting: None,
            displays,
            primary,
            limits: api::Limits {
                sizes,
                scales: limits.scales.clone(),
                displays_max: Some(u32::from(limits.virtual_max)),
                physical_max: Some(u32::from(limits.physical_max)),
                streams_max: Some(u32::from(limits.streams_max)),
                min: Some([limits.min.0, limits.min.1]),
                max: Some([limits.max.0, limits.max.1]),
            },
            encoding: api::Encoding {
                bitrate: wanted.bitrate,
                sharp: wanted.sharp,
                codec: codec(wanted.codec),
                cpu: wanted.software,
                using: self.displays.as_ref().map(|d| {
                    let (c, software) = d.encoding_now();
                    api::Using {
                        codec: codec(c),
                        hardware: !software,
                    }
                }),
                outputs,
            },
            sound: api::Sound {
                to_headset: wanted.audio,
                volume: lead.and_then(crate::session::Session::shared_volume),
                mic_on: mic_session.is_some(),
                mic,
                mic_failure,
            },
            pairing: self.door.pairing_state(
                Instant::now(),
                pairing::code_shown(),
                pairing::code_for(),
            ),
            nearby: self.door.nearby(),
            counting: crate::usage::possible().then_some(wanted.count),
            problems,
        }
    }

    /// The displays of the set kept for the next headset, none of them on the desktop
    /// now, in the order of the rows.
    fn saved_displays(&self) -> (Vec<api::Display>, Option<u32>) {
        let set = self.store.screens();
        let mut order: Vec<ScreenId> = set.rows.iter().flatten().map(|e| e.id).collect();
        for id in set.specs.keys().chain(set.physical.keys()) {
            if !order.contains(id) {
                order.push(*id);
            }
        }
        let mut monitors = 0;
        let list = order
            .into_iter()
            .filter_map(|id| {
                if let Some(spec) = set.specs.get(&id) {
                    monitors += 1;
                    return Some(api::Display {
                        id: id.number(),
                        kind: api::DisplayKind::Virtual,
                        name: format!("Prostir-{monitors}"),
                        mode: api::Mode {
                            w: spec.width,
                            h: spec.height,
                            scale: spec.percent(),
                        },
                        state: api::DisplayState::Kept,
                        ..api::Display::default()
                    });
                }
                let physical = set.physical.get(&id)?;
                Some(api::Display {
                    id: id.number(),
                    kind: api::DisplayKind::Physical,
                    name: physical.key.connector.clone(),
                    output: physical.key.connector.clone(),
                    builtin: physical.builtin,
                    shown: physical.shown,
                    state: api::DisplayState::Absent,
                    ..api::Display::default()
                })
            })
            .collect();
        (list, set.primary.map(ScreenId::number))
    }
}

/// A display as the headset protocol has it, for the local API.
fn api_display(d: &wire::Display, output: String) -> api::Display {
    let (kind, builtin, shown) = match &d.kind {
        wire::DisplayKind::Virtual => (api::DisplayKind::Virtual, false, true),
        wire::DisplayKind::Window { .. } => (api::DisplayKind::Window, false, true),
        wire::DisplayKind::Physical { builtin, shown, .. } => {
            (api::DisplayKind::Physical, *builtin, *shown)
        }
    };
    let (state, failure) = match &d.state {
        wire::DisplayState::Starting => (api::DisplayState::Starting, None),
        wire::DisplayState::Running => (api::DisplayState::Running, None),
        wire::DisplayState::Absent => (api::DisplayState::Absent, None),
        wire::DisplayState::Failed { error, detail } => (
            api::DisplayState::Failed,
            Some(api::Problem {
                code: api::of_code(*error),
                text: detail.clone(),
            }),
        ),
    };
    api::Display {
        id: d.id.0,
        kind,
        name: d.name.clone(),
        output,
        builtin,
        shown,
        mode: api::Mode {
            w: d.mode.w,
            h: d.mode.h,
            scale: d.mode.scale,
        },
        desk: d.desk.map(|r| api::DeskRect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }),
        placing: false,
        state,
        failure,
    }
}

/// This program, to start again (the window, the service): as a wrapper (Nix) names
/// it, else this very file, also after an update replaced it.
use prostir_window::launch::this_program;

/// Opens the settings window (one per user: a second start brings the first one
/// forward): `prostir window`, a process of its own, so the service never starts the
/// window's toolkit or loads its graphics libraries (EGL, X11), and a window that
/// crashes takes nothing else down. Its process is waited for on a thread of its own,
/// which sleeps until it closes.
pub fn open_settings() -> Result<(), Failure> {
    // A test build never opens a real window on the desktop the tests run on.
    if cfg!(feature = "fake-desktop") {
        info!("fake desktop: the settings window opens");
        return Ok(());
    }
    let failed = |e: std::io::Error| Failure::new(ApiError::Other, format!("prostir window: {e}"));
    let mut window = std::process::Command::new(this_program().map_err(failed)?);
    window.arg("window").stdin(std::process::Stdio::null());
    let mut child = window.spawn().map_err(failed)?;
    std::thread::Builder::new()
        .name("settings-window".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|e| Failure::new(ApiError::Other, e.to_string()))?;
    Ok(())
}

/// Wakes Prostir for the settings window (`prostir`, Prostir not on the bus): the
/// sockets' watcher starts it for the window (SIGUSR1); else systemd starts it, when
/// its socket waits for the headset; else it starts anew, its errors where this
/// program's go.
pub fn wake_for_window() -> std::io::Result<crate::ctl::Woken> {
    wake(|| start_detached(std::process::Stdio::inherit()).map(crate::ctl::Woken::Started))
}

/// Wakes Prostir for an agent (the tool `start` of `prostir mcp`), as for the window,
/// but nothing it starts writes to the agent's pipes: a new Prostir starts in the
/// user's systemd manager (`systemd-run --user`, its log in the journal) where there
/// is one, else on its own with nothing on its stdio. Without a systemd user manager
/// and without the desktop session's variables, nothing could show its monitors, so
/// it is not started at all.
pub fn wake_for_agent() -> std::io::Result<crate::ctl::Woken> {
    use crate::ctl::Woken;
    wake(|| {
        if platform::autostart::start_in_user_manager(&["serve", "--open-settings"]) {
            return Ok(Woken::Asleep { opens_window: true });
        }
        let has = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        if !has("XDG_RUNTIME_DIR") || !(has("WAYLAND_DISPLAY") || has("DISPLAY")) {
            return Err(std::io::Error::other(
                "this program has no desktop session (no XDG_RUNTIME_DIR, or no \
                 WAYLAND_DISPLAY nor DISPLAY) and there is no systemd user manager: \
                 give the MCP server the session's variables, or start Prostir from the \
                 desktop",
            ));
        }
        start_detached(std::process::Stdio::null()).map(Woken::Started)
    })
}

/// Wakes a sleeping Prostir (its sockets' watcher, systemd's socket), else `start`s
/// it.
fn wake(
    start: impl FnOnce() -> std::io::Result<crate::ctl::Woken>,
) -> std::io::Result<crate::ctl::Woken> {
    use crate::ctl::Woken;
    if let Some(pid) = platform::watcher() {
        // SAFETY: a signal to a process of this user, which takes it.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) } == 0 {
            return Ok(Woken::Asleep { opens_window: true });
        }
    }
    if platform::autostart::start_service() {
        return Ok(Woken::Asleep {
            opens_window: false,
        });
    }
    start()
}

/// Starts `prostir serve --open-settings` on its own, apart from this terminal, its
/// stderr to `errors` (nothing goes to stdin and stdout).
fn start_detached(errors: std::process::Stdio) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(this_program()?);
    command
        .args(["serve", "--open-settings"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(errors);
    // SAFETY: setsid is async-signal-safe and touches nothing of the parent's.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log says what a request asks and with what, never a pairing code.
    #[test]
    fn the_log_says_what_was_asked() {
        let sound = logged(&Request::SoundToHeadset { on: false });
        assert!(sound.contains(r#""request":"sound_to_headset""#), "{sound}");
        assert!(sound.contains(r#""on":false"#), "{sound}");
        let code = logged(&Request::PairingCode {
            code: "482913".into(),
        });
        assert!(
            code.contains("pairing_code") && !code.contains("482913"),
            "{code}"
        );
    }

    /// A screenshot waits for its job of its own worker: the same job number of the
    /// next worker is another job. Those over their time are taken, then all.
    #[test]
    fn screenshots_wait_for_their_own_worker() {
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = |n: u32| {
            let tx = tx.clone();
            Reply::new(move |_| tx.send(n).unwrap())
        };
        let now = Instant::now();
        let later = now + std::time::Duration::from_secs(20);
        let mut shots = Shots::default();
        shots.push(1, JobId(5), reply(1), later);
        shots.push(2, JobId(6), reply(2), now);
        shots.push(2, JobId(7), reply(3), later);
        assert_eq!(shots.due(), Some(now));
        assert!(
            shots.take(2, JobId(5)).is_none(),
            "worker 2 has no job 5 here"
        );
        shots.take(1, JobId(5)).unwrap().send(CtlReply::Done);
        assert_eq!(rx.try_recv(), Ok(1));
        assert!(shots.take(1, JobId(5)).is_none(), "answered once");
        let over = shots.over(now, false);
        assert_eq!(
            over.iter().map(|(g, j, _)| (*g, j.0)).collect::<Vec<_>>(),
            [(2, 6)]
        );
        drop(over);
        assert_eq!(rx.try_recv(), Ok(2), "a reply dropped still answers");
        assert_eq!(shots.over(now, true).len(), 1);
        assert_eq!(shots.due(), None);
    }

    /// A command whose arguments the protocol does not carry is refused before it
    /// goes; the longest that fits goes.
    #[test]
    fn commands_fit_the_wire() {
        let profile = |n: usize| Command::Profile {
            name: "я".repeat(n),
        };
        assert!(fits_the_wire(&profile(32)).is_ok(), "64 bytes");
        let long = fits_the_wire(&profile(33)).unwrap_err();
        assert_eq!(long.error, ApiError::BadArgument);
        assert!(fits_the_wire(&Command::Recenter).is_ok());
        assert!(fits_the_wire(&Command::Distance { steps: -128 }).is_ok());
    }
}
