//! The state each side keeps and tells the other: the PC's displays, encoding, sound,
//! limits and rights, and the headset's capabilities, preferences, presence, battery,
//! volume and microphone. Each piece is a *part*: a message of its own when it
//! changes, and the same bytes inside `Snapshot` (from the PC) or `Open` (from the
//! headset) at the start of a session, so the state at the start and the state after
//! a change are read by the same code.
//!
//! The types here are what the parts are made of; [`crate::HostMsg`] and
//! [`crate::ClientMsg`] carry them.
//!
//! # Limits of lists and texts
//!
//! A list or text over its limit is refused by every reader, so a limit cannot grow
//! without a new protocol version. What grows from version to version (parts,
//! features, codecs, sizes) has limits up to what a `u16` count holds; the size of a
//! message bounds the rest.

use crate::Codec;
use crate::wire::{Reader, Wire, WireError, wire_enum, wire_struct};

/// Most displays in [`Displays`] and in an arrangement: with "No limits" 10 virtual
/// monitors, 4 displays of the PC and 3 window panels make 17.
pub const MAX_DISPLAYS: usize = 32;
/// Longest name of a display, in bytes.
pub const MAX_DISPLAY_NAME: usize = 64;
/// Longest title of a window on a panel, in bytes.
pub const MAX_TITLE: usize = 128;
/// Longest connector name of a display of the PC (`eDP-1`), in bytes.
pub const MAX_CONNECTOR: usize = 32;
/// Longest technical detail of an error, in bytes.
pub const MAX_DETAIL: usize = 512;
/// Most entries of a list that grows from version to version (features, parts).
pub const MAX_GROWING: usize = 65_535;
/// Most codecs a list may name ([`Encoding::choices`], [`HeadsetCaps::decoders`]).
pub const MAX_CODECS: usize = 64;
/// Most sizes and scales the PC offers ([`Limits`]).
pub const MAX_SIZES: usize = 64;
pub const MAX_SCALES: usize = 32;
/// Longest short name (an app version, a model, a desktop), in bytes.
pub const MAX_SHORT: usize = 32;
/// Longest language code, in bytes.
pub const MAX_LANGUAGE: usize = 8;
/// Most profiles the headset lists ([`Profiles`]).
pub const MAX_PROFILES: usize = 32;

/// The text `text` cut to at most `max` bytes, on a character boundary: what the PC
/// puts into its state (window titles, error details, names), so a long text never
/// makes a message impossible to send.
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A `u32`, `u16` or `u8` with a meaning, written as the number it wraps.
macro_rules! number {
    ($(#[$meta:meta])* $name:ident($inner:ty)) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub $inner);

        impl Wire for $name {
            fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
                self.0.put(out)
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
                Ok(Self(<$inner>::get(r)?))
            }
        }
    };
}

number! {
    /// A display of the PC: a virtual monitor, a window panel or a display of the PC
    /// itself. The PC gives numbers out from a counter that never goes back, so a
    /// number is never another display's, in this run or a later one.
    DisplayId(u32)
}

impl std::fmt::Display for DisplayId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "display {}", self.0)
    }
}

number! {
    /// A generation of a stream: one counter for every stream of a run of the PC app,
    /// from 1, a new one at every `StreamStart`. A frame, its acknowledgement and a
    /// request for a keyframe name the generation they belong to, so nothing of an
    /// older start of a stream is taken for the new one.
    Gen(u32)
}

number! {
    /// A request of the headset or a command of the PC: each side counts its own from
    /// 1 in every session, and each gets exactly one reply with its number.
    ReqId(u32)
}

number! {
    /// A capability (see [`caps`]): numbers a reader does not know are ignored.
    Feature(u16)
}

/// Why something failed. A code a reader does not know reads as [`ErrorCode::OTHER`],
/// so a new code needs no new protocol version. The headset shows its own text for a
/// code it knows, in its language, and the `detail` that comes with it (technical, in
/// the PC's language) under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErrorCode(u16);

impl ErrorCode {
    /// Something else: only the detail says what.
    pub const OTHER: Self = Self(0);
    /// A request or command this side does not know or cannot do.
    pub const UNSUPPORTED: Self = Self(1);
    /// This headset has no right to it.
    pub const NOT_ALLOWED: Self = Self(2);
    /// No such display (any more).
    pub const UNKNOWN_DISPLAY: Self = Self(3);
    /// As many as the PC allows.
    pub const LIMIT: Self = Self(4);
    /// A size the PC does not make.
    pub const BAD_SIZE: Self = Self(5);
    /// The last monitor cannot be removed.
    pub const LAST_DISPLAY: Self = Self(6);
    /// No active window on the PC.
    pub const NO_WINDOW: Self = Self(7);
    /// The desktop did not do it (an output not made, KScreen stuck).
    pub const DESKTOP: Self = Self(8);
    /// The encoder could not start.
    pub const ENCODER: Self = Self(9);
    /// The picture of a display could not be taken.
    pub const CAPTURE: Self = Self(10);
    /// Sound could not be routed.
    pub const SOUND: Self = Self(11);
    /// The mouse is on none of the displays.
    pub const NOT_ON_DISPLAYS: Self = Self(12);
    /// The headset has no profile of that name.
    pub const NO_SUCH_PROFILE: Self = Self(13);
    /// An argument out of range.
    pub const BAD_ARGUMENT: Self = Self(14);
    /// The highest code this build knows.
    const LAST: u16 = 14;

