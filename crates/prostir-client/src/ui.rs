//! The floating menu panel. It is drawn on the CPU into an RGBA image and only
//! re-uploaded when something changes (hover, a click, news from the host). Pointing at
//! another button draws only that button's part of the picture again (see
//! [`Panel::damage`]), from copies kept after the first time.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};
use prostir_proto::Codec;
use prostir_proto::state::{Encoding, ErrorCode, Limits};
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, PixmapMut, Rect, Stroke, Transform};

use crate::clarity::Dev;
#[cfg(feature = "preview")]
use crate::clarity::{Filter, MonitorSize};
use crate::ids::DisplayKey;
use crate::lang::{Language, Moment, Texts};
use crate::note::{Kind, Note};
use crate::platform::store::UpdateHint;
use crate::settings::{Background, Curve, HandMode, Preset, Settings, Sky};

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 960;
/// The small menu by the left wrist, in pixels.
pub const WRIST_WIDTH: u32 = 512;
pub const WRIST_HEIGHT: u32 = 512;
/// The sizes shown before the PC says its own.
const STANDARD_SIZES: [(u32, u32); 5] = [
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (3440, 1440),
    (1440, 2560),
];

static REGULAR: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");
static MEDIUM: &[u8] = include_bytes!("../assets/NotoSans-Medium.ttf");

/// A change of how the PC encodes, from the menu: `None` leaves a setting as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodingChange {
    pub bitrate: Option<u16>,
    pub codec: Option<Codec>,
    pub software: Option<bool>,
    pub sharp: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorEntry {
    pub id: DisplayKey,
    /// What the user reads for it (see [`display_label`]); never the id.
    pub label: String,
    pub width: u32,
    pub height: u32,
    /// Where it stands around the user, to tell monitors apart.
    pub place: Place,
    /// As seen from where the user sits, in radians: centre (right, up) and half
    /// size, for the map in the menu.
    pub seen: Option<[f32; 4]>,
    /// Its scale, percent (100: one pixel of the picture is one of the desktop).
    pub scale: u16,
    /// How its picture is.
    pub picture: Picture,
}

/// How a monitor's picture is, said under its name in the menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Picture {
    #[default]
    Shows,
    /// No picture since its video failed; being recovered.
    Recovering,
    /// It cannot be shown, for this reason (in the menu's words).
    NotShown(String),
}

/// A display of the PC itself, with its switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalEntry {
    pub id: DisplayKey,
    /// What the person calls it (see `physical::label`).
    pub name: String,
    /// Its connector (`HDMI-A-1`), shown smaller under a name of its own.
    pub connector: Option<String>,
    /// Shown in the headset (as asked, while the PC has not answered yet).
    pub shown: bool,
    /// Connected now: one that is not (a closed laptop's panel) shows nothing, and
    /// counts for no limit.
    pub connected: bool,
}

/// Profiles on one page of the Profiles tab.
pub const PROFILES_PER_PAGE: usize = 10;

/// What the user reads for a display named `name` by the PC: the number the PC's
/// desktop gives its output (`Prostir-3` is monitor 3, as in the PC's display
/// settings), or the PC's own name for a display of the PC (`eDP-1`). Never the
/// display's id, which is the PC's bookkeeping and grows with every monitor made.
pub fn display_label(name: &str) -> String {
    match name.strip_prefix("Prostir-") {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => n.to_owned(),
        _ => name.to_owned(),
    }
}

/// Where a monitor stands, as seen by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Place {
    #[default]
    Unknown,
    Left,
    Center,
    Right,
    Above,
    Below,
}

impl Place {
    /// The message that names it; none where it is not known.
    fn name(self) -> Option<&'static str> {
        Some(match self {
            Place::Unknown => return None,
            Place::Left => "headset-place-left",
            Place::Center => "headset-place-middle",
            Place::Right => "headset-place-right",
            Place::Above => "headset-place-above",
            Place::Below => "headset-place-below",
        })
    }
}

/// Pairing with a new PC: either show `code` (the PC asks for it) or let the user
/// type the PC's code into `typed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingView {
    pub host: String,
    pub code: Option<String>,
    pub typed: String,
    /// The last attempt's codes did not match.
    pub failed: bool,
}

/// Why the headset asks whether to pair with a PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferWhy {
    /// A PC this headset has not paired with.
    New,
    /// The last pairing stopped after several wrong codes.
    WrongCodes,
    /// The PC forgot this headset and ended the session.
    Forgotten,
    /// The PC did not take part in pairing (it has no dialog for the code, the code
    /// was not typed there, another headset is pairing): its own words are in the
    /// error line. `pc_shows`: the PC was to show the code.
    PcRefused { pc_shows: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    pub pairing: Option<PairingView>,
    /// A PC that announces itself but does not accept the connection (a firewall).
    pub unreachable: Option<String>,
    /// A PC to pair with: its name, and why the headset asks.
    pub pair_offer: Option<(String, OfferWhy)>,
    /// Name of the connected PC, `None` while searching.
    pub host: Option<String>,
    /// PCs seen on the network while not connected: (name, address).
    pub found: Vec<(String, String)>,
    /// IP being typed on the keypad.
    pub ip_input: Option<String>,
    /// What the search screen says while no PC answers.
    pub hint: Option<crate::session::Hint>,
    /// The PC's virtual monitors, sorted by id.
    pub monitors: Vec<MonitorEntry>,
    /// The monitor picked on the map (its actions show under it).
    pub selected: Option<DisplayKey>,
    /// The PC's own displays shown, on the map: centre and half size as seen, in
    /// radians, and what they are called.
    pub physical_seen: Vec<([f32; 4], String)>,
    /// The PC's own displays, each with its switch, those connected first.
    pub physical: Vec<PhysicalEntry>,
    /// Which of them the Monitors tab shows, when there are more than fit.
    pub physical_page: u8,
    pub primary: Option<DisplayKey>,
    /// The screen picked for cinema (see `layout::cinema_choice`).
    pub cinema_screen: Option<DisplayKey>,
    /// PC sound goes to the headset.
    pub audio_wanted: bool,
    pub mic_wanted: bool,
    /// Access to the microphone was refused in the system.
    pub mic_denied: bool,
    /// How the PC encodes and what it can encode with (while connected).
    pub pc: Option<Encoding>,
    /// The monitors the PC makes for this headset (while connected).
    pub limits: Option<Limits>,
    /// The PC makes monitors at another scale (its capability 10): the menu offers its
    /// scales.
    pub can_scale: bool,
    /// The scale a monitor added gets, percent.
    pub add_scale: u16,
    /// Window panels shown (detached PC windows).
    pub windows: Vec<MonitorEntry>,
    /// The PC can move the mouse (for following the gaze).
    pub can_move_mouse: bool,
    /// Names of the PCs this headset is paired with.
    pub paired: Vec<String>,
    /// The headset's battery: percent, charging.
    pub battery: Option<(u8, bool)>,
    /// Local time, `14:05`, and the date, `Ср, 18 лист.`, for the palm menu.
    pub clock: std::sync::Arc<str>,
    pub date: std::sync::Arc<str>,
    /// The paired PC waiting for a second press to be forgotten.
    pub confirm_forget: Option<usize>,
    pub settings: Settings,
    pub room_available: bool,
    pub panorama_available: bool,
    /// The user put a `background.jpg/png` on the headset.
    pub custom_available: bool,
    pub curve_available: bool,
    /// The headset can show a passthrough window onto the keyboard.
    pub keyboard_available: bool,
    /// The headset tracks hands: the choice of what they do shows.
    pub hands_available: bool,
    /// The display rates to choose from, usual and saving the battery, in hertz (none:
    /// the runtime chooses, the choice is hidden).
    pub display_rates: Option<(u16, u16)>,
    /// The display rate now, as the runtime says.
    pub display_rate_now: Option<u16>,
    pub error: Option<String>,
    /// Monitor whose "×" was pressed once and now asks for confirmation.
    pub confirm_remove: Option<DisplayKey>,
    /// A profile whose "Save here" was pressed once (press again to overwrite).
    pub confirm_save: Option<usize>,
    /// A profile whose "×" was pressed once (press again to delete it).
    pub confirm_delete: Option<usize>,
    pub tab: Tab,
    /// The menu's language.
    pub texts: Texts,
    /// The languages there are files for.
    pub languages: std::sync::Arc<[crate::lang::Known]>,
    /// Which eight of them the Language page shows, when there are more than nine.
    pub language_page: u8,
    /// First run: show the tips page instead of the tabs.
    pub intro: bool,
    /// The PC speaks another protocol: (the PC is newer, the PC app's version).
    pub update: Option<(bool, String)>,
    /// Where this build of the headset app is updated (from its store).
    pub update_from: UpdateHint,
    /// The profiles saved, by the names the menu shows (the one given, or "Profile 2").
    pub profiles: Vec<String>,
    /// Which of them the Profiles tab shows (see [`PROFILES_PER_PAGE`]).
    pub profile_page: u8,
    /// The profile last switched to (until something is changed by hand).
    pub active_profile: Option<usize>,
    /// What the menu says under the button pressed last: done, waiting for the PC, or
    /// why it failed (see `note`).
    pub note: Option<Note>,
    /// "For development", in the preview build only (`None`: no such part).
    pub dev: Option<DevView>,
}

/// What "For development" shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevView {
    pub dev: Dev,
    /// The headset's pixels per degree are known: the sizes by them can be chosen.
    pub known: bool,
    /// "Reset" has something to put back.
    pub can_reset: bool,
}

/// Which control asked the PC for something: its answer is said under the button pressed
/// for it (see `note::Notes::asked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    AddMonitor,
    Remove(DisplayKey),
    MakePrimary(DisplayKey),
    /// A display of the PC itself shown or hidden.
    Physical(DisplayKey),
    /// A monitor's scale.
    Scale(DisplayKey),
    Sound,
    Encoding,
    DetachWindow,
    WindowHere,
    Screenshot,
    FindMouse,
    Profile(usize),
}

/// Why a request failed: the PC's code, or something on this side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Pc(ErrorCode),
    /// Not sent: no connection.
    NotConnected,
    /// The connection broke before the answer.
    Disconnected,
    /// No answer in time.
    TimedOut,
}

impl Failure {
    /// The message the person reads; `None` where only the PC's detail says what
    /// happened.
    pub fn text(self) -> Option<&'static str> {
        Some(match self {
            Self::NotConnected => "headset-failure-not-connected",
            Self::Disconnected => "headset-failure-disconnected",
            Self::TimedOut => "headset-failure-timed-out",
            Self::Pc(code) => match code.known() {
                ErrorCode::UNSUPPORTED => "headset-failure-unsupported",
                ErrorCode::NOT_ALLOWED => "headset-failure-not-allowed",
                ErrorCode::UNKNOWN_DISPLAY => "headset-failure-unknown-display",
                ErrorCode::LIMIT => "headset-failure-limit",
                ErrorCode::BAD_SIZE => "headset-failure-bad-size",
                ErrorCode::LAST_DISPLAY => "headset-failure-last-display",
                ErrorCode::NO_WINDOW => "headset-failure-no-window",
                ErrorCode::DESKTOP => "headset-failure-desktop",
                ErrorCode::ENCODER => "headset-failure-encoder",
                ErrorCode::CAPTURE => "headset-failure-capture",
                ErrorCode::SOUND => "headset-failure-sound",
                ErrorCode::NOT_ON_DISPLAYS => "headset-failure-not-on-displays",
                ErrorCode::NO_SUCH_PROFILE => "headset-failure-no-such-profile",
                ErrorCode::BAD_ARGUMENT => "headset-failure-bad-argument",
                _ => return None,
            },
        })
    }
}

/// Pages of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// Large tiles for what is used every day.
    #[default]
    Quick,
    Monitors,
    /// Every profile: switched to, saved, deleted, a new one.
    Profiles,
    Settings(Section),
}

/// A part of the Settings tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Look,
    /// Hands, the mouse, menu size and handedness.
    Controls,
    Sound,
    Language,
    Pc,
    /// The PCs this headset is paired with.
    Paired,
    Help,
    /// "For development": in the preview build only (see `UiState::dev`).
    #[cfg(feature = "preview")]
    Dev,
}

impl Tab {
    fn section(self) -> Option<Section> {
        match self {
            Tab::Settings(s) => Some(s),
            _ => None,
        }
    }

    /// The same top-level tab (any section of Settings counts as Settings).
    fn same_page(self, other: Tab) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add {
        width: u32,
        height: u32,
    },
    Remove(DisplayKey),
    MakePrimary(DisplayKey),
    /// Move a monitor left (-1) or right (+1) in the row.
    Move {
        id: DisplayKey,
        delta: i32,
    },
    /// Show (`true`) or hide a display of the PC itself.
    ShowPhysical(DisplayKey, bool),
    /// The scale a monitor added gets, percent.
    AddScale(u16),
    /// Another scale for a monitor, percent.
    SetScale(DisplayKey, u16),
    /// "No limits": more monitors and displays than the PC advises.
    NoLimits(bool),
    SetAudio(bool),
    SetMic(bool),
    Preset(Preset),
    Curve(Curve),
    Background(Background),
    Hands(HandMode),
    Focus(bool),
    KeyboardWindow(bool),
    Cinema(bool),
    /// Which monitor (position in the row) cinema mode shows.
    CinemaScreen(DisplayKey),
    ApplyProfile(usize),
    /// Save the setup over profile `n` (second press confirms).
    SaveProfile(usize),
    /// Save the setup as a new profile, at the end of the list.
    NewProfile,
    /// Delete profile `n` (second press confirms).
    DeleteProfile(usize),
    /// Show these profiles on the Profiles tab (see `UiState::profile_page`).
    ProfilePage(u8),
    /// Show these displays of the PC on the Monitors tab (see
    /// `UiState::physical_page`).
    PhysicalPage(u8),
    Tab(Tab),
    /// Lay everything out again in front of the user, where they look now.
    Recenter,
    /// The first-run tips were read.
    IntroDone,
    Language(Language),
    /// Show these languages on the Language page (see `UiState::language_page`).
    LanguagePage(u8),
    /// Connect to the n-th PC in `UiState::found`.
    ConnectTo(usize),
    ShowKeypad,
    Key(char),
    Backspace,
    ConnectTyped,
    CancelKeypad,
    /// Disconnect and pick another PC.
    ChangeHost,
    /// Pair the other way round (swap which side shows the code).
    PairOtherWay,
    /// Answer to "Pair with this PC?".
    PairAnswer(bool),
    /// Ask for microphone access again after it was refused.
    AskMic,
    /// Change how the PC encodes.
    SetEncoding(EncodingChange),
    /// Show the first-run tips again.
    ShowTips,
    /// The monitors follow the head.
    Follow(bool),
    /// A monitor let go almost against another sticks to it.
    Magnet(bool),
    /// Dragging one monitor moves the whole arrangement.
    MoveAll(bool),
    /// Put the PC's active window on a panel of its own.
    DetachWindow,
    /// Move the PC's active window to the monitor in front.
    WindowHere,
    /// Save a picture of the monitor in front on the PC.
    Screenshot,
    /// Close a window panel (its window goes back to the monitors).
    CloseWindow(DisplayKey),
    /// Pick a monitor on the map.
    SelectMonitor(DisplayKey),
    /// What the A button does: take the seams out of the arrangement.
    Tidy,
    /// Open the main menu (from the wrist menu).
    OpenMenu,
    /// Show where the mouse is.
    FindMouse,
    /// The mouse follows the gaze.
    GazeMouse(bool),
    LargeMenu(bool),
    LeftHanded(bool),
    /// 72 Hz display to save battery.
    BatterySaver(bool),
    /// Show the PC's notifications in the headset.
    Notifications(bool),
    /// Rest reminders every so many minutes of wearing (0: none).
    RestEvery(u16),
    /// Forget paired PC number `n` (second press confirms).
    ForgetPc(usize),
    /// Sound comes from the monitor of the window playing it.
    SoundFromScreen(bool),
    /// 12-hour clock.
    Clock12h(bool),
    DateStyle(crate::settings::DateStyle),
    /// Show the round ☰ button under the main monitor.
    MenuButton(bool),
    /// How bright the monitors are, percent.
    Brightness(u8),
    /// The note under a button was pressed: it goes.
    HideNote,
    /// "For development": how large the monitors are made (laid out again at once).
    #[cfg(feature = "preview")]
    DevSize(MonitorSize),
    /// "For development": the monitors back where they stood before another size;
    /// those added in between stay where they were put.
    #[cfg(feature = "preview")]
    DevSizeReset,
    /// "For development": the layers' supersampling and sharpening.
    #[cfg(feature = "preview")]
    DevSuperSampling(Filter),
    #[cfg(feature = "preview")]
    DevSharpening(Filter),
    /// "For development": the numbers next to the monitor looked at.
    #[cfg(feature = "preview")]
    DevMetrics(bool),
    Close,
}

