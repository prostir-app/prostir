//! The floating menu panel. It is drawn on the CPU into an RGBA image and only
//! re-uploaded when something changes (hover, a click, news from the host).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};
use prostir_proto::Codec;
use prostir_proto::state::{Encoding, ErrorCode, Limits};
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::ids::DisplayKey;
use crate::lang::{Language, Moment, Texts};
use crate::settings::{Background, Curve, HandMode, Preset, Settings, Sky};
use crate::store::UpdateHint;

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
}

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
            Place::Left => "place-left",
            Place::Center => "place-middle",
            Place::Right => "place-right",
            Place::Above => "place-above",
            Place::Below => "place-below",
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
    /// Quest monitors, sorted by id.
    pub monitors: Vec<MonitorEntry>,
    /// The monitor picked on the map (its actions show under it).
    pub selected: Option<DisplayKey>,
    /// The laptop screen on the map: centre and half size as seen, in radians.
    pub mirror_seen: Option<[f32; 4]>,
    pub primary: Option<DisplayKey>,
    /// The screen picked for cinema (see `layout::cinema_choice`).
    pub cinema_screen: Option<DisplayKey>,
    pub mirror_wanted: bool,
    /// PC sound goes to the headset.
    pub audio_wanted: bool,
    pub mic_wanted: bool,
    /// Access to the microphone was refused in the system.
    pub mic_denied: bool,
    /// How the PC encodes and what it can encode with (while connected).
    pub pc: Option<Encoding>,
    /// The monitors the PC makes for this headset (while connected).
    pub limits: Option<Limits>,
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
    /// What the display of the PC the switch shows or hides is called; none while no
    /// display of the PC is connected (the lid closed, no monitor).
    pub mirror_name: Option<String>,
    pub settings: Settings,
    pub room_available: bool,
    pub panorama_available: bool,
    /// The user put a `background.jpg/png` on the headset.
    pub custom_available: bool,
    pub curve_available: bool,
    /// The headset can show a passthrough window onto the keyboard.
    pub keyboard_available: bool,
    pub error: Option<String>,
    /// Monitor whose "×" was pressed once and now asks for confirmation.
    pub confirm_remove: Option<DisplayKey>,
    /// A profile whose "Save current" was pressed once (press again to overwrite).
    pub confirm_save: Option<usize>,
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
    /// Which profile slots hold a saved profile (the others are empty).
    pub saved_profiles: [bool; crate::profiles::SLOTS],
    /// The profile last switched to (until something is changed by hand).
    pub active_profile: Option<usize>,
    /// What the menu asked the PC for and has no answer to yet, or that failed lately:
    /// shown by the control that asked.
    pub asks: Vec<AskView>,
}

/// Which control asked the PC for something (see [`AskView`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    AddMonitor,
    Remove(DisplayKey),
    MakePrimary(DisplayKey),
    Mirror,
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
            Self::NotConnected => "failure-not-connected",
            Self::Disconnected => "failure-disconnected",
            Self::TimedOut => "failure-timed-out",
            Self::Pc(code) => match code.known() {
                ErrorCode::UNSUPPORTED => "failure-unsupported",
                ErrorCode::NOT_ALLOWED => "failure-not-allowed",
                ErrorCode::UNKNOWN_DISPLAY => "failure-unknown-display",
                ErrorCode::LIMIT => "failure-limit",
                ErrorCode::BAD_SIZE => "failure-bad-size",
                ErrorCode::LAST_DISPLAY => "failure-last-display",
                ErrorCode::NO_WINDOW => "failure-no-window",
                ErrorCode::DESKTOP => "failure-desktop",
                ErrorCode::ENCODER => "failure-encoder",
                ErrorCode::CAPTURE => "failure-capture",
                ErrorCode::SOUND => "failure-sound",
                ErrorCode::NOT_ON_DISPLAYS => "failure-not-on-displays",
                ErrorCode::NO_SUCH_PROFILE => "failure-no-such-profile",
                ErrorCode::BAD_ARGUMENT => "failure-bad-argument",
                _ => return None,
            },
        })
    }
}