    /// The code `n`, or [`ErrorCode::OTHER`] for one this build does not know.
    pub fn from_code(n: u16) -> Self {
        if n <= Self::LAST {
            Self(n)
        } else {
            Self::OTHER
        }
    }

    pub fn code(self) -> u16 {
        self.0
    }
}

impl ErrorCode {
    /// The code as this build knows it: an unknown one reads as [`ErrorCode::OTHER`].
    /// The number itself is kept as it came, for the log.
    pub fn known(self) -> Self {
        Self::from_code(self.0)
    }
}

impl Wire for ErrorCode {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        self.0.put(out)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(u16::get(r)?))
    }
}

/// The capabilities each side names in its caps part ([`HostCaps`], [`HeadsetCaps`]).
/// A value of an enum goes on the wire only when the other side named the capability
/// it needs (see `PROTOCOL.md`, "Capabilities").
pub mod caps {
    /// What the PC can do.
    pub mod pc {
        use crate::state::Feature;
        /// Move the mouse (to where the user looks).
        pub const MOVE_MOUSE: Feature = Feature(1);
        /// A screenshot of one display (not only the whole desktop).
        pub const SCREENSHOT_ONE: Feature = Feature(2);
        /// Window panels.
        pub const WINDOWS: Feature = Feature(3);
        /// Displays of the PC itself.
        pub const PHYSICAL: Feature = Feature(4);
        /// One volume for the PC's slider and the headset's buttons.
        pub const VOLUME: Feature = Feature(5);
        /// Desktop notifications.
        pub const NOTIFICATIONS: Feature = Feature(6);
        /// Which display the sound comes from.
        pub const SOUND_FROM: Feature = Feature(7);
        /// Commands from the PC.
        pub const COMMANDS: Feature = Feature(8);
        /// "No limits".
        pub const NO_LIMITS: Feature = Feature(9);
        /// Monitors with a scale ("larger text").
        pub const SCALE: Feature = Feature(10);
        /// The headset microphone as an up stream.
        pub const MIC: Feature = Feature(11);
        pub const CAMERA: Feature = Feature(12);
        pub const POSES: Feature = Feature(13);
        /// Uses the gaze for which display gets the most bits.
        pub const GAZE: Feature = Feature(14);
    }

    /// What the headset can do.
    pub mod headset {
        use crate::state::Feature;
        /// Takes commands from the PC.
        pub const COMMANDS: Feature = Feature(1);
        /// One volume for the PC's slider and the headset's buttons.
        pub const VOLUME: Feature = Feature(2);
        /// Sends its microphone as an up stream.
        pub const MIC: Feature = Feature(3);
        pub const CAMERA: Feature = Feature(4);
        pub const POSES: Feature = Feature(5);
        /// Says where the user looks.
        pub const GAZE: Feature = Feature(6);
        /// Shows displays of the PC itself.
        pub const PHYSICAL: Feature = Feature(7);
        /// Shows window panels.
        pub const WINDOWS: Feature = Feature(8);
        /// Its compositor shows a stream's rows bottom first: the PC sends them so, and
        /// the picture shows the right way up.
        pub const ROWS_UP: Feature = Feature(9);
    }
}

wire_struct! {
    /// A size in pixels and a scale in percent (100: one pixel of the picture is one
    /// pixel of the desktop; 150: "larger text").
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Mode {
        pub w: u32,
        pub h: u32,
        pub scale: u16,
    }
}

impl Mode {
    /// `w`x`h` at 100 %.
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, scale: 100 }
    }

    fn check(&self) -> Result<(), WireError> {
        if self.scale == 0 {
            return Err(WireError::Invalid("a scale of 0 %"));
        }
        Ok(())
    }
}

wire_struct! {
    /// Where a display is on the PC's desktop, in logical pixels.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DeskRect {
        pub x: i32,
        pub y: i32,
        pub w: u32,
        pub h: u32,
    }
}

wire_enum! {
    /// What a display is.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DisplayKind {
        /// A virtual monitor made for the headset.
        0 => Virtual,
        /// A small output made for one window, shown as a floating panel. The title
        /// is empty for a headset without the right to private things.
        1 => Window {
            #[wire(max = MAX_TITLE)]
            title: String,
        },
        /// A display of the PC itself (`builtin`: the laptop's own panel), streamed as
        /// it is while `shown`.
        2 => Physical {
            builtin: bool,
            #[wire(max = MAX_CONNECTOR)]
            connector: String,
            shown: bool,
        },
    }
}

wire_enum! {
    /// How a display is doing.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DisplayState {
        /// Being made, or waiting to stream.
        0 => Starting,
        1 => Running,
        /// It could not start; it keeps its place and is tried again.
        2 => Failed {
            error: ErrorCode,
            #[wire(max = MAX_DETAIL)]
            detail: String,
        },
        /// A display of the PC that is known but not connected now (the lid closed,
        /// the cable out). It keeps its number and its place.
        3 => Absent,
    }
}

wire_struct! {
    /// One display of the PC as the headset is told about it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Display {
        pub id: DisplayId,
        pub kind: DisplayKind,
        #[wire(max = MAX_DISPLAY_NAME)]
        pub name: String,
        pub mode: Mode,
        /// Where it is on the PC's desktop, as laid out; none while it is nowhere.
        pub desk: Option<DeskRect>,
        pub state: DisplayState,
    }
}

impl Display {
    pub fn is_virtual(&self) -> bool {
        matches!(self.kind, DisplayKind::Virtual)
    }

    pub fn is_window(&self) -> bool {
        matches!(self.kind, DisplayKind::Window { .. })
    }