impl Action {
    /// The same control as `other`: the same action, or a switch whose one button
    /// turns it on and off (a tile on the Quick page), which after a press stands for
    /// the other value.
    pub fn same_control(self, other: Action) -> bool {
        use Action::{Cinema, Focus, KeyboardWindow, SetAudio, SetMic};
        self == other
            || matches!(
                (self, other),
                (Focus(_), Focus(_))
                    | (Cinema(_), Cinema(_))
                    | (KeyboardWindow(_), KeyboardWindow(_))
                    | (SetAudio(_), SetAudio(_))
                    | (SetMic(_), SetMic(_))
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Style {
    Normal,
    Selected,
    /// A tab: the current one, or one to switch to.
    TabOn,
    TabOff,
    Danger,
    /// Asking to confirm a destructive action.
    Armed,
    Disabled,
}

enum Item {
    Panel(Rect),
    Box(Rect, [u8; 4]),
    /// A small filled circle: a status light.
    Dot {
        x: f32,
        y: f32,
        r: f32,
        color: [u8; 4],
    },
    Text {
        x: f32,
        baseline: f32,
        size: f32,
        medium: bool,
        color: [u8; 4],
        text: String,
        /// How wide it may be (the panel's edge when wider); a longer text is drawn
        /// smaller (see [`Panel::fit`]).
        room: f32,
        /// It holds a name: shortened with an ellipsis where even the smallest size is
        /// too wide.
        shorten: bool,
    },
    Button {
        rect: Rect,
        label: String,
        /// The label's size: smaller when a long one does not fit (see [`Panel::fit`]).
        size: f32,
        style: Style,
        action: Option<Action>,
        /// The label is a name (a monitor's, as the PC calls it), not a text of the
        /// menu, in full: where it does not fit even at the smallest size, the label is
        /// shortened with an ellipsis (see [`Panel::fit`]), and pointed at, the button
        /// shows it whole above itself.
        full: Option<String>,
        /// What the button stands for, also while it is chosen or unavailable and
        /// `action` is none: the note about it is said under it (see [`Note::on`]).
        key: Option<Action>,
    },
    /// What the menu says under the button pressed (see [`Note`]): drawn over what
    /// lies below the button, on a box `rect` with a point towards the button at `tip`
    /// (x), up from the box's top, or down from its bottom where the box is above the
    /// button; none on the bottom line. Pressed, it goes.
    Note {
        rect: Rect,
        tip: Option<f32>,
        above: bool,
        /// Inside its own (tall) button: a press there is the button's.
        inside: bool,
        kind: Kind,
        text: String,
        size: f32,
        detail: Option<String>,
        detail_size: f32,
    },
    /// A ring sector around (`cx`, `cy`) from radius `r0` to `r1` and angle `a0` to
    /// `a1` (radians, clockwise from straight up), pressed like a button: the round
    /// palm menu. `r0` 0 and a full turn make a round button.
    Wedge {
        cx: f32,
        cy: f32,
        r0: f32,
        r1: f32,
        a0: f32,
        a1: f32,
        label: String,
        size: f32,
        style: Style,
        action: Option<Action>,
    },
}

impl Item {
    /// Where a shape is drawn (`None` for a text and a note, which are drawn wherever
    /// they reach).
    fn bounds(&self) -> Option<Rect> {
        match self {
            Item::Panel(r) | Item::Box(r, _) | Item::Button { rect: r, .. } => Some(*r),
            Item::Dot { x, y, r, .. } => Some(rect(x - r, y - r, 2.0 * r, 2.0 * r)),
            Item::Wedge { cx, cy, r1, .. } => Some(rect(cx - r1, cy - r1, 2.0 * r1, 2.0 * r1)),
            Item::Text { .. } | Item::Note { .. } => None,
        }
    }

    /// Whether (`x`, `y`) is on this wedge.
    fn wedge_contains(&self, x: f32, y: f32) -> bool {
        let Item::Wedge {
            cx,
            cy,
            r0,
            r1,
            a0,
            a1,
            ..
        } = self
        else {
            return false;
        };
        let (dx, dy) = (x - cx, y - cy);
        let r = (dx * dx + dy * dy).sqrt();
        let a = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU);
        // A sector may run past a full turn (the one straight up).
        let within = |a: f32| a >= *a0 && a <= *a1;
        r >= *r0 && r <= *r1 && (within(a) || within(a + std::f32::consts::TAU))
    }
}

/// Where a wedge's label goes: the middle of the sector.
fn wedge_center(cx: f32, cy: f32, r0: f32, r1: f32, a0: f32, a1: f32) -> (f32, f32) {
    if r0 <= 0.0 {
        return (cx, cy);
    }
    let (r, a) = ((r0 + r1) / 2.0, (a0 + a1) / 2.0);
    (cx + r * a.sin(), cy - r * a.cos())
}

/// The box around a ring sector (see [`Item::Wedge`]).
fn sector_bounds(cx: f32, cy: f32, r0: f32, r1: f32, a0: f32, a1: f32) -> Rect {
    let at = |r: f32, a: f32| (cx + r * a.sin(), cy - r * a.cos());
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for i in 0..=32 {
        let a = a0 + (a1 - a0) * i as f32 / 32.0;
        for r in [r0.max(0.0), r1] {
            let (x, y) = at(r, a);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    rect(x0, y0, x1 - x0, y1 - y0)
}

/// A ring sector as a path (see [`Item::Wedge`]), `None` if degenerate.
fn sector(cx: f32, cy: f32, r0: f32, r1: f32, a0: f32, a1: f32) -> Option<Path> {
    if r0 <= 0.0 && a1 - a0 >= std::f32::consts::TAU - 1e-3 {
        return circle(cx, cy, r1);
    }
    let at = |r: f32, a: f32| (cx + r * a.sin(), cy - r * a.cos());
    let steps = 32;
    let mut pb = PathBuilder::new();
    let (x, y) = at(r1, a0);
    pb.move_to(x, y);
    for i in 1..=steps {
        let (x, y) = at(r1, a0 + (a1 - a0) * i as f32 / steps as f32);
        pb.line_to(x, y);
    }
    for i in (0..=steps).rev() {
        let (x, y) = at(r0, a0 + (a1 - a0) * i as f32 / steps as f32);
        pb.line_to(x, y);
    }
    pb.close();
    let path = pb.finish();
    debug_assert!(path.is_some(), "degenerate sector at {cx}, {cy}");
    path
}

const TEXT: [u8; 4] = [232, 234, 240, 255];
/// Secondary text: light enough to read comfortably in the headset.
const MUTED: [u8; 4] = [168, 175, 190, 255];
/// Section titles above groups of options.
const SECTION: [u8; 4] = [206, 211, 222, 255];
/// Status lights.
const GOOD: [u8; 4] = [74, 222, 128, 255];
const WAIT: [u8; 4] = [251, 191, 36, 255];
const ACCENT: [u8; 4] = [61, 126, 255, 255];
const DANGER: [u8; 4] = [255, 107, 107, 255];

/// A part of the menu's picture, in whole pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Area {
    pub const EMPTY: Area = Area {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    /// `r` and `margin` around it, within a picture `pw`×`ph`; none outside it.
    fn around(r: Rect, margin: f32, pw: u32, ph: u32) -> Option<Self> {
        let clamp = |v: f32, max: u32| v.clamp(0.0, max as f32) as u32;
        let (x0, y0) = (
            clamp((r.left() - margin).floor(), pw),
            clamp((r.top() - margin).floor(), ph),
        );
        let (x1, y1) = (
            clamp((r.right() + margin).ceil(), pw),
            clamp((r.bottom() + margin).ceil(), ph),
        );
        (x1 > x0 && y1 > y0).then_some(Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// Larger by `by` on each side, within a picture `pw`×`ph`.
    fn grown(self, by: u32, pw: u32, ph: u32) -> Self {
        let (x0, y0) = (self.x.saturating_sub(by), self.y.saturating_sub(by));
        let (x1, y1) = (
            (self.x + self.w + by).min(pw),
            (self.y + self.h + by).min(ph),
        );
        Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// The smallest area holding both.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let (x0, y0) = (self.x.min(other.x), self.y.min(other.y));
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Its pixels' bytes.
    fn len(&self) -> usize {
        self.w as usize * self.h as usize * 4
    }
}

/// An item's part of the picture, kept as not pointed at and as pointed at (see
/// [`Panel::render_if_dirty`]).
#[derive(Debug, Clone, Copy)]
struct LitPart {
    item: usize,
    area: Area,
    /// Where its two copies start in `Panel::lit_pixels`.
    plain: usize,
    lit: usize,
}

/// How far around an item pointed at its look changes (its outline, smoothing).
const LIT_MARGIN: f32 = 3.0;
/// How much larger than its area a part is drawn before it is cut: shapes cut at the
/// edge of a part are smoothed a little differently there.
const PART_PAD: u32 = 2;

/// `area` of `pixmap`, row by row, added to `out`.
fn copy_out(pixmap: &Pixmap, area: Area, out: &mut Vec<u8>) {
    let row = pixmap.width() as usize * 4;
    let (x, w) = (area.x as usize * 4, area.w as usize * 4);
    for y in area.y as usize..(area.y + area.h) as usize {
        let at = y * row + x;
        out.extend_from_slice(&pixmap.data()[at..at + w]);
    }
}

/// `area` of `pixmap` from `from`, its rows one after another.
fn copy_in(pixmap: &mut Pixmap, area: Area, from: &[u8]) {
    let row = pixmap.width() as usize * 4;
    let (x, w) = (area.x as usize * 4, area.w as usize * 4);
    let data = pixmap.data_mut();
    for (k, y) in (area.y as usize..(area.y + area.h) as usize).enumerate() {
        let at = y * row + x;
        data[at..at + w].copy_from_slice(&from[k * w..k * w + w]);
    }
}

/// Where shapes and texts are drawn: a whole picture, or a part of the menu's picture
/// whose top left corner is `at` (each moved by it).
struct Canvas<'a> {
    pm: PixmapMut<'a>,
    at: (i32, i32),
}

impl Canvas<'_> {
    fn whole(pixmap: &mut Pixmap) -> Canvas<'_> {
        Canvas {
            pm: pixmap.as_mut(),
            at: (0, 0),
        }
    }

    fn transform(&self) -> Transform {
        Transform::from_translate(-self.at.0 as f32, -self.at.1 as f32)
    }

    /// Whether `r` (in the whole picture) reaches into it.
    fn meets(&self, r: Rect) -> bool {
        let (x, y) = (self.at.0 as f32, self.at.1 as f32);
        let (w, h) = (self.pm.width() as f32, self.pm.height() as f32);
        r.right() + 2.0 >= x
            && r.left() - 2.0 <= x + w
            && r.bottom() + 2.0 >= y
            && r.top() - 2.0 <= y + h
    }
}

pub struct Panel {
    regular: Font,
    medium: Font,
    glyphs: HashMap<(bool, char, u32), (Metrics, Vec<u8>)>,
    items: Vec<Item>,
    state: Option<UiState>,
    hovered: Option<usize>,
    pixmap: Pixmap,
    /// The items changed: the whole picture is drawn again.
    dirty: bool,
    /// The item drawn pointed at in `pixmap` now (the rest is drawn as not pointed at).
    lit: Option<usize>,
    /// Each item pointed at since the picture was last drawn whole: its part of the
    /// picture, as not pointed at and as pointed at, kept in `lit_pixels`; pointing at
    /// it again copies them back without drawing (see [`Panel::render_if_dirty`]).
    lit_parts: Vec<LitPart>,
    lit_pixels: Vec<u8>,
    /// Where a part is drawn before it is kept (its memory kept between parts).
    scratch: Vec<u8>,
    /// What the last [`Panel::render_if_dirty`] changed: `None` for all of it.
    damage: Option<Area>,
    /// The wrist menu rather than the main one.
    wrist: bool,
    /// The page shown.
    page: Page,
    /// The button pressed last, where it was, and on which page: a note about it is
    /// said there, also when the button has gone since (a monitor removed) or another
    /// one took its place (the next PC in the list after one was forgotten).
    pressed: Option<(Action, Rect, Page)>,
}

/// Which page the menu shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Offer,
    Pairing,
    Update,
    Intro,
    Welcome,
    Tab(Tab),
}

impl Page {
    /// The page `s` shows (as [`Panel::build`] picks it).
    fn of(s: &UiState) -> Self {
        if s.pair_offer.is_some() {
            Self::Offer
        } else if s.pairing.is_some() {
            Self::Pairing
        } else if s.update.is_some() {
            Self::Update
        } else if s.intro && s.host.is_some() {
            Self::Intro
        } else if s.host.is_none() {
            Self::Welcome
        } else {
            Self::Tab(s.tab)
        }
    }
}

/// A note's sizes: its text, the PC's detail under it, the room on either side.
const NOTE_SIZE: f32 = 19.0;
const NOTE_DETAIL_SIZE: f32 = 15.0;
const NOTE_PAD: f32 = 18.0;
/// The gap between a button and its note, where the note's point is.
const NOTE_GAP: f32 = 10.0;
/// The least room a note keeps from what it does not cover.
const NOTE_CLEAR: f32 = 6.0;
/// Room on a note's right for its "×".
const NOTE_CLOSE: f32 = 26.0;
/// The highest a note reaches: under the tabs, which stay in view.
const NOTE_TOP: f32 = 192.0;

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel {
    pub fn new() -> Self {
        let load = |bytes| Font::from_bytes(bytes, FontSettings::default()).expect("bundled font");
        Self {
            regular: load(REGULAR),
            medium: load(MEDIUM),
            glyphs: HashMap::new(),
            items: Vec::new(),
            state: None,
            hovered: None,
            pixmap: Pixmap::new(WIDTH, HEIGHT).expect("panel size"),
            dirty: true,
            lit: None,
            lit_parts: Vec::new(),
            lit_pixels: Vec::new(),
            scratch: Vec::new(),
            damage: None,
            wrist: false,
            page: Page::Welcome,
            pressed: None,
        }
    }

    /// The small menu by the left wrist: four big buttons.
    pub fn wrist() -> Self {
        Self {
            pixmap: Pixmap::new(WRIST_WIDTH, WRIST_HEIGHT).expect("wrist menu size"),
            wrist: true,
            ..Self::new()
        }
    }

    pub fn set_state(&mut self, state: &UiState) {
        if self.state.as_ref() == Some(state) {
            return;
        }
        let mut items = if self.wrist {
            self.build_wrist(state)
        } else {
            self.build(state)
        };
        self.fit(&mut items);
        self.items = items;
        self.state = Some(state.clone());
        self.hovered = None;
        self.dirty = true;
    }

    /// The picture as last drawn (premultiplied RGBA, top row first).
    pub fn pixels(&self) -> &[u8] {
        self.pixmap.data()
    }

    /// The part of the picture the last [`Panel::render_if_dirty`] changed: `None` for
    /// all of it (the items changed), else the parts of the button pointed at before and
    /// of the one pointed at now (empty when neither changes how it looks).
    pub fn damage(&self) -> Option<Area> {
        self.damage
    }

    /// Updates the hovered button from a point in panel pixels (`None`: not pointing at the panel).
    pub fn hover(&mut self, point: Option<(f32, f32)>) {
        let hovered = point.and_then(|(x, y)| {
            // A note lies over the buttons below its own one; one inside its own tile
            // leaves the press to the tile.
            let on_note = self.items.iter().rposition(|item| {
                matches!(item, Item::Note { rect, inside: false, .. }
                    if x >= rect.left() && x <= rect.right() && y >= rect.top() && y <= rect.bottom())
            });
            on_note.or_else(|| self.items.iter().position(|item| match item {
                Item::Button {
                    rect,
                    action: Some(_),
                    style,
                    ..
                } => {
                    *style != Style::Disabled
                        && x >= rect.left()
                        && x <= rect.right()
                        && y >= rect.top()
                        && y <= rect.bottom()
                }
                Item::Wedge {
                    action: Some(_),
                    style,
                    ..
                } => *style != Style::Disabled && item.wedge_contains(x, y),
                _ => false,
            }))
        });
        self.hovered = hovered;
    }

    pub fn hovered_action(&self) -> Option<Action> {
        match self.items.get(self.hovered?)? {
            Item::Button { action, .. } | Item::Wedge { action, .. } => *action,
            Item::Note { .. } => Some(Action::HideNote),
            _ => None,
        }
    }

    /// The hovered button is pressed: its action. Where it is, is kept for the note
    /// about it (see [`Panel::pressed`]).
    pub fn press(&mut self) -> Option<Action> {
        let action = self.hovered_action()?;
        if let Some(Item::Button { rect, .. }) = self.hovered.and_then(|i| self.items.get(i)) {
            self.pressed = Some((action, *rect, self.page));
        }
        Some(action)
    }

    /// Labels and texts that do not fit: a button label wider than its button, text
    /// running off the panel or into another text or button. Empty when the layout is
    /// clean; checked by the tests for every page in both languages.
    pub fn overflows(&mut self) -> Vec<String> {
        let items = std::mem::take(&mut self.items);
        let (width, height) = (self.pixmap.width() as f32, self.pixmap.height() as f32);
        let mut boxes: Vec<(Rect, String, bool)> = Vec::new(); // (bounds, text, is_button)
        // Buttons that ask to confirm: they take the place of others, never cover any.
        let mut armed: Vec<(Rect, String)> = Vec::new();
        let mut found = Vec::new();
        for item in &items {
            match item {
                Item::Wedge {
                    r0,
                    r1,
                    a0,
                    a1,
                    label,
                    size,
                    ..
                } => {
                    found.extend(self.missing_glyphs(label));
                    let tw = self.text_width(label, *size, true);
                    // Room for the label: the arc through the middle, or the circle.
                    let room = if *r0 <= 0.0 {
                        2.0 * r1 * 0.8
                    } else {
                        (r0 + r1) / 2.0 * (a1 - a0) * 0.85
                    };
                    if tw > room {
                        found.push(format!("wedge too narrow: {label:?} ({tw:.0} > {room:.0})"));
                    }
                }
                Item::Button {
                    rect: r,
                    label,
                    size,
                    style,
                    ..
                } => {
                    if *style == Style::Armed {
                        armed.push((*r, label.clone()));
                    }
                    found.extend(self.missing_glyphs(label));
                    if r.bottom() > height - 36.0 {
                        found.push(format!("below the bottom: button {label:?}"));
                    }
                    let tw = self.text_width(label, *size, true);
                    if tw > r.width() - 16.0 {
                        found.push(format!(
                            "button too narrow: {label:?} ({tw:.0} > {:.0})",
                            r.width() - 16.0
                        ));
                    }
                    // An empty label: a tile, whose own texts are drawn on top of it.
                    if !label.is_empty() {
                        boxes.push((*r, label.clone(), true));
                    }
                }
                Item::Text {
                    x,
                    baseline,
                    size,
                    medium,
                    text,
                    room,
                    ..
                } => {
                    found.extend(self.missing_glyphs(text));
                    let tw = self.text_width(text, *size, *medium);
                    if tw > *room + 0.5 {
                        found.push(format!(
                            "wider than its room: {text:?} ({tw:.0} > {room:.0})"
                        ));
                    }
                    // Below the error line (which itself sits at HEIGHT - 34).
                    if *baseline > height - 34.0 {
                        found.push(format!("below the bottom: {text:?}"));
                    }
                    if x + tw > width - 32.0 {
                        found.push(format!("off the panel: {text:?}"));
                    }
                    let r = rect(*x, baseline - size * 0.75, tw, size * 0.95);
                    boxes.push((r, text.clone(), false));
                }
                // Over the page on purpose: only itself is checked.
                Item::Note {
                    rect: r,
                    text,
                    size,
                    detail,
                    detail_size,
                    ..
                } => {
                    found.extend(self.missing_glyphs(text));
                    let room = r.width() - 2.0 * NOTE_PAD - NOTE_CLOSE;
                    let tw = self.text_width(text, *size, true);
                    if tw > room + 0.5 {
                        found.push(format!("note wider than its box: {text:?}"));
                    }
                    if let Some(detail) = detail {
                        found.extend(self.missing_glyphs(detail));
                        if self.text_width(detail, *detail_size, false) > room + 0.5 {
                            found.push(format!("note detail wider than its box: {detail:?}"));
                        }
                    }
                    if r.left() < 16.0 || r.right() > width - 16.0 {
                        found.push(format!("note off the panel: {text:?}"));
                    }
                    if r.top() < NOTE_TOP || r.bottom() > height - 12.0 {
                        found.push(format!("note over the tabs or off the panel: {text:?}"));
                    }
                }
                _ => {}
            }
        }
        found.extend(self.under_the_note(&items));
        for (a, ta) in &armed {
            for (b, tb, _) in &boxes {
                if (b != a || tb != ta) && overlap(a, b) {
                    found.push(format!("the confirmation {ta:?} covers {tb:?}"));
                }
            }
        }
        for (i, (a, ta, a_button)) in boxes.iter().enumerate() {
            for (b, tb, b_button) in &boxes[i + 1..] {
                if (*a_button && *b_button) || !overlap(a, b) {
                    continue;
                }
                found.push(format!("{ta:?} overlaps {tb:?}"));
            }
        }
        self.items = items;
        found
    }

    /// What a note over the page (not one inside its own tile) leaves half covered: a
    /// button or a text it covers only in part (peeking out from under it), and the
    /// error line it covers at all.
    fn under_the_note(&mut self, items: &[Item]) -> Vec<String> {
        let height = self.pixmap.height() as f32;
        let mut found = Vec::new();
        let Some(note) = items.iter().find_map(|i| match i {
            Item::Note {
                rect,
                inside: false,
                ..
            } => Some(*rect),
            _ => None,
        }) else {
            return found;
        };
        let inside = |c: &Rect| {
            c.left() >= note.left() - 0.5
                && c.right() <= note.right() + 0.5
                && c.top() >= note.top() - 0.5
                && c.bottom() <= note.bottom() + 0.5
        };
        for item in items {
            let (c, what) = match item {
                Item::Button { rect, label, .. } => (*rect, label.clone()),
                Item::Text {
                    x,
                    baseline,
                    size,
                    medium,
                    text,
                    ..
                } => {
                    let tw = self.text_width(text, *size, *medium);
                    let c = rect(*x, baseline - size * 0.75, tw.max(1.0), size * 0.95);
                    if *baseline > height - 40.0 && overlap(&c, &note) {
                        found.push(format!("the note covers the error line {text:?}"));
                    }
                    (c, text.clone())
                }
                _ => continue,
            };
            if overlap(&c, &note) && !inside(&c) {
                found.push(format!("{what:?} peeks out from under the note"));
            }
        }
        found
    }

    /// Texts and labels longer than their room (a language with longer words than
    /// those the layout was made with) drawn smaller, down to [`SMALLEST`], so they
    /// stay on their button and off their neighbours. What fits stays as it is.
    fn fit(&mut self, items: &mut [Item]) {
        let width = self.pixmap.width() as f32;
        for item in items.iter_mut() {
            match item {
                Item::Button {
                    rect,
                    label,
                    size,
                    full,
                    ..
                } => {
                    let room = rect.width() - 16.0;
                    *size = self.fitting(label, *size, true, room);
                    if full.is_some() {
                        self.shorten(label, *size, true, room);
                    }
                }
                Item::Text {
                    x,
                    size,
                    medium,
                    text,
                    room,
                    shorten,
                    ..
                } => {
                    let room = room.min(width - 32.0 - *x);
                    *size = self.fitting(text, *size, *medium, room);
                    if *shorten {
                        self.shorten(text, *size, *medium, room);
                    }
                }
                // As big as fits, then shortened: a reason or the PC's detail can be
                // longer than the panel is wide.
                Item::Note {
                    rect,
                    text,
                    size,
                    detail,
                    detail_size,
                    ..
                } => {
                    let room = rect.width() - 2.0 * NOTE_PAD - NOTE_CLOSE;
                    *size = self.fitting(text, *size, true, room);
                    self.shorten(text, *size, true, room);
                    if let Some(detail) = detail {
                        *detail_size = self.fitting(detail, *detail_size, false, room);
                        self.shorten(detail, *detail_size, false, room);
                    }
                }
                _ => {}
            }
        }
    }

    /// The size up to `size` at which `s` is at most `room` wide, but not below
    /// [`SMALLEST`].
    fn fitting(&mut self, s: &str, size: f32, medium: bool, room: f32) -> f32 {
        let mut size = size;
        while size > SMALLEST && self.text_width(s, size, medium) > room {
            size = (size - 1.0).max(SMALLEST);
        }
        size
    }

    fn missing_glyphs(&self, text: &str) -> Option<String> {
        let missing: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && self.regular.lookup_glyph_index(*c) == 0)
            .collect();
        (!missing.is_empty()).then(|| format!("no glyph for {missing:?} in {text:?}"))
    }

    /// The palm menu: a round menu of what is needed without opening the main one.
    /// Six sectors around a round button in the middle (clock, date, battery).
    fn build_wrist(&mut self, s: &UiState) -> Vec<Item> {
        use std::f32::consts::TAU;
        let l = &s.texts;
        let st = s.settings;
        let (w, h) = (WRIST_WIDTH as f32, WRIST_HEIGHT as f32);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let outer = w / 2.0 - 4.0;
        let mut items = vec![Item::Dot {
            x: cx,
            y: cy,
            r: outer,
            color: [16, 18, 24, 235],
        }];
        let sectors = [
            (l.t("headset-wrist-in-front"), false, Action::Recenter),
            (l.t("headset-focus"), st.focus, Action::Focus(!st.focus)),
            (l.t("headset-cinema"), st.cinema, Action::Cinema(!st.cinema)),
            (l.t("headset-wrist-mouse"), false, Action::FindMouse),
            (l.t("headset-wrist-screenshot"), false, Action::Screenshot),
            (l.t("headset-line-up"), false, Action::Tidy),
        ];
        let (r0, r1) = (outer * 0.4, outer - 10.0);
        let step = TAU / sectors.len() as f32;
        let gap = 0.035;
        for (i, (label, on, action)) in sectors.into_iter().enumerate() {
            // The first sector straight up.
            let mid = i as f32 * step;
            let (a0, a1) = (mid - step / 2.0 + gap, mid + step / 2.0 - gap);
            let (a0, a1) = if a0 < 0.0 {
                (a0 + TAU, a1 + TAU)
            } else {
                (a0, a1)
            };
            // As big as fits.
            let room = (r0 + r1) / 2.0 * (a1 - a0) * 0.85;
            let mut size = 28.0;
            while size > 16.0 && self.text_width(label, size, true) > room {
                size -= 1.0;
            }
            items.push(Item::Wedge {
                cx,
                cy,
                r0,
                r1,
                a0,
                a1,
                label: label.to_owned(),
                size,
                style: if on { Style::Selected } else { Style::Normal },
                action: Some(action),
            });
        }
        // The middle: the time, and the main menu under it (the whole circle is the
        // button).
        items.push(Item::Wedge {
            cx,
            cy,
            r0: 0.0,
            r1: r0 - 12.0,
            a0: 0.0,
            a1: TAU,
            label: String::new(),
            size: 28.0,
            style: Style::Normal,
            action: Some(Action::OpenMenu),
        });
        let (date, time, menu) = (18.0, 42.0, 18.0);
        // In the middle, as big as fits across the circle at that height (`room`).
        let centred = |this: &mut Self, s: &str, size: f32, medium: bool, room: f32| {
            let size = this.fitting(s, size, medium, room);
            let x = cx - this.text_width(s, size, medium) / 2.0;
            (x, size)
        };
        // Across the middle, `above` (or below) it, inside the sectors around it.
        let inner = r0 - 12.0;
        let free = r0 - 4.0;
        let across = |above: f32| 2.0 * (free * free - above * above).max(0.0).sqrt();
        // The battery line: measured at the middle of its letters (its digits reach a
        // little higher, still clear of the sectors).
        if let Some((percent, charging)) = s.battery {
            let id = if charging {
                "headset-battery-charging"
            } else {
                "headset-battery-level"
            };
            let battery = l.f(id, &[("percent", percent.into())]);
            let (x, size) = centred(self, &battery, 15.0, false, across(56.0));
            let color = if percent <= 15 && !charging {
                DANGER
            } else {
                MUTED
            };
            items.push(text_in(
                across(56.0),
                text(x, cy - 52.0, size, false, color, &battery),
            ));
        }
        let (x, date) = centred(self, &s.date, date, false, across(40.0));
        items.push(text_in(
            across(40.0),
            text(x, cy - 26.0, date, false, MUTED, &s.date),
        ));
        // "12:05 PM" is wider than "14:05": smaller, to stay in the circle.
        let mut time = time;
        while time > 22.0 && self.text_width(&s.clock, time, true) > 1.6 * inner {
            time -= 1.0;
        }
        let (x, time) = centred(self, &s.clock, time, true, 1.6 * inner);
        items.push(text(
            x,
            cy + 16.0,
            time,
            true,
            [255, 255, 255, 255],
            &s.clock,
        ));
        let label = l.t("headset-menu");
        let (x, menu) = centred(self, label, menu, false, across(50.0));
        items.push(text_in(
            across(50.0),
            text(x, cy + 46.0, menu, false, MUTED, label),
        ));
        items
    }

    /// Premultiplied RGBA, top row first, when it changed since the last call; what
    /// changed is [`Panel::damage`]. New items are drawn whole, nothing pointed at;
    /// then only the button pointed at before and the one pointed at now are drawn
    /// again, each from the copies of its part kept the first time it was pointed at
    /// (see [`LitPart`]): a ray trembling on a button's edge copies a few kilobytes,
    /// without drawing and without taking memory.
    pub fn render_if_dirty(&mut self) -> Option<&[u8]> {
        if !self.dirty && self.hovered == self.lit {
            return None;
        }
        let mut damage = if self.dirty {
            self.dirty = false;
            self.render();
            self.lit = None;
            self.lit_parts.clear();
            self.lit_pixels.clear();
            None
        } else {
            Some(Area::EMPTY)
        };
        if self.hovered != self.lit {
            if let Some(i) = self.lit
                && let Some(part) = self.lit_parts.iter().find(|p| p.item == i)
            {
                let (area, at) = (part.area, part.plain);
                copy_in(&mut self.pixmap, area, &self.lit_pixels[at..]);
                damage = damage.map(|d| d.union(area));
            }
            if let Some(area) = self.hovered.and_then(|i| self.light(i)) {
                damage = damage.map(|d| d.union(area));
            }
            self.lit = self.hovered;
        }
        self.damage = damage;
        Some(self.pixmap.data())
    }

    /// Item `i` drawn pointed at, over the picture drawn as not pointed at: its part
    /// drawn the first time and kept, copied back after. Where, if anywhere.
    fn light(&mut self, i: usize) -> Option<Area> {
        if let Some(part) = self.lit_parts.iter().find(|p| p.item == i) {
            let (area, at) = (part.area, part.lit);
            copy_in(&mut self.pixmap, area, &self.lit_pixels[at..]);
            return Some(area);
        }
        let area = self.lit_area(i)?;
        let plain = self.lit_pixels.len();
        copy_out(&self.pixmap, area, &mut self.lit_pixels);
        let lit = self.lit_pixels.len();
        self.draw_part(area, Some(i));
        self.lit_pixels.extend_from_slice(&self.scratch);
        self.lit_parts.push(LitPart {
            item: i,
            area,
            plain,
            lit,
        });
        copy_in(&mut self.pixmap, area, &self.lit_pixels[lit..]);
        Some(area)
    }

    /// The part of the picture item `i` changes when pointed at: its own place, and the
    /// whole name shown above a shortened one; none for what is not pointed at.
    fn lit_area(&mut self, i: usize) -> Option<Area> {
        let r = match self.items.get(i)? {
            Item::Button {
                rect: r,
                label,
                full: Some(full),
                ..
            } if full != label => {
                let (r, full) = (*r, full.clone());
                let (tip, _) = self.tip_rect(r, &full);
                union(&r, &tip)
            }
            Item::Button { rect: r, .. } => *r,
            Item::Wedge {
                cx,
                cy,
                r0,
                r1,
                a0,
                a1,
                ..
            } => sector_bounds(*cx, *cy, *r0, *r1, *a0, *a1),
            Item::Note { rect: r, tip, .. } => match tip {
                Some(_) => rect(
                    r.left(),
                    r.top() - NOTE_GAP,
                    r.width(),
                    r.height() + 2.0 * NOTE_GAP,
                ),
                None => *r,
            },
            _ => return None,
        };
        Area::around(r, LIT_MARGIN, self.pixmap.width(), self.pixmap.height())
    }

    /// `area` of the picture drawn anew into `scratch` (its rows one after another),
    /// with item `lit` pointed at. Drawn a little larger and cut, so the edges of the
    /// part are what drawing the whole picture makes there.
    fn draw_part(&mut self, area: Area, lit: Option<usize>) {
        let (pw, ph) = (self.pixmap.width(), self.pixmap.height());
        let outer = area.grown(PART_PAD, pw, ph);
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.resize(outer.len(), 0);
        let items = std::mem::take(&mut self.items);
        if let Some(pm) = PixmapMut::from_bytes(&mut scratch, outer.w, outer.h) {
            let mut c = Canvas {
                pm,
                at: (outer.x as i32, outer.y as i32),
            };
            self.paint(&mut c, &items, lit);
        }
        self.items = items;
        // The inner part's rows, moved to the front.
        let row = outer.w as usize * 4;
        let (dx, dy) = ((area.x - outer.x) as usize * 4, (area.y - outer.y) as usize);
        let inner = area.w as usize * 4;
        for y in 0..area.h as usize {
            let from = (dy + y) * row + dx;
            scratch.copy_within(from..from + inner, y * inner);
        }
        scratch.truncate(area.len());
        self.scratch = scratch;
    }

    /// The page `s` shows, and the note over it.
    fn build(&mut self, s: &UiState) -> Vec<Item> {
        self.page = Page::of(s);
        let mut items = self.build_page(s);
        if let Some(note) = &s.note {
            let item = self.note_item(note, &items, s.error.is_some());
            items.push(item);
        }
        items
    }

    /// Where the note about button `on` goes: under the button where it was pressed,
    /// on this page; else under the button of the page that stands for `on` (the same
    /// action, then the same switch); none where the page has no such button.
    fn note_anchor(&self, on: Action, items: &[Item]) -> Option<Rect> {
        if let Some((pressed, at, page)) = self.pressed
            && page == self.page
            && pressed.same_control(on)
        {
            return Some(at);
        }
        let find = |same: &dyn Fn(Action) -> bool| {
            items.iter().find_map(|item| match item {
                Item::Button {
                    rect,
                    key: Some(key),
                    ..
                } if same(*key) => Some(*rect),
                _ => None,
            })
        };
        find(&|key| key == on).or_else(|| find(&|key| key.same_control(on)))
    }

    /// `note` as drawn, never over another control: right under its button (above it
    /// where the space below is taken), with its point at the button; else in the
    /// nearest free space of the button's column, then of another column (without a
    /// point); inside the lower part of a tall button (a tile, whose state line it
    /// covers) where it fits across it; on the bottom line (above the error line, when
    /// there is one), else under the tabs, else the lowest free place, for a wait and
    /// where the page has no button for it. As wide as its text and at least as the
    /// button, within the panel under the tabs. Only on a page with no free space at
    /// all does it go over what lies there, grown over whatever it would cut through
    /// (see [`Panel::covering`]). Its texts are made to fit by [`Panel::fit`].
    fn note_item(&mut self, note: &Note, items: &[Item], error: bool) -> Item {
        let (pw, ph) = (WIDTH as f32, HEIGHT as f32);
        let text_w = self.text_width(&note.text, NOTE_SIZE, true);
        let detail_w = note
            .detail
            .as_deref()
            .map_or(0.0, |d| self.text_width(d, NOTE_DETAIL_SIZE, false));
        let h = if note.detail.is_some() { 66.0 } else { 46.0 };
        // A little more than the text and its "×", which then never measure wider than
        // their room.
        let wanted = text_w.max(detail_w) + 2.0 * NOTE_PAD + NOTE_CLOSE + 2.0;
        let widest = pw - 64.0;
        // The lowest it reaches: above the error line, when there is one.
        let lowest = if error { ph - 66.0 } else { ph - 24.0 };
        let anchor = match note.kind {
            Kind::Waiting => None,
            _ => note.on.and_then(|on| self.note_anchor(on, items)),
        };
        // What lies on the page, made once (it measures every text) and only when a
        // place is looked for.
        let mut taken: Option<Vec<Rect>> = None;
        let placed = anchor.and_then(|b| {
            // A tile: room enough under its title, for a text that fits across it.
            if b.height() >= 2.0 * h + 20.0 && wanted <= b.width() - 20.0 {
                let w = (b.width() - 20.0).min(widest);
                let r = rect(b.left() + 10.0, b.bottom() - 10.0 - h, w, h);
                return Some((r, None, false, true));
            }
            let taken = taken.insert(self.taken(items));
            self.free_by(b, wanted, h, taken, lowest)
                .map(|(r, tip, above)| (r, tip, above, false))
        });
        let (rect_, tip, above, inside) = match placed {
            Some(placed) => placed,
            // On the bottom line where it is free, else under the tabs; else over
            // what lies on the bottom line, grown over what it would cut through, or
            // under the tabs where the bottom of the page is too full for that.
            None => {
                let w = wanted.min(widest);
                let x = (pw - w) / 2.0;
                let bottom = rect(x, lowest - h, w, h);
                let top = rect(x, NOTE_TOP, w, h);
                let taken = match taken {
                    Some(taken) => taken,
                    None => self.taken(items),
                };
                let free = |r: &Rect| !taken.iter().any(|t| overlap(r, t));
                let r = match [bottom, top]
                    .into_iter()
                    .find(free)
                    .or_else(|| self.free_low(wanted, h, &taken, lowest))
                {
                    Some(r) => r,
                    None => {
                        let nothing = rect(0.0, ph, pw, 1.0);
                        let whole = (44.0, pw - 44.0);
                        [bottom, top]
                            .into_iter()
                            .find_map(|r| self.covering(r, nothing, false, items, whole, lowest))
                            .unwrap_or(bottom)
                    }
                };
                (r, None, false, false)
            }
        };
        Item::Note {
            rect: rect_,
            tip,
            above,
            inside,
            kind: note.kind,
            text: note.text.to_string(),
            size: NOTE_SIZE,
            detail: note.detail.as_deref().map(str::to_owned),
            detail_size: NOTE_DETAIL_SIZE,
        }
    }

    /// Where the page has something a note must not cover: its buttons, texts (as
    /// wide as they measure) and boxes, under the tabs, each with [`NOTE_CLEAR`]
    /// around it.
    fn taken(&mut self, items: &[Item]) -> Vec<Rect> {
        let mut taken = Vec::new();
        for item in items {
            let r = match item {
                Item::Button { rect, .. } | Item::Box(rect, _) => *rect,
                Item::Text {
                    x,
                    baseline,
                    size,
                    medium,
                    text,
                    ..
                } => {
                    let tw = self.text_width(text, *size, *medium);
                    rect(*x, baseline - size * 0.8, tw.max(1.0), size * 1.05)
                }
                _ => continue,
            };
            if r.bottom() > NOTE_TOP {
                let c = NOTE_CLEAR;
                taken.push(rect(
                    r.left() - c,
                    r.top() - c,
                    r.width() + 2.0 * c,
                    r.height() + 2.0 * c,
                ));
            }
        }
        taken
    }

    /// A free place (one that overlaps nothing `taken`) for a note `wanted` wide and
    /// `h` high about button `b`: right under it, else right above it (with its point
    /// at the button, and whether above); else the nearest free place in the button's
    /// column, then in another column wide enough (no point). Under the tabs and down
    /// to `lowest`; none where the page has no free place.
    fn free_by(
        &self,
        b: Rect,
        wanted: f32,
        h: f32,
        taken: &[Rect],
        lowest: f32,
    ) -> Option<(Rect, Option<f32>, bool)> {
        let free = |r: &Rect| !taken.iter().any(|t| overlap(r, t));
        let (cl, cr) = self.column(b);
        let w = wanted.max(b.width()).min(cr - cl);
        let mid = b.left() + b.width() / 2.0;
        let x = (mid - w / 2.0).clamp(cl, cr - w);
        let fits = |y: f32| y >= NOTE_TOP && y + h <= lowest;
        let (below, above) = (b.bottom() + NOTE_GAP, b.top() - NOTE_GAP - h);
        for (y, up) in [(below, false), (above, true)] {
            let r = rect(x, y, w, h);
            if fits(y) && free(&r) {
                let tip = mid.clamp(r.left() + 18.0, r.right() - 18.0);
                return Some((r, Some(tip), up));
            }
        }
        // The nearest free place, down or up from the button in 2-pixel steps: across
        // its middle, else to one side of the column.
        let nearest = |xs: [f32; 3], w: f32, from: f32| {
            let steps = ((lowest - NOTE_TOP) / 2.0).ceil() as usize + 1;
            (0..=steps).find_map(|i| {
                let d = i as f32 * 2.0;
                [from + d, from - d]
                    .into_iter()
                    .filter(|y| fits(*y))
                    .flat_map(|y| xs.map(|x| rect(x, y, w, h)))
                    .find(free)
            })
        };
        if let Some(r) = nearest([x, cl, cr - w], w, below) {
            return Some((r, None, false));
        }
        self.columns()
            .into_iter()
            .filter(|&(l, r)| (l, r) != (cl, cr) && r - l >= wanted.min(cr - cl))
            .find_map(|(l, r)| {
                let w = wanted.max(b.width()).min(r - l);
                nearest([l + (r - l - w) / 2.0, l, r - w], w, b.top())
            })
            .map(|r| (r, None, false))
    }

    /// The lowest free place on the page for a note `wanted` wide (narrower to fit a
    /// column) and `h` high, in any column: none where the page has none.
    fn free_low(&self, wanted: f32, h: f32, taken: &[Rect], lowest: f32) -> Option<Rect> {
        let free = |r: &Rect| !taken.iter().any(|t| overlap(r, t));
        let columns: Vec<(f32, f32)> = self
            .columns()
            .into_iter()
            .filter(|&(l, r)| r - l >= wanted.min(WIDTH as f32 - 88.0) * 0.5)
            .collect();
        let mut y = lowest - h;
        while y >= NOTE_TOP {
            for &(l, r) in &columns {
                let w = wanted.min(r - l);
                let found = [l + (r - l - w) / 2.0, l, r - w]
                    .into_iter()
                    .map(|x| rect(x, y, w, h))
                    .find(free);
                if found.is_some() {
                    return found;
                }
            }
            y -= 2.0;
        }
        None
    }

    /// The columns of the page: left and right edges.
    fn columns(&self) -> Vec<(f32, f32)> {
        let pw = WIDTH as f32;
        match self.page {
            Page::Tab(Tab::Monitors) | Page::Welcome => vec![(44.0, 624.0), (680.0, pw - 44.0)],
            Page::Tab(Tab::Settings(_)) => vec![(44.0, 274.0), (324.0, pw - 44.0)],
            _ => vec![(44.0, pw - 44.0)],
        }
    }

    /// The column of the page button `b` is in: left and right edge.
    fn column(&self, b: Rect) -> (f32, f32) {
        let pw = WIDTH as f32;
        match self.page {
            // Two columns: the monitors' map and their options; the welcome and the
            // PCs found.
            Page::Tab(Tab::Monitors) | Page::Welcome if b.right() <= 640.0 => (44.0, 624.0),
            Page::Tab(Tab::Monitors) | Page::Welcome => (680.0, pw - 44.0),
            // The list of parts, and the part shown.
            Page::Tab(Tab::Settings(_)) if b.right() <= 300.0 => (44.0, 274.0),
            Page::Tab(Tab::Settings(_)) => (324.0, pw - 44.0),
            _ => (44.0, pw - 44.0),
        }
    }

    /// Note box `r`, below its button `b` (or `above` it), made so that nothing peeks
    /// out from under it: grown over each button and text of its column (`cl` to `cr`)
    /// it covers only in part, up and down and to the sides; a title goes with the
    /// buttons right under it, so a row is never left without its title. Above its
    /// button, it stays above the button's own title. Under the tabs and down to
    /// `lowest`; none where it does not fit there, or would cover its own button.
    fn covering(
        &mut self,
        r: Rect,
        b: Rect,
        above: bool,
        items: &[Item],
        (cl, cr): (f32, f32),
        lowest: f32,
    ) -> Option<Rect> {
        // What lies in the column: buttons, and texts as wide as they measure.
        let mut buttons = Vec::new();
        let mut texts = Vec::new();
        for item in items {
            match item {
                Item::Button { rect: c, .. } if *c != b => buttons.push(*c),
                Item::Text {
                    x,
                    baseline,
                    size,
                    medium,
                    text,
                    ..
                } => {
                    let tw = self.text_width(text, *size, *medium);
                    texts.push(rect(*x, baseline - size * 0.8, tw.max(1.0), size * 1.05));
                }
                _ => {}
            }
        }
        // A text with buttons starting right under it, from its left edge on: their
        // title, covered with them or not at all.
        let row_under = |t: &Rect, c: &Rect| {
            c.top() >= t.bottom() - 2.0
                && c.top() <= t.bottom() + 34.0
                && c.left() >= t.left() - 4.0
                && c.left() < t.left() + 720.0
        };
        let in_column = |c: &Rect| c.left() >= cl - 1.0 && c.right() <= cr + 1.0;
        buttons.retain(in_column);
        texts.retain(in_column);
        let mut units: Vec<Rect> = buttons.iter().chain(&texts).copied().collect();
        for t in &texts {
            let group = buttons
                .iter()
                .filter(|c| row_under(t, c))
                .fold(None, |u: Option<Rect>, c| {
                    Some(u.map_or(*c, |u| union(&u, c)))
                });
            if let Some(group) = group {
                units.push(union(t, &group));
            }
        }
        let mut r = r;
        if above {
            // The button's own title stays readable: the note goes above it.
            let own = texts
                .iter()
                .filter(|t| row_under(t, &b))
                .map(Rect::top)
                .fold(f32::MAX, f32::min);
            if own < f32::MAX && r.bottom() > own - NOTE_GAP {
                let h = r.height();
                r = rect(r.left(), own - NOTE_GAP - h, r.width(), h);
            }
        }
        // Growing can reach the next thing: a few passes settle it.
        for _ in 0..6 {
            let mut grown = r;
            for u in &units {
                // What its own button is part of stays as it is.
                if overlap(u, &b) || !overlap(&grown, u) {
                    continue;
                }
                grown = union(&grown, u);
            }
            if grown == r {
                break;
            }
            r = grown;
        }
        // Within the column, under the tabs, above the error line, off its button.
        let fits = r.top() >= NOTE_TOP
            && r.bottom() <= lowest
            && r.left() >= cl - 1.0
            && r.right() <= cr + 1.0;
        (fits && !overlap(&r, &b)).then_some(r)
    }

    fn build_page(&mut self, s: &UiState) -> Vec<Item> {
        let mut items = vec![Item::Panel(rect(0.0, 0.0, WIDTH as f32, HEIGHT as f32))];
        self.build_body(s, &mut items);
        // The error line, on every page (each leaves room for it): what went wrong
        // shows whatever the page, pairing and the first screens too.
        if let Some(e) = &s.error {
            items.push(text(44.0, HEIGHT as f32 - 34.0, 21.0, true, DANGER, e));
        }
        items
    }

    /// The page under the error line: the header, then the page itself.
    fn build_body(&mut self, s: &UiState, items: &mut Vec<Item>) {
        let l = &s.texts;
        let w = WIDTH as f32;

        // Header.
        items.push(text(44.0, 70.0, 34.0, true, TEXT, "Prostir"));
        let title_w = self.text_width("Prostir", 34.0, true);
        items.push(text(
            44.0 + title_w + 12.0,
            70.0,
            18.0,
            false,
            MUTED,
            &prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
        ));
        let (status, light) = match &s.host {
            _ if s.update.is_some() => (l.t("headset-status-update-needed").to_owned(), DANGER),
            _ if s.pairing.is_some() => (l.t("headset-status-pairing").to_owned(), WAIT),
            Some(h) => (l.f("headset-status-connected", &[("pc", h.into())]), GOOD),
            None => (l.t("headset-status-searching").to_owned(), WAIT),
        };
        let sw = self.text_width(&status, 21.0, false);
        let change_w = if s.host.is_some() { 190.0 } else { 0.0 };
        let sx = w - 120.0 - change_w - sw;
        items.push(Item::Dot {
            x: sx - 16.0,
            y: 59.0,
            r: 6.0,
            color: light,
        });
        items.push(text(sx, 66.0, 21.0, false, MUTED, &status));
        if s.host.is_some() {
            items.push(button(
                rect(w - 98.0 - 170.0, 32.0, 170.0, 48.0),
                l.t("headset-change-pc"),
                Style::Normal,
                Some(Action::ChangeHost),
            ));
        }
        items.push(button(
            rect(w - 88.0, 28.0, 56.0, 56.0),
            "×",
            Style::Normal,
            Some(Action::Close),
        ));
        items.push(Item::Box(
            rect(32.0, 108.0, w - 64.0, 2.0),
            [255, 255, 255, 18],
        ));

        let (lx, lw) = (44.0, 580.0);
        let (rx, rw) = (680.0, 556.0);
        // Each page's texts stay in their column (see `in_column`).
        let at = items.len();
        // Pairing has the page to itself.
        let whole = w - 2.0 * lx;
        if let Some((host, why)) = &s.pair_offer {
            build_pair_offer(l, items, lx, lw, host, *why);
            in_column(&mut items[at..], lx, whole);
            return;
        }
        if let Some(p) = &s.pairing {
            self.build_pairing(l, p, items, lx, lw, 150.0);
            in_column(&mut items[at..], lx, whole);
            return;
        }
        if let Some((pc_newer, pc_app)) = &s.update {
            build_update(l, items, lx, whole, *pc_newer, pc_app, s.update_from);
            in_column(&mut items[at..], lx, whole);
            return;
        }
        if s.intro && s.host.is_some() {
            build_intro(l, items, lx, whole);
            in_column(&mut items[at..], lx, whole);
            return;
        }
        // No PC yet: how to get one, and the PCs found so far.
        if s.host.is_none() {
            build_welcome(l, items, lx, lw, 150.0);
            in_column(&mut items[at..], lx, lw);
            let right = items.len();
            self.build_connection(s, items, rx, rw, 150.0);
            in_column(&mut items[right..], rx, rw);
            return;
        }

        // Tabs: everyday things, the monitors, the profiles, everything set once.
        let tabs = [
            (Tab::Quick, l.t("headset-tab-quick")),
            (Tab::Monitors, l.t("headset-tab-monitors")),
            (Tab::Profiles, l.t("headset-tab-profiles")),
            (
                Tab::Settings(s.tab.section().unwrap_or_default()),
                l.t("headset-tab-settings"),
            ),
        ];
        let tab_w = (w - 2.0 * lx - 3.0 * 10.0) / 4.0;
        // A track the tabs sit in, so they read as one control, apart from the
        // option buttons below.
        items.push(Item::Box(
            rect(lx - 6.0, 122.0, w - 2.0 * lx + 12.0, 62.0),
            [255, 255, 255, 10],
        ));
        for (i, (tab, label)) in tabs.into_iter().enumerate() {
            let r = rect(lx + i as f32 * (tab_w + 10.0), 128.0, tab_w, 50.0);
            if s.tab.same_page(tab) {
                items.push(button(r, label, Style::TabOn, None));
            } else {
                items.push(button(r, label, Style::TabOff, Some(Action::Tab(tab))));
            }
        }

        let top = 230.0;
        let at = items.len();
        match s.tab {
            Tab::Quick => {
                build_quick(s, items, lx, whole, top - 12.0);
                in_column(&mut items[at..], lx, whole);
            }
            Tab::Monitors => {
                monitor_list(s, items, lx, lw, top - 12.0);
                in_column(&mut items[at..], lx, lw);
                let right = items.len();
                build_monitor_options(s, items, rx, rw, top);
                in_column(&mut items[right..], rx, rw);
            }
            Tab::Profiles => {
                let wrap = |t: &str, size: f32, room: f32| self.wrapped(t, size, false, room);
                build_profiles(s, items, top - 12.0, &wrap);
            }
            Tab::Settings(section) => {
                let wrap = |t: &str, size: f32, room: f32| self.wrapped(t, size, false, room);
                build_settings(s, section, items, top - 12.0, &wrap);
            }
        }
    }

    /// The code to type on the PC, or a keypad for the PC's code.
    fn build_pairing(
        &mut self,
        l: &Texts,
        p: &PairingView,
        items: &mut Vec<Item>,
        lx: f32,
        lw: f32,
        mut y: f32,
    ) -> f32 {
        items.push(text(
            lx,
            y,
            21.0,
            false,
            MUTED,
            &l.f("headset-pairing-with", &[("pc", (&p.host).into())]),
        ));
        y += 40.0;
        if p.failed {
            items.push(text(
                lx,
                y,
                20.0,
                true,
                DANGER,
                l.t("headset-pairing-failed"),
            ));
            y += 34.0;
        }
        if let Some(code) = &p.code {
            items.push(text(
                lx,
                y,
                20.0,
                false,
                TEXT,
                l.t("headset-pairing-type-on-pc"),
            ));
            y += 30.0;
            items.push(Item::Box(rect(lx, y, lw, 130.0), [255, 255, 255, 16]));
            let cw = self.text_width(code, 84.0, true);
            items.push(text(lx + (lw - cw) / 2.0, y + 98.0, 84.0, true, TEXT, code));
            y += 150.0;
            items.push(text(lx, y, 18.0, false, MUTED, l.t("headset-pairing-once")));
            y += 30.0;
            items.push(button(
                rect(lx, y, lw, 50.0),
                l.t("headset-pairing-other-way-type-here"),
                Style::Normal,
                Some(Action::PairOtherWay),
            ));
            return y + 62.0;
        }
        for line in [
            l.t("headset-pairing-type-pc-code"),
            l.t("headset-pairing-where-pc-code"),
        ] {
            items.push(text(lx, y, 20.0, false, TEXT, line));
            y += 30.0;
        }
        items.push(Item::Box(rect(lx, y, lw, 64.0), [255, 255, 255, 16]));
        let shown: String = (0..6)
            .map(|i| p.typed.chars().nth(i).unwrap_or('·'))
            .flat_map(|c| [c, ' '])
            .collect();
        let sw = self.text_width(shown.trim_end(), 40.0, true);
        items.push(text(
            lx + (lw - sw) / 2.0,
            y + 47.0,
            40.0,
            true,
            TEXT,
            shown.trim_end(),
        ));
        y += 76.0;
        let ready = p.typed.len() == 6;
        let erase = l.t("headset-key-delete");
        let done = l.t("headset-key-done");
        let keys = [
            "1", "2", "3", "4", "5", "6", "7", "8", "9", erase, "0", done,
        ];
        let (kw, kh) = ((lw - 20.0) / 3.0, 50.0);
        for (i, k) in keys.iter().enumerate() {
            let r = rect(
                lx + (i % 3) as f32 * (kw + 10.0),
                y + (i / 3) as f32 * (kh + 8.0),
                kw,
                kh,
            );
            let (style, action) = match i {
                9 => (Style::Normal, Some(Action::Backspace)),
                11 if ready => (Style::Selected, Some(Action::ConnectTyped)),
                11 => (Style::Disabled, None),
                _ => (Style::Normal, k.chars().next().map(Action::Key)),
            };
            items.push(button(r, k, style, action));
        }
        y += 4.0 * (kh + 8.0) + 4.0;
        items.push(button(
            rect(lx, y, lw, 50.0),
            l.t("headset-pairing-other-way-type-on-pc"),
            Style::Normal,
            Some(Action::PairOtherWay),
        ));
        y + 62.0
    }

    /// PCs found on the network and a keypad for typing an address.
    fn build_connection(
        &self,
        s: &UiState,
        items: &mut Vec<Item>,
        lx: f32,
        lw: f32,
        mut y: f32,
    ) -> f32 {
        let l = &s.texts;
        y += 18.0;
        if let Some(typed) = &s.ip_input {
            let shown = if typed.is_empty() {
                "192.168.1.10"
            } else {
                typed.as_str()
            };
            let color = if typed.is_empty() { MUTED } else { TEXT };
            items.push(Item::Box(rect(lx, y, lw, 56.0), [255, 255, 255, 16]));
            items.push(text(lx + 20.0, y + 38.0, 26.0, true, color, shown));
            y += 68.0;
            let keys = [
                "1",
                "2",
                "3",
                "4",
                "5",
                "6",
                "7",
                "8",
                "9",
                ".",
                "0",
                l.t("headset-key-delete"),
            ];
            let (kw, kh) = ((lw - 20.0) / 3.0, 50.0);
            for (i, k) in keys.iter().enumerate() {
                let r = rect(
                    lx + (i % 3) as f32 * (kw + 10.0),
                    y + (i / 3) as f32 * (kh + 8.0),
                    kw,
                    kh,
                );
                let action = match k.chars().next() {
                    Some(c) if k.len() == 1 => Action::Key(c),
                    _ => Action::Backspace,
                };
                items.push(button(r, k, Style::Normal, Some(action)));
            }
            y += 4.0 * (kh + 8.0) + 4.0;
            let half = (lw - 10.0) / 2.0;
            let ready = !typed.is_empty();
            items.push(button(
                rect(lx, y, half, 50.0),
                l.t("headset-connect"),
                if ready {
                    Style::Selected
                } else {
                    Style::Disabled
                },
                ready.then_some(Action::ConnectTyped),
            ));
            items.push(button(
                rect(lx + half + 10.0, y, half, 50.0),
                l.t("headset-cancel"),
                Style::Normal,
                Some(Action::CancelKeypad),
            ));
            return y + 62.0;
        }
        if s.found.is_empty() {
            items.push(text(
                lx,
                y + 20.0,
                21.0,
                true,
                MUTED,
                l.t("headset-connect-searching"),
            ));
            y += 48.0;
            if let Some(hint) = s.hint {
                let (title, body) = match hint {
                    crate::session::Hint::NewPc => {
                        ("headset-connect-new-title", "headset-connect-new-pair")
                    }
                    crate::session::Hint::Silent => (
                        "headset-connect-silent-title",
                        "headset-connect-silent-check",
                    ),
                };
                items.push(text(lx, y + 16.0, 21.0, true, TEXT, l.t(title)));
                y += 32.0;
                for body in [body, "headset-connect-or-type"] {
                    for line in self.wrapped(l.t(body), 19.0, false, lw) {
                        items.push(text(lx, y + 16.0, 19.0, false, MUTED, &line));
                        y += 27.0;
                    }
                    y += 6.0;
                }
            }
        } else {
            items.push(text(
                lx,
                y + 14.0,
                21.0,
                false,
                MUTED,
                l.t("headset-connect-found"),
            ));
            y += 30.0;
            for (i, (name, addr)) in s.found.iter().enumerate().take(5) {
                let label = format!("{name}   {addr}");
                items.push(button(
                    rect(lx, y, lw, 52.0),
                    &label,
                    Style::Normal,
                    Some(Action::ConnectTo(i)),
                ));
                y += 60.0;
            }
            if let Some(name) = &s.unreachable {
                // Seen on the network, but the connection itself is blocked.
                let name: String = name.chars().take(30).collect();
                for line in [
                    name,
                    l.t("headset-connect-blocked").to_owned(),
                    l.t("headset-connect-allow-port").to_owned(),
                ] {
                    items.push(text(lx, y + 16.0, 18.0, false, DANGER, &line));
                    y += 28.0;
                }
                y += 6.0;
            }
        }
        items.push(button(
            rect(lx, y + 6.0, 280.0, 48.0),
            l.t("headset-connect-type-ip"),
            Style::Normal,
            Some(Action::ShowKeypad),
        ));
        y += 66.0;
        // A cable needs developer mode and adb: not something a store install has.
        if s.found.is_empty() && s.hint.is_some() && !cfg!(feature = "store") {
            for line in self.wrapped(l.t("headset-connect-or-usb"), 19.0, false, lw) {
                items.push(text(lx, y + 16.0, 19.0, false, MUTED, &line));
                y += 27.0;
            }
        }
        y
    }

    /// `text` at `size` broken into lines at most `room` wide, between words (a word
    /// wider than `room` is a line of its own).
    fn wrapped(&self, text: &str, size: f32, medium: bool, room: f32) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        for word in text.split_whitespace() {
            let was = line.len();
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            if was > 0 && self.measured(&line, size, medium) > room {
                line.truncate(was);
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    fn glyph(&mut self, medium: bool, c: char, size: f32) -> &(Metrics, Vec<u8>) {
        let font = if medium { &self.medium } else { &self.regular };
        cached_glyph(&mut self.glyphs, font, medium, c, size)
    }

    /// `name`, at `size`, shortened with an ellipsis by whole letters where it is wider
    /// than `room` (see `prostir_lang::shortened`); nothing where not even a letter
    /// fits.
    fn shorten(&self, name: &mut String, size: f32, medium: bool, room: f32) {
        if self.measured(name, size, medium) > room {
            *name = prostir_lang::shortened(name, |s| self.measured(s, size, medium) <= room)
                .unwrap_or_default();
        }
    }

    /// [`Panel::text_width`] without the glyph cache, for a question asked many times
    /// over one text (how much of a name fits).
    fn measured(&self, s: &str, size: f32, medium: bool) -> f32 {
        let font = if medium { &self.medium } else { &self.regular };
        s.chars().map(|c| font.metrics(c, size).advance_width).sum()
    }

    fn text_width(&mut self, s: &str, size: f32, medium: bool) -> f32 {
        s.chars()
            .map(|c| self.glyph(medium, c, size).0.advance_width)
            .sum()
    }

    /// The whole picture drawn anew, nothing pointed at.
    fn render(&mut self) {
        let mut pixmap = std::mem::replace(
            &mut self.pixmap,
            Pixmap::new(1, 1).expect("1x1 is a valid size"),
        );
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let items = std::mem::take(&mut self.items);
        self.paint(&mut Canvas::whole(&mut pixmap), &items, None);
        self.items = items;
        self.pixmap = pixmap;
    }

    /// `items` drawn on `c`, with item `lit` pointed at. Shapes wholly outside `c`
    /// are passed over.
    fn paint(&mut self, c: &mut Canvas<'_>, items: &[Item], lit: Option<usize>) {
        for (i, item) in items.iter().enumerate() {
            if item.bounds().is_some_and(|b| !c.meets(b)) {
                continue;
            }
            match item {
                Item::Panel(r) => fill(c, rounded(*r, 36.0), [16, 18, 24, 240]),
                // Drawn last, over everything (see `Panel::draw_note`).
                Item::Note { .. } => {}
                Item::Box(r, color) => fill(c, rounded(*r, 14.0), *color),
                Item::Dot { x, y, r, color } => fill(c, circle(*x, *y, *r), *color),
                Item::Text {
                    x,
                    baseline,
                    size,
                    medium,
                    color,
                    text,
                    ..
                } => self.draw_text(c, *x, *baseline, *size, *medium, *color, text),
                Item::Wedge {
                    cx,
                    cy,
                    r0,
                    r1,
                    a0,
                    a1,
                    label,
                    size,
                    style,
                    ..
                } => {
                    let hovered = lit == Some(i);
                    let bg = match (style, hovered) {
                        (Style::Selected, _) => ACCENT,
                        (_, true) => [255, 255, 255, 52],
                        _ => [255, 255, 255, 20],
                    };
                    fill(c, sector(*cx, *cy, *r0, *r1, *a0, *a1), bg);
                    let (mx, my) = wedge_center(*cx, *cy, *r0, *r1, *a0, *a1);
                    let tw = self.text_width(label, *size, true);
                    self.draw_text(
                        c,
                        mx - tw / 2.0,
                        my + size * 0.36,
                        *size,
                        true,
                        [255, 255, 255, 255],
                        label,
                    );
                }
                Item::Button {
                    rect: r,
                    label,
                    size,
                    style,
                    ..
                } => {
                    let size = *size;
                    let hovered = lit == Some(i);
                    let (bg, fg) = match (style, hovered) {
                        (Style::Selected, _) => (ACCENT, [255, 255, 255, 255]),
                        (Style::TabOn, _) => ([255, 255, 255, 34], [255, 255, 255, 255]),
                        (Style::TabOff, true) => ([255, 255, 255, 22], [255, 255, 255, 255]),
                        (Style::TabOff, false) => ([0, 0, 0, 0], MUTED),
                        (Style::Armed, _) => ([220, 70, 70, 255], [255, 255, 255, 255]),
                        // Unavailable: an outline only, so it does not look pressable.
                        (Style::Disabled, _) => ([0, 0, 0, 0], [112, 118, 132, 255]),
                        (Style::Danger, true) => ([220, 70, 70, 255], [255, 255, 255, 255]),
                        (_, true) => ([255, 255, 255, 46], [255, 255, 255, 255]),
                        _ => ([255, 255, 255, 20], TEXT),
                    };
                    fill(c, rounded(*r, 12.0), bg);
                    match style {
                        Style::Disabled => {
                            stroke(c, rounded(*r, 12.0), [255, 255, 255, 26], 1.5);
                        }
                        // The current tab: an accent bar along its bottom edge. The
                        // primary monitor on the map can be narrower than a tab.
                        Style::TabOn => {
                            let inset = (r.width() / 4.0).min(28.0);
                            let bar = rect(
                                r.left() + inset,
                                r.bottom() - 6.0,
                                r.width() - 2.0 * inset,
                                4.0,
                            );
                            fill(c, rounded(bar, 2.0), ACCENT);
                        }
                        _ => {}
                    }
                    let tw = self.text_width(label, size, true);
                    let baseline = r.top() + r.height() / 2.0 + size * 0.36;
                    self.draw_text(
                        c,
                        r.left() + (r.width() - tw) / 2.0,
                        baseline,
                        size,
                        true,
                        fg,
                        label,
                    );
                }
            }
        }
        if let Some(Item::Note {
            rect: r,
            tip,
            above,
            kind,
            text,
            size,
            detail,
            detail_size,
            ..
        }) = items.iter().rev().find(|i| matches!(i, Item::Note { .. }))
        {
            let hovered = matches!(lit.and_then(|i| items.get(i)), Some(Item::Note { .. }));
            self.draw_note(
                c,
                (*r, *tip, *above, *kind, hovered),
                (text, *size),
                detail.as_deref().map(|d| (d, *detail_size)),
            );
        }
        // A name shortened on its button, pointed at: whole, above it.
        if let Some(Item::Button {
            rect: r,
            label,
            full: Some(full),
            ..
        }) = lit.and_then(|i| items.get(i))
            && label != full
        {
            self.draw_tip(c, *r, full);
        }
    }

    /// A note (see [`Item::Note`]): a dark box with an edge in its kind's colour, a
    /// point towards its button, its text in the middle and the detail under it, and
    /// a "×" on its right (pressing anywhere on it takes it away).
    fn draw_note(
        &mut self,
        c: &mut Canvas<'_>,
        (r, tip, above, kind, hovered): (Rect, Option<f32>, bool, Kind, bool),
        (text, size): (&str, f32),
        detail: Option<(&str, f32)>,
    ) {
        let (edge, fg) = match kind {
            Kind::Done => (GOOD, TEXT),
            Kind::Waiting => (WAIT, TEXT),
            Kind::Failed => (DANGER, DANGER),
            Kind::Info => (MUTED, TEXT),
        };
        if let Some(tx) = tip {
            let (base, point) = if above {
                (r.bottom() - 1.0, r.bottom() + NOTE_GAP - 1.0)
            } else {
                (r.top() + 1.0, r.top() - NOTE_GAP + 1.0)
            };
            let mut pb = PathBuilder::new();
            pb.move_to(tx - 10.0, base);
            pb.line_to(tx, point);
            pb.line_to(tx + 10.0, base);
            pb.close();
            fill(c, pb.finish(), edge);
        }
        let shape = rounded(r, 12.0);
        fill(c, shape.clone(), [26, 30, 40, 255]);
        let [er, eg, eb, _] = edge;
        let alpha = if hovered { 255 } else { 190 };
        stroke(c, shape, [er, eg, eb, alpha], 2.0);
        // In the middle of the box (which may have grown over what it covers), left
        // of the "×".
        let mid = r.top() + r.height() / 2.0;
        let close = if hovered { TEXT } else { MUTED };
        let cw = self.text_width("×", 26.0, true);
        let cx = r.right() - NOTE_PAD / 2.0 - NOTE_CLOSE / 2.0 - cw / 2.0;
        let cy = mid.min(r.top() + 23.0) + 8.0;
        self.draw_text(c, cx, cy, 26.0, true, close, "×");
        let (left, room) = (r.left() + NOTE_PAD, r.width() - 2.0 * NOTE_PAD - NOTE_CLOSE);
        let tw = self.text_width(text, size, true);
        let x = left + (room - tw) / 2.0;
        match detail {
            None => self.draw_text(c, x, mid + size * 0.36, size, true, fg, text),
            Some((detail, dsize)) => {
                self.draw_text(c, x, mid - 4.0, size, true, fg, text);
                let dw = self.text_width(detail, dsize, false);
                let dx = left + (room - dw) / 2.0;
                self.draw_text(c, dx, mid + dsize + 4.0, dsize, false, MUTED, detail);
            }
        }
    }

    /// The whole picture's size (the main menu's or the palm menu's).
    fn picture_size(&self) -> (f32, f32) {
        if self.wrist {
            (WRIST_WIDTH as f32, WRIST_HEIGHT as f32)
        } else {
            (WIDTH as f32, HEIGHT as f32)
        }
    }

    /// Where [`Panel::draw_tip`] puts `text` for `over`, and its size.
    fn tip_rect(&mut self, over: Rect, text: &str) -> (Rect, f32) {
        let (pw, ph) = self.picture_size();
        let size = self.fitting(text, 20.0, true, pw - 64.0);
        let tw = self.text_width(text, size, true);
        let (w, h) = (tw + 28.0, size + 18.0);
        let x = (over.left() + over.width() / 2.0 - w / 2.0).clamp(16.0, (pw - 16.0 - w).max(16.0));
        let y = if over.top() - h - 6.0 >= 8.0 {
            over.top() - h - 6.0
        } else {
            (over.bottom() + 6.0).min(ph - h - 8.0)
        };
        (rect(x, y, w, h), size)
    }

    /// `text` on a dark label just above `over` (below it where there is no room above),
    /// within the panel, drawn smaller where it is wide.
    fn draw_tip(&mut self, c: &mut Canvas<'_>, over: Rect, text: &str) {
        let (r, size) = self.tip_rect(over, text);
        fill(c, rounded(r, 10.0), [16, 18, 24, 245]);
        stroke(c, rounded(r, 10.0), [255, 255, 255, 40], 1.5);
        self.draw_text(
            c,
            r.left() + 14.0,
            r.top() + r.height() / 2.0 + size * 0.36,
            size,
            true,
            TEXT,
            text,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        c: &mut Canvas<'_>,
        x: f32,
        baseline: f32,
        size: f32,
        medium: bool,
        color: [u8; 4],
        s: &str,
    ) {
        let (pw, ph) = (c.pm.width() as i32, c.pm.height() as i32);
        let (ox, oy) = c.at;
        let data = c.pm.data_mut();
        // The cache and the fonts are separate fields: each glyph is borrowed from the
        // cache, not copied out of it.
        let font = if medium { &self.medium } else { &self.regular };
        let mut pen = x;
        for ch in s.chars() {
            let (m, coverage) = cached_glyph(&mut self.glyphs, font, medium, ch, size);
            // Where it is in the whole picture, then in the part drawn.
            let gx = (pen + m.xmin as f32).round() as i32 - ox;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32 - oy;
            pen += m.advance_width;
            let (w, h) = (m.width as i32, m.height as i32);
            // Only the part of the glyph inside the pixmap.
            let (rows, cols) = (
                gy.max(0) - gy..h.min(ph - gy),
                gx.max(0) - gx..w.min(pw - gx),
            );
            if rows.is_empty() || cols.is_empty() {
                continue;
            }
            for row in rows {
                let py = gy + row;
                for col in cols.clone() {
                    let px = gx + col;
                    let cov = coverage[(row * w + col) as usize] as u32;
                    let a = cov * color[3] as u32 / 255;
                    if a == 0 {
                        continue;
                    }
                    let i = ((py * pw + px) * 4) as usize;
                    for k in 0..3 {
                        let src = color[k] as u32 * a / 255;
                        data[i + k] = (src + data[i + k] as u32 * (255 - a) / 255) as u8;
                    }
                    data[i + 3] = (a + data[i + 3] as u32 * (255 - a) / 255) as u8;
                }
            }
        }
    }
}

/// Glyph `c` of `font` (the medium one if `medium`) at `size`, rasterized once and
/// kept in `glyphs`.
fn cached_glyph<'a>(
    glyphs: &'a mut HashMap<(bool, char, u32), (Metrics, Vec<u8>)>,
    font: &Font,
    medium: bool,
    c: char,
    size: f32,
) -> &'a (Metrics, Vec<u8>) {
    glyphs
        .entry((medium, c, size.to_bits()))
        .or_insert_with(|| font.rasterize(c, size))
}

impl Panel {
    /// A button on its own layer: `label` on a pill, accent-coloured when `lit`.
    /// Premultiplied RGBA.
    pub fn pill_image(&mut self, label: &str, width: u32, height: u32, lit: bool) -> Vec<u8> {
        let mut p = Pixmap::new(width, height).expect("pill size");
        let c = &mut Canvas::whole(&mut p);
        let (w, h) = (width as f32, height as f32);
        let bg = if lit { ACCENT } else { [16, 18, 24, 235] };
        fill(c, rounded(rect(0.0, 0.0, w, h), h / 2.0), bg);
        let mut size = h * 0.4;
        while size > 12.0 && self.text_width(label, size, true) > w - h * 0.6 {
            size -= 1.0;
        }
        let tw = self.text_width(label, size, true);
        let fg = [255, 255, 255, 255];
        self.draw_text(
            c,
            (w - tw) / 2.0,
            h / 2.0 + size * 0.36,
            size,
            true,
            fg,
            label,
        );
        p.take()
    }

    /// Where a monitor being added will appear, `width`×`height` (its own shape): a
    /// faint frame with `label` in the middle, until its picture comes.
    /// Premultiplied RGBA.
    pub fn placeholder_image(&mut self, label: &str, width: u32, height: u32) -> Vec<u8> {
        let mut p = Pixmap::new(width, height).expect("placeholder size");
        let c = &mut Canvas::whole(&mut p);
        let (w, h) = (width as f32, height as f32);
        let edge = (w.min(h) * 0.02).max(4.0);
        fill(
            c,
            rounded(rect(0.0, 0.0, w, h), edge * 3.0),
            [120, 160, 255, 150],
        );
        fill(
            c,
            rounded(rect(edge, edge, w - 2.0 * edge, h - 2.0 * edge), edge * 2.0),
            [16, 18, 24, 170],
        );
        let mut size = h * 0.12;
        while size > 12.0 && self.text_width(label, size, true) > w * 0.8 {
            size -= 2.0;
        }
        let tw = self.text_width(label, size, true);
        self.draw_text(
            c,
            (w - tw) / 2.0,
            h / 2.0 + size * 0.36,
            size,
            true,
            TEXT,
            label,
        );
        p.take()
    }

    /// A short message on a dark pill, `width`×`height`, for a toast layer: one
    /// line, or two when long (the end cut off with "…" if even that is not enough).
    /// Premultiplied RGBA.
    pub fn toast_image(&mut self, message: &str, width: u32, height: u32) -> Vec<u8> {
        let mut p = toast_pill(width, height);
        let c = &mut Canvas::whole(&mut p);
        let (w, h) = (width as f32, height as f32);
        let room = w - 72.0;
        let one = h * 0.3;
        if self.text_width(message, one, true) <= room {
            let tw = self.text_width(message, one, true);
            self.draw_text(
                c,
                (w - tw) / 2.0,
                h / 2.0 + one * 0.36,
                one,
                true,
                TEXT,
                message,
            );
            return p.take();
        }
        // Two lines, split at a word.
        let size = h * 0.24;
        let mut first = String::new();
        let mut rest = message;
        for (at, _) in message.match_indices(' ') {
            if self.text_width(&message[..at], size, true) > room {
                break;
            }
            message[..at].clone_into(&mut first);
            rest = message[at + 1..].trim_start();
        }
        if first.is_empty() {
            message.clone_into(&mut first);
            rest = "";
        }
        let mut second = rest.to_owned();
        while self.text_width(&second, size, true) > room && second.chars().count() > 1 {
            second = second
                .chars()
                .take(second.chars().count() - 2)
                .collect::<String>()
                + "…";
        }
        for (k, line) in [first, second].iter().enumerate() {
            let tw = self.text_width(line, size, true).min(room);
            let baseline = h / 2.0 - size * 0.25 + k as f32 * size * 1.25;
            self.draw_text(c, (w - tw) / 2.0, baseline, size, k == 0, TEXT, line);
        }
        p.take()
    }
}

/// The dark pill of a toast, `width`×`height`, with nothing written on it.
pub fn toast_pill(width: u32, height: u32) -> Pixmap {
    let mut p = Pixmap::new(width, height).expect("toast size");
    let c = &mut Canvas::whole(&mut p);
    fill(
        c,
        rounded(rect(0.0, 0.0, width as f32, height as f32), 28.0),
        [16, 18, 24, 235],
    );
    p
}

impl Panel {
    /// `message` on one line in the middle of `pill` (a [`toast_pill`] or a copy of
    /// one), smaller where it would not fit, never in two lines: for a label drawn
    /// again and again. With the glyphs of its letters and size drawn once before,
    /// nothing is allocated.
    pub fn write_line(&mut self, pill: &mut Pixmap, message: &str) {
        let (w, h) = (pill.width() as f32, pill.height() as f32);
        let room = w - 72.0;
        let mut size = h * 0.3;
        while size > 12.0 && self.text_width(message, size, true) > room {
            size -= 2.0;
        }
        let tw = self.text_width(message, size, true);
        let c = &mut Canvas::whole(pill);
        self.draw_text(
            c,
            (w - tw) / 2.0,
            h / 2.0 + size * 0.36,
            size,
            true,
            TEXT,
            message,
        );
    }
}

/// Pointer dot: white with a dark rim so it shows on light and dark content.
/// Premultiplied RGBA, `size`×`size`.
pub fn cursor_image(size: u32) -> Vec<u8> {
    let mut p = Pixmap::new(size, size).expect("cursor size");
    let canvas = &mut Canvas::whole(&mut p);
    let c = size as f32 / 2.0;
    fill(canvas, circle(c, c, c - 1.0), [20, 22, 28, 200]);
    fill(canvas, circle(c, c, c * 0.62), [255, 255, 255, 255]);
    p.take()
}

/// Round "☰" button that opens the menu. Premultiplied RGBA, `size`×`size`.
pub fn menu_button_image(size: u32, hovered: bool) -> Vec<u8> {
    let mut p = Pixmap::new(size, size).expect("button size");
    let c = &mut Canvas::whole(&mut p);
    let s = size as f32;
    let bg = if hovered { ACCENT } else { [16, 18, 24, 225] };
    fill(c, circle(s / 2.0, s / 2.0, s / 2.0 - 1.0), bg);
    for i in -1..=1 {
        let y = s / 2.0 + i as f32 * s * 0.15;
        fill(
            c,
            rounded(rect(s * 0.3, y - s * 0.035, s * 0.4, s * 0.07), s * 0.035),
            [255, 255, 255, 255],
        );
    }
    p.take()
}

/// The virtual monitors with reorder / primary / remove, then the laptop screen.
/// The monitors as seen from where the user sits, on a small map: press one to pick
/// it, and its actions show underneath.
fn monitor_list(s: &UiState, items: &mut Vec<Item>, lx: f32, lw: f32, mut y: f32) -> f32 {
    let l = &s.texts;
    let map_h = 210.0;
    items.push(Item::Box(rect(lx, y, lw, map_h), [255, 255, 255, 8]));
    // Fit everything seen into the box, keeping proportions.
    let all: Vec<[f32; 4]> = s
        .monitors
        .iter()
        .filter_map(|m| m.seen)
        .chain(s.physical_seen.iter().map(|(seen, _)| *seen))
        .collect();
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for [cx, cy, hw, hh] in &all {
        x0 = x0.min(cx - hw);
        x1 = x1.max(cx + hw);
        y0 = y0.min(cy - hh);
        y1 = y1.max(cy + hh);
    }
    let pad = 18.0;
    let scale = if all.is_empty() {
        1.0
    } else {
        ((lw - 2.0 * pad) / (x1 - x0).max(1e-3)).min((map_h - 2.0 * pad) / (y1 - y0).max(1e-3))
    };
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let to_rect = |[cx, cy, hw, hh]: [f32; 4]| {
        let gap = 3.0;
        rect(
            lx + lw / 2.0 + (cx - hw - mx) * scale + gap,
            y + map_h / 2.0 - (cy + hh - my) * scale + gap,
            (2.0 * hw * scale - 2.0 * gap).max(8.0),
            (2.0 * hh * scale - 2.0 * gap).max(8.0),
        )
    };
    // The PC's own displays: fainter, each with its name inside, smaller or shortened
    // where it does not fit (they are switched under the map, not picked on it).
    for (seen, name) in &s.physical_seen {
        let r = to_rect(*seen);
        items.push(Item::Box(r, [255, 255, 255, 14]));
        let size = (r.height() * 0.32).clamp(SMALLEST, 18.0);
        if r.width() > 24.0 && r.height() > size + 4.0 {
            let mut label = text_in(
                r.width() - 12.0,
                text(
                    r.left() + 6.0,
                    r.top() + r.height() / 2.0 + size * 0.36,
                    size,
                    false,
                    MUTED,
                    name,
                ),
            );
            if let Item::Text { shorten, .. } = &mut label {
                *shorten = true;
            }
            items.push(label);
        }
    }
    let selected = s.selected.or(s.primary);
    for m in &s.monitors {
        let Some(seen) = m.seen else { continue };
        let r = to_rect(seen);
        let style = if selected == Some(m.id) {
            Style::Selected
        } else if s.primary == Some(m.id) {
            Style::TabOn
        } else {
            Style::Normal
        };
        // Its name in the middle: smaller on a small tile, then shortened (in full
        // under the map once picked).
        let size = (r.height() * 0.4).clamp(14.0, 30.0);
        items.push(name_button(
            r,
            &m.label,
            size,
            style,
            Some(Action::SelectMonitor(m.id)),
        ));
    }
    // Monitors not placed in view (all but the one shown in cinema mode, say): a row
    // of small buttons along the bottom of the map, so they can still be picked.
    let mut bx = lx + 8.0;
    for m in s.monitors.iter().filter(|m| m.seen.is_none()) {
        let style = if selected == Some(m.id) {
            Style::Selected
        } else if s.primary == Some(m.id) {
            Style::TabOn
        } else {
            Style::Normal
        };
        let r = rect(bx, y + map_h - 44.0, 40.0, 36.0);
        items.push(name_button(
            r,
            &m.label,
            button_font_size(&m.label),
            style,
            Some(Action::SelectMonitor(m.id)),
        ));
        bx += 46.0;
    }
    y += map_h + 12.0;
    // The picked monitor: what it is, and what can be done with it.
    if let Some(m) = selected.and_then(|id| s.monitors.iter().find(|m| m.id == id)) {
        // Its name, left of its buttons: smaller when long, then shortened (whole on
        // its tile when pointed at).
        let mut title = text_in(
            lw - 58.0 - 10.0 - 210.0 - 10.0,
            text(
                lx,
                y + 24.0,
                22.0,
                true,
                TEXT,
                &l.f("headset-monitor-n", &[("n", (&m.label).into())]),
            ),
        );
        if let Item::Text { shorten, .. } = &mut title {
            *shorten = true;
        }
        items.push(title);
        let remove = rect(lx + lw - 58.0, y + 7.0, 48.0, 48.0);
        let primary = rect(lx + lw - 58.0 - 10.0 - 210.0, y + 7.0, 210.0, 48.0);
        // Left of the buttons: its size and place, or what is wrong with its picture.
        let (color, line) = match &m.picture {
            Picture::Shows => (
                MUTED,
                match m.place.name() {
                    Some(place) => format!("{} × {} · {}", m.width, m.height, l.t(place)),
                    None => format!("{} × {}", m.width, m.height),
                },
            ),
            Picture::Recovering => (WAIT, l.t("headset-video-trouble").to_owned()),
            Picture::NotShown(why) => (DANGER, why.clone()),
        };
        let mut info = text_in(
            primary.left() - lx - 10.0,
            text(lx, y + 50.0, 16.0, false, color, &line),
        );
        if let Item::Text { shorten, .. } = &mut info {
            *shorten = true;
        }
        items.push(info);
        // Another scale, where the PC makes monitors at its scales.
        let scales = scales_of(s);
        if !scales.is_empty() {
            let (y0, bw) = (y + 66.0, 92.0);
            let n = scales.len().min(3);
            let label = l.t("headset-scale");
            items.push(text_in(
                lw - n as f32 * (bw + 8.0) - 10.0,
                text(lx, y0 + 25.0, 18.0, false, MUTED, label),
            ));
            for (i, &scale) in scales.iter().take(3).enumerate() {
                let r = rect(
                    lx + lw - 10.0 - (n - i) as f32 * (bw + 8.0) + 8.0,
                    y0,
                    bw,
                    36.0,
                );
                let on = m.scale == scale;
                let action = Action::SetScale(m.id, scale);
                let mut b = button(
                    r,
                    &l.f("headset-percent", &[("value", scale.into())]),
                    if on { Style::Selected } else { Style::Normal },
                    (!on).then_some(action),
                );
                if let Item::Button { key, size, .. } = &mut b {
                    *key = Some(action);
                    *size = 17.0;
                }
                items.push(b);
            }
            y += 48.0;
        }
        if s.confirm_remove == Some(m.id) {
            // In the row of the buttons it stands for, never over the scale's row.
            items.push(button(
                rect(
                    primary.left(),
                    remove.top(),
                    remove.right() - primary.left(),
                    remove.height(),
                ),
                l.t("headset-monitor-remove-confirm"),
                Style::Armed,
                Some(Action::Remove(m.id)),
            ));
        } else {
            if s.primary == Some(m.id) {
                items.push(button(
                    primary,
                    l.t("headset-monitor-primary"),
                    Style::Selected,
                    None,
                ));
            } else {
                items.push(button(
                    primary,
                    l.t("headset-monitor-make-primary"),
                    Style::Normal,
                    Some(Action::MakePrimary(m.id)),
                ));
            }
            let removable = s.monitors.len() > 1;
            items.push(button(
                remove,
                "×",
                if removable {
                    Style::Danger
                } else {
                    Style::Disabled
                },
                removable.then_some(Action::Remove(m.id)),
            ));
        }
        y += 70.0;
    }
    // Detached windows: a name, the size and a close button.
    for (i, w) in s.windows.iter().enumerate() {
        items.push(Item::Box(rect(lx, y, lw, 52.0), [255, 255, 255, 8]));
        items.push(text(
            lx + 20.0,
            y + 25.0,
            22.0,
            true,
            TEXT,
            &l.f("headset-window-n", &[("n", (i + 1).into())]),
        ));
        items.push(text(
            lx + 20.0,
            y + 46.0,
            16.0,
            false,
            MUTED,
            &format!("{} × {}", w.width, w.height),
        ));
        items.push(button(
            rect(lx + lw - 52.0, y + 6.0, 42.0, 40.0),
            "×",
            Style::Danger,
            Some(Action::CloseWindow(w.id)),
        ));
        y += 58.0;
    }
    y = pc_displays(s, items, lx, lw, y + 34.0);
    items.push(button(
        rect(lx, y, lw, 52.0),
        l.t("headset-monitors-line-up"),
        Style::Normal,
        Some(Action::Tidy),
    ));
    y + 52.0
}

/// Scales the PC makes monitors at, where it makes them at more than one: what the
/// menu offers.
fn scales_of(s: &UiState) -> &[u16] {
    match &s.limits {
        Some(l) if s.can_scale && l.scales.len() > 1 => &l.scales,
        _ => &[],
    }
}

/// The PC's own displays from `y` (those connected first), each on a row: its name,
/// its connector smaller under it, and its switch (shown or hidden, said by the
/// highlighted option). One not connected says so, without a switch (there is
/// nothing to show) unless it is still shown, so it can be hidden; with as many shown
/// as the PC allows, the others' "Shown" waits, saying why. As many rows as fit
/// above "Line up", a page at a time, the pages turned beside the title. Where the
/// next thing goes.
fn pc_displays(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) -> f32 {
    let l = &s.texts;
    if s.physical.is_empty() {
        return y - 18.0;
    }
    // Room above "Line up" and the bottom line, where a note has its free place
    // (above the error line, when there is one).
    let error = if s.error.is_some() { 42.0 } else { 0.0 };
    let rows = ((HEIGHT as f32 - 166.0 - error - y) / 60.0)
        .floor()
        .max(1.0) as usize;
    let pages = s.physical.len().div_ceil(rows);
    let page = usize::from(s.physical_page) % pages;
    let title_room = if pages > 1 { w - PAGER_W - 10.0 } else { w };
    items.push(text_in(
        title_room,
        text(x, y, 21.0, true, SECTION, l.t("headset-pc-displays")),
    ));
    if pages > 1 {
        pager(
            items,
            l,
            (x + w, y - 8.0),
            (page, pages),
            Action::PhysicalPage,
        );
    }
    y += 16.0;
    let max = s.limits.as_ref().map(|l| usize::from(l.physical_max));
    let shown = s.physical.iter().filter(|d| d.shown && d.connected).count();
    let full = max.is_some_and(|max| shown >= max);
    let bw = 128.0;
    for d in s.physical.iter().skip(page * rows).take(rows) {
        items.push(Item::Box(rect(x, y, w, 54.0), [255, 255, 255, 8]));
        // One shown and then disconnected keeps its switch, to be hidden.
        let switched = d.connected || d.shown;
        let room = if switched {
            w - 2.0 * bw - 10.0 - 32.0
        } else {
            w - 32.0
        };
        let mut name = text_in(room, text(x + 16.0, y + 24.0, 19.0, true, TEXT, &d.name));
        if let Item::Text { shorten, .. } = &mut name {
            *shorten = true;
        }
        items.push(name);
        let note = if !d.connected {
            Some(l.t("headset-pc-display-not-connected"))
        } else if full && !d.shown {
            Some(l.t("headset-pc-display-limit"))
        } else {
            None
        };
        let under = match (&d.connector, note) {
            (Some(c), Some(n)) => format!("{c} · {n}"),
            (Some(c), None) => c.clone(),
            (None, Some(n)) => n.to_owned(),
            (None, None) => String::new(),
        };
        if !under.is_empty() {
            let mut t = text_in(room, text(x + 16.0, y + 45.0, 15.0, false, MUTED, &under));
            if let Item::Text { shorten, .. } = &mut t {
                *shorten = true;
            }
            items.push(t);
        }
        let can_show = !full || d.shown;
        let switch = [
            (l.t("headset-pc-display-shown"), true, can_show),
            (l.t("headset-pc-display-hidden"), false, true),
        ];
        for (i, (label, on, enabled)) in switch.into_iter().enumerate().filter(|_| switched) {
            let r = rect(
                x + w - 2.0 * bw - 14.0 + i as f32 * (bw + 6.0),
                y + 7.0,
                bw,
                40.0,
            );
            let action = Action::ShowPhysical(d.id, on);
            let chosen = d.shown == on;
            let style = match (enabled, chosen) {
                (_, true) => Style::Selected,
                (false, _) => Style::Disabled,
                (true, false) => Style::Normal,
            };
            let mut b = button(r, label, style, (enabled && !chosen).then_some(action));
            if let Item::Button { key, size, .. } = &mut b {
                *key = Some(action);
                *size = 17.0;
            }
            items.push(b);
        }
        y += 60.0;
    }
    y + 4.0
}

/// How wide [`pager`] is.
const PAGER_W: f32 = 250.0;

/// "‹ Page 2 of 3 ›", its right edge at `right` and its middle at `mid` (x, y): the
/// buttons turn to the page before and after (`turn` to it), round.
fn pager(
    items: &mut Vec<Item>,
    l: &Texts,
    (right, mid): (f32, f32),
    (page, pages): (usize, usize),
    turn: fn(u8) -> Action,
) {
    let to = |p: usize| turn(u8::try_from(p % pages).unwrap_or(0));
    let (bw, bh) = (44.0, 36.0);
    items.push(button(
        rect(right - PAGER_W, mid - bh / 2.0, bw, bh),
        "‹",
        Style::Normal,
        Some(to(page + pages - 1)),
    ));
    items.push(button(
        rect(right - bw, mid - bh / 2.0, bw, bh),
        "›",
        Style::Normal,
        Some(to(page + 1)),
    ));
    let label = l.f(
        "headset-page-of",
        &[("page", (page + 1).into()), ("pages", pages.into())],
    );
    items.push(text_in(
        PAGER_W - 2.0 * bw - 16.0,
        text(
            right - PAGER_W + bw + 8.0,
            mid + 6.0,
            16.0,
            false,
            MUTED,
            &label,
        ),
    ));
}

/// A row with a title, a muted note and one button on the right.
fn hints(l: &Texts, items: &mut Vec<Item>, lx: f32, mut y: f32) {
    for line in [
        l.t("headset-hint-finger"),
        l.t("headset-hint-hand"),
        l.t("headset-hint-controller"),
        l.t("headset-hint-stick"),
    ] {
        items.push(text(lx, y, 19.0, false, MUTED, line));
        y += 30.0;
    }
}

/// A titled group of buttons where one option is selected:
/// `(label, action, selected, enabled)`. An empty title continues the previous group.
fn segmented(
    items: &mut Vec<Item>,
    x: f32,
    w: f32,
    y: &mut f32,
    title: &str,
    options: Vec<(&str, Action, bool, bool)>,
) {
    if !title.is_empty() {
        items.push(text_in(w, text(x, *y, 21.0, true, SECTION, title)));
        *y += 18.0;
    } else {
        *y -= 30.0;
    }
    let n = options.len() as f32;
    let bw = (w - 10.0 * (n - 1.0)) / n;
    for (i, (label, action, selected, enabled)) in options.into_iter().enumerate() {
        let r = rect(x + i as f32 * (bw + 10.0), *y, bw, 48.0);
        let style = match (enabled, selected) {
            (false, _) => Style::Disabled,
            (true, true) => Style::Selected,
            (true, false) => Style::Normal,
        };
        let mut b = button(r, label, style, (enabled && !selected).then_some(action));
        if let Item::Button { key, .. } = &mut b {
            *key = Some(action);
        }
        items.push(b);
    }
    *y += 48.0 + 40.0;
}

/// Add a monitor, layout, curve.
fn build_monitor_options(s: &UiState, items: &mut Vec<Item>, rx: f32, rw: f32, mut y: f32) -> f32 {
    let l = &s.texts;
    let scales = scales_of(s);
    // The scale the new monitor gets, beside the title: small choices on its right.
    let (sw, sgap) = (76.0, 6.0);
    let shown = scales.len().min(3);
    let choices_w = shown as f32 * (sw + sgap);
    if shown > 0 {
        let label = l.t("headset-scale");
        let label_w = 120.0;
        items.push(text_in(
            label_w,
            text(rx + rw - choices_w - label_w, y, 17.0, false, MUTED, label),
        ));
        for (i, &scale) in scales.iter().take(3).enumerate() {
            let r = rect(
                rx + rw - choices_w + sgap + i as f32 * (sw + sgap),
                y - 26.0,
                sw,
                36.0,
            );
            let on = s.add_scale == scale;
            let action = Action::AddScale(scale);
            let mut b = button(
                r,
                &l.f("headset-percent", &[("value", scale.into())]),
                if on { Style::Selected } else { Style::Normal },
                (!on).then_some(action),
            );
            if let Item::Button { key, size, .. } = &mut b {
                *key = Some(action);
                *size = 16.0;
            }
            items.push(b);
        }
    }
    let title_room = if shown > 0 {
        rw - choices_w - 130.0
    } else {
        rw
    };
    items.push(text_in(
        title_room,
        text(rx, y, 21.0, false, MUTED, l.t("headset-add-monitor")),
    ));
    y += 18.0;
    // The sizes the PC offers (it says them when the headset connects), six at most.
    let max = s.limits.as_ref().map(|l| usize::from(l.virtual_max));
    let full = s.host.is_none() || max.is_none_or(|max| s.monitors.len() >= max);
    let sizes: Vec<(u32, u32)> = match &s.limits {
        Some(l) if !l.sizes.is_empty() => {
            let mut sizes: Vec<(u32, u32)> = Vec::new();
            for m in &l.sizes {
                if !sizes.contains(&(m.w, m.h)) {
                    sizes.push((m.w, m.h));
                }
            }
            sizes.truncate(6);
            sizes
        }
        _ => STANDARD_SIZES.to_vec(),
    };
    let bw = (rw - 2.0 * 10.0) / 3.0;
    for (i, (width, height)) in sizes.iter().enumerate() {
        let label = format!("{width}×{height}");
        let r = rect(
            rx + (i % 3) as f32 * (bw + 10.0),
            y + (i / 3) as f32 * 58.0,
            bw,
            48.0,
        );
        let (style, action) = if full {
            (Style::Disabled, None)
        } else {
            (
                Style::Normal,
                Some(Action::Add {
                    width: *width,
                    height: *height,
                }),
            )
        };
        items.push(button(r, &label, style, action));
    }
    y += sizes.len().div_ceil(3) as f32 * 58.0 + 40.0;
    if let Some(max) = max.filter(|max| s.monitors.len() >= *max) {
        items.push(text(
            rx,
            y - 22.0,
            18.0,
            false,
            MUTED,
            &l.f("headset-monitors-at-most", &[("count", max.into())]),
        ));
        y += 10.0;
    }

    let st = s.settings;
    let layouts = items.len();
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-layout"),
        vec![
            (
                l.t("headset-layout-around"),
                Action::Preset(Preset::Arc),
                st.preset == Preset::Arc,
                true,
            ),
            (
                l.t("headset-layout-wall"),
                Action::Preset(Preset::Wall),
                st.preset == Preset::Wall,
                true,
            ),
            (
                l.t("headset-layout-lying"),
                Action::Preset(Preset::Overhead),
                st.preset == Preset::Overhead,
                true,
            ),
        ],
    );
    // The chosen layout pressed again lays the monitors out by it anew, as choosing it
    // does (without going through another one first).
    for item in items.get_mut(layouts..).into_iter().flatten() {
        if let Item::Button {
            style: Style::Selected,
            action,
            ..
        } = item
        {
            *action = Some(Action::Preset(st.preset));
        }
    }
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-curve"),
        vec![
            (
                l.t("headset-curve-flat"),
                Action::Curve(Curve::Flat),
                st.curve == Curve::Flat,
                true,
            ),
            (
                l.t("headset-curve-gentle"),
                Action::Curve(Curve::Gentle),
                st.curve == Curve::Gentle,
                s.curve_available,
            ),
            (
                l.t("headset-curve-strong"),
                Action::Curve(Curve::Strong),
                st.curve == Curve::Strong,
                s.curve_available,
            ),
        ],
    );
    // Two switches side by side.
    let half = (rw - 16.0) / 2.0;
    let mut y2 = y;
    segmented(
        items,
        rx,
        half,
        &mut y,
        l.t("headset-follow-head"),
        vec![
            (l.t("headset-no"), Action::Follow(false), !st.follow, true),
            (l.t("headset-yes"), Action::Follow(true), st.follow, true),
        ],
    );
    segmented(
        items,
        rx + half + 16.0,
        half,
        &mut y2,
        l.t("headset-magnet"),
        vec![
            (l.t("headset-no"), Action::Magnet(false), !st.magnet, true),
            (l.t("headset-yes"), Action::Magnet(true), st.magnet, true),
        ],
    );
    y = y.max(y2);
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-drag"),
        vec![
            (
                l.t("headset-drag-one"),
                Action::MoveAll(false),
                !st.move_all,
                true,
            ),
            (
                l.t("headset-drag-all"),
                Action::MoveAll(true),
                st.move_all,
                true,
            ),
        ],
    );
    // The cinema screen can be picked before switching to cinema.
    if s.monitors.len() > 1 {
        let sizes: Vec<(DisplayKey, u32, u32)> = s
            .monitors
            .iter()
            .map(|m| (m.id, m.width, m.height))
            .collect();
        let shown = crate::layout::cinema_choice(&sizes, s.cinema_screen);
        let short = s.monitors.len() > 3;
        let labels: Vec<String> = s
            .monitors
            .iter()
            .map(|m| {
                if short {
                    m.label.clone()
                } else {
                    l.f("headset-monitor-n", &[("n", (&m.label).into())])
                }
            })
            .collect();
        let from = items.len();
        segmented(
            items,
            rx,
            rw,
            &mut y,
            l.t("headset-cinema-screen"),
            s.monitors
                .iter()
                .zip(&labels)
                .map(|(m, label)| {
                    (
                        label.as_str(),
                        Action::CinemaScreen(m.id),
                        shown == Some(m.id),
                        true,
                    )
                })
                .collect(),
        );
        // The monitors' names, as the PC calls them: shortened where long.
        for item in items.get_mut(from..).into_iter().flatten() {
            if let Item::Button { label, full, .. } = item {
                *full = Some(label.clone());
            }
        }
    }
    y
}

