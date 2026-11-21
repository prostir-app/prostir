//! OpenXR main loop.
//!
//! Each monitor of the PC is decoded by MediaCodec straight into an Android surface
//! owned by the Quest compositor (XR_KHR_android_surface_swapchain) and shown as
//! a quad or cylinder layer. The menu, its button, pointer dots and the
//! panorama are small CPU-drawn images; nothing is rendered per frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use android_activity::{AndroidApp, InputStatus, MainEvent, PollEvent};
use anyhow::{Result, anyhow, bail};
use glam::{Vec2, Vec3};
use log::{error, info, warn};
use openxr as xr;
use openxr::sys::Handle as _;
use prostir_proto::request::{Asked, Command, Outcome, Primary, Request};
use prostir_proto::state::{
    self, Battery, DisplayId, HeadsetCaps, HeadsetState, Mode, PcState, Prefs, Presence, ReqId,
    caps,
};
use prostir_proto::{ClientMsg, Codec};

use crate::audio::Gains;
use crate::ids::{DisplayKey, PcId, Target, Thing};
use crate::input::Controls;
use crate::lang::{Lang, LangState, Moment, Texts};
use crate::layout::{self, Cylinder, Grab, Hit, Placement, Ray};
use crate::mic::Mic;
use crate::net::{Feedback, NetEvent};
use crate::overlay::{self, Overlay, Sphere};
use crate::pending::{self, Asks, ProfileSwitch};
use crate::presence;
use crate::profiles::{self, Profile};
#[cfg(all(feature = "selftest", not(feature = "store")))]
use crate::selftest::device::{self as selftest, Act, Selftest};
use crate::settings::{Background, HandMode, Settings};
use crate::store::{StoreCheck, UpdateHint};
use crate::system::SystemEvent;
use crate::ui::{self, Action, MonitorEntry, Panel, Tab, UiState};
use crate::world::{
    ATTACH_PX, ATTACH_WIDTH, BUTTON_SIZE, KEYBOARD_ASPECT, Pickable, WRIST_MENU_WIDTH, World,
    attach_height, panel_height, panel_width, wrist_menu_height,
};
use crate::xrutil::{self, Gfx, ImageLayer, as_layer, pose, posef, vec3};
use crate::{background, decoder, egl, net, passthrough, persist, physical};
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
use no_selftest::{self as selftest, Act, Selftest};

const CURSOR_PX: u32 = 64;
const BUTTON_PX: u32 = 128;
/// Toast texture and its size in the room.
const TOAST_PX: (u32, u32) = (1024, 132);
const TOAST_WIDTH: f32 = 0.46;
/// How long a toast stays.
const TOAST_FOR: Duration = Duration::from_secs(4);
const ERROR_SHOWN_FOR: Duration = Duration::from_secs(8);
const CONFIRM_FOR: Duration = Duration::from_secs(4);
/// How long the app, closing, waits for the network thread to end.
const NET_STOPS_WITHIN: Duration = Duration::from_secs(1);
/// How long a pinch must be held on a monitor before it starts dragging it.
const HOLD_TO_DRAG: Duration = Duration::from_millis(300);

/// `XR_HAND_JOINT_INDEX_TIP_EXT`.
const INDEX_TIP: usize = 10;
/// Touching the menu with a fingertip, like a touchscreen. Distances from the menu's
/// surface, positive in front of it.
const POKE_HOVER: f32 = 0.06;
const POKE_PRESS: f32 = 0.008;
const POKE_RELEASE: f32 = 0.025;
/// A finger further behind the menu than this is not touching it (reaching past).
const POKE_BEHIND: f32 = -0.06;

/// Width of the placeholder's picture, in pixels (its height follows the monitor's
/// shape).
const PLACEHOLDER_PX: u32 = 1024;

/// A monitor held with both hands: the distance between them sets its width.
#[derive(Clone, Copy)]
struct Scaling {
    id: DisplayKey,
    /// The hand that took hold second (the first one keeps its ordinary grab).
    hand: usize,
    start_distance: f32,
    start_width: f32,
}

/// One fingertip against the menu.
#[derive(Default, Clone, Copy)]
struct Poke {
    /// Distance to the menu in the previous frame, while in front of it.
    last_z: Option<f32>,
    /// Touched and not yet pulled back.
    pressed: bool,
    /// Dragging the menu: the touched point in the menu's own plane.
    drag: Option<Vec2>,
    /// The last touch: pulling the hand back often closes thumb and index into a
    /// pinch, which must not click whatever this hand's ray points at.
    touched_at: Option<Instant>,
}

/// How long the ring of a touch spreads.
const TOUCH_RING: Duration = Duration::from_millis(350);

/// Pinches of a hand that just touched the menu are ignored this long.
const AFTER_TOUCH: Duration = Duration::from_millis(600);

#[unsafe(no_mangle)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "the signature android-activity calls"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(if cfg!(debug_assertions) {
                log::LevelFilter::Debug
            } else {
                log::LevelFilter::Info
            })
            .with_tag("prostir"),
    );
    crate::alloc_count::set_main_thread();
    let start = selftest::count_run();
    let mut test = Selftest::requested(&app, start);
    let result = run(&app, test.as_mut());
    if let Err(e) = &result {
        error!("{e:#}");
    }
    if let Some(test) = test {
        let external = app.external_data_path();
        let (stay, code) = test.finish(external.as_deref(), result.as_ref().err());
        // The activity finishes once this returns; without `relaunch=1` the process
        // goes too, so the next start is a new one.
        if !stay {
            let _ = std::thread::Builder::new()
                .name("selftest-exit".into())
                .spawn(move || {
                    std::thread::sleep(Duration::from_millis(500));
                    // SAFETY: ends the process at once, without the exit handlers
                    // and destructors other threads may still be in the middle of.
                    unsafe { libc::_exit(code) };
                });
        }
    }
}

/// 90 Hz (or the nearest below), or 72 Hz to save battery.
fn set_refresh_rate(session: &xr::Session<Gfx>, saver: bool) {
    let limit = if saver { 72.0 } else { 90.0 };
    let rates = session
        .enumerate_display_refresh_rates()
        .unwrap_or_default();
    info!("display rates offered: {rates:?}");
    let best = rates.into_iter().filter(|&r| r <= limit).reduce(f32::max);
    if let Some(rate) = best {
        info!("display at {rate} Hz");
        let _ = session.request_display_refresh_rate(rate);
    }
}

/// The headset's local time, broken down.
fn local_time() -> Option<libc::tm> {
    // SAFETY: plain libc time calls on a zeroed `tm`.
    unsafe {
        let now = libc::time(ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&raw const now, &raw mut tm).is_null()).then_some(tm)
    }
}

/// A moment of the local time, as the menu writes it.
fn moment(tm: &libc::tm) -> Moment {
    let small = |v: libc::c_int, max: u8| v.clamp(0, libc::c_int::from(max)) as u8;
    Moment {
        year: tm.tm_year + 1900,
        month: small(tm.tm_mon, 11),
        day: small(tm.tm_mday, 31),
        weekday: small(tm.tm_wday, 6),
        hour: small(tm.tm_hour, 23),
        minute: small(tm.tm_min, 59),
    }
}

/// The time and the date on the palm menu, written again only when the minute, the
/// language or how they are written changes (not every frame).
#[derive(Default)]
struct ClockFace {
    /// What the texts were written for.
    shown: Option<(Moment, bool, crate::settings::DateStyle, Texts)>,
    /// Shared with the menu's state, which is made anew every frame.
    clock: Arc<str>,
    date: Arc<str>,
}

impl ClockFace {
    fn update(
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

/// One stream: the compositor surface it is decoded into and its decoder thread.
struct Monitor {
    width_px: u32,
    height_px: u32,
    decoder: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    swapchain: xr::Swapchain<Gfx>,
}

impl Monitor {
    fn aspect(&self) -> f32 {
        self.height_px as f32 / self.width_px as f32
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // The decoder must stop writing into the surface before the swapchain goes away.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.decoder.take() {
            let _ = t.join();
        }
    }
}

/// How long the "Attach" button under a pointed-at detached window stays after the
/// pointer left.
const ATTACH_LINGER: Duration = Duration::from_millis(1200);
/// How long an open palm faces the face before its menu appears.
const PALM_DWELL: Duration = Duration::from_millis(250);
/// How long the gaze rests on another monitor before the mouse goes there.
const GAZE_DWELL: Duration = Duration::from_millis(600);
/// How long the "where is the mouse" rings show.
const FINDER_FOR: Duration = Duration::from_millis(1600);

/// Where the palm menu goes: lying on the palm (a little above it, its top towards the
/// fingers), while an open palm is turned to the face. It takes a quarter of a second
/// of that to appear, so hands passing by (typing, reaching) do not bring it up.
/// `since`: when the palm started facing the face.
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
fn palm_menu_place(
    joints: Option<&[(Vec3, f32); 26]>,
    right_hand: bool,
    head: Vec3,
    previous: Option<Placement>,
    since: &mut Option<Instant>,
) -> Option<Placement> {
    let Some(j) = joints else {
        *since = None;
        return None;
    };
    let (palm, wrist) = (j[0].0, j[1].0);
    // Knuckles, a hand's width apart: a steady palm normal. For the left hand it is
    // little × index from the wrist (checked in the headset: the other order points
    // out of the back of the hand); the right hand is its mirror image.
    let (index, middle, little, middle_tip) = (j[7].0, j[12].0, j[22].0, j[15].0);
    let mut normal = (little - wrist).cross(index - wrist).normalize_or_zero();
    if right_hand {
        normal = -normal;
    }
    let facing = normal.dot((head - palm).normalize_or_zero());
    let open = middle_tip.distance(palm) > 0.07;
    let needed = if previous.is_some() { 0.55 } else { 0.8 };
    if facing < needed || !open {
        *since = None;
        return None;
    }
    if previous.is_none() && since.get_or_insert_with(Instant::now).elapsed() < PALM_DWELL {
        return None;
    }
    let up = (middle - wrist)
        .reject_from_normalized(normal)
        .normalize_or(Vec3::Y);
    let rotation = glam::Quat::from_mat3(&glam::Mat3::from_cols(up.cross(normal), up, normal));
    let mut at = palm + normal * 0.03;
    let mut rotation = rotation.normalize();
    // Hands shake a little: the menu follows smoothly.
    if let Some(p) = previous {
        at = p.position.lerp(at, 0.35);
        rotation = p.rotation.slerp(rotation, 0.35);
    }
    Some(Placement {
        position: at,
        rotation,
        width: WRIST_MENU_WIDTH,
    })
}

/// Everything shown in the headset and how it is arranged.
struct Scene {
    monitors: HashMap<DisplayKey, Monitor>,
    /// Where everything stands in the room, and the rules it is arranged by.
    world: World,
    mirror_wanted: bool,
    audio_wanted: bool,
    mic_wanted: bool,
    /// The monitor the mouse is on (from the PC).
    focus: Option<DisplayKey>,
    /// Resizing a monitor with both hands.
    scaling: Option<Scaling>,
    /// Whether the headset is on the head, from the session, the activity and the
    /// headset's sensor; streaming pauses otherwise.
    presence: presence::Presence,
    settings: Settings,
    panel_open: bool,
    grabs: [Option<Grab>; 2],
    host: Option<String>,
    /// PCs announcing themselves while not connected.
    found: Vec<net::FoundHost>,
    /// IP typed on the menu keypad.
    ip_input: Option<String>,
    /// Pairing with a new PC in progress.
    pairing: Option<ui::PairingView>,
    /// A PC on the network that refuses the connection (a firewall).
    unreachable: Option<String>,
    /// The connected PC's state, as it last said it in this session: its displays,
    /// encoding, sound, limits, what it can do.
    pc: PcState,
    /// The menu's requests to the PC in this session: waiting, failed lately, and
    /// the PC's commands waiting for one of them (see `pending`).
    asks: Asks,
    /// The microphone permission the PC was last told.
    mic_permission: u8,
    /// The microphone's switch and permission, shared with the network thread.
    mic: Arc<Mic>,
    /// Where the PC's sound comes from, for the audio thread.
    gains: Arc<Gains>,
    /// An unknown PC waiting for "Pair" or "Not now" (survives the disconnect that
    /// follows the question).
    pair_offer: Option<(String, ui::OfferWhy)>,
    steering: net::Steering,
    /// When the connection was lost (or the app started without one).
    offline_since: Option<Instant>,
    feedback: Option<Feedback>,
    error: Option<(String, Instant)>,
    /// "×" pressed once on this monitor; a second press within a few seconds removes it.
    confirm_remove: Option<(DisplayKey, Instant)>,
    /// "Save current" pressed once for this profile.
    confirm_save: Option<(usize, Instant)>,
    /// Paired PCs (key, name), read from the trust file when the menu opens.
    paired: Vec<([u8; 32], String)>,
    trust_file: PathBuf,
    confirm_forget: Option<(usize, Instant)>,
    /// Menu page shown.
    tab: Tab,
    /// The detached window whose "Attach" button shows, and when it was pointed at.
    attach_for: Option<(DisplayKey, Instant)>,
    /// The monitor picked on the menu's map.
    selected: Option<DisplayKey>,
    /// The headset's battery (percent, charging), read once a minute.
    headset_battery: Option<(u8, bool)>,
    /// The headset's model as the system names it, for `HeadsetCaps`.
    model: String,
    /// The monitor the sound comes from, as the PC says.
    sound_from: Option<DisplayKey>,
    /// The display rate setting changed: apply it.
    refresh_due: bool,
    /// Time worn since the last rest reminder.
    worn_for: Duration,
    /// Since when the headset is off the head (for rest reminders).
    off_since: Option<Instant>,
    /// Mouse follows the gaze: the monitor looked at and since when.
    gaze_on: Option<(DisplayKey, Instant)>,
    /// When the mouse was last sent somewhere by the gaze.
    gaze_moved: Option<Instant>,
    /// "Where is the mouse": monitor, point across its image, since when.
    finder: Option<(DisplayKey, Vec2, Instant)>,
    /// A short message shown on its own: text, since when, where (`None` until placed).
    toast: Option<(String, Instant, Option<Placement>)>,
    /// The PC speaks another protocol: (the PC is newer, its app version).
    update_needed: Option<(bool, String)>,
    /// Head position and gaze this frame.
    head: (Vec3, Vec3),
    /// The head's right, rolled and tilted with it (where the right ear is).
    head_right: Vec3,
    /// Where the eyes are, for the map in the menu: moves only when the head has moved
    /// a few centimetres (the menu redraws when its picture changes).
    view_eye: Option<Vec3>,
    /// Tracked hands are in view this frame (the menu then opens within reach).
    hands_in_view: bool,
    /// The headset's own system language.
    system_lang: Option<Lang>,
    /// The menu's language: what it shows, and the one it waits for.
    lang: LangState,
    clock: ClockFace,
    /// Which languages the Language page shows, when there are many.
    language_page: u8,
    profiles: profiles::Slots,
    /// The profile last switched to, until something is changed by hand.
    active_profile: Option<usize>,
    /// The files changed in this pass, handed to the files' writer after it (see
    /// `save_due`).
    saves: persist::Dirty,
    /// The files whose failed save the menu told of, once each.
    save_failures_told: persist::Dirty,
    /// The files' writer of this start. Last, and stopped when the scene goes (see
    /// `Drop`), however `run` ends.
    saver: persist::Saver,
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
    /// Display `id` of the PC of this session (or of the last one).
    fn key(&self, id: DisplayId) -> Option<DisplayKey> {
        self.world.key(id)
    }