    pub fn is_physical(&self) -> bool {
        matches!(self.kind, DisplayKind::Physical { .. })
    }

    /// A display of the PC that is shown in the headset.
    pub fn is_shown_physical(&self) -> bool {
        matches!(self.kind, DisplayKind::Physical { shown: true, .. })
    }
}

wire_struct! {
    /// The PC's displays (a part): every display there is, in the order of the rows
    /// the headset arranged (top to bottom, left to right), then those the rows leave
    /// out. The one source of which displays exist: a display leaves the headset only
    /// when it leaves this list.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Displays {
        #[wire(max = MAX_DISPLAYS)]
        pub displays: Vec<Display>,
        /// The desktop's primary display (where the panel lives), if one of these.
        pub primary: Option<DisplayId>,
    }
}

impl Displays {
    pub fn get(&self, id: DisplayId) -> Option<&Display> {
        self.displays.iter().find(|d| d.id == id)
    }

    pub fn ids(&self) -> impl Iterator<Item = DisplayId> + '_ {
        self.displays.iter().map(|d| d.id)
    }

    /// Whether `id` is one of the displays, or none.
    pub fn knows(&self, id: Option<DisplayId>) -> bool {
        id.is_none_or(|id| self.get(id).is_some())
    }

    /// Each display once, each mode with a scale, the primary among them.
    pub fn check(&self) -> Result<(), WireError> {
        for (i, d) in self.displays.iter().enumerate() {
            if self.displays[..i].iter().any(|o| o.id == d.id) {
                return Err(WireError::Repeated(d.id.0));
            }
            d.mode.check()?;
        }
        if !self.knows(self.primary) {
            return Err(WireError::Invalid("a primary display that is not listed"));
        }
        Ok(())
    }
}

wire_struct! {
    /// How the person wants the PC to encode.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Wanted {
        /// Mbit/s for each monitor.
        pub bitrate: u16,
        /// None: one this headset does not decode (an AV1 chosen with another headset).
        pub codec: Option<Codec>,
        /// On the CPU.
        pub software: bool,
        /// The slower preset that keeps small text clearer.
        pub sharp: bool,
    }
}

wire_struct! {
    /// How the PC encodes (a part): what is wanted, what it encodes with now
    /// (`bool`: on the graphics card), and what it could encode with.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Encoding {
        pub wanted: Wanted,
        pub using: Option<(Codec, bool)>,
        #[wire(max = MAX_CODECS)]
        pub choices: Vec<(Codec, bool)>,
    }
}

wire_enum! {
    /// The PC's end of the headset microphone (the virtual microphone programs record
    /// from).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MicDevice {
        0 => Off,
        1 => Ready,
        /// It could not be made: a state that lasts, not a passing problem.
        2 => Failed {
            error: ErrorCode,
            #[wire(max = MAX_DETAIL)]
            detail: String,
        },
    }
}

wire_struct! {
    /// Sound (a part): whether the PC's sound plays in the headset, and its end of the
    /// microphone.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Sound {
        pub to_headset: bool,
        pub mic: MicDevice,
    }
}

wire_enum! {
    /// What a stream from the headset to the PC carries.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum UpKind {
        0 => Mic,
        1 => Camera,
        /// The poses of the head and hands.
        2 => Poses,
    }
}

wire_enum! {
    /// The format of a stream from the headset.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum UpFormat {
        /// Signed 16-bit little-endian samples, interleaved.
        0 => Pcm16 { rate: u32, channels: u8 },
        1 => Video { codec: Codec, w: u32, h: u32 },
        2 => Poses { hz: u8 },
    }
}

impl UpFormat {
    /// The microphone at 48 kHz, one or two channels: what the PC takes.
    pub fn check(&self) -> Result<(), WireError> {
        match *self {
            Self::Pcm16 { rate, channels }
                if rate != crate::AUDIO_RATE || !(1..=2).contains(&channels) =>
            {
                Err(WireError::Invalid(
                    "sound other than 48 kHz, one or two channels",
                ))
            }
            Self::Video { w, h, .. } if w == 0 || h == 0 => {
                Err(WireError::Invalid("a video of no size"))
            }
            _ => Ok(()),
        }
    }
}

wire_struct! {
    /// A program on the PC started or stopped recording from the headset's microphone
    /// (camera later): a part for each kind.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UpWanted {
        pub kind: UpKind,
        pub on: bool,
    }
}

wire_struct! {
    /// What the PC allows (a part): how many displays of each kind, how many streams,
    /// the sizes it makes and the sizes and scales it offers.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Limits {
        pub virtual_max: u8,
        pub windows_max: u8,
        pub physical_max: u8,
        /// Streams at once, of every kind: at most 30 (a lane each).
        pub streams_max: u8,
        /// The smallest and the largest monitor, in pixels.
        pub min: (u32, u32),
        pub max: (u32, u32),
        #[wire(max = MAX_SIZES)]
        pub sizes: Vec<Mode>,
        /// Percent.
        #[wire(max = MAX_SCALES)]
        pub scales: Vec<u16>,
    }
}

impl Limits {
    pub fn check(&self) -> Result<(), WireError> {
        self.sizes.iter().try_for_each(Mode::check)?;
        if self.scales.contains(&0) {
            return Err(WireError::Invalid("a scale of 0 %"));
        }
        Ok(())
    }
}