/// Background and hands on the left, focus mode on the right.
fn build_look(s: &UiState, items: &mut Vec<Item>, lx: f32, lw: f32, rx: f32, rw: f32, top: f32) {
    let l = &s.texts;
    let st = s.settings;
    let mut y = top;
    let bg = |b: Background| (Action::Background(b), st.background == b);
    let (a1, s1) = bg(Background::Black);
    let (a2, s2) = bg(Background::Room);
    let (a3, s3) = bg(Background::Custom);
    let (a4, s4) = bg(Background::RoomDim);
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-background"),
        vec![
            (l.t("headset-background-black"), a1, s1, true),
            (l.t("headset-background-room"), a2, s2, s.room_available),
            (l.t("headset-background-dimmed"), a4, s4, s.room_available),
            (
                l.t("headset-background-own"),
                a3,
                s3,
                s.panorama_available && s.custom_available,
            ),
        ],
    );
    // Where an own photo comes from, while there is none yet.
    if s.panorama_available && !s.custom_available {
        for (k, line) in [
            l.t("headset-background-own-where"),
            "Android/data/dev.prostir.client/files/background.jpg",
        ]
        .into_iter()
        .enumerate()
        {
            items.push(text(
                lx,
                y - 10.0 + 24.0 * k as f32,
                17.0,
                false,
                MUTED,
                line,
            ));
        }
        y += 68.0;
    }
    let skies = [
        (l.t("headset-sky-night"), Sky::Night),
        (l.t("headset-sky-sunset"), Sky::Sunset),
        (l.t("headset-sky-space"), Sky::Space),
        (l.t("headset-sky-aurora"), Sky::Aurora),
    ];
    segmented(
        items,
        lx,
        lw,
        &mut y,
        "",
        skies
            .iter()
            .map(|(label, sky)| {
                let (a, sel) = bg(Background::Sky(*sky));
                (*label, a, sel, s.panorama_available)
            })
            .collect(),
    );

    // Side by side, or (one column) under the background and hands.
    let mut y = if rx == lx { y - 10.0 } else { top };
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-focus"),
        vec![
            (l.t("headset-off"), Action::Focus(false), !st.focus, true),
            (
                l.t("headset-focus-dim-others"),
                Action::Focus(true),
                st.focus,
                true,
            ),
        ],
    );
    y -= 12.0;
    items.push(text(rx, y, 19.0, false, MUTED, l.t("headset-focus-note")));
    y += 48.0;
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-keyboard-window"),
        vec![
            (
                l.t("headset-off"),
                Action::KeyboardWindow(false),
                !st.keyboard_window,
                s.keyboard_available,
            ),
            (
                l.t("headset-show"),
                Action::KeyboardWindow(true),
                st.keyboard_window,
                s.keyboard_available,
            ),
        ],
    );
    y -= 12.0;
    items.push(text(
        rx,
        y,
        19.0,
        false,
        MUTED,
        l.t("headset-keyboard-window-note"),
    ));
    y += 48.0;
    let every = st.rest_every;
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-rest"),
        [
            (0, l.t("headset-off")),
            (20, l.t("headset-rest-every-20")),
            (60, l.t("headset-rest-hourly")),
        ]
        .into_iter()
        .map(|(m, label)| (label, Action::RestEvery(m), every == m, true))
        .collect(),
    );
    let levels = [100u8, 80, 60, 40];
    let labels = levels.map(|p| l.f("headset-percent", &[("value", p.into())]));
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("headset-brightness"),
        levels
            .iter()
            .zip(&labels)
            .map(|(&p, label)| {
                (
                    label.as_str(),
                    Action::Brightness(p),
                    st.brightness == p,
                    true,
                )
            })
            .collect(),
    );
}

