//! Prostir's local API: what a program of this user on the PC can ask of the PC app
//! (`prostir ctl`, the settings window, scripts, hot keys), what it gets back, what it
//! is told when something happens, and the whole state it can show. The PC app is the
//! service; every client builds on these same types, so the two sides cannot drift.
//!
//! Everything here is JSON and nothing else: no transport (D-Bus on Linux, a named pipe
//! on Windows later), no user interface. `LOCAL-API.md` at the top of the repository
//! describes the D-Bus interface around it and the schema; the tests below pin the
//! schema, so a change shows in review.
//!
//! Reading is lenient where a newer service or client may send more: unknown fields
//! are ignored, a missing field of the state takes its default, and an unknown value
//! of a closed set reads as `Other`. Requests are read strictly by the service: a
//! request it cannot read is refused (`Unsupported` for a kind it does not know,
//! `BadArgument` otherwise) rather than guessed at.

use serde::{Deserialize, Serialize};

/// Reads an enum of names leniently: a name this build does not know, or a value
/// that is no name at all, reads as `Other` (what a newer side may send). The names
/// are those `Serialize` writes.
macro_rules! lenient {
    ($ty:ident: $($var:ident),+) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = serde_json::Value::deserialize(d)?;
                Ok([$($ty::$var),+]
                    .into_iter()
                    .find(|v| serde_json::to_value(v).ok().as_ref() == Some(&value))
                    .unwrap_or($ty::Other))
            }
        }
    };
}

lenient!(ApiError: Unsupported, NotAllowed, UnknownDisplay, Limit, BadSize, LastDisplay, NoWindow, Desktop, Encoder, Capture, Sound, NotOnDisplays, NoSuchProfile, BadArgument, NotConnected, TimedOut, Disconnected);
lenient!(Via: Wifi, Usb);
lenient!(Reason: Quit, TakenOff, Silence, Forgotten);
lenient!(Phase: Idle, Live, Lingering);
lenient!(ThemeMode: System, Light, Dark);
lenient!(Role: Main, Guest);
lenient!(DisplayKind: Virtual, Physical, Window);
lenient!(DisplayState: Starting, Running, Failed, Absent, Kept);
lenient!(Codec: H264, H265, Av1);
lenient!(MicState: Off, Ready, Failed);
lenient!(Counting: Unasked, Yes, No);

/// The service's name on the session bus.
pub const BUS_NAME: &str = "io.github.prostir_app.Prostir";
/// Its object.
pub const OBJECT_PATH: &str = "/io/github/prostir_app/Prostir";
/// Its interface. An incompatible change gets `Prostir2` next to it.
pub const INTERFACE: &str = "io.github.prostir_app.Prostir1";
/// Prefix of the service's D-Bus errors: `<prefix>NotConnected` and so on.
pub const ERROR_PREFIX: &str = "io.github.prostir_app.Prostir1.Error.";
/// The settings window's name on the bus (one window per user).
pub const SETTINGS_NAME: &str = "io.github.prostir_app.Prostir.Settings";

/// How long the service waits for the headset to answer a command.
pub const COMMAND_SECONDS: u64 = 30;
/// How long a client waits for a reply: a little more than a command takes.
pub const CALL_SECONDS: u64 = 35;
/// `StateChanged` goes out at most this many times a second.
pub const STATES_PER_SECOND: u32 = 10;

/// The version of [`State`] (its `v`): a change that older clients would misread gets
/// a new number. New fields do not.
pub const STATE_VERSION: u32 = 1;