/// The rights a headset may have ([`Access::perms`]).
pub mod perms {
    /// The mouse and keyboard (the mouse following the gaze, windows moved to a monitor).
    pub const INPUT: u32 = 1;
    pub const MIC: u32 = 2;
    pub const CAMERA: u32 = 4;
    pub const CLIPBOARD: u32 = 8;
    /// Changes: monitors, the PC's settings.
    pub const CHANGES: u32 = 16;
    /// Private things: notifications, window titles, where screenshots go.
    pub const PRIVATE: u32 = 32;
    /// Every right this build knows.
    pub const ALL: u32 = INPUT | MIC | CAMERA | CLIPBOARD | CHANGES | PRIVATE;
}

/// The role of a headset ([`Access::role`]): the main one arranges the desktop;
/// any other value reads as a guest.
pub mod role {
    pub const MAIN: u8 = 0;
    pub const GUEST: u8 = 1;
}

wire_struct! {
    /// This headset's role and rights on the PC (a part).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Access {
        pub role: u8,
        /// Bits of [`perms`]; unknown bits are ignored.
        pub perms: u32,
    }
}

impl Access {
    /// The main headset with every right: one headset, as before rights existed.
    pub const MAIN: Self = Self {
        role: role::MAIN,
        perms: perms::ALL,
    };

    pub fn is_main(&self) -> bool {
        self.role == role::MAIN
    }

    pub fn may(&self, perm: u32) -> bool {
        self.perms & perm == perm
    }
}

/// Operating systems ([`PcInfo::os`]); any other value reads as another one.
pub mod os {
    pub const LINUX: u8 = 1;
    pub const WINDOWS: u8 = 2;
}

wire_struct! {
    /// About the PC (a part).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PcInfo {
        #[wire(max = MAX_DISPLAY_NAME)]
        pub name: String,
        /// The PC app's version.
        #[wire(max = MAX_SHORT)]
        pub app: String,
        /// Its interface language (`uk`, `en`).
        #[wire(max = MAX_LANGUAGE)]
        pub language: String,
        pub os: u8,
        /// The desktop (`KDE Plasma`, `Hyprland`).
        #[wire(max = MAX_SHORT)]
        pub desktop: String,
    }
}

wire_struct! {
    /// What the PC can do (a part, in `Snapshot` only).
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct HostCaps {
        #[wire(max = MAX_GROWING)]
        pub features: Vec<Feature>,
    }
}

impl HostCaps {
    pub fn has(&self, f: Feature) -> bool {
        self.features.contains(&f)
    }
}

wire_struct! {
    /// The display the mouse or the active window is on (a part); none: none of them.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Focus {
        pub display: Option<DisplayId>,
    }
}

wire_struct! {
    /// The display whose window plays the sound (a part, while the headset asks for
    /// it); none: not known.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct SoundFrom {
        pub display: Option<DisplayId>,
    }
}

wire_struct! {
    /// A video decoder of the headset: the codec (as [`Codec`]'s number, so one a PC
    /// does not know simply does not match) and the widest and the tallest picture it
    /// takes. Not every picture within both decodes in time: see [`PixelBudget`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Decoder {
        pub codec: u8,
        pub max_w: u32,
        pub max_h: u32,
    }
}

wire_struct! {
    /// How many pixels a picture may have for the headset's decoder of `codec` to
    /// decode 60 of them a second (a decoder's limit is pixels per frame and per
    /// second, not a box).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PixelBudget {
        pub codec: u8,
        pub max_pixels: u32,
    }
}

wire_struct! {
    /// The pixel budget of the headset's decoders (a part, in `Open` only, from 0.2.0
    /// B6; an older PC skips it and goes by the sides alone). It travels apart from
    /// [`HeadsetCaps`] so that part keeps its bytes; the PC reads it into
    /// [`HeadsetCaps::pixels`].
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct DecoderPixels {
        #[wire(max = MAX_CODECS)]
        pub budgets: Vec<PixelBudget>,
    }
}

impl DecoderPixels {
    pub fn check(&self) -> Result<(), WireError> {
        if self.budgets.iter().any(|b| b.max_pixels == 0) {
            return Err(WireError::Invalid("a pixel budget of 0"));
        }
        Ok(())
    }
}

wire_struct! {
    /// What the headset can do (a part, in `Open` only, and always there).
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct HeadsetCaps {
        #[wire(max = MAX_GROWING)]
        pub features: Vec<Feature>,
        #[wire(max = MAX_CODECS)]
        pub decoders: Vec<Decoder>,
        /// Streams it decodes at once.
        pub streams_max: u8,
        /// Compositor layers it can show at once.
        pub layers_max: u8,
        #[wire(max = MAX_SHORT)]
        pub model: String,
        /// The pixel budget of each decoder: not in this part's bytes but in
        /// [`DecoderPixels`], which [`HeadsetState`] reads into here. None for a codec:
        /// its decoder goes by its sides alone.
        #[wire(skip)]
        pub pixels: Vec<PixelBudget>,
    }
}

impl HeadsetCaps {
    pub fn has(&self, f: Feature) -> bool {
        self.features.contains(&f)
    }

    /// Whether it decodes `codec`.
    pub fn decodes(&self, codec: Codec) -> bool {
        self.decoder(codec).is_some()
    }

    /// Its decoder for `codec`.
    pub fn decoder(&self, codec: Codec) -> Option<&Decoder> {
        let n = crate::wire::encode(&codec).ok()?[0];
        self.decoders.iter().find(|d| d.codec == n)
    }