/// How a request of the menu stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskState {
    Waiting,
    /// Why, and the PC's detail (technical, in the PC's language).
    Failed {
        why: Failure,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskView {
    pub what: Asked,
    pub state: AskState,
}

impl UiState {
    /// How the request of control `what` stands, if one is waiting or failed lately.
    fn ask(&self, what: Asked) -> Option<&AskState> {
        self.asks
            .iter()
            .rev()
            .find(|a| a.what == what)
            .map(|a| &a.state)
    }

    /// A short line for control `what`: waiting, or why it failed (the code's text,
    /// else the PC's detail), and whether it is a failure.
    fn ask_note(&self, what: Asked) -> Option<(String, bool)> {
        let l = &self.texts;
        Some(match self.ask(what)? {
            AskState::Waiting => (l.t("ask-waiting").to_owned(), false),
            AskState::Failed { why, detail } => (
                why.text()
                    .map_or_else(|| detail.clone(), |id| l.t(id).to_owned()),
                true,
            ),
        })
    }

    /// The PC's detail under a failure of control `what`, when there is a text above it.
    fn ask_detail(&self, what: Asked) -> Option<&str> {
        match self.ask(what)? {
            AskState::Failed { why, detail } if why.text().is_some() && !detail.is_empty() => {
                Some(detail)
            }
            _ => None,
        }
    }
}

/// Pages of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// Large tiles for what is used every day.
    #[default]
    Quick,
    Monitors,
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
    Help,
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
    SetMirror(bool),
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
    SaveProfile(usize),
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
    Close,
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

pub struct Panel {
    regular: Font,
    medium: Font,
    glyphs: HashMap<(bool, char, u32), (Metrics, Vec<u8>)>,
    items: Vec<Item>,
    state: Option<UiState>,
    hovered: Option<usize>,
    pixmap: Pixmap,
    dirty: bool,
    /// The wrist menu rather than the main one.
    wrist: bool,
}

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
            wrist: false,
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

    /// Updates the hovered button from a point in panel pixels (`None`: not pointing at the panel).
    pub fn hover(&mut self, point: Option<(f32, f32)>) {
        let hovered = point.and_then(|(x, y)| {
            self.items.iter().position(|item| match item {
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
            })
        });
        if hovered != self.hovered {
            self.hovered = hovered;
            self.dirty = true;
        }
    }

    pub fn hovered_action(&self) -> Option<Action> {
        match self.items.get(self.hovered?)? {
            Item::Button { action, .. } | Item::Wedge { action, .. } => *action,
            _ => None,
        }
    }

    /// Labels and texts that do not fit: a button label wider than its button, text
    /// running off the panel or into another text or button. Empty when the layout is
    /// clean; checked by the tests for every page in both languages.
    pub fn overflows(&mut self) -> Vec<String> {
        let items = std::mem::take(&mut self.items);
        let (width, height) = (self.pixmap.width() as f32, self.pixmap.height() as f32);
        let mut boxes: Vec<(Rect, String, bool)> = Vec::new(); // (bounds, text, is_button)
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
                    ..
                } => {
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
                _ => {}
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
            (l.t("wrist-in-front"), false, Action::Recenter),
            (l.t("focus"), st.focus, Action::Focus(!st.focus)),
            (l.t("cinema"), st.cinema, Action::Cinema(!st.cinema)),
            (l.t("wrist-mouse"), false, Action::FindMouse),
            (l.t("wrist-screenshot"), false, Action::Screenshot),
            (l.t("line-up"), false, Action::Tidy),
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
                "battery-charging"
            } else {
                "battery-level"
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
        let label = l.t("menu");
        let (x, menu) = centred(self, label, menu, false, across(50.0));
        items.push(text_in(
            across(50.0),
            text(x, cy + 46.0, menu, false, MUTED, label),
        ));
        items
    }

    /// Premultiplied RGBA, top row first, when it changed since the last call.
    pub fn render_if_dirty(&mut self) -> Option<&[u8]> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        self.render();
        Some(self.pixmap.data())
    }

    fn build(&mut self, s: &UiState) -> Vec<Item> {
        let l = &s.texts;
        let mut items = vec![Item::Panel(rect(0.0, 0.0, WIDTH as f32, HEIGHT as f32))];
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
            _ if s.update.is_some() => (l.t("status-update-needed").to_owned(), DANGER),
            _ if s.pairing.is_some() => (l.t("status-pairing").to_owned(), WAIT),
            Some(h) => (l.f("status-connected", &[("pc", h.into())]), GOOD),
            None => (l.t("status-searching").to_owned(), WAIT),
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
                l.t("change-pc"),
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
            build_pair_offer(l, &mut items, lx, lw, host, *why);
            in_column(&mut items[at..], lx, whole);
            return items;
        }
        if let Some(p) = &s.pairing {
            self.build_pairing(l, p, &mut items, lx, lw, 150.0);
            in_column(&mut items[at..], lx, whole);
            return items;
        }
        if let Some((pc_newer, pc_app)) = &s.update {
            build_update(l, &mut items, lx, whole, *pc_newer, pc_app, s.update_from);
            in_column(&mut items[at..], lx, whole);
            return items;
        }
        if s.intro && s.host.is_some() {
            build_intro(l, &mut items, lx, whole);
            in_column(&mut items[at..], lx, whole);
            return items;
        }
        // No PC yet: how to get one, and the PCs found so far.
        if s.host.is_none() {
            build_welcome(l, &mut items, lx, lw, 150.0);
            in_column(&mut items[at..], lx, lw);
            let right = items.len();
            self.build_connection(s, &mut items, rx, rw, 150.0);
            in_column(&mut items[right..], rx, rw);
            return items;
        }

        // Tabs: everyday things, the monitors, everything set once.
        let tabs = [
            (Tab::Quick, l.t("tab-quick")),
            (Tab::Monitors, l.t("tab-monitors")),
            (
                Tab::Settings(s.tab.section().unwrap_or_default()),
                l.t("tab-settings"),
            ),
        ];
        let tab_w = (w - 2.0 * lx - 2.0 * 10.0) / 3.0;
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
                build_quick(s, &mut items, lx, whole, top - 12.0);
                in_column(&mut items[at..], lx, whole);
            }
            Tab::Monitors => {
                let left = monitor_list(s, &mut items, lx, lw, top - 12.0);
                in_column(&mut items[at..], lx, lw);
                let right = items.len();
                build_monitor_options(s, &mut items, rx, rw, top);
                in_column(&mut items[right..], rx, rw);
                let below = items.len();
                save_as_profile(s, &mut items, lx, lw, left + 30.0);
                in_column(&mut items[below..], lx, lw);
            }
            Tab::Settings(section) => build_settings(s, section, &mut items, top - 12.0),
        }
        if let Some(e) = &s.error {
            items.push(text(lx, HEIGHT as f32 - 34.0, 21.0, true, DANGER, e));
        }
        items
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
            &l.f("pairing-with", &[("pc", (&p.host).into())]),
        ));
        y += 40.0;
        if p.failed {
            items.push(text(lx, y, 20.0, true, DANGER, l.t("pairing-failed")));
            y += 34.0;
        }
        if let Some(code) = &p.code {
            items.push(text(lx, y, 20.0, false, TEXT, l.t("pairing-type-on-pc")));
            y += 30.0;
            items.push(Item::Box(rect(lx, y, lw, 130.0), [255, 255, 255, 16]));
            let cw = self.text_width(code, 84.0, true);
            items.push(text(lx + (lw - cw) / 2.0, y + 98.0, 84.0, true, TEXT, code));
            y += 150.0;
            items.push(text(lx, y, 18.0, false, MUTED, l.t("pairing-once")));
            y += 30.0;
            items.push(button(
                rect(lx, y, lw, 50.0),
                l.t("pairing-other-way-type-here"),
                Style::Normal,
                Some(Action::PairOtherWay),
            ));
            return y + 62.0;
        }
        for line in [l.t("pairing-type-pc-code"), l.t("pairing-where-pc-code")] {
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
        let erase = l.t("key-delete");
        let done = l.t("key-done");
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
            l.t("pairing-other-way-type-on-pc"),
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
                l.t("key-delete"),
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
                l.t("connect"),
                if ready {
                    Style::Selected
                } else {
                    Style::Disabled
                },
                ready.then_some(Action::ConnectTyped),
            ));
            items.push(button(
                rect(lx + half + 10.0, y, half, 50.0),
                l.t("cancel"),
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
                l.t("connect-searching"),
            ));
            y += 48.0;
        } else {
            items.push(text(lx, y + 14.0, 21.0, false, MUTED, l.t("connect-found")));
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
                    l.t("connect-blocked").to_owned(),
                    l.t("connect-allow-port").to_owned(),
                ] {
                    items.push(text(lx, y + 16.0, 18.0, false, DANGER, &line));
                    y += 28.0;
                }
                y += 6.0;
            }
        }
        items.push(button(
            rect(lx, y + 6.0, 280.0, 48.0),
            l.t("connect-type-ip"),
            Style::Normal,
            Some(Action::ShowKeypad),
        ));
        y + 66.0
    }

    fn glyph(&mut self, medium: bool, c: char, size: f32) -> &(Metrics, Vec<u8>) {
        let key = (medium, c, size.to_bits());
        let font = if medium { &self.medium } else { &self.regular };
        self.glyphs
            .entry(key)
            .or_insert_with(|| font.rasterize(c, size))
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

    fn render(&mut self) {
        let mut pixmap = std::mem::replace(
            &mut self.pixmap,
            Pixmap::new(1, 1).expect("1x1 is a valid size"),
        );
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let items = std::mem::take(&mut self.items);
        for (i, item) in items.iter().enumerate() {
            match item {
                Item::Panel(r) => fill(&mut pixmap, rounded(*r, 36.0), [16, 18, 24, 240]),
                Item::Box(r, color) => fill(&mut pixmap, rounded(*r, 14.0), *color),
                Item::Dot { x, y, r, color } => fill(&mut pixmap, circle(*x, *y, *r), *color),
                Item::Text {
                    x,
                    baseline,
                    size,
                    medium,
                    color,
                    text,
                    ..
                } => self.draw_text(&mut pixmap, *x, *baseline, *size, *medium, *color, text),
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
                    let hovered = self.hovered == Some(i);
                    let bg = match (style, hovered) {
                        (Style::Selected, _) => ACCENT,
                        (_, true) => [255, 255, 255, 52],
                        _ => [255, 255, 255, 20],
                    };
                    fill(&mut pixmap, sector(*cx, *cy, *r0, *r1, *a0, *a1), bg);
                    let (mx, my) = wedge_center(*cx, *cy, *r0, *r1, *a0, *a1);
                    let tw = self.text_width(label, *size, true);
                    self.draw_text(
                        &mut pixmap,
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
                    let hovered = self.hovered == Some(i);
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
                    fill(&mut pixmap, rounded(*r, 12.0), bg);
                    match style {
                        Style::Disabled => {
                            stroke(&mut pixmap, rounded(*r, 12.0), [255, 255, 255, 26], 1.5);
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
                            fill(&mut pixmap, rounded(bar, 2.0), ACCENT);
                        }
                        _ => {}
                    }
                    let tw = self.text_width(label, size, true);
                    let baseline = r.top() + r.height() / 2.0 + size * 0.36;
                    self.draw_text(
                        &mut pixmap,
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
        // A name shortened on its button, pointed at: whole, above it.
        if let Some(Item::Button {
            rect: r,
            label,
            full: Some(full),
            ..
        }) = self.hovered.and_then(|i| items.get(i))
            && label != full
        {
            self.draw_tip(&mut pixmap, *r, full);
        }
        self.items = items;
        self.pixmap = pixmap;
    }

    /// `text` on a dark label just above `over` (below it where there is no room above),
    /// within the panel, drawn smaller where it is wide.
    fn draw_tip(&mut self, pixmap: &mut Pixmap, over: Rect, text: &str) {
        let (pw, ph) = (pixmap.width() as f32, pixmap.height() as f32);
        let size = self.fitting(text, 20.0, true, pw - 64.0);
        let tw = self.text_width(text, size, true);
        let (w, h) = (tw + 28.0, size + 18.0);
        let x = (over.left() + over.width() / 2.0 - w / 2.0).clamp(16.0, (pw - 16.0 - w).max(16.0));
        let y = if over.top() - h - 6.0 >= 8.0 {
            over.top() - h - 6.0
        } else {
            (over.bottom() + 6.0).min(ph - h - 8.0)
        };
        let r = rect(x, y, w, h);
        fill(pixmap, rounded(r, 10.0), [16, 18, 24, 245]);
        stroke(pixmap, rounded(r, 10.0), [255, 255, 255, 40], 1.5);
        self.draw_text(
            pixmap,
            x + 14.0,
            y + h / 2.0 + size * 0.36,
            size,
            true,
            TEXT,
            text,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        x: f32,
        baseline: f32,
        size: f32,
        medium: bool,
        color: [u8; 4],
        s: &str,
    ) {
        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let mut pen = x;
        for c in s.chars() {
            let (m, coverage) = self.glyph(medium, c, size).clone();
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
            let data = pixmap.data_mut();
            for row in 0..m.height as i32 {
                let py = gy + row;
                if py < 0 || py >= ph {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let px = gx + col;
                    if px < 0 || px >= pw {
                        continue;
                    }
                    let cov = coverage[(row * m.width as i32 + col) as usize] as u32;
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
            pen += m.advance_width;
        }
    }
}

impl Panel {
    /// A button on its own layer: `label` on a pill, accent-coloured when `lit`.
    /// Premultiplied RGBA.
    pub fn pill_image(&mut self, label: &str, width: u32, height: u32, lit: bool) -> Vec<u8> {
        let mut p = Pixmap::new(width, height).expect("pill size");
        let (w, h) = (width as f32, height as f32);
        let bg = if lit { ACCENT } else { [16, 18, 24, 235] };
        fill(&mut p, rounded(rect(0.0, 0.0, w, h), h / 2.0), bg);
        let mut size = h * 0.4;
        while size > 12.0 && self.text_width(label, size, true) > w - h * 0.6 {
            size -= 1.0;
        }
        let tw = self.text_width(label, size, true);
        let fg = [255, 255, 255, 255];
        self.draw_text(
            &mut p,
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
        let (w, h) = (width as f32, height as f32);
        let edge = (w.min(h) * 0.02).max(4.0);
        fill(
            &mut p,
            rounded(rect(0.0, 0.0, w, h), edge * 3.0),
            [120, 160, 255, 150],
        );
        fill(
            &mut p,
            rounded(rect(edge, edge, w - 2.0 * edge, h - 2.0 * edge), edge * 2.0),
            [16, 18, 24, 170],
        );
        let mut size = h * 0.12;
        while size > 12.0 && self.text_width(label, size, true) > w * 0.8 {
            size -= 2.0;
        }
        let tw = self.text_width(label, size, true);
        self.draw_text(
            &mut p,
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
        let mut p = Pixmap::new(width, height).expect("toast size");
        let (w, h) = (width as f32, height as f32);
        fill(
            &mut p,
            rounded(rect(0.0, 0.0, w, h), 28.0),
            [16, 18, 24, 235],
        );
        let room = w - 72.0;
        let one = h * 0.3;
        if self.text_width(message, one, true) <= room {
            let tw = self.text_width(message, one, true);
            self.draw_text(
                &mut p,
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
            self.draw_text(&mut p, (w - tw) / 2.0, baseline, size, k == 0, TEXT, line);
        }
        p.take()
    }
}

/// Pointer dot: white with a dark rim so it shows on light and dark content.
/// Premultiplied RGBA, `size`×`size`.
pub fn cursor_image(size: u32) -> Vec<u8> {
    let mut p = Pixmap::new(size, size).expect("cursor size");
    let c = size as f32 / 2.0;
    fill(&mut p, circle(c, c, c - 1.0), [20, 22, 28, 200]);
    fill(&mut p, circle(c, c, c * 0.62), [255, 255, 255, 255]);
    p.take()
}

/// Round "☰" button that opens the menu. Premultiplied RGBA, `size`×`size`.
pub fn menu_button_image(size: u32, hovered: bool) -> Vec<u8> {
    let mut p = Pixmap::new(size, size).expect("button size");
    let s = size as f32;
    let bg = if hovered { ACCENT } else { [16, 18, 24, 225] };
    fill(&mut p, circle(s / 2.0, s / 2.0, s / 2.0 - 1.0), bg);
    for i in -1..=1 {
        let y = s / 2.0 + i as f32 * s * 0.15;
        fill(
            &mut p,
            rounded(rect(s * 0.3, y - s * 0.035, s * 0.4, s * 0.07), s * 0.035),
            [255, 255, 255, 255],
        );
    }
    p.take()
}

/// Quest monitors with reorder / primary / remove, then the laptop screen.
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
        .chain(s.mirror_seen)
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
    if let Some(m) = s.mirror_seen {
        items.push(Item::Box(to_rect(m), [255, 255, 255, 14]));
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
                &l.f("monitor-n", &[("n", (&m.label).into())]),
            ),
        );
        if let Item::Text { shorten, .. } = &mut title {
            *shorten = true;
        }
        items.push(title);
        // Removing it or making it primary: how that stands, in place of its size.
        let note = s
            .ask_note(Asked::Remove(m.id))
            .or_else(|| s.ask_note(Asked::MakePrimary(m.id)));
        let remove = rect(lx + lw - 58.0, y + 7.0, 48.0, 48.0);
        let primary = rect(lx + lw - 58.0 - 10.0 - 210.0, y + 7.0, 210.0, 48.0);
        // Left of the buttons.
        items.push(text_in(
            primary.left() - lx - 10.0,
            text(
                lx,
                y + 50.0,
                16.0,
                false,
                if note.as_ref().is_some_and(|n| n.1) {
                    DANGER
                } else {
                    MUTED
                },
                &match (note.as_ref(), m.place.name()) {
                    (Some((note, _)), _) => note.clone(),
                    (None, Some(place)) => {
                        format!("{} × {} · {}", m.width, m.height, l.t(place))
                    }
                    (None, None) => format!("{} × {}", m.width, m.height),
                },
            ),
        ));
        if s.confirm_remove == Some(m.id) {
            items.push(button(
                rect(
                    primary.left(),
                    y + 7.0,
                    remove.right() - primary.left(),
                    48.0,
                ),
                l.t("monitor-remove-confirm"),
                Style::Armed,
                Some(Action::Remove(m.id)),
            ));
        } else {
            if s.primary == Some(m.id) {
                items.push(button(
                    primary,
                    l.t("monitor-primary"),
                    Style::Selected,
                    None,
                ));
            } else {
                items.push(button(
                    primary,
                    l.t("monitor-make-primary"),
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
            &l.f("window-n", &[("n", (i + 1).into())]),
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
    // A display of the PC: shown or hidden, said by the highlighted option, named; with
    // none connected (the lid closed, no monitor) there is nothing to switch.
    let connected = s.mirror_name.is_some();
    let name = s
        .mirror_name
        .clone()
        .unwrap_or_else(|| l.t("pc-display").to_owned());
    let title = if let Some((note, _)) = s.ask_note(Asked::Mirror) {
        format!("{name} · {note}")
    } else if !connected {
        format!("{name} · {}", l.t("pc-display-not-connected"))
    } else {
        name
    };
    y += 34.0;
    segmented(
        items,
        lx,
        lw,
        &mut y,
        &title,
        vec![
            (
                l.t("pc-display-shown"),
                Action::SetMirror(true),
                s.mirror_wanted,
                connected,
            ),
            (
                l.t("pc-display-hidden"),
                Action::SetMirror(false),
                !s.mirror_wanted,
                connected,
            ),
        ],
    );
    y -= 16.0;
    items.push(button(
        rect(lx, y, lw, 52.0),
        l.t("monitors-line-up"),
        Style::Normal,
        Some(Action::Tidy),
    ));
    y + 52.0
}

/// A row with a title, a muted note and one button on the right.
fn hints(l: &Texts, items: &mut Vec<Item>, lx: f32, mut y: f32) {
    for line in [
        l.t("hint-finger"),
        l.t("hint-hand"),
        l.t("hint-controller"),
        l.t("hint-stick"),
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
        items.push(button(
            r,
            label,
            style,
            (enabled && !selected).then_some(action),
        ));
    }
    *y += 48.0 + 40.0;
}

/// Add a monitor, layout, curve.
fn build_monitor_options(s: &UiState, items: &mut Vec<Item>, rx: f32, rw: f32, mut y: f32) -> f32 {
    let l = &s.texts;
    items.push(text(rx, y, 21.0, false, MUTED, l.t("add-monitor")));
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
    // How the last "add" stands: waiting, or why it failed and the PC's detail.
    if let Some((note, failed)) = s.ask_note(Asked::AddMonitor) {
        let color = if failed { DANGER } else { MUTED };
        items.push(text(rx, y - 22.0, 18.0, true, color, &note));
        if let Some(detail) = s.ask_detail(Asked::AddMonitor) {
            items.push(text(rx, y, 15.0, false, MUTED, detail));
            y += 20.0;
        }
        y += 10.0;
    } else if let Some(max) = max.filter(|max| s.monitors.len() >= *max) {
        items.push(text(
            rx,
            y - 22.0,
            18.0,
            false,
            MUTED,
            &l.f("monitors-at-most", &[("count", max.into())]),
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
        l.t("layout"),
        vec![
            (
                l.t("layout-around"),
                Action::Preset(Preset::Arc),
                st.preset == Preset::Arc,
                true,
            ),
            (
                l.t("layout-wall"),
                Action::Preset(Preset::Wall),
                st.preset == Preset::Wall,
                true,
            ),
            (
                l.t("layout-lying"),
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
        l.t("curve"),
        vec![
            (
                l.t("curve-flat"),
                Action::Curve(Curve::Flat),
                st.curve == Curve::Flat,
                true,
            ),
            (
                l.t("curve-gentle"),
                Action::Curve(Curve::Gentle),
                st.curve == Curve::Gentle,
                s.curve_available,
            ),
            (
                l.t("curve-strong"),
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
        l.t("follow-head"),
        vec![
            (l.t("no"), Action::Follow(false), !st.follow, true),
            (l.t("yes"), Action::Follow(true), st.follow, true),
        ],
    );
    segmented(
        items,
        rx + half + 16.0,
        half,
        &mut y2,
        l.t("magnet"),
        vec![
            (l.t("no"), Action::Magnet(false), !st.magnet, true),
            (l.t("yes"), Action::Magnet(true), st.magnet, true),
        ],
    );
    y = y.max(y2);
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("drag"),
        vec![
            (l.t("drag-one"), Action::MoveAll(false), !st.move_all, true),
            (l.t("drag-all"), Action::MoveAll(true), st.move_all, true),
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
                    l.f("monitor-n", &[("n", (&m.label).into())])
                }
            })
            .collect();
        let from = items.len();
        segmented(
            items,
            rx,
            rw,
            &mut y,
            l.t("cinema-screen"),
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
        l.t("background"),
        vec![
            (l.t("background-black"), a1, s1, true),
            (l.t("background-room"), a2, s2, s.room_available),
            (l.t("background-dimmed"), a4, s4, s.room_available),
            (
                l.t("background-own"),
                a3,
                s3,
                s.panorama_available && s.custom_available,
            ),
        ],
    );
    // Where an own photo comes from, while there is none yet.
    if s.panorama_available && !s.custom_available {
        for (k, line) in [
            l.t("background-own-where"),
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
        (l.t("sky-night"), Sky::Night),
        (l.t("sky-sunset"), Sky::Sunset),
        (l.t("sky-space"), Sky::Space),
        (l.t("sky-aurora"), Sky::Aurora),
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
        l.t("focus"),
        vec![
            (l.t("off"), Action::Focus(false), !st.focus, true),
            (l.t("focus-dim-others"), Action::Focus(true), st.focus, true),
        ],
    );
    y -= 12.0;
    items.push(text(rx, y, 19.0, false, MUTED, l.t("focus-note")));
    y += 48.0;
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("keyboard-window"),
        vec![
            (
                l.t("off"),
                Action::KeyboardWindow(false),
                !st.keyboard_window,
                s.keyboard_available,
            ),
            (
                l.t("show"),
                Action::KeyboardWindow(true),
                st.keyboard_window,
                s.keyboard_available,
            ),
        ],
    );
    y -= 12.0;
    items.push(text(rx, y, 19.0, false, MUTED, l.t("keyboard-window-note")));
    y += 48.0;
    let every = st.rest_every;
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("rest"),
        [
            (0, l.t("off")),
            (20, l.t("rest-every-20")),
            (60, l.t("rest-hourly")),
        ]
        .into_iter()
        .map(|(m, label)| (label, Action::RestEvery(m), every == m, true))
        .collect(),
    );
    let levels = [100u8, 80, 60, 40];
    let labels = levels.map(|p| l.f("percent", &[("value", p.into())]));
    segmented(
        items,
        rx,
        rw,
        &mut y,
        l.t("brightness"),
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
        OfferWhy::New => l.t("offer-new"),
        OfferWhy::WrongCodes => l.t("offer-wrong-codes"),
        OfferWhy::Forgotten => l.t("offer-forgotten"),
    };
    items.push(text(x, y, 34.0, true, TEXT, title));
    y += 60.0;
    let host: String = host.chars().take(40).collect();
    items.push(text(x, y, 24.0, false, TEXT, &host));
    y += 44.0;
    for line in [l.t("offer-own-pc"), l.t("offer-code")] {
        items.push(text(x, y, 20.0, false, MUTED, line));
        y += 32.0;
    }
    y += 20.0;
    let half = (w - 12.0) / 2.0;
    let pair = match why {
        OfferWhy::New => l.t("offer-pair"),
        OfferWhy::WrongCodes => l.t("offer-try-again"),
        OfferWhy::Forgotten => l.t("offer-pair-again"),
    };
    items.push(button(
        rect(x, y, half, 56.0),
        pair,
        Style::Selected,
        Some(Action::PairAnswer(true)),
    ));
    items.push(button(
        rect(x + half + 12.0, y, half, 56.0),
        l.t("offer-not-now"),
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
        l.t("update-headset")
    } else {
        l.t("update-pc")
    };
    items.push(text(x, y, 34.0, true, TEXT, title));
    y += 60.0;
    let versions = l.f(
        "update-versions",
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
        l.t("update-versions-differ"),
    ));
    y += 50.0;
    items.push(Item::Box(rect(x, y, w, 130.0), [255, 255, 255, 12]));
    let (how, where_) = if pc_newer {
        (l.t(headset.how), headset.place)
    } else {
        (l.t("update-pc-from"), SITE)
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
        l.t("update-connects-by-itself"),
    ));
}

/// Prostir's site: the PC part, and which desktops it runs on (kept there, so a new
/// one needs no new headset app).
pub const SITE: &str = "prostir.ishawyha.dev";

/// Before any PC is found: what Prostir needs and where to get the PC part.
fn build_welcome(l: &Texts, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    items.push(text(x, y + 6.0, 26.0, true, TEXT, l.t("welcome-title")));
    y += 36.0;
    items.push(text(
        x,
        y,
        19.0,
        false,
        MUTED,
        &l.f("welcome-pc-app", &[("site", SITE.into())]),
    ));
    y += 30.0;
    let steps: [(&str, &str, bool); 3] = [
        (l.t("welcome-install"), SITE, true),
        (l.t("welcome-start"), l.t("welcome-start-note"), false),
        // USB needs developer mode and adb: not something a store install has.
        (
            l.t("welcome-finds"),
            l.t(if cfg!(feature = "store") {
                "welcome-finds-wifi"
            } else {
                "welcome-finds-wifi-usb"
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
    items.push(text(x, y, 28.0, true, TEXT, l.t("intro-title")));
    y += 34.0;
    let tips = [
        ("intro-menu", "intro-menu-text"),
        ("intro-move", "intro-move-text"),
        ("intro-line-up", "intro-line-up-text"),
        ("intro-finger", "intro-finger-text"),
        ("intro-off", "intro-off-text"),
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
        l.t("intro-done"),
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
    note: Option<&(String, bool)>,
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
    // What it asked the PC for and how that stands, in place of its state.
    let (state, color) = match note {
        Some((note, true)) => (note.as_str(), DANGER),
        Some((note, false)) => (note.as_str(), sub),
        None => (state, sub),
    };
    items.push(text_in(
        room,
        text(r.left() + 24.0, r.top() + 104.0, 19.0, false, color, state),
    ));
}

/// Everyday things in one place: profiles to switch to, and toggles as tiles.
fn build_quick(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    let l = &s.texts;
    let st = s.settings;
    let connected = s.host.is_some();
    items.push(text(x, y + 14.0, 21.0, true, SECTION, l.t("profile")));
    y += 32.0;
    let cw = (w - 2.0 * 12.0) / 3.0;
    for (i, saved) in s.saved_profiles.iter().enumerate() {
        let r = rect(x + i as f32 * (cw + 12.0), y, cw, 72.0);
        let active = s.active_profile == Some(i);
        let style = match (connected && *saved, active) {
            (false, _) => Style::Disabled,
            (true, true) => Style::Selected,
            (true, false) => Style::Normal,
        };
        let action = (connected && *saved && !active).then_some(Action::ApplyProfile(i));
        let name = l.f("profile-n", &[("n", (i + 1).into())]);
        items.push(button(r, &name, style, action));
        if let Some((note, failed)) = s.ask_note(Asked::Profile(i)) {
            let color = if failed { DANGER } else { MUTED };
            items.push(text(r.left(), r.bottom() + 22.0, 17.0, false, color, &note));
        } else if !saved {
            let empty = l.t("profile-empty");
            items.push(text(r.left(), r.bottom() + 22.0, 17.0, false, MUTED, empty));
        }
    }
    y += 72.0 + 40.0;
    let on_off = |on: bool| {
        if on { l.t("on") } else { l.t("off") }
    };
    let tw = (w - 2.0 * 12.0) / 3.0;
    let th = 140.0;
    let full = s
        .limits
        .as_ref()
        .is_none_or(|l| s.windows.len() >= usize::from(l.windows_max));
    let tiles: [(&str, String, bool, Option<Action>); 9] = [
        (
            l.t("quick-in-front"),
            l.t("quick-in-front-state").to_owned(),
            false,
            Some(Action::Recenter),
        ),
        (
            l.t("focus"),
            on_off(st.focus).to_owned(),
            st.focus,
            Some(Action::Focus(!st.focus)),
        ),
        (
            l.t("cinema"),
            on_off(st.cinema).to_owned(),
            st.cinema,
            Some(Action::Cinema(!st.cinema)),
        ),
        (
            l.t("keyboard-window"),
            on_off(st.keyboard_window).to_owned(),
            st.keyboard_window,
            s.keyboard_available
                .then_some(Action::KeyboardWindow(!st.keyboard_window)),
        ),
        (
            l.t("pc-sound"),
            if s.audio_wanted {
                l.t("sound-in-headset")
            } else {
                l.t("sound-on-pc")
            }
            .to_owned(),
            s.audio_wanted,
            connected.then_some(Action::SetAudio(!s.audio_wanted)),
        ),
        (
            l.t("microphone"),
            if s.mic_wanted {
                l.t("quick-mic-headset")
            } else {
                l.t("quick-mic-pc")
            }
            .to_owned(),
            s.mic_wanted,
            connected.then_some(Action::SetMic(!s.mic_wanted)),
        ),
        (
            l.t("quick-detach"),
            if full {
                l.t("quick-detach-full")
            } else {
                l.t("quick-detach-state")
            }
            .to_owned(),
            false,
            (connected && !full).then_some(Action::DetachWindow),
        ),
        (
            l.t("quick-window-here"),
            l.t("quick-window-here-state").to_owned(),
            false,
            connected.then_some(Action::WindowHere),
        ),
        (
            l.t("quick-screenshot"),
            l.t("quick-screenshot-state").to_owned(),
            false,
            connected.then_some(Action::Screenshot),
        ),
    ];
    let asked = [
        None,
        None,
        None,
        None,
        Some(Asked::Sound),
        None,
        Some(Asked::DetachWindow),
        Some(Asked::WindowHere),
        Some(Asked::Screenshot),
    ];
    for (i, (title, state, on, action)) in tiles.into_iter().enumerate() {
        let r = rect(
            x + (i % 3) as f32 * (tw + 12.0),
            y + (i / 3) as f32 * (th + 12.0),
            tw,
            th,
        );
        let note = asked[i].and_then(|what| s.ask_note(what));
        tile(items, r, title, &state, on, action, note.as_ref());
    }
}

/// Saves the current setup into one of the profiles (second press confirms).
fn save_as_profile(s: &UiState, items: &mut Vec<Item>, x: f32, w: f32, mut y: f32) {
    let l = &s.texts;
    items.push(text(x, y, 21.0, true, SECTION, l.t("profile-save")));
    y += 18.0;
    let bw = (w - 2.0 * 10.0) / 3.0;
    // Something to save: the PC's monitors.
    let can = s.host.is_some() && !s.monitors.is_empty();
    for i in 0..s.saved_profiles.len() {
        let armed = s.confirm_save == Some(i);
        let label = if armed {
            l.t("profile-replace").to_owned()
        } else {
            l.f("profile-n", &[("n", (i + 1).into())])
        };
        // The profile the monitors are as now: saved to or switched to last.
        let active = s.active_profile == Some(i);
        let style = match (can, armed, active) {
            (false, _, _) => Style::Disabled,
            (true, true, _) => Style::Armed,
            (true, false, true) => Style::Selected,
            (true, false, false) => Style::Normal,
        };
        items.push(button(
            rect(x + i as f32 * (bw + 10.0), y, bw, 48.0),
            &label,
            style,
            can.then_some(Action::SaveProfile(i)),
        ));
    }
}

/// Settings: the parts listed on the left, the chosen one on the right.
fn build_settings(s: &UiState, section: Section, items: &mut Vec<Item>, top: f32) {
    let l = &s.texts;
    let (nx, nw) = (44.0, 230.0);
    let sections = [
        (Section::Look, l.t("section-look")),
        (Section::Controls, l.t("section-controls")),
        (Section::Sound, l.t("section-sound")),
        (Section::Language, l.t("section-language")),
        (Section::Pc, l.t("section-pc")),
        (Section::Help, l.t("section-help")),
    ];
    for (i, (sec, label)) in sections.into_iter().enumerate() {
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
    settings_page(s, section, items, cx, cw, ctop);
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
                l.t("language-title"),
                vec![
                    choice(l.t("language-like-pc"), Language::LikePc),
                    choice(l.t("language-like-system"), Language::LikeSystem),
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
                    l.t("language-more"),
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
                l.t("time"),
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
                l.t("date-like-menu"),
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
            segmented(items, cx, w, &mut y, l.t("date"), options);
        }
        Section::Pc => {
            let w = cw.min(700.0);
            let mut y = build_pc(s, items, cx, w, ctop);
            let on = s.settings.notifications;
            segmented(
                items,
                cx,
                w,
                &mut y,
                l.t("pc-notifications"),
                vec![
                    (l.t("off"), Action::Notifications(false), !on, true),
                    (l.t("on"), Action::Notifications(true), on, true),
                ],
            );
            paired_pcs(s, items, cx, w, y + 10.0);
        }
        Section::Controls => build_controls(s, items, cx, cw.min(640.0), ctop),
        Section::Help => {
            hints(l, items, cx, ctop + 10.0);
            items.push(button(
                rect(cx, ctop + 170.0, 300.0, 50.0),
                l.t("help-show-tips"),
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
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("hands"),
        vec![
            (
                l.t("hide"),
                Action::Hands(HandMode::Hidden),
                st.hands == HandMode::Hidden,
                true,
            ),
            (
                l.t("show"),
                Action::Hands(HandMode::Visible),
                st.hands == HandMode::Visible,
                true,
            ),
            (
                l.t("hands-use"),
                Action::Hands(HandMode::Control),
                st.hands == HandMode::Control,
                true,
            ),
        ],
    );
    let on_off = |on: bool, act: fn(bool) -> Action, enabled: bool| {
        vec![
            (l.t("off"), act(false), !on, enabled),
            (l.t("on"), act(true), on, enabled),
        ]
    };
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("gaze-mouse"),
        on_off(st.gaze_mouse, Action::GazeMouse, s.can_move_mouse),
    );
    items.push(text(
        lx,
        y - 18.0,
        18.0,
        false,
        MUTED,
        if s.can_move_mouse || s.host.is_none() {
            l.t("gaze-mouse-note")
        } else {
            l.t("gaze-mouse-unavailable")
        },
    ));
    y += 22.0;
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("menu-size"),
        vec![
            (
                l.t("menu-size-normal"),
                Action::LargeMenu(false),
                !st.large_menu,
                true,
            ),
            (
                l.t("menu-size-large"),
                Action::LargeMenu(true),
                st.large_menu,
                true,
            ),
        ],
    );
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("palm-menu"),
        vec![
            (
                l.t("palm-menu-left"),
                Action::LeftHanded(false),
                !st.left_handed,
                true,
            ),
            (
                l.t("palm-menu-right"),
                Action::LeftHanded(true),
                st.left_handed,
                true,
            ),
        ],
    );
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("display-rate"),
        vec![
            (
                l.t("display-rate-normal"),
                Action::BatterySaver(false),
                !st.battery_saver,
                true,
            ),
            (
                l.t("display-rate-saver"),
                Action::BatterySaver(true),
                st.battery_saver,
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
        l.t("display-rate-note"),
    ));
    y += 22.0;
    segmented(
        items,
        lx,
        lw,
        &mut y,
        l.t("menu-button"),
        vec![
            (l.t("show"), Action::MenuButton(true), st.menu_button, true),
            (
                l.t("hide"),
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
    items.push(text(x, y, 21.0, true, SECTION, l.t("paired-pcs")));
    y += 14.0;
    if s.paired.is_empty() {
        items.push(text(x, y + 26.0, 19.0, false, MUTED, l.t("paired-none")));
        return;
    }
    // Two fit; with more, one and how many others.
    let shown = if s.paired.len() > 2 {
        1
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
                l.t("paired-forget-confirm")
            } else {
                l.t("paired-forget")
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
            &l.f("paired-more", &[("count", (s.paired.len() - shown).into())]),
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
            l.t("pc-not-connected"),
        ));
        return y + 70.0;
    };
    let wanted = pc.wanted;
    let can = |codec: Codec, gpu: bool| pc.choices.contains(&(codec, gpu));
    let any_gpu = pc.choices.iter().any(|(_, gpu)| *gpu);
    let set = |change: EncodingChange| Action::SetEncoding(change);
    let none = EncodingChange::default();
    // How the last change stands, after the first title.
    let title = match s.ask_note(Asked::Encoding) {
        Some((note, _)) => format!("{} · {note}", l.t("pc-quality")),
        None => l.t("pc-quality").to_owned(),
    };
    segmented(
        items,
        x,
        w,
        &mut y,
        &title,
        [
            (15, l.t("pc-quality-economy")),
            (30, l.t("pc-quality-normal")),
            (50, l.t("pc-quality-high")),
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
        l.t("pc-encoding-on"),
        vec![
            (
                l.t("pc-gpu"),
                set(EncodingChange {
                    software: Some(false),
                    ..none
                }),
                gpu,
                any_gpu,
            ),
            (
                l.t("pc-cpu"),
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
        l.t("pc-codec"),
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
    items.push(text(x, y - 18.0, 18.0, false, MUTED, l.t("pc-codec-note")));
    y += 22.0;
    segmented(
        items,
        x,
        w,
        &mut y,
        l.t("pc-sharp"),
        vec![
            (
                l.t("off"),
                set(EncodingChange {
                    sharp: Some(false),
                    ..none
                }),
                !wanted.sharp,
                gpu,
            ),
            (
                l.t("on"),
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

/// PC sound and headset microphone.
fn build_sound(s: &UiState, items: &mut Vec<Item>, lx: f32, lw: f32, mut y: f32) {
    let l = &s.texts;
    // The current state is the highlighted option, not a button label to decode.
    y += 18.0;
    let title = match s.ask_note(Asked::Sound) {
        Some((note, _)) => format!("{} · {note}", l.t("sound-plays")),
        None => l.t("sound-plays").to_owned(),
    };
    segmented(
        items,
        lx,
        lw,
        &mut y,
        &title,
        vec![
            (
                l.t("sound-in-headset"),
                Action::SetAudio(true),
                s.audio_wanted,
                true,
            ),
            (
                l.t("sound-on-pc"),
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
        l.t("sound-apps-hear"),
        vec![
            (
                l.t("sound-mic-headset"),
                Action::SetMic(true),
                s.mic_wanted,
                true,
            ),
            (
                l.t("sound-mic-pc"),
                Action::SetMic(false),
                !s.mic_wanted,
                true,
            ),
        ],
    );
    y -= 24.0;
    if s.mic_denied {
        for line in [l.t("sound-mic-denied-1"), l.t("sound-mic-denied-2")] {
            items.push(text(lx, y, 19.0, false, DANGER, line));
            y += 30.0;
        }
        items.push(button(
            rect(lx, y - 8.0, 260.0, 48.0),
            l.t("sound-ask-again"),
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
        l.t("sound-from-screen"),
        vec![
            (
                l.t("off"),
                Action::SoundFromScreen(false),
                !s.settings.sound_from_screen,
                true,
            ),
            (
                l.t("on"),
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
        l.t("sound-from-screen-note"),
    ));
    y += 30.0;
    for line in [
        l.t("sound-about-1"),
        l.t("sound-about-2"),
        l.t("sound-about-3"),
        l.t("sound-about-4"),
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

// Shapes that come out degenerate (negative or non-finite sizes) are a bug, caught
// by the debug asserts in tests. In the app they are drawn as nothing: one bad shape
// must not close the menu, and with it the app.

/// A circle, `None` if degenerate.
fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    let path = PathBuilder::from_circle(cx, cy, r);
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
    }
}

/// A rounded rectangle, `None` if degenerate.
fn rounded(r: Rect, radius: f32) -> Option<Path> {
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0);
    let k = rad * 0.552_284_8;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - rad + k, t, rt, t + rad - k, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - rad + k, rt - rad + k, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - k, b, l, b - rad + k, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - k, l + rad - k, t, l + rad, t);
    pb.close();
    let path = pb.finish();
    debug_assert!(path.is_some(), "degenerate rounded rect {r:?}");
    path
}

/// Outlines `path`; nothing when there is none (a degenerate shape).
fn stroke(pixmap: &mut Pixmap, path: Option<Path>, [r, g, b, a]: [u8; 4], width: f32) {
    let Some(path) = path else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// Fills `path`; nothing when there is none (a degenerate shape).
fn fill(pixmap: &mut Pixmap, path: Option<Path>, [r, g, b, a]: [u8; 4]) {
    let Some(path) = path else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display `n` of the one PC of these tests.
    fn k(n: u32) -> DisplayKey {
        DisplayKey::new(crate::ids::PcId([7; 8]), prostir_proto::state::DisplayId(n))
    }
    use crate::store::{NoStore, Store as _};

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
            }],
            host: Some("workstation".into()),
            found: Vec::new(),
            ip_input: None,
            monitors: vec![
                MonitorEntry {
                    id: k(101),
                    label: "1".into(),
                    width: 2560,
                    height: 1440,
                    place: Place::Unknown,
                    seen: Some([-1.0, 0.0, 0.5, 0.28]),
                },
                MonitorEntry {
                    id: k(102),
                    label: "2".into(),
                    width: 2560,
                    height: 1440,
                    place: Place::Unknown,
                    seen: Some([0.0, 0.0, 0.5, 0.28]),
                },
                MonitorEntry {
                    id: k(103),
                    label: "4".into(),
                    width: 1440,
                    height: 2560,
                    place: Place::Unknown,
                    seen: Some([0.78, 0.12, 0.28, 0.5]),
                },
            ],
            selected: None,
            mirror_seen: Some([0.0, -0.55, 0.35, 0.22]),
            primary: Some(k(102)),
            cinema_screen: None,
            mirror_wanted: true,
            audio_wanted: true,
            mic_wanted: true,
            mirror_name: Some("Екран HDMI-A-1".into()),
            settings: Settings::default(),
            room_available: true,
            panorama_available: true,
            custom_available: false,
            curve_available: true,
            keyboard_available: true,
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
            saved_profiles: [true, false, true],
            active_profile: None,
            asks: Vec::new(),
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

    /// The switch of the PC's display names the display it acts on; with none of the
    /// PC's displays connected it says so and switches nothing.
    #[test]
    fn the_pc_display_switch_names_its_display() {
        let items = |s: &UiState| Panel::new().build(s);
        let switches = |s: &UiState| {
            items(s)
                .into_iter()
                .filter(|i| {
                    matches!(
                        i,
                        Item::Button {
                            action: Some(Action::SetMirror(_)),
                            ..
                        }
                    )
                })
                .count()
        };
        let says = |s: &UiState, what: &str| {
            items(s)
                .iter()
                .any(|i| matches!(i, Item::Text { text, .. } if text == what))
        };
        let mut s = sample_state();
        s.tab = Tab::Monitors;
        s.mirror_wanted = false;
        assert!(says(&s, "Екран HDMI-A-1"));
        assert_eq!(switches(&s), 1, "the option not chosen");
        s.mirror_name = None;
        assert!(says(&s, "Екран ПК · не підключено"));
        assert_eq!(switches(&s), 0);
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

    /// The profile buttons of a tab: label, style, action.
    fn profile_buttons(st: &UiState) -> Vec<(String, Style, Option<Action>)> {
        Panel::new()
            .build(st)
            .into_iter()
            .filter_map(|i| match i {
                Item::Button {
                    label,
                    style,
                    action,
                    ..
                } if label.starts_with("Profile") || label == "Replace?" => {
                    Some((label, style, action))
                }
                _ => None,
            })
            .collect()
    }

    /// The profile saved to or switched to last is the one highlighted, where it is
    /// switched to (Quick) and where it is saved (Monitors); saving to it again asks
    /// first, as for any saved one.
    #[test]
    fn the_active_profile_is_highlighted() {
        let mut st = sample_state();
        st.texts = crate::lang::builtin("en");
        st.active_profile = Some(1);
        st.saved_profiles = [true, true, false];

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
                ("Profile 2".into(), Style::Selected, None),
                ("Profile 3".into(), Style::Disabled, None),
            ]
        );

        st.tab = Tab::Monitors;
        let save = profile_buttons(&st);
        assert_eq!(
            save,
            [
                (
                    "Profile 1".into(),
                    Style::Normal,
                    Some(Action::SaveProfile(0))
                ),
                (
                    "Profile 2".into(),
                    Style::Selected,
                    Some(Action::SaveProfile(1))
                ),
                (
                    "Profile 3".into(),
                    Style::Normal,
                    Some(Action::SaveProfile(2))
                ),
            ]
        );

        // Pressed once to save over it: it asks, and the question shows over the mark.
        st.confirm_save = Some(1);
        assert_eq!(
            profile_buttons(&st)[1],
            (
                "Replace?".into(),
                Style::Armed,
                Some(Action::SaveProfile(1))
            )
        );

        // Changed by hand: no profile is the active one.
        st.confirm_save = None;
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
                Tab::Settings(Section::Help),
            ] {
                let mut st = sample_state();
                st.texts = lang.clone();
                st.tab = tab;
                states.push(st.clone());
                st.confirm_remove = Some(k(103));
                st.settings.cinema = true;
                st.settings.focus = true;
                st.mic_wanted = false;
                st.audio_wanted = false;
                st.mirror_wanted = false;
                st.mirror_name = None;
                st.active_profile = Some(1);
                st.error = Some(lang.f("monitors-at-most", &[("count", 6.into())]));
                states.push(st.clone());
                // Every control waiting for the PC, then every one failed, on the
                // pages that have them.
                let asks_here = matches!(
                    tab,
                    Tab::Quick | Tab::Monitors | Tab::Settings(Section::Sound | Section::Pc)
                );
                let every = [
                    Asked::AddMonitor,
                    Asked::Remove(k(102)),
                    Asked::MakePrimary(k(103)),
                    Asked::Mirror,
                    Asked::Sound,
                    Asked::Encoding,
                    Asked::DetachWindow,
                    Asked::WindowHere,
                    Asked::Screenshot,
                    Asked::Profile(0),
                ];
                let answers = [
                    AskState::Waiting,
                    AskState::Failed {
                        why: Failure::Pc(ErrorCode::LIMIT),
                        detail: "At most 6 monitors".into(),
                    },
                    AskState::Failed {
                        why: Failure::Disconnected,
                        detail: String::new(),
                    },
                ];
                for state in answers.iter().filter(|_| asks_here) {
                    let mut asking = st.clone();
                    asking.asks = every
                        .iter()
                        .map(|&what| AskView {
                            what,
                            state: state.clone(),
                        })
                        .collect();
                    states.push(asking);
                }
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
                    })
                    .collect();
                states.push(st);
            }
            for why in [OfferWhy::New, OfferWhy::WrongCodes, OfferWhy::Forgotten] {
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
                            Item::Text { text, size, .. } => Some((text.clone(), *size)),
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
        st.mirror_seen = spots.get(&MIRROR_ID).copied();
        st.monitors = monitors
            .iter()
            .map(|&(id, width, height, ..)| MonitorEntry {
                id: k(id),
                label: (id + 1).to_string(),
                width,
                height,
                place: Place::Center,
                seen: spots.get(&id).copied(),
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
}