    /// The number display `k` has on the PC of this session; none for a display of
    /// another PC.
    fn on_pc(&self, k: DisplayKey) -> Option<DisplayId> {
        self.world.on_pc(k)
    }

    /// Display `id` as its PC's list says (`None`: not listed, or another PC's).
    fn display(&self, id: DisplayKey) -> Option<&state::Display> {
        self.pc.displays.as_ref()?.get(self.on_pc(id)?)
    }

    /// The PC can move the mouse (for following the gaze).
    fn can_move_mouse(&self) -> bool {
        self.pc
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::MOVE_MOUSE))
    }

    /// The display of the PC itself the switch shows or hides: one connected now,
    /// never a closed laptop's panel (see [`physical::switch`]).
    fn physical_id(&self) -> Option<DisplayKey> {
        self.key(physical::switch(self.pc.displays.as_ref()?)?.id)
    }

    /// What the switch's display is called, if a display of the PC is connected.
    fn physical_name(&self) -> Option<String> {
        physical::switch(self.pc.displays.as_ref()?).map(|d| physical::label(d, self.texts()))
    }

    /// What the world asks for after it changed: the PC is told the rows of its
    /// desktop (see `World::settle`), and a hand lets go of what the world no longer
    /// has it hold (laid out anew, or gone).
    fn world_done(&mut self) {
        if let Some(rows) = self.world.take_arrange() {
            self.send(ClientMsg::Arrange { rows });
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
    fn current_profile(&self) -> Profile {
        let shown = self
            .pc
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
    fn adding_due(&mut self) {
        let waits = self.asks.waits(ui::Asked::AddMonitor);
        self.world.adding_due(Instant::now(), waits);
    }

    /// Turns cinema mode on or off (see `World::set_cinema`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn set_cinema(&mut self, on: bool) {
        self.world
            .set_cinema(&self.settings, on, self.head.1, Instant::now());
        self.settings.cinema = on;
        self.world_done();
    }

    /// The files changed in this pass (`saves`, and those the world changed), handed
    /// to the files' writer: each one's text written into the writer's buffer for it,
    /// once, whatever changed it how often. The writing itself is done on the writer's
    /// thread.
    fn save_due(&mut self) {
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
    fn flush_saves(&mut self, why: &str) {
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
    fn saves_told(&mut self) {
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
    fn forget_pc(&mut self, pc: PcId) {
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
    fn follow_gaze(&mut self) {
        let busy = self.world.wrist().is_some() || self.world.holding() || self.settings.cinema;
        if !self.settings.gaze_mouse || !self.can_move_mouse() || self.feedback.is_none() || busy {
            self.gaze_on = None;
            return;
        }
        let (head, forward) = self.head;
        let ray = Ray {
            origin: head,
            dir: forward.normalize_or(-Vec3::Z),
        };
        let asks = &self.asks;
        let looked = self
            .world
            .looked_at(ray, &self.settings, self.panel_open, |k| {
                asks.waits(ui::Asked::Remove(k))
            });
        let Some((id, uv)) = looked else {
            self.gaze_on = None;
            return;
        };
        if self.focus == Some(id) {
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
                    if let Some(display) = self.on_pc(id) {
                        self.send(ClientMsg::MouseTo {
                            id: display,
                            u: f(uv.x),
                            v: f(uv.y),
                        });
                    }
                    self.focus = Some(id);
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
    fn remind_rest(&mut self, dt: f32) {
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
    fn place_sound(&self) {
        let angle = self
            .sound_from
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
    fn show_toast(&mut self, text: String) {
        // Not the text: it can be a PC notification (a message, a code).
        info!("toast shown");
        self.toast = Some((text, Instant::now(), None));
    }

    /// Profile `i` as the menu names it ("Profile 2").
    fn profile_name(&self, i: usize) -> String {
        self.texts().f("profile-n", &[("n", (i + 1).into())])
    }

    /// The monitor most straight ahead of the head (not a window panel).
    fn monitor_in_front(&self) -> Option<DisplayKey> {
        let asks = &self.asks;
        self.world
            .monitor_in_front(self.head, |k| asks.waits(ui::Asked::Remove(k)))
    }

    /// Turns the whole arrangement around the user (see `World::recenter`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn recenter(&mut self) {
        self.world
            .recenter(&self.settings, self.head, Instant::now());
        self.world_done();
    }

    /// Brings the monitors closer or further (see `World::closer`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn closer(&mut self, steps: i8) {
        self.world.closer(steps, Instant::now());
        self.world_done();
    }

    /// The A button (see `World::tidy_up`).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn tidy_up(&mut self) {
        self.world.tidy_up(&self.settings, Instant::now());
        self.world_done();
    }

    /// Opens the menu in front of the user: within arm's reach, a little lower, when
    /// hands are in view (to touch it), further away for controllers.
    fn open_panel(&mut self, head: Vec3, forward: Vec3, touch: bool) {
        self.world.open_menu((head, forward), touch, &self.settings);
        self.panel_open = true;
    }

    /// Tells the PC whether the headset is worn (quietly: nothing to show the user),
    /// and keeps it for the next `Open`.
    fn tell_presence(&self) {
        self.steering.set_headset(self.headset_state());
        if let Some(f) = &self.feedback {
            let _ = f.send(ClientMsg::Presence {
                part: Presence {
                    worn: self.presence.worn(),
                },
            });
        }
    }

    /// Tells the PC once the signals say the headset was put on or taken off, and
    /// lets the network thread connect only while it is worn.
    fn presence_due(&mut self, now: Instant) {
        if let Some(worn) = self.presence.update(now) {
            info!("headset {}", if worn { "worn" } else { "not worn" });
            self.tell_presence();
            self.steering.set_worn(worn);
        }
    }

    /// The battery was read (once a minute, more often on the palm menu): the menu
    /// shows it, the PC's tray hears it, and the next `Open` says it.
    fn battery_read(&mut self, battery: Option<(u8, bool)>) {
        self.headset_battery = battery;
        self.steering.set_headset(self.headset_state());
        if let Some((percent, charging)) = battery
            && self.feedback.is_some()
        {
            self.send(ClientMsg::Battery {
                part: Battery { percent, charging },
            });
        }
    }

    /// The headset's settings the PC acts on.
    fn prefs(&self) -> Prefs {
        prefs_of(&self.settings)
    }

    /// What the headset says about itself when it opens a session.
    fn headset_state(&self) -> HeadsetState {
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
    fn mic_changed(&mut self) {
        self.steering.set_headset(self.headset_state());
        self.mic_permission = self.mic.permission();
        let part = mic_state(&self.settings, &self.mic);
        if self.feedback.is_some() {
            self.send(ClientMsg::UpState { part });
        }
    }

    /// The settings the PC acts on changed: it hears at once, and the next session
    /// starts with them.
    fn prefs_changed(&mut self) {
        self.steering.set_headset(self.headset_state());
        let part = self.prefs();
        if self.feedback.is_some() {
            self.send(ClientMsg::Prefs { part });
        }
    }

    /// Control `what` asks the PC to change something: one reply comes with its
    /// number, and the control shows how it stands meanwhile. Without a connection
    /// nothing is sent (and nothing kept to send later: repeating "add a monitor"
    /// after reconnecting could add two).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn request(&mut self, asked: Request, what: ui::Asked) -> Option<ReqId> {
        if self.feedback.is_none() {
            self.ask_failed(what, ui::Failure::NotConnected, String::new());
            return None;
        }
        let req = self.asks.ask(what, Instant::now());
        info!("request {}: {asked:?}", req.0);
        self.send(ClientMsg::request(req, asked));
        Some(req)
    }

    /// [`Scene::request`] about display `k`, made with the number `k` has on its PC:
    /// failed as not connected when `k` is not of the PC of this session.
    fn request_for(
        &mut self,
        k: DisplayKey,
        asked: impl FnOnce(DisplayId) -> Request,
        what: ui::Asked,
    ) -> Option<ReqId> {
        let Some(id) = self.on_pc(k) else {
            self.ask_failed(what, ui::Failure::NotConnected, String::new());
            return None;
        };
        self.request(asked(id), what)
    }

    /// What the user reads for display `id`: as the PC's desktop names it.
    fn label_of(&self, id: DisplayKey) -> Option<String> {
        let d = self.pc.displays.as_ref()?.get(self.on_pc(id)?)?;
        Some(ui::display_label(&d.name))
    }

    /// Request `req` was answered: which control asked it, `None` for one this side
    /// does not wait for (answered after it gave up on it).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn answered(&mut self, req: ReqId, outcome: &Outcome) -> Option<ui::Asked> {
        let answered = self.asks.answered(req, outcome, Instant::now());
        // A command of the PC that waited for this answer gets it, with the same code.
        if let Some((command, outcome)) = answered.command {
            self.send(ClientMsg::Reply {
                req: command,
                outcome,
            });
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
            warn!("request {} failed (code {})", req.0, error.code());
            self.tell_failure(what, ui::Failure::Pc(*error), detail);
        }
        Some(what)
    }

    /// Request of control `what` failed: the control says why. A control the menu
    /// does not show while it works (a profile closes the menu, the palm menu finds the
    /// mouse) says it with a short message.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn ask_failed(&mut self, what: ui::Asked, why: ui::Failure, detail: String) {
        self.tell_failure(what, why, &detail);
        self.asks.failed(what, why, detail, Instant::now());
    }

    /// A control the menu does not show while it works (a profile closes the menu,
    /// the palm menu finds the mouse) says its failure with a short message.
    fn tell_failure(&mut self, what: ui::Asked, why: ui::Failure, detail: &str) {
        if matches!(what, ui::Asked::Profile(_) | ui::Asked::FindMouse) {
            let text = why
                .text()
                .map_or_else(|| detail.to_owned(), |id| self.texts().t(id).to_owned());
            self.show_toast(text);
        }
    }

    /// Requests without an answer for 30 s count as failed (a late answer is let go),
    /// and failures are shown for a while; a profile switch waited for long enough is
    /// laid out with what streams.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn expire_asks(&mut self) {
        let now = Instant::now();
        for (req, what) in self.asks.expire(now) {
            self.world.switch_given_up(req);
            self.tell_failure(what, ui::Failure::TimedOut, "");
        }
        self.world.switch_due(&self.settings, now);
        self.world_done();
    }

    /// How each control's request stands, for the menu.
    fn ask_views(&self) -> Vec<ui::AskView> {
        self.asks.views()
    }

    fn send(&mut self, msg: ClientMsg) {
        let sent = self.feedback.as_ref().map(|f| f.send(msg));
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
    fn texts(&self) -> &Texts {
        self.lang.texts()
    }

    /// The language the menu is to show, if it is neither shown nor already asked for:
    /// the `system` thread is to load it (chosen in the menu, else the PC's, else the
    /// headset's).
    fn wanted_lang(&mut self) -> Option<Lang> {
        let s = &self.settings;
        self.lang
            .wanted(s.language, s.pc_language, self.system_lang)
    }

    /// The time and the date on the palm menu, for this minute.
    fn tick_clock(&mut self) {
        let s = &self.settings;
        let now = local_time().map(|t| moment(&t));
        self.clock
            .update(now, s.clock_12h, s.date_style, self.lang.texts());
    }

    fn show_error(&mut self, text: String) {
        warn!("{text}");
        self.put_error(text);
    }

    /// [`Scene::show_error`] without the line in the log.
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn put_error(&mut self, text: String) {
        self.error = Some((text, Instant::now()));
    }

    /// What the menu shows; `update_from`: where this build is updated (its store's).
    #[expect(
        clippy::disallowed_methods,
        reason = "until step 7.7 passes the time in and moves the threads to the shell"
    )]
    fn ui_state(
        &self,
        room: bool,
        keyboard: bool,
        panorama: bool,
        custom: bool,
        update_from: UpdateHint,
    ) -> UiState {
        let spots = self.world.map_spots();
        UiState {
            host: self.host.clone(),
            found: self
                .found
                .iter()
                .map(|h| (h.name.clone(), h.addr.ip().to_string()))
                .collect(),
            ip_input: self.ip_input.clone(),
            pairing: self.pairing.clone(),
            pair_offer: self.pair_offer.clone(),
            unreachable: self.unreachable.clone(),
            pc: self.pc.encoding.clone(),
            limits: self.pc.limits.clone(),
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
            audio_wanted: self.audio_wanted,
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
            paired: self.paired.iter().map(|(_, n)| n.clone()).collect(),
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
            update: self.update_needed.clone(),
            update_from,
            saved_profiles: self.profiles.each_ref().map(Option::is_some),
            active_profile: self.active_profile,
            asks: self.ask_views(),
        }
    }
}

/// The user's own panorama (`background.jpg`/`.png` next to `host.txt`), else the night sky.
/// The user's own panorama (`background.jpg/.png` next to `host.txt`), if there is one.
/// The menu tab as stored in the settings (the menu opens on the last one).
fn tab_code(tab: Tab) -> u8 {
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

fn tab_from_code(code: u8) -> Tab {
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

#[expect(
    clippy::disallowed_methods,
    reason = "looked for when the menu opens, in the frame, until step 7.7 moves it out"
)]
fn custom_panorama_path(dir: Option<&Path>) -> Option<PathBuf> {
    let d = dir?;
    ["background.jpg", "background.jpeg", "background.png"]
        .iter()
        .map(|n| d.join(n))
        .find(|p| p.exists())
}

/// Width, height and RGBA pixels of a panorama.
type Panorama = (u32, u32, Vec<u8>);

/// Pixels for a panorama background: generated sky or the user's picture.
fn load_panorama(choice: Background, dir: Option<&Path>) -> Panorama {
    if choice == Background::Custom
        && let Some(path) = custom_panorama_path(dir)
    {
        match image::open(&path) {
            Ok(img) => {
                let img = if img.width() > 4096 {
                    img.resize(4096, 2048, image::imageops::FilterType::Triangle)
                } else {
                    img
                };
                let rgba = img.to_rgba8();
                info!("panorama from {}", path.display());
                return (rgba.width(), rgba.height(), rgba.into_raw());
            }
            Err(e) => warn!("cannot read {}: {e}", path.display()),
        }
    }
    let sky = match choice {
        Background::Sky(sky) => sky,
        _ => background::Sky::Night,
    };
    (
        background::WIDTH,
        background::HEIGHT,
        background::generate(sky),
    )
}

/// The app; with `test`, the selftest's run of it (see `selftest::device`).
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
fn run(app: &AndroidApp, mut test: Option<&mut Selftest>) -> Result<()> {
    // SAFETY: pointers come straight from the running NativeActivity.
    let platform = unsafe { xr::AndroidPlatformInfo::new(app.vm_as_ptr(), app.activity_as_ptr()) };
    // SAFETY: libopenxr_loader.so is the Khronos loader bundled in the APK.
    let entry = unsafe { xr::Entry::load(&platform) }
        .map_err(|e| anyhow!("cannot load OpenXR loader: {e}"))?;
    let available = entry.enumerate_extensions()?;
    if !available.khr_android_surface_swapchain {
        bail!("the runtime lacks XR_KHR_android_surface_swapchain");
    }
    let mut exts = xr::ExtensionSet::default();
    exts.khr_android_create_instance = true;
    exts.khr_opengl_es_enable = true;
    exts.khr_android_surface_swapchain = true;
    exts.fb_passthrough = available.fb_passthrough;
    exts.fb_display_refresh_rate = available.fb_display_refresh_rate;
    exts.fb_composition_layer_settings = available.fb_composition_layer_settings;
    exts.fb_composition_layer_image_layout = available.fb_composition_layer_image_layout;
    exts.khr_composition_layer_cylinder = available.khr_composition_layer_cylinder;
    exts.khr_composition_layer_equirect2 = available.khr_composition_layer_equirect2;
    exts.ext_hand_interaction = available.ext_hand_interaction;
    exts.ext_hand_tracking = available.ext_hand_tracking;
    exts.fb_hand_tracking_aim = available.fb_hand_tracking_aim;
    exts.fb_triangle_mesh = available.fb_triangle_mesh;
    exts.khr_composition_layer_color_scale_bias = available.khr_composition_layer_color_scale_bias;
    // Taken off, said at once by the headset's sensor (the session may stay shown
    // until the system sleeps).
    exts.ext_user_presence = available.ext_user_presence;
    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "Prostir",
            application_version: 1,
            engine_name: "prostir",
            engine_version: 1,
            api_version: xr::Version::new(1, 0, 0),
        },
        &exts,
        &[],
        &platform,
    )?;
    info!(
        "Prostir {}: cylinder layers: {}, equirect: {}, hand interaction: {}, user presence: {}",
        prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
        exts.khr_composition_layer_cylinder,
        exts.khr_composition_layer_equirect2,
        exts.ext_hand_interaction,
        exts.ext_user_presence
    );
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let _ = instance.graphics_requirements::<Gfx>(system)?;
    // As the system names the headset: only shown (the PC's list of headsets,
    // pairing) and logged.
    let model = crate::system::model();
    info!("headset: {model:?}");
    if let Some(t) = test.as_deref_mut() {
        t.runtime(app, &instance, system, &available);
        t.headset_caps(&headset_caps(&model));
    }

    let egl = egl::EglContext::new()?;
    // SAFETY: the EGL handles stay alive (owned by `egl`) for the whole session.
    let (session, mut frame_waiter, mut frame_stream) = unsafe {
        instance.create_session::<Gfx>(
            system,
            &xr::opengles::SessionCreateInfo::Android {
                display: egl.display.as_ptr().cast(),
                config: egl.config.as_ptr().cast(),
                context: egl.context.as_ptr().cast(),
            },
        )
    }?;
    let gl = xrutil::load_gl(&egl.egl);

    let data_dir: PathBuf = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    // The selftest keeps its own data, and its fake PC is the only PC it connects to.
    let data_dir = match test.as_deref_mut() {
        Some(t) => t.data_dir(&data_dir),
        None => data_dir,
    };
    let fake_pc = test.as_deref_mut().and_then(|t| t.start_pc(app, &data_dir));
    if test.as_deref().is_some_and(Selftest::over) {
        return Ok(());
    }
    // A write cut short leaves its temporary file; the real one is whole.
    let persisted = persist::File::ALL.map(persist::File::name);
    for name in persisted.into_iter().chain(["identity", "paired_hosts"]) {
        prostir_proto::secure::remove_leftovers(&data_dir.join(name));
    }
    // The files are read here once; from now on the model in memory is what counts,
    // and the files' writer only writes them, off the frame. However this function
    // ends, what waits is written and the thread waited for (see `persist::Saver`,
    // and `Scene`'s `Drop` once the scene has it).
    let (loaded, saver) = persist::start(&data_dir);
    let persist::Loaded {
        mut settings,
        desk,
        profiles: saved_profiles,
        keyboard: keyboard_place,
        untouched: _,
    } = loaded;
    // This start's own: a new start in the same process begins afresh.
    let mic = Arc::new(Mic::new(settings.mic, app.clone()));
    let gains = Arc::new(Gains::default());

    // Passthrough keeps the cameras busy and costs battery: only when "Room" is chosen.
    let mut passthrough = if exts.fb_passthrough {
        passthrough::Passthrough::new(&session, settings.background.is_room())
            .inspect_err(|e| warn!("passthrough unavailable: {e:#}"))
            .ok()
    } else {
        None
    };

    let local =
        session.create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::Posef::IDENTITY)?;
    let view = session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?;
    let mut controls = Controls::new(&instance, &session, exts.ext_hand_interaction)?;
    controls.hands_enabled = settings.hands == HandMode::Control;

    let format = xrutil::color_format(&session);
    let mut panel_layer = ImageLayer::new(&session, format, ui::WIDTH, ui::HEIGHT, false)?;
    let mut button_layer = ImageLayer::new(&session, format, BUTTON_PX, BUTTON_PX, false)?;
    let mut cursor_layer = ImageLayer::new(&session, format, CURSOR_PX, CURSOR_PX, true)?;
    let mut toast_layer = ImageLayer::new(&session, format, TOAST_PX.0, TOAST_PX.1, false)?;
    let mut attach_layer = ImageLayer::new(&session, format, ATTACH_PX.0, ATTACH_PX.1, false)?;
    // What the "Attach" texture shows now: language and lit (pointed at).
    let mut attach_drawn: Option<(Texts, bool)> = None;
    // The placeholder of a monitor being added: its layer, made for a size and
    // language.
    let mut placeholder: Option<(ImageLayer, (u32, u32), Texts)> = None;
    cursor_layer.upload(&gl, &ui::cursor_image(CURSOR_PX))?;
    let mut button_hovered = false;
    // The left hand's menu gesture as last seen, to act once per gesture.
    let mut menu_gesture_held = false;
    // Whether the palm menu showed last frame (it shows the battery).
    let mut palm_was_shown = false;
    button_layer.upload(&gl, &ui::menu_button_image(BUTTON_PX, false))?;
    let mut panel = Panel::new();
    let mut wrist_panel = Panel::wrist();
    let mut wrist_layer =
        ImageLayer::new(&session, format, ui::WRIST_WIDTH, ui::WRIST_HEIGHT, false)?;
    // The right fingertip left the wrist menu since its last press.
    let mut wrist_poke_armed = true;
    // Since when an open palm faces the face (its menu shows after a moment).
    let mut palm_since: Option<Instant> = None;
    // Tracked hands and pointer rays, drawn on top of everything.
    let hand_trackers = if exts.ext_hand_tracking {
        let make = |h| session.create_hand_tracker(h);
        make(xr::Hand::LEFT)
            .and_then(|l| Ok([l, make(xr::Hand::RIGHT)?]))
            .inspect_err(|e| warn!("hand tracking unavailable: {e}"))
            .ok()
    } else {
        None
    };
    let mut overlay = Overlay::new(&gl, &instance, system, &session, format)
        .inspect_err(|e| warn!("hand/ray overlay unavailable: {e:#}"))
        .ok();
    // The panorama shown and the background it was made for.
    let mut pano_layer: Option<(Background, ImageLayer)> = None;
    let mut pano_job: Option<(Background, JoinHandle<Panorama>)> = None;
    // The selftest reads nothing of the person's there (`host.txt`, a panorama).
    let external_dir = app.external_data_path().filter(|_| test.is_none());
    let mut custom_pano = custom_panorama_path(external_dir.as_deref()).is_some();
    let mut panel_was_open = false;

    // The menu's language, before the first frame: the headset's own language once,
    // the files there are, and the language to show. From here on the system thread
    // reads the files.
    let system_lang = crate::system::language();
    #[expect(
        clippy::disallowed_methods,
        reason = "the start, before the first frame, reads the language files once"
    )]
    let (lang, lang_loader) = crate::lang::start(
        external_dir.as_ref().map(|d| d.join("lang")),
        settings.language,
        settings.pc_language,
        system_lang,
    );
    // A language chosen once whose file is gone is "like the PC" again.
    settings.language = crate::lang::kept(settings.language, lang.known());
    // What the network thread was last told (it names a PC over USB).
    let mut told_texts = lang.texts().clone();

    // The battery once now, for the first `Open`; from here on the system thread reads
    // it, away from the frame.
    let battery = crate::system::battery();
    let system_thread = crate::system::System::start(lang_loader);

    let (net_tx, net_rx) = mpsc::channel();
    let steering = net::Steering::default();
    steering.set_texts(told_texts.clone());
    // What the first `Open` says, before the network thread can connect.
    steering.set_headset(headset_state(&settings, true, battery, &mic, &model));
    // However this function ends, the network thread and its session end with it, and
    // the thread is waited for (up to a second): a new start in the same process must
    // not find this one's still connected to the PC.
    struct StopNetwork(net::Steering, Option<JoinHandle<()>>);
    impl Drop for StopNetwork {
        fn drop(&mut self) {
            self.0.shut_down();
            if let Some(thread) = self.1.take() {
                crate::system::join_within("net", thread, NET_STOPS_WITHIN);
            }
        }
    }
    // A selftest connects to its fake PC and nothing else, never to a real PC.
    debug_assert!(test.is_none() || fake_pc.is_some());
    let network = net::spawn(
        net_tx,
        net::Discovery {
            identity_file: data_dir.join("identity"),
            trust_file: data_dir.join("paired_hosts"),
            host_file: external_dir.as_ref().map(|p| p.join("host.txt")),
            last_host_file: data_dir.join("last_host"),
            only: fake_pc,
            name: model.clone(),
        },
        steering.clone(),
        net::Sound {
            gains: gains.clone(),
            mic: mic.clone(),
        },
    );
    let _stop_network = StopNetwork(steering.clone(), Some(network));

    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let _wifi_lock = xrutil::acquire_low_latency_wifi(app, &vm)
        .inspect(|_| info!("Wi-Fi low-latency mode on"))
        .inspect_err(|e| warn!("Wi-Fi low-latency lock unavailable: {e:#}"))
        .ok();

    let world = World::new(
        desk,
        keyboard_place,
        exts.khr_composition_layer_cylinder,
        &settings,
    );
    let mut scene = Scene {
        monitors: HashMap::new(),
        world,
        mirror_wanted: true,
        audio_wanted: true,
        mic_wanted: settings.mic,
        focus: None,
        scaling: None,
        presence: presence::Presence::new(),
        settings,
        panel_open: false,
        grabs: [None, None],
        host: None,
        found: Vec::new(),
        ip_input: None,
        pairing: None,
        pair_offer: None,
        unreachable: None,
        pc: PcState::default(),
        asks: Asks::default(),
        mic_permission: mic.permission(),
        mic,
        gains,
        steering,
        offline_since: Some(Instant::now()),
        feedback: None,
        error: None,
        confirm_remove: None,
        confirm_save: None,
        paired: Vec::new(),
        trust_file: data_dir.join("paired_hosts"),
        confirm_forget: None,
        tab: tab_from_code(settings.last_tab),
        update_needed: None,
        attach_for: None,
        selected: None,
        headset_battery: battery,
        model,
        sound_from: None,
        refresh_due: false,
        worn_for: Duration::ZERO,
        off_since: None,
        gaze_on: None,
        gaze_moved: None,
        finder: None,
        toast: None,
        head: (Vec3::ZERO, -Vec3::Z),
        head_right: Vec3::X,
        view_eye: None,
        hands_in_view: false,
        system_lang,
        lang,
        clock: ClockFace::default(),
        language_page: 0,
        profiles: saved_profiles,
        active_profile: None,
        saves: persist::Dirty::default(),
        save_failures_told: persist::Dirty::default(),
        saver,
    };
    // The language's file had lines that could not be read: said once, in front.
    if let Some(warning) = scene.texts().warning() {
        let text = warning.to_owned();
        scene.show_toast(text);
    }
    let mut hold_since: [Option<Instant>; 2] = [None, None];
    // A hand's pinch on a menu button: clicked when let go, unless it was held long
    // enough to drag the menu instead (as on the Quest's own windows).
    let mut pending_click: [Option<(Action, Vec3)>; 2] = [None, None];
    // Grabs started on a monitor's bar (they end when the press ends).

    let mut pokes = [Poke::default(); 2];
    // Touches that just happened: (where, when), drawn as a ring spreading out.
    let mut rings: Vec<(Vec3, Instant)> = Vec::new();
    // Where this build comes from. A store build checks the user owns the app, as its
    // store requires at launch; a build from GitHub has nothing to check.
    let mut store = crate::store::of_this_build(app);
    // Since when the store says the app is not the user's: it closes after saying so.
    let mut not_owned_since: Option<Instant> = None;
    // What the person is told then, and the language it is in.
    let mut not_owned_text: Option<(Texts, String)> = None;
    let mut session_running = false;
    // The input is the app's: false while the system menu is open over it (the app
    // keeps drawing, but shows no hands or rays of its own and takes no input).
    let mut focused = false;
    let mut last_time: Option<xr::Time> = None;
    let mut event_storage = xr::EventDataBuffer::new();
    // What the selftest asked for, done in the next frame drawn.
    let mut selftest_act = Act::Nothing;
    let mut allocations = crate::alloc_count::Tally::default();

    'main: loop {
        if let Some(t) = test.as_deref_mut() {
            t.loop_top();
        }
        // What the last pass changed goes to the files' writer, however the pass ended.
        scene.save_due();
        let mut destroy = false;
        let mut paused = false;
        // Paused (true) or resumed (false), the last one of this pass.
        let mut activity = None;
        let timeout = if session_running {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };
        let mut input_waiting = false;
        app.poll_events(Some(timeout), |event| match event {
            PollEvent::Main(MainEvent::Destroy) => destroy = true,
            PollEvent::Main(MainEvent::Pause) => {
                paused = true;
                activity = Some(true);
            }
            PollEvent::Main(MainEvent::Resume { .. }) => activity = Some(false),
            PollEvent::Main(MainEvent::InputAvailable) => input_waiting = true,
            _ => {}
        });
        if destroy {
            break;
        }
        if let Some(paused) = activity {
            // The system may freeze a paused app soon: the PC hears it first.
            scene.presence.paused(paused);
            scene.presence_due(Instant::now());
        }
        if paused {
            // The system may end a paused app without a word: what waits is written now.
            scene.flush_saves("paused");
        }
        if let StoreCheck::NotOwned { store: name } = store.poll() {
            let since = *not_owned_since.get_or_insert_with(Instant::now);
            // Written once in each language, then compared with what the menu shows.
            if not_owned_text
                .as_ref()
                .is_none_or(|(texts, _)| texts != scene.texts())
            {
                let text = scene
                    .texts()
                    .f("store-not-confirmed", &[("store", name.into())]);
                not_owned_text = Some((scene.texts().clone(), text));
            }
            let what = not_owned_text
                .as_ref()
                .map_or("", |(_, text)| text.as_str());
            if scene.error.as_ref().is_none_or(|(t, _)| t != what) {
                scene.show_error(what.to_owned());
                // In front of the user, wherever the menu was last.
                let (head, forward) = scene.head;
                let hands = scene.hands_in_view;
                scene.open_panel(head, forward, hands);
                // The 6 s count from when the message is up, not from the failure.
                not_owned_since = Some(Instant::now());
            }
            // Long enough to read, then close as the store expects.
            if since.elapsed() > Duration::from_secs(6) {
                break;
            }
        }
        if input_waiting {
            // Android waits for every key event (e.g. the volume buttons) to be taken
            // and answers "not responding" after 5 s otherwise. Hand them all back to
            // the system so the volume buttons keep working.
            if let Ok(mut events) = app.input_events_iter() {
                while events.next(|_| InputStatus::Unhandled) {}
            }
        }

        while let Some(event) = instance.poll_event(&mut event_storage)? {
            match event {
                xr::Event::SessionStateChanged(e) => {
                    info!("session state: {:?}", e.state());
                    if let Some(t) = test.as_deref_mut() {
                        t.session_state(e.state());
                    }
                    let now_focused = e.state() == xr::SessionState::FOCUSED;
                    if focused && !now_focused {
                        // A drag cannot go on under the system menu.
                        scene.grabs = [None, None];
                        scene.scaling = None;
                        scene.world.cancel_holds();
                    }
                    focused = now_focused;
                    // Hidden or stopped, the PC stops encoding until the app is shown
                    // again; told before the session ends and the files are written.
                    let shown = match e.state() {
                        xr::SessionState::VISIBLE | xr::SessionState::FOCUSED => {
                            Some(presence::Session::Shown)
                        }
                        xr::SessionState::READY | xr::SessionState::SYNCHRONIZED => {
                            Some(presence::Session::Hidden)
                        }
                        xr::SessionState::STOPPING | xr::SessionState::IDLE => {
                            Some(presence::Session::Stopped)
                        }
                        _ => None,
                    };
                    if let Some(shown) = shown {
                        scene.presence.session(shown);
                        scene.presence_due(Instant::now());
                    }
                    match e.state() {
                        xr::SessionState::READY => {
                            session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                            session_running = true;
                            if exts.fb_display_refresh_rate {
                                set_refresh_rate(&session, scene.settings.battery_saver);
                            }
                            if let Some(t) = test.as_deref_mut()
                                && exts.fb_display_refresh_rate
                            {
                                t.refresh(
                                    session.enumerate_display_refresh_rates().ok(),
                                    session.get_display_refresh_rate().ok(),
                                );
                            }
                        }
                        xr::SessionState::STOPPING => {
                            session.end()?;
                            session_running = false;
                            // Taken off (the headset may sleep for long): saved now.
                            scene.flush_saves("stopping");
                        }
                        xr::SessionState::IDLE => {
                            scene.flush_saves("idle");
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => break 'main,
                        _ => {}
                    }
                }
                xr::Event::UserPresenceChangedEXT(e) => {
                    scene.presence.user(Instant::now(), e.is_user_present());
                }
                xr::Event::InstanceLossPending(_) => break 'main,
                _ => {}
            }
        }
        // The sensor's "away" counts once it lasted (see `presence::AWAY_GRACE`): looked
        // at every pass, also while nothing is drawn.
        scene.presence_due(Instant::now());

        while let Ok(event) = net_rx.try_recv() {
            if let Some(t) = test.as_deref_mut() {
                t.net_event(&event);
            }
            match event {
                NetEvent::Command { req, asked } => {
                    take_command(&mut scene, req, &asked, &mut passthrough, &mut controls);
                }
                event => handle_net_event(&mut scene, &session, &vm, event),
            }
            scene.world_done();
        }
        // Only the newest battery reading matters (several wait after a pause).
        let mut newest = None;
        while let Some(event) = system_thread.try_recv() {
            match event {
                SystemEvent::Battery(reading) => newest = Some(reading),
                SystemEvent::Texts(texts) => {
                    if scene.lang.loaded(texts)
                        && let Some(warning) = scene.texts().warning()
                    {
                        let text = warning.to_owned();
                        scene.show_toast(text);
                    }
                }
                SystemEvent::Languages(known) => {
                    scene.lang.listed(known);
                }
            }
        }
        if let Some(reading) = newest {
            scene.battery_read(reading);
        }
        scene.saves_told();
        // Another language chosen (or the PC's changed): loaded on the system thread,
        // the menu keeps the one it has until then.
        if let Some(lang) = scene.wanted_lang() {
            system_thread.load_language(lang);
        }
        scene.tick_clock();

        // Panorama: generated or decoded off the main thread, uploaded once per choice.
        let wanted = scene.settings.background;
        let wants_pano = matches!(wanted, Background::Sky(_) | Background::Custom);
        if wants_pano
            && exts.khr_composition_layer_equirect2
            && pano_layer.as_ref().is_none_or(|(b, _)| *b != wanted)
            && pano_job.is_none()
        {
            let dir = external_dir.clone();
            pano_job = Some((
                wanted,
                std::thread::spawn(move || load_panorama(wanted, dir.as_deref())),
            ));
        }
        if pano_job.as_ref().is_some_and(|(_, j)| j.is_finished()) {
            let (made_for, job) = pano_job.take().expect("checked just above");
            match job.join() {
                Ok((w, h, pixels)) => {
                    let mut layer = ImageLayer::new(&session, format, w, h, true)?;
                    layer.upload(&gl, &pixels)?;
                    pano_layer = Some((made_for, layer));
                }
                Err(_) => warn!("panorama generation panicked"),
            }
        }

        if let Some(t) = test.as_deref_mut() {
            match t.step() {
                Act::Finish => break 'main,
                Act::Nothing => {}
                act => selftest_act = act,
            }
        }

        if !session_running {
            continue;
        }

        let state = frame_waiter.wait()?;
        let frame_start = test.as_deref().map(Selftest::frame_start);
        frame_stream.begin()?;
        let time = state.predicted_display_time;
        if !state.should_render {
            frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &[])?;
            continue;
        }
        // How long the menu took to draw in this frame, for the selftest.
        let mut menu_drawn: Option<Duration> = None;
        let dt = last_time
            .map_or(0.0f32, |t| (time.as_nanos() - t.as_nanos()) as f32 * 1e-9)
            .clamp(0.0, 0.1);
        last_time = Some(time);

        controls.sync(&session, &local, time)?;
        if !focused {
            controls.state = Default::default();
        }
        let (head, forward, right) =
            view.locate(&local, time)
                .map_or((Vec3::ZERO, -Vec3::Z, Vec3::X), |l| {
                    let turn = xrutil::quat(l.pose.orientation);
                    (vec3(l.pose.position), turn * -Vec3::Z, turn * Vec3::X)
                });

        scene.head = (head, forward);
        scene.head_right = right;
        if scene.view_eye.is_none_or(|e| e.distance(head) > 0.05) && head != Vec3::ZERO {
            scene.view_eye = Some(head);
        }
        scene.world.follow_head(&scene.settings, dt, scene.head);
        scene.place_sound();
        scene.remind_rest(dt);
        if std::mem::take(&mut scene.refresh_due) && exts.fb_display_refresh_rate {
            set_refresh_rate(&session, scene.settings.battery_saver);
        }
        if focused {
            scene.follow_gaze();
        }

        // Tracked hands: joints for drawing, fingertips for touching the menu.
        let joints: [Option<[(Vec3, f32); 26]>; 2] = match &hand_trackers {
            Some(trackers) if focused && scene.settings.hands != HandMode::Hidden => {
                std::array::from_fn(|h| {
                    local
                        .locate_hand_joints(&trackers[h], time)
                        .ok()
                        .flatten()
                        .and_then(|l| overlay::joints(&l))
                })
            }
            _ => [None, None],
        };
        let tips: [Option<Vec3>; 2] = std::array::from_fn(|h| joints[h].map(|j| j[INDEX_TIP].0));
        let hands_in_view = tips.iter().any(Option::is_some);
        scene.hands_in_view = hands_in_view;
        controls.hands_from_joints(&joints, head, forward);
        let menu_hand = usize::from(scene.settings.left_handed);
        let wrist = palm_menu_place(
            joints[menu_hand].as_ref(),
            menu_hand == 1,
            head,
            scene.world.wrist(),
            &mut palm_since,
        );
        scene.world.set_wrist(wrist);

        // Pairing needs the menu, and so do the first-run tips: open it where the
        // user is looking.
        let first_run =
            (!scene.settings.intro_seen && scene.host.is_some()) || scene.update_needed.is_some();
        if (scene.pairing.is_some() || scene.pair_offer.is_some() || first_run) && !scene.panel_open
        {
            scene.open_panel(head, forward, hands_in_view);
        }
        // Without a PC there is nothing to see: show the connection screen.
        if !scene.panel_open
            && scene
                .offline_since
                .is_some_and(|t| t.elapsed() > Duration::from_secs(4))
        {
            scene.open_panel(head, forward, hands_in_view);
            scene.offline_since = None;
        }
        // The left hand's menu gesture counts as the menu button.
        let gesture = match &hand_trackers {
            Some(t) if exts.fb_hand_tracking_aim => {
                xrutil::hand_menu_gesture(&instance, &t[0], &local, time)
            }
            _ => false,
        };
        let gesture_pressed = gesture && !menu_gesture_held;
        menu_gesture_held = gesture;
        if controls.menu_pressed(&session) || gesture_pressed {
            if scene.panel_open {
                scene.panel_open = false;
            } else {
                scene.open_panel(head, forward, hands_in_view);
            }
        }
        if controls.align_pressed(&session) {
            scene.tidy_up();
        }
        // What the selftest asks for, as the menu would do it.
        match std::mem::replace(&mut selftest_act, Act::Nothing) {
            Act::MoveMonitor => {
                if let Some(&(id, _, _)) = scene.world.monitors().first() {
                    let took = Instant::now();
                    let action = Action::Move { id, delta: 1 };
                    handle_action(&mut scene, action, &mut passthrough, &mut controls);
                    // Handed to the files' writer now, so the time is the whole of what
                    // saving costs the frame.
                    scene.save_due();
                    if let Some(t) = test.as_deref_mut() {
                        t.saved(took.elapsed());
                    }
                }
            }
            Act::OpenMenu => scene.open_panel(head, forward, hands_in_view),
            Act::SwitchTab => {
                let tab = match scene.tab {
                    Tab::Monitors => Tab::Quick,
                    _ => Tab::Monitors,
                };
                let took = Instant::now();
                handle_action(
                    &mut scene,
                    Action::Tab(tab),
                    &mut passthrough,
                    &mut controls,
                );
                scene.save_due();
                if let Some(t) = test.as_deref_mut() {
                    t.saved(took.elapsed());
                }
            }
            Act::CloseMenu => {
                handle_action(&mut scene, Action::Close, &mut passthrough, &mut controls);
            }
            Act::Nothing | Act::Finish => {}
        }
        scene.world.place_button(&scene.settings);
        scene.expire_asks();
        scene.adding_due();
        // The system may let the app record now, or not any more.
        if scene.mic.permission() != scene.mic_permission {
            scene.mic_changed();
        }
        // The palm menu shows the battery: the system thread reads it afresh when the
        // menu appears, and more often while it shows.
        if scene.world.wrist().is_some() != palm_was_shown {
            palm_was_shown = scene.world.wrist().is_some();
            system_thread.palm_menu(palm_was_shown);
        }

        // Pointing, clicking and dragging.
        let can = Pickable {
            menu: scene.panel_open,
            attach: scene.attach_for.map(|(id, _)| id),
        };
        let mut picks: [Option<Target>; 2] = std::array::from_fn(|h| {
            let asks = &scene.asks;
            controls.state[h].ray.and_then(|r| {
                scene.world.pick(r, &scene.settings, can, |k| {
                    asks.waits(ui::Asked::Remove(k))
                })
            })
        });
        // A detached window pointed at shows "Attach" under it; it lingers so the
        // pointer can move down onto it.
        let pointed = picks.iter().flatten().find_map(|p| match p.thing {
            Thing::Display(id) | Thing::Attach(id) if scene.world.is_window(id) => Some(id),
            _ => None,
        });
        if let Some(id) = pointed {
            scene.attach_for = Some((id, Instant::now()));
        } else if scene
            .attach_for
            .is_some_and(|(id, t)| t.elapsed() > ATTACH_LINGER || !scene.monitors.contains_key(&id))
        {
            scene.attach_for = None;
        }

        // The wrist menu: pointed at, or touched with the right index finger.
        let wrist_point = |local: Vec2| {
            (
                (local.x / WRIST_MENU_WIDTH + 0.5) * ui::WRIST_WIDTH as f32,
                (0.5 - local.y / wrist_menu_height()) * ui::WRIST_HEIGHT as f32,
            )
        };
        let mut wrist_hover = picks
            .iter()
            .flatten()
            .find_map(|p| matches!(p.thing, Thing::Wrist).then(|| wrist_point(p.hit.local)));
        let mut wrist_pressed = false;
        let poking_hand = 1 - usize::from(scene.settings.left_handed);
        if let (Some(w), Some(tip)) = (scene.world.wrist(), tips[poking_hand]) {
            let local = w.rotation.inverse() * (tip - w.position);
            let inside =
                local.x.abs() < WRIST_MENU_WIDTH / 2.0 && local.y.abs() < wrist_menu_height() / 2.0;
            if inside && local.z < 0.04 {
                wrist_hover = Some(wrist_point(Vec2::new(local.x, local.y)));
            }
            if inside && local.z < 0.008 && local.z > -0.03 && wrist_poke_armed {
                wrist_poke_armed = false;
                wrist_pressed = true;
                rings.push((tip, Instant::now()));
            }
            if !inside || local.z > 0.02 {
                wrist_poke_armed = true;
            }
        } else {
            wrist_poke_armed = true;
        }
        if scene.world.wrist().is_some() {
            let ui_state = scene.ui_state(
                passthrough.is_some(),
                passthrough
                    .as_ref()
                    .is_some_and(passthrough::Passthrough::window_supported),
                exts.khr_composition_layer_equirect2,
                custom_pano,
                store.update_hint(),
            );
            wrist_panel.set_state(&ui_state);
            wrist_panel.hover(wrist_hover);
            if wrist_pressed && let Some(action) = wrist_panel.hovered_action() {
                controls.pulse(&session, poking_hand);
                handle_action(&mut scene, action, &mut passthrough, &mut controls);
            }
        }
        let panel_point = |hit: &Hit, large: bool| {
            let x = (hit.local.x / panel_width(large) + 0.5) * ui::WIDTH as f32;
            let y = (0.5 - hit.local.y / panel_height(large)) * ui::HEIGHT as f32;
            (x, y)
        };

        // Touching the menu with a fingertip: hover when close, press on contact,
        // drag the menu by touching an empty spot.
        let mut poke_hover: [Option<(Vec2, f32)>; 2] = [None, None];
        for h in 0..2 {
            let poke = &mut pokes[h];
            let Some(tip) = tips[h].filter(|_| scene.panel_open) else {
                *poke = Poke {
                    touched_at: poke.touched_at,
                    ..Poke::default()
                };
                continue;
            };
            let place = scene.world.menu();
            let l = place.rotation.inverse() * (tip - place.position);
            let (half_w, half_h) = (
                panel_width(scene.settings.large_menu) / 2.0,
                panel_height(scene.settings.large_menu) / 2.0,
            );
            if let Some(anchor) = poke.drag {
                if l.z > POKE_RELEASE {
                    *poke = Poke {
                        touched_at: poke.touched_at,
                        ..Poke::default()
                    };
                    continue;
                }
                // The touched point follows the finger; pushing deeper moves it away.
                let shift = Vec3::new(l.x - anchor.x, l.y - anchor.y, (l.z + 0.03).min(0.0));
                let position = place.position + place.rotation * shift;
                scene.world.move_menu(position, head);
                picks[h] = None;
                continue;
            }
            let inside = l.x.abs() <= half_w && l.y.abs() <= half_h;
            if !inside || l.z > POKE_HOVER || l.z < POKE_BEHIND {
                poke.last_z = None;
                poke.pressed = false;
                continue;
            }
            // Near the menu, the finger is the pointer, not this hand's ray.
            picks[h] = None;
            let at = Vec2::new(l.x, l.y);
            poke_hover[h] = Some((at, l.z));
            let px = (
                (l.x / panel_width(scene.settings.large_menu) + 0.5) * ui::WIDTH as f32,
                (0.5 - l.y / panel_height(scene.settings.large_menu)) * ui::HEIGHT as f32,
            );
            if poke.pressed && l.z > POKE_RELEASE {
                poke.pressed = false;
            }
            let arrived = poke.last_z.is_some_and(|z| z >= POKE_PRESS);
            // Hand tracking jitters: a finger "bouncing" off the menu is not a second tap.
            let settled = poke
                .touched_at
                .is_none_or(|t| t.elapsed() > Duration::from_millis(400));
            if !poke.pressed && arrived && settled && l.z < POKE_PRESS {
                poke.pressed = true;
                poke.touched_at = Some(Instant::now());
                rings.push((
                    place.position + place.rotation * Vec3::new(l.x, l.y, 0.003),
                    Instant::now(),
                ));
                panel.hover(Some(px));
                match panel.hovered_action() {
                    Some(action) => {
                        handle_action(&mut scene, action, &mut passthrough, &mut controls);
                    }
                    None => poke.drag = Some(at),
                }
            }
            poke.last_z = Some(l.z);
        }

        let poke_px = poke_hover.iter().flatten().next().map(|(at, _)| {
            (
                (at.x / panel_width(scene.settings.large_menu) + 0.5) * ui::WIDTH as f32,
                (0.5 - at.y / panel_height(scene.settings.large_menu)) * ui::HEIGHT as f32,
            )
        });
        let hover = poke_px.or_else(|| {
            [1, 0].into_iter().find_map(|h| match picks[h] {
                Some(Target {
                    thing: Thing::Menu,
                    hit,
                }) => Some(panel_point(&hit, scene.settings.large_menu)),
                _ => None,
            })
        });
        panel.hover(hover);
        let over_button = picks.iter().any(|p| {
            matches!(
                p,
                Some(Target {
                    thing: Thing::MenuButton,
                    ..
                })
            )
        });
        if over_button != button_hovered {
            button_hovered = over_button;
            button_layer.upload(&gl, &ui::menu_button_image(BUTTON_PX, over_button))?;
        }

        // Both hands on one monitor: the distance between them sets its width.
        if let Some(s) = scene.scaling
            && let (Some(a), Some(b)) = (controls.state[0].ray, controls.state[1].ray)
        {
            let d = a.origin.distance(b.origin);
            let width = (s.start_width * d / s.start_distance.max(0.05)).clamp(0.2, 8.0);
            scene.world.resize(&scene.settings, width);
        }

        #[allow(clippy::needless_range_loop)] // `hand` indexes several per-hand arrays
        for hand in 0..2 {
            let mut input = controls.state[hand];
            // A hand that just touched the menu: its next pinch is the hand pulling
            // back, not a click.
            if input.is_hand
                && pokes[hand]
                    .touched_at
                    .is_some_and(|t| t.elapsed() < AFTER_TOUCH)
            {
                input.select.pressed = false;
            }
            if input.select.pressed {
                let mut clicked = false;
                match picks[hand] {
                    Some(Target {
                        thing: Thing::Attach(id),
                        hit,
                    }) => {
                        clicked = true;
                        rings.push((hit.point, Instant::now()));
                        controls.pulse(&session, hand);
                        scene.attach_for = None;
                        handle_action(
                            &mut scene,
                            Action::CloseWindow(id),
                            &mut passthrough,
                            &mut controls,
                        );
                    }
                    Some(Target {
                        thing: Thing::Menu,
                        hit,
                    }) => {
                        panel.hover(Some(panel_point(&hit, scene.settings.large_menu)));
                        if let Some(action) = panel.hovered_action() {
                            if input.is_hand {
                                // Decided when the pinch opens: a click, or a drag.
                                pending_click[hand] = Some((action, hit.point));
                            } else {
                                rings.push((hit.point, Instant::now()));
                                clicked = true;
                                controls.pulse(&session, hand);
                                handle_action(&mut scene, action, &mut passthrough, &mut controls);
                            }
                        }
                    }
                    Some(Target {
                        thing: Thing::Wrist,
                        hit,
                    }) => {
                        clicked = true;
                        wrist_panel.hover(Some(wrist_point(hit.local)));
                        if let Some(action) = wrist_panel.hovered_action() {
                            rings.push((hit.point, Instant::now()));
                            controls.pulse(&session, hand);
                            handle_action(&mut scene, action, &mut passthrough, &mut controls);
                        }
                    }
                    Some(Target {
                        thing: Thing::MenuButton,
                        hit,
                    }) => {
                        rings.push((hit.point, Instant::now()));
                        clicked = true;
                        controls.pulse(&session, hand);
                        if scene.panel_open {
                            scene.panel_open = false;
                        } else {
                            scene.open_panel(head, forward, hands_in_view);
                        }
                    }
                    _ => {}
                }
                // Hands drag with pinch-and-hold, like the Quest's own windows.
                if input.is_hand && !clicked && picks[hand].is_some() {
                    hold_since[hand] = Some(Instant::now());
                }
            }
            if input.select.released {
                hold_since[hand] = None;
                if let Some((action, point)) = pending_click[hand].take() {
                    rings.push((point, Instant::now()));
                    controls.pulse(&session, hand);
                    handle_action(&mut scene, action, &mut passthrough, &mut controls);
                }
            }
            let (start_grab, end_grab) = if input.is_hand {
                let held = input.select.down
                    && hold_since[hand].is_some_and(|t| t.elapsed() >= HOLD_TO_DRAG);
                (held && scene.grabs[hand].is_none(), input.select.released)
            } else {
                (input.grab.pressed, input.grab.released)
            };

            if end_grab && scene.scaling.is_some_and(|s| s.hand == hand) {
                scene.scaling = None;
                scene.world.resize_end();
            }
            if end_grab && let Some(released) = scene.grabs[hand].take() {
                // Letting go with the first hand ends resizing too.
                scene.scaling = None;
                // Let go next to another monitor: it sticks to it, edge to edge.
                if scene
                    .world
                    .let_go(released.id, &scene.settings, Instant::now())
                {
                    controls.pulse(&session, hand);
                }
                scene.world_done();
            }
            let Some(ray) = input.ray else { continue };
            if let Some(mut grab) = scene.grabs[hand] {
                // Moved from where it goes now; the world keeps the keyboard flat, a
                // monitor on a wall parallel to it, and moves the rest along with
                // "move all together".
                if let Some(mut p) = scene.world.place(grab.id) {
                    grab.update(&mut p, ray, head, input.stick, dt);
                    scene.world.drag(&scene.settings, grab.id, p, head);
                    scene.grabs[hand] = Some(grab);
                }
            } else if start_grab {
                hold_since[hand] = None;
                // Held on a button: the menu moves, the button is not pressed.
                pending_click[hand] = None;
                let other = scene.grabs[1 - hand].map(|g| g.id);
                let grabbed = match picks[hand] {
                    Some(Target {
                        thing: thing @ (Thing::Menu | Thing::Keyboard),
                        hit,
                    }) if other != Some(thing) => {
                        take(&mut scene.world, thing, hit, &scene.settings)
                    }
                    // The other hand already holds it: resize with both.
                    Some(Target {
                        thing: Thing::Display(id),
                        ..
                    }) if other == Some(Thing::Display(id)) => {
                        if let (Some(a), Some(b)) = (controls.state[0].ray, controls.state[1].ray)
                            && let Some(start_width) = scene.world.resize_start(id)
                        {
                            scene.scaling = Some(Scaling {
                                id,
                                hand,
                                start_distance: a.origin.distance(b.origin),
                                start_width,
                            });
                            controls.pulse(&session, hand);
                        }
                        None
                    }
                    Some(Target {
                        thing: thing @ Thing::Display(_),
                        hit,
                    }) => take(&mut scene.world, thing, hit, &scene.settings),
                    _ => None,
                }
                .map(|g| {
                    if input.is_hand {
                        g.with_reach(ray.origin, head)
                    } else {
                        g
                    }
                });
                if grabbed.is_some() {
                    controls.pulse(&session, hand);
                    scene.grabs[hand] = grabbed;
                }
            }
        }

        // Menu contents.
        if scene.panel_open && !panel_was_open {
            // The user may have copied a picture onto the headset meanwhile.
            custom_pano = custom_panorama_path(external_dir.as_deref()).is_some();
            // Or a language file: listed again on the system thread.
            system_thread.list_languages();
            // And paired with another PC.
            #[expect(
                clippy::disallowed_methods,
                reason = "the trusted PCs are read here until step 7.6 gives them to the network thread"
            )]
            let paired = prostir_proto::secure::TrustStore::load(&scene.trust_file);
            scene.paired = paired.entries().to_vec();
        }
        // The network thread names a PC over USB in the menu's language.
        if *scene.texts() != told_texts {
            told_texts = scene.texts().clone();
            scene.steering.set_texts(told_texts.clone());
        }
        panel_was_open = scene.panel_open;
        // A new toast: drawn once, placed where the user looks now.
        if let Some((text, _, place @ None)) = &mut scene.toast {
            toast_layer.upload(&gl, &panel.toast_image(text, TOAST_PX.0, TOAST_PX.1))?;
            let (head, forward) = scene.head;
            let at = head + forward.normalize_or(-Vec3::Z) * 0.7 - Vec3::Y * 0.18;
            *place = Some(Placement {
                position: at,
                rotation: layout::facing(at, head),
                width: TOAST_WIDTH,
            });
        }
        if scene
            .toast
            .as_ref()
            .is_some_and(|t| t.1.elapsed() > TOAST_FOR)
        {
            scene.toast = None;
        }
        if scene.attach_for.is_some() {
            let lit = picks
                .iter()
                .flatten()
                .any(|p| matches!(p.thing, Thing::Attach(_)));
            if attach_drawn
                .as_ref()
                .is_none_or(|(t, l)| t != scene.texts() || *l != lit)
            {
                let label = scene.texts().t("attach");
                attach_layer
                    .upload(&gl, &panel.pill_image(label, ATTACH_PX.0, ATTACH_PX.1, lit))?;
                attach_drawn = Some((scene.texts().clone(), lit));
            }
        }
        if let Some(a) = scene.world.adding()
            && placeholder
                .as_ref()
                .is_none_or(|p| p.1 != a.size || p.2 != *scene.texts())
        {
            let (w, h) = a.size;
            let px_h = (PLACEHOLDER_PX * h / w.max(1)).clamp(64, 4 * PLACEHOLDER_PX);
            let mut layer = ImageLayer::new(&session, format, PLACEHOLDER_PX, px_h, true)?;
            let label = scene.texts().t("adding-monitor");
            layer.upload(&gl, &panel.placeholder_image(label, PLACEHOLDER_PX, px_h))?;
            placeholder = Some((layer, a.size, scene.texts().clone()));
        }
        if scene.world.wrist().is_some()
            && let Some(pixels) = wrist_panel.render_if_dirty()
        {
            wrist_layer.upload(&gl, pixels)?;
        }
        if scene.panel_open {
            let ui_state = scene.ui_state(
                passthrough.is_some(),
                passthrough
                    .as_ref()
                    .is_some_and(passthrough::Passthrough::window_supported),
                exts.khr_composition_layer_equirect2,
                custom_pano,
                store.update_hint(),
            );
            panel.set_state(&ui_state);
            let drawing = test.is_some().then(Instant::now);
            if let Some(pixels) = panel.render_if_dirty() {
                menu_drawn = drawing.map(|t| t.elapsed());
                panel_layer.upload(&gl, pixels)?;
            }
        }

        // Hands and rays: only rendered when there is something to show.
        let mut spheres: Vec<Sphere> = Vec::new();
        // Light on the menu where a fingertip is about to touch: wide and faint from a
        // few centimetres away, small and bright right before contact.
        let place = scene.world.menu();
        for (h, hover) in poke_hover.iter().enumerate() {
            let Some((at, z)) = hover else { continue };
            let near = (1.0 - z.max(0.0) / POKE_HOVER).clamp(0.0, 1.0);
            let pressed = if pokes[h].pressed { 0.25 } else { 0.0 };
            spheres.push(Sphere::glow(
                place.position + place.rotation * Vec3::new(at.x, at.y, 0.003),
                0.034 - 0.020 * near,
                [
                    0.72,
                    0.84,
                    1.0,
                    (0.12 + 0.6 * near * near + pressed).min(1.0),
                ],
            ));
        }
        // A touch spreads out as a ring for a moment.
        rings.retain(|(_, at)| at.elapsed() < TOUCH_RING);
        for (center, at) in &rings {
            let k = at.elapsed().as_secs_f32() / TOUCH_RING.as_secs_f32();
            spheres.push(Sphere::ring(
                *center,
                0.012 + 0.03 * k,
                [0.8, 0.9, 1.0, 0.9 * (1.0 - k)],
            ));
        }
        // Where a ray points: a small light on the menu, a wider faint one on monitors.
        for pick in picks.iter().flatten() {
            let (radius, alpha) = match pick.thing {
                Thing::Menu | Thing::MenuButton | Thing::Keyboard => (0.012, 0.55),
                Thing::Display(_) | Thing::Adding { .. } => (0.03, 0.3),
                Thing::Attach(_) => (0.01, 0.5),
                Thing::Wrist => (0.008, 0.55),
            };
            spheres.push(Sphere::glow(
                pick.hit.point,
                radius,
                [0.72, 0.84, 1.0, alpha],
            ));
        }
        // Where the mouse is: rings closing in on it, three times.
        if let Some((id, uv, at)) = scene.finder {
            let age = at.elapsed();
            match (scene.world.shown(id), age < FINDER_FOR) {
                (Some(p), true) => {
                    let c = scene
                        .world
                        .curve_radius(&scene.settings)
                        .filter(|_| !scene.world.is_window(id));
                    let (point, _) = layout::image_point(p, scene.world.height_of(id), c, uv);
                    let t = age.as_secs_f32() / FINDER_FOR.as_secs_f32();
                    for k in 0..3 {
                        let phase = (t * 3.0 - k as f32 * 0.33).rem_euclid(1.0);
                        spheres.push(Sphere::ring(
                            point,
                            0.09 * (1.0 - phase) + 0.01,
                            [1.0, 0.85, 0.35, 0.9 * phase * (1.0 - t * 0.5)],
                        ));
                    }
                    spheres.push(Sphere::glow(point, 0.02, [1.0, 0.85, 0.35, 0.8]));
                }
                _ => scene.finder = None,
            }
        }
        // The magnet while dragging: the edge the monitor would stick to lights up.
        for g in scene.grabs.iter().flatten() {
            let Some(s) =
                g.id.display()
                    .and_then(|id| scene.world.stick_of(&scene.settings, id))
            else {
                continue;
            };
            let Some(n) = scene.world.shown(s.neighbour).copied() else {
                continue;
            };
            let (half_w, half_h) = (n.width / 2.0, scene.world.height_of(s.neighbour) / 2.0);
            let steps = 28;
            for i in 0..=steps {
                let k = i as f32 / steps as f32 * 2.0 - 1.0;
                let local = if s.across {
                    Vec3::new(s.side * half_w, k * half_h, 0.01)
                } else {
                    Vec3::new(k * half_w, s.side * half_h, 0.01)
                };
                spheres.push(Sphere::glow(
                    n.position + n.rotation * local,
                    0.03,
                    [0.55, 0.75, 1.0, 0.55],
                ));
            }
        }
        // Hands are drawn over everything (the compositor has no depth for them), so
        // near a menu they would cover its text: the hand holding the palm menu is not
        // drawn, and a hand about to touch a menu is see-through.
        for (h, j) in joints.iter().enumerate() {
            let Some(j) = j else { continue };
            if h == menu_hand && scene.world.wrist().is_some() {
                continue;
            }
            let tip = j[INDEX_TIP].0;
            let near_palm = scene
                .world
                .wrist()
                .is_some_and(|w| w.position.distance(tip) < 0.15);
            let near_panel = poke_hover[h].is_some();
            let alpha = if near_palm || near_panel { 0.3 } else { 1.0 };
            overlay::hand_spheres(j, alpha, &mut spheres);
        }
        for (input, pick) in controls.state.iter().zip(&picks) {
            if let Some(ray) = input.ray {
                let length = pick.map_or(1.2, |p| p.hit.t).min(4.0);
                overlay::ray_spheres(ray.origin, ray.dir, length, &mut spheres);
                if !input.is_hand {
                    // Where the controller is, since it is not visible otherwise.
                    spheres.push(Sphere {
                        center: ray.origin,
                        radius: 0.012,
                        color: [0.30, 0.32, 0.38, 1.0],
                        kind: overlay::Kind::Ball,
                    });
                }
            }
        }
        let views = if spheres.is_empty() {
            None
        } else {
            session
                .locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, time, &local)
                .ok()
                .map(|(_, v)| v)
        };
        if let Some(ov) = &mut overlay {
            ov.render(&gl, views.as_deref().unwrap_or(&[]), &mut spheres)?;
        }
        let projection_views = overlay
            .as_ref()
            .zip(views.as_ref())
            .and_then(|(o, v)| o.layer_views(v));
        let hands_layer = projection_views
            .as_ref()
            .map(|v| overlay::projection_layer(&local, v));

        // ---- Layers, back to front ----
        // Desktop text is denser than the display, so ask the compositor to supersample
        // and sharpen instead of shimmering through plain bilinear sampling.
        let quality = xr::sys::CompositionLayerSettingsFB {
            ty: xr::sys::CompositionLayerSettingsFB::TYPE,
            next: ptr::null(),
            layer_flags: xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SUPER_SAMPLING
                | xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SHARPENING,
        };
        let quality_next: *const std::ffi::c_void = if exts.fb_composition_layer_settings {
            (&raw const quality).cast()
        } else {
            ptr::null()
        };
        // Android surfaces store rows top-down while the compositor samples them
        // GL-style (bottom-up), which shows the desktop upside down.
        let flip = xr::sys::CompositionLayerImageLayoutFB {
            ty: xr::sys::CompositionLayerImageLayoutFB::TYPE,
            next: quality_next.cast_mut(),
            flags: xr::sys::CompositionLayerImageLayoutFlagsFB::VERTICAL_FLIP,
        };
        let video_next = if exts.fb_composition_layer_image_layout {
            (&raw const flip).cast()
        } else {
            quality_next
        };

        // The keyboard window: placed in front of the user at desk height the first
        // time, then wherever it was moved.
        if let Some(pt) = passthrough.as_mut().filter(|pt| pt.window_supported()) {
            let place = scene
                .settings
                .keyboard_window
                .then(|| scene.world.keyboard_here((head, forward)));
            let shown = place.map(|p| (posef(&p), p.width, p.width * KEYBOARD_ASPECT));
            if let Err(e) = pt.set_window(&local, time, shown) {
                warn!("keyboard window: {e:#}");
                scene.settings.keyboard_window = false;
            }
        }
        let passthrough_layer = |handle: Option<xr::sys::PassthroughLayerFB>| {
            handle.map(|layer_handle| xr::sys::CompositionLayerPassthroughFB {
                ty: xr::sys::CompositionLayerPassthroughFB::TYPE,
                next: ptr::null(),
                flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
                space: xr::sys::Space::NULL,
                layer_handle,
            })
        };
        let keyboard_layer = passthrough_layer(
            passthrough
                .as_ref()
                .and_then(passthrough::Passthrough::window_layer),
        );
        let passthrough_layer = passthrough_layer(
            passthrough
                .as_ref()
                .and_then(passthrough::Passthrough::room_layer),
        );
        let pano = pano_layer
            .as_ref()
            .filter(|(b, _)| *b == scene.settings.background)
            .map(|(_, l)| xr::sys::CompositionLayerEquirect2KHR {
                ty: xr::sys::CompositionLayerEquirect2KHR::TYPE,
                next: ptr::null(),
                layer_flags: xr::CompositionLayerFlags::EMPTY,
                space: local.as_raw(),
                eye_visibility: xr::EyeVisibility::BOTH,
                sub_image: l.sub_image(),
                pose: xr::Posef::IDENTITY,
                radius: 0.0,
                central_horizontal_angle: std::f32::consts::TAU,
                upper_vertical_angle: std::f32::consts::FRAC_PI_2,
                lower_vertical_angle: -std::f32::consts::FRAC_PI_2,
            });

        // Far monitors first so nearer (and grabbed) ones draw on top.
        scene.world.animate(&scene.settings, dt);
        // Keep the desktop's arrangement in step once nothing is being moved.
        scene.world.settle(Instant::now());
        scene.world_done();
        let asks = &scene.asks;
        // Gone from view as soon as asked to go; back if the PC does not remove it. Far
        // to near, two at about one distance (a monitor glued over a row and that row)
        // in the order they had, so where they overlap never flickers.
        scene
            .world
            .order_drawing(head, |k| asks.waits(ui::Asked::Remove(k)));
        // Focus mode: monitors without the mouse fade to a dim level (colour scale on
        // the compositor layer, no extra rendering).
        scene.world.brighten(&scene.settings, dt, scene.focus);
        let curve = scene.world.curve_radius(&scene.settings);
        let order = scene.world.draw_list();
        let dims: Vec<xr::sys::CompositionLayerColorScaleBiasKHR> = order
            .iter()
            .map(|(_, id)| {
                // Fading in: premultiplied, so colour and alpha scale together.
                let t = scene.world.appear(*id);
                let t = t * t * (3.0 - 2.0 * t);
                // Focus dimming, the brightness setting and fading in, together.
                let b =
                    scene.world.brightness(*id) * t * f32::from(scene.settings.brightness) / 100.0;
                xr::sys::CompositionLayerColorScaleBiasKHR {
                    ty: xr::sys::CompositionLayerColorScaleBiasKHR::TYPE,
                    next: video_next,
                    color_scale: xr::Color4f {
                        r: b,
                        g: b,
                        b,
                        a: t,
                    },
                    color_bias: xr::Color4f {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    },
                }
            })
            .collect();
        let mut quads = Vec::new();
        let mut cylinders = Vec::new();
        for (i, (_, id)) in order.iter().enumerate() {
            let scale = dims[i].color_scale;
            let fading = exts.khr_composition_layer_color_scale_bias && scale.a < 1.0;
            let layer_flags = if fading {
                xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA
            } else {
                xr::CompositionLayerFlags::EMPTY
            };
            let next = if exts.khr_composition_layer_color_scale_bias && scale.r < 1.0 {
                (&raw const dims[i]).cast()
            } else {
                video_next
            };
            let (Some(m), Some(p)) = (scene.monitors.get(id), scene.world.shown(*id)) else {
                continue;
            };
            let height = p.width * m.aspect();
            let sub_image = xr::sys::SwapchainSubImage {
                swapchain: m.swapchain.as_raw(),
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di {
                        width: m.width_px as i32,
                        height: m.height_px as i32,
                    },
                },
                image_array_index: 0,
            };
            // Window panels stay flat.
            if let Some(radius) = curve.filter(|_| !scene.world.is_window(*id)) {
                let c = Cylinder::new(p, radius);
                cylinders.push(xr::sys::CompositionLayerCylinderKHR {
                    ty: xr::sys::CompositionLayerCylinderKHR::TYPE,
                    next,
                    layer_flags,
                    space: local.as_raw(),
                    eye_visibility: xr::EyeVisibility::BOTH,
                    sub_image,
                    pose: pose(c.center, p.rotation),
                    radius: c.radius,
                    central_angle: c.central_angle,
                    aspect_ratio: c.arc_length() / height,
                });
            } else {
                quads.push(xr::sys::CompositionLayerQuad {
                    ty: xr::sys::CompositionLayerQuad::TYPE,
                    next,
                    layer_flags,
                    space: local.as_raw(),
                    eye_visibility: xr::EyeVisibility::BOTH,
                    sub_image,
                    pose: posef(p),
                    size: xr::Extent2Df {
                        width: p.width,
                        height,
                    },
                });
            }
        }

        let button = scene
            .world
            .button()
            .map(|b| button_layer.quad(&local, posef(&b), BUTTON_SIZE, BUTTON_SIZE));
        let panel_quad = (scene.panel_open && panel_layer.ready()).then(|| {
            panel_layer.quad(
                &local,
                posef(&scene.world.menu()),
                panel_width(scene.settings.large_menu),
                panel_height(scene.settings.large_menu),
            )
        });
        let mut cursors: Vec<xr::sys::CompositionLayerQuad> = picks
            .iter()
            .flatten()
            .map(|&Target { hit, .. }| {
                let size = 0.011 * hit.t.clamp(0.3, 4.0);
                let at = hit.point + hit.normal * 0.004;
                cursor_layer.quad(
                    &local,
                    pose(at, layout::facing(at, at + hit.normal)),
                    size,
                    size,
                )
            })
            .collect();
        // Under a fingertip near the menu: a dot that shrinks as the finger gets closer.
        for (at, z) in poke_hover.iter().flatten() {
            let place = &scene.world.menu();
            let normal = place.rotation * Vec3::Z;
            let point =
                place.position + place.rotation * Vec3::new(at.x, at.y, 0.0) + normal * 0.002;
            let size = 0.006 + z.max(0.0) * 0.15;
            cursors.push(cursor_layer.quad(
                &local,
                pose(point, layout::facing(point, point + normal)),
                size,
                size,
            ));
        }

        let mut layers: Vec<&xr::CompositionLayerBase<'_, Gfx>> = Vec::with_capacity(16);
        if let Some(l) = &passthrough_layer {
            layers.push(as_layer(l));
        } else if let Some(l) = &pano {
            layers.push(as_layer(l));
        }
        layers.extend(keyboard_layer.iter().map(as_layer));
        layers.extend(cylinders.iter().map(as_layer));
        layers.extend(quads.iter().map(as_layer));
        let placeholder_quad = scene.world.adding().and_then(|a| {
            let (layer, _, _) = placeholder.as_ref()?;
            Some(layer.quad(&local, posef(&a.place), a.place.width, a.height()))
        });
        layers.extend(placeholder_quad.iter().map(as_layer));
        layers.extend(button.iter().map(as_layer));
        layers.extend(panel_quad.iter().map(as_layer));
        let toast_quad = scene.toast.as_ref().and_then(|t| t.2).map(|p| {
            toast_layer.quad(
                &local,
                posef(&p),
                TOAST_WIDTH,
                TOAST_WIDTH * TOAST_PX.1 as f32 / TOAST_PX.0 as f32,
            )
        });
        layers.extend(toast_quad.iter().map(as_layer));
        let attach_quad = scene
            .attach_for
            .filter(|_| attach_drawn.is_some())
            .and_then(|(id, _)| scene.world.attach_place(id))
            .map(|p| attach_layer.quad(&local, posef(&p), ATTACH_WIDTH, attach_height()));
        layers.extend(attach_quad.iter().map(as_layer));
        let wrist_quad = scene
            .world
            .wrist()
            .filter(|_| wrist_layer.ready())
            .map(|w| wrist_layer.quad(&local, posef(&w), WRIST_MENU_WIDTH, wrist_menu_height()));
        layers.extend(wrist_quad.iter().map(as_layer));
        layers.extend(hands_layer.iter().map(as_layer));
        layers.extend(cursors.iter().map(as_layer));
        let layers_wanted = layers.len();
        // The compositor supports 16 layers at most.
        layers.truncate(16);
        match frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &layers) {
            // One frame the compositor refused is not worth closing the app over.
            Err(
                e @ (xr::sys::Result::ERROR_POSE_INVALID
                | xr::sys::Result::ERROR_LAYER_INVALID
                | xr::sys::Result::ERROR_SWAPCHAIN_RECT_INVALID),
            ) => {
                warn!("frame refused: {e}");
            }
            other => other?,
        }
        if let (Some(t), Some(start)) = (test.as_deref_mut(), &frame_start) {
            t.frame_end(
                start,
                time.as_nanos(),
                Duration::from_nanos(state.predicted_display_period.as_nanos().max(0) as u64),
                menu_drawn,
                layers_wanted,
            );
        }
        allocations.frame();
    }

    info!("exiting");
    // What waits is written, and the files' writer ends (see `Scene`'s `Drop`).
    drop(scene);
    Ok(())
}