    /// The pixel budget of its decoder for `codec`, if it said one.
    pub fn max_pixels(&self, codec: Codec) -> Option<u32> {
        let n = crate::wire::encode(&codec).ok()?[0];
        self.pixels
            .iter()
            .find(|b| b.codec == n)
            .map(|b| b.max_pixels)
    }

    /// Whether it decodes a picture of `w`x`h` in `codec`: within the sides of its
    /// decoder for that codec and within that decoder's pixel budget (when it said
    /// one), and H.264, which every headset decodes, at any size when it names no
    /// decoder for it. The PC never starts a stream that does not fit.
    pub fn fits(&self, codec: Codec, w: u32, h: u32) -> bool {
        match self.decoder(codec) {
            Some(d) => {
                w <= d.max_w
                    && h <= d.max_h
                    && self
                        .max_pixels(codec)
                        .is_none_or(|p| u64::from(w) * u64::from(h) <= u64::from(p))
            }
            None => codec == Codec::H264,
        }
    }

    /// The pixel budget as its own part, when there is one.
    pub fn pixels_part(&self) -> Option<DecoderPixels> {
        (!self.pixels.is_empty()).then(|| DecoderPixels {
            budgets: self.pixels.clone(),
        })
    }

    pub fn check(&self) -> Result<(), WireError> {
        if self.decoders.iter().any(|d| d.max_w == 0 || d.max_h == 0) {
            return Err(WireError::Invalid("a decoder of no size"));
        }
        Ok(())
    }
}

wire_struct! {
    /// The headset's settings the PC acts on (a part).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Prefs {
        /// Pass the PC's notifications to the headset.
        pub notifications: bool,
        /// Say which display the sound comes from.
        pub sound_from_screen: bool,
        /// "Focus": the displays out of focus get fewer frames.
        pub focus_mode: bool,
        /// The mouse follows the gaze after resting this long on a display.
        pub gaze_mouse_ms: Option<u16>,
        /// "No limits": more displays than the safe budgets allow.
        pub no_limits: bool,
    }
}

wire_struct! {
    /// Whether the headset is worn (a part); streaming pauses while it is not.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Presence {
        pub worn: bool,
    }
}

wire_struct! {
    /// The headset's battery (a part), for the PC's tray.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Battery {
        pub percent: u8,
        pub charging: bool,
    }
}

wire_struct! {
    /// The headset's volume (a part): its level in percent, and the counters that
    /// tell an answer from a change (see [`crate::volume`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Volume {
        pub level: u8,
        pub muted: bool,
        /// Changes made on the headset itself so far.
        pub seq: u32,
        /// The PC's request this answers; 0 for a change of the headset's own.
        pub ask: u32,
    }
}

impl Volume {
    pub fn check(&self) -> Result<(), WireError> {
        if self.level > 100 {
            return Err(WireError::Invalid("a volume over 100 %"));
        }
        Ok(())
    }
}

wire_struct! {
    /// The PC asks the headset to take its slider's level (see [`crate::volume`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VolumeAsk {
        pub level: u8,
        pub muted: bool,
        /// The last [`Volume::seq`] the PC saw.
        pub seen: u32,
        /// This request's number, counted by the PC from 1.
        pub ask: u32,
    }
}

impl VolumeAsk {
    pub fn check(&self) -> Result<(), WireError> {
        if self.level > 100 {
            return Err(WireError::Invalid("a volume over 100 %"));
        }
        Ok(())
    }
}

/// The headset's microphone permission ([`UpState::permission`]); any other value
/// reads as not known.
pub mod permission {
    pub const UNKNOWN: u8 = 0;
    pub const GRANTED: u8 = 1;
    pub const DENIED: u8 = 2;
    /// The system is asking the user now.
    pub const ASKING: u8 = 3;
}

wire_struct! {
    /// A profile saved in the headset: its name as the person gave it, or, empty, none
    /// (the headset's menu then names it by its number, "Profile 2").
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ProfileName {
        #[wire(max = MAX_DISPLAY_NAME)]
        pub name: String,
    }
}

wire_struct! {
    /// The profiles saved in the headset (a part, from 0.2.0 step 7.10), in the order
    /// its menu lists them, and the one in use (switched to or saved last, until
    /// something is changed by hand), by its place in `list`. Commands name a profile
    /// by its name or its number from 1 in this order.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Profiles {
        #[wire(max = MAX_PROFILES)]
        pub list: Vec<ProfileName>,
        pub active: Option<u8>,
    }
}

wire_struct! {
    /// The headset's switch for a kind of up stream, and whether the system lets it
    /// record (a part for each kind).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UpState {
        pub kind: UpKind,
        pub switch: bool,
        /// See [`permission`].
        pub permission: u8,
    }
}

wire_struct! {
    /// A display in a row of [`Row`], with where its centre is seen, in pixels of one
    /// scale shared by all displays: across (`None`: just right of the previous one)
    /// and down (`None`: not known, centred in the row).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Placed {
        pub id: DisplayId,
        pub center: Option<i32>,
        pub middle: Option<i32>,
    }
}

wire_struct! {
    /// A row of displays, left to right.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Row {
        #[wire(max = MAX_DISPLAYS)]
        pub entries: Vec<Placed>,
    }
}

impl Row {
    pub fn ids(&self) -> impl Iterator<Item = DisplayId> + '_ {
        self.entries.iter().map(|e| e.id)
    }
}

