//! Headset-side preferences chosen in the menu, kept between launches.

#![forbid(unsafe_code)]

pub use crate::background::Sky;
use crate::lang::{Lang, Language};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Around the user: wherever a monitor is put, it turns to face them.
    Arc,
    /// A flat wall: all monitors in one plane, facing the same way, for walking along.
    Wall,
    /// Above the head, for lying down.
    Overhead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    Flat,
    Gentle,
    Strong,
}

impl Curve {
    /// Cylinder radius as a multiple of the monitor's distance from the user; `None` is flat.
    pub fn radius_factor(self) -> Option<f32> {
        match self {
            Curve::Flat => None,
            Curve::Gentle => Some(2.0),
            Curve::Strong => Some(1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Black,
    /// Passthrough cameras.
    Room,
    /// The room, darker: the monitors stand out more.
    RoomDim,
    /// A built-in panorama.
    Sky(Sky),
    /// The user's own `background.jpg/png`.
    Custom,
}

impl Background {
    /// Shows the room through the cameras.
    pub fn is_room(self) -> bool {
        matches!(self, Background::Room | Background::RoomDim)
    }

    /// How much of the room shows behind the monitors (1: all; less darkens it).
    pub fn room_opacity(self) -> f32 {
        if self == Background::RoomDim {
            0.4
        } else {
            1.0
        }
    }
}

/// How the date on the palm menu is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateStyle {
    /// In the menu's language.
    #[default]
    LikeMenu,
    /// `Ср, 18 лист.`
    Ukrainian,
    /// `Wed, Nov 18`
    English,
    /// `18.11.2026`
    Numbers,
}

/// What tracked hands do in the headset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandMode {
    /// Not drawn and not tracked by the app (saves rendering every frame).
    Hidden,
    /// Drawn, e.g. to see them over the keyboard, but they do not click or drag.
    Visible,
    /// Drawn and point, click and drag. Off by default: hands over a keyboard
    /// would otherwise click things while typing.
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub preset: Preset,
    pub curve: Curve,
    pub background: Background,
    pub hands: HandMode,
    /// Dim the monitors the mouse is not on.
    pub focus: bool,
    /// One big screen further away; the other monitors are hidden.
    pub cinema: bool,
    /// The first-run tips were shown and dismissed.
    pub intro_seen: bool,
    /// A passthrough window onto the real keyboard.
    pub keyboard_window: bool,
    /// The headset microphone may be used by the PC. Decided here, on the headset:
    /// the PC can only ask.
    pub mic: bool,
    /// The menu tab shown last (see `app::tab_code`).
    pub last_tab: u8,
    /// The monitors come along when the user turns or moves away.
    pub follow: bool,
    /// A monitor let go almost against another sticks to it.
    pub magnet: bool,
    /// Dragging one monitor moves the whole arrangement.
    pub move_all: bool,
    /// "Locked": the monitors stay where they are, a hand or a controller takes none
    /// (the menu's own arranging, and the PC's, still move them).
    pub locked: bool,
    /// Sound placed on the side of the monitor whose window plays it.
    pub sound_from_screen: bool,
    /// The clock in 12-hour time (2:05 PM) rather than 24-hour (14:05).
    pub clock_12h: bool,
    /// How the date is written (see [`DateStyle`]).
    pub date_style: DateStyle,
    /// The round ☰ button under the main monitor.
    pub menu_button: bool,
    /// How bright the monitors are, percent (40 to 100).
    pub brightness: u8,
    /// The mouse goes to the monitor the user looks at (where the PC allows it).
    pub gaze_mouse: bool,
    /// A bigger menu, easier to read and hit.
    pub large_menu: bool,
    /// Left-handed: the wrist menu on the right wrist, touched with the left hand.
    pub left_handed: bool,
    /// The headset display at 72 Hz instead of 90 (the video is 60 frames a second).
    pub battery_saver: bool,
    /// The PC's desktop notifications shown in the headset.
    pub notifications: bool,
    /// "No limits": more monitors and displays than the safe budgets allow (the PC
    /// reads it and puts its own hard limits on top).
    pub no_limits: bool,
    /// Rest reminders: minutes of wearing between them (0: none).
    pub rest_every: u16,
    pub language: Language,
    /// The language of the PC last connected to (for "like the PC" before connecting).
    pub pc_language: Option<Lang>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preset: Preset::Arc,
            curve: Curve::Flat,
            background: Background::Black,
            hands: HandMode::Visible,
            focus: false,
            cinema: false,
            intro_seen: false,
            keyboard_window: false,
            mic: true,
            last_tab: 0,
            follow: false,
            magnet: true,
            move_all: false,
            locked: false,
            sound_from_screen: false,
            clock_12h: false,
            date_style: DateStyle::LikeMenu,
            menu_button: true,
            brightness: 100,
            gaze_mouse: false,
            large_menu: false,
            left_handed: false,
            battery_saver: false,
            notifications: false,
            no_limits: false,
            rest_every: 0,
            language: Language::LikePc,
            pc_language: None,
        }
    }
}