/// A PC to pair with (a new one, or one that forgot this headset): pair only if the
/// user says so.
fn build_pair_offer(l: &Texts, items: &mut Vec<Item>, x: f32, w: f32, host: &str, why: OfferWhy) {
    let mut y = 180.0;
    let title = match why {
        OfferWhy::New => l.t("headset-offer-new"),
        OfferWhy::WrongCodes => l.t("headset-offer-wrong-codes"),
        OfferWhy::Forgotten => l.t("headset-offer-forgotten"),
        OfferWhy::PcRefused { .. } => l.t("headset-offer-pc-refused"),
    };
    items.push(text(x, y, 34.0, true, TEXT, title));
    y += 60.0;
    let host: String = host.chars().take(40).collect();
    items.push(text(x, y, 24.0, false, TEXT, &host));
    y += 44.0;
    for line in [l.t("headset-offer-own-pc"), l.t("headset-offer-code")] {
        items.push(text(x, y, 20.0, false, MUTED, line));
        y += 32.0;
    }
    y += 20.0;
    // Refused by the PC: the other side may show the code.
    if let OfferWhy::PcRefused { pc_shows } = why {
        let other = if pc_shows {
            l.t("headset-pairing-other-way-type-on-pc")
        } else {
            l.t("headset-pairing-other-way-type-here")
        };
        items.push(button(
            rect(x, y, w, 56.0),
            other,
            Style::Normal,
            Some(Action::PairOtherWay),
        ));
        y += 68.0;
    }
    let half = (w - 12.0) / 2.0;
    let pair = match why {
        OfferWhy::New => l.t("headset-offer-pair"),
        OfferWhy::WrongCodes | OfferWhy::PcRefused { .. } => l.t("headset-offer-try-again"),
        OfferWhy::Forgotten => l.t("headset-offer-pair-again"),
    };
    items.push(button(
        rect(x, y, half, 56.0),
        pair,
        Style::Selected,
        Some(Action::PairAnswer(true)),
    ));
    items.push(button(
        rect(x + half + 12.0, y, half, 56.0),
        l.t("headset-offer-not-now"),
        Style::Normal,
        Some(Action::PairAnswer(false)),
    ));
}