/// Rows from the headset: at most [`MAX_DISPLAYS`] displays in all, each once.
pub fn check_rows(rows: &[Row]) -> Result<(), WireError> {
    let len = rows.iter().map(|r| r.entries.len()).sum();
    if len > MAX_DISPLAYS {
        return Err(WireError::TooLong {
            len,
            max: MAX_DISPLAYS,
        });
    }
    let mut seen = Vec::with_capacity(len);
    for id in rows.iter().flat_map(Row::ids) {
        if seen.contains(&id) {
            return Err(WireError::Repeated(id.0));
        }
        seen.push(id);
    }
    Ok(())
}

/// A part of the state inside `Snapshot` or `Open`: the tag of the message it is, and
/// the message's payload, so it is read by the same code as that message on its own.
/// A part a reader does not know is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub tag: u8,
    pub body: Vec<u8>,
}

impl Wire for Part {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        self.tag.put(out)?;
        crate::wire::Limited::put(&self.body, out, MAX_GROWING)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            tag: u8::get(r)?,
            body: crate::wire::Limited::get(r, MAX_GROWING)?,
        })
    }
}

/// The PC's state in parts: what the PC builds from what it has and compares with
/// what it sent this session (so a part goes out only when it changed), and what the
/// headset keeps from `Snapshot` and the parts after it. One type for both, so the two
/// cannot drift apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PcState {
    pub displays: Option<Displays>,
    pub info: Option<PcInfo>,
    pub caps: Option<HostCaps>,
    pub access: Option<Access>,
    pub limits: Option<Limits>,
    pub encoding: Option<Encoding>,
    pub sound: Option<Sound>,
    /// A part for each kind of up stream.
    pub up_wanted: std::collections::BTreeMap<UpKind, UpWanted>,
    pub focus: Option<Focus>,
    pub sound_from: Option<SoundFrom>,
}

/// Tags of the PC's parts, in the order they go: `Displays` first (other parts name its
/// displays), `Focus` and `SoundFrom` last.
const PC_PARTS: &[u8] = &[31, 36, 37, 35, 34, 32, 33, 40, 16, 23];

impl PcState {
    /// Whether messages with `tag` are parts of the PC's state.
    pub(crate) fn is_part(tag: u8) -> bool {
        PC_PARTS.contains(&tag)
    }

    /// Every part there is, in the order they go.
    pub fn parts(&self) -> Vec<crate::HostMsg> {
        self.changed(&Self::default())
    }

    /// The parts that differ from `sent`, whole, in the order they go.
    pub fn changed(&self, sent: &Self) -> Vec<crate::HostMsg> {
        use crate::HostMsg as M;
        let mut out = Vec::new();
        macro_rules! each {
            ($($field:ident => $variant:ident),*) => {$(
                if let Some(part) = &self.$field
                    && sent.$field.as_ref() != Some(part)
                {
                    out.push(M::$variant { part: part.clone() });
                }
            )*};
        }
        each!(
            displays => Displays,
            info => PcInfo,
            caps => HostCaps,
            access => Access,
            limits => Limits,
            encoding => Encoding,
            sound => Sound
        );
        for (kind, now) in &self.up_wanted {
            if sent.up_wanted.get(kind) != Some(now) {
                out.push(M::UpWanted { part: *now });
            }
        }
        each!(focus => Focus, sound_from => SoundFrom);
        out
    }

    /// The whole state as one `Snapshot`.
    pub fn snapshot(&self) -> Result<crate::HostMsg, WireError> {
        let parts = self
            .parts()
            .iter()
            .filter_map(crate::HostMsg::to_part)
            .collect::<Result<Vec<Part>, WireError>>()?;
        Ok(crate::HostMsg::Snapshot { parts })
    }

    /// Takes the part `msg`; gives back a message that is not one. A part that names
    /// a display the state does not list is the PC's bug: an error.
    pub fn apply(&mut self, msg: crate::HostMsg) -> Result<Option<crate::HostMsg>, WireError> {
        use crate::HostMsg as M;
        let known = |id: Option<DisplayId>, displays: &Option<Displays>| {
            displays.as_ref().is_none_or(|d| d.knows(id))
        };
        match msg {
            M::Displays { part } => self.displays = Some(part),
            M::PcInfo { part } => self.info = Some(part),
            M::HostCaps { part } => self.caps = Some(part),
            M::Access { part } => self.access = Some(part),
            M::Limits { part } => self.limits = Some(part),
            M::Encoding { part } => self.encoding = Some(part),
            M::Sound { part } => self.sound = Some(part),
            M::UpWanted { part } => {
                self.up_wanted.insert(part.kind, part);
            }
            M::Focus { part } if known(part.display, &self.displays) => self.focus = Some(part),
            M::SoundFrom { part } if known(part.display, &self.displays) => {
                self.sound_from = Some(part);
            }
            M::Focus { .. } | M::SoundFrom { .. } => {
                return Err(WireError::Invalid("a display that is not listed"));
            }
            other => return Ok(Some(other)),
        }
        Ok(None)
    }

    /// The state a `Snapshot`'s parts hold. A part this build does not know is skipped;
    /// one twice (of the same kind, for the parts of each kind) is an error.
    pub fn from_snapshot(parts: &[Part]) -> Result<Self, WireError> {
        let mut state = Self::default();
        let mut seen = Vec::new();
        for part in parts.iter().filter(|p| Self::is_part(p.tag)) {
            let msg = crate::HostMsg::decode(part.tag, &part.body)?;
            let kind = match &msg {
                crate::HostMsg::UpWanted { part } => Some(part.kind),
                _ => None,
            };
            if seen.contains(&(part.tag, kind)) {
                return Err(WireError::Invalid("a part twice in one snapshot"));
            }
            seen.push((part.tag, kind));
            state.apply(msg)?;
        }
        Ok(state)
    }
}

