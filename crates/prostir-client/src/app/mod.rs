//! The headset's model (step 7, 4.1 and 4.2): everything it keeps and the rules between
//! its parts. What changes it comes in as events ([`Model::handle`]): what the network
//! thread and the PC say, the menu's actions, the system. The hands act on it as the
//! [`interaction::Scene`] they work with (see `hands`). The time goes on in
//! [`Model::tick`] (every pass of the loop, with or without frames) and
//! [`Model::animate`] (the end of a frame). What it wants done is [`Effect`]s, which the
//! shell does in the order they came ([`Model::take_effects`]); the files it changed
//! are said once a pass ([`Model::end_pass`]).
//!
//! It holds no handle of the shell (no swapchain, no thread, no outbox, no microphone):
//! only the facts ("a stream of display 3 shows, 2560×1440"). It reads no file and no
//! clock: the time (`now`, taken once a pass by the shell) and the wall clock come in
//! as parameters, so it builds and is tested on any system.

#![forbid(unsafe_code)]

mod actions;
mod auto;
mod gaze;
mod hands;
mod session;
#[cfg(test)]
mod tests;
mod volume;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Vec2, Vec3};
use log::{debug, info, warn};
use prostir_proto::request::{Outcome, Request};
use prostir_proto::state::{
    self, Battery, DisplayId, ErrorCode, Gen, HeadsetState, PcState, Prefs, Presence, ReqId, caps,
};
use prostir_proto::{ClientMsg, Codec};

use crate::caps::{Caps, Offer};
use crate::ids::{DisplayKey, PcId};
use crate::interaction::{FrameInput, Head};
use crate::lang::{Known, Lang, LangState, Moment, Texts};
use crate::layout::{self, Desk, Placement};
use crate::net::NetEvent;
use crate::note::{self, Note, Notes};
use crate::pending;
use crate::persist::{self, PersistEvent};
use crate::platform::store::UpdateHint;
use crate::presence;
use crate::profiles::{self, Profile};
use crate::session::{Attempt, NetCmd, Session};
use crate::settings::Settings;
use crate::ui::{self, Action, Tab};
use crate::video::{self, Happened, VideoEvent, Videos};
use crate::world::World;

pub use actions::By;
use actions::Forgetting;

/// How long an error stays in the menu.
pub const ERROR_SHOWN_FOR: Duration = Duration::from_secs(8);
/// How long a press that asks for a second one (remove, replace, forget) waits for it.
pub const CONFIRM_FOR: Duration = Duration::from_secs(4);
/// How long a toast stays.
pub const TOAST_FOR: Duration = Duration::from_secs(4);
/// How long the "where is the mouse" rings show.
pub const FINDER_FOR: Duration = Duration::from_millis(1600);
/// How long the gaze rests on another monitor before the mouse goes there.
pub const GAZE_DWELL: Duration = Duration::from_millis(600);
/// After the gaze sent the mouse somewhere, how long before it may again.
const GAZE_AGAIN: Duration = Duration::from_millis(1500);
/// Without a PC this long, the menu opens on its own (the connection screen).
const OFFLINE_OPENS_MENU: Duration = Duration::from_secs(4);
/// Taken off this long counts as a rest.
const REST_OFF: Duration = Duration::from_secs(5 * 60);
/// The store says the app is not the person's: how long the message shows before the
/// app closes.
const NOT_OWNED_FOR: Duration = Duration::from_secs(6);
/// Every monitor shown this long, the next one hidden for want of layers is said
/// again.
const HIDDEN_SAID_AGAIN: Duration = Duration::from_secs(60);

/// What the model starts with: what the files held, what the system said, what the
/// headset can.
pub struct Start {
    pub now: Instant,
    pub settings: Settings,
    pub desk: Desk,
    pub keyboard: Option<Placement>,
    pub profiles: profiles::Slots,
    /// The menu's language, loaded before the first frame, and the languages there are.
    pub lang: LangState,
    /// The headset's own system language.
    pub system_lang: Option<Lang>,
    pub battery: Option<(u8, bool)>,
    /// The system's volume now, percent and muted (`None`: it could not be read).
    pub volume: Option<(u8, bool)>,
    /// What the headset can do (its model among it), as the shell found it.
    pub caps: Caps,
    /// The display rate now, as the runtime says (`None`: it does not).
    pub refresh_hz: Option<f32>,
    /// The person's own picture for the background is on the headset.
    pub custom_panorama: bool,
    /// Where this build is updated from (its store's).
    pub update_from: UpdateHint,
    /// Whether the system lets the app record (see `prostir_proto::state::permission`).
    pub mic_permission: u8,
    /// The selftest's fake PC: the only address ever dialed.
    pub only: Option<SocketAddr>,
}

/// What changed the model: one at a time, each with the time it is handled at.
pub enum Event {
    /// What the network thread said. `Connected` and `StreamStart` come as the two
    /// events below: the shell keeps their handles (the outbox, the frames).
    Net(NetEvent),
    /// The session with PC `pc` is open (see [`NetEvent::Connected`]).
    Connected {
        pc: PcId,
        addr: SocketAddr,
        language: Option<Lang>,
        opened_with: HeadsetState,
    },
    /// Generation `generation` of display `id`'s stream started, `size` pixels in
    /// `codec` (its surface and decoder being made; whether they could be is a
    /// [`Event::Video`] after).
    StreamStarted {
        id: DisplayKey,
        generation: Gen,
        codec: Codec,
        size: (u32, u32),
    },
    /// What became of a stream's video: trouble, a picture again, or a lasting failure
    /// (see `video`).
    Video(VideoEvent),
    /// A message to PC `pc` was not sent: no session with it (`None`), or why not.
    SendFailed { pc: PcId, why: Option<String> },
    /// A menu's action, pressed `by` (or the same done for a command or the selftest).
    Menu(Action, By),
    /// What the headset and its system said.
    Platform(PlatformEvent),
    /// What the files' writer said.
    Persist(PersistEvent),
}

/// What the headset and its system said (step 7, 5).
#[derive(Debug)]
pub enum PlatformEvent {
    /// The OpenXR session's state changed: shown, hidden or stopped, as presence sees
    /// it. Like the next two, it is kept; the PC is told once the shell says
    /// ([`Model::presence_due`], after it looked at the screen where presence wants
    /// it).
    Session(presence::Session),
    /// The activity was paused (`true`) or resumed.
    Paused(bool),
    /// The headset's sensor says the user is there, or not.
    UserPresent(bool),
    /// The screen is on for the user (read only while presence wants it): the PC is
    /// told what changed.
    Interactive(bool),
    /// The microphone asked the system for access: its prompt's pause is the app's own.
    MicAsked,
    /// Whether the system lets the app record, and whether access was refused, as last
    /// found.
    Mic { permission: u8, denied: bool },
    /// The battery was read.
    Battery(Option<(u8, bool)>),
    /// The system's volume was read (now and then while a session is open, and right
    /// after each set): `level` percent, muted or not, after the `done`th
    /// [`SoundCmd::SetVolume`].
    Volume { level: u8, muted: bool, done: u32 },
    /// The system moved the room's origin to where the person is now (its "recenter"):
    /// what stands in the room moves with it, as it always did.
    SpaceMoved,
    /// The display rate is now this, changed by the system or as asked.
    RefreshRate(f32),
    /// The cameras broke for good (the runtime said so): the "Room" backgrounds and the
    /// window onto the keyboard go for this start.
    CamerasLost,
    /// A language loaded, ready for the menu.
    Texts(Texts),
    /// The languages there are files for, listed again.
    Languages(Vec<Known>),
    /// The store says this app is not the person's: `store` names it.
    NotOwned { store: &'static str },
    /// The cameras could not be switched as the background asks.
    CamerasFailed(String),
    /// The window onto the keyboard could not be shown.
    KeyboardWindowFailed,
    /// Whether the person's own background picture is on the headset (looked for when
    /// the menu opened).
    CustomPanorama(bool),
    /// This many monitors do not fit in the layers the compositor takes (see
    /// `layers::plan`): said when the number changes.
    LayersHidden(usize),
}

/// What the model asks the shell to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// To the outbox of PC `pc`'s session (it only queues); the menu says when it
    /// cannot be sent.
    Send(PcId, ClientMsg),
    /// The same, about the headset itself (presence, battery, the microphone, the
    /// settings the PC acts on): quiet, a message that cannot be sent is let go.
    Tell(PcId, ClientMsg),
    /// To the network thread.
    Net(NetCmd),
    /// The text of this file changed in the pass (see [`Model::write_file`]): once a
    /// file a pass.
    Save(persist::File),
    /// A short vibration of this hand's controller.
    Haptic(usize),
    Xr(XrCmd),
    Sound(SoundCmd),
    Mic(MicCmd),
    Video(VideoCmd),
    System(SystemCmd),
    /// The app closes (the store said it is not the person's).
    Exit,
}