/// The headset and the PC cannot talk: which one to update, and where (`headset`:
/// where this build of the headset app is updated).
fn build_update(
    l: &Texts,
    items: &mut Vec<Item>,
    x: f32,
    w: f32,
    pc_newer: bool,
    pc_app: &str,
    headset: UpdateHint,
) {
    let ours = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
    let mut y = 180.0;
    let title = if pc_newer {
        l.t("headset-update-headset")
    } else {
        l.t("headset-update-pc")
    };
    items.push(text(x, y, 34.0, true, TEXT, title));
    y += 60.0;
    let versions = l.f(
        "headset-update-versions",
        &[("pc", pc_app.into()), ("headset", ours.into())],
    );
    items.push(text(x, y, 22.0, false, MUTED, &versions));
    y += 36.0;
    items.push(text(
        x,
        y,
        22.0,
        false,
        MUTED,
        l.t("headset-update-versions-differ"),
    ));
    y += 50.0;
    items.push(Item::Box(rect(x, y, w, 130.0), [255, 255, 255, 12]));
    let (how, where_) = if pc_newer {
        (l.t(headset.how), headset.place)
    } else {
        (l.t("headset-update-pc-from"), SITE)
    };
    items.push(text(x + 28.0, y + 50.0, 22.0, false, TEXT, how));
    items.push(text(x + 28.0, y + 100.0, 32.0, true, ACCENT, where_));
    y += 170.0;
    items.push(text(
        x,
        y,
        19.0,
        false,
        MUTED,
        l.t("headset-update-connects-by-itself"),
    ));
}

/// Prostir's site: the PC part, and which desktops it runs on (kept there, so a new
/// one needs no new headset app).
pub const SITE: &str = "prostir.ishawyha.dev";

/// Before any PC is found: what Prostir needs and where to get the PC part.
fn build_welcome(l: &Texts, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    items.push(text(
        x,
        y + 6.0,
        26.0,
        true,
        TEXT,
        l.t("headset-welcome-title"),
    ));
    y += 36.0;
    items.push(text(
        x,
        y,
        19.0,
        false,
        MUTED,
        &l.f("headset-welcome-pc-app", &[("site", SITE.into())]),
    ));
    y += 30.0;
    let steps: [(&str, &str, bool); 3] = [
        (l.t("headset-welcome-install"), SITE, true),
        (
            l.t("headset-welcome-start"),
            l.t("headset-welcome-start-note"),
            false,
        ),
        // USB needs developer mode and adb: not something a store install has.
        (
            l.t("headset-welcome-finds"),
            l.t(if cfg!(feature = "store") {
                "headset-welcome-finds-wifi"
            } else {
                "headset-welcome-finds-wifi-usb"
            }),
            false,
        ),
    ];
    for (n, (title, note, link)) in steps.into_iter().enumerate() {
        y += 16.0;
        let h = if link { 116.0 } else { 96.0 };
        items.push(Item::Box(rect(x, y, w, h), [255, 255, 255, 12]));
        items.push(text(
            x + 22.0,
            y + 40.0,
            28.0,
            true,
            ACCENT,
            &format!("{}", n + 1),
        ));
        items.push(text(x + 64.0, y + 38.0, 23.0, true, TEXT, title));
        let (size, color, medium) = if link {
            (30.0, ACCENT, true)
        } else {
            (19.0, MUTED, false)
        };
        items.push(text(
            x + 64.0,
            y + if link { 88.0 } else { 72.0 },
            size,
            medium,
            color,
            note,
        ));
        y += h;
    }
}

/// The first-run page: how to reach the menu, move and line up monitors, and what
/// happens when the headset comes off.
fn build_intro(l: &Texts, items: &mut Vec<Item>, x: f32, w: f32) {
    let mut y = 160.0;
    items.push(text(x, y, 28.0, true, TEXT, l.t("headset-intro-title")));
    y += 34.0;
    let tips = [
        ("headset-intro-menu", "headset-intro-menu-text"),
        ("headset-intro-move", "headset-intro-move-text"),
        ("headset-intro-line-up", "headset-intro-line-up-text"),
        ("headset-intro-finger", "headset-intro-finger-text"),
        ("headset-intro-off", "headset-intro-off-text"),
    ];
    for (title, tip) in tips {
        y += 28.0;
        items.push(Item::Box(rect(x, y, w, 96.0), [255, 255, 255, 12]));
        items.push(text(x + 24.0, y + 38.0, 23.0, true, TEXT, l.t(title)));
        items.push(text(x + 24.0, y + 74.0, 20.0, false, MUTED, l.t(tip)));
        y += 96.0;
    }
    y += 36.0;
    items.push(button(
        rect(x, y, 280.0, 56.0),
        l.t("headset-intro-done"),
        Style::Selected,
        Some(Action::IntroDone),
    ));
}

/// A big square button with a title and its state underneath: what is on is lit.
#[allow(clippy::too_many_arguments)]
fn tile(
    items: &mut Vec<Item>,
    r: Rect,
    title: &str,
    state: &str,
    on: bool,
    action: Option<Action>,
) {
    let style = match (&action, on) {
        (None, _) => Style::Disabled,
        (_, true) => Style::Selected,
        _ => Style::Normal,
    };
    items.push(button(r, "", style, action));
    let (fg, sub) = if on {
        ([255, 255, 255, 255], [225, 235, 255, 255])
    } else {
        (TEXT, MUTED)
    };
    let room = r.width() - 48.0;
    items.push(text_in(
        room,
        text(r.left() + 26.0, r.top() + 62.0, 27.0, true, fg, title),
    ));
    items.push(text_in(
        room,
        text(r.left() + 24.0, r.top() + 104.0, 19.0, false, sub, state),
    ));
}

/// Everyday things in one place: profiles to switch to, and toggles as tiles.
fn build_quick(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    let l = &s.texts;
    let st = s.settings;
    let connected = s.host.is_some();
    items.push(text(
        x,
        y + 14.0,
        21.0,
        true,
        SECTION,
        l.t("headset-profiles"),
    ));
    y += 32.0;
    // The first three, the one in use among them (in place of the third where it is
    // further down the list), and the way to all of them.
    let cw = (w - 3.0 * 12.0) / 4.0;
    let mut first: Vec<usize> = (0..s.profiles.len().min(3)).collect();
    if let Some(a) = s
        .active_profile
        .filter(|a| *a >= 3 && *a < s.profiles.len())
    {
        first[2] = a;
    }
    for (k, &i) in first.iter().enumerate() {
        let r = rect(x + k as f32 * (cw + 12.0), y, cw, 72.0);
        let active = s.active_profile == Some(i);
        let style = match (connected, active) {
            (false, _) => Style::Disabled,
            (true, true) => Style::Selected,
            (true, false) => Style::Normal,
        };
        // The one in use too: pressed again, its monitors go back where it has them.
        let action = connected.then_some(Action::ApplyProfile(i));
        let mut b = name_button(r, &s.profiles[i], 19.0, style, action);
        if let Item::Button { key, .. } = &mut b {
            *key = Some(Action::ApplyProfile(i));
        }
        items.push(b);
    }
    if s.profiles.is_empty() {
        let none = l.t("headset-profiles-none-short");
        items.push(text_in(
            3.0 * (cw + 12.0) - 12.0,
            text(x, y + 44.0, 19.0, false, MUTED, none),
        ));
    }
    items.push(button(
        rect(x + 3.0 * (cw + 12.0), y, cw, 72.0),
        l.t("headset-profiles-all"),
        Style::Normal,
        Some(Action::Tab(Tab::Profiles)),
    ));
    y += 72.0 + 40.0;
    let on_off = |on: bool| {
        if on {
            l.t("headset-on")
        } else {
            l.t("headset-off")
        }
    };
    let tw = (w - 2.0 * 12.0) / 3.0;
    let th = 140.0;
    let full = s
        .limits
        .as_ref()
        .is_none_or(|l| s.windows.len() >= usize::from(l.windows_max));
    let tiles: [(&str, String, bool, Option<Action>); 9] = [
        (
            l.t("headset-quick-in-front"),
            l.t("headset-quick-in-front-state").to_owned(),
            false,
            Some(Action::Recenter),
        ),
        (
            l.t("headset-focus"),
            on_off(st.focus).to_owned(),
            st.focus,
            Some(Action::Focus(!st.focus)),
        ),
        (
            l.t("headset-cinema"),
            on_off(st.cinema).to_owned(),
            st.cinema,
            Some(Action::Cinema(!st.cinema)),
        ),
        (
            l.t("headset-keyboard-window"),
            on_off(st.keyboard_window).to_owned(),
            st.keyboard_window,
            s.keyboard_available
                .then_some(Action::KeyboardWindow(!st.keyboard_window)),
        ),
        (
            l.t("headset-pc-sound"),
            if s.audio_wanted {
                l.t("headset-sound-in-headset")
            } else {
                l.t("headset-sound-on-pc")
            }
            .to_owned(),
            s.audio_wanted,
            connected.then_some(Action::SetAudio(!s.audio_wanted)),
        ),
        (
            l.t("headset-microphone"),
            if s.mic_wanted {
                l.t("headset-quick-mic-headset")
            } else {
                l.t("headset-quick-mic-pc")
            }
            .to_owned(),
            s.mic_wanted,
            connected.then_some(Action::SetMic(!s.mic_wanted)),
        ),
        (
            l.t("headset-quick-detach"),
            if full {
                l.t("headset-quick-detach-full")
            } else {
                l.t("headset-quick-detach-state")
            }
            .to_owned(),
            false,
            (connected && !full).then_some(Action::DetachWindow),
        ),
        (
            l.t("headset-quick-window-here"),
            l.t("headset-quick-window-here-state").to_owned(),
            false,
            connected.then_some(Action::WindowHere),
        ),
        (
            l.t("headset-quick-screenshot"),
            l.t("headset-quick-screenshot-state").to_owned(),
            false,
            connected.then_some(Action::Screenshot),
        ),
    ];
    for (i, (title, state, on, action)) in tiles.into_iter().enumerate() {
        let r = rect(
            x + (i % 3) as f32 * (tw + 12.0),
            y + (i / 3) as f32 * (th + 12.0),
            tw,
            th,
        );
        tile(items, r, title, &state, on, action);
    }
}

/// Every profile, ten to a page, two to a row across the page, each with its name to
/// switch to it (the one in use highlighted), "Save here" to save the setup over it and
/// "×" to delete it (each asks again first); "New profile" saves the setup as one
/// more; with more than ten, the pages turn beside it. Under the list, how many there
/// may be and where they are named.
fn build_profiles(s: &UiState, items: &mut Vec<Item>, mut y: f32, wrap: Wrap<'_>) {
    let l = &s.texts;
    let (x, w) = (44.0, WIDTH as f32 - 88.0);
    let at = items.len();
    let connected = s.host.is_some();
    // Something to save: the PC's monitors.
    let can_save = connected && !s.monitors.is_empty();
    let full = s.profiles.len() >= crate::profiles::MAX_PROFILES;
    let pages = s.profiles.len().div_ceil(PROFILES_PER_PAGE).max(1);
    let page = usize::from(s.profile_page) % pages;
    let pager_room = if pages > 1 { PAGER_W + 20.0 } else { 0.0 };
    items.push(text_in(
        w - 330.0 - pager_room,
        text(x, y + 30.0, 21.0, true, SECTION, l.t("headset-profiles")),
    ));
    if pages > 1 {
        pager(
            items,
            l,
            (x + w - 320.0, y + 24.0),
            (page, pages),
            Action::ProfilePage,
        );
    }
    let new = Action::NewProfile;
    let mut b = button(
        rect(x + w - 300.0, y, 300.0, 48.0),
        l.t("headset-profile-new"),
        if can_save && !full {
            Style::Normal
        } else {
            Style::Disabled
        },
        (can_save && !full).then_some(new),
    );
    if let Item::Button { key, .. } = &mut b {
        *key = Some(new);
    }
    items.push(b);
    y += 70.0;
    if s.profiles.is_empty() {
        let none = l.t("headset-profiles-none");
        for (k, line) in wrap(none, 19.0, w).iter().enumerate() {
            items.push(text(
                x,
                y + 30.0 + k as f32 * 28.0,
                19.0,
                false,
                MUTED,
                line,
            ));
        }
    }
    let col_w = (w - 32.0) / 2.0;
    let on_page = s
        .profiles
        .len()
        .saturating_sub(page * PROFILES_PER_PAGE)
        .min(PROFILES_PER_PAGE);
    for (k, (i, name)) in s
        .profiles
        .iter()
        .enumerate()
        .skip(page * PROFILES_PER_PAGE)
        .take(PROFILES_PER_PAGE)
        .enumerate()
    {
        let cx = x + (k % 2) as f32 * (col_w + 32.0);
        let ry = y + (k / 2) as f32 * 62.0;
        let active = s.active_profile == Some(i);
        let (save_w, del_w) = (170.0, 52.0);
        let name_w = col_w - save_w - del_w - 20.0;
        let style = match (connected, active) {
            (false, _) => Style::Disabled,
            (true, true) => Style::Selected,
            (true, false) => Style::Normal,
        };
        let apply = Action::ApplyProfile(i);
        let mut b = name_button(
            rect(cx, ry, name_w, 52.0),
            name,
            19.0,
            style,
            connected.then_some(apply),
        );
        if let Item::Button { key, .. } = &mut b {
            *key = Some(apply);
        }
        items.push(b);
        let tools = rect(cx + name_w + 10.0, ry, save_w + del_w + 10.0, 52.0);
        if s.confirm_delete == Some(i) {
            items.push(button(
                tools,
                l.t("headset-profile-delete-confirm"),
                Style::Armed,
                Some(Action::DeleteProfile(i)),
            ));
            continue;
        }
        let save = Action::SaveProfile(i);
        let armed = s.confirm_save == Some(i);
        let mut b = button(
            rect(tools.left(), ry, save_w, 52.0),
            if armed {
                l.t("headset-profile-replace")
            } else {
                l.t("headset-profile-save-here")
            },
            match (can_save, armed) {
                (false, _) => Style::Disabled,
                (true, true) => Style::Armed,
                (true, false) => Style::Normal,
            },
            can_save.then_some(save),
        );
        if let Item::Button { key, .. } = &mut b {
            *key = Some(save);
        }
        items.push(b);
        items.push(button(
            rect(tools.right() - del_w, ry, del_w, 52.0),
            "×",
            Style::Danger,
            Some(Action::DeleteProfile(i)),
        ));
    }
    y += on_page.div_ceil(2) as f32 * 62.0;
    if s.profiles.is_empty() {
        y += 90.0;
    }
    if full {
        items.push(text(
            x,
            y + 10.0,
            18.0,
            false,
            MUTED,
            &l.f(
                "headset-profiles-at-most",
                &[("count", crate::profiles::MAX_PROFILES.into())],
            ),
        ));
        y += 34.0;
    }
    for (k, line) in wrap(l.t("headset-profiles-rename-hint"), 17.0, w)
        .iter()
        .enumerate()
    {
        items.push(text(
            x,
            y + 20.0 + k as f32 * 26.0,
            17.0,
            false,
            MUTED,
            line,
        ));
    }
    in_column(&mut items[at..], x, w);
}