/// The headset's state in parts: what it says in `Open` and changes after, as the PC
/// keeps it for the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadsetState {
    pub caps: Option<HeadsetCaps>,
    pub prefs: Option<Prefs>,
    pub presence: Option<Presence>,
    pub battery: Option<Battery>,
    pub volume: Option<Volume>,
    /// A part for each kind of up stream.
    pub up: std::collections::BTreeMap<UpKind, UpState>,
    /// The profiles saved in the headset (from 0.2.0 step 7.10).
    pub profiles: Option<Profiles>,
}

/// Tags of the headset's parts, in the order they go.
const HEADSET_PARTS: &[u8] = &[138, 144, 134, 115, 119, 135, 136, 143];

impl HeadsetState {
    /// Whether messages with `tag` are parts of the headset's state.
    pub(crate) fn is_part(tag: u8) -> bool {
        HEADSET_PARTS.contains(&tag)
    }

    /// Every part there is, in the order they go.
    pub fn parts(&self) -> Vec<crate::ClientMsg> {
        use crate::ClientMsg as M;
        let mut out = Vec::new();
        out.extend(self.caps.clone().map(|part| M::HeadsetCaps { part }));
        out.extend(
            self.caps
                .as_ref()
                .and_then(HeadsetCaps::pixels_part)
                .map(|part| M::DecoderPixels { part }),
        );
        out.extend(self.prefs.map(|part| M::Prefs { part }));
        out.extend(self.presence.map(|part| M::Presence { part }));
        out.extend(self.battery.map(|part| M::Battery { part }));
        out.extend(self.volume.map(|part| M::Volume { part }));
        out.extend(self.up.values().map(|part| M::UpState { part: *part }));
        out.extend(self.profiles.clone().map(|part| M::Profiles { part }));
        out
    }

    /// The whole state as `Open`.
    pub fn open(&self) -> Result<crate::ClientMsg, WireError> {
        let parts = self
            .parts()
            .iter()
            .filter_map(crate::ClientMsg::to_part)
            .collect::<Result<Vec<Part>, WireError>>()?;
        Ok(crate::ClientMsg::Open { parts })
    }

    /// Takes the part `msg`; gives back a message that is not one.
    pub fn apply(&mut self, msg: crate::ClientMsg) -> Option<crate::ClientMsg> {
        use crate::ClientMsg as M;
        match msg {
            // The budget may come before or after the caps it belongs to.
            M::HeadsetCaps { mut part } => {
                if part.pixels.is_empty() {
                    part.pixels = self.caps.take().map(|c| c.pixels).unwrap_or_default();
                }
                self.caps = Some(part);
            }
            M::DecoderPixels { part } => {
                self.caps.get_or_insert_with(HeadsetCaps::default).pixels = part.budgets;
            }
            M::Prefs { part } => self.prefs = Some(part),
            M::Presence { part } => self.presence = Some(part),
            M::Battery { part } => self.battery = Some(part),
            M::Volume { part } => self.volume = Some(part),
            M::UpState { part } => {
                self.up.insert(part.kind, part);
            }
            M::Profiles { part } => self.profiles = Some(part),
            other => return Some(other),
        }
        None
    }

