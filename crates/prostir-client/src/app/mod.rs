//! The headset's state and its rules (step 7, 4.1): what the menu, the PC and the
//! hands change, and what follows from it. Until step 7.7 is done it still holds the
//! shell's handles (streams, the microphone, the outboxes) and runs on the headset only.

mod actions;
mod session;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Vec2, Vec3};
use log::{info, warn};
use prostir_proto::ClientMsg;
use prostir_proto::request::{Outcome, Request};
use prostir_proto::state::{
    self, Battery, DisplayId, HeadsetState, PcState, Prefs, Presence, ReqId, caps,
};

pub(crate) use actions::{By, Forgetting, handle_action};
pub(crate) use session::{handle_net_event, take_command};

use crate::audio::Gains;
use crate::ids::{DisplayKey, PcId};
use crate::lang::{Lang, LangState, Moment, Texts};
use crate::layout::{self, Grab, Placement, Ray};
use crate::main_loop::{Monitor, Scaling, local_time, moment};
use crate::mic::Mic;
use crate::net::Feedback;
use crate::note::{self, Note, Notes};
use crate::pending;
use crate::presence;
use crate::profiles::{self, Profile};
use crate::session::{Attempt, Session};
use crate::settings::Settings;
use crate::store::UpdateHint;
use crate::ui::{self, Action, MonitorEntry, Tab, UiState};
use crate::world::World;
use crate::{persist, physical};

/// The time and the date on the palm menu, written again only when the minute, the
/// language or how they are written changes (not every frame).
#[derive(Default)]
pub(crate) struct ClockFace {
    /// What the texts were written for.
    pub(crate) shown: Option<(Moment, bool, crate::settings::DateStyle, Texts)>,
    /// Shared with the menu's state, which is made anew every frame.
    pub(crate) clock: Arc<str>,
    pub(crate) date: Arc<str>,
}

impl ClockFace {
    pub(crate) fn update(
        &mut self,
        now: Option<Moment>,
        twelve: bool,
        style: crate::settings::DateStyle,
        texts: &Texts,
    ) {
        let Some(now) = now else {
            if self.shown.take().is_some() {
                self.clock = Arc::default();
                self.date = Arc::default();
            }
            return;
        };
        let same = self
            .shown
            .as_ref()
            .is_some_and(|(m, t, s, l)| *m == now && *t == twelve && *s == style && l == texts);
        if !same {
            self.clock = texts.clock(twelve, &now).into();
            self.date = texts.date(style, &now).into();
            self.shown = Some((now, twelve, style, texts.clone()));
        }
    }
}

/// While the app is put away, how often the screen is read (see
/// `system::interactive`) until the headset is known to be asleep.
pub(crate) const SCREEN_EVERY: Duration = Duration::from_millis(250);
pub(crate) const ERROR_SHOWN_FOR: Duration = Duration::from_secs(8);
pub(crate) const CONFIRM_FOR: Duration = Duration::from_secs(4);