/// Splits a text at words into lines: `wrap(text, size, room)` (see
/// [`Panel::wrapped`]), for the pages built without the fonts at hand.
type Wrap<'a> = &'a dyn Fn(&str, f32, f32) -> Vec<String>;

/// Settings: the parts listed on the left, the chosen one on the right.
fn build_settings(s: &UiState, section: Section, items: &mut Vec<Item>, top: f32, wrap: Wrap<'_>) {
    let l = &s.texts;
    let (nx, nw) = (44.0, 230.0);
    let sections = [
        (Section::Look, l.t("headset-section-look")),
        (Section::Controls, l.t("headset-section-controls")),
        (Section::Sound, l.t("headset-section-sound")),
        (Section::Language, l.t("headset-section-language")),
        (Section::Pc, l.t("headset-section-pc")),
        (Section::Paired, l.t("headset-section-paired")),
        (Section::Help, l.t("headset-section-help")),
    ];
    // "For development" only where the build has it.
    #[cfg(feature = "preview")]
    let dev = s
        .dev
        .is_some()
        .then(|| (Section::Dev, l.t("headset-section-dev")));
    #[cfg(not(feature = "preview"))]
    let dev = None;
    for (i, (sec, label)) in sections.into_iter().chain(dev).enumerate() {
        let r = rect(nx, top + i as f32 * 60.0, nw, 52.0);
        if sec == section {
            items.push(button(r, label, Style::TabOn, None));
        } else {
            items.push(button(
                r,
                label,
                Style::TabOff,
                Some(Action::Tab(Tab::Settings(sec))),
            ));
        }
    }
    // A thin line between the list and the content.
    items.push(Item::Box(
        rect(nx + nw + 20.0, top, 2.0, 640.0),
        [255, 255, 255, 14],
    ));
    let (cx, cw) = (nx + nw + 50.0, WIDTH as f32 - (nx + nw + 50.0) - 44.0);
    let ctop = top + 12.0;
    let at = items.len();
    settings_page(s, section, items, cx, cw, ctop, wrap);
    in_column(&mut items[at..], cx, cw);
}

/// The chosen part of Settings, in the column from `cx`, `cw` wide.
fn settings_page(
    s: &UiState,
    section: Section,
    items: &mut Vec<Item>,
    cx: f32,
    cw: f32,
    ctop: f32,
    wrap: Wrap<'_>,
) {
    let l = &s.texts;
    match section {
        Section::Look => {
            let col = cw.min(640.0);
            build_look(s, items, cx, col, cx, col, ctop);
        }
        Section::Sound => build_sound(s, items, cx, cw.min(620.0), ctop - 12.0),
        Section::Language => {
            let w = cw.min(620.0);
            let mut y = ctop;
            let chosen = s.settings.language;
            let choice = |label, language: Language| {
                (label, Action::Language(language), chosen == language, true)
            };
            segmented(
                items,
                cx,
                w,
                &mut y,
                l.t("headset-language-title"),
                vec![
                    choice(l.t("headset-language-like-pc"), Language::LikePc),
                    choice(l.t("headset-language-like-system"), Language::LikeSystem),
                ],
            );
            // Every language there is a file for, by the name it calls itself, three
            // to a row, three rows at most: with more, eight at a time and "More…" to
            // the next ones.
            let all = &s.languages;
            let (shown, more) = if all.len() <= 9 {
                (&all[..], None)
            } else {
                let pages = all.len().div_ceil(8);
                let page = usize::from(s.language_page) % pages;
                let next = u8::try_from((page + 1) % pages).unwrap_or(0);
                let end = (page * 8 + 8).min(all.len());
                (&all[page * 8..end], Some(next))
            };
            let mut options: Vec<_> = shown
                .iter()
                .map(|k| choice(k.name.as_str(), Language::Fixed(k.lang)))
                .collect();
            if let Some(next) = more {
                options.push((
                    l.t("headset-language-more"),
                    Action::LanguagePage(next),
                    false,
                    true,
                ));
            }
            for row in options.chunks(3) {
                let options = row.to_vec();
                // The buttons of a shorter last row as wide as those of a full one.
                let k = options.len() as f32;
                let width = k * (w - 20.0) / 3.0 + 10.0 * (k - 1.0);
                segmented(items, cx, width, &mut y, "", options);
            }
            // The clock and date on the palm menu, each choice shown as it looks.
            let st = s.settings;
            let example = Moment::EXAMPLE;
            let clocks = [false, true].map(|twelve| l.clock(twelve, &example));
            segmented(
                items,
                cx,
                w,
                &mut y,
                l.t("headset-time"),
                vec![
                    (
                        clocks[0].as_str(),
                        Action::Clock12h(false),
                        !st.clock_12h,
                        true,
                    ),
                    (
                        clocks[1].as_str(),
                        Action::Clock12h(true),
                        st.clock_12h,
                        true,
                    ),
                ],
            );
            use crate::settings::DateStyle;
            let styles = [DateStyle::Ukrainian, DateStyle::English, DateStyle::Numbers];
            let dates = styles.map(|style| l.date(style, &example));
            let mut options = vec![(
                l.t("headset-date-like-menu"),
                Action::DateStyle(DateStyle::LikeMenu),
                st.date_style == DateStyle::LikeMenu,
                true,
            )];
            options.extend(styles.iter().zip(&dates).map(|(&style, label)| {
                (
                    label.as_str(),
                    Action::DateStyle(style),
                    st.date_style == style,
                    true,
                )
            }));
            segmented(items, cx, w, &mut y, l.t("headset-date"), options);
        }
        Section::Pc => {
            let w = cw.min(700.0);
            let mut y = build_pc(s, items, cx, w, ctop);
            // Two switches side by side.
            let half = (w - 16.0) / 2.0;
            let mut y2 = y;
            let on = s.settings.notifications;
            segmented(
                items,
                cx,
                half,
                &mut y,
                l.t("headset-pc-notifications"),
                vec![
                    (l.t("headset-off"), Action::Notifications(false), !on, true),
                    (l.t("headset-on"), Action::Notifications(true), on, true),
                ],
            );
            let on = s.settings.no_limits;
            segmented(
                items,
                cx + half + 16.0,
                half,
                &mut y2,
                l.t("headset-no-limits"),
                vec![
                    (l.t("headset-off"), Action::NoLimits(false), !on, true),
                    (l.t("headset-on"), Action::NoLimits(true), on, true),
                ],
            );
            // What it costs, under its own switch.
            for (k, line) in wrap(l.t("headset-no-limits-note"), 17.0, half)
                .iter()
                .enumerate()
            {
                items.push(text(
                    cx + half + 16.0,
                    y2 - 18.0 + k as f32 * 23.0,
                    17.0,
                    false,
                    MUTED,
                    line,
                ));
            }
        }
        Section::Paired => paired_pcs(s, items, cx, cw.min(700.0), ctop + 10.0),
        Section::Controls => build_controls(s, items, cx, cw.min(640.0), ctop),
        #[cfg(feature = "preview")]
        Section::Dev => {
            if let Some(dev) = s.dev {
                build_dev(s, dev, items, cx, cw.min(700.0), ctop);
            }
        }
        Section::Help => {
            hints(l, items, cx, ctop + 10.0);
            items.push(button(
                rect(cx, ctop + 170.0, 300.0, 50.0),
                l.t("headset-help-show-tips"),
                Style::Normal,
                Some(Action::ShowTips),
            ));
            items.push(text(
                cx,
                ctop + 270.0,
                19.0,
                false,
                MUTED,
                &format!("Prostir {} · {SITE}", env!("CARGO_PKG_VERSION")),
            ));
        }
    }
}

/// Hands, the mouse, the menu's size and which hand does what.
fn build_controls(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    let l = &s.texts;
    let st = s.settings;
    let (lx, lw) = (x, w);
    // Without tracked hands there is nothing to choose for them.
    if s.hands_available {
        segmented(
            items,
            lx,
            lw,
            &mut y,
            l.t("headset-hands"),
            vec![
                (
                    l.t("headset-hide"),
                    Action::Hands(HandMode::Hidden),
                    st.hands == HandMode::Hidden,
                    true,
                ),
                (
                    l.t("headset-show"),
                    Action::Hands(HandMode::Visible),
                    st.hands == HandMode::Visible,
                    true,
                ),
                (
                    l.t("headset-hands-use"),
                    Action::Hands(HandMode::Control),
                    st.hands == HandMode::Control,
                    true,
                ),
            ],
        );
    }
    let on_off = |on: bool, act: fn(bool) -> Action, enabled: bool| {
        vec![
            (l.t("headset-off"), act(false), !on, enabled),
            (l.t("headset-on"), act(true), on, enabled),
        ]
    };
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-gaze-mouse"),
        on_off(st.gaze_mouse, Action::GazeMouse, s.can_move_mouse),
    );
    items.push(text(
        lx,
        y - 18.0,
        18.0,
        false,
        MUTED,
        if s.can_move_mouse || s.host.is_none() {
            l.t("headset-gaze-mouse-note")
        } else {
            l.t("headset-gaze-mouse-unavailable")
        },
    ));
    y += 22.0;
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-menu-size"),
        vec![
            (
                l.t("headset-menu-size-normal"),
                Action::LargeMenu(false),
                !st.large_menu,
                true,
            ),
            (
                l.t("headset-menu-size-large"),
                Action::LargeMenu(true),
                st.large_menu,
                true,
            ),
        ],
    );
    // The palm menu needs tracked hands.
    if s.hands_available {
        segmented(
            items,
            lx,
            lw,
            &mut y,
            l.t("headset-palm-menu"),
            vec![
                (
                    l.t("headset-palm-menu-left"),
                    Action::LeftHanded(false),
                    !st.left_handed,
                    true,
                ),
                (
                    l.t("headset-palm-menu-right"),
                    Action::LeftHanded(true),
                    st.left_handed,
                    true,
                ),
            ],
        );
    }
    // The rates the runtime offers, by the rule; none to choose from: hidden.
    if let Some((usual, saver)) = s.display_rates {
        let usual_text = l.f("headset-display-rate-normal", &[("hz", usual.into())]);
        let saver_text = l.f("headset-display-rate-saver", &[("hz", saver.into())]);
        segmented(
            items,
            lx,
            lw,
            &mut y,
            l.t("headset-display-rate"),
            vec![
                (
                    usual_text.as_str(),
                    Action::BatterySaver(false),
                    !st.battery_saver,
                    true,
                ),
                (
                    saver_text.as_str(),
                    Action::BatterySaver(true),
                    st.battery_saver,
                    true,
                ),
            ],
        );
        // What the display shows now, when that is not what was chosen (the system
        // changed it); else why the slower rate loses nothing.
        let chosen = if st.battery_saver { saver } else { usual };
        let note = match s.display_rate_now {
            Some(now) if now != chosen => l.f("headset-display-rate-now", &[("hz", now.into())]),
            _ => l.f("headset-display-rate-note", &[("hz", saver.into())]),
        };
        items.push(text(lx, y - 18.0, 18.0, false, MUTED, &note));
        y += 22.0;
    }
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-menu-button"),
        vec![
            (
                l.t("headset-show"),
                Action::MenuButton(true),
                st.menu_button,
                true,
            ),
            (
                l.t("headset-hide"),
                Action::MenuButton(false),
                !st.menu_button,
                true,
            ),
        ],
    );
}

/// The PCs this headset is paired with, each with "Forget" (pressed twice).
fn paired_pcs(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    let l = &s.texts;
    items.push(text(x, y, 21.0, true, SECTION, l.t("headset-paired-pcs")));
    y += 14.0;
    if s.paired.is_empty() {
        items.push(text(
            x,
            y + 26.0,
            19.0,
            false,
            MUTED,
            l.t("headset-paired-none"),
        ));
        return;
    }
    // Eight fit; with more, seven and how many others.
    let shown = if s.paired.len() > 8 {
        7
    } else {
        s.paired.len()
    };
    for (i, name) in s.paired.iter().take(shown).enumerate() {
        items.push(Item::Box(rect(x, y, w, 56.0), [255, 255, 255, 10]));
        // Long names are cut, not run under the button.
        let mut label: String = name.chars().take(32).collect();
        if name.chars().count() > 32 {
            label.push('…');
        }
        items.push(text(x + 18.0, y + 36.0, 20.0, true, TEXT, &label));
        let armed = s.confirm_forget == Some(i);
        items.push(button(
            rect(x + w - 200.0, y + 6.0, 192.0, 44.0),
            if armed {
                l.t("headset-paired-forget-confirm")
            } else {
                l.t("headset-paired-forget")
            },
            if armed { Style::Armed } else { Style::Normal },
            Some(Action::ForgetPc(i)),
        ));
        y += 62.0;
    }
    if s.paired.len() > shown {
        items.push(text(
            x,
            y + 20.0,
            18.0,
            false,
            MUTED,
            &l.f(
                "headset-paired-more",
                &[("count", (s.paired.len() - shown).into())],
            ),
        ));
    }
}

/// How the PC encodes: quality, codec, graphics card or CPU, text preset.
fn build_pc(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) -> f32 {
    let l = &s.texts;
    let Some(pc) = &s.pc else {
        items.push(text(
            x,
            y + 20.0,
            21.0,
            false,
            MUTED,
            l.t("headset-pc-not-connected"),
        ));
        return y + 70.0;
    };
    let wanted = pc.wanted;
    let can = |codec: Codec, gpu: bool| pc.choices.contains(&(codec, gpu));
    let any_gpu = pc.choices.iter().any(|(_, gpu)| *gpu);
    let set = |change: EncodingChange| Action::SetEncoding(change);
    let none = EncodingChange::default();
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("headset-pc-quality"),
        [
            (15, l.t("headset-pc-quality-economy")),
            (30, l.t("headset-pc-quality-normal")),
            (50, l.t("headset-pc-quality-high")),
        ]
        .into_iter()
        .map(|(q, label)| {
            let change = EncodingChange {
                bitrate: Some(q),
                ..none
            };
            (label, set(change), wanted.bitrate == q, true)
        })
        .collect(),
    );
    let gpu = !wanted.software;
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("headset-pc-encoding-on"),
        vec![
            (
                l.t("headset-pc-gpu"),
                set(EncodingChange {
                    software: Some(false),
                    ..none
                }),
                gpu,
                any_gpu,
            ),
            (
                l.t("headset-pc-cpu"),
                set(EncodingChange {
                    software: Some(true),
                    codec: Some(Codec::H264),
                    ..none
                }),
                !gpu,
                can(Codec::H264, false),
            ),
        ],
    );
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("headset-pc-codec"),
        vec![
            (
                "HEVC",
                set(EncodingChange {
                    codec: Some(Codec::H265),
                    ..none
                }),
                wanted.codec == Some(Codec::H265),
                gpu && can(Codec::H265, true),
            ),
            (
                "H.264",
                set(EncodingChange {
                    codec: Some(Codec::H264),
                    ..none
                }),
                wanted.codec == Some(Codec::H264),
                can(Codec::H264, true) || can(Codec::H264, false),
            ),
        ],
    );
    items.push(text(
        x,
        y - 18.0,
        18.0,
        false,
        MUTED,
        l.t("headset-pc-codec-note"),
    ));
    y += 22.0;
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("headset-pc-sharp"),
        vec![
            (
                l.t("headset-off"),
                set(EncodingChange {
                    sharp: Some(false),
                    ..none
                }),
                !wanted.sharp,
                gpu,
            ),
            (
                l.t("headset-on"),
                set(EncodingChange {
                    sharp: Some(true),
                    ..none
                }),
                wanted.sharp,
                gpu,
            ),
        ],
    );
    y
}

/// "For development" (the preview build): how large the monitors are made and "Reset",
/// the layers' filters, the numbers next to the monitor looked at, and the PC's
/// settings that change how text looks, side by side.
#[cfg(feature = "preview")]
fn build_dev(s: &UiState, v: DevView, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    let l = &s.texts;
    let dev = v.dev;
    let (half, gap) = ((w - 16.0) / 2.0, 16.0);
    let size = |label, size: MonitorSize, known: bool| {
        (label, Action::DevSize(size), dev.size == size, known)
    };
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("headset-dev-size"),
        vec![
            size(
                l.t("headset-dev-size-standard"),
                MonitorSize::Standard,
                true,
            ),
            size(l.t("headset-dev-size-one"), MonitorSize::OneToOne, v.known),
            size(l.t("headset-dev-size-main"), MonitorSize::MainOnly, v.known),
            size(l.t("headset-dev-size-85"), MonitorSize::Scale85, v.known),
        ],
    );
    if !v.known {
        items.push(text(
            x,
            y - 24.0,
            17.0,
            false,
            MUTED,
            l.t("headset-dev-unknown"),
        ));
        y += 16.0;
    }
    let filter = |title, now: Filter, make: fn(Filter) -> Action| {
        let option = |label, f: Filter| (label, make(f), now == f, true);
        (
            title,
            vec![
                option(l.t("headset-dev-auto"), Filter::Auto),
                option(l.t("headset-on"), Filter::On),
                option(l.t("headset-off"), Filter::Off),
            ],
        )
    };
    let (title, options) = filter(
        l.t("headset-dev-super-sampling"),
        dev.super_sampling,
        Action::DevSuperSampling,
    );
    segmented(items, x, w, &mut y, title, options);
    let (title, options) = filter(
        l.t("headset-dev-sharpening"),
        dev.sharpening,
        Action::DevSharpening,
    );
    segmented(items, x, w, &mut y, title, options);
    // The numbers, and "Reset" of the size on their right.
    let top = y;
    let on = dev.metrics;
    segmented(
        items,
        x,
        half,
        &mut y,
        l.t("headset-dev-metrics"),
        vec![
            (l.t("headset-off"), Action::DevMetrics(false), !on, true),
            (l.t("headset-on"), Action::DevMetrics(true), on, true),
        ],
    );
    items.push(button(
        rect(x + half + gap, top + 18.0, half, 48.0),
        l.t("headset-dev-reset"),
        if v.can_reset {
            Style::Normal
        } else {
            Style::Disabled
        },
        v.can_reset.then_some(Action::DevSizeReset),
    ));
    // The PC's settings that change how text looks, as on its own page.
    let Some(pc) = &s.pc else {
        items.push(text(
            x,
            y + 10.0,
            19.0,
            false,
            MUTED,
            l.t("headset-pc-not-connected"),
        ));
        return;
    };
    let wanted = pc.wanted;
    let can = |codec: Codec, gpu: bool| pc.choices.contains(&(codec, gpu));
    let gpu = !wanted.software;
    let none = EncodingChange::default();
    let set = Action::SetEncoding;
    const RATES: [u16; 3] = [33, 60, 100];
    let rates: Vec<String> = RATES
        .iter()
        .map(|&q| l.f("headset-dev-mbit", &[("rate", q.into())]))
        .collect();
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("headset-pc-quality"),
        RATES
            .into_iter()
            .zip(&rates)
            .map(|(q, label)| {
                let change = EncodingChange {
                    bitrate: Some(q),
                    ..none
                };
                (label.as_str(), set(change), wanted.bitrate == q, true)
            })
            .collect(),
    );
    let mut y2 = y;
    segmented(
        items,
        x,
        half,
        &mut y,
        l.t("headset-pc-codec"),
        vec![
            (
                "HEVC",
                set(EncodingChange {
                    codec: Some(Codec::H265),
                    ..none
                }),
                wanted.codec == Some(Codec::H265),
                gpu && can(Codec::H265, true),
            ),
            (
                "H.264",
                set(EncodingChange {
                    codec: Some(Codec::H264),
                    ..none
                }),
                wanted.codec == Some(Codec::H264),
                can(Codec::H264, true) || can(Codec::H264, false),
            ),
        ],
    );
    segmented(
        items,
        x + half + gap,
        half,
        &mut y2,
        l.t("headset-pc-sharp"),
        vec![
            (
                l.t("headset-off"),
                set(EncodingChange {
                    sharp: Some(false),
                    ..none
                }),
                !wanted.sharp,
                gpu,
            ),
            (
                l.t("headset-on"),
                set(EncodingChange {
                    sharp: Some(true),
                    ..none
                }),
                wanted.sharp,
                gpu,
            ),
        ],
    );
}

/// PC sound and headset microphone.
fn build_sound(s: &UiState, items: &mut Vec<Item>, lx: f32, lw: f32, mut y: f32) {
    let l = &s.texts;
    // The current state is the highlighted option, not a button label to decode.
    y += 18.0;
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-sound-plays"),
        vec![
            (
                l.t("headset-sound-in-headset"),
                Action::SetAudio(true),
                s.audio_wanted,
                true,
            ),
            (
                l.t("headset-sound-on-pc"),
                Action::SetAudio(false),
                !s.audio_wanted,
                true,
            ),
        ],
    );
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-sound-apps-hear"),
        vec![
            (
                l.t("headset-sound-mic-headset"),
                Action::SetMic(true),
                s.mic_wanted,
                true,
            ),
            (
                l.t("headset-sound-mic-pc"),
                Action::SetMic(false),
                !s.mic_wanted,
                true,
            ),
        ],
    );
    y -= 24.0;
    if s.mic_denied {
        for line in [
            l.t("headset-sound-mic-denied-1"),
            l.t("headset-sound-mic-denied-2"),
        ] {
            items.push(text(lx, y, 19.0, false, DANGER, line));
            y += 30.0;
        }
        items.push(button(
            rect(lx, y - 8.0, 260.0, 48.0),
            l.t("headset-sound-ask-again"),
            Style::Normal,
            Some(Action::AskMic),
        ));
        y += 52.0;
    }
    y += 24.0;
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("headset-sound-from-screen"),
        vec![
            (
                l.t("headset-off"),
                Action::SoundFromScreen(false),
                !s.settings.sound_from_screen,
                true,
            ),
            (
                l.t("headset-on"),
                Action::SoundFromScreen(true),
                s.settings.sound_from_screen,
                true,
            ),
        ],
    );
    items.push(text(
        lx,
        y - 18.0,
        18.0,
        false,
        MUTED,
        l.t("headset-sound-from-screen-note"),
    ));
    y += 30.0;
    for line in [
        l.t("headset-sound-about-1"),
        l.t("headset-sound-about-2"),
        l.t("headset-sound-about-3"),
        l.t("headset-sound-about-4"),
    ] {
        items.push(text(lx, y, 19.0, false, MUTED, line));
        y += 30.0;
    }
}

/// The smallest a text or label is drawn to make it fit.
const SMALLEST: f32 = 13.0;

fn button_font_size(label: &str) -> f32 {
    match label {
        "×" => 32.0,
        "‹" | "›" => 34.0,
        l if l.len() == 1 => 26.0,
        _ => 19.0,
    }
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    a.left() < b.right() && b.left() < a.right() && a.top() < b.bottom() && b.top() < a.bottom()
}

/// The smallest rectangle holding both.
fn union(a: &Rect, b: &Rect) -> Rect {
    let (l, t) = (a.left().min(b.left()), a.top().min(b.top()));
    let (r, bottom) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    rect(l, t, r - l, bottom - t)
}

// Shapes that come out degenerate (negative or non-finite sizes) are a bug, caught
// by the debug asserts in tests. In the app they are drawn as nothing: one bad shape
// must not close the menu, and with it the app.

/// A circle, `None` if degenerate.
fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    let path = (r > 0.0 && r.is_finite())
        .then(|| {
            let n = (r * std::f32::consts::TAU / 3.0).ceil().clamp(12.0, 128.0) as usize;
            let mut pb = PathBuilder::new();
            pb.move_to(cx + r, cy);
            arc(&mut pb, (cx, cy), r, 0.0, std::f32::consts::TAU, n);
            pb.close();
            pb.finish()
        })
        .flatten();
    debug_assert!(
        path.is_some(),
        "degenerate circle at {cx}, {cy}, radius {r}"
    );
    path
}

/// A rectangle; a degenerate one becomes an empty one at a finite spot.
fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    let r = Rect::from_xywh(x, y, w, h);
    debug_assert!(r.is_some(), "degenerate rect at {x}, {y}, size {w} x {h}");
    r.unwrap_or_else(|| {
        let finite = |v: f32| {
            if v.is_finite() {
                v.clamp(-1e6, 1e6)
            } else {
                0.0
            }
        };
        let (x, y, w, h) = (finite(x), finite(y), finite(w), finite(h));
        Rect::from_xywh(x, y, w.max(0.0), h.max(0.0)).expect("finite and bounded")
    })
}

fn text(x: f32, baseline: f32, size: f32, medium: bool, color: [u8; 4], s: &str) -> Item {
    Item::Text {
        x,
        baseline,
        size,
        medium,
        color,
        text: s.to_owned(),
        room: f32::INFINITY,
        shorten: false,
    }
}

/// The texts among `items` with no room of their own stay in the column from `x`, `w`
/// wide: a longer one is drawn smaller rather than run into the next column.
fn in_column(items: &mut [Item], x: f32, w: f32) {
    for item in items {
        if let Item::Text { x: at, room, .. } = item
            && room.is_infinite()
        {
            *room = (x + w - *at).max(0.0);
        }
    }
}

/// A text that may be at most `room` wide (in a column, on a tile).
fn text_in(room: f32, item: Item) -> Item {
    match item {
        Item::Text {
            x,
            baseline,
            size,
            medium,
            color,
            text,
            shorten,
            ..
        } => Item::Text {
            x,
            baseline,
            size,
            medium,
            color,
            text,
            room,
            shorten,
        },
        other => other,
    }
}

fn button(rect: Rect, label: &str, style: Style, action: Option<Action>) -> Item {
    Item::Button {
        rect,
        label: label.to_owned(),
        size: button_font_size(label),
        style,
        action,
        full: None,
        key: action,
    }
}

/// A button labelled with a monitor's `name` at `size`, centred on it: smaller where
/// it does not fit, then shortened with an ellipsis (see [`Panel::fit`]).
fn name_button(rect: Rect, name: &str, size: f32, style: Style, action: Option<Action>) -> Item {
    Item::Button {
        rect,
        label: name.to_owned(),
        size,
        style,
        action,
        full: Some(name.to_owned()),
        key: action,
    }
}

