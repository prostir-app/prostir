//! `settings.txt`: the settings, the rights of each headset, and the lines this
//! version does not know, kept.

use super::*;

/// The PC's settings, as the person chose them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Mbit/s per monitor.
    pub bitrate: u32,
    /// The person chose the quality (a `bitrate` line): it then holds for every
    /// monitor. Not chosen, a monitor of 2560x1440 or more streams at
    /// [`USB_LARGE`] over USB.
    pub bitrate_chosen: bool,
    /// The clearer-text encoder preset (VA-API's `target-usage` 4), as chosen: on or
    /// off for every monitor. Not chosen (`None`, written as no line): on for monitors
    /// smaller than 2560x1440, where it keeps text far clearer at the same bitrate
    /// within the time of a frame (see `encoder::slow_fits`).
    pub sharp: Option<bool>,
    pub codec: Codec,
    /// Encode on the CPU.
    pub software: bool,
    /// PC sound to the headset.
    pub audio: bool,
    /// The laptop screen in the headset.
    pub mirror: bool,
    /// The answer to "Count this PC among users?" (see `usage`): `count unasked`,
    /// `count yes` or `count no`. Nothing is counted until it is yes.
    pub count: Counting,
    /// Starting with the session, as the person chose it (`autostart on|off`); none
    /// yet before the first start, which turns it on (see `platform::autostart`).
    /// Once kept, an update never changes it.
    pub autostart: Option<bool>,
    /// The settings window's look (`window-theme`, `window-palette`,
    /// `window-language`), written only when not the default.
    pub window: prostir_api::WindowLook,
    /// Each headset's role and rights, by its key (hex): `access KEY main input mic`.
    /// A headset without a line is the main one, with every right.
    pub access: BTreeMap<String, Access>,
}

impl Settings {
    /// Whether clearer text shows as on: chosen so, or not chosen (on where it fits).
    pub fn clear_text(&self) -> bool {
        self.sharp.unwrap_or(true)
    }

    /// Clearer text chosen `on`: a choice only when it changes what shows, so a
    /// headset that sends what it shows changes nothing.
    pub fn choose_clear_text(&mut self, on: bool) {
        if on != self.clear_text() {
            self.sharp = Some(on);
        }
    }

    /// The role and rights of the headset with key `key`.
    pub fn access_of(&self, key: &[u8; 32]) -> Access {
        self.access.get(&hex(key)).copied().unwrap_or(Access::MAIN)
    }
}

/// A key as `access` lines name it.
pub(super) fn hex(key: &[u8; 32]) -> String {
    prostir_proto::secure::to_hex(key)
}

/// The words of rights in `access` lines.
const PERMS: [(&str, u32); 6] = [
    ("input", perms::INPUT),
    ("mic", perms::MIC),
    ("camera", perms::CAMERA),
    ("clipboard", perms::CLIPBOARD),
    ("changes", perms::CHANGES),
    ("private", perms::PRIVATE),
];

/// The words of the rights in `perms`, as `access` lines write them.
pub fn perm_words(perms: u32) -> Vec<String> {
    PERMS
        .iter()
        .filter(|(_, bit)| perms & bit != 0)
        .map(|(word, _)| (*word).to_owned())
        .collect()
}

/// No right at all: a headset whose `access` line has a word that is no role or right.
const NO_RIGHTS: Access = Access {
    role: role::GUEST,
    perms: 0,
};

/// `KEY main input mic …`: a headset's role and rights, and whether the line was
/// mistyped. A line with a key and a word that is no role or right (a typo) makes that
/// headset a guest with no rights, never one with every right as a headset without a
/// line is; `None` for a line without a key.
fn parse_access(value: &str) -> Option<(String, Access, bool)> {
    let mut words = value.split_whitespace();
    let key = words.next()?;
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let key = key.to_ascii_lowercase();
    let role = match words.next() {
        Some("main") => role::MAIN,
        Some("guest") => role::GUEST,
        _ => return Some((key, NO_RIGHTS, true)),
    };
    let mut perms = 0;
    for w in words {
        match PERMS.iter().find(|(name, _)| *name == w) {
            Some((_, bit)) => perms |= bit,
            None => return Some((key, NO_RIGHTS, true)),
        }
    }
    Some((key, Access { role, perms }, false))
}

fn access_text(key: &str, a: Access) -> String {
    let role = if a.is_main() { "main" } else { "guest" };
    let mut text = format!("access {key} {role}");
    for (name, bit) in PERMS {
        if a.may(bit) {
            text.push(' ');
            text.push_str(name);
        }
    }
    text
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bitrate: 30,
            bitrate_chosen: false,
            sharp: None,
            codec: Codec::H265,
            software: false,
            audio: true,
            mirror: true,
            count: Counting::Unasked,
            autostart: None,
            window: prostir_api::WindowLook::default(),
            access: BTreeMap::new(),
        }
    }
}

fn on_off(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

fn counting_name(count: Counting) -> &'static str {
    match count {
        Counting::Yes => "yes",
        Counting::No => "no",
        Counting::Unasked | Counting::Other => "unasked",
    }
}