/// How long the gaze rests on another monitor before the mouse goes there.
pub(crate) const GAZE_DWELL: Duration = Duration::from_millis(600);
/// Everything shown in the headset and how it is arranged.
pub(crate) struct Scene {
    pub(crate) monitors: HashMap<DisplayKey, Monitor>,
    /// Where everything stands in the room, and the rules it is arranged by.
    pub(crate) world: World,
    pub(crate) mirror_wanted: bool,
    pub(crate) mic_wanted: bool,
    /// Resizing a monitor with both hands.
    pub(crate) scaling: Option<Scaling>,
    /// Whether the headset is on the head, from the session, the activity and the
    /// headset's sensor; streaming pauses otherwise.
    pub(crate) presence: presence::Presence,
    pub(crate) settings: Settings,
    pub(crate) panel_open: bool,
    pub(crate) grabs: [Option<Grab>; 2],
    /// IP typed on the menu keypad.
    pub(crate) ip_input: Option<String>,
    /// The connections to PCs: the search, and each PC's link with its state and the
    /// menu's requests to it (see `session`).
    pub(crate) session: Session,
    /// Messages to each PC in a session.
    pub(crate) outboxes: BTreeMap<PcId, Feedback>,
    /// The microphone permission the PC was last told.
    pub(crate) mic_permission: u8,
    /// The microphone's switch and permission, shared with the network thread.
    pub(crate) mic: Arc<Mic>,
    /// Where the PC's sound comes from, for the audio thread.
    pub(crate) gains: Arc<Gains>,
    pub(crate) error: Option<(String, Instant)>,
    /// "×" pressed once on this monitor; a second press within a few seconds removes it.
    pub(crate) confirm_remove: Option<(DisplayKey, Instant)>,
    /// "Save current" pressed once for this profile.
    pub(crate) confirm_save: Option<(usize, Instant)>,
    pub(crate) confirm_forget: Option<(usize, Instant)>,
    /// Menu page shown.
    pub(crate) tab: Tab,
    /// The detached window whose "Attach" button shows, and when it was pointed at.
    pub(crate) attach_for: Option<(DisplayKey, Instant)>,
    /// The monitor picked on the menu's map.
    pub(crate) selected: Option<DisplayKey>,
    /// The headset's battery (percent, charging), read once a minute.
    pub(crate) headset_battery: Option<(u8, bool)>,
    /// The headset's model as the system names it, for `HeadsetCaps`.
    pub(crate) model: String,
    /// The display rate setting changed: apply it.
    pub(crate) refresh_due: bool,
    /// Time worn since the last rest reminder.
    pub(crate) worn_for: Duration,
    /// Since when the headset is off the head (for rest reminders).
    pub(crate) off_since: Option<Instant>,
    /// Mouse follows the gaze: the monitor looked at and since when.
    pub(crate) gaze_on: Option<(DisplayKey, Instant)>,
    /// When the mouse was last sent somewhere by the gaze.
    pub(crate) gaze_moved: Option<Instant>,
    /// "Where is the mouse": monitor, point across its image, since when.
    pub(crate) finder: Option<(DisplayKey, Vec2, Instant)>,
    /// A short message shown on its own: text, since when, where (`None` until placed).
    pub(crate) toast: Option<(String, Instant, Option<Placement>)>,
    /// What the open menu says under the button pressed (see `note`).
    pub(crate) notes: Notes,
    /// The press being done is the main menu's (not the palm menu's, nor a command of
    /// the PC): what it comes to is said in the menu.
    pub(crate) press_in_menu: bool,
    /// The user chose a language in the menu: said once it shows.
    pub(crate) language_asked: bool,
    /// A PC being forgotten, until the network thread says it is (or why not).
    pub(crate) forgetting: Option<Forgetting>,
    /// Head position and gaze this frame.
    pub(crate) head: (Vec3, Vec3),
    /// The head's right, rolled and tilted with it (where the right ear is).
    pub(crate) head_right: Vec3,
    /// Where the eyes are, for the map in the menu: moves only when the head has moved
    /// a few centimetres (the menu redraws when its picture changes).
    pub(crate) view_eye: Option<Vec3>,
    /// Tracked hands are in view this frame (the menu then opens within reach).
    pub(crate) hands_in_view: bool,
    /// The headset's own system language.
    pub(crate) system_lang: Option<Lang>,
    /// The menu's language: what it shows, and the one it waits for.
    pub(crate) lang: LangState,
    pub(crate) clock: ClockFace,
    /// Which languages the Language page shows, when there are many.
    pub(crate) language_page: u8,
    pub(crate) profiles: profiles::Slots,
    /// The profile last switched to, until something is changed by hand.
    pub(crate) active_profile: Option<usize>,
    /// The files changed in this pass, handed to the files' writer after it (see
    /// `save_due`).
    pub(crate) saves: persist::Dirty,
    /// The files whose failed save the menu told of, once each.
    pub(crate) save_failures_told: persist::Dirty,
    /// The files' writer of this start. Last, and stopped when the scene goes (see
    /// `Drop`), however `run` ends.
    pub(crate) saver: persist::Saver,
}

impl Drop for Scene {
    /// What the last pass changed is written, and the files' writer ends, before the
    /// app does, also when `run` ends early with an error or a panic.
    fn drop(&mut self) {
        self.save_due();
        self.saver.stop(persist::Saver::STOPS_WITHIN);
    }
}

impl Scene {
    /// Display `id` of the PC the menu acts on (see `Session::main`).
    pub(crate) fn key(&self, id: DisplayId) -> Option<DisplayKey> {
        self.session.main().map(|pc| DisplayKey::new(pc, id))
    }

    /// The main PC's state (see `Session::state`).
    pub(crate) fn pc(&self) -> &PcState {
        self.session.state()
    }

    /// Display `id` as its PC's list says (`None`: not listed, or no session with its
    /// PC).
    pub(crate) fn display(&self, id: DisplayKey) -> Option<&state::Display> {
        self.session.pc(id.pc)?.state.displays.as_ref()?.get(id.id)
    }