/// A rounded rectangle, `None` if degenerate.
fn rounded(r: Rect, radius: f32) -> Option<Path> {
    use std::f32::consts::{FRAC_PI_2, PI};
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0).max(0.0);
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    // Each corner a few straight steps (a tenth of a pixel off the arc at most).
    let n = (rad / 1.5).ceil().clamp(2.0, 24.0) as usize;
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    arc(&mut pb, (rt - rad, t + rad), rad, -FRAC_PI_2, 0.0, n);
    pb.line_to(rt, b - rad);
    arc(&mut pb, (rt - rad, b - rad), rad, 0.0, FRAC_PI_2, n);
    pb.line_to(l + rad, b);
    arc(&mut pb, (l + rad, b - rad), rad, FRAC_PI_2, PI, n);
    pb.line_to(l, t + rad);
    arc(&mut pb, (l + rad, t + rad), rad, PI, 1.5 * PI, n);
    pb.close();
    let path = pb.finish();
    debug_assert!(path.is_some(), "degenerate rounded rect {r:?}");
    path
}

/// An arc around `center` of radius `r` from angle `a0` to `a1` (radians, clockwise on
/// the screen from the right), in `n` straight steps, its start already in `pb`.
/// Straight steps rather than curves: a shape cut at the edge of a part of the picture
/// is then drawn there as it is in the whole picture (see `Panel::draw_part`).
fn arc(pb: &mut PathBuilder, (cx, cy): (f32, f32), r: f32, a0: f32, a1: f32, n: usize) {
    for i in 1..=n {
        let a = a0 + (a1 - a0) * i as f32 / n as f32;
        pb.line_to(cx + r * a.cos(), cy + r * a.sin());
    }
}

/// Outlines `path`; nothing when there is none (a degenerate shape).
fn stroke(c: &mut Canvas<'_>, path: Option<Path>, [r, g, b, a]: [u8; 4], width: f32) {
    let Some(path) = path else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    let t = c.transform();
    c.pm.stroke_path(&path, &paint, &stroke, t, None);
}