/// The answer a `count` line holds. `on` and `off` are what earlier builds of 0.2
/// wrote, where counting was on until turned off: `off` was the person's choice and
/// stays no, but `on` was mostly the default nobody chose, so it is no answer.
fn parse_counting(value: &str) -> Option<Counting> {
    match value {
        "yes" => Some(Counting::Yes),
        "no" | "off" => Some(Counting::No),
        "unasked" | "on" => Some(Counting::Unasked),
        _ => None,
    }
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::H265 => "h265",
        Codec::Av1 => "av1",
    }
}

/// A palette's or a language's name: one short word of letters, digits and `-`.
pub fn is_word(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The settings in `text`. Before version 3 clearer text was off by default and
/// `sharp off` was written all the same, so it was mostly the default nobody chose:
/// it is no choice (as `count on` is no answer); `sharp on` was chosen and stays. So
/// is `bitrate 30`, the default; another quality was chosen. From version 3 on, a
/// `sharp` or `bitrate` line is a choice and there is none without one. 0.1's
/// `monitors.txt` has no version: as before version 3.
pub(super) fn parse_settings(text: &str) -> (Settings, Kept) {
    let mut s = Settings::default();
    let mut kept = Kept::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let value = value.trim();
        let flag = match value {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        let known = match (key, flag) {
            ("version", _) => value.parse().map(|v| kept.version = v).is_ok(),
            ("bitrate", _) => value
                .parse()
                .map(|v| {
                    s.bitrate = bitrate_in_range(v);
                    s.bitrate_chosen = true;
                })
                .is_ok(),
            ("codec", _) => match value {
                "h264" => Some(Codec::H264),
                "h265" => Some(Codec::H265),
                "av1" => Some(Codec::Av1),
                _ => None,
            }
            .map(|c| s.codec = c)
            .is_some(),
            ("sharp", Some(v)) => {
                s.sharp = Some(v);
                true
            }
            ("software", Some(v)) => {
                s.software = v;
                true
            }
            ("audio", Some(v)) => {
                s.audio = v;
                true
            }
            ("mirror", Some(v)) => {
                s.mirror = v;
                true
            }
            ("count", _) => parse_counting(value).map(|c| s.count = c).is_some(),
            ("autostart", Some(v)) => {
                s.autostart = Some(v);
                true
            }
            ("window-theme", _) => match value {
                "system" => Some(prostir_api::ThemeMode::System),
                "light" => Some(prostir_api::ThemeMode::Light),
                "dark" => Some(prostir_api::ThemeMode::Dark),
                _ => None,
            }
            .map(|theme| s.window.theme = theme)
            .is_some(),
            ("window-palette", _) if is_word(value) => {
                value.clone_into(&mut s.window.palette);
                true
            }
            ("window-language", _) if is_word(value) => {
                s.window.language = Some(value.to_owned());
                true
            }
            ("access", _) => match parse_access(value) {
                // A mistyped line for the same headset wins, before or after.
                Some((key, a, false)) if kept.mistyped.contains(&key) => {
                    kept.beside_mistyped.insert(key, a);
                    true
                }
                Some((key, a, false)) => {
                    s.access.insert(key, a);
                    true
                }
                Some((key, a, true)) => {
                    warn!(
                        "settings.txt: \"{line}\" has a word that is no role or right: \
                         that headset gets no rights until it is put right"
                    );
                    if let Some(good) = s.access.insert(key.clone(), a)
                        && !kept.mistyped.contains(&key)
                    {
                        kept.beside_mistyped.insert(key.clone(), good);
                    }
                    kept.mistyped.insert(key);
                    false
                }
                None => false,
            },
            _ => false,
        };
        if !known {
            kept.unknown.push(line.to_owned());
        }
    }
    if kept.version < SHARP_CHOSEN {
        if s.sharp == Some(false) {
            s.sharp = None;
        }
        // The default, written for everyone.
        s.bitrate_chosen &= s.bitrate != Settings::default().bitrate;
    }
    (s, kept)
}

/// The first version of `settings.txt` whose `sharp` and `bitrate` lines are always
/// a choice.
const SHARP_CHOSEN: u32 = 3;

pub(super) fn settings_text(s: &Settings, kept: &Kept) -> String {
    let mut text = format!(
        "{}codec {}\nsoftware {}\naudio {}\nmirror {}\ncount {}\n",
        kept.head(),
        codec_name(s.codec),
        on_off(s.software),
        on_off(s.audio),
        on_off(s.mirror),
        counting_name(s.count),
    );
    if s.bitrate_chosen {
        let _ = writeln!(text, "bitrate {}", s.bitrate);
    }
    if let Some(on) = s.sharp {
        let _ = writeln!(text, "sharp {}", on_off(on));
    }
    if let Some(on) = s.autostart {
        let _ = writeln!(text, "autostart {}", on_off(on));
    }
    let window = prostir_api::WindowLook::default();
    if s.window.theme != window.theme {
        let _ = writeln!(text, "window-theme {}", s.window.theme.name());
    }
    if s.window.palette != window.palette {
        let _ = writeln!(text, "window-palette {}", s.window.palette);
    }
    if let Some(language) = &s.window.language {
        let _ = writeln!(text, "window-language {language}");
    }
    for (key, a) in &s.access {
        // A headset with a mistyped line keeps its other line as it was.
        let a = if kept.mistyped.contains(key) {
            kept.beside_mistyped.get(key)
        } else {
            Some(a)
        };
        if let Some(a) = a {
            text.push_str(&access_text(key, *a));
            text.push('\n');
        }
    }
    text
}