/// For OpenXR and the runtime's extensions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum XrCmd {
    /// The display rate to ask for (one of `Caps::refresh_rates`, by the rule of
    /// `Caps::rate`).
    RefreshRate(f32),
    /// The cameras: on or off, and how opaque.
    Room { on: bool, opacity: f32 },
}

/// For the PC's sound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoundCmd {
    /// Where the sound comes from, as an angle to the right of where the user looks
    /// (`None`: from straight ahead).
    Place(Option<f32>),
    /// Put the system's volume at `level` percent (muted or not), without the system's
    /// own display of it; the `n`th set (its reading says so, see
    /// [`PlatformEvent::Volume`]).
    SetVolume { level: u8, muted: bool, n: u32 },
}

/// For the microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicCmd {
    /// The user's switch.
    Allowed(bool),
    /// Show the system's prompt again.
    AskAgain,
    /// Look whether the system lets the app record now.
    RefreshPermission,
}

/// For the streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCmd {
    /// The display is gone from its PC's list: its stream, surface and decoder go.
    Drop(DisplayKey),
}

/// For the system thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemCmd {
    /// Load this language's files.
    LoadLanguage(Lang),
    /// The palm menu (which shows the battery) appeared or went.
    PalmMenu(bool),
    /// The menu opened: the person may have put a language file or a background
    /// picture on the headset meanwhile.
    LookForFiles,
    /// Follow the system's volume, of `steps` above silence, read this often (`None`:
    /// only after a set or a volume button), and at once when this starts.
    WatchVolume { steps: u8, every: Option<Duration> },
}

/// A short message shown on its own, below the line of sight.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub since: Instant,
    /// Where it stands (`None` until a frame placed it).
    pub place: Option<Placement>,
    /// Each toast its own number: a new one is drawn again.
    pub n: u64,
}

/// What the menu itself keeps (read by the menu's view, see `view`).
#[derive(Debug)]
pub struct Menu {
    pub open: bool,
    /// The page shown.
    pub tab: Tab,
    /// The monitor picked on the menu's map.
    pub selected: Option<DisplayKey>,
    /// The address typed on the menu's keypad.
    pub ip_input: Option<String>,
    pub error: Option<(String, Instant)>,
    /// "×" pressed once on this monitor; a second press within a few seconds removes it.
    pub confirm_remove: Option<(DisplayKey, Instant)>,
    /// "Save here" pressed once for this profile.
    pub confirm_save: Option<(usize, Instant)>,
    /// "×" pressed once for this profile.
    pub confirm_delete: Option<(usize, Instant)>,
    pub confirm_forget: Option<(usize, Instant)>,
    /// Which profiles the Profiles tab shows, when there are many.
    pub profile_page: u8,
    /// Which displays of the PC the Monitors tab shows, when there are more than fit.
    pub physical_page: u8,
    /// The scale a monitor added gets, percent.
    pub add_scale: u16,
    /// A monitor added is in Auto (where the PC changes scales in place).
    pub add_auto: bool,
    /// A monitor added at a scale chosen by hand, PC and request: taken out of Auto
    /// once made.
    pub add_fixed: Option<(PcId, ReqId)>,
    /// Displays of the PC itself asked to be shown or hidden, the last ask of each
    /// first: their switches show it until the PC answers.
    pub physical_asked: Vec<(DisplayKey, bool)>,
    /// The encoding and the PC's sound shown at once as asked: as they were before the
    /// last request about them, with its PC and number, put back if it fails.
    pub encoding_before: Option<(PcId, ReqId, state::Wanted)>,
    pub sound_before: Option<(PcId, ReqId, bool)>,
    /// What the open menu says under the button pressed (see `note`).
    pub notes: Notes,
    /// Which languages the Language page shows, when there are many.
    pub language_page: u8,
    /// The profile last switched to, until something is changed by hand.
    pub active_profile: Option<usize>,
    pub toast: Option<Toast>,
}

/// The headset itself, as the system says.
#[derive(Debug)]
pub struct Headset {
    /// Percent and whether it is charging, read once a minute.
    pub battery: Option<(u8, bool)>,
    /// What it can do, as the shell found it at the start (the cameras may go later).
    pub caps: Caps,
    /// The display rate now, as the runtime says.
    pub refresh_hz: Option<f32>,
    /// The person put a picture of their own for the background on the headset (looked
    /// for again each time the menu opens).
    pub custom_panorama: bool,
    /// Where this build is updated from (its store's).
    pub update_from: UpdateHint,
    /// Whether the system lets the app record, as last found.
    pub mic_permission: u8,
    /// Access to the microphone was refused in the system.
    pub mic_denied: bool,
    /// The microphone's switch, as the menu shows it.
    pub mic_wanted: bool,
}

/// The time and the date on the palm menu, written again only when the minute, the
/// language or how they are written changes (not every frame).
#[derive(Debug, Default)]
pub struct ClockFace {
    /// What the texts were written for.
    shown: Option<(Moment, bool, crate::settings::DateStyle, Texts)>,
    /// Shared with the menu's state.
    pub clock: Arc<str>,
    pub date: Arc<str>,
}

impl ClockFace {
    /// For `now` (none: no clock); whether the texts changed.
    fn update(
        &mut self,
        now: Option<Moment>,
        twelve: bool,
        style: crate::settings::DateStyle,
        texts: &Texts,
    ) -> bool {
        let Some(now) = now else {
            if self.shown.take().is_some() {
                self.clock = Arc::default();
                self.date = Arc::default();
                return true;
            }
            return false;
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
        !same
    }
}

/// The monitors that do not fit in the compositor's layers (see `layers`).
#[derive(Debug, Default)]
struct Hidden {
    /// How many now.
    now: usize,
    /// How many the person was told of.
    told: usize,
    /// Since when every monitor shows again.
    all_shown_since: Option<Instant>,
}

/// The store said the app is not the person's.
#[derive(Debug)]
struct NotOwned {
    /// Since when its message shows.
    since: Instant,
    /// What the person is told, and the language it is in.
    text: Option<(Texts, String)>,
}

/// The headset's model (see the module).
pub struct Model {
    /// The time of what is being handled now.
    now: Instant,
    /// Changes with everything the menu may show (see [`Model::rev`]).
    rev: u64,
    effects: Vec<Effect>,

    /// Where everything stands in the room, and the rules it is arranged by.
    world: World,
    /// The generation of each display's stream, and how its picture is.
    videos: Videos,
    /// The connections to PCs: the search, and each PC's link with its state and the
    /// menu's requests to it.
    session: Session,
    settings: Settings,
    profiles: profiles::Slots,
    /// Whether the headset is on the head, from the session, the activity and the
    /// headset's sensor; streaming pauses otherwise.
    presence: presence::Presence,
    menu: Menu,
    headset: Headset,
    /// PCs with a session open now (whose outbox the shell has).
    sessions: BTreeSet<PcId>,
    /// The microphone permission the PC was last told.
    mic_told: u8,
    /// The profiles or the one in use changed: the PCs hear at the end of the pass.
    profiles_changed: bool,
    /// The palm menu showed when last looked at.
    palm_shown: bool,
    /// The volume shared with the PC (`None`: the app cannot set the system's).
    volume: Option<volume::SharedVolume>,
    /// "Auto" of the monitors' scales (see `autoscale`), and its list, filled again
    /// at each look.
    auto: crate::autoscale::Auto,
    auto_list: Vec<crate::autoscale::Monitor>,
    auto_asks: Vec<(DisplayKey, u16)>,