/// A hand takes `thing`, pointed at with `hit`, where it is drawn: a monitor easing
/// to its place goes on from there, under the hand (see `World::hold`).
fn take(world: &mut World, thing: Thing, hit: Hit, s: &Settings) -> Option<Grab> {
    world
        .hold(thing, s)
        .map(|at| Grab::start(thing, hit.t, hit.local, &at))
}

/// What to tell the user about a connection that ended; nothing where there is
/// nothing to say (the headset was off the head, or the PC already said why).
fn disconnect_text(why: net::Disconnect) -> Option<&'static str> {
    use prostir_proto::PcGoodbye;
    Some(match why {
        net::Disconnect::Goodbye(PcGoodbye::Quit) => "bye-pc-quit",
        net::Disconnect::Goodbye(PcGoodbye::Forgotten) => "bye-forgotten",
        net::Disconnect::Goodbye(PcGoodbye::Other) => "bye-pc-ended",
        net::Disconnect::Silence => "bye-silence",
        net::Disconnect::Goodbye(PcGoodbye::TakenOff | PcGoodbye::Replaced | PcGoodbye::Failed)
        | net::Disconnect::Closed => return None,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
fn handle_net_event(
    scene: &mut Scene,
    session: &xr::Session<Gfx>,
    vm: &jni::JavaVM,
    event: NetEvent,
) {
    match event {
        NetEvent::Connected {
            pc,
            addr,
            name,
            feedback,
            language,
            opened_with,
        } => {
            info!("connected to {addr} ({name:?}), PC {pc}, language {language:?}");
            scene.world.connected(pc);
            if language.is_some() && language != scene.settings.pc_language {
                scene.settings.pc_language = language;
                scene.saves.mark(persist::File::Settings);
            }
            let host = name.unwrap_or_else(|| {
                if addr.ip().is_loopback() {
                    scene.texts().t("pc-usb").to_owned()
                } else {
                    addr.ip().to_string()
                }
            });
            scene.host = Some(host);
            scene.feedback = Some(feedback);
            scene.asks.new_session();
            scene.update_needed = None;
            // Whether the system lets the app record (the PC shows when it does not).
            scene.mic.refresh_permission();
            // What changed since `Open` went (it carried the state of that moment).
            let now = scene.headset_state();
            if now.up != opened_with.up {
                scene.mic_changed();
            }
            if now.prefs != opened_with.prefs {
                scene.prefs_changed();
            }
            if now.presence != opened_with.presence {
                scene.tell_presence();
            }
            scene.offline_since = None;
            scene.ip_input = None;
            scene.unreachable = None;
        }
        NetEvent::Hosts(hosts) => scene.found = hosts,
        NetEvent::Unreachable(name) => scene.unreachable = name,
        NetEvent::Part(part) => part_came(scene, part),
        // Taken in the frame loop, where the menu's actions are done (see `take_command`).
        NetEvent::Command { .. } => {}
        NetEvent::Notice(text) => scene.show_toast(text),
        NetEvent::Reply { req, outcome } => match (scene.answered(req, &outcome), outcome) {
            (Some(ui::Asked::Profile(_)), Outcome::Failed { .. }) => scene.world.switch_to(None),
            (None, _) | (_, Outcome::Failed { .. }) => {}
            (_, Outcome::Saved { path }) => {
                let text = scene
                    .texts()
                    .f("toast-screenshot-saved", &[("path", path.into())]);
                scene.show_toast(text);
            }
            (_, Outcome::Pointer { id, u, v }) => {
                let f = |v: u16| f32::from(v) / 65535.0;
                scene.finder = scene
                    .key(id)
                    .map(|k| (k, Vec2::new(f(u), f(v)), Instant::now()));
            }
            (Some(ui::Asked::Profile(slot)), Outcome::Ids { ids }) => {
                if scene.world.switching_with(req) {
                    // Switched to: the PC made its monitors (which it shows comes with
                    // its list of displays). The menu is closed by now: a toast says it.
                    scene.active_profile = Some(slot);
                    let name = scene.profile_name(slot);
                    scene.show_toast(name);
                    let keys = ids.into_iter().filter_map(|id| scene.key(id)).collect();
                    scene
                        .world
                        .profile_made(keys, &scene.settings, scene.head.1, Instant::now());
                }
            }
            (Some(ui::Asked::AddMonitor), Outcome::Added { id }) => {
                let key = scene.key(id);
                scene.world.adding_made(req, key);
            }
            (_, Outcome::Done | Outcome::Added { .. } | Outcome::Ids { .. }) => {}
        },
        NetEvent::Problem { error, detail } => {
            warn!("a problem on the PC (code {})", error.code());
            scene.show_error(detail);
        }
        NetEvent::TooBig {
            name,
            size: (w, h),
            codec,
        } => {
            let text = scene.texts().f(
                "error-too-big",
                &[
                    ("name", name.into()),
                    ("size", format!("{w}×{h}").into()),
                    ("codec", format!("{codec:?}").into()),
                ],
            );
            scene.show_error(text);
        }
        NetEvent::Pairing { host, code } => {
            let failed = scene.pairing.as_ref().is_some_and(|p| p.failed);
            scene.pairing = Some(ui::PairingView {
                host,
                code,
                typed: String::new(),
                failed,
            });
        }
        NetEvent::PairingFailed => {
            if let Some(p) = &mut scene.pairing {
                p.failed = true;
                p.typed.clear();
            }
        }
        NetEvent::Paired => scene.pairing = None,
        NetEvent::PairOffer { host, why } => {
            scene.pairing = None;
            scene.pair_offer = Some((host, why));
        }
        NetEvent::Incompatible { pc_newer, pc_app } => {
            warn!("the PC runs Prostir {pc_app}: versions differ");
            scene.update_needed = Some((pc_newer, pc_app));
        }
        NetEvent::Disconnected { why } => {
            // What waited for an answer gets none, nor do the commands of that PC.
            for what in scene.asks.disconnected(Instant::now()) {
                scene.tell_failure(what, ui::Failure::Disconnected, "");
            }
            scene.offline_since = Some(Instant::now());
            scene.pairing = None;
            scene.host = None;
            scene.feedback = None;
            // The next session's list is news, not displays removed.
            scene.world.disconnected();
            scene.pc = PcState::default();
            if let Some(id) = disconnect_text(why) {
                let text = scene.texts().t(id).to_owned();
                scene.show_toast(text);
            }
        }
        NetEvent::HostError(text) => scene.show_error(text),
        NetEvent::StreamStart {
            id,
            generation,
            width,
            height,
            codec,
            frames,
            feedback,
        } => {
            let stream = decoder::Stream { id, generation };
            let Some(id) = scene.key(id) else {
                warn!("a stream of {id} before the session");
                return;
            };
            scene.monitors.remove(&id);
            match xrutil::create_surface_swapchain(session, vm, width, height) {
                Ok((swapchain, window)) => {
                    let stop = Arc::new(AtomicBool::new(false));
                    let decoder = decoder::spawn(decoder::DecoderJob {
                        stream,
                        codec,
                        width,
                        height,
                        window,
                        frames,
                        feedback,
                        stop: stop.clone(),
                    });
                    scene.monitors.insert(
                        id,
                        Monitor {
                            width_px: width,
                            height_px: height,
                            decoder: Some(decoder),
                            stop,
                            swapchain,
                        },
                    );
                    scene.world.stream_started(
                        id,
                        (width, height),
                        &scene.settings,
                        scene.head,
                        Instant::now(),
                    );
                }
                Err(e) => {
                    scene.world.stream_ended(id);
                    error!("monitor {id}: {e:#}");
                }
            }
        }
        // The stream stopped; the display keeps its place (only the list says it is gone).
        NetEvent::StreamEnd { id } => {
            if let Some(k) = scene.key(id) {
                scene.monitors.remove(&k);
                scene.world.stream_ended(k);
            }
        }
    }
}

/// The PC listed its displays anew (see `World::listed`): the displays gone end their
/// streams, and the switch of the PC's own displays shows how it stands.
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
fn displays_changed(scene: &mut Scene) {
    let Some(displays) = scene.pc.displays.as_ref() else {
        return;
    };
    if scene.world.pc().is_none() {
        return;
    }
    // As the switch says: its display shown; with none connected, any one kept shown.
    scene.mirror_wanted = physical::switch(displays).map_or_else(
        || {
            displays
                .displays
                .iter()
                .any(state::Display::is_shown_physical)
        },
        state::Display::is_shown_physical,
    );
    let gone = scene
        .world
        .listed(displays, &scene.settings, scene.head, Instant::now());
    for id in gone {
        scene.monitors.remove(id);
    }
}

/// A part of the PC's state came: kept, and what follows from it done.
fn part_came(scene: &mut Scene, part: prostir_proto::HostMsg) {
    use prostir_proto::HostMsg as M;
    let follow = match &part {
        M::Displays { .. } => Some(true),
        M::Sound { part } => {
            scene.audio_wanted = part.to_headset;
            None
        }
        M::Focus { part } => {
            scene.focus = part.display.and_then(|id| scene.key(id));
            None
        }
        M::SoundFrom { part } => {
            scene.sound_from = part.display.and_then(|id| scene.key(id));
            None
        }
        M::HostCaps { part } => {
            info!("the PC can: {:?}", part.features);
            None
        }
        _ => None,
    };
    // Checked by the network thread already: a part of the PC's state.
    if let Err(e) = scene.pc.apply(part) {
        warn!("a part of the PC's state: {e}");
    }
    if follow.is_some() {
        displays_changed(scene);
    }
}

/// The headset's settings the PC acts on, from `s`.
fn prefs_of(s: &Settings) -> Prefs {
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
fn headset_state(
    settings: &Settings,
    worn: bool,
    battery: Option<(u8, bool)>,
    mic: &Mic,
    model: &str,
) -> HeadsetState {
    let mic = mic_state(settings, mic);
    HeadsetState {
        caps: Some(headset_caps(model)),
        prefs: Some(prefs_of(settings)),
        presence: Some(Presence { worn }),
        battery: battery.map(|(percent, charging)| Battery { percent, charging }),
        // The Quest's own volume follows the PC's slider from 0.2.0 step 7.
        volume: None,
        up: std::iter::once((mic.kind, mic)).collect(),
    }
}

/// The microphone's switch, and whether the system lets the app record.
fn mic_state(settings: &Settings, mic: &Mic) -> state::UpState {
    state::UpState {
        kind: state::UpKind::Mic,
        switch: settings.mic,
        permission: mic.permission(),
    }
}

/// What this headset can do, for the PC: the displays it shows, the codecs it decodes
/// and how large (for now fixed: H.264 up to 4K, HEVC up to 8K), how many streams and
/// compositor layers at once; and its model, as the system names it.
fn headset_caps(model: &str) -> HeadsetCaps {
    let decoder = |codec: Codec, max_w, max_h| state::Decoder {
        codec: prostir_proto::wire::encode(&codec).map_or(0, |b| b[0]),
        max_w,
        max_h,
    };
    HeadsetCaps {
        features: vec![
            caps::headset::PHYSICAL,
            caps::headset::WINDOWS,
            caps::headset::COMMANDS,
            caps::headset::MIC,
        ],
        decoders: vec![
            decoder(Codec::H264, 4096, 4096),
            decoder(Codec::H265, 8192, 4320),
        ],
        streams_max: 16,
        layers_max: 16,
        model: model.to_owned(),
    }
}

/// Does what the PC asks (its tray, later `prostir ctl` and hotkeys), as the menu would
/// (see `commands`), and answers it once: after the parts it changed (`Prefs`), and for
/// a profile once the PC answered the profile's own request.
fn take_command(
    scene: &mut Scene,
    req: ReqId,
    asked: &Asked<Command>,
    passthrough: &mut Option<passthrough::Passthrough>,
    controls: &mut Controls,
) {
    use crate::commands::Does;
    use prostir_proto::state::ErrorCode;
    info!("the PC asks: {asked:?}");
    let outcome = match crate::commands::what_to_do(asked) {
        Does::Act(action) => {
            handle_action(scene, action, passthrough, controls);
            Outcome::Done
        }
        Does::Profile(i) if scene.profiles.get(i).is_none_or(Option::is_none) => {
            Outcome::failed(ErrorCode::NO_SUCH_PROFILE, &(i + 1).to_string())
        }
        Does::Profile(i) => {
            let before = scene.asks.last();
            handle_action(scene, Action::ApplyProfile(i), passthrough, controls);
            if scene.asks.last() > before {
                // Answered with the PC's answer to the profile's monitors.
                scene.asks.answer_command_after(scene.asks.last(), req);
                return;
            }
            Outcome::failed(ErrorCode::OTHER, "no connection")
        }
        Does::Closer(steps) => {
            scene.closer(steps);
            Outcome::Done
        }
        Does::NoLimits(on) => {
            scene.settings.no_limits = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
            Outcome::Done
        }
        Does::Fail(code, detail) => Outcome::failed(code, &detail),
    };
    scene.send(ClientMsg::Reply { req, outcome });
}

/// Passthrough cameras on only for the "room" background.
fn set_passthrough(scene: &mut Scene, passthrough: &mut Option<passthrough::Passthrough>) {
    let background = scene.settings.background;
    let opacity = if background == Background::RoomDim {
        0.4
    } else {
        1.0
    };
    if let Some(pt) = passthrough
        && let Err(e) = pt
            .set_room(background.is_room())
            .and_then(|()| pt.set_room_opacity(opacity))
    {
        let text = scene
            .texts()
            .f("error-cameras", &[("error", format!("{e:#}").into())]);
        scene.show_error(text);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
fn handle_action(
    scene: &mut Scene,
    action: Action,
    passthrough: &mut Option<passthrough::Passthrough>,
    controls: &mut Controls,
) {
    // Changing the setup by hand leaves the profile it came from.
    if matches!(
        action,
        Action::Add { .. }
            | Action::Remove(_)
            | Action::MakePrimary(_)
            | Action::Move { .. }
            | Action::SetMirror(_)
            | Action::Preset(_)
    ) {
        scene.active_profile = None;
    }
    match action {
        Action::Add { width, height } => {
            let req = scene.request(
                Request::AddDisplay {
                    mode: Mode::new(width, height),
                },
                ui::Asked::AddMonitor,
            );
            // Shown at once where it will appear; the monitor takes that place once
            // its picture comes (not in cinema, where new monitors are not placed).
            scene
                .world
                .adding_started(req, (width, height), scene.head.1, Instant::now());
        }
        Action::Remove(id) => {
            let confirmed = scene
                .confirm_remove
                .is_some_and(|(c, at)| c == id && at.elapsed() < CONFIRM_FOR);
            if confirmed {
                scene.confirm_remove = None;
                scene.request_for(
                    id,
                    |id| Request::RemoveDisplay { id },
                    ui::Asked::Remove(id),
                );
            } else {
                scene.confirm_remove = Some((id, Instant::now()));
            }
        }
        Action::Tab(tab) => {
            scene.tab = tab;
            scene.settings.last_tab = tab_code(tab);
            scene.saves.mark(persist::File::Settings);
        }
        Action::Recenter => {
            scene.recenter();
            // An open menu comes along, in front again; a closed one stays closed (the
            // palm menu has this button too).
            if scene.panel_open {
                let (head, forward) = scene.head;
                let touch = scene.hands_in_view;
                scene.open_panel(head, forward, touch);
            }
        }
        Action::IntroDone => {
            scene.settings.intro_seen = true;
            scene.saves.mark(persist::File::Settings);
        }
        Action::LanguagePage(page) => scene.language_page = page,
        Action::Language(language) => {
            scene.settings.language = language;
            scene.saves.mark(persist::File::Settings);
        }
        Action::CinemaScreen(screen) => {
            scene
                .world
                .pick_cinema(screen, &scene.settings, Instant::now());
        }
        Action::Cinema(on) => {
            scene.set_cinema(on);
            scene.saves.mark(persist::File::Settings);
        }
        Action::KeyboardWindow(on) => {
            scene.settings.keyboard_window = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Focus(on) => {
            scene.settings.focus = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::DetachWindow => {
            scene.request(Request::DetachWindow, ui::Asked::DetachWindow);
            // The panel appears in front: the menu steps aside.
            scene.panel_open = false;
        }
        Action::WindowHere => {
            if let Some(id) = scene.monitor_in_front() {
                scene.request_for(id, |id| Request::WindowHere { id }, ui::Asked::WindowHere);
            }
        }
        Action::Screenshot => {
            if let Some(id) = scene.monitor_in_front() {
                scene.request_for(id, |id| Request::Screenshot { id }, ui::Asked::Screenshot);
            }
        }
        Action::CloseWindow(id) => {
            scene.request_for(
                id,
                |id| Request::RemoveDisplay { id },
                ui::Asked::Remove(id),
            );
        }
        Action::Tidy => scene.tidy_up(),
        Action::SelectMonitor(id) => {
            scene.selected = Some(id);
            scene.confirm_remove = None;
        }
        Action::FindMouse => {
            scene.request(Request::FindMouse, ui::Asked::FindMouse);
        }
        Action::GazeMouse(on) => {
            scene.settings.gaze_mouse = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::LargeMenu(on) => {
            scene.settings.large_menu = on;
            scene.saves.mark(persist::File::Settings);
            scene.world.menu_size(on);
        }
        Action::RestEvery(minutes) => {
            scene.settings.rest_every = minutes;
            scene.saves.mark(persist::File::Settings);
            scene.worn_for = Duration::ZERO;
        }
        Action::Brightness(percent) => {
            scene.settings.brightness = percent;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Clock12h(on) => {
            scene.settings.clock_12h = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::DateStyle(style) => {
            scene.settings.date_style = style;
            scene.saves.mark(persist::File::Settings);
        }
        Action::MenuButton(on) => {
            scene.settings.menu_button = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::SoundFromScreen(on) => {
            scene.settings.sound_from_screen = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
            if !on {
                scene.sound_from = None;
            }
        }
        Action::Notifications(on) => {
            scene.settings.notifications = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::BatterySaver(on) => {
            scene.settings.battery_saver = on;
            scene.saves.mark(persist::File::Settings);
            scene.refresh_due = true;
        }
        Action::LeftHanded(on) => {
            scene.settings.left_handed = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::OpenMenu => {
            let (head, forward) = scene.head;
            let touch = scene.hands_in_view;
            scene.open_panel(head, forward, touch);
        }
        Action::MoveAll(on) => {
            scene.settings.move_all = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Magnet(on) => {
            scene.settings.magnet = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Follow(on) => {
            scene.settings.follow = on;
            scene.saves.mark(persist::File::Settings);
            scene.world.follow_chosen(&scene.settings);
        }
        Action::Hands(mode) => {
            scene.settings.hands = mode;
            scene.saves.mark(persist::File::Settings);
            controls.hands_enabled = mode == HandMode::Control;
        }
        Action::MakePrimary(id) => {
            scene.request_for(
                id,
                |id| Request::SetPrimary { id },
                ui::Asked::MakePrimary(id),
            );
        }
        // Swaps the places of a monitor and its neighbour in the order; the others
        // stay.
        Action::Move { id, delta } => scene.world.swap(id, delta),
        Action::SetMic(enabled) => {
            scene.mic_wanted = enabled;
            scene.settings.mic = enabled;
            scene.saves.mark(persist::File::Settings);
            scene.mic.set_allowed(enabled);
            scene.mic_changed();
        }
        Action::SetAudio(enabled) => {
            scene.audio_wanted = enabled;
            scene.request(
                Request::SetSound {
                    to_headset: enabled,
                },
                ui::Asked::Sound,
            );
        }
        Action::SetMirror(enabled) => {
            if let Some(id) = scene.physical_id() {
                scene.mirror_wanted = enabled;
                scene.request_for(
                    id,
                    |id| Request::ShowPhysical { id, shown: enabled },
                    ui::Asked::Mirror,
                );
            }
        }
        Action::Preset(preset) => {
            scene.settings.preset = preset;
            scene.saves.mark(persist::File::Settings);
            scene.world.preset_chosen(&scene.settings, Instant::now());
        }
        Action::Curve(curve) => {
            scene.settings.curve = curve;
            scene.saves.mark(persist::File::Settings);
            // Flat it hangs tilted, curved it continues the cylinder.
            scene.world.curve_chosen(&scene.settings);
        }
        Action::Background(background) => {
            scene.settings.background = background;
            scene.saves.mark(persist::File::Settings);
            set_passthrough(scene, passthrough);
        }
        Action::ApplyProfile(i) => {
            // An empty slot has nothing to switch to.
            let Some(p) = scene.profiles.get(i).cloned().flatten() else {
                return;
            };
            // The PC's own displays the profile showed that this PC has.
            let shown_physical: Vec<DisplayId> = p
                .shown
                .iter()
                .filter(|k| scene.display(**k).is_some_and(state::Display::is_physical))
                .map(|k| k.id)
                .collect();
            let req = scene.request(
                Request::SetDisplays {
                    modes: p.monitors.clone(),
                    primary: Some(Primary::Index { index: p.primary }),
                    shown_physical,
                },
                ui::Asked::Profile(i),
            );
            // Placed once the PC answers with its monitors' numbers. Cinema, the
            // background, the curve and the preset are not the profile's: they stay.
            scene.world.switch_to(req.map(|req| ProfileSwitch {
                frame: p.frame,
                rows: p.rows,
                ..ProfileSwitch::new(req, p.places)
            }));
            // The monitors changing is the answer: the menu gets out of the way.
            scene.panel_open = false;
        }
        Action::SaveProfile(i) => {
            // Nothing to save without monitors; a profile there is replaced on the
            // second press.
            if scene.world.monitors().is_empty() {
                return;
            }
            let replaces = scene.profiles.get(i).is_some_and(Option::is_some);
            let confirmed = !replaces
                || scene
                    .confirm_save
                    .is_some_and(|(c, at)| c == i && at.elapsed() < CONFIRM_FOR);
            if confirmed {
                scene.confirm_save = None;
                let profile = scene.current_profile();
                if let Some(slot) = scene.profiles.get_mut(i) {
                    *slot = Some(profile);
                    scene.saves.mark(persist::File::Profiles);
                    scene.active_profile = Some(i);
                    let name = scene.profile_name(i);
                    let text = scene
                        .texts()
                        .f("toast-profile-saved", &[("name", name.into())]);
                    scene.show_toast(text);
                }
            } else {
                scene.confirm_save = Some((i, Instant::now()));
            }
        }
        Action::AskMic => scene.mic.ask_again(),
        Action::ForgetPc(i) => {
            let confirmed = scene
                .confirm_forget
                .is_some_and(|(c, at)| c == i && at.elapsed() < CONFIRM_FOR);
            if !confirmed {
                scene.confirm_forget = Some((i, Instant::now()));
            } else if let Some((key, name)) = scene.paired.get(i).cloned() {
                scene.confirm_forget = None;
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the trusted PCs are read here until step 7.6 gives them to the network thread"
                )]
                let mut store = prostir_proto::secure::TrustStore::load(&scene.trust_file);
                match store.remove(&key) {
                    Ok(()) => {
                        info!("forgot PC {name:?}");
                        let pc = PcId::of(&key);
                        scene.forget_pc(pc);
                        scene.steering.trust_changed();
                        // Connected to it (by its key, not its name: two PCs may share
                        // one): leave, and do not come back on our own.
                        if scene.world.pc() == Some(pc) && scene.feedback.is_some() {
                            scene.steering.forget_current();
                        }
                    }
                    Err(e) => scene.show_error(format!("{e}")),
                }
                scene.paired = store.entries().to_vec();
            }
        }
        Action::SetEncoding(change) => {
            // Shown at once; the PC confirms (or corrects) with its `Encoding`.
            if let Some(pc) = &mut scene.pc.encoding {
                let w = &mut pc.wanted;
                w.bitrate = change.bitrate.unwrap_or(w.bitrate);
                w.codec = change.codec.or(w.codec);
                w.software = change.software.unwrap_or(w.software);
                w.sharp = change.sharp.unwrap_or(w.sharp);
            }
            scene.request(
                Request::SetEncoding {
                    bitrate: change.bitrate,
                    codec: change.codec,
                    software: change.software,
                    sharp: change.sharp,
                },
                ui::Asked::Encoding,
            );
        }
        Action::ShowTips => {
            scene.settings.intro_seen = false;
            scene.saves.mark(persist::File::Settings);
        }
        Action::ConnectTo(i) => {
            if let Some(h) = scene.found.get(i) {
                scene.steering.connect_to(h.addr);
            }
        }
        Action::ShowKeypad => scene.ip_input = Some(String::new()),
        Action::Key(c) if scene.pairing.is_some() => {
            if let Some(p) = &mut scene.pairing
                && p.typed.len() < 6
                && c.is_ascii_digit()
            {
                p.typed.push(c);
            }
        }
        Action::Backspace if scene.pairing.is_some() => {
            if let Some(p) = &mut scene.pairing {
                p.typed.pop();
            }
        }
        Action::ConnectTyped if scene.pairing.is_some() => {
            if let Some(p) = &mut scene.pairing {
                scene.steering.submit_code(std::mem::take(&mut p.typed));
                p.failed = false;
            }
        }
        Action::PairAnswer(pair) => {
            scene.steering.answer_offer(pair);
            scene.pair_offer = None;
        }
        Action::PairOtherWay => {
            scene.steering.switch_pairing_side();
            scene.pairing = None;
        }
        Action::Key(c) => {
            if let Some(t) = &mut scene.ip_input
                && t.len() < 21
            {
                t.push(c);
            }
        }
        Action::Backspace => {
            if let Some(t) = &mut scene.ip_input {
                t.pop();
            }
        }
        Action::ConnectTyped => {
            let typed = scene.ip_input.clone().unwrap_or_default();
            match net::parse_addr(&typed) {
                Some(addr) => {
                    scene.steering.connect_to(addr);
                    scene.ip_input = None;
                }
                None => {
                    let text = scene.texts().f("error-not-ip", &[("typed", typed.into())]);
                    scene.show_error(text);
                }
            }
        }
        Action::CancelKeypad => scene.ip_input = None,
        Action::ChangeHost => scene.steering.change_host(),
        Action::Close => {
            scene.panel_open = false;
            // Closing the tips counts as read, or they would open again right away.
            if !scene.settings.intro_seen {
                scene.settings.intro_seen = true;
                scene.saves.mark(persist::File::Settings);
            }
        }
    }
    scene.world_done();
}

/// Builds without the selftest (the store's): the same calls, for a selftest that
/// never exists, so they do nothing and cost nothing.
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
mod no_selftest {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use android_activity::AndroidApp;
    use openxr as xr;
    use prostir_proto::state::HeadsetCaps;

    use crate::net::NetEvent;

    pub fn count_run() -> u32 {
        0
    }

    #[expect(
        dead_code,
        reason = "made only by a selftest, which this build has not"
    )]
    pub enum Act {
        Nothing,
        MoveMonitor,
        OpenMenu,
        SwitchTab,
        CloseMenu,
        Finish,
    }

    pub struct FrameStart;

    /// No value of it can be made.
    pub enum Selftest {}

    impl Selftest {
        pub fn requested(_app: &AndroidApp, _run: u32) -> Option<Self> {
            None
        }

        pub fn finish(self, _: Option<&Path>, _: Option<&anyhow::Error>) -> (bool, i32) {
            match self {}
        }

        pub fn runtime(
            &self,
            _: &AndroidApp,
            _: &xr::Instance,
            _: xr::SystemId,
            _: &xr::ExtensionSet,
        ) {
            match *self {}
        }

        pub fn headset_caps(&self, _: &HeadsetCaps) {
            match *self {}
        }

        pub fn data_dir(&self, _: &Path) -> PathBuf {
            match *self {}
        }

        pub fn start_pc(&self, _: &AndroidApp, _: &Path) -> Option<std::net::SocketAddr> {
            match *self {}
        }

        pub fn over(&self) -> bool {
            match *self {}
        }

        pub fn session_state(&self, _: xr::SessionState) {
            match *self {}
        }

        pub fn refresh(&self, _: Option<Vec<f32>>, _: Option<f32>) {
            match *self {}
        }

        pub fn net_event(&self, _: &NetEvent) {
            match *self {}
        }

        pub fn loop_top(&self) {
            match *self {}
        }

        pub fn step(&self) -> Act {
            match *self {}
        }

        pub fn frame_start(&self) -> FrameStart {
            match *self {}
        }

        pub fn frame_end(
            &self,
            _: &FrameStart,
            _: i64,
            _: Duration,
            _: Option<Duration>,
            _: usize,
        ) {
            match *self {}
        }

        pub fn saved(&self, _: Duration) {
            match *self {}
        }
    }
}