/// Anything one can ask of Prostir. On the wire `{"request": "recenter"}`,
/// `{"request": "add_monitor", "w": 2560, "h": 1440, "scale": 125}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    // Commands for the headset: answered once the main headset answers, within
    // `COMMAND_SECONDS`.
    Recenter,
    LineUp,
    ShowMouse,
    ResetArrangement,
    /// Switch to a profile saved in the headset: its name, or its number from 1 as
    /// the headset's menu lists them.
    Profile {
        name: String,
    },
    /// Gives profile `profile` (its name or number) the name `name`; an empty one
    /// gives it back the headset menu's own ("Profile 2").
    RenameProfile {
        profile: String,
        name: String,
    },
    Cinema {
        on: bool,
    },
    Focus {
        on: bool,
    },
    Follow {
        on: bool,
    },
    NoLimits {
        on: bool,
    },
    /// "Locked": the monitors stay where they are, no hand in the headset takes one.
    Lock {
        on: bool,
    },
    Preset {
        preset: u8,
    },
    // The PC's own state: answered in the same pass that changed it, after the
    // `StateChanged` that shows the change.
    /// A virtual monitor of `w`x`h` pixels at `scale` percent: answers its `id` at
    /// once, and the display stands `starting` in the state until it runs.
    AddMonitor {
        w: u32,
        h: u32,
        #[serde(default = "full_scale")]
        scale: u16,
    },
    RemoveMonitor {
        id: u32,
    },
    /// Tries a failed display again now.
    RetryMonitor {
        id: u32,
    },
    /// A display of the PC shown in the headset or not.
    ShowPhysical {
        id: u32,
        shown: bool,
    },
    /// Another scale for a monitor, percent: in place where the desktop does that,
    /// else made anew at it, same id. It takes the monitor out of Auto.
    SetScale {
        id: u32,
        scale: u16,
    },
    /// Auto on or off for a monitor: its scale chosen by the main headset from how
    /// large it shows the monitor (`auto_scale` of the display).
    SetScaleAuto {
        id: u32,
        on: bool,
    },
    SetPrimary {
        id: u32,
    },
    /// Moves a display on the desktop to about `x`, `y` (its top-left corner, logical
    /// pixels); the desktop's arrangement lines it up. While the monitors are on the
    /// desktop; the main headset's next arrangement replaces it.
    Place {
        id: u32,
        x: i32,
        y: i32,
    },
    /// A picture of a virtual monitor or a window panel, saved as a PNG in the
    /// Pictures folder as the headset's screenshots are: answers its `path` (and
    /// `whole` where the desktop could only take all of it), once the desktop took it.
    Screenshot {
        id: u32,
    },
    /// Mbit/s for each monitor, 5 to 100.
    SetQuality {
        mbit: u32,
    },
    /// The slower preset that keeps small text clearer.
    SetSharp {
        on: bool,
    },
    SetCodec {
        codec: CodecChoice,
        /// Encode on the CPU even where the graphics card could.
        #[serde(default)]
        cpu: bool,
    },
    /// The PC's sound plays in the headset (`true`) or on the PC.
    SoundToHeadset {
        on: bool,
    },
    /// The main headset's volume (0 to 100, unmuted), while the PC shares it
    /// (`sound.volume` is not null); `Unsupported` otherwise, `BadArgument` above 100.
    Volume {
        level: u8,
    },
    /// The headset microphone's switch (`Unsupported`: it is the headset user's,
    /// changed only in the headset).
    SetMic {
        on: bool,
    },
    /// The answer to "Count this PC among users?": `yes`, `no`, or `unasked` to put
    /// the question back.
    SetCounting {
        counting: Counting,
    },
    /// Start Prostir with the session.
    Autostart {
        on: bool,
    },
    /// The settings window's look, kept in `settings.txt`.
    WindowLook {
        theme: ThemeMode,
        palette: String,
        #[serde(default)]
        language: Option<String>,
    },
    /// Lets a new headset pair for a while: answers `seconds_left` (from step 6.8;
    /// until then `Unsupported`).
    PairNewHeadset,
    StopPairing,
    /// The code a headset shows, typed on the PC (from step 6.8).
    PairingCode {
        code: String,
    },
    /// Forgets one paired headset, by its `key` (fingerprint), its whole key or its
    /// name; its session ends.
    ForgetHeadset {
        headset: String,
    },
    ForgetAll,
    /// Opens the settings window.
    ShowSettings,
    Quit,
}

fn full_scale() -> u16 {
    100
}

/// Every request there is, by its name on the wire: tells a request of another
/// version from a mistyped one.
pub const REQUESTS: &[&str] = &[
    "recenter",
    "line_up",
    "show_mouse",
    "reset_arrangement",
    "profile",
    "rename_profile",
    "cinema",
    "focus",
    "follow",
    "no_limits",
    "lock",
    "preset",
    "add_monitor",
    "remove_monitor",
    "retry_monitor",
    "show_physical",
    "set_scale",
    "set_scale_auto",
    "set_primary",
    "place",
    "screenshot",
    "set_quality",
    "set_sharp",
    "set_codec",
    "sound_to_headset",
    "volume",
    "set_mic",
    "set_counting",
    "autostart",
    "window_look",
    "pair_new_headset",
    "stop_pairing",
    "pairing_code",
    "forget_headset",
    "forget_all",
    "show_settings",
    "quit",
];

impl Request {
    /// Reads a request; what is wrong with it otherwise.
    pub fn from_json(text: &str) -> Result<Self, Failure> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| Failure::new(ApiError::BadArgument, format!("not JSON: {e}")))?;
        let known = value
            .get("request")
            .and_then(serde_json::Value::as_str)
            .map(|d| REQUESTS.contains(&d));
        let request = serde_json::from_value(value).map_err(|e| match known {
            Some(false) => Failure::new(ApiError::Unsupported, e.to_string()),
            _ => Failure::new(ApiError::BadArgument, e.to_string()),
        })?;
        // The state reads an answer it does not know leniently; a request may not.
        if let Self::SetCounting {
            counting: Counting::Other,
        } = request
        {
            return Err(Failure::new(
                ApiError::BadArgument,
                "counting: expected yes, no or unasked",
            ));
        }
        Ok(request)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a request always serializes")
    }

    /// Whether this is a command for the headset (answered by the headset).
    pub fn for_headset(&self) -> bool {
        matches!(
            self,
            Self::Recenter
                | Self::LineUp
                | Self::ShowMouse
                | Self::ResetArrangement
                | Self::Profile { .. }
                | Self::RenameProfile { .. }
                | Self::Cinema { .. }
                | Self::Focus { .. }
                | Self::Follow { .. }
                | Self::NoLimits { .. }
                | Self::Lock { .. }
                | Self::Preset { .. }
        )
    }
}

/// The codec asked for: `auto` lets the PC pick (H.265 where the headset and the PC
/// can, else H.264).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodecChoice {
    H264,
    H265,
    Auto,
}