    /// The PC can move the mouse (for following the gaze).
    pub(crate) fn can_move_mouse(&self) -> bool {
        self.pc()
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::MOVE_MOUSE))
    }

    /// The monitor the mouse is on, as the PC says.
    pub(crate) fn focus(&self) -> Option<DisplayKey> {
        self.key(self.pc().focus.as_ref()?.display?)
    }

    /// The display of the PC itself the switch shows or hides: one connected now,
    /// never a closed laptop's panel (see [`physical::switch`]).
    pub(crate) fn physical_id(&self) -> Option<DisplayKey> {
        self.key(physical::switch(self.pc().displays.as_ref()?)?.id)
    }

    /// What the switch's display is called, if a display of the PC is connected.
    pub(crate) fn physical_name(&self) -> Option<String> {
        physical::switch(self.pc().displays.as_ref()?).map(|d| physical::label(d, self.texts()))
    }

    /// What the world asks for after it changed: the PC is told the rows of its
    /// desktop (see `World::settle`), and a hand lets go of what the world no longer
    /// has it hold (laid out anew, or gone).
    pub(crate) fn world_done(&mut self) {
        while let Some((pc, rows)) = self.world.take_arrange() {
            // Only to a PC in a session: the world sends nothing otherwise.
            if self.session.connected(pc) {
                self.send_to(pc, ClientMsg::Arrange { rows });
            }
        }
        for grab in &mut self.grabs {
            if grab.is_some_and(|g| !self.world.holds(g.id)) {
                *grab = None;
            }
        }
        if self
            .scaling
            .is_some_and(|s| self.world.resizing() != Some(s.id))
        {
            self.scaling = None;
        }
    }

    /// The setup as it is now (see `Profile::of`): the monitors left to right with
    /// their scales, the primary, the PC's own displays shown, and where they stand (in
    /// cinema, the desk arrangement as kept before it).
    pub(crate) fn current_profile(&self) -> Profile {
        let shown = self
            .pc()
            .displays
            .iter()
            .flat_map(|d| &d.displays)
            .filter(|d| d.is_shown_physical())
            .filter_map(|d| self.key(d.id))
            .collect();
        self.world
            .profile(shown, |k| self.display(k).map_or(100, |d| d.mode.scale))
    }

    /// The placeholder of a monitor being added goes once its picture is there, it
    /// failed, the connection ended, or it waited too long.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn adding_due(&mut self) {
        let waits = self.session.waits(ui::Asked::AddMonitor);
        self.world.adding_due(Instant::now(), waits);
    }

    /// Turns cinema mode on or off (see `World::set_cinema`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn set_cinema(&mut self, on: bool) {
        self.world
            .set_cinema(&self.settings, on, self.head.1, Instant::now());
        self.settings.cinema = on;
        self.world_done();
    }

    /// The files changed in this pass (`saves`, and those the world changed), handed
    /// to the files' writer: each one's text written into the writer's buffer for it,
    /// once, whatever changed it how often. The writing itself is done on the writer's
    /// thread.
    pub(crate) fn save_due(&mut self) {
        use persist::File;
        self.saves.add(self.world.take_saves());
        for file in self.saves.take() {
            match file {
                File::Settings => self.saver.save(file, |out| self.settings.write_text(out)),
                File::Desk => self
                    .saver
                    .save(file, |out| self.world.desk().write_text(out)),
                File::Profiles => self
                    .saver
                    .save(file, |out| profiles::write_text(out, &self.profiles)),
                File::Keyboard => {
                    if let Some(place) = &self.world.keyboard() {
                        self.saver
                            .save(file, |out| layout::write_keyboard(out, place));
                    }
                }
            }
        }
    }

    /// Hands over what changed and waits until it is written (for
    /// [`persist::Saver::STOPS_WITHIN`] at most): the app is `why` (paused, or the
    /// headset taken off), and may not come back.
    pub(crate) fn flush_saves(&mut self, why: &str) {
        self.save_due();
        if !self.saver.flush(persist::Saver::STOPS_WITHIN) {
            warn!(
                "{why}: the files were not all written within {} ms",
                persist::Saver::STOPS_WITHIN.as_millis()
            );
        }
    }

    /// What the files' writer said: a file that could not be saved is told once in
    /// this start (the next save tries again, and the log says each time), and files
    /// that failed together in one message.
    pub(crate) fn saves_told(&mut self) {
        let mut failed = persist::Dirty::default();
        let mut first: Option<(persist::File, String)> = None;
        while let Some(event) = self.saver.try_recv() {
            match event {
                persist::PersistEvent::Failed { file, error } => {
                    if !self.save_failures_told.has(file) && !failed.has(file) {
                        failed.mark(file);
                        first.get_or_insert((file, error));
                    }
                }
            }
        }
        let Some((file, error)) = first else {
            return;
        };
        let mut several = 0;
        for f in failed.take() {
            self.save_failures_told.mark(f);
            several += 1;
        }
        let id = if several > 1 {
            "error-save-files"
        } else {
            match file {
                persist::File::Settings => "error-save-settings",
                persist::File::Desk => "error-save-desk",
                persist::File::Profiles => "error-save-profiles",
                persist::File::Keyboard => "error-save-keyboard",
            }
        };
        let text = self.texts().f(id, &[("error", error.into())]);
        // The log has it already, from the writer.
        self.put_error(text);
    }

    /// PC `pc` was forgotten: the places of its displays go (see `World::forget_pc`),
    /// and no profile shows its displays any more. Paired again later, its displays
    /// are laid out afresh.
    pub(crate) fn forget_pc(&mut self, pc: PcId) {
        self.world.forget_pc(pc);
        if profiles::forget_pc(&mut self.profiles, pc) {
            self.saves.mark(persist::File::Profiles);
        }
    }

    /// Mouse follows the gaze: after resting on another monitor for a moment, the
    /// mouse goes to where the user looks on it (as drawn).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn follow_gaze(&mut self) {
        let busy = self.world.wrist().is_some() || self.world.holding() || self.settings.cinema;
        let main = self.session.main();
        if !self.settings.gaze_mouse || !self.can_move_mouse() || main.is_none() || busy {
            self.gaze_on = None;
            return;
        }
        let (head, forward) = self.head;
        let ray = Ray {
            origin: head,
            dir: forward.normalize_or(-Vec3::Z),
        };
        let session = &self.session;
        let looked = self
            .world
            .looked_at(ray, &self.settings, self.panel_open, |k| {
                session.waits(ui::Asked::Remove(k))
            });
        let Some((id, uv)) = looked else {
            self.gaze_on = None;
            return;
        };
        if self.focus() == Some(id) {
            self.gaze_on = None;
            return;
        }
        match self.gaze_on {
            Some((on, since)) if on == id => {
                let recently = self
                    .gaze_moved
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
                if since.elapsed() >= GAZE_DWELL && !recently {
                    let f = |v: f32| (v * 65535.0) as u16;
                    info!("gaze: mouse to monitor {id} at {:.2},{:.2}", uv.x, uv.y);
                    self.send_to(
                        id.pc,
                        ClientMsg::MouseTo {
                            id: id.id,
                            u: f(uv.x),
                            v: f(uv.y),
                        },
                    );
                    // Where the mouse is now, until the PC says it.
                    if let Some(p) = self.session.pc_mut(id.pc) {
                        p.state.focus = Some(state::Focus {
                            display: Some(id.id),
                        });
                    }
                    self.gaze_moved = Some(Instant::now());
                    self.gaze_on = None;
                }
            }
            _ => self.gaze_on = Some((id, Instant::now())),
        }
    }

    /// Rest reminders: after so many minutes of wearing, a short message. Every 20
    /// minutes it is the 20-20-20 rule (look 6 m away for 20 seconds); hourly, a break.
    /// Taking the headset off for 5 minutes counts as a rest.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn remind_rest(&mut self, dt: f32) {
        let every = u64::from(self.settings.rest_every) * 60;
        if every == 0 {
            return;
        }
        if !self.presence.worn() {
            self.off_since.get_or_insert_with(Instant::now);
            return;
        }
        if self
            .off_since
            .take()
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(5 * 60))
        {
            self.worn_for = Duration::ZERO;
        }
        self.worn_for += Duration::from_secs_f32(dt);
        if self.worn_for.as_secs() < every {
            return;
        }
        self.worn_for = Duration::ZERO;
        let id = if every <= 20 * 60 {
            "rest-eyes"
        } else {
            "rest-hour"
        };
        self.show_toast(self.texts().t(id).to_owned());
    }

    /// "Sound from the screen": the sound placed towards the monitor whose window plays
    /// it, as the head turns.
    pub(crate) fn place_sound(&self) {
        let from = self.pc().sound_from.as_ref().and_then(|s| s.display);
        let angle = from
            .and_then(|id| self.key(id))
            .filter(|_| self.settings.sound_from_screen)
            .and_then(|id| self.world.shown(id))
            .map(|p| {
                // In the head's own terms, so lying down or tilting the head keeps the
                // ears right: how far towards the right ear, and how far in front.
                let (head, forward) = self.head;
                let to = (p.position - head).normalize_or_zero();
                // Right of where the user looks is positive.
                to.dot(self.head_right).atan2(to.dot(forward))
            });
        self.gains.place(angle);
    }

    /// Shows `text` for a few seconds, below the line of sight.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn show_toast(&mut self, text: String) {
        // Not the text: it can be a PC notification (a message, a code).
        info!("toast shown");
        self.toast = Some((text, Instant::now(), None));
    }

    /// Profile `i` as the menu names it ("Profile 2").
    pub(crate) fn profile_name(&self, i: usize) -> String {
        self.texts().f("profile-n", &[("n", (i + 1).into())])
    }

    /// The monitor most straight ahead of the head (not a window panel).
    pub(crate) fn monitor_in_front(&self) -> Option<DisplayKey> {
        let session = &self.session;
        self.world
            .monitor_in_front(self.head, |k| session.waits(ui::Asked::Remove(k)))
    }

    /// Turns the whole arrangement around the user (see `World::recenter`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn recenter(&mut self) {
        self.world
            .recenter(&self.settings, self.head, Instant::now());
        self.world_done();
    }

    /// Brings the monitors closer or further (see `World::closer`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn closer(&mut self, steps: i8) {
        self.world.closer(steps, Instant::now());
        self.world_done();
    }

    /// The A button (see `World::tidy_up`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn tidy_up(&mut self) {
        self.world.tidy_up(&self.settings, Instant::now());
        self.world_done();
    }

    /// Opens the menu in front of the user: within arm's reach, a little lower, when
    /// hands are in view (to touch it), further away for controllers.
    pub(crate) fn open_panel(&mut self, head: Vec3, forward: Vec3, touch: bool) {
        self.world.open_menu((head, forward), touch, &self.settings);
        self.panel_open = true;
        // What was said while it was closed came as a toast.
        self.notes.clear();
    }

    /// Tells the PC whether the headset is worn (quietly: nothing to show the user),
    /// and keeps it for the next `Open`.
    pub(crate) fn tell_presence(&mut self) {
        let state = self.headset_state();
        self.session.set_headset(state);
        self.tell_all(&ClientMsg::Presence {
            part: Presence {
                worn: self.presence.worn(),
            },
        });
    }

    /// Tells every PC in a session `msg` (about the headset itself, the same to each),
    /// quietly.
    pub(crate) fn tell_all(&self, msg: &ClientMsg) {
        for f in self.outboxes.values() {
            let _ = f.send(msg.clone());
        }
    }

    /// Tells the PC once the signals say the headset was put on, taken off or put to
    /// sleep (asleep, the PC need not wait for it to come back), and lets the network
    /// thread connect only while it is worn.
    pub(crate) fn presence_due(&mut self, now: Instant) {
        let Some(state) = self.presence.update(now) else {
            return;
        };
        info!("headset {state:?}");
        self.tell_presence();
        if state == presence::State::Asleep {
            self.tell_all(&ClientMsg::Asleep);
        }
        self.session.set_worn(self.presence.worn());
    }

    /// Reads the screen where presence wants it (only while the app is put away, at
    /// once and then at most every `SCREEN_EVERY`; see `Presence::wants_screen`),
    /// then tells the PC what changed.
    pub(crate) fn presence_now(&mut self, now: Instant, screen_read: &mut Option<Instant>) {
        if !self.presence.wants_screen() {
            *screen_read = None;
        } else if screen_read.is_none_or(|at| now.saturating_duration_since(at) >= SCREEN_EVERY) {
            *screen_read = Some(now);
            if let Some(on) = crate::system::interactive() {
                self.presence.interactive(on);
            }
        }
        self.presence_due(now);
    }

    /// The battery was read (once a minute, more often on the palm menu): the menu
    /// shows it, the PC's tray hears it, and the next `Open` says it.
    pub(crate) fn battery_read(&mut self, battery: Option<(u8, bool)>) {
        self.headset_battery = battery;
        let state = self.headset_state();
        self.session.set_headset(state);
        if let Some((percent, charging)) = battery {
            self.tell_all(&ClientMsg::Battery {
                part: Battery { percent, charging },
            });
        }
    }

    /// The headset's settings the PC acts on.
    pub(crate) fn prefs(&self) -> Prefs {
        prefs_of(&self.settings)
    }

    /// What the headset says about itself when it opens a session.
    pub(crate) fn headset_state(&self) -> HeadsetState {
        headset_state(
            &self.settings,
            self.presence.worn(),
            self.headset_battery,
            &self.mic,
            &self.model,
        )
    }

    /// The microphone's switch or permission changed: the PC hears at once (it makes
    /// its end of the microphone, or says there is no permission), and the next session
    /// starts with it.
    pub(crate) fn mic_changed(&mut self) {
        let state = self.headset_state();
        self.session.set_headset(state);
        self.mic_permission = self.mic.permission();
        let part = mic_state(&self.settings, &self.mic);
        self.tell_all(&ClientMsg::UpState { part });
    }

    /// The settings the PC acts on changed: it hears at once, and the next session
    /// starts with them.
    pub(crate) fn prefs_changed(&mut self) {
        let state = self.headset_state();
        self.session.set_headset(state);
        let part = self.prefs();
        self.tell_all(&ClientMsg::Prefs { part });
    }

    /// Control `what` asks the PC to change something: one reply comes with its
    /// number, and the button pressed for it waits under itself meanwhile (the reply
    /// is said there, see `note`). Without a connection
    /// nothing is sent (and nothing kept to send later: repeating "add a monitor"
    /// after reconnecting could add two).
    pub(crate) fn request(&mut self, asked: Request, what: ui::Asked) -> Option<ReqId> {
        let pc = self.session.main();
        self.request_of(pc, asked, what)
    }

    /// [`Scene::request`] of PC `pc` (none: no PC, which fails as not connected).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn request_of(
        &mut self,
        pc: Option<PcId>,
        asked: Request,
        what: ui::Asked,
    ) -> Option<ReqId> {
        let Some((pc, req)) = pc.zip(self.session.ask(pc, what, Instant::now())) else {
            self.ask_failed(ui::Failure::NotConnected);
            return None;
        };
        info!("request {}: {asked:?}", req.0);
        self.send_to(pc, ClientMsg::request(req, asked));
        if let Some(on) = self.notes.asked(pc, req, what) {
            let waiting = Note::waiting(Some(on), self.texts());
            self.tell(waiting, false, true);
        }
        Some(req)
    }

    /// [`Scene::request`] about display `k`, of its PC with the number `k` has there:
    /// failed as not connected without a session with that PC.
    pub(crate) fn request_for(
        &mut self,
        k: DisplayKey,
        asked: impl FnOnce(DisplayId) -> Request,
        what: ui::Asked,
    ) -> Option<ReqId> {
        let pc = self.session.connected(k.pc).then_some(k.pc);
        self.request_of(pc, asked(k.id), what)
    }

    /// What the user reads for display `id`: as the PC's desktop names it.
    pub(crate) fn label_of(&self, id: DisplayKey) -> Option<String> {
        let d = self.display(id)?;
        Some(ui::display_label(&d.name))
    }

    /// Request `req` was answered: which control asked it, `None` for one this side
    /// does not wait for (answered after it gave up on it). What it comes to is said
    /// under the button that asked (see `note::answer`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn answered(
        &mut self,
        pc: PcId,
        req: ReqId,
        outcome: &Outcome,
    ) -> Option<ui::Asked> {
        let answered = self.session.answered(pc, req, outcome, Instant::now());
        // A command of the PC that waited for this answer gets it, with the same code.
        if let Some((command, outcome)) = answered.command {
            self.send_to(
                pc,
                ClientMsg::Reply {
                    req: command,
                    outcome,
                },
            );
        }
        let Some(what) = answered.what else {
            info!(
                "an answer to request {}, which nothing waits for (no answer within {} s)",
                req.0,
                pending::ANSWER_WITHIN.as_secs()
            );
            return None;
        };
        let took = answered.took.unwrap_or_default().as_millis();
        info!("request {} answered in {took} ms", req.0);
        if let Outcome::Failed { error, detail } = outcome {
            warn!("request {} failed (code {}): {detail}", req.0, error.code());
        }
        let asker = self.notes.answered(pc, req);
        if let Some(told) = note::answer(what, asker, outcome, self.texts()) {
            self.tell(told.note, told.toast, asker.on.is_some());
        }
        Some(what)
    }

    /// A request was not sent, for `why`: said under the button pressed for it.
    pub(crate) fn ask_failed(&mut self, why: ui::Failure) {
        let note = Note::failure(self.notes.pressed(), why, "", self.texts());
        self.tell_press(note, true);
    }

    /// Request `req` of PC `pc` got no answer and never will (`why`: no answer in time,
    /// the connection broke): said under the button that asked.
    pub(crate) fn given_up(&mut self, pc: PcId, req: ReqId, why: ui::Failure) {
        let on = self.notes.answered(pc, req).on;
        let note = Note::failure(on, why, "", self.texts());
        self.tell(note, true, on.is_some());
    }

    /// News worth a `toast` (or not), about a press `in_menu` (or the menu's own news):
    /// said where `note::said` puts it.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn tell(&mut self, note: Note, toast: bool, in_menu: bool) {
        match note::said(self.panel_open, in_menu, toast) {
            note::Said::Note => self.notes.say(note, Instant::now()),
            note::Said::Toast => self.show_toast(note.toast_text()),
            note::Said::Nothing => {}
        }
    }

    /// What the press being done came to: in the menu for a press of the menu, else a
    /// toast where it is worth one.
    pub(crate) fn tell_press(&mut self, note: Note, toast: bool) {
        self.tell(note, toast, self.press_in_menu);
    }

    /// The menu's own action `action` was done: said under its button, where what it
    /// changed does not show on the button itself (see `note::done_text`).
    pub(crate) fn done(&mut self, action: Action) {
        if let Some(id) = note::done_text(action) {
            let note = Note::done(Some(action), self.texts().t(id));
            self.tell_press(note, false);
        }
    }

    /// `action` needs a monitor in front of the user, and there is none.
    pub(crate) fn no_monitor_in_front(&mut self, action: Action) {
        let note = Note::failed(Some(action), self.texts().t("note-no-monitor-in-front"));
        self.tell_press(note, true);
    }

    /// The menu shows the language chosen (loaded, or the one it had): see
    /// `note::language_note`.
    pub(crate) fn language_shown(&mut self) {
        let asked = std::mem::take(&mut self.language_asked);
        let chosen = self.settings.language;
        let texts = self.texts();
        if let Some(told) = note::language_note(chosen, asked, texts.warning(), texts) {
            self.tell(told.note, told.toast, true);
        }
    }

    /// Requests without an answer for 30 s count as failed (a late answer is let go),
    /// and a note shown long enough goes; a profile switch waited for long enough is
    /// laid out with what streams.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn expire_asks(&mut self) {
        let now = Instant::now();
        for (pc, req, _) in self.session.expire(now) {
            self.world.switch_given_up(pc, req);
            self.given_up(pc, req, ui::Failure::TimedOut);
        }
        self.notes.expire(now);
        self.world.switch_due(&self.settings, now);
        self.world_done();
    }

    /// Sends `msg` to PC `pc`; the menu says when it cannot.
    pub(crate) fn send_to(&mut self, pc: PcId, msg: ClientMsg) {
        let sent = self.outboxes.get(&pc).map(|f| f.send(msg));
        match sent {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                let text = self
                    .texts()
                    .f("error-send", &[("error", e.to_string().into())]);
                self.show_error(text);
            }
            None => {
                let text = self.texts().t("error-not-connected").to_owned();
                self.show_error(text);
            }
        }
    }

    /// The menu's texts, in the language it shows now.
    pub(crate) fn texts(&self) -> &Texts {
        self.lang.texts()
    }

    /// The language the menu is to show, if it is neither shown nor already asked for:
    /// the `system` thread is to load it (chosen in the menu, else the PC's, else the
    /// headset's).
    pub(crate) fn wanted_lang(&mut self) -> Option<Lang> {
        let s = &self.settings;
        self.lang
            .wanted(s.language, s.pc_language, self.system_lang)
    }

    /// The time and the date on the palm menu, for this minute.
    pub(crate) fn tick_clock(&mut self) {
        let s = &self.settings;
        let now = local_time().map(|t| moment(&t));
        self.clock
            .update(now, s.clock_12h, s.date_style, self.lang.texts());
    }

    pub(crate) fn show_error(&mut self, text: String) {
        warn!("{text}");
        self.put_error(text);
    }

    /// [`Scene::show_error`] without the line in the log.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn put_error(&mut self, text: String) {
        self.error = Some((text, Instant::now()));
    }

    /// What the menu shows; `update_from`: where this build is updated (its store's).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    pub(crate) fn ui_state(
        &self,
        room: bool,
        keyboard: bool,
        panorama: bool,
        custom: bool,
        update_from: UpdateHint,
    ) -> UiState {
        let spots = self.world.map_spots();
        let search = self.session.search();
        UiState {
            host: self.session.host().map(str::to_owned),
            found: search
                .found
                .iter()
                .map(|h| (h.name.clone(), h.addr.ip().to_string()))
                .collect(),
            ip_input: self.ip_input.clone(),
            pairing: match &search.attempt {
                Some(Attempt::Pairing { view, .. }) => Some(view.clone()),
                _ => None,
            },
            pair_offer: match &search.attempt {
                Some(Attempt::PairOffer { label, why, .. }) => Some((label.clone(), *why)),
                _ => None,
            },
            unreachable: search.unreachable.clone(),
            pc: self.pc().encoding.clone(),
            limits: self.pc().limits.clone(),
            can_move_mouse: self.can_move_mouse(),
            monitors: self
                .world
                .monitors()
                .iter()
                .enumerate()
                .map(|(i, &(id, width, height))| MonitorEntry {
                    id,
                    label: self.label_of(id).unwrap_or_else(|| (i + 1).to_string()),
                    width,
                    height,
                    place: self.world.place_of(id, self.view_eye),
                    seen: spots.get(&id).copied(),
                })
                .collect(),
            windows: self
                .monitors
                .iter()
                .filter(|(id, _)| self.world.is_window(**id))
                .map(|(id, m)| MonitorEntry {
                    id: *id,
                    label: self.label_of(*id).unwrap_or_default(),
                    width: m.width_px,
                    height: m.height_px,
                    place: ui::Place::Unknown,
                    seen: None,
                })
                .collect(),
            primary: self.world.primary(),
            cinema_screen: self.world.desk().cinema(),
            selected: self.selected.filter(|id| self.monitors.contains_key(id)),
            mirror_seen: self
                .world
                .mirror()
                .filter(|id| self.monitors.contains_key(id))
                .and_then(|id| spots.get(&id).copied()),
            mirror_wanted: self.mirror_wanted,
            audio_wanted: self.pc().sound.as_ref().is_none_or(|s| s.to_headset),
            mic_wanted: self.mic_wanted,
            mic_denied: self.mic_wanted && self.mic.denied(),
            mirror_name: self.physical_name(),
            settings: self.settings,
            room_available: room,
            panorama_available: panorama,
            custom_available: custom,
            curve_available: self.world.curve_supported(),
            keyboard_available: keyboard,
            error: self
                .error
                .as_ref()
                .filter(|(_, at)| at.elapsed() < ERROR_SHOWN_FOR)
                .map(|(t, _)| t.clone()),
            confirm_remove: self
                .confirm_remove
                .filter(|(_, at)| at.elapsed() < CONFIRM_FOR)
                .map(|(id, _)| id),
            confirm_save: self
                .confirm_save
                .filter(|(_, at)| at.elapsed() < CONFIRM_FOR)
                .map(|(i, _)| i),
            paired: self
                .session
                .paired()
                .iter()
                .map(|p| p.name.clone())
                .collect(),
            battery: self.headset_battery,
            clock: self.clock.clock.clone(),
            date: self.clock.date.clone(),
            confirm_forget: self
                .confirm_forget
                .filter(|(_, at)| at.elapsed() < CONFIRM_FOR)
                .map(|(i, _)| i),
            tab: self.tab,
            texts: self.texts().clone(),
            languages: self.lang.known().clone(),
            language_page: self.language_page,
            intro: !self.settings.intro_seen,
            update: search.update.clone(),
            update_from,
            saved_profiles: self.profiles.each_ref().map(Option::is_some),
            active_profile: self.active_profile,
            note: self.notes.shown().cloned(),
        }
    }
}