impl Settings {
    /// Reads `key value` lines; unknown keys are ignored and missing ones default.
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("preset", v)) => {
                    s.preset = match v {
                        "wall" => Preset::Wall,
                        "overhead" => Preset::Overhead,
                        _ => Preset::Arc,
                    }
                }
                Some(("curve", v)) => {
                    s.curve = match v {
                        "gentle" => Curve::Gentle,
                        "strong" => Curve::Strong,
                        _ => Curve::Flat,
                    }
                }
                Some(("focus", v)) => s.focus = v == "on",
                Some(("cinema", v)) => s.cinema = v == "on",
                Some(("intro", v)) => s.intro_seen = v == "seen",
                Some(("keyboard-window", v)) => s.keyboard_window = v == "on",
                Some(("mic", v)) => s.mic = v != "off",
                Some(("tab", v)) => s.last_tab = v.trim().parse().unwrap_or(0),
                Some(("follow", v)) => s.follow = v == "on",
                Some(("magnet", v)) => s.magnet = v != "off",
                Some(("move-all", v)) => s.move_all = v == "on",
                Some(("locked", v)) => s.locked = v == "on",
                Some(("sound-from-screen", v)) => s.sound_from_screen = v == "on",
                Some(("clock", v)) => s.clock_12h = v == "12h",
                Some(("date", v)) => {
                    s.date_style = match v {
                        "uk" => DateStyle::Ukrainian,
                        "en" => DateStyle::English,
                        "numbers" => DateStyle::Numbers,
                        _ => DateStyle::LikeMenu,
                    }
                }
                Some(("menu-button", v)) => s.menu_button = v != "off",
                Some(("brightness", v)) => {
                    s.brightness = v.trim().parse::<u8>().unwrap_or(100).clamp(40, 100);
                }
                Some(("gaze-mouse", v)) => s.gaze_mouse = v == "on",
                Some(("large-menu", v)) => s.large_menu = v == "on",
                Some(("left-handed", v)) => s.left_handed = v == "on",
                Some(("battery-saver", v)) => s.battery_saver = v == "on",
                Some(("notifications", v)) => s.notifications = v == "on",
                Some(("no-limits", v)) => s.no_limits = v == "on",
                Some(("rest-every", v)) => s.rest_every = v.trim().parse().unwrap_or(0),
                Some(("language", v)) => {
                    s.language = match v {
                        "like-pc" => Language::LikePc,
                        "like-system" => Language::LikeSystem,
                        code => Lang::new(code).map_or(Language::LikePc, Language::Fixed),
                    }
                }
                Some(("pc-language", v)) => s.pc_language = Lang::from_tag(v),
                Some(("hands", v)) => {
                    s.hands = match v {
                        "hidden" => HandMode::Hidden,
                        "on" | "control" => HandMode::Control,
                        _ => HandMode::Visible,
                    }
                }
                Some(("background", v)) => {
                    s.background = match v {
                        "room" => Background::Room,
                        "room-dim" => Background::RoomDim,
                        "custom" => Background::Custom,
                        "panorama" | "sky-night" => Background::Sky(Sky::Night),
                        "sky-sunset" => Background::Sky(Sky::Sunset),
                        "sky-space" => Background::Sky(Sky::Space),
                        "sky-aurora" => Background::Sky(Sky::Aurora),
                        _ => Background::Black,
                    }
                }
                _ => {}
            }
        }
        s
    }

    /// The text of `settings.txt`.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        self.write_text(&mut text);
        text
    }

    /// [`Settings::to_text`] added to `out` (a buffer kept between saves).
    pub fn write_text(&self, out: &mut String) {
        use std::fmt::Write as _;
        let preset = match self.preset {
            Preset::Arc => "arc",
            Preset::Wall => "wall",
            Preset::Overhead => "overhead",
        };
        let curve = match self.curve {
            Curve::Flat => "flat",
            Curve::Gentle => "gentle",
            Curve::Strong => "strong",
        };
        let background = match self.background {
            Background::Black => "black",
            Background::Room => "room",
            Background::RoomDim => "room-dim",
            Background::Custom => "custom",
            Background::Sky(Sky::Night) => "sky-night",
            Background::Sky(Sky::Sunset) => "sky-sunset",
            Background::Sky(Sky::Space) => "sky-space",
            Background::Sky(Sky::Aurora) => "sky-aurora",
        };
        let hands = match self.hands {
            HandMode::Hidden => "hidden",
            HandMode::Visible => "visible",
            HandMode::Control => "control",
        };
        let on = |b: bool| if b { "on" } else { "off" };
        let _ = write!(
            out,
            "preset {preset}\ncurve {curve}\nbackground {background}\nhands {hands}\n\
             focus {}\ncinema {}\nlanguage {}\npc-language {}\nintro {}\nkeyboard-window {}\nmic {}\ntab {}\nfollow {}\n\
             gaze-mouse {}\nlarge-menu {}\nleft-handed {}\nbattery-saver {}\nnotifications {}\nrest-every {}\nmagnet {}\nmove-all {}\nlocked {}\nsound-from-screen {}\nclock {}\ndate {}\nmenu-button {}\nbrightness {}\nno-limits {}\n",
            on(self.focus),
            on(self.cinema),
            match &self.language {
                Language::LikePc => "like-pc",
                Language::LikeSystem => "like-system",
                Language::Fixed(l) => l.as_str(),
            },
            self.pc_language.as_ref().map_or("", Lang::as_str),
            if self.intro_seen { "seen" } else { "new" },
            on(self.keyboard_window),
            on(self.mic),
            self.last_tab,
            on(self.follow),
            on(self.gaze_mouse),
            on(self.large_menu),
            on(self.left_handed),
            on(self.battery_saver),
            on(self.notifications),
            self.rest_every,
            on(self.magnet),
            on(self.move_all),
            on(self.locked),
            on(self.sound_from_screen),
            if self.clock_12h { "12h" } else { "24h" },
            match self.date_style {
                DateStyle::LikeMenu => "like-menu",
                DateStyle::Ukrainian => "uk",
                DateStyle::English => "en",
                DateStyle::Numbers => "numbers",
            },
            on(self.menu_button),
            self.brightness,
            on(self.no_limits)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = Settings {
            preset: Preset::Overhead,
            curve: Curve::Gentle,
            background: Background::Sky(Sky::Aurora),
            hands: HandMode::Control,
            focus: true,
            cinema: true,
            intro_seen: true,
            keyboard_window: true,
            mic: false,
            last_tab: 2,
            follow: true,
            magnet: false,
            move_all: true,
            locked: true,
            sound_from_screen: true,
            clock_12h: true,
            date_style: DateStyle::Numbers,
            menu_button: false,
            brightness: 60,
            gaze_mouse: true,
            large_menu: true,
            left_handed: true,
            battery_saver: true,
            notifications: true,
            no_limits: true,
            rest_every: 20,
            language: Language::Fixed(Lang::FALLBACK),
            pc_language: Lang::new("uk"),
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
        // Written into a buffer that held a longer text: only the new text is read.
        let mut out = Settings::default().to_text().repeat(2);
        out.clear();
        s.write_text(&mut out);
        assert_eq!(out, s.to_text());
        // Which screen cinema shows is the arrangement's now (`layout.txt`): a line
        // of an older version is passed over.
        let older = format!("cinema-screen 2\n{}", s.to_text());
        assert_eq!(Settings::parse(&older), s);
        assert!(!s.to_text().contains("cinema-screen"));
    }

    /// Any language code is kept, and the two "like" choices; what is not a code is
    /// "like the PC", as before.
    #[test]
    fn languages() {
        let read = |v: &str| Settings::parse(&format!("language {v}\n")).language;
        assert_eq!(read("uk"), Language::Fixed(Lang::new("uk").unwrap()));
        assert_eq!(read("pt-BR"), Language::Fixed(Lang::new("pt-BR").unwrap()));
        assert_eq!(read("like-system"), Language::LikeSystem);
        assert_eq!(read("like-pc"), Language::LikePc);
        assert_eq!(read("../../etc"), Language::LikePc);
        for language in [
            Language::LikeSystem,
            Language::Fixed(Lang::new("de").unwrap()),
        ] {
            let s = Settings {
                language,
                ..Settings::default()
            };
            assert_eq!(Settings::parse(&s.to_text()).language, language);
        }
        let pc = Settings::parse("pc-language uk_UA.UTF-8\n").pc_language;
        assert_eq!(pc.map(|l| l.to_string()).as_deref(), Some("uk-UA"));
    }
}