    /// The state `Open`'s parts hold: `HeadsetCaps` must be among them. A part this
    /// build does not know is skipped; one twice is an error.
    pub fn from_open(parts: &[Part]) -> Result<Self, WireError> {
        let mut state = Self::default();
        let mut seen = Vec::new();
        for part in parts.iter().filter(|p| Self::is_part(p.tag)) {
            let msg = crate::ClientMsg::decode(part.tag, &part.body)?;
            let kind = match &msg {
                crate::ClientMsg::UpState { part } => Some(part.kind),
                _ => None,
            };
            if seen.contains(&(part.tag, kind)) {
                return Err(WireError::Invalid("a part twice in Open"));
            }
            seen.push((part.tag, kind));
            state.apply(msg);
        }
        if !seen.iter().any(|(tag, _)| *tag == 138) {
            return Err(WireError::Invalid(
                "Open without the headset's capabilities",
            ));
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{decode, encode};

    fn display(id: u32) -> Display {
        Display {
            id: DisplayId(id),
            kind: DisplayKind::Virtual,
            name: format!("Prostir-{id}"),
            mode: Mode::new(2560, 1440),
            desk: Some(DeskRect {
                x: -2560,
                y: 0,
                w: 2560,
                h: 1440,
            }),
            state: DisplayState::Running,
        }
    }

    /// Each kind of display says what it is, and only that.
    #[test]
    fn a_display_says_what_kind_it_is() {
        let kinds = [
            (DisplayKind::Virtual, [true, false, false]),
            (
                DisplayKind::Window {
                    title: "Notes".into(),
                },
                [false, true, false],
            ),
            (
                DisplayKind::Physical {
                    builtin: true,
                    connector: "eDP-1".into(),
                    shown: false,
                },
                [false, false, true],
            ),
        ];
        for (kind, expected) in kinds {
            let d = Display {
                kind: kind.clone(),
                ..display(1)
            };
            assert_eq!(
                [d.is_virtual(), d.is_window(), d.is_physical()],
                expected,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn texts_are_cut_on_a_character_boundary() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 3), "abc");
        // "ї" takes two bytes: never half of it.
        assert_eq!(clip("aїb", 2), "a");
        assert_eq!(clip("aїb", 3), "aї");
        let long = "ї".repeat(400);
        let cut = clip(&long, MAX_DETAIL);
        assert!(cut.len() <= MAX_DETAIL && cut.len() >= MAX_DETAIL - 1);
        let failed = DisplayState::Failed {
            error: ErrorCode::DESKTOP,
            detail: cut,
        };
        assert!(encode(&failed).is_ok());
    }

    #[test]
    fn unknown_error_codes_read_as_other() {
        assert_eq!(decode::<ErrorCode>(&[4, 0]), Ok(ErrorCode::LIMIT));
        let unknown = decode::<ErrorCode>(&[200, 0]).unwrap();
        assert_eq!(unknown.known(), ErrorCode::OTHER);
        assert_eq!(unknown.code(), 200, "kept for the log");
        assert_eq!(ErrorCode::from_code(14), ErrorCode::BAD_ARGUMENT);
        assert_eq!(ErrorCode::from_code(15), ErrorCode::OTHER);
    }

    #[test]
    fn displays_are_checked() {
        let mut d = Displays {
            displays: vec![display(1), display(100)],
            primary: Some(DisplayId(1)),
        };
        assert_eq!(d.check(), Ok(()));
        d.primary = Some(DisplayId(7));
        assert!(matches!(d.check(), Err(WireError::Invalid(_))));
        d.primary = None;
        d.displays.push(display(1));
        assert_eq!(d.check(), Err(WireError::Repeated(1)));
        d.displays.pop();
        d.displays[0].mode.scale = 0;
        assert!(matches!(d.check(), Err(WireError::Invalid(_))));
        // A whole list of the most displays with the longest texts fits in a part.
        let mut big = display(0);
        big.kind = DisplayKind::Window {
            title: "w".repeat(MAX_TITLE),
        };
        big.name = "n".repeat(MAX_DISPLAY_NAME);
        big.state = DisplayState::Failed {
            error: ErrorCode::OTHER,
            detail: "d".repeat(MAX_DETAIL),
        };
        let all = Displays {
            displays: (0..MAX_DISPLAYS as u32)
                .map(|i| Display {
                    id: DisplayId(i),
                    ..big.clone()
                })
                .collect(),
            primary: Some(DisplayId(0)),
        };
        let bytes = encode(&all).unwrap();
        assert!(bytes.len() < MAX_GROWING, "{} bytes", bytes.len());
        assert_eq!(decode::<Displays>(&bytes).unwrap(), all);
        let mut over = all;
        over.displays.push(display(99));
        assert!(encode(&over).is_err(), "over the frozen limit");
    }

    #[test]
    fn rows_are_checked() {
        let at = |id| Placed {
            id: DisplayId(id),
            center: None,
            middle: Some(-3),
        };
        let rows = vec![
            Row {
                entries: vec![at(1), at(2)],
            },
            Row {
                entries: vec![at(300)],
            },
        ];
        assert_eq!(check_rows(&rows), Ok(()));
        let twice = vec![Row {
            entries: vec![at(1), at(1)],
        }];
        assert_eq!(check_rows(&twice), Err(WireError::Repeated(1)));
        let many: Vec<Row> = (0..2)
            .map(|r| Row {
                entries: (0..20).map(|i| at(r * 20 + i)).collect(),
            })
            .collect();
        assert!(matches!(check_rows(&many), Err(WireError::TooLong { .. })));
    }

    #[test]
    fn values_from_the_other_side_are_checked() {
        let v = Volume {
            level: 101,
            muted: false,
            seq: 0,
            ask: 0,
        };
        assert!(v.check().is_err());
        assert!(Volume { level: 100, ..v }.check().is_ok());
        let pcm = |rate, channels| UpFormat::Pcm16 { rate, channels }.check();
        assert!(pcm(48_000, 1).is_ok() && pcm(48_000, 2).is_ok());
        assert!(pcm(44_100, 1).is_err() && pcm(48_000, 0).is_err() && pcm(48_000, 3).is_err());
        let caps = HeadsetCaps {
            decoders: vec![Decoder {
                codec: 0,
                max_w: 0,
                max_h: 4096,
            }],
            ..HeadsetCaps::default()
        };
        assert!(caps.check().is_err());
    }

    #[test]
    fn headset_decoders_match_codecs_by_number() {
        let caps = HeadsetCaps {
            decoders: vec![
                Decoder {
                    codec: 1,
                    max_w: 4096,
                    max_h: 4096,
                },
                // A codec of a newer headset: it matches nothing.
                Decoder {
                    codec: 77,
                    max_w: 8192,
                    max_h: 8192,
                },
            ],
            ..HeadsetCaps::default()
        };
        assert!(caps.decodes(Codec::H265));
        assert!(!caps.decodes(Codec::H264));
    }

    #[test]
    fn rights_and_roles() {
        assert!(Access::MAIN.is_main() && Access::MAIN.may(perms::PRIVATE | perms::CHANGES));
        let guest = Access {
            role: 9,
            perms: perms::INPUT | 1 << 20,
        };
        assert!(!guest.is_main(), "an unknown role reads as a guest");
        assert!(guest.may(perms::INPUT) && !guest.may(perms::CHANGES));
    }

    #[test]
    fn a_part_is_a_tag_and_a_payload() {
        let part = Part {
            tag: 31,
            body: vec![1, 2, 3],
        };
        assert_eq!(encode(&part).unwrap(), [31, 3, 0, 1, 2, 3]);
        assert_eq!(decode::<Part>(&[31, 3, 0, 1, 2, 3]).unwrap(), part);
    }
}