/// What went wrong, by name: the D-Bus error is `ERROR_PREFIX` and the name. The
/// first ones are the headset protocol's codes; `NotConnected`, `TimedOut` and
/// `Disconnected` are the local API's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ApiError {
    Unsupported,
    NotAllowed,
    UnknownDisplay,
    Limit,
    BadSize,
    LastDisplay,
    NoWindow,
    Desktop,
    Encoder,
    Capture,
    Sound,
    NotOnDisplays,
    NoSuchProfile,
    BadArgument,
    /// No headset is connected for a command for the headset.
    NotConnected,
    /// The headset did not answer within `COMMAND_SECONDS`.
    TimedOut,
    /// The headset left before it answered.
    Disconnected,
    /// Anything else, and any name a newer Prostir sends that this one does not know.
    #[default]
    Other,
}

impl ApiError {
    /// Every one of them.
    pub const ALL: [Self; 18] = [
        Self::Unsupported,
        Self::NotAllowed,
        Self::UnknownDisplay,
        Self::Limit,
        Self::BadSize,
        Self::LastDisplay,
        Self::NoWindow,
        Self::Desktop,
        Self::Encoder,
        Self::Capture,
        Self::Sound,
        Self::NotOnDisplays,
        Self::NoSuchProfile,
        Self::BadArgument,
        Self::NotConnected,
        Self::TimedOut,
        Self::Disconnected,
        Self::Other,
    ];

    /// The name, as in the JSON and the D-Bus error.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unsupported => "Unsupported",
            Self::NotAllowed => "NotAllowed",
            Self::UnknownDisplay => "UnknownDisplay",
            Self::Limit => "Limit",
            Self::BadSize => "BadSize",
            Self::LastDisplay => "LastDisplay",
            Self::NoWindow => "NoWindow",
            Self::Desktop => "Desktop",
            Self::Encoder => "Encoder",
            Self::Capture => "Capture",
            Self::Sound => "Sound",
            Self::NotOnDisplays => "NotOnDisplays",
            Self::NoSuchProfile => "NoSuchProfile",
            Self::BadArgument => "BadArgument",
            Self::NotConnected => "NotConnected",
            Self::TimedOut => "TimedOut",
            Self::Disconnected => "Disconnected",
            Self::Other => "Other",
        }
    }

    /// The error of `name`; `Other` for one this build does not know.
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|e| e.name() == name)
            .unwrap_or(Self::Other)
    }
}

/// A failure: its name and a text in the PC's language, for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub error: ApiError,
    pub text: String,
}

impl Failure {
    pub fn new(error: ApiError, text: impl Into<String>) -> Self {
        Self {
            error,
            text: text.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.text.is_empty() {
            f.write_str(self.error.name())
        } else {
            write!(f, "{}: {}", self.error.name(), self.text)
        }
    }
}

impl std::error::Error for Failure {}

/// What a request gave, as one JSON object: `{"revision": 7}`,
/// `{"id": 103, "revision": 8}` (a display made), `{"seconds_left": 180, "revision":
/// 9}` (pairing open), `{"path": "/home/me/Pictures/Prostir/1.png", "whole": false,
/// "revision": 9}` (a screenshot). `revision` is that of the state that already has
/// the change.
///
/// On D-Bus a failure is a D-Bus error instead (its name and text); a transport
/// without errors of its own (the named pipe) puts `error` and `text` here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Answer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds_left: Option<u32>,
    /// The file a screenshot was saved to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The screenshot shows the whole desktop, not only the display asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whole: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub revision: u32,
}

impl Answer {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("an answer always serializes")
    }

    /// The failure it carries, if it is one.
    pub fn failure(&self) -> Option<Failure> {
        self.error
            .map(|e| Failure::new(e, self.text.clone().unwrap_or_default()))
    }
}

/// How a headset is connected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    #[default]
    Wifi,
    Usb,
    Other,
}

impl Via {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Usb => "usb",
            Self::Other => "other",
        }
    }
}

/// Why a headset's session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// The headset app or the PC app quit.
    Quit,
    TakenOff,
    /// Nothing came from it for 5 s.
    Silence,
    Forgotten,
    Other,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::TakenOff => "taken-off",
            Self::Silence => "silence",
            Self::Forgotten => "forgotten",
            Self::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Self {
        [Self::Quit, Self::TakenOff, Self::Silence, Self::Forgotten]
            .into_iter()
            .find(|r| r.name() == name)
            .unwrap_or(Self::Other)
    }
}

/// What happened to a headset (each is a D-Bus signal too). `headset` is its `key`
/// (as in [`Headset::key`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    HeadsetConnected {
        headset: String,
        name: String,
        via: Via,
    },
    HeadsetDisconnected {
        headset: String,
        reason: Reason,
    },
    HeadsetWorn {
        headset: String,
        worn: bool,
    },
    /// An event this build does not know (from a newer Prostir).
    #[serde(other)]
    Other,
}