    // The frame.
    /// Head position and gaze.
    head: (Vec3, Vec3),
    /// The head's right, rolled and tilted with it (where the right ear is).
    head_right: Vec3,
    /// Where the eyes are, for the map in the menu: moves only when the head has moved
    /// a few centimetres (the menu redraws when its picture changes).
    view_eye: Option<Vec3>,
    /// Tracked hands are in view (the menu then opens within reach).
    hands_in_view: bool,
    /// Where the window onto the keyboard is this frame (`None`: not shown).
    keyboard_window: Option<Placement>,
    /// Where each monitor stands for the menu (`World::place_of`), as last looked at
    /// in a drag: kept to be filled again.
    map_places: Vec<(DisplayKey, ui::Place)>,

    // The menu's helpers.
    /// The press being done is the main menu's (not the palm menu's, nor a command of
    /// the PC): what it comes to is said in the menu.
    press_in_menu: bool,
    /// The user chose a language in the menu: said once it shows.
    language_asked: bool,
    /// A PC being forgotten, until the network thread says it is (or why not).
    forgetting: Option<Forgetting>,
    /// Each toast's number.
    toasts: u64,
    /// "Where is the mouse": monitor, point across its image, since when.
    finder: Option<(DisplayKey, Vec2, Instant)>,
    /// Mouse follows the gaze: the monitor looked at and since when.
    gaze_on: Option<(DisplayKey, Instant)>,
    /// When the mouse was last sent somewhere by the gaze.
    gaze_moved: Option<Instant>,
    /// What the PCs were told of the monitor looked at (see `gaze`).
    gaze_told: gaze::Told,
    /// Time worn since the last rest reminder.
    worn_for: Duration,
    /// Since when the headset is off the head (for rest reminders).
    off_since: Option<Instant>,

    // The texts.
    system_lang: Option<Lang>,
    /// The menu's language: what it shows, and the one it waits for.
    lang: LangState,
    clock: ClockFace,

    // The files.
    /// The files changed in this pass, said once after it (see [`Model::end_pass`]).
    saves: persist::Dirty,
    /// The files whose failed save the menu told of, once each.
    save_failures_told: persist::Dirty,
    /// Failed saves come in this pass, and the first of them, said together.
    save_failures: persist::Dirty,
    first_failure: Option<(persist::File, String)>,

    not_owned: Option<NotOwned>,
    /// The monitors that do not fit in the compositor's layers.
    hidden: Hidden,
    /// The monitor the head looked at in the last frame, while the numbers of "For
    /// development" are shown (see [`Model::metrics_place`]); not in the menu.
    #[cfg(feature = "preview")]
    looked_monitor: Option<DisplayKey>,
}

impl Model {
    /// The model at the start.
    pub fn new(start: Start) -> Self {
        let Start {
            now,
            settings,
            desk,
            keyboard,
            profiles,
            lang,
            system_lang,
            battery,
            volume,
            mut caps,
            refresh_hz,
            custom_panorama,
            update_from,
            mic_permission,
            only,
        } = start;
        // A volume that could not be read is not shared: the PC is not told of it.
        let volume = match (
            caps.volume_steps
                .filter(|_| caps.offers(Offer::SharedVolume)),
            volume,
        ) {
            (Some(steps), Some(now)) => Some(volume::SharedVolume::new(steps, now)),
            _ => {
                caps.volume_steps = None;
                None
            }
        };
        let ppd = crate::clarity::pixels_per_degree(&caps.model);
        let world = World::new(desk, keyboard, caps.cylinder, ppd, &settings);
        let mut m = Self {
            now,
            rev: 0,
            effects: Vec::new(),
            world,
            videos: Videos::default(),
            session: Session::new(crate::session::MAX_PCS, only, now),
            profiles,
            presence: presence::Presence::new(),
            menu: Menu {
                open: false,
                tab: tab_from_code(settings.last_tab),
                selected: None,
                ip_input: None,
                error: None,
                confirm_remove: None,
                confirm_save: None,
                confirm_delete: None,
                confirm_forget: None,
                profile_page: 0,
                physical_page: 0,
                add_scale: 100,
                add_auto: true,
                add_fixed: None,
                physical_asked: Vec::new(),
                encoding_before: None,
                sound_before: None,
                notes: Notes::default(),
                language_page: 0,
                active_profile: None,
                toast: None,
            },
            headset: Headset {
                battery,
                caps,
                refresh_hz,
                custom_panorama,
                update_from,
                mic_permission,
                mic_denied: false,
                mic_wanted: settings.mic,
            },
            settings,
            sessions: BTreeSet::new(),
            mic_told: mic_permission,
            profiles_changed: false,
            palm_shown: false,
            volume,
            auto: crate::autoscale::Auto::default(),
            auto_list: Vec::new(),
            auto_asks: Vec::new(),
            head: (Vec3::ZERO, -Vec3::Z),
            head_right: Vec3::X,
            view_eye: None,
            hands_in_view: false,
            keyboard_window: None,
            map_places: Vec::new(),
            press_in_menu: false,
            language_asked: false,
            forgetting: None,
            toasts: 0,
            finder: None,
            gaze_on: None,
            gaze_moved: None,
            gaze_told: gaze::Told::default(),
            worn_for: Duration::ZERO,
            off_since: None,
            system_lang,
            lang,
            clock: ClockFace::default(),
            saves: persist::Dirty::default(),
            save_failures_told: persist::Dirty::default(),
            save_failures: persist::Dirty::default(),
            first_failure: None,
            not_owned: None,
            hidden: Hidden::default(),
            #[cfg(feature = "preview")]
            looked_monitor: None,
        };
        // The language's file had lines that could not be read: said once, in front.
        if let Some(warning) = m.texts().warning() {
            let text = warning.to_owned();
            m.show_toast(text);
        }
        m
    }

    // ---- What the shell reads ----

    /// Changes whenever something the menu shows may have changed: the menu's state is
    /// made again only then (see `view`).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// The time of the last thing handled.
    pub fn now(&self) -> Instant {
        self.now
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// The generation of each display's stream, and how its picture is.
    pub fn videos(&self) -> &Videos {
        &self.videos
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn profiles(&self) -> &profiles::Slots {
        &self.profiles
    }

    pub fn menu(&self) -> &Menu {
        &self.menu
    }

    pub fn headset(&self) -> &Headset {
        &self.headset
    }

    /// The menu's texts, in the language it shows now.
    pub fn texts(&self) -> &Texts {
        self.lang.texts()
    }

    pub fn lang(&self) -> &LangState {
        &self.lang
    }

    pub fn clock(&self) -> &ClockFace {
        &self.clock
    }

    /// Where the eyes are for the menu's map.
    pub fn view_eye(&self) -> Option<Vec3> {
        self.view_eye
    }

    /// Display `id` of the PC the menu acts on (see `Session::main`).
    pub fn key(&self, id: DisplayId) -> Option<DisplayKey> {
        self.session.main().map(|pc| DisplayKey::new(pc, id))
    }

    /// The main PC's state (see `Session::state`).
    pub fn pc(&self) -> &PcState {
        self.session.state()
    }

    /// Display `id` as its PC's list says (`None`: not listed, or no session with its
    /// PC).
    pub fn display(&self, id: DisplayKey) -> Option<&state::Display> {
        self.session.pc(id.pc)?.state.displays.as_ref()?.get(id.id)
    }

    /// The PC can move the mouse (for following the gaze).
    pub fn can_move_mouse(&self) -> bool {
        self.pc()
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::MOVE_MOUSE))
    }

    /// The monitor the mouse is on, as the PC says.
    pub fn focus(&self) -> Option<DisplayKey> {
        self.key(self.pc().focus.as_ref()?.display?)
    }