/// The menu tab as stored in the settings (the menu opens on the last one).
pub(crate) fn tab_code(tab: Tab) -> u8 {
    use ui::Section;
    match tab {
        Tab::Quick => 0,
        Tab::Monitors => 1,
        Tab::Settings(Section::Look) => 2,
        Tab::Settings(Section::Sound) => 3,
        Tab::Settings(Section::Language) => 4,
        Tab::Settings(Section::Pc) => 5,
        Tab::Settings(Section::Help) => 6,
        Tab::Settings(Section::Controls) => 7,
    }
}

pub(crate) fn tab_from_code(code: u8) -> Tab {
    use ui::Section;
    match code {
        1 => Tab::Monitors,
        2 => Tab::Settings(Section::Look),
        3 => Tab::Settings(Section::Sound),
        4 => Tab::Settings(Section::Language),
        5 => Tab::Settings(Section::Pc),
        6 => Tab::Settings(Section::Help),
        7 => Tab::Settings(Section::Controls),
        _ => Tab::Quick,
    }
}

/// The headset's settings the PC acts on, from `s`.
pub(crate) fn prefs_of(s: &Settings) -> Prefs {
    Prefs {
        notifications: s.notifications,
        sound_from_screen: s.sound_from_screen,
        focus_mode: s.focus,
        gaze_mouse_ms: s
            .gaze_mouse
            .then(|| u16::try_from(GAZE_DWELL.as_millis()).unwrap_or(u16::MAX)),
        no_limits: s.no_limits,
    }
}

/// What the headset says about itself when it opens a session: what it can do, its
/// settings, whether it is worn, its battery; `model` as the system names it.
pub(crate) fn headset_state(
    settings: &Settings,
    worn: bool,
    battery: Option<(u8, bool)>,
    mic: &Mic,
    model: &str,
) -> HeadsetState {
    let mic = mic_state(settings, mic);
    HeadsetState {
        caps: Some(crate::device::headset_caps(model)),
        prefs: Some(prefs_of(settings)),
        presence: Some(Presence { worn }),
        battery: battery.map(|(percent, charging)| Battery { percent, charging }),
        // The Quest's own volume follows the PC's slider from 0.2.0 step 7.
        volume: None,
        up: std::iter::once((mic.kind, mic)).collect(),
    }
}

/// The microphone's switch, and whether the system lets the app record.
pub(crate) fn mic_state(settings: &Settings, mic: &Mic) -> state::UpState {
    state::UpState {
        kind: state::UpKind::Mic,
        switch: settings.mic,
        permission: mic.permission(),
    }
}