/// Everything there is to show of Prostir, as one JSON object (`GetState`,
/// `StateChanged`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// [`STATE_VERSION`].
    pub v: u32,
    pub pc: Pc,
    /// The settings window's own look, kept with the rest of the settings.
    pub window: WindowLook,
    /// Starting with the session; `null`: this PC cannot set it from here.
    pub autostart: Option<bool>,
    /// Paired headsets, those with a session first (in the order they connected).
    pub headsets: Vec<Headset>,
    /// A headset being connected now (encryption, versions), by name.
    pub connecting: Option<String>,
    /// In the order of the rows, then the rest.
    pub displays: Vec<Display>,
    /// The desktop's primary display.
    pub primary: Option<u32>,
    pub limits: Limits,
    pub encoding: Encoding,
    pub sound: Sound,
    /// While the PC is pairing.
    pub pairing: Option<Pairing>,
    /// Headsets that asked for this PC recently, by name (from step 6.8).
    pub nearby: Vec<String>,
    /// The answer to "Count this PC among users?"; `null`: this build or this run
    /// never counts (a debug build, no `count` feature or key, `PROSTIR_NO_COUNT=1`), so
    /// there is nothing to ask.
    pub counting: Option<Counting>,
    /// What is wrong now, in the PC's language.
    pub problems: Vec<Problem>,
    /// The PC's Wi-Fi, when it reaches its network over Wi-Fi, as found when the last
    /// headset connected over the network; `null` on a cable or before.
    pub wifi: Option<Wifi>,
}

impl State {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("the state always serializes")
    }

    /// Reads a state, leniently.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The main headset in a session now, else the first one.
    pub fn connected(&self) -> Option<(&Headset, &Session)> {
        let sessions = || {
            self.headsets
                .iter()
                .filter_map(|h| h.session.as_ref().map(|s| (h, s)))
        };
        sessions()
            .find(|(_, s)| s.main)
            .or_else(|| sessions().next())
    }

    pub fn display(&self, id: u32) -> Option<&Display> {
        self.displays.iter().find(|d| d.id == id)
    }
}

/// The PC's own Wi-Fi link.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wifi {
    /// `wlan0`.
    pub interface: String,
    /// The card's power saving: on, it can hold back what comes for the PC by tens of
    /// milliseconds. `null` when the card does not say.
    pub power_save: Option<bool>,
    /// The channel's frequency and width in MHz; 0 when not known.
    pub freq_mhz: u32,
    pub width_mhz: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pc {
    /// The name headsets see.
    pub name: String,
    /// The PC app's version.
    pub app: String,
    /// `KDE Plasma`, `Hyprland`; empty while there are no monitors.
    pub desktop: String,
    pub phase: Phase,
    /// The PC's addresses on its networks (`192.168.1.10`; a VPN's too), as a person
    /// types one into a headset that does not find the PC by itself (from step 7.9;
    /// empty before).
    pub addresses: Vec<String>,
}

/// Where Prostir is between headsets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Waiting for a headset, without monitors.
    #[default]
    Idle,
    /// A headset is connected.
    Live,
    /// The monitors wait a while for the headset to come back.
    Lingering,
    Other,
}

/// The person's answer to "Count this PC among users?". Nothing is counted until it
/// is `Yes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Counting {
    /// Not asked yet, or not answered: the settings window asks.
    #[default]
    Unasked,
    Yes,
    No,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// As the desktop says.
    #[default]
    System,
    Light,
    Dark,
    Other,
}

impl ThemeMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::System | Self::Other => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowLook {
    pub theme: ThemeMode,
    /// `ink` or `graphite`.
    pub palette: String,
    /// A language code (`uk`, `en`); `null`: the desktop's.
    pub language: Option<String>,
}