    /// The PC makes monitors at another scale (its capability 10).
    pub fn can_scale(&self) -> bool {
        self.pc()
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::SCALE))
    }

    /// Profile `i` as the menu names it: by the name it was given, else by its number
    /// ("Profile 2").
    pub fn profile_name(&self, i: usize) -> String {
        match self.profiles.get(i).and_then(|p| p.name.as_ref()) {
            Some(name) => name.clone(),
            None => self
                .texts()
                .f("headset-profile-n", &[("n", (i + 1).into())]),
        }
    }

    /// What the user reads for display `id`: as the PC's desktop names it.
    pub fn label_of(&self, id: DisplayKey) -> Option<String> {
        let d = self.display(id)?;
        Some(ui::display_label(&d.name))
    }

    /// The toast shown now.
    pub fn toast(&self) -> Option<&Toast> {
        self.menu.toast.as_ref()
    }

    /// The numbers of "For development" are shown next to the monitor looked at: in
    /// the preview build, while chosen.
    pub fn metrics_shown(&self) -> bool {
        cfg!(feature = "preview") && self.world.dev().metrics
    }

    /// The monitor whose numbers of "For development" are shown (see
    /// [`Model::metrics_place`]).
    #[cfg(feature = "preview")]
    pub fn metrics_for(&self) -> Option<DisplayKey> {
        self.looked_monitor
            .filter(|_| self.metrics_shown())
            .filter(|id| self.world.shown(*id).is_some())
    }

    /// Where the numbers of "For development" stand: just under the monitor looked at,
    /// turned as it is, as large to the eye at any distance; where something stands
    /// there (a row under it, the PC's own display), just over it; where something
    /// stands there too, beside it on its right, its bottom edge level with the
    /// monitor's. `None` while not shown. Nothing is allocated.
    #[cfg(feature = "preview")]
    pub fn metrics_place(&self) -> Option<crate::layers::Quad> {
        let id = self.metrics_for()?;
        let p = *self.world.shown(id)?;
        let (half_w, half_h) = (p.width / 2.0, self.world.height_of(id) / 2.0);
        let d = p.position.distance(self.head.0).max(0.3);
        let width = 0.3 * d;
        let (w, h) = crate::layers::METRICS_PX;
        let height = width * h as f32 / w as f32;
        let gap = 0.01 * d;
        // In the monitor's own plane: x right, y up, from its middle.
        let below = Vec2::new(0.0, -(half_h + gap + height / 2.0));
        let above = Vec2::new(0.0, half_h + gap + height / 2.0);
        let beside = Vec2::new(half_w + gap + width / 2.0, -half_h + height / 2.0);
        let at = [below, above, beside]
            .into_iter()
            .find(|c| !self.world.stands_at(&p, *c, Vec2::new(width, height), id))
            .unwrap_or(below);
        let (right, up, out) = (
            p.rotation * Vec3::X,
            p.rotation * Vec3::Y,
            p.rotation * Vec3::Z,
        );
        Some(crate::layers::Quad {
            at: Placement {
                position: p.position + right * at.x + up * at.y + out * 0.005,
                rotation: p.rotation,
                width,
            },
            height,
        })
    }

    /// "Where is the mouse": on which monitor, where across its image, how far the
    /// rings went (0 to 1).
    pub fn finder(&self) -> Option<(DisplayKey, Vec2, f32)> {
        let (id, uv, at) = self.finder?;
        let age = self.now.saturating_duration_since(at);
        Some((id, uv, age.as_secs_f32() / FINDER_FOR.as_secs_f32()))
    }

    /// Where the window onto the keyboard is this frame (`None`: not shown).
    pub fn keyboard_window(&self) -> Option<Placement> {
        self.keyboard_window
    }

    /// The headset counts as on the head.
    pub fn worn(&self) -> bool {
        self.presence.worn()
    }

    /// Presence wants the screen read (see `Presence::wants_screen`).
    pub fn wants_screen(&self) -> bool {
        self.presence.wants_screen()
    }

    /// The magnet while dragging monitor `id`: the edge it would stick to (asked every
    /// frame of a drag; its memory is the room's).
    pub fn magnet(&mut self, id: DisplayKey) -> Option<layout::Stick<DisplayKey>> {
        self.world.stick_of(&self.settings, id)
    }

    /// The text of `file`, as the model has it now, written into `out`.
    pub fn write_file(&self, file: persist::File, out: &mut String) {
        use persist::File;
        match file {
            File::Settings => self.settings.write_text(out),
            File::Desk => self.world.desk().write_text(out),
            File::Profiles => profiles::write_text(out, &self.profiles),
            File::Keyboard => {
                if let Some(place) = &self.world.keyboard() {
                    layout::write_keyboard(out, place);
                }
            }
        }
    }

    /// The effects asked for since the last call, in their order, moved into `out`
    /// (emptied first; both keep their memory).
    pub fn take_effects(&mut self, out: &mut Vec<Effect>) {
        self.effects
            .extend(self.session.take_cmds().map(Effect::Net));
        out.clear();
        std::mem::swap(&mut self.effects, out);
    }

    /// The end of a pass of the loop: each file changed in it is to be saved, once; the
    /// PCs hear of the profiles if they changed.
    pub fn end_pass(&mut self) {
        self.saves.add(self.world.take_saves());
        for file in self.saves.take() {
            self.effects.push(Effect::Save(file));
        }
        self.watch_volume();
        let volume = self.volume_changed();
        if self.profiles_changed {
            self.tell_profiles();
        } else if volume {
            let state = self.headset_state();
            self.session.set_headset(state);
        }
    }

    /// The PCs hear of the profiles now, and the next session starts with them.
    fn tell_profiles(&mut self) {
        self.profiles_changed = false;
        let state = self.headset_state();
        self.session.set_headset(state);
        let part = self.profiles_part();
        self.tell_all(&ClientMsg::Profiles { part });
    }

    // ---- What changes it ----

    /// `event` came, at `now`.
    pub fn handle(&mut self, now: Instant, event: Event) {
        self.now = now;
        self.rev += 1;
        match event {
            Event::Net(event) => self.net_event(event),
            Event::Connected {
                pc,
                addr,
                language,
                opened_with,
            } => self.connected(pc, addr, language, &opened_with),
            Event::StreamStarted {
                id,
                generation,
                codec,
                size,
            } => {
                self.videos.started(id, generation, codec, size);
                let head = self.head;
                self.world
                    .stream_started(id, size, &self.settings, head, now);
            }
            Event::Video(e) => self.video(&e),
            Event::SendFailed { pc, why } => {
                let text = match why {
                    Some(e) => self.texts().f("headset-error-send", &[("error", e.into())]),
                    None => self.texts().t("headset-error-not-connected").to_owned(),
                };
                warn!("to PC {pc}: {text}");
                self.put_error(text);
            }
            Event::Menu(action, by) => self.act(action, by),
            Event::Platform(event) => self.platform(event),
            Event::Persist(PersistEvent::Failed { file, error }) => {
                if !self.save_failures_told.has(file) && !self.save_failures.has(file) {
                    self.save_failures.mark(file);
                    self.first_failure.get_or_insert((file, error));
                }
            }
        }
        self.world_done();
    }

    /// Once a pass of the loop, with or without frames (`input` says): the time goes on
    /// (requests time out, a placeholder goes, the desktop's arrangement is told once
    /// it stood, the search goes on), and with a frame the head is followed.
    pub fn tick(&mut self, now: Instant, input: &FrameInput) {
        self.presence_due(now);
        // The time alone changes what the connection screen shows: an answer gone
        // stale, the hint (both in `Search`).
        if self.session.tick(now) {
            self.rev += 1;
        }
        // A step of the search may change what the connection screen shows.
        let before = self.effects.len();
        self.effects
            .extend(self.session.take_cmds().map(Effect::Net));
        if self.effects.len() != before {
            self.rev += 1;
        }
        self.saves_told();
        // Another language chosen (or the PC's changed): loaded on the system thread,
        // the menu keeps the one it has until then.
        if let Some(lang) = self.wanted_lang() {
            self.effects
                .push(Effect::System(SystemCmd::LoadLanguage(lang)));
        }
        let s = &self.settings;
        if self
            .clock
            .update(input.wall, s.clock_12h, s.date_style, self.lang.texts())
        {
            self.rev += 1;
        }
        self.expire_asks();
        self.adding_due();
        self.world.settle(now);
        self.world_done();
        self.auto_scale();
        self.expire_shown();
        if let Some(head) = input.head {
            self.frame(input, head);
        } else {
            self.tell_gaze(None);
        }
    }

    /// The end of a frame, after the hands: what is drawn eases towards where it goes,
    /// the desktop's arrangement is told once nothing moves, and what is drawn is put
    /// in order (far to near, dimmed out of focus).
    pub fn animate(&mut self, now: Instant, input: &FrameInput) {
        self.now = now;
        let Some(Head { position, .. }) = input.head else {
            return;
        };
        // A new toast: placed where the user looks now.
        let (head, forward) = self.head;
        if let Some(Toast {
            place: place @ None,
            ..
        }) = &mut self.menu.toast
        {
            let at = head + forward.normalize_or(-Vec3::Z) * 0.7 - Vec3::Y * 0.18;
            *place = Some(Placement {
                position: at,
                rotation: layout::facing(at, head),
                width: TOAST_WIDTH,
            });
        }
        // The window onto the keyboard: in front of the user at desk height the first
        // time, then wherever it was moved.
        let keyboard = self.headset.caps.offers(Offer::KeyboardWindow);
        self.keyboard_window = (keyboard && self.settings.keyboard_window)
            .then(|| self.world.keyboard_here(self.head));
        // Far monitors first so nearer (and grabbed) ones draw on top.
        self.world.animate(&self.settings, input.dt);
        // Keep the desktop's arrangement in step once nothing is being moved.
        self.world.settle(now);
        self.world_done();
        let session = &self.session;
        // Gone from view as soon as asked to go; back if the PC does not remove it.
        // Far to near, two at about one distance (a monitor glued over a row and that
        // row) in the order they had, so where they overlap never flickers.
        self.world
            .order_drawing(position, |k| session.waits(ui::Asked::Remove(k)));
        // Focus mode: monitors without the mouse fade to a dim level (colour scale on
        // the compositor layer, no extra rendering).
        let focus = self.focus();
        self.world.brighten(&self.settings, input.dt, focus);
        // The rings of "where is the mouse" go once they are done, or with their
        // monitor.
        if self.finder.is_some_and(|(id, _, at)| {
            now.saturating_duration_since(at) >= FINDER_FOR || self.world.shown(id).is_none()
        }) {
            self.finder = None;
        }
    }

    /// The start of a frame, before the hands: the head, following it, where the
    /// sound comes from, the rest reminders, the mouse after the gaze, and the menu
    /// opened where it has to be.
    fn frame(&mut self, input: &FrameInput, head: Head) {
        let Head {
            position,
            forward,
            right,
        } = head;
        self.head = (position, forward);
        self.head_right = right;
        if self.view_eye.is_none_or(|e| e.distance(position) > 0.05) && position != Vec3::ZERO {
            self.view_eye = Some(position);
            self.rev += 1;
        }
        // While following, and on its last step (which keeps the arrangement).
        let was_following = self.world.following();
        self.world.follow_head(&self.settings, input.dt, self.head);
        if was_following || self.world.following() {
            self.rev += 1;
        }
        self.place_sound();
        self.remind_rest(input.dt);
        let looked = self.looked(input.focused);
        #[cfg(feature = "preview")]
        {
            self.looked_monitor = looked.map(|(k, _)| k);
        }
        self.tell_gaze(looked);
        if input.focused {
            self.follow_gaze(looked);
        }
        self.hands_in_view = input.joints.iter().any(Option::is_some);
        // Pairing needs the menu, and so do the first-run tips: open it where the
        // user is looking.
        let search = self.session.search();
        let first_run =
            (!self.settings.intro_seen && self.session.host().is_some()) || search.update.is_some();
        let asking = matches!(
            search.attempt,
            Some(Attempt::Pairing { .. } | Attempt::PairOffer { .. })
        );
        if (asking || first_run) && !self.menu.open {
            self.open_panel();
        }
        // Without a PC there is nothing to see: show the connection screen.
        if !self.menu.open && self.session.offline_for(self.now, OFFLINE_OPENS_MENU) {
            self.open_panel();
        }
        self.world.place_button(&self.settings);
    }

    // ---- Inside ----

    /// What the world asks for after it changed: the PC is told the rows of its
    /// desktop (see `World::settle`).
    fn world_done(&mut self) {
        // Changed by hand: no longer as the profile it came from has it.
        if self.world.take_rearranged() {
            self.leave_profile();
        }
        while let Some((pc, rows)) = self.world.take_arrange() {
            self.rev += 1;
            // Only to a PC in a session: the world sends nothing otherwise.
            if self.session.connected(pc) {
                self.send_to(pc, ClientMsg::Arrange { rows });
            }
        }
    }

    /// Whether a monitor of the menu's map stands elsewhere (left, right, above...) than
    /// when last looked at: in a drag that is all the menu shows of it, the map itself
    /// waits for the hand to let go (see `World::map_spots`). Nothing is allocated once
    /// the list is long enough.
    fn map_moved(&mut self) -> bool {
        let (world, eye) = (&self.world, self.view_eye);
        let monitors = world.monitors();
        let mut moved = self.map_places.len() != monitors.len();
        self.map_places.resize(
            monitors.len(),
            (
                DisplayKey::new(PcId([0; 8]), DisplayId(0)),
                ui::Place::Unknown,
            ),
        );
        for (kept, &(id, _, _)) in self.map_places.iter_mut().zip(monitors) {
            let now = (id, world.place_of(id, eye));
            moved |= *kept != now;
            *kept = now;
        }
        moved
    }

    /// Errors, questions and toasts shown long enough go.
    fn expire_shown(&mut self) {
        let now = self.now;
        let over = |at: Instant, d: Duration| now.saturating_duration_since(at) >= d;
        let m = &mut self.menu;
        let error = m.error.take_if(|(_, at)| over(*at, ERROR_SHOWN_FOR));
        let remove = m.confirm_remove.take_if(|(_, at)| over(*at, CONFIRM_FOR));
        let save = m.confirm_save.take_if(|(_, at)| over(*at, CONFIRM_FOR));
        let delete = m.confirm_delete.take_if(|(_, at)| over(*at, CONFIRM_FOR));
        let forget = m.confirm_forget.take_if(|(_, at)| over(*at, CONFIRM_FOR));
        m.toast
            .take_if(|t| now.saturating_duration_since(t.since) > TOAST_FOR);
        if error.is_some()
            || remove.is_some()
            || save.is_some()
            || delete.is_some()
            || forget.is_some()
        {
            self.rev += 1;
        }
    }

    /// The headset's presence as the PC is told, at `now`: once the signals say the
    /// headset was put on, taken off or put to sleep (asleep, the PC need not wait for
    /// it to come back), and the network thread connects only while it is worn.
    pub fn presence_due(&mut self, now: Instant) {
        self.now = now;
        let Some(state) = self.presence.update(now) else {
            return;
        };
        info!("headset {state:?}");
        self.rev += 1;
        // Off the head counts as a rest from when it was taken off, whether frames
        // are drawn meanwhile or not (hidden, none are).
        if self.presence.worn() {
            if self
                .off_since
                .take()
                .is_some_and(|t| now.saturating_duration_since(t) >= REST_OFF)
            {
                self.worn_for = Duration::ZERO;
            }
        } else {
            self.off_since.get_or_insert(now);
        }
        self.tell_presence();
        if state == presence::State::Asleep {
            self.tell_all(&ClientMsg::Asleep);
        }
        self.session.set_worn(self.presence.worn());
    }

    /// What the headset and its system said.
    fn platform(&mut self, event: PlatformEvent) {
        match event {
            PlatformEvent::Session(shown) => self.presence.session(shown),
            PlatformEvent::Paused(paused) => self.presence.paused(paused),
            PlatformEvent::UserPresent(present) => self.presence.user(self.now, present),
            PlatformEvent::Interactive(on) => {
                self.presence.interactive(on);
                self.presence_due(self.now);
            }
            PlatformEvent::MicAsked => self.presence.asked(self.now),
            PlatformEvent::Mic { permission, denied } => {
                self.headset.mic_denied = denied;
                self.headset.mic_permission = permission;
                // The system may let the app record now, or not any more.
                if permission != self.mic_told {
                    self.mic_changed();
                    if self.headset.mic_wanted
                        && let Some(told) = note::permission_note(self.mic_told, self.texts())
                    {
                        self.tell(told.note, told.toast, true);
                    }
                }
            }
            PlatformEvent::Battery(battery) => self.battery_read(battery),
            PlatformEvent::Volume { level, muted, done } => self.volume_read(level, muted, done),
            PlatformEvent::SpaceMoved => {
                // In the room's terms nothing moved: the arrangement went with the
                // origin. The menu's map is drawn from the eyes again.
                info!("the room's origin moved: the arrangement moves with it");
                self.view_eye = None;
            }
            PlatformEvent::RefreshRate(hz) => {
                info!("display at {hz} Hz");
                self.headset.refresh_hz = Some(hz);
            }
            PlatformEvent::CamerasLost => self.cameras_lost(),
            PlatformEvent::Texts(texts) => {
                if self.lang.loaded(texts) {
                    self.language_shown();
                }
            }
            PlatformEvent::Languages(known) => {
                self.lang.listed(known);
            }
            PlatformEvent::NotOwned { store } => self.not_owned(store),
            PlatformEvent::CamerasFailed(error) => {
                let text = self
                    .texts()
                    .f("headset-error-cameras", &[("error", error.into())]);
                self.show_error(text);
            }
            // Not offered for the rest of this run, and said; the setting stays as
            // chosen, and the next start tries again.
            PlatformEvent::KeyboardWindowFailed => {
                if let Some(room) = &mut self.headset.caps.room {
                    room.keyboard_window = false;
                }
                self.keyboard_window = None;
                let text = self.texts().t("headset-error-keyboard-window").to_owned();
                self.show_error(text);
            }
            PlatformEvent::CustomPanorama(there) => self.headset.custom_panorama = there,
            PlatformEvent::LayersHidden(hidden) => self.layers_hidden(hidden),
        }
    }

    /// `hidden` monitors do not fit in the compositor's layers now (the farthest go
    /// first, see `layers`). The person is told once, when more go than were said; after
    /// a while with every monitor shown, the next time is said again.
    fn layers_hidden(&mut self, hidden: usize) {
        let now = self.now;
        let h = &mut self.hidden;
        if hidden == 0 {
            if h.now > 0 {
                debug!("every monitor shows again");
                h.all_shown_since = Some(now);
            }
            h.now = 0;
            return;
        }
        if h.all_shown_since
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) >= HIDDEN_SAID_AGAIN)
        {
            h.told = 0;
        }
        h.now = hidden;
        if hidden <= h.told {
            return;
        }
        h.told = hidden;
        warn!(
            "{hidden} monitors hidden: the compositor takes {} layers in a frame",
            self.headset.caps.layers_max
        );
        let text = self
            .texts()
            .f("headset-layers-hidden", &[("count", hidden.into())]);
        self.show_toast(text);
    }

    /// What became of a stream's video (of its generation streaming now only). A
    /// lasting failure is said once; the monitor keeps its place, and the menu and the
    /// room show it with its reason.
    fn video(&mut self, e: &VideoEvent) {
        if self.videos.happened(e).is_none() {
            return;
        }
        info!("monitor {}: {:?}", e.id, e.what);
        let Happened::Failed(_) = e.what else {
            return;
        };
        // It stays where it stands, in the room and in the rows the PC hears: the PC's
        // desktop does not move for it, and the shell shows why in its place.
        let Some(failed) = self.videos.failure(e.id) else {
            return;
        };
        let reason = video::why_text(&failed, self.texts());
        let name = self.texts().f(
            "headset-monitor-n",
            &[("n", self.label_of(e.id).unwrap_or_default().into())],
        );
        let text = self.texts().f(
            "headset-video-failed",
            &[("name", name.into()), ("reason", reason.into())],
        );
        warn!("{text}");
        self.tell(Note::failed(None, &text), true, false);
    }

    /// The cameras broke for good: the "Room" backgrounds and the window onto the
    /// keyboard are no longer offered (the background chosen is kept for the next
    /// start; until then nothing shows behind the monitors), said once.
    fn cameras_lost(&mut self) {
        if self.headset.caps.room.take().is_none() {
            return;
        }
        self.keyboard_window = None;
        let text = self.texts().t("headset-error-cameras-lost").to_owned();
        self.show_error(text);
    }

    /// The display rate to ask for as the settings say, if the runtime lets the app
    /// choose (see `Caps::rate`).
    pub fn wanted_rate(&self) -> Option<f32> {
        self.headset.caps.rate(self.settings.battery_saver)
    }

    /// The store says the app is not the person's: the menu says so in front of the
    /// user (naming the store), long enough to read, then the app closes as the store
    /// expects.
    fn not_owned(&mut self, store: &'static str) {
        let now = self.now;
        let mut n = self.not_owned.take().unwrap_or(NotOwned {
            since: now,
            text: None,
        });
        // Written once in each language, then compared with what the menu shows.
        if n.text
            .as_ref()
            .is_none_or(|(texts, _)| texts != self.texts())
        {
            let text = self
                .texts()
                .f("headset-store-not-confirmed", &[("store", store.into())]);
            n.text = Some((self.texts().clone(), text));
        }
        let what = n.text.as_ref().map_or("", |(_, text)| text.as_str());
        if self.menu.error.as_ref().is_none_or(|(t, _)| t != what) {
            let what = what.to_owned();
            self.show_error(what);
            // In front of the user, wherever the menu was last.
            self.open_panel();
            // The time counts from when the message is up, not from the failure.
            n.since = now;
        }
        if now.saturating_duration_since(n.since) > NOT_OWNED_FOR {
            self.effects.push(Effect::Exit);
        }
        self.not_owned = Some(n);
    }

    /// The setup as it is now (see `Profile::of`): the monitors left to right with
    /// their scales, the primary, the PC's own displays shown, and where they stand (in
    /// cinema, the desk arrangement as kept before it).
    fn current_profile(&self) -> Profile {
        let shown = self
            .pc()
            .displays
            .iter()
            .flat_map(|d| &d.displays)
            .filter(|d| d.is_shown_physical())
            .filter_map(|d| self.key(d.id))
            .collect();
        let mut profile = self
            .world
            .profile(shown, |k| self.display(k).map_or(100, |d| d.mode.scale));
        // Which are in Auto, by place (the profile's monitors are the lineup's).
        let monitors = self.world.monitors();
        profile.auto = (0..profile.monitors.len())
            .filter(|&i| monitors.get(i).is_some_and(|m| self.is_auto(m.0)))
            .collect();
        profile
    }

    /// The placeholder of a monitor being added goes once its picture is there, it
    /// failed, the connection ended, or it waited too long.
    fn adding_due(&mut self) {
        let waits = self.session.waits(ui::Asked::AddMonitor);
        let before = self.world.adding().is_some();
        self.world.adding_due(self.now, waits);
        if before != self.world.adding().is_some() {
            self.rev += 1;
        }
    }

    /// Turns cinema mode on or off (see `World::set_cinema`).
    fn set_cinema(&mut self, on: bool) {
        self.world
            .set_cinema(&self.settings, on, self.head.1, self.now);
        self.settings.cinema = on;
        self.world_done();
    }

    /// What the files' writer said in this pass: a file that could not be saved is
    /// told once in this start (the next save tries again, and the log says each
    /// time), and files that failed together in one message.
    fn saves_told(&mut self) {
        let Some((file, error)) = self.first_failure.take() else {
            return;
        };
        let mut several = 0;
        for f in self.save_failures.take() {
            self.save_failures_told.mark(f);
            several += 1;
        }
        let id = if several > 1 {
            "headset-error-save-files"
        } else {
            match file {
                persist::File::Settings => "headset-error-save-settings",
                persist::File::Desk => "headset-error-save-desk",
                persist::File::Profiles => "headset-error-save-profiles",
                persist::File::Keyboard => "headset-error-save-keyboard",
            }
        };
        let text = self.texts().f(id, &[("error", error.into())]);
        // The log has it already, from the writer.
        self.rev += 1;
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
    fn follow_gaze(&mut self, looked: Option<(DisplayKey, Vec2)>) {
        let busy = self.world.wrist().is_some() || self.world.holding() || self.settings.cinema;
        let main = self.session.main();
        if !self.settings.gaze_mouse || !self.can_move_mouse() || main.is_none() || busy {
            self.gaze_on = None;
            return;
        }
        let Some((id, uv)) = looked else {
            self.gaze_on = None;
            return;
        };
        if self.focus() == Some(id) {
            self.gaze_on = None;
            return;
        }
        let now = self.now;
        match self.gaze_on {
            Some((on, since)) if on == id => {
                let recently = self
                    .gaze_moved
                    .is_some_and(|t| now.saturating_duration_since(t) < GAZE_AGAIN);
                if now.saturating_duration_since(since) >= GAZE_DWELL && !recently {
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
                    self.rev += 1;
                    self.gaze_moved = Some(now);
                    self.gaze_on = None;
                }
            }
            _ => self.gaze_on = Some((id, now)),
        }
    }

    /// Rest reminders: after so many minutes of wearing, a short message. Every 20
    /// minutes it is the 20-20-20 rule (look 6 m away for 20 seconds); hourly, a break.
    /// Taking the headset off for 5 minutes counts as a rest.
    fn remind_rest(&mut self, dt: f32) {
        let every = u64::from(self.settings.rest_every) * 60;
        if every == 0 {
            return;
        }
        // Off the head is a rest (see `presence_due`).
        if !self.presence.worn() {
            return;
        }
        self.worn_for += Duration::from_secs_f32(dt);
        if self.worn_for.as_secs() < every {
            return;
        }
        self.worn_for = Duration::ZERO;
        let id = if every <= 20 * 60 {
            "headset-rest-eyes"
        } else {
            "headset-rest-hour"
        };
        self.show_toast(self.texts().t(id).to_owned());
    }

    /// "Sound from the screen": the sound placed towards the monitor whose window plays
    /// it, as the head turns.
    fn place_sound(&mut self) {
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
        self.effects.push(Effect::Sound(SoundCmd::Place(angle)));
    }

    /// Shows `text` for a few seconds, below the line of sight.
    fn show_toast(&mut self, text: String) {
        // Not the text: it can be a PC notification (a message, a code).
        info!("toast shown");
        self.toasts += 1;
        self.menu.toast = Some(Toast {
            text,
            since: self.now,
            place: None,
            n: self.toasts,
        });
    }

    /// The setup changed where it is made (a monitor moved, added, removed...): no
    /// profile is the one in use.
    fn leave_profile(&mut self) {
        if self.menu.active_profile.take().is_some() {
            self.profiles_changed();
        }
    }

    /// The profiles or the one in use changed: the PCs hear of it once this pass ends
    /// (see [`Model::end_pass`]).
    fn profiles_changed(&mut self) {
        self.profiles_changed = true;
        self.rev += 1;
    }

    /// The profiles as the PCs are told: their names (empty for one without), and the
    /// one in use.
    fn profiles_part(&self) -> state::Profiles {
        state::Profiles {
            list: self
                .profiles
                .iter()
                .map(|p| state::ProfileName {
                    name: p.name.clone().unwrap_or_default(),
                })
                .collect(),
            active: self.menu.active_profile.and_then(|a| u8::try_from(a).ok()),
        }
    }

    /// The monitor most straight ahead of the head (not a window panel).
    fn monitor_in_front(&self) -> Option<DisplayKey> {
        let session = &self.session;
        self.world
            .monitor_in_front(self.head, |k| session.waits(ui::Asked::Remove(k)))
    }

    /// Turns the whole arrangement around the user (see `World::recenter`).
    fn recenter(&mut self) {
        self.world.recenter(&self.settings, self.head, self.now);
        self.world_done();
    }

    /// The A button (see `World::tidy_up`).
    fn tidy_up(&mut self) {
        self.world.tidy_up(&self.settings, self.now);
        self.world_done();
    }

    /// Opens the menu in front of the user: within arm's reach, a little lower, when
    /// hands are in view (to touch it), further away for controllers.
    fn open_panel(&mut self) {
        self.world
            .open_menu(self.head, self.hands_in_view, &self.settings);
        if !self.menu.open {
            // The person may have copied a picture or a language file onto the
            // headset meanwhile.
            self.effects.push(Effect::System(SystemCmd::LookForFiles));
        }
        self.menu.open = true;
        self.rev += 1;
        // What was said while it was closed came as a toast.
        self.menu.notes.clear();
    }

    fn close_panel(&mut self) {
        self.menu.open = false;
        self.rev += 1;
    }

    /// The person closes the menu (its ×, the menu button or its gesture): the
    /// first-run tips count as read, or they would open again right away.
    fn close_menu(&mut self) {
        self.close_panel();
        if !self.settings.intro_seen {
            self.settings.intro_seen = true;
            self.saves.mark(persist::File::Settings);
        }
    }

    /// Tells the PC whether the headset is worn (quietly: nothing to show the user),
    /// and keeps it for the next `Open`.
    fn tell_presence(&mut self) {
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
    fn tell_all(&mut self, msg: &ClientMsg) {
        for pc in &self.sessions {
            self.effects.push(Effect::Tell(*pc, msg.clone()));
        }
    }

    /// The battery was read (once a minute, more often on the palm menu): the menu
    /// shows it, the PC's tray hears it, and the next `Open` says it.
    fn battery_read(&mut self, battery: Option<(u8, bool)>) {
        self.headset.battery = battery;
        let state = self.headset_state();
        self.session.set_headset(state);
        if let Some((percent, charging)) = battery {
            self.tell_all(&ClientMsg::Battery {
                part: Battery { percent, charging },
            });
        }
    }

    /// What the headset says about itself when it opens a session.
    pub fn headset_state(&self) -> HeadsetState {
        HeadsetState {
            profiles: Some(self.profiles_part()),
            // With its count of the headset's own changes.
            volume: self.volume.as_ref().map(volume::SharedVolume::now),
            ..headset_state(
                &self.settings,
                self.presence.worn(),
                self.headset.battery,
                self.volume.as_ref().map(|v| (v.now().level, v.now().muted)),
                self.headset.mic_permission,
                &self.headset.caps,
            )
        }
    }

    /// The microphone's switch or permission changed: the PC hears at once (it makes
    /// its end of the microphone, or says there is no permission), and the next session
    /// starts with it.
    fn mic_changed(&mut self) {
        let state = self.headset_state();
        self.session.set_headset(state);
        self.mic_told = self.headset.mic_permission;
        let part = mic_state(&self.settings, self.headset.mic_permission);
        self.tell_all(&ClientMsg::UpState { part });
    }

    /// The settings the PC acts on changed: it hears at once, and the next session
    /// starts with them.
    fn prefs_changed(&mut self) {
        let state = self.headset_state();
        self.session.set_headset(state);
        let part = prefs_of(&self.settings);
        self.tell_all(&ClientMsg::Prefs { part });
    }

    /// Control `what` asks the PC to change something: one reply comes with its
    /// number, and the button pressed for it waits under itself meanwhile (the reply
    /// is said there, see `note`). Without a connection nothing is sent (and nothing
    /// kept to send later: repeating "add a monitor" after reconnecting could add two).
    fn request(&mut self, asked: Request, what: ui::Asked) -> Option<ReqId> {
        let pc = self.session.main();
        self.request_of(pc, asked, what)
    }

    /// [`Model::request`] of PC `pc` (none: no PC, which fails as not connected).
    fn request_of(&mut self, pc: Option<PcId>, asked: Request, what: ui::Asked) -> Option<ReqId> {
        let Some((pc, req)) = pc.zip(self.session.ask(pc, what, self.now)) else {
            self.ask_failed(ui::Failure::NotConnected);
            return None;
        };
        info!("request {}: {asked:?}", req.0);
        self.send_to(pc, ClientMsg::request(req, asked));
        if let Some(on) = self.menu.notes.asked(pc, req, what) {
            let waiting = Note::waiting(Some(on), self.texts());
            self.tell(waiting, false, true);
        }
        Some(req)
    }

    /// [`Model::request`] about display `k`, of its PC with the number `k` has there:
    /// failed as not connected without a session with that PC.
    fn request_for(
        &mut self,
        k: DisplayKey,
        asked: impl FnOnce(DisplayId) -> Request,
        what: ui::Asked,
    ) -> Option<ReqId> {
        let pc = self.session.connected(k.pc).then_some(k.pc);
        self.request_of(pc, asked(k.id), what)
    }

    /// Request `req` was answered: which control asked it, `None` for one this side
    /// does not wait for (answered after it gave up on it). What it comes to is said
    /// under the button that asked (see `note::answer`).
    fn answered(&mut self, pc: PcId, req: ReqId, outcome: &Outcome) -> Option<ui::Asked> {
        let answered = self.session.answered(pc, req, outcome, self.now);
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
        // Auto's own: nothing said in the menu.
        if let ui::Asked::AutoScale(k) = what {
            self.auto_scale_answered(k, outcome);
            return Some(what);
        }
        // A monitor added at a scale chosen by hand leaves Auto.
        if let (ui::Asked::AddMonitor, Outcome::Added { id }) = (what, outcome)
            && self.menu.add_fixed.take_if(|f| *f == (pc, req)).is_some()
        {
            self.added_fixed(pc, *id);
        }
        // A display of the PC asked to be shown or hidden: its switch shows what the
        // PC says again.
        if let ui::Asked::Physical(k) = what {
            self.menu.physical_asked.retain(|(a, _)| *a != k);
        }
        self.shown_as_asked(pc, req, matches!(outcome, Outcome::Failed { .. }));
        let took = answered.took.unwrap_or_default().as_millis();
        info!("request {} answered in {took} ms", req.0);
        if let Outcome::Failed { error, detail } = outcome {
            warn!("request {} failed (code {}): {detail}", req.0, error.code());
        }
        let asker = self.menu.notes.answered(pc, req);
        if let Some(told) = note::answer(what, asker, outcome, self.texts()) {
            self.tell(told.note, told.toast, asker.on.is_some());
        }
        Some(what)
    }

    /// Request `req` of PC `pc` was answered (`failed`: refused, or no answer will
    /// come): the encoding or the sound it showed at once as asked shows what it was
    /// before when it failed, and what the PC says from then on either way.
    fn shown_as_asked(&mut self, pc: PcId, req: ReqId, failed: bool) {
        if let Some((_, _, before)) = self
            .menu
            .encoding_before
            .take_if(|(p, r, _)| *p == pc && *r == req)
            && failed
            && let Some(e) = self
                .session
                .pc_mut(pc)
                .and_then(|p| p.state.encoding.as_mut())
        {
            e.wanted = before;
            self.rev += 1;
        }
        if let Some((_, _, before)) = self
            .menu
            .sound_before
            .take_if(|(p, r, _)| *p == pc && *r == req)
            && failed
            && let Some(s) = self.session.pc_mut(pc).and_then(|p| p.state.sound.as_mut())
        {
            s.to_headset = before;
            self.rev += 1;
        }
    }

    /// A request was not sent, for `why`: said under the button pressed for it.
    fn ask_failed(&mut self, why: ui::Failure) {
        let note = Note::failure(self.menu.notes.pressed(), why, "", self.texts());
        self.tell_press(note, true);
    }

    /// Request `req` of PC `pc` got no answer and never will (`why`: no answer in time,
    /// the connection broke): said under the button that asked.
    fn given_up(&mut self, pc: PcId, req: ReqId, why: ui::Failure) {
        self.shown_as_asked(pc, req, true);
        // A command of the PC waiting for it gets its answer: none came.
        if let Some(command) = self
            .session
            .pc_mut(pc)
            .and_then(|p| p.asks.take_command_after(req))
        {
            let detail = format!(
                "the headset got no answer in {} s",
                pending::ANSWER_WITHIN.as_secs()
            );
            self.send_to(
                pc,
                ClientMsg::Reply {
                    req: command,
                    outcome: Outcome::failed(ErrorCode::OTHER, &detail),
                },
            );
        }
        let on = self.menu.notes.answered(pc, req).on;
        let note = Note::failure(on, why, "", self.texts());
        self.tell(note, true, on.is_some());
    }

    /// News worth a `toast` (or not), about a press `in_menu` (or the menu's own news):
    /// said where `note::said` puts it.
    fn tell(&mut self, note: Note, toast: bool, in_menu: bool) {
        self.rev += 1;
        match note::said(self.menu.open, in_menu, toast) {
            note::Said::Note => self.menu.notes.say(note, self.now),
            note::Said::Toast => self.show_toast(note.toast_text()),
            note::Said::Nothing => {}
        }
    }

    /// What the press being done came to: in the menu for a press of the menu, else a
    /// toast where it is worth one.
    fn tell_press(&mut self, note: Note, toast: bool) {
        self.tell(note, toast, self.press_in_menu);
    }

    /// The menu's own action `action` was done: said under its button, where what it
    /// changed does not show on the button itself (see `note::done_text`).
    fn done(&mut self, action: Action) {
        if let Some(id) = note::done_text(action) {
            let note = Note::done(Some(action), self.texts().t(id));
            self.tell_press(note, false);
        }
    }

    /// `action` needs a monitor in front of the user, and there is none.
    fn no_monitor_in_front(&mut self, action: Action) {
        let note = Note::failed(
            Some(action),
            self.texts().t("headset-note-no-monitor-in-front"),
        );
        self.tell_press(note, true);
    }

    /// The menu shows the language chosen (loaded, or the one it had): see
    /// `note::language_note`.
    fn language_shown(&mut self) {
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
    fn expire_asks(&mut self) {
        let now = self.now;
        for (pc, req, what) in self.session.expire(now) {
            self.world.switch_given_up(pc, req);
            // Auto's own asks were nobody's press: nothing said.
            if matches!(what, ui::Asked::AutoScale(_)) {
                continue;
            }
            self.given_up(pc, req, ui::Failure::TimedOut);
        }
        if self.menu.notes.shown().is_some() {
            self.menu.notes.expire(now);
            if self.menu.notes.shown().is_none() {
                self.rev += 1;
            }
        }
        let switching = self.world.switching();
        self.world.switch_due(&self.settings, now);
        if switching != self.world.switching() {
            self.rev += 1;
        }
        self.world_done();
    }

    /// Sends `msg` to PC `pc` (the shell says when it cannot).
    fn send_to(&mut self, pc: PcId, msg: ClientMsg) {
        self.effects.push(Effect::Send(pc, msg));
    }

    /// The language the menu is to show, if it is neither shown nor already asked for:
    /// the `system` thread is to load it (chosen in the menu, else the PC's, else the
    /// headset's).
    fn wanted_lang(&mut self) -> Option<Lang> {
        let s = &self.settings;
        self.lang
            .wanted(s.language, s.pc_language, self.system_lang)
    }

    fn show_error(&mut self, text: String) {
        warn!("{text}");
        self.put_error(text);
    }

    /// [`Model::show_error`] without the line in the log.
    fn put_error(&mut self, text: String) {
        self.rev += 1;
        self.menu.error = Some((text, self.now));
    }
}

/// The toast's width in the room.
pub const TOAST_WIDTH: f32 = 0.46;

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
        Tab::Profiles => 8,
        Tab::Settings(Section::Paired) => 9,
        #[cfg(feature = "preview")]
        Tab::Settings(Section::Dev) => 10,
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
        8 => Tab::Profiles,
        9 => Tab::Settings(Section::Paired),
        // "For development" only where the build has it.
        #[cfg(feature = "preview")]
        10 => Tab::Settings(Section::Dev),
        _ => Tab::Quick,
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
/// settings, whether it is worn, its battery, its volume (percent and muted: shared
/// where `caps` offers it and it could be read), the microphone's switch and
/// `permission`; what it can do as `caps` says.
pub fn headset_state(
    settings: &Settings,
    worn: bool,
    battery: Option<(u8, bool)>,
    volume: Option<(u8, bool)>,
    permission: u8,
    caps: &Caps,
) -> HeadsetState {
    let mic = mic_state(settings, permission);
    let mut told = caps.headset_caps();
    let volume = volume.filter(|_| caps.offers(Offer::SharedVolume));
    if volume.is_none() {
        told.features.retain(|f| *f != caps::headset::VOLUME);
    }
    HeadsetState {
        caps: Some(told),
        prefs: Some(prefs_of(settings)),
        presence: Some(Presence { worn }),
        battery: battery.map(|(percent, charging)| Battery { percent, charging }),
        volume: volume
            .map(|(level, muted)| prostir_proto::volume::HeadsetVolume::new(level, muted).now()),
        up: std::iter::once((mic.kind, mic)).collect(),
        // The model adds them (see `Model::headset_state`).
        profiles: None,
    }
}

/// The microphone's switch, and whether the system lets the app record.
fn mic_state(settings: &Settings, permission: u8) -> state::UpState {
    state::UpState {
        kind: state::UpKind::Mic,
        switch: settings.mic,
        permission,
    }
}