/// Fills `path`; nothing when there is none (a degenerate shape).
fn fill(c: &mut Canvas<'_>, path: Option<Path>, [r, g, b, a]: [u8; 4]) {
    let Some(path) = path else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    let t = c.transform();
    c.pm.fill_path(&path, &paint, FillRule::Winding, t, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display `n` of the one PC of these tests.
    fn k(n: u32) -> DisplayKey {
        DisplayKey::new(crate::ids::PcId([7; 8]), prostir_proto::state::DisplayId(n))
    }
    use crate::platform::store::{NoStore, Store as _};

    /// What a PC with an H.265 decoder on the headset offers.
    pub fn sample_limits() -> Limits {
        let mode = prostir_proto::state::Mode::new;
        Limits {
            virtual_max: 6,
            windows_max: 3,
            physical_max: 1,
            streams_max: 16,
            min: (640, 480),
            max: (5120, 2880),
            sizes: vec![
                mode(1920, 1080),
                mode(2560, 1440),
                mode(3840, 2160),
                mode(3440, 1440),
                mode(5120, 1440),
                mode(1440, 2560),
            ],
            scales: vec![100, 125, 150],
        }
    }

    /// The built-in languages, as the menu lists them.
    fn languages() -> std::sync::Arc<[crate::lang::Known]> {
        ["en", "uk"]
            .iter()
            .map(|code| {
                let texts = crate::lang::builtin(code);
                crate::lang::Known {
                    lang: texts.lang(),
                    name: texts.t(prostir_lang::NAME_ID).to_owned(),
                }
            })
            .collect()
    }

    /// The longer pseudo-language (see [`crate::lang::pseudo`]).
    fn pseudo() -> Texts {
        let qps: &'static str =
            Box::leak(crate::lang::pseudo(crate::lang::BUILTIN[0].1).into_boxed_str());
        let mut loader =
            crate::lang::Loader::with(&[crate::lang::BUILTIN[0], ("qps", qps)], Vec::new());
        #[expect(clippy::disallowed_methods, reason = "built-in files only, in a test")]
        let texts = loader.load(crate::lang::Lang::new("qps").expect("a code"));
        texts
    }

    /// "For development", where the build has it.
    fn dev_tab() -> Option<Tab> {
        #[cfg(feature = "preview")]
        return Some(Tab::Settings(Section::Dev));
        #[cfg(not(feature = "preview"))]
        None
    }

    pub fn sample_state() -> UiState {
        UiState {
            pairing: None,
            unreachable: Some("workstation-with-a-long-name".into()),
            pair_offer: None,
            mic_denied: true,
            limits: Some(sample_limits()),
            pc: Some(Encoding {
                wanted: prostir_proto::state::Wanted {
                    bitrate: 30,
                    codec: Some(Codec::H265),
                    software: false,
                    sharp: true,
                },
                using: Some((Codec::H265, true)),
                choices: vec![
                    (Codec::H265, true),
                    (Codec::H264, true),
                    (Codec::H264, false),
                ],
            }),
            can_move_mouse: false,
            paired: vec!["workstation".into(), "a-much-longer-laptop-name".into()],
            battery: Some((100, true)),
            clock: "12:59 PM".into(),
            date: "Wed, Nov 18".into(),
            confirm_forget: Some(1),
            windows: vec![MonitorEntry {
                id: k(110),
                label: "5".into(),
                width: 1200,
                height: 800,
                place: Place::Unknown,
                seen: None,
                scale: 100,
                picture: Picture::Shows,
            }],
            host: Some("workstation".into()),
            found: Vec::new(),
            ip_input: None,
            hint: None,
            monitors: vec![
                MonitorEntry {
                    id: k(101),
                    label: "1".into(),
                    width: 2560,
                    height: 1440,
                    place: Place::Unknown,
                    seen: Some([-1.0, 0.0, 0.5, 0.28]),
                    scale: 100,
                    picture: Picture::Shows,
                },
                MonitorEntry {
                    id: k(102),
                    label: "2".into(),
                    width: 2560,
                    height: 1440,
                    place: Place::Unknown,
                    seen: Some([0.0, 0.0, 0.5, 0.28]),
                    scale: 100,
                    picture: Picture::Shows,
                },
                MonitorEntry {
                    id: k(103),
                    label: "4".into(),
                    width: 1440,
                    height: 2560,
                    place: Place::Unknown,
                    seen: Some([0.78, 0.12, 0.28, 0.5]),
                    scale: 100,
                    picture: Picture::Shows,
                },
            ],
            selected: None,
            physical_seen: vec![([0.0, -0.55, 0.35, 0.22], "DELL U2720Q".into())],
            physical_page: 0,
            physical: vec![
                PhysicalEntry {
                    id: k(100),
                    name: "Екран ноутбука".into(),
                    connector: Some("eDP-1".into()),
                    shown: false,
                    connected: false,
                },
                PhysicalEntry {
                    id: k(117),
                    name: "DELL U2720Q".into(),
                    connector: Some("HDMI-A-1".into()),
                    shown: true,
                    connected: true,
                },
            ],
            primary: Some(k(102)),
            cinema_screen: None,
            audio_wanted: true,
            mic_wanted: true,
            settings: Settings::default(),
            room_available: true,
            panorama_available: true,
            custom_available: false,
            curve_available: true,
            keyboard_available: true,
            hands_available: true,
            display_rates: Some((90, 72)),
            display_rate_now: Some(90),
            error: None,
            confirm_remove: None,
            confirm_save: None,
            tab: Tab::Monitors,
            texts: crate::lang::builtin("uk"),
            languages: languages(),
            language_page: 0,
            intro: false,
            update: None,
            update_from: NoStore.update_hint(),
            profiles: vec!["Профіль 1".into(), "Робота".into(), "Профіль 3".into()],
            profile_page: 0,
            confirm_delete: None,
            can_scale: true,
            add_scale: 100,
            active_profile: None,
            note: None,
            dev: Some(DevView {
                dev: Dev::default(),
                known: true,
                can_reset: true,
            }),
        }
    }

    /// The menu names monitors as the PC's desktop does (`Prostir-4` is 4), never by
    /// their ids (101, 102, 103 here): on every page, picked or not, in cinema mode.
    #[test]
    fn labels_never_show_ids() {
        assert_eq!(display_label("Prostir-4"), "4");
        assert_eq!(display_label("eDP-1"), "eDP-1");
        assert_eq!(display_label("Prostir-"), "Prostir-");
        let texts = |s: &UiState| -> Vec<String> {
            Panel::new()
                .build(s)
                .into_iter()
                .filter_map(|i| match i {
                    Item::Text { text, .. } => Some(text),
                    Item::Button { label, .. } | Item::Wedge { label, .. } => Some(label),
                    _ => None,
                })
                .collect()
        };
        let mut seen_labels = false;
        for tab in [Tab::Quick, Tab::Monitors, Tab::Settings(Section::Look)] {
            for selected in [None, Some(k(101)), Some(k(103))] {
                for cinema in [false, true] {
                    let mut s = sample_state();
                    s.tab = tab;
                    s.selected = selected;
                    s.settings.cinema = cinema;
                    // The fourth one, off the map, shows as a small button.
                    s.monitors.push(MonitorEntry {
                        id: k(107),
                        label: "6".into(),
                        seen: None,
                        scale: 100,
                        ..s.monitors[0].clone()
                    });
                    for t in texts(&s) {
                        for id in ["101", "102", "103", "107", "110"] {
                            assert!(!t.contains(id), "{t:?} shows id {id}");
                        }
                        seen_labels |= t == "Монітор 4" || t == "6";
                    }
                }
            }
        }
        assert!(seen_labels, "the labels are shown");
    }

    /// The placeholder of a monitor being added is drawn at the monitor's own shape,
    /// wide or tall.
    #[test]
    fn a_placeholder_takes_the_shape_of_its_monitor() {
        let mut p = Panel::new();
        for (w, h) in [(1024, 288), (1024, 576), (576, 1024)] {
            let image = p.placeholder_image("Adding a monitor…", w, h);
            assert_eq!(image.len(), (w * h * 4) as usize);
            // The middle is drawn (the text), the corner is not.
            assert!(image.iter().any(|b| *b != 0));
            assert_eq!(&image[..4], &[0, 0, 0, 0], "a rounded corner");
        }
    }

    #[test]
    fn hover_and_click() {
        let mut p = Panel::new();
        p.set_state(&sample_state());
        assert!(p.render_if_dirty().is_some());
        assert!(p.render_if_dirty().is_none());
        // Close button in the top-right corner.
        p.hover(Some((WIDTH as f32 - 60.0, 56.0)));
        assert_eq!(p.hovered_action(), Some(Action::Close));
        assert!(p.render_if_dirty().is_some());
        p.hover(None);
        assert_eq!(p.hovered_action(), None);
    }

    /// The sizes to add are the PC's, and none can be added beyond its limit.
    #[test]
    fn sizes_come_from_the_pc() {
        let adds = |s: &UiState| -> Vec<(String, bool)> {
            Panel::new()
                .build(s)
                .into_iter()
                .filter_map(|i| match i {
                    Item::Button {
                        label,
                        action: a @ (Some(Action::Add { .. }) | None),
                        ..
                    } if label.contains('×') && !label.contains(' ') => Some((label, a.is_some())),
                    _ => None,
                })
                .collect()
        };
        let mut s = sample_state();
        s.tab = Tab::Monitors;
        let offered = adds(&s);
        assert!(offered.contains(&("5120×1440".into(), true)), "{offered:?}");
        assert_eq!(offered.len(), 6);
        s.limits.as_mut().unwrap().virtual_max = s.monitors.len() as u8;
        assert!(adds(&s).iter().all(|(_, on)| !on), "at the PC's limit");
        s.limits = None;
        assert!(adds(&s).iter().all(|(_, on)| !on), "before the PC said");
    }

    /// A display's row: its name, "Shown" (its action where pressable, its style) and
    /// "Hidden".
    type Row = (String, Option<Action>, Style, Option<Action>, Style);

    /// The switches of the PC's own displays, a row each for those that have one.
    fn physical_rows(s: &UiState) -> Vec<Row> {
        let items = Panel::new().build(s);
        s.physical
            .iter()
            .filter_map(|d| {
                let of = |on: bool| {
                    items.iter().find_map(|i| match i {
                        Item::Button {
                            key: Some(Action::ShowPhysical(k, o)),
                            action,
                            style,
                            ..
                        } if *k == d.id && *o == on => Some((*action, *style)),
                        _ => None,
                    })
                };
                let (show, show_style) = of(true)?;
                let (hide, hide_style) = of(false).expect("both halves of the switch");
                Some((d.name.clone(), show, show_style, hide, hide_style))
            })
            .collect()
    }

    /// Every connected display of the PC has its switch, named as the PC names it, its
    /// connector under; one not connected says so, without a switch; with as many
    /// shown as the PC allows, the others' "Shown" waits, saying why; none at all: no
    /// switches.
    #[test]
    fn every_pc_display_has_its_switch() {
        let says = |s: &UiState, what: &str| {
            Panel::new()
                .build(s)
                .iter()
                .any(|i| matches!(i, Item::Text { text, .. } if text == what))
        };
        let mut s = sample_state();
        s.tab = Tab::Monitors;
        let rows = physical_rows(&s);
        assert_eq!(
            rows,
            [(
                "DELL U2720Q".into(),
                None,
                Style::Selected,
                Some(Action::ShowPhysical(k(117), false)),
                Style::Normal
            ),]
        );
        assert!(says(&s, "Екран ноутбука"));
        assert!(says(&s, "eDP-1 · не підключено"));
        assert!(says(&s, "HDMI-A-1"));
        // The laptop's lid opened, the PC allowing one: its "Shown" waits.
        s.physical[0].connected = true;
        let rows = physical_rows(&s);
        assert_eq!((rows[0].1, rows[0].2), (None, Style::Disabled));
        assert!(says(&s, "eDP-1 · досягнуто межі ПК"));
        // Allowing two: it can be shown.
        s.limits.as_mut().unwrap().physical_max = 2;
        let rows = physical_rows(&s);
        assert_eq!(rows[0].1, Some(Action::ShowPhysical(k(100), true)));
        s.physical.clear();
        assert!(!says(
            &s,
            crate::lang::builtin("uk").t("headset-pc-displays")
        ));
    }

    /// A display of the PC shown and then disconnected (the laptop's lid closed) keeps
    /// its switch, so it can still be hidden; "Shown" stays as it is.
    #[test]
    fn a_disconnected_pc_display_left_shown_can_be_hidden() {
        let mut s = sample_state();
        s.tab = Tab::Monitors;
        s.physical[0].shown = true;
        let rows = physical_rows(&s);
        assert_eq!(
            rows[0],
            (
                "Екран ноутбука".into(),
                None,
                Style::Selected,
                Some(Action::ShowPhysical(k(100), false)),
                Style::Normal
            )
        );
    }

    /// More displays of the PC than fit above "Line up": a page at a time, the pages
    /// turned beside the title, round; every one of them can be switched on some page.
    #[test]
    fn every_pc_display_can_be_switched_page_by_page() {
        let mut s = sample_state();
        s.tab = Tab::Monitors;
        s.selected = Some(k(102));
        s.physical = (0..7)
            .map(|n| PhysicalEntry {
                id: k(200 + n),
                name: format!("DP-{n}"),
                connector: None,
                shown: false,
                connected: true,
            })
            .collect();
        let mut switched = std::collections::BTreeSet::new();
        let mut pages = 0;
        for page in 0..4 {
            s.physical_page = page;
            let items = Panel::new().build(&s);
            let turns: Vec<Action> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Button {
                        action: Some(a @ Action::PhysicalPage(_)),
                        ..
                    } => Some(*a),
                    _ => None,
                })
                .collect();
            assert_eq!(turns.len(), 2, "back and on");
            for i in &items {
                if let Item::Button {
                    key: Some(Action::ShowPhysical(id, true)),
                    ..
                } = i
                {
                    switched.insert(*id);
                }
            }
            pages += 1;
            let mut p = Panel::new();
            p.set_state(&s);
            assert!(p.overflows().is_empty(), "{:?}", p.overflows());
        }
        assert_eq!(switched.len(), 7, "after {pages} pages");
    }

    /// The scale: of a monitor picked, where the PC makes monitors at its scales,
    /// its own highlighted; for a monitor added, the one chosen; none where the PC has
    /// one scale or does not name the capability.
    #[test]
    fn scales_where_the_pc_makes_them() {
        let scales = |s: &UiState| -> Vec<(Action, Style)> {
            Panel::new()
                .build(s)
                .into_iter()
                .filter_map(|i| match i {
                    Item::Button {
                        key: Some(a @ (Action::SetScale(..) | Action::AddScale(_))),
                        style,
                        ..
                    } => Some((a, style)),
                    _ => None,
                })
                .collect()
        };
        let mut s = sample_state();
        s.tab = Tab::Monitors;
        s.selected = Some(k(102));
        s.monitors[1].scale = 125;
        s.add_scale = 150;
        let all = scales(&s);
        assert_eq!(all.len(), 6, "{all:?}");
        assert!(all.contains(&(Action::AddScale(150), Style::Selected)));
        assert!(all.contains(&(Action::AddScale(100), Style::Normal)));
        assert!(all.contains(&(Action::SetScale(k(102), 125), Style::Selected)));
        assert!(all.contains(&(Action::SetScale(k(102), 150), Style::Normal)));
        s.can_scale = false;
        assert!(scales(&s).is_empty());
        s.can_scale = true;
        s.limits.as_mut().unwrap().scales = vec![100];
        assert!(scales(&s).is_empty());
    }

    /// Every page and state, in both languages, without text that does not fit.
    #[test]
    fn wedges_are_hit_where_drawn() {
        let w = Item::Wedge {
            cx: 0.0,
            cy: 0.0,
            r0: 10.0,
            r1: 20.0,
            a0: std::f32::consts::TAU - 0.5,
            a1: std::f32::consts::TAU + 0.5,
            label: String::new(),
            size: 10.0,
            style: Style::Normal,
            action: None,
        };
        // Straight up, a little left and right of it; not in the middle or below.
        assert!(w.wedge_contains(0.0, -15.0));
        assert!(w.wedge_contains(3.0, -15.0));
        assert!(w.wedge_contains(-3.0, -15.0));
        assert!(!w.wedge_contains(0.0, -5.0));
        assert!(!w.wedge_contains(0.0, 15.0));
    }

    /// The PC names the monitors ("Selftest 2", or much longer): on the map each name
    /// is centred on its tile, smaller where it does not fit, then shortened with an
    /// ellipsis by whole letters; never past its tile. Picked, the monitor's name shows
    /// in full under the map. In every language, the longer pseudo-language too.
    #[test]
    fn monitor_names_fit_their_tiles() {
        let names = [
            "Selftest 2",
            "Selftest monitor with a much longer name 1",
            "Монітор із дуже довгою назвою на ПК 3",
            "Pre\u{301}sentation wall-12",
        ];
        for lang in [
            crate::lang::builtin("uk"),
            crate::lang::builtin("en"),
            pseudo(),
        ] {
            let mut st = sample_state();
            st.texts = lang;
            st.tab = Tab::Monitors;
            for (m, name) in st.monitors.iter_mut().zip(names) {
                m.label = name.into();
            }
            // One not placed in view: on the row of small buttons under the map.
            st.monitors[2].seen = None;
            st.monitors[2].label = names[3].into();
            st.selected = Some(st.monitors[1].id);
            let mut p = Panel::new();
            p.set_state(&st);
            assert_eq!(p.overflows(), Vec::<String>::new(), "{:?}", st.texts);
            let tiles: Vec<(Rect, String, f32)> = p
                .items
                .iter()
                .filter_map(|i| match i {
                    Item::Button {
                        rect,
                        label,
                        size,
                        full: Some(_),
                        action: Some(Action::SelectMonitor(_)),
                        ..
                    } => Some((*rect, label.clone(), *size)),
                    _ => None,
                })
                .collect();
            assert_eq!(tiles.len(), 3);
            for (r, label, size) in &tiles {
                assert!(!label.is_empty(), "something of it shows");
                let w = p.text_width(label, *size, true);
                assert!(w <= r.width() - 16.0 + 0.5, "{label:?} fits its tile");
                let whole = names.contains(&label.as_str());
                assert!(
                    whole || label.contains('…'),
                    "{label:?}: whole or shortened"
                );
            }
            // The short one whole, the long ones shortened.
            assert!(tiles.iter().any(|t| t.1 == names[0]));
            assert!(
                tiles
                    .iter()
                    .any(|t| t.1.starts_with("Selftest") && t.1.contains('…'))
            );
            assert!(
                tiles
                    .iter()
                    .any(|t| t.1.contains('…') && t.1.ends_with(" 1")),
                "the ending that tells it apart"
            );
            // Pointed at, a shortened one shows its name whole above it.
            let (r, _, _) = tiles.iter().find(|t| t.1.contains('…')).unwrap();
            let (cx, above) = (
                (r.left() + r.width() / 2.0) as usize,
                (r.top() - 25.0) as usize,
            );
            let at = |px: &[u8]| px[(above * WIDTH as usize + cx) * 4..][..4].to_vec();
            let plain = at(p.render_if_dirty().unwrap());
            p.hover(Some((cx as f32, r.top() + r.height() / 2.0)));
            let pointed = at(p.render_if_dirty().unwrap());
            assert_ne!(plain, pointed, "its whole name over the map");
        }
    }

    /// Every layout can be pressed, the chosen one too: pressed again, it lays the
    /// monitors out anew.
    #[test]
    fn the_chosen_layout_can_be_pressed_again() {
        for preset in [Preset::Arc, Preset::Wall, Preset::Overhead] {
            let mut st = sample_state();
            st.tab = Tab::Monitors;
            st.settings.preset = preset;
            let presets: Vec<(Action, Style)> = Panel::new()
                .build(&st)
                .into_iter()
                .filter_map(|i| match i {
                    Item::Button {
                        action: Some(a @ Action::Preset(_)),
                        style,
                        ..
                    } => Some((a, style)),
                    _ => None,
                })
                .collect();
            assert_eq!(presets.len(), 3, "{preset:?}");
            assert!(presets.contains(&(Action::Preset(preset), Style::Selected)));
        }
    }

    /// The buttons of a page standing for a profile: label, style, action.
    fn profile_buttons(st: &UiState) -> Vec<(String, Style, Option<Action>)> {
        Panel::new()
            .build(st)
            .into_iter()
            .filter_map(|i| match i {
                Item::Button {
                    label,
                    style,
                    action,
                    key:
                        Some(
                            Action::ApplyProfile(_)
                            | Action::SaveProfile(_)
                            | Action::DeleteProfile(_)
                            | Action::NewProfile,
                        ),
                    ..
                } => Some((label, style, action)),
                _ => None,
            })
            .collect()
    }

    /// The profile saved to or switched to last is the one highlighted, where it is
    /// switched to (Quick and Profiles); saving over one or deleting one asks first;
    /// a new one is saved from the setup as it is.
    #[test]
    fn the_active_profile_is_highlighted() {
        let mut st = sample_state();
        st.texts = crate::lang::builtin("en");
        st.profiles = vec!["Profile 1".into(), "Work".into()];
        st.active_profile = Some(1);

        st.tab = Tab::Quick;
        let quick = profile_buttons(&st);
        assert_eq!(
            quick,
            [
                (
                    "Profile 1".into(),
                    Style::Normal,
                    Some(Action::ApplyProfile(0))
                ),
                (
                    "Work".into(),
                    Style::Selected,
                    Some(Action::ApplyProfile(1))
                ),
            ]
        );

        st.tab = Tab::Profiles;
        let all = profile_buttons(&st);
        let save = |i| {
            (
                "Save here".to_owned(),
                Style::Normal,
                Some(Action::SaveProfile(i)),
            )
        };
        let delete = |i| {
            (
                "×".to_owned(),
                Style::Danger,
                Some(Action::DeleteProfile(i)),
            )
        };
        assert_eq!(
            all,
            [
                (
                    "New profile".into(),
                    Style::Normal,
                    Some(Action::NewProfile)
                ),
                (
                    "Profile 1".into(),
                    Style::Normal,
                    Some(Action::ApplyProfile(0))
                ),
                save(0),
                delete(0),
                (
                    "Work".into(),
                    Style::Selected,
                    Some(Action::ApplyProfile(1))
                ),
                save(1),
                delete(1),
            ]
        );

        // Pressed once to save over it, or to delete it: it asks.
        st.confirm_save = Some(1);
        assert!(profile_buttons(&st).contains(&(
            "Replace?".into(),
            Style::Armed,
            Some(Action::SaveProfile(1))
        )));
        st.confirm_save = None;
        st.confirm_delete = Some(0);
        assert!(profile_buttons(&st).contains(&(
            "Press again to delete".into(),
            Style::Armed,
            Some(Action::DeleteProfile(0))
        )));
        st.confirm_delete = None;

        // Without monitors nothing is saved; at most twenty.
        let mut none = st.clone();
        none.monitors.clear();
        assert!(
            profile_buttons(&none)
                .iter()
                .filter(|(_, _, a)| matches!(a, Some(Action::NewProfile | Action::SaveProfile(_))))
                .count()
                == 0
        );
        let mut full = st.clone();
        full.profiles = (1..=crate::profiles::MAX_PROFILES)
            .map(|n| format!("Profile {n}"))
            .collect();
        assert_eq!(
            profile_buttons(&full)[0],
            ("New profile".into(), Style::Disabled, None)
        );
        // Ten a page: the second page has the rest, the page buttons say which.
        full.profile_page = 1;
        let second = profile_buttons(&full);
        assert!(second.iter().any(|(l, ..)| l == "Profile 11"));
        assert!(!second.iter().any(|(l, ..)| l == "Profile 10"));

        // None in use (the setup changed by hand, see `app`): none is marked.
        st.tab = Tab::Quick;
        st.active_profile = None;
        assert!(
            profile_buttons(&st)
                .iter()
                .all(|(_, style, _)| *style == Style::Normal)
        );
    }

    #[test]
    fn nothing_overflows() {
        let mut states = Vec::new();
        for lang in [
            crate::lang::builtin("uk"),
            crate::lang::builtin("en"),
            pseudo(),
        ] {
            for tab in [
                Tab::Quick,
                Tab::Monitors,
                Tab::Settings(Section::Look),
                Tab::Settings(Section::Controls),
                Tab::Settings(Section::Sound),
                Tab::Settings(Section::Language),
                Tab::Settings(Section::Pc),
                Tab::Settings(Section::Paired),
                Tab::Settings(Section::Help),
                Tab::Profiles,
            ]
            .into_iter()
            .chain(dev_tab())
            {
                let mut st = sample_state();
                st.texts = lang.clone();
                st.tab = tab;
                states.push(st.clone());
                st.confirm_remove = Some(k(103));
                st.selected = Some(k(103));
                st.settings.cinema = true;
                st.settings.focus = true;
                st.mic_wanted = false;
                st.audio_wanted = false;
                for d in &mut st.physical {
                    d.shown = false;
                    d.connected = true;
                }
                st.confirm_delete = Some(1);
                st.active_profile = Some(1);
                st.error = Some(lang.f("headset-monitors-at-most", &[("count", 6.into())]));
                states.push(st.clone());
                // Twenty languages, every page of them.
                if tab == Tab::Settings(Section::Language) {
                    for page in 0..3 {
                        let mut many = st.clone();
                        many.languages = (0..20)
                            .map(|i| crate::lang::Known {
                                lang: crate::lang::Lang::new(&format!("x{i}")).expect("a code"),
                                name: format!("Мова номер {i}"),
                            })
                            .collect();
                        many.language_page = page;
                        states.push(many);
                    }
                }
                // Many paired PCs.
                let mut many = st.clone();
                many.paired = (0..5).map(|i| format!("pc-{i}")).collect();
                states.push(many);
                // Six monitors.
                st.monitors = (0..6)
                    .map(|id| MonitorEntry {
                        id: k(id),
                        label: (id + 1).to_string(),
                        width: 3440,
                        height: 1440,
                        // The longest name.
                        place: Place::Right,
                        seen: Some([(id as f32 - 2.5) * 0.9, 0.0, 0.45, 0.19]),
                        scale: 100,
                        picture: Picture::Shows,
                    })
                    .collect();
                states.push(st);
            }
            for why in [
                OfferWhy::New,
                OfferWhy::WrongCodes,
                OfferWhy::Forgotten,
                OfferWhy::PcRefused { pc_shows: false },
                OfferWhy::PcRefused { pc_shows: true },
            ] {
                let mut offer = sample_state();
                offer.texts = lang.clone();
                offer.host = None;
                offer.pair_offer = Some((
                    "a-very-long-pc-name.example.lan 192.168.100.200".into(),
                    why,
                ));
                states.push(offer);
            }
            for pc_newer in [true, false] {
                let mut update = sample_state();
                update.texts = lang.clone();
                update.host = None;
                update.update = Some((pc_newer, "0.12.3".into()));
                states.push(update);
            }
            let mut intro = sample_state();
            intro.texts = lang.clone();
            intro.intro = true;
            states.push(intro);
            let mut offline = sample_state();
            offline.texts = lang.clone();
            offline.host = None;
            for hint in [crate::session::Hint::NewPc, crate::session::Hint::Silent] {
                let mut hinted = offline.clone();
                hinted.hint = Some(hint);
                states.push(hinted);
            }
            offline.found = vec![("workstation".into(), "192.168.1.10".into())];
            states.push(offline.clone());
            offline.ip_input = Some("192.168.100.200".into());
            states.push(offline);
            for (code, typed, failed) in [
                (Some("482 913".to_owned()), String::new(), false),
                (None, "4829".to_owned(), true),
            ] {
                let mut pairing = sample_state();
                pairing.texts = lang.clone();
                pairing.pairing = Some(PairingView {
                    host: "workstation".into(),
                    code,
                    typed,
                    failed,
                });
                states.push(pairing);
            }
        }
        // Each page with the error line too, the longest ones there are.
        let with_error: Vec<UiState> = states
            .iter()
            .filter(|st| st.error.is_none())
            .flat_map(|st| {
                [
                    "headset-error-cameras-lost",
                    "headset-error-keyboard-window",
                ]
                .map(|id| UiState {
                    error: Some(st.texts.t(id).to_owned()),
                    ..st.clone()
                })
            })
            .collect();
        states.extend(with_error);
        let mut problems = Vec::new();
        for st in &states {
            let mut p = Panel::new();
            p.set_state(st);
            for o in p.overflows() {
                problems.push(format!("{:?}/{:?}: {o}", st.texts, st.tab));
            }
            // The built-in languages fit as they are: nothing of theirs is drawn
            // smaller.
            if st.texts.lang().as_str() != "qps" {
                let sizes = |items: &[Item]| -> Vec<(String, f32)> {
                    items
                        .iter()
                        .filter_map(|i| match i {
                            // Names are the PC's too, shortened where long.
                            Item::Text {
                                text,
                                size,
                                shorten: false,
                                ..
                            } => Some((text.clone(), *size)),
                            // Names are the PC's, not the menu's texts.
                            Item::Button {
                                label,
                                size,
                                full: None,
                                ..
                            } => Some((label.clone(), *size)),
                            _ => None,
                        })
                        .collect()
                };
                let made = Panel::new().build(st);
                for ((text, was), (_, now)) in sizes(&made).iter().zip(sizes(&p.items)) {
                    if (was - now).abs() > f32::EPSILON {
                        problems.push(format!("{:?}: {text:?} drawn smaller", st.texts));
                    }
                }
            }
        }
        problems.dedup();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// The note drawn: its box, its point, whether it is above its button, its text
    /// and that text's size.
    fn note_of(p: &Panel) -> Option<(Rect, Option<f32>, bool, String, f32)> {
        p.items.iter().find_map(|i| match i {
            Item::Note {
                rect,
                tip,
                above,
                text,
                size,
                ..
            } => Some((*rect, *tip, *above, text.clone(), *size)),
            _ => None,
        })
    }

    /// Where the button standing for `key` is.
    fn button_at(p: &Panel, key: Action) -> Rect {
        p.items
            .iter()
            .find_map(|i| match i {
                Item::Button {
                    rect, key: Some(k), ..
                } if *k == key => Some(*rect),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no button for {key:?}"))
    }

    fn middle(r: Rect) -> (f32, f32) {
        (r.left() + r.width() / 2.0, r.top() + r.height() / 2.0)
    }

    /// The buttons a note at `r` covers, not being inside them.
    fn covered_by(p: &Panel, r: Rect) -> Vec<String> {
        p.items
            .iter()
            .filter_map(|i| match i {
                Item::Button { rect: b, label, .. }
                    if overlap(b, &r)
                        && !(b.left() <= r.left()
                            && b.right() >= r.right()
                            && b.top() <= r.top()
                            && b.bottom() >= r.bottom()) =>
                {
                    Some(label.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// A note sits right under the button it is about, its point at the button's
    /// middle, where the space there is free; else in the nearest free space, never
    /// over another control (the row below stays pressable); inside a tile that stands
    /// for the other value since, which still takes its own press; where a button was
    /// pressed after the button has gone; on the bottom line for a wait and where the
    /// page has no button for it, above the error line.
    #[test]
    fn a_note_sits_under_its_button() {
        let texts = crate::lang::builtin("uk");
        let mut st = sample_state();
        let add = Action::Add {
            width: 2560,
            height: 1440,
        };
        st.note = Some(Note::done(Some(add), texts.t("headset-note-monitor-added")));
        let mut p = Panel::new();
        p.set_state(&st);
        let b = button_at(&p, add);
        let (r, _, _, text, size) = note_of(&p).expect("a note");
        assert_eq!(text, "Монітор додано");
        assert!(r.width() >= b.width());
        assert!(
            (size - NOTE_SIZE).abs() < f32::EPSILON,
            "drawn at its own size"
        );
        assert!(p.overflows().is_empty(), "{:?}", p.overflows());
        // The sizes below are not covered: pressing one adds it.
        assert_eq!(covered_by(&p, r), Vec::<String>::new());
        let below = Action::Add {
            width: 5120,
            height: 1440,
        };
        p.hover(Some(middle(button_at(&p, below))));
        assert_eq!(p.press(), Some(below));
        // Free space right under its button: there, with its point at the button.
        let tidy = Action::Tidy;
        st.note = Some(Note::done(Some(tidy), "Готово"));
        let mut p = Panel::new();
        p.set_state(&st);
        let b = button_at(&p, tidy);
        let (r, tip, above, ..) = note_of(&p).expect("a note");
        assert!(!above);
        assert!(
            (r.top() - (b.bottom() + NOTE_GAP)).abs() < 0.01,
            "{r:?} under {b:?}"
        );
        assert_eq!(tip, Some(middle(b).0));

        // The last row of the page: not over the rows above either.
        let cinema = Action::CinemaScreen(k(103));
        st.note = Some(Note::done(Some(cinema), "Готово"));
        let mut p = Panel::new();
        p.set_state(&st);
        let (r, ..) = note_of(&p).expect("a note");
        assert_eq!(covered_by(&p, r), Vec::<String>::new());
        assert!(p.overflows().is_empty(), "{:?}", p.overflows());

        // A tile pressed: it turns the sound to the PC, and then stands for turning
        // it back. The note covers its state line, under its title.
        let mut st = sample_state();
        st.tab = Tab::Quick;
        st.audio_wanted = false;
        st.note = Some(Note::done(
            Some(Action::SetAudio(false)),
            texts.t("headset-note-sound-pc"),
        ));
        let mut p = Panel::new();
        p.set_state(&st);
        let tile = button_at(&p, Action::SetAudio(true));
        let (r, tip, ..) = note_of(&p).expect("a note");
        assert_eq!(tip, None);
        assert!(r.left() > tile.left() && r.right() < tile.right());
        assert!(r.top() > tile.top() + 70.0 && r.bottom() < tile.bottom());
        p.hover(Some(middle(r)));
        assert_eq!(
            p.press(),
            Some(Action::SetAudio(true)),
            "the tile's own press"
        );

        // Monitor 2 removed: the note stays where its "×" was.
        let mut st = sample_state();
        st.selected = Some(k(102));
        let mut p = Panel::new();
        p.set_state(&st);
        let remove = Action::Remove(k(102));
        let cross = button_at(&p, remove);
        p.hover(Some(middle(cross)));
        assert_eq!(p.press(), Some(remove));
        st.monitors.retain(|m| m.id != k(102));
        st.selected = None;
        st.note = Some(Note::done(
            Some(remove),
            texts.t("headset-note-monitor-removed"),
        ));
        p.set_state(&st);
        // In the column where it was, over nothing that is there now.
        let (r, ..) = note_of(&p).expect("a note");
        assert!(
            r.top() > cross.top() && r.right() <= 624.5,
            "{r:?} by {cross:?}"
        );
        assert_eq!(covered_by(&p, r), Vec::<String>::new());

        // No button for it here: the bottom line, above the error line when there is
        // one.
        for (on, error, waiting) in [
            (None, None, false),
            (Some(Action::FindMouse), None, false),
            (
                None,
                Some("Не вдалося зберегти налаштування".to_owned()),
                false,
            ),
            (Some(add), None, true),
            (
                Some(add),
                Some("Не вдалося зберегти налаштування".to_owned()),
                true,
            ),
        ] {
            let mut st = sample_state();
            st.error.clone_from(&error);
            st.note = Some(if waiting {
                Note::waiting(on, &texts)
            } else {
                Note::failed(on, texts.t("headset-failure-not-on-displays"))
            });
            let mut p = Panel::new();
            p.set_state(&st);
            let (r, tip, ..) = note_of(&p).expect("a note");
            assert_eq!(tip, None);
            let bottom = if error.is_some() { 66.0 } else { 24.0 };
            assert!((r.bottom() - (HEIGHT as f32 - bottom)).abs() < 0.01);
            assert!(p.overflows().is_empty(), "{:?}", p.overflows());
        }
    }

    /// The longest notes, a failure with the PC's detail and a long "done", fit under
    /// every button of every page, with and without the error line, in both languages
    /// at their own size, in the longer pseudo-language made smaller: nothing peeks out
    /// from under them, and the tabs and the error line stay in view (see
    /// `Panel::under_the_note`). A wait and news without a button go on the bottom line
    /// of every page, else the lowest free place, over nothing.
    #[test]
    fn notes_fit_under_every_button() {
        let mut problems = Vec::new();
        for lang in [
            crate::lang::builtin("uk"),
            crate::lang::builtin("en"),
            pseudo(),
        ] {
            let mut pages = Vec::new();
            for tab in [
                Tab::Quick,
                Tab::Monitors,
                Tab::Settings(Section::Look),
                Tab::Settings(Section::Controls),
                Tab::Settings(Section::Sound),
                Tab::Settings(Section::Language),
                Tab::Settings(Section::Pc),
                Tab::Settings(Section::Paired),
                Tab::Settings(Section::Help),
                Tab::Profiles,
            ]
            .into_iter()
            .chain(dev_tab())
            {
                let mut st = sample_state();
                st.texts = lang.clone();
                st.tab = tab;
                st.selected = Some(k(103));
                pages.push(st.clone());
                st.error = Some(lang.t("headset-error-not-connected").to_owned());
                pages.push(st);
            }
            let mut offline = sample_state();
            offline.texts = lang.clone();
            offline.host = None;
            offline.ip_input = Some("192.168.100.200".into());
            pages.push(offline);
            let failed = Note {
                detail: Some("At most 6 monitors of 5120×1440 on this PC".into()),
                ..Note::failed(None, lang.t("headset-failure-bad-size"))
            };
            let done = Note::done(None, lang.t("headset-note-mic-ask"));
            let mut p = Panel::new();
            for page in pages {
                // On the bottom line, above the error line when there is one (or where
                // nothing is).
                let lowest = HEIGHT as f32 - if page.error.is_some() { 66.0 } else { 24.0 };
                for note in [Note::waiting(Some(Action::Tidy), &lang), done.clone()] {
                    let mut st = page.clone();
                    st.note = Some(note);
                    p.set_state(&st);
                    problems.extend(p.overflows());
                    let (r, tip, ..) = note_of(&p).expect("a note");
                    let bottom = (r.bottom() - lowest).abs() < 0.01;
                    let top = (r.top() - NOTE_TOP).abs() < 0.01;
                    let free = covered_by(&p, r).is_empty();
                    if tip.is_some() || !(bottom || top || free) {
                        problems.push(format!("{:?}: not on the bottom line", page.tab));
                    }
                }
                p.set_state(&page);
                let keys: Vec<Action> = p
                    .items
                    .iter()
                    .filter_map(|i| match i {
                        // The header, the tabs and the parts of Settings: their news is
                        // never under them (a page changes, or the menu closes), it
                        // goes on the bottom line.
                        Item::Button { key, rect, .. }
                            if rect.top() > NOTE_TOP && !matches!(key, Some(Action::Tab(_))) =>
                        {
                            *key
                        }
                        _ => None,
                    })
                    .collect();
                assert!(!keys.is_empty());
                for on in keys {
                    for note in [&failed, &done] {
                        let mut st = page.clone();
                        st.note = Some(Note {
                            on: Some(on),
                            ..note.clone()
                        });
                        p.set_state(&st);
                        for o in p.overflows() {
                            problems.push(format!("{:?}/{:?}/{on:?}: {o}", lang, page.tab));
                        }
                        let Some((r, ..)) = note_of(&p) else {
                            panic!("no note")
                        };
                        // Over no other control: inside a tile it stands for, or where
                        // nothing is. The densest parts of Settings have no free place
                        // as wide as a long note: there it goes over the bottom line.
                        let dense =
                            matches!(page.tab, Tab::Settings(Section::Look | Section::Controls));
                        let inside = |b: &Rect| {
                            b.left() <= r.left()
                                && b.right() >= r.right()
                                && b.top() <= r.top()
                                && b.bottom() >= r.bottom()
                        };
                        for i in &p.items {
                            if let Item::Button { rect: b, label, .. } = i
                                && overlap(b, &r)
                                && !inside(b)
                                && !dense
                            {
                                problems.push(format!(
                                    "{:?}/{on:?}: the note covers {label:?}",
                                    page.tab
                                ));
                            }
                        }
                        // The built-in languages are made smaller at most (in a narrow
                        // column), never cut short.
                        let whole = p.items.iter().all(|i| match i {
                            Item::Note { text, detail, .. } => {
                                **text == *note.text && detail.as_deref() == note.detail.as_deref()
                            }
                            _ => true,
                        });
                        if lang.lang().as_str() != "qps" && !whole {
                            problems.push(format!("{lang:?}/{on:?}: note cut short"));
                        }
                    }
                }
            }
        }
        problems.dedup();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Where two pictures `width` wide differ by more than `by`: none, or how many
    /// bytes, by how much at most, and the box around them.
    fn differ(a: &[u8], b: &[u8], width: u32, by: u8) -> Option<String> {
        assert_eq!(a.len(), b.len());
        let (mut n, mut most, mut bx) = (0, 0, (u32::MAX, u32::MAX, 0, 0));
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            if x.abs_diff(*y) > by {
                n += 1;
                most = most.max(x.abs_diff(*y));
                let px = (i / 4) as u32;
                let (cx, cy) = (px % width, px / width);
                bx = (bx.0.min(cx), bx.1.min(cy), bx.2.max(cx), bx.3.max(cy));
            }
        }
        (n > 0).then(|| format!("{n} bytes differ, by {most} at most, in {bx:?}"))
    }

    /// The whole picture with item `lit` pointed at, drawn at once.
    fn drawn_whole(p: &mut Panel, lit: usize) -> Vec<u8> {
        let mut pixmap = Pixmap::new(p.pixmap.width(), p.pixmap.height()).unwrap();
        let items = std::mem::take(&mut p.items);
        p.paint(&mut Canvas::whole(&mut pixmap), &items, Some(lit));
        p.items = items;
        pixmap.take()
    }

    /// Pointing at a button (a tab, a monitor on the map with its whole name above,
    /// the note, a sector of the palm menu) draws only its part again, and the picture
    /// is the one drawing it whole with that button pointed at makes (a curved edge cut
    /// at the part's edge is smoothed by at most one step otherwise); going back to a
    /// button already pointed at draws nothing (copied) and changes only its part;
    /// pointing at nothing gives back the picture with nothing pointed at, to the bit.
    #[test]
    fn pointing_draws_only_its_part_as_the_whole_would() {
        let mut monitors = sample_state();
        monitors.monitors[0].label = "DELL U2720Q a very long name".into();
        monitors.note = Some(Note::done(
            Some(Action::Preset(Preset::Wall)),
            "Розкладку змінено",
        ));
        let mut quick = sample_state();
        quick.tab = Tab::Quick;
        let mut pc = sample_state();
        pc.tab = Tab::Settings(Section::Pc);
        let mut checked = 0;
        for (wrist, st) in [
            (false, monitors),
            (false, quick),
            (false, pc),
            (true, sample_state()),
        ] {
            let mut p = if wrist { Panel::wrist() } else { Panel::new() };
            p.set_state(&st);
            let plain = p.render_if_dirty().unwrap().to_vec();
            assert_eq!(p.damage(), None, "drawn whole");
            let points: Vec<(usize, (f32, f32))> = p
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| match item {
                    Item::Button { rect: r, .. } | Item::Note { rect: r, .. } => {
                        Some((i, (r.left() + r.width() / 2.0, r.top() + r.height() / 2.0)))
                    }
                    Item::Wedge { .. } => {
                        let b = item.bounds()?;
                        Some((i, (b.left() + b.width() / 2.0, b.top() + 30.0)))
                    }
                    _ => None,
                })
                .collect();
            let mut before: Option<usize> = None;
            for &(i, at) in &points {
                p.hover(Some(at));
                if p.hovered != Some(i) {
                    continue;
                }
                let drawn = p.render_if_dirty().expect("another button").to_vec();
                let whole = drawn_whole(&mut p, i);
                let w = p.pixmap.width();
                let what = match &p.items[i] {
                    Item::Button { label, rect, .. } => format!("{label:?} {rect:?}"),
                    Item::Note { rect, .. } => format!("note {rect:?}"),
                    _ => String::new(),
                };
                assert_eq!(
                    differ(&drawn, &whole, w, 0),
                    None,
                    "item {i} {what} {:?} of {:?}",
                    p.damage(),
                    st.tab
                );
                // The parts of the one pointed at before and of this one, not the whole
                // picture (the two may be far apart: the box around both).
                let damage = p.damage().expect("parts");
                assert!(!damage.is_empty());
                if let Some(b) = before {
                    // Back to the one before: its kept copies.
                    let at = points.iter().find(|(j, _)| *j == b).unwrap().1;
                    p.hover(Some(at));
                    let back = p.render_if_dirty().unwrap().to_vec();
                    assert_eq!(differ(&back, &drawn_whole(&mut p, b), w, 0), None);
                    p.hover(Some(points.iter().find(|(j, _)| *j == i).unwrap().1));
                    let again = p.render_if_dirty().unwrap().to_vec();
                    assert_eq!(differ(&again, &drawn, w, 0), None);
                }
                before = Some(i);
                checked += 1;
            }
            p.hover(None);
            let w = p.pixmap.width();
            let none = p.render_if_dirty().unwrap().to_vec();
            assert_eq!(differ(&none, &plain, w, 0), None);

            assert!(p.render_if_dirty().is_none(), "nothing changed");
        }
        assert!(checked > 60, "{checked} buttons");
    }

    /// The palm menu in every language, the longer one too, without text that does not
    /// fit.
    #[test]
    fn the_palm_menu_fits() {
        for lang in [
            crate::lang::builtin("uk"),
            crate::lang::builtin("en"),
            pseudo(),
        ] {
            for (battery, twelve) in [
                (Some((100, true)), true),
                (Some((9, false)), false),
                (None, false),
            ] {
                let mut st = sample_state();
                let day = Moment::EXAMPLE;
                st.clock = lang.clock(twelve, &day).into();
                st.date = lang.date(crate::settings::DateStyle::LikeMenu, &day).into();
                st.battery = battery;
                st.texts = lang.clone();
                let mut p = Panel::wrist();
                p.set_state(&st);
                let mut problems = p.overflows();
                // The built-in languages fit as they are: the battery, the date and
                // "Menu" at their own sizes (the 12-hour clock is made smaller to fit
                // in any language, and so are the sectors' labels).
                if lang.lang().as_str() != "qps" {
                    for item in &p.items {
                        if let Item::Text { text, size, .. } = item
                            && *text != *st.clock
                        {
                            let own = if text.contains('%') { 15.0 } else { 18.0 };
                            if (size - own).abs() > f32::EPSILON {
                                problems.push(format!("{text:?} drawn smaller ({size})"));
                            }
                        }
                    }
                }
                assert!(problems.is_empty(), "{lang:?}: {}", problems.join("\n"));
            }
        }
    }

    /// The Monitors tab for `monitors` (id, width, height, desktop centre in pixels)
    /// and the laptop screen (centre), standing on a flat wall as on the desktop and
    /// mapped the way the headset maps them, drawn with each monitor picked in turn.
    fn pick_each(
        panel: &mut Panel,
        monitors: &[(u32, u32, u32, f32, f32)],
        mirror: Option<(f32, f32)>,
        primary: u32,
    ) {
        use crate::layout::{Desktop, Placement};
        use glam::{Quat, Vec3};
        /// The laptop screen's number in these tests.
        const MIRROR_ID: u32 = 100;
        const METRES_PER_PX: f32 = 0.0003;
        let mirror = mirror.map(|(x, y)| (MIRROR_ID, 1920, 1080, x, y));
        let all: Vec<_> = monitors.iter().copied().chain(mirror).collect();
        let placements: HashMap<u32, Placement> = all
            .iter()
            .map(|&(id, w, _, x, y)| {
                let place = Placement {
                    position: Vec3::new(x * METRES_PER_PX, -y * METRES_PER_PX, -2.0),
                    rotation: Quat::IDENTITY,
                    width: w as f32 * METRES_PER_PX,
                };
                (id, place)
            })
            .collect();
        let sizes: Vec<(u32, u32, u32)> = all.iter().map(|&(id, w, h, ..)| (id, w, h)).collect();
        let center = (Vec3::ZERO, 0.0);
        let spots = Desktop::of(&placements, &sizes, center, 0.0, Some(primary), None).spots();
        let mut st = sample_state();
        st.tab = Tab::Monitors;
        st.primary = Some(k(primary));
        st.physical_seen = spots
            .get(&MIRROR_ID)
            .map(|seen| (*seen, "eDP-1".to_owned()))
            .into_iter()
            .collect();
        st.monitors = monitors
            .iter()
            .map(|&(id, width, height, ..)| MonitorEntry {
                id: k(id),
                label: (id + 1).to_string(),
                width,
                height,
                place: Place::Center,
                seen: spots.get(&id).copied(),
                scale: 100,
                picture: Picture::Shows,
            })
            .collect();
        for selected in std::iter::once(None).chain(monitors.iter().map(|m| Some(k(m.0)))) {
            st.selected = selected;
            panel.set_state(&st);
            // Every shape is still made, only next to no pixels are filled: fast.
            panel.pixmap = Pixmap::new(1, 1).expect("1x1 is a valid size");
            panel.render_if_dirty();
        }
    }

    /// Six monitors that overlapped and then spread out, the primary a narrow
    /// portrait one: picking another one on the map closed the app.
    #[test]
    fn picking_on_a_spread_out_map() {
        // As in monitors.txt: top-left corners, the third one primary.
        let corners = [
            (3440, 1440, 7600, 5600),
            (3440, 1440, -1720, 1280),
            (1440, 2560, 720, 1280),
            (2560, 1440, 2720, 1280),
            (3440, 1440, 4024, 2720),
            (2560, 1440, 5120, 4160),
        ];
        let monitors: Vec<_> = corners
            .iter()
            .enumerate()
            .map(|(id, &(w, h, x, y))| {
                let centre = |at: i32, size: u32| (at + size as i32 / 2) as f32;
                (id as u32, w, h, centre(x, w), centre(y, h))
            })
            .collect();
        let laptop = Some((192.0 + 960.0, 3840.0 + 540.0));
        pick_each(&mut Panel::new(), &monitors, laptop, 2);
    }

    /// Random arrangements of 1 to 6 monitors of the sizes the menu adds, close
    /// together, overlapping or far apart, with and without the laptop screen: the
    /// map draws whichever monitor is picked.
    #[test]
    fn picking_on_any_map() {
        const SIZES: [(u32, u32); 5] = [
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
            (3440, 1440),
            (1440, 2560),
        ];
        // xorshift64*, so every run tries the same arrangements.
        struct Rng(u64);
        impl Rng {
            fn below(&mut self, n: u64) -> u64 {
                self.0 ^= self.0 >> 12;
                self.0 ^= self.0 << 25;
                self.0 ^= self.0 >> 27;
                (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) % n
            }
            fn within(&mut self, spread: u64) -> f32 {
                self.below(2 * spread) as f32 - spread as f32
            }
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut panel = Panel::new();
        for _ in 0..500 {
            // Centres (pixels) all within a few pixels, near each other, or far apart.
            let spread = [1, 2_000, 20_000, 2_000_000][rng.below(4) as usize];
            let count = 1 + rng.below(6) as u32;
            let monitors: Vec<_> = (0..count)
                .map(|id| {
                    let (w, h) = SIZES[rng.below(5) as usize];
                    (id, w, h, rng.within(spread), rng.within(spread))
                })
                .collect();
            let laptop = (rng.below(2) == 0).then(|| (rng.within(spread), rng.within(spread)));
            let primary = rng.below(u64::from(count)) as u32;
            pick_each(&mut panel, &monitors, laptop, primary);
        }
    }

    /// The error line shows on every page: the tabs, and also the pages before them
    /// (no PC yet, pairing, a PC to pair with, another version, the first tips), so
    /// what the store or the PC says while connecting is seen.
    #[test]
    fn the_error_line_is_on_every_page() {
        let says = |s: &UiState, what: &str| {
            Panel::new()
                .build(s)
                .iter()
                .any(|i| matches!(i, Item::Text { text, .. } if text == what))
        };
        let mut tabs = sample_state();
        tabs.error = Some("Not confirmed".into());
        let mut welcome = tabs.clone();
        welcome.host = None;
        let mut intro = tabs.clone();
        intro.intro = true;
        let mut pairing = welcome.clone();
        pairing.pairing = Some(PairingView {
            host: "PC".into(),
            code: None,
            typed: String::new(),
            failed: false,
        });
        let mut offer = welcome.clone();
        offer.pair_offer = Some(("PC".into(), OfferWhy::New));
        let mut update = welcome.clone();
        update.update = Some((true, "0.12.3".into()));
        for (page, st) in [
            ("tabs", tabs),
            ("welcome", welcome),
            ("intro", intro),
            ("pairing", pairing),
            ("offer", offer),
            ("update", update),
        ] {
            assert!(says(&st, "Not confirmed"), "not on the {page} page");
        }
    }
}