impl Default for WindowLook {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            palette: "ink".into(),
            language: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Headset {
    /// Its key's fingerprint (12 hex digits): how requests and events name it.
    pub key: String,
    pub name: String,
    /// The model it says it is, while connected.
    pub model: String,
    /// Its Prostir app's version as it last connected (`0.2.0`), where known: two
    /// apps on one headset (a release and a test build) are two headsets.
    pub app: String,
    /// When it last connected or left, in Unix seconds, where known.
    pub last_seen: Option<u64>,
    /// Its role by its line in `settings.txt` (a headset without one is `main`).
    pub role: Role,
    /// Its rights: `input`, `mic`, `camera`, `clipboard`, `changes`, `private`.
    pub rights: Vec<String>,
    /// Its session, while it is connected.
    pub session: Option<Session>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Main,
    Guest,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub via: Via,
    /// On the head; streaming pauses while no headset is.
    pub worn: bool,
    pub battery: Option<Battery>,
    /// The headset's profile in use, where it says: its name, or its number from 1
    /// where it has none (what `profile` takes).
    pub profile: Option<String>,
    /// The profiles saved in the headset, in its menu's order, where it says: the name
    /// each was given, empty for one called by its number ("Profile 2").
    pub profiles: Vec<String>,
    /// Whether the headset's system lets it record its microphone, where known.
    pub mic_allowed: Option<bool>,
    /// Its microphone switch is on.
    pub mic_on: bool,
    /// The main session now: its arrangement is the desktop's, commands go to it.
    pub main: bool,
    /// It takes commands at all.
    pub commands: bool,
    /// Its "No limits", where it takes commands.
    pub no_limits: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayKind {
    /// A monitor made for the headset.
    #[default]
    Virtual,
    /// A display of the PC itself.
    Physical,
    /// A small output for one window (a floating panel in the headset).
    Window,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    #[default]
    Starting,
    Running,
    /// Could not start; see `failure`. It keeps its place and is tried again.
    Failed,
    /// A display of the PC that is not connected now (the lid closed, the cable out).
    Absent,
    /// A monitor of the set kept for the next headset, while there are no monitors on
    /// the desktop (no headset, or it was taken off).
    Kept,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    /// Percent: 100, 125, 150.
    pub scale: u16,
}

/// Where a display is on the PC's desktop, in logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeskRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub id: u32,
    pub kind: DisplayKind,
    /// `Prostir-2`, or a display of the PC's label.
    pub name: String,
    /// The desktop's name for the output (`eDP-1`), while it has one.
    pub output: String,
    /// A display of the PC: the laptop's own panel.
    pub builtin: bool,
    /// A display of the PC: shown in the headset.
    pub shown: bool,
    pub mode: Mode,
    pub desk: Option<DeskRect>,
    /// A monitor just added that the headset is placing: no `desk` until it stands
    /// where the headset put it. Left out when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub placing: bool,
    /// A monitor in Auto: the main headset chooses its scale. Left out when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto_scale: bool,
    pub state: DisplayState,
    pub failure: Option<Problem>,
}

/// What the PC allows: for the main headset while one is connected, else what any
/// headset gets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Sizes to offer for a new monitor, `[w, h]`.
    pub sizes: Vec<[u32; 2]>,
    /// Scales to offer, percent.
    pub scales: Vec<u16>,
    /// How many virtual monitors at once.
    pub displays_max: Option<u32>,
    /// How many displays of the PC shown at once.
    pub physical_max: Option<u32>,
    /// How many streams of every kind at once.
    pub streams_max: Option<u32>,
    /// The smallest and the largest monitor, `[w, h]`.
    pub min: Option<[u32; 2]>,
    pub max: Option<[u32; 2]>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    #[default]
    H264,
    H265,
    Av1,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Encoding {
    /// Mbit/s for each monitor.
    pub bitrate: u32,
    /// Mbit/s for each monitor of 2560x1440 or more now, where another: over USB while
    /// the quality is not chosen.
    pub large: Option<u32>,
    pub sharp: bool,
    /// Clearer text is not chosen: on for every monitor, and off for one whose encoder
    /// keeps dropping frames with it (`slowed` of its output).
    pub sharp_by_size: bool,
    /// As chosen.
    pub codec: Codec,
    /// On the CPU, as chosen.
    pub cpu: bool,
    /// What it encodes with now, while there are monitors.
    pub using: Option<Using>,
    /// How each display that streams now encodes, in the order of `displays`.
    pub outputs: Vec<OutputEncoding>,
}

/// How one display that streams encodes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputEncoding {
    pub display: u32,
    /// The encoder, for people: `VA-API`, `NVENC`, `x264`.
    pub encoder: String,
    pub codec: Codec,
    /// On the graphics card.
    pub hardware: bool,
    /// Its frames go through the CPU on their way to the encoder.
    pub copies: bool,
    /// Why it is not on the best encoder this PC has for it, in the PC's language;
    /// `null` when it is.
    pub note: Option<String>,
    /// Its encoder kept dropping frames with clearer text and went to the faster
    /// setting, until clearer text is chosen again.
    pub slowed: bool,
    /// What its encoder made over the last window of the PC's report (about 10 s,
    /// while a headset is connected); `null` before one ended.
    pub stats: Option<StreamStats>,
}

/// What one display's encoder made over a window, whoever got it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamStats {
    /// How long the window was, in milliseconds.
    pub window_ms: u32,
    /// Frames encoded, and of them those that only repeat the picture before (a
    /// still screen's keepalive).
    pub frames: u32,
    pub repeats: u32,
    pub bytes: u64,
    pub keyframes: u32,
    /// The largest keyframe, in bytes.
    pub keyframe_max: u32,
    /// From a frame entering the encoder's converter to leaving its parser,
    /// microseconds.
    pub encode_us_avg: u32,
    pub encode_us_max: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Using {
    pub codec: Codec,
    /// On the graphics card.
    pub hardware: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MicState {
    #[default]
    Off,
    Ready,
    Failed,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sound {
    /// The PC's sound plays in the headset.
    pub to_headset: bool,
    /// The main headset's volume, while the PC's volume device follows it.
    pub volume: Option<Volume>,
    /// The microphone switch of the headset whose microphone the PC takes is on.
    pub mic_on: bool,
    /// The PC's end of it (the virtual microphone programs record from).
    pub mic: MicState,
    pub mic_failure: Option<Problem>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Volume {
    pub level: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pairing {
    /// Seconds until the pairing window closes, when this state was made (from step
    /// 6.8; 0 until then).
    pub seconds_left: u32,
    /// How long it was opened for; 0: no time limit.
    pub seconds_total: u32,
    /// A code made here, to type in the headset, while the PC shows one.
    pub pc_code: Option<String>,
    /// A headset showing a code and waiting for it to be typed here, by name.
    pub code_for: Option<String>,
    /// How the last attempt ended, if one did.
    pub last: Option<PairResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairResult {
    /// A new headset is trusted now.
    Paired {
        name: String,
    },
    /// The codes did not match.
    Wrong,
    Other,
}

/// Lenient: an ending this build does not know, with data or without, reads as
/// `Other`; `paired` without a name reads with an empty one.
impl<'de> Deserialize<'de> for PairResult {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        if value.as_str() == Some("wrong") {
            return Ok(Self::Wrong);
        }
        let name = value.get("paired").map(|p| {
            p.get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        });
        Ok(match name {
            Some(name) => Self::Paired { name: name.into() },
            None => Self::Other,
        })
    }
}

/// Something wrong: its name and its text in the PC's language.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Problem {
    pub code: ApiError,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_request() -> Vec<Request> {
        vec![
            Request::Recenter,
            Request::LineUp,
            Request::ShowMouse,
            Request::ResetArrangement,
            Request::Profile {
                name: "Робота".into(),
            },
            Request::RenameProfile {
                profile: "2".into(),
                name: "Ігри".into(),
            },
            Request::Cinema { on: true },
            Request::Focus { on: false },
            Request::Follow { on: true },
            Request::NoLimits { on: true },
            Request::Lock { on: true },
            Request::Preset { preset: 3 },
            Request::AddMonitor {
                w: 2560,
                h: 1440,
                scale: 125,
            },
            Request::RemoveMonitor { id: 103 },
            Request::RetryMonitor { id: 103 },
            Request::ShowPhysical {
                id: 104,
                shown: true,
            },
            Request::SetScale {
                id: 103,
                scale: 150,
            },
            Request::SetScaleAuto { id: 103, on: true },
            Request::SetPrimary { id: 103 },
            Request::Place {
                id: 103,
                x: -1920,
                y: 0,
            },
            Request::Screenshot { id: 103 },
            Request::SetQuality { mbit: 30 },
            Request::SetSharp { on: true },
            Request::SetCodec {
                codec: CodecChoice::H265,
                cpu: false,
            },
            Request::SoundToHeadset { on: false },
            Request::Volume { level: 40 },
            Request::SetMic { on: true },
            Request::SetCounting {
                counting: Counting::No,
            },
            Request::Autostart { on: true },
            Request::WindowLook {
                theme: ThemeMode::Dark,
                palette: "graphite".into(),
                language: Some("uk".into()),
            },
            Request::PairNewHeadset,
            Request::StopPairing,
            Request::PairingCode {
                code: "123456".into(),
            },
            Request::ForgetHeadset {
                headset: "a1b2c3d4e5f6".into(),
            },
            Request::ForgetAll,
            Request::ShowSettings,
            Request::Quit,
        ]
    }

    /// Every request reads back as itself, under the name [`REQUESTS`] gives it.
    #[test]
    fn requests_round_trip() {
        let all = every_request();
        assert_eq!(all.len(), REQUESTS.len());
        for (request, name) in all.iter().zip(REQUESTS) {
            let json = request.to_json();
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["request"], *name, "{json}");
            assert_eq!(&Request::from_json(&json).unwrap(), request, "{json}");
        }
        assert_eq!(
            Request::AddMonitor {
                w: 1920,
                h: 1080,
                scale: 125
            }
            .to_json(),
            r#"{"request":"add_monitor","w":1920,"h":1080,"scale":125}"#
        );
    }

    /// The JSON as clients write it: fields with a default may be left out, unknown
    /// fields are let be, and what is wrong says why.
    #[test]
    fn requests_as_written() {
        let read = Request::from_json;
        assert_eq!(
            read(r#"{"request":"add_monitor","w":1920,"h":1080}"#).unwrap(),
            Request::AddMonitor {
                w: 1920,
                h: 1080,
                scale: 100
            }
        );
        assert_eq!(
            read(r#"{"request":"set_codec","codec":"auto","later":1}"#).unwrap(),
            Request::SetCodec {
                codec: CodecChoice::Auto,
                cpu: false
            }
        );
        let error = |s: &str| read(s).unwrap_err().error;
        assert_eq!(error(r#"{"request":"fly"}"#), ApiError::Unsupported);
        assert_eq!(error(r#"{"request":"cinema"}"#), ApiError::BadArgument);
        assert_eq!(
            error(r#"{"request":"preset","preset":300}"#),
            ApiError::BadArgument
        );
        assert_eq!(error("recenter"), ApiError::BadArgument);
        assert_eq!(error("{}"), ApiError::BadArgument);
        // The answer about counting is one of three, and never left out.
        assert_eq!(
            read(r#"{"request":"set_counting","counting":"yes"}"#).unwrap(),
            Request::SetCounting {
                counting: Counting::Yes
            }
        );
        assert_eq!(
            error(r#"{"request":"set_counting","counting":"maybe"}"#),
            ApiError::BadArgument
        );
        assert_eq!(
            error(r#"{"request":"set_counting"}"#),
            ApiError::BadArgument
        );
        assert_eq!(
            error(r#"{"request":"set_counting","on":true}"#),
            ApiError::BadArgument
        );
        assert!(Request::Recenter.for_headset());
        assert!(!Request::Quit.for_headset());
    }

    /// Answers and events as clients read them.
    #[test]
    fn answers_and_events_on_the_wire() {
        let done = Answer {
            revision: 3,
            ..Answer::default()
        };
        assert_eq!(done.to_json(), r#"{"revision":3}"#);
        let added = Answer {
            id: Some(105),
            revision: 4,
            ..Answer::default()
        };
        assert_eq!(added.to_json(), r#"{"id":105,"revision":4}"#);
        let saved = Answer {
            path: Some("/home/me/Pictures/Prostir/1.png".into()),
            whole: Some(false),
            revision: 5,
            ..Answer::default()
        };
        assert_eq!(
            saved.to_json(),
            r#"{"path":"/home/me/Pictures/Prostir/1.png","whole":false,"revision":5}"#
        );
        let failed = Answer {
            error: Some(ApiError::NotConnected),
            text: Some("No headset".into()),
            revision: 4,
            ..Answer::default()
        };
        let json = failed.to_json();
        assert_eq!(
            json,
            r#"{"error":"NotConnected","text":"No headset","revision":4}"#
        );
        let back: Answer = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.failure(),
            Some(Failure::new(ApiError::NotConnected, "No headset"))
        );
        let newer: Answer = serde_json::from_str(r#"{"error":"Later","more":[1]}"#).unwrap();
        assert_eq!(newer.error, Some(ApiError::Other));
        let gone = Event::HeadsetDisconnected {
            headset: "a1b2c3d4e5f6".into(),
            reason: Reason::TakenOff,
        };
        assert_eq!(
            serde_json::to_string(&gone).unwrap(),
            r#"{"event":"headset_disconnected","headset":"a1b2c3d4e5f6","reason":"taken-off"}"#
        );
        for e in ApiError::ALL {
            assert_eq!(serde_json::to_value(e).unwrap(), e.name());
            assert_eq!(ApiError::from_name(e.name()), e);
        }
        assert_eq!(Reason::from_name("taken-off"), Reason::TakenOff);
    }

    fn sample() -> State {
        State {
            v: STATE_VERSION,
            pc: Pc {
                name: "desk".into(),
                app: "0.2.0".into(),
                desktop: "KDE Plasma".into(),
                phase: Phase::Live,
                addresses: vec!["192.168.1.10".into()],
            },
            window: WindowLook::default(),
            autostart: Some(true),
            headsets: vec![Headset {
                key: "a1b2c3d4e5f6".into(),
                name: "Quest 3".into(),
                model: "Quest 3".into(),
                app: "0.2.0".into(),
                last_seen: Some(1_795_000_000),
                role: Role::Main,
                rights: vec!["input".into(), "mic".into()],
                session: Some(Session {
                    via: Via::Wifi,
                    worn: true,
                    battery: Some(Battery {
                        percent: 80,
                        charging: false,
                    }),
                    profile: Some("Робота".into()),
                    profiles: vec![String::new(), "Робота".into()],
                    mic_allowed: Some(true),
                    mic_on: false,
                    main: true,
                    commands: true,
                    no_limits: Some(false),
                }),
            }],
            connecting: None,
            displays: vec![Display {
                id: 103,
                kind: DisplayKind::Virtual,
                name: "Prostir-1".into(),
                output: "Virtual-Prostir-1".into(),
                builtin: false,
                shown: true,
                mode: Mode {
                    w: 2560,
                    h: 1440,
                    scale: 125,
                },
                desk: Some(DeskRect {
                    x: 0,
                    y: 0,
                    w: 2048,
                    h: 1152,
                }),
                placing: false,
                auto_scale: true,
                state: DisplayState::Failed,
                failure: Some(Problem {
                    code: ApiError::Desktop,
                    text: "KScreen".into(),
                }),
            }],
            primary: Some(103),
            limits: Limits {
                sizes: vec![[1920, 1080]],
                scales: vec![100, 125],
                displays_max: Some(6),
                physical_max: Some(2),
                streams_max: Some(8),
                min: Some([640, 360]),
                max: Some([3840, 2160]),
            },
            encoding: Encoding {
                bitrate: 30,
                large: Some(60),
                sharp: false,
                sharp_by_size: false,
                codec: Codec::H265,
                cpu: false,
                using: Some(Using {
                    codec: Codec::H264,
                    hardware: true,
                }),
                outputs: vec![OutputEncoding {
                    display: 103,
                    encoder: "x264".into(),
                    codec: Codec::H264,
                    hardware: false,
                    copies: true,
                    note: Some("y".into()),
                    slowed: true,
                    stats: Some(StreamStats {
                        window_ms: 10_000,
                        frames: 600,
                        repeats: 5,
                        bytes: 30_000_000,
                        keyframes: 1,
                        keyframe_max: 750_000,
                        encode_us_avg: 4300,
                        encode_us_max: 9100,
                    }),
                }],
            },
            sound: Sound {
                to_headset: true,
                volume: Some(Volume {
                    level: 40,
                    muted: false,
                }),
                mic_on: false,
                mic: MicState::Off,
                mic_failure: None,
            },
            pairing: None,
            nearby: Vec::new(),
            counting: Some(Counting::Unasked),
            problems: vec![Problem {
                code: ApiError::Sound,
                text: "x".into(),
            }],
            wifi: Some(Wifi {
                interface: "wlan0".into(),
                power_save: Some(true),
                freq_mhz: 5180,
                width_mhz: 80,
            }),
        }
    }

    /// The state as clients read it: a change of the format shows here in review.
    #[test]
    fn state_on_the_wire() {
        let state = sample();
        let json = state.to_json();
        let expected = concat!(
            r#"{"v":1,"pc":{"name":"desk","app":"0.2.0","desktop":"KDE Plasma","phase":"live","addresses":["192.168.1.10"]},"#,
            r#""window":{"theme":"system","palette":"ink","language":null},"autostart":true,"#,
            r#""headsets":[{"key":"a1b2c3d4e5f6","name":"Quest 3","model":"Quest 3","#,
            r#""app":"0.2.0","last_seen":1795000000,"role":"main","rights":["input","mic"],"#,
            r#""session":{"via":"wifi","worn":true,"battery":{"percent":80,"charging":false},"#,
            r#""profile":"Робота","profiles":["","Робота"],"mic_allowed":true,"mic_on":false,"#,
            r#""main":true,"commands":true,"#,
            r#""no_limits":false}}],"connecting":null,"#,
            r#""displays":[{"id":103,"kind":"virtual","name":"Prostir-1","#,
            r#""output":"Virtual-Prostir-1","builtin":false,"shown":true,"#,
            r#""mode":{"w":2560,"h":1440,"scale":125},"desk":{"x":0,"y":0,"w":2048,"h":1152},"#,
            r#""auto_scale":true,"state":"failed","failure":{"code":"Desktop","text":"KScreen"}}],"primary":103,"#,
            r#""limits":{"sizes":[[1920,1080]],"scales":[100,125],"displays_max":6,"#,
            r#""physical_max":2,"streams_max":8,"min":[640,360],"max":[3840,2160]},"#,
            r#""encoding":{"bitrate":30,"large":60,"sharp":false,"sharp_by_size":false,"#,
            r#""codec":"h265","#,
            r#""cpu":false,"#,
            r#""using":{"codec":"h264","hardware":true},"outputs":[{"display":103,"#,
            r#""encoder":"x264","codec":"h264","hardware":false,"copies":true,"note":"y","slowed":true,"#,
            r#""stats":{"window_ms":10000,"frames":600,"repeats":5,"bytes":30000000,"#,
            r#""keyframes":1,"keyframe_max":750000,"encode_us_avg":4300,"#,
            r#""encode_us_max":9100}}]},"#,
            r#""sound":{"to_headset":true,"volume":{"level":40,"muted":false},"mic_on":false,"#,
            r#""mic":"off","mic_failure":null},"pairing":null,"nearby":[],"counting":"unasked","#,
            r#""problems":[{"code":"Sound","text":"x"}],"#,
            r#""wifi":{"interface":"wlan0","power_save":true,"freq_mhz":5180,"width_mhz":80}}"#,
        );
        assert_eq!(json, expected);
        assert_eq!(State::from_json(&json).unwrap(), state);
    }

    /// A newer service's state still reads: unknown fields and values, missing ones.
    #[test]
    fn a_state_reads_leniently() {
        let s = State::from_json(
            r#"{"v":2,"future":{"x":1},"headsets":[{"name":"Quest 3",
                "session":{"via":"bluetooth","worn":true}}],
                "displays":[{"id":3,"kind":"hologram","state":"failed",
                "failure":{"code":"Later","text":"No."}}],
                "pairing":{"seconds_left":100,"last":{"paired":{"name":"Quest 3S"}}}}"#,
        )
        .unwrap();
        let (h, session) = s.connected().unwrap();
        assert_eq!(h.name, "Quest 3");
        assert_eq!(session.via, Via::Other);
        assert_eq!(s.displays[0].kind, DisplayKind::Other);
        assert_eq!(s.displays[0].state, DisplayState::Failed);
        assert_eq!(
            s.displays[0].failure.as_ref().unwrap().code,
            ApiError::Other
        );
        assert_eq!(s.window, WindowLook::default());
        // Without the answer about counting: nothing to ask.
        assert_eq!(s.counting, None);
        assert_eq!(
            s.pairing.unwrap().last,
            Some(PairResult::Paired {
                name: "Quest 3S".into()
            })
        );
        assert!(State::from_json("[1").is_err());
    }

    /// Every closed set reads what a newer side may send: an unknown name, a name
    /// with data, a value that is no name.
    #[test]
    fn closed_sets_read_leniently() {
        let read = |json: &str| serde_json::from_str::<PairResult>(json).unwrap();
        assert_eq!(read(r#""wrong""#), PairResult::Wrong);
        assert_eq!(
            read(r#"{"paired":{"name":"Quest 3S"}}"#),
            PairResult::Paired {
                name: "Quest 3S".into()
            }
        );
        assert_eq!(read(r#"{"locked":{"until":5}}"#), PairResult::Other);
        assert_eq!(read(r#""later""#), PairResult::Other);
        assert_eq!(read("7"), PairResult::Other);
        let kind = |json: &str| serde_json::from_str::<DisplayKind>(json).unwrap();
        assert_eq!(kind(r#""physical""#), DisplayKind::Physical);
        assert_eq!(kind(r#"{"hologram":{"depth":2}}"#), DisplayKind::Other);
        assert_eq!(
            serde_json::from_str::<ApiError>(r#"{"Later":1}"#).unwrap(),
            ApiError::Other
        );
        assert_eq!(
            serde_json::from_str::<Reason>(r#""taken-off""#).unwrap(),
            Reason::TakenOff
        );
        assert_eq!(serde_json::from_str::<Via>("null").unwrap(), Via::Other);
        assert_eq!(
            serde_json::from_str::<Counting>(r#""later""#).unwrap(),
            Counting::Other
        );
        let event: Event =
            serde_json::from_str(r#"{"event":"headset_charging","headset":"a1"}"#).unwrap();
        assert_eq!(event, Event::Other);
        // Every name reads back as itself.
        for e in ApiError::ALL {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<ApiError>(&json).unwrap(), e);
        }
    }
}
