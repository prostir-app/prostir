//! The headset's texts as data: Fluent files, one per language, read with
//! `prostir-lang` as on the PC (domain `headset`). English and Ukrainian are built
//! in (`lang/en.ftl`, `lang/uk.ftl` at the root of the repository, one file per language
//! for the PC app, the settings window and this app; the headset's messages start with
//! `headset-`); English is complete and stands behind every other language message by
//! message. Another language is a file `<code>.ftl` in
//! `lang/` of the app's external folder (`Android/data/dev.prostir.client/files/lang/`);
//! one for a built-in code replaces that language's messages one by one.
//!
//! The code names messages only (`menu`, `status-connected`); every text a person
//! reads is in the files. [`Texts`] is a loaded language: the messages without
//! placeholders are formatted once when it is loaded, so the menu looks them up and
//! never formats them again; messages with placeholders are formatted when the menu
//! is built, which happens only when what it shows changed.
//!
//! Files are read only by the [`Loader`]: [`start`] before the first frame, then the
//! `system` thread. The frame only decides which language it wants and swaps in a
//! [`Texts`] that came ready (see [`LangState`]). The loader's calls are disallowed in
//! `clippy.toml`, so each caller carries an `expect` that says why it may read
//! files; that the caller is not the frame is for review to see.
//!
//! The menu's font is Noto Sans (Latin, Cyrillic, Greek), drawn without shaping: a
//! language in another script (Arabic, Devanagari, CJK) or written right to left does
//! not show correctly until a font and shaping of its own come (after 0.2.0). The
//! menu's tests list every character the font lacks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub use prostir_lang::Arg;

use crate::settings::DateStyle;

/// The domain of the headset's files among Prostir's translations.
pub const DOMAIN: &str = "headset";

/// What the ids of the headset's messages start with in the shared language files.
pub const PREFIX: &str = "headset-";

/// The files built into the app: English (complete, behind every other one) and
/// Ukrainian.
pub const BUILTIN: &[(&str, &str)] = prostir_lang::BUILTIN;

/// A language's code (`uk`, `en`, `pt-BR`): what `settings.txt` keeps, a file is named
/// by, and the PC and the system say. Short and kept inline, so the settings stay a
/// plain value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Lang {
    len: u8,
    bytes: [u8; Lang::MAX],
}

impl Lang {
    /// The longest code kept (`ca-ES-valencia` is 14).
    pub const MAX: usize = 15;
    /// English: complete and built in, the language everything falls back to.
    pub const FALLBACK: Lang = Lang::from_ascii(b"en");

    const fn from_ascii(code: &[u8]) -> Self {
        let mut bytes = [0; Lang::MAX];
        let mut i = 0;
        while i < code.len() {
            bytes[i] = code[i];
            i += 1;
        }
        Self {
            len: code.len() as u8,
            bytes,
        }
    }

    /// A code of letters, digits and `-`, up to [`Lang::MAX`] long; `None` otherwise.
    pub fn new(code: &str) -> Option<Self> {
        let code = code.trim();
        let fits = !code.is_empty()
            && code.len() <= Self::MAX
            && code.starts_with(|c: char| c.is_ascii_alphabetic())
            && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        fits.then(|| Self::from_ascii(code.as_bytes()))
    }

    /// From a tag the system or the PC says (`uk_UA.UTF-8`, `en-US`, `pt_BR@euro`): the
    /// language and the region, without the encoding and the modifier.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.trim().split(['.', '@']).next().unwrap_or("");
        if tag.is_empty() || tag == "C" || tag == "POSIX" {
            return None;
        }
        Self::new(&tag.replace('_', "-"))
    }

    pub fn as_str(&self) -> &str {
        // Only ASCII is ever stored.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("en")
    }

    /// The language without its region: `pt` of `pt-BR`.
    fn primary(&self) -> &str {
        self.as_str().split('-').next().unwrap_or("")
    }
}

impl std::fmt::Debug for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The language chosen in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// The connected PC's language (the last one seen; the headset's before any).
    #[default]
    LikePc,
    /// The headset's own system language.
    LikeSystem,
    Fixed(Lang),
}

/// A language there is a file for, and what it calls itself (`Українська`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub lang: Lang,
    pub name: String,
}

/// The language the menu shows for `choice` among the `known` ones: the PC's for
/// "like the PC" (English when there is no file for it; the system's before the PC
/// said), the system's for "like the system", else the chosen one (as "like the PC"
/// when there is no file for it, see [`kept`]); English when nothing else is found.
pub fn resolve(choice: Language, pc: Option<Lang>, system: Option<Lang>, known: &[Known]) -> Lang {
    let pick = |l: Option<Lang>| l.and_then(|l| matching(l, known));
    match kept(choice, known) {
        Language::Fixed(l) => pick(Some(l)),
        Language::LikeSystem => pick(system),
        Language::LikePc if pc.is_some() => pick(pc),
        Language::LikePc => pick(system),
    }
    .unwrap_or(Lang::FALLBACK)
}

/// `choice` as it stands with the `known` languages: a fixed language there is no
/// file for is "like the PC", as an unknown word in `settings.txt` always was.
pub fn kept(choice: Language, known: &[Known]) -> Language {
    match choice {
        Language::Fixed(l) if matching(l, known).is_none() => Language::LikePc,
        other => other,
    }
}

/// The known language for `want`: the same code (`pt-BR`), else the same language
/// without the region (`uk` for `uk-UA`).
fn matching(want: Lang, known: &[Known]) -> Option<Lang> {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    known
        .iter()
        .find(|k| same(k.lang.as_str(), want.as_str()))
        .or_else(|| known.iter().find(|k| same(k.lang.as_str(), want.primary())))
        .map(|k| k.lang)
}

/// A moment as the menu writes it: the local date and time, from the clock the shell
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub year: i32,
    /// 0 is January.
    pub month: u8,
    pub day: u8,
    /// 0 is Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
}

impl Moment {
    /// The moment the date and time choices show themselves with: Wednesday,
    /// 18 November 2026, 14:05.
    pub const EXAMPLE: Moment = Moment {
        year: 2026,
        month: 10,
        day: 18,
        weekday: 3,
        hour: 14,
        minute: 5,
    };
}

/// Stand-ins for what a date fills in, formatted into the language's pattern once:
/// characters of Unicode's private use area, which no text has.
const MARK_WEEKDAY: char = '\u{E000}';
const MARK_DAY: char = '\u{E001}';
const MARK_MONTH: char = '\u{E002}';
const MARK_TIME: char = '\u{E003}';

/// One language's words for the date and the 12-hour clock.
#[derive(Debug)]
struct Calendar {
    /// `date` with stand-ins for its parts.
    date: String,
    weekdays: [String; 7],
    months: [String; 12],
    /// `time-12h` with a stand-in for the time, before noon and after.
    am: String,
    pm: String,
}

impl Calendar {
    fn of(strings: &prostir_lang::Strings) -> Self {
        let mark = |c: char| Arg::Text(c.to_string());
        let half = |half: &str| {
            strings.f(
                "headset-time-12h",
                &[("time", mark(MARK_TIME)), ("half", half.into())],
            )
        };
        Self {
            date: strings.f(
                "headset-date-format",
                &[
                    ("weekday", mark(MARK_WEEKDAY)),
                    ("day", mark(MARK_DAY)),
                    ("month", mark(MARK_MONTH)),
                ],
            ),
            weekdays: std::array::from_fn(|i| strings.t(&format!("headset-weekday-{i}"))),
            months: std::array::from_fn(|i| strings.t(&format!("headset-month-{i}"))),
            am: half("am"),
            pm: half("pm"),
        }
    }

    fn date(&self, m: &Moment) -> String {
        let mut out = String::with_capacity(self.date.len() + 16);
        for c in self.date.chars() {
            match c {
                MARK_WEEKDAY => out.push_str(pick(&self.weekdays, m.weekday)),
                MARK_MONTH => out.push_str(pick(&self.months, m.month)),
                MARK_DAY => {
                    use std::fmt::Write as _;
                    let _ = write!(out, "{}", m.day);
                }
                c => out.push(c),
            }
        }
        out
    }

    fn clock_12h(&self, m: &Moment) -> String {
        use std::fmt::Write as _;
        let hour = match m.hour % 12 {
            0 => 12,
            h => h,
        };
        let template = if m.hour < 12 { &self.am } else { &self.pm };
        let mut out = String::with_capacity(template.len() + 8);
        for c in template.chars() {
            if c == MARK_TIME {
                let _ = write!(out, "{hour}:{:02}", m.minute);
            } else {
                out.push(c);
            }
        }
        out
    }
}

fn pick<const N: usize>(words: &[String; N], i: u8) -> &str {
    words.get(usize::from(i)).map_or("", String::as_str)
}

/// The calendars of the two date styles that are always the same language.
#[derive(Debug)]
struct Fixed {
    uk: Calendar,
    en: Calendar,
}

/// One language loaded, ready for the menu: cheap to clone (one shared copy), equal
/// only to itself (the menu is built again when another one comes).
#[derive(Clone)]
pub struct Texts(Arc<Loaded>);

struct Loaded {
    lang: Lang,
    strings: prostir_lang::Strings,
    /// Every message of English's formatted once, in this language.
    plain: HashMap<String, String>,
    calendar: Calendar,
    fixed: Arc<Fixed>,
    /// What the person is told about the files, in this language: lines that could
    /// not be read, or no file at all for the language.
    warning: Option<String>,
}

impl PartialEq for Texts {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Texts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Texts({})", self.0.lang)
    }
}

impl Texts {
    fn new(lang: Lang, strings: prostir_lang::Strings, fixed: Arc<Fixed>) -> Self {
        let (ids, _) = prostir_lang::messages(english());
        // The headset's own messages: the file is shared with the PC app and the window.
        let plain = ids
            .into_keys()
            .filter(|id| id.starts_with(PREFIX) || id == prostir_lang::NAME_ID)
            .map(|id| {
                let text = strings.t(&id);
                (id, text)
            })
            .collect();
        let calendar = Calendar::of(&strings);
        let language = || [("language", Arg::from(lang.as_str()))];
        let warning = if strings.code() != lang.as_str() && lang != Lang::FALLBACK {
            Some(strings.f("headset-language-missing", &language()))
        } else if !strings.problems().is_empty() {
            Some(strings.f("headset-language-problems", &language()))
        } else {
            None
        };
        Self(Arc::new(Loaded {
            lang,
            strings,
            plain,
            calendar,
            fixed,
            warning,
        }))
    }

    /// What to tell the person about this language's files, if anything (formatted
    /// when it was loaded).
    pub fn warning(&self) -> Option<&str> {
        self.0.warning.as_deref()
    }

    /// The language these are (as asked for; what it lacks is English).
    pub fn lang(&self) -> Lang {
        self.0.lang
    }

    /// The text of message `id`, formatted when the language was loaded; `id` itself
    /// when no file has it.
    pub fn t<'a>(&'a self, id: &'a str) -> &'a str {
        self.0.plain.get(id).map_or(id, String::as_str)
    }

    /// The text of message `id` with its placeholders filled in (a number also picks
    /// the plural form).
    pub fn f(&self, id: &str, args: &[(&str, Arg)]) -> String {
        self.0.strings.f(id, args)
    }

    /// The date of `m` as `style` writes it (`Ср, 18 лист.`, `Wed, Nov 18`, `18.11.2026`).
    pub fn date(&self, style: DateStyle, m: &Moment) -> String {
        match style {
            DateStyle::LikeMenu => self.0.calendar.date(m),
            DateStyle::Ukrainian => self.0.fixed.uk.date(m),
            DateStyle::English => self.0.fixed.en.date(m),
            DateStyle::Numbers => format!("{:02}.{:02}.{}", m.day, u32::from(m.month) + 1, m.year),
        }
    }

    /// The time of `m`: `14:05`, or on the 12-hour clock `2:05 PM`.
    pub fn clock(&self, twelve: bool, m: &Moment) -> String {
        if twelve {
            self.0.calendar.clock_12h(m)
        } else {
            format!("{:02}:{:02}", m.hour, m.minute)
        }
    }
}

fn english() -> &'static str {
    BUILTIN
        .iter()
        .find(|(code, _)| *code == prostir_lang::FALLBACK)
        .map_or("", |(_, text)| text)
}

/// Reads the language files: the built-in ones and those in the app's folder. It reads
/// the disk: [`start`] uses it before the first frame, the `system` thread after.
pub struct Loader {
    builtin: Vec<(&'static str, &'static str)>,
    dirs: Vec<PathBuf>,
    fixed: Option<Arc<Fixed>>,
    /// The files the list was made from (name, size, time changed) and the list:
    /// the files are read again only when these change.
    listed: Option<(Vec<FileStamp>, Vec<Known>)>,
}

type FileStamp = (PathBuf, u64, Option<std::time::SystemTime>);

/// The menu's language at the start, before the first frame: the files there are
/// (built in, and in `dir`), and the language `choice` wants among them, loaded.
/// From here on the `system` thread keeps the loader.
#[expect(
    clippy::disallowed_methods,
    reason = "the start, before the first frame, reads the language files once"
)]
pub fn start(
    dir: Option<PathBuf>,
    choice: Language,
    pc: Option<Lang>,
    system: Option<Lang>,
) -> (LangState, Loader) {
    let mut loader = Loader::new(dir);
    let known = loader.languages();
    let first = resolve(choice, pc, system, &known);
    let state = LangState::new(loader.load(first), known);
    (state, loader)
}

impl Loader {
    /// The built-in files and those in `dir` (`files/lang/`; none in the selftest).
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self::with(BUILTIN, dir.into_iter().collect())
    }

    /// Other built-in files (English among them) and folders: tests and previews.
    pub fn with(builtin: &[(&'static str, &'static str)], dirs: Vec<PathBuf>) -> Self {
        Self {
            builtin: builtin.to_vec(),
            dirs,
            fixed: None,
            listed: None,
        }
    }

    fn catalog(&self) -> prostir_lang::Catalog {
        prostir_lang::Catalog::with_dirs(&self.builtin, self.dirs.clone())
    }

    /// Every language there is a file for, by code; codes longer than [`Lang::MAX`]
    /// are left out. The files are read again only when one was added, removed or
    /// changed since the last time.
    #[expect(
        clippy::disallowed_methods,
        reason = "the loader is the one that reads the language files"
    )]
    pub fn languages(&mut self) -> Vec<Known> {
        self.languages_if_changed()
            .or_else(|| self.listed.as_ref().map(|(_, known)| known.clone()))
            .unwrap_or_default()
    }

    /// The languages, if the files changed since they were last listed (or never were).
    #[expect(
        clippy::disallowed_methods,
        reason = "the loader is the one that reads the language files"
    )]
    pub fn languages_if_changed(&mut self) -> Option<Vec<Known>> {
        let mut stamps: Vec<FileStamp> = Vec::new();
        for dir in &self.dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "ftl") {
                    let info = entry.metadata().ok();
                    let size = info.as_ref().map_or(0, std::fs::Metadata::len);
                    let changed = info.and_then(|m| m.modified().ok());
                    stamps.push((path, size, changed));
                }
            }
        }
        stamps.sort();
        if self
            .listed
            .as_ref()
            .is_some_and(|(seen, _)| *seen == stamps)
        {
            return None;
        }
        let known: Vec<Known> = self
            .catalog()
            .languages()
            .into_iter()
            .filter_map(|l| match Lang::new(&l.code) {
                Some(lang) => Some(Known { lang, name: l.name }),
                None => {
                    log::warn!("language file {}.ftl: the code is too long", l.code);
                    None
                }
            })
            .collect();
        self.listed = Some((stamps, known.clone()));
        Some(known)
    }

    /// The texts of `lang`, English behind them.
    #[expect(
        clippy::disallowed_methods,
        reason = "the loader is the one that reads the language files"
    )]
    pub fn load(&mut self, lang: Lang) -> Texts {
        let strings = self.catalog().load(lang.as_str());
        for p in strings.problems() {
            log::warn!("{}: {}", p.file, p.what);
        }
        let fixed = self
            .fixed
            .get_or_insert_with(|| {
                // The built-in files only: the same words whatever was dropped in.
                let builtin = prostir_lang::Catalog::with_dirs(BUILTIN, Vec::new());
                Arc::new(Fixed {
                    uk: Calendar::of(&builtin.load("uk")),
                    en: Calendar::of(&builtin.load("en")),
                })
            })
            .clone();
        Texts::new(lang, strings, fixed)
    }
}

/// The built-in texts of `code` (English when it has none), for tests. Reads no file.
#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a loader of the built-in files only, with no folder to read"
)]
pub fn builtin(code: &str) -> Texts {
    Loader::with(BUILTIN, Vec::new()).load(Lang::new(code).unwrap_or(Lang::FALLBACK))
}

/// The language the menu shows and the one it waits for. The frame only decides and
/// swaps: the files are read by the [`Loader`] on the `system` thread, and the menu
/// shows the language it has until the new one comes ready.
#[derive(Debug)]
pub struct LangState {
    texts: Texts,
    known: Arc<[Known]>,
    asked: Option<Lang>,
}

impl LangState {
    /// What the start loaded.
    pub fn new(texts: Texts, known: Vec<Known>) -> Self {
        Self {
            texts,
            known: known.into(),
            asked: None,
        }
    }

    pub fn texts(&self) -> &Texts {
        &self.texts
    }

    /// The languages there are files for.
    pub fn known(&self) -> &Arc<[Known]> {
        &self.known
    }

    /// The language `choice` asks for now, if it is neither shown nor already asked
    /// for: the `system` thread is to load it.
    pub fn wanted(
        &mut self,
        choice: Language,
        pc: Option<Lang>,
        system: Option<Lang>,
    ) -> Option<Lang> {
        let want = resolve(choice, pc, system, &self.known);
        if want == self.texts.lang() {
            self.asked = None;
            return None;
        }
        if self.asked == Some(want) {
            return None;
        }
        self.asked = Some(want);
        Some(want)
    }

    /// A language came loaded: shown if it is the one asked for last (not one the
    /// menu was switched away from meanwhile); whether it is.
    pub fn loaded(&mut self, texts: Texts) -> bool {
        if self.asked != Some(texts.lang()) {
            return false;
        }
        self.asked = None;
        self.texts = texts;
        true
    }

    /// The languages there are files for, listed again (a file may have been added);
    /// whether the list changed.
    pub fn listed(&mut self, known: Vec<Known>) -> bool {
        if *self.known == *known {
            return false;
        }
        self.known = known.into();
        true
    }
}

/// A longer pseudo-language made of `english` (its `.ftl` text): every text half as
/// long again, its letters with accents, the placeholders as they are. The menu is
/// checked with it for texts that do not fit.
pub fn pseudo(english: &str) -> String {
    let mut out = String::with_capacity(english.len() * 2);
    for line in english.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
        } else if line.starts_with(prostir_lang::NAME_ID) {
            out.push_str("language-name = Pseudo");
        } else if let Some((head, text)) = split_text(line) {
            out.push_str(head);
            out.push_str(&longer(text));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A line's text after `id = ` or a variant's `[key] `; none for a line that only
/// opens or closes a selector.
fn split_text(line: &str) -> Option<(&str, &str)> {
    let at = if line.starts_with(|c: char| c.is_ascii_lowercase()) {
        line.find(" = ")? + 3
    } else if let Some(open) = line
        .find('[')
        .filter(|_| line.trim_start().starts_with(['[', '*']))
    {
        open + line[open..].find(']')? + 2
    } else {
        0
    };
    let text = line.get(at..)?;
    let bare = text.trim();
    if bare.is_empty() || bare == "}" || bare.ends_with("->") {
        return None;
    }
    Some((&line[..at], text))
}

/// `text` with accents on its letters and half its own length again added, outside
/// the `{ ... }` placeholders.
fn longer(text: &str) -> String {
    let accent = |c: char| match c {
        'a' => 'á',
        'e' => 'é',
        'i' => 'í',
        'o' => 'ó',
        'u' => 'ú',
        'y' => 'ý',
        'n' => 'ñ',
        'c' => 'ç',
        's' => 'š',
        'z' => 'ž',
        'A' => 'Á',
        'E' => 'É',
        'I' => 'Í',
        'O' => 'Ó',
        'U' => 'Ú',
        'S' => 'Š',
        c => c,
    };
    let mut plain = String::new();
    let mut out = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 && c != '}' {
            let c = accent(c);
            out.push(c);
            plain.push(c);
        } else {
            out.push(c);
        }
    }
    // Half as many characters again, from its own letters, in words of five (a file
    // trims a text's end, so it never ends in a space).
    let extra = plain.chars().count().div_ceil(2);
    let mut letters = plain.chars().filter(|c| c.is_alphanumeric()).cycle();
    for i in 0..extra {
        let c = if i % 6 == 5 {
            Some(' ')
        } else {
            letters.next()
        };
        out.push(if i == 0 { ' ' } else { c.unwrap_or('x') });
    }
    if out.ends_with(' ') {
        out.push(letters.next().unwrap_or('x'));
    }
    out
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests make the language folders the loader reads, and load as the system thread does"
)]
mod tests {
    use super::*;

    const EN: &str = BUILTIN[0].1;
    const UK: &str = BUILTIN[1].1;

    #[test]
    fn codes() {
        assert_eq!(
            Lang::from_tag("uk_UA.UTF-8").map(|l| l.to_string()),
            Some("uk-UA".into())
        );
        assert_eq!(Lang::from_tag("en-US").unwrap().as_str(), "en-US");
        assert_eq!(Lang::from_tag(""), None);
        assert_eq!(Lang::from_tag("C"), None);
        assert_eq!(Lang::new("uk").unwrap().as_str(), "uk");
        assert_eq!(
            Lang::new("ca-ES-valencia").unwrap().as_str(),
            "ca-ES-valencia"
        );
        assert_eq!(Lang::new("much-too-long-a-code"), None);
        assert_eq!(Lang::new("../uk"), None);
        assert_eq!(Lang::FALLBACK.as_str(), "en");
    }

    fn known(codes: &[&str]) -> Vec<Known> {
        codes
            .iter()
            .map(|c| Known {
                lang: Lang::new(c).unwrap(),
                name: (*c).to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_menu_language() {
        let l = |c: &str| Lang::new(c);
        let k = known(&["de", "en", "uk"]);
        let r = |choice, pc: &str, system: &str| resolve(choice, l(pc), l(system), &k).to_string();
        assert_eq!(r(Language::LikePc, "uk", "en-US"), "uk");
        assert_eq!(
            r(Language::LikePc, "uk-UA", "en"),
            "uk",
            "without the region"
        );
        // A PC in a language without a file: English, as before.
        assert_eq!(r(Language::LikePc, "fr", "uk"), "en");
        // No PC seen yet: the headset's.
        assert_eq!(r(Language::LikePc, "", "uk-UA"), "uk");
        assert_eq!(r(Language::LikePc, "", ""), "en");
        assert_eq!(r(Language::LikeSystem, "uk", "de-AT"), "de");
        assert_eq!(
            r(Language::Fixed(Lang::new("de").unwrap()), "uk", "uk"),
            "de"
        );
        // The file of a chosen language is gone: like the PC.
        assert_eq!(
            r(Language::Fixed(Lang::new("pl").unwrap()), "uk", "en"),
            "uk"
        );
        assert_eq!(
            kept(Language::Fixed(Lang::new("pl").unwrap()), &k),
            Language::LikePc
        );
        let de = Language::Fixed(Lang::new("de").unwrap());
        assert_eq!(kept(de, &k), de);
    }

    /// Both built-in files have every message, and nothing else.
    #[test]
    fn both_files_have_the_same_messages() {
        for (name, file) in [("en", EN), ("uk", UK)] {
            let r = prostir_lang::check(EN, file);
            assert!(r.is_clean(), "{name}.ftl: {r:?}");
        }
    }

    /// The plural forms are Fluent's, by the language's rules.
    #[test]
    fn plurals() {
        let uk = builtin("uk");
        let at_most = |t: &Texts, n: u32| t.f("headset-monitors-at-most", &[("count", n.into())]);
        assert_eq!(at_most(&uk, 1), "Максимум 1 монітор");
        assert_eq!(at_most(&uk, 3), "Максимум 3 монітори");
        assert_eq!(at_most(&uk, 6), "Максимум 6 моніторів");
        assert_eq!(at_most(&uk, 21), "Максимум 21 монітор");
        let en = builtin("en");
        assert_eq!(at_most(&en, 1), "At most 1 monitor");
        assert_eq!(at_most(&en, 6), "At most 6 monitors");
    }

    #[test]
    fn dates_and_times() {
        let m = Moment::EXAMPLE;
        let uk = builtin("uk");
        let en = builtin("en");
        assert_eq!(uk.date(DateStyle::LikeMenu, &m), "Ср, 18 лист.");
        assert_eq!(en.date(DateStyle::LikeMenu, &m), "Wed, Nov 18");
        // The fixed styles are the same in every language.
        assert_eq!(en.date(DateStyle::Ukrainian, &m), "Ср, 18 лист.");
        assert_eq!(uk.date(DateStyle::English, &m), "Wed, Nov 18");
        assert_eq!(uk.date(DateStyle::Numbers, &m), "18.11.2026");
        assert_eq!(uk.clock(false, &m), "14:05");
        assert_eq!(en.clock(true, &m), "2:05 PM");
        let night = Moment {
            hour: 0,
            minute: 7,
            ..m
        };
        assert_eq!(uk.clock(true, &night), "12:07 AM");
        assert_eq!(uk.clock(false, &night), "00:07");
        let sunday = Moment {
            weekday: 0,
            month: 0,
            day: 4,
            ..m
        };
        assert_eq!(uk.date(DateStyle::LikeMenu, &sunday), "Нд, 4 січ.");
        assert_eq!(en.date(DateStyle::LikeMenu, &sunday), "Sun, Jan 4");
    }

    /// A message missing from a language comes from English; one missing everywhere
    /// shows its id.
    #[test]
    fn missing_messages() {
        let dir = std::env::temp_dir().join(format!("prostir-headset-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.ftl"),
            "language-name = Deutsch\nheadset-menu = Menü\n",
        )
        .unwrap();
        let mut loader = Loader::new(Some(dir.clone()));
        let names: Vec<_> = loader.languages().into_iter().map(|k| k.name).collect();
        assert_eq!(names, ["Deutsch", "English", "Українська"]);
        let de = loader.load(Lang::new("de").unwrap());
        assert_eq!(de.t("headset-menu"), "Menü");
        assert_eq!(de.t("headset-tab-settings"), "Settings");
        assert_eq!(de.t("no-such-message"), "no-such-message");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Choosing a language in the menu only asks for it, once, and the menu goes on
    /// showing what it has (the files may even be gone meanwhile); what the `system`
    /// thread loaded is then swapped in.
    #[test]
    fn a_language_switch_only_asks_and_swaps() {
        let dir =
            std::env::temp_dir().join(format!("prostir-headset-switch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.ftl"),
            "language-name = Deutsch\nheadset-menu = Menü\n",
        )
        .unwrap();
        // The start: the list and the first language.
        let mut loader = Loader::new(Some(dir.clone()));
        let mut state = LangState::new(loader.load(Lang::FALLBACK), loader.languages());
        // The files go away: whatever the frame does now, it cannot read them.
        std::fs::remove_dir_all(&dir).unwrap();
        let de = Lang::new("de").unwrap();
        let choice = Language::Fixed(de);
        assert_eq!(state.wanted(choice, None, None), Some(de), "asked once");
        assert_eq!(state.wanted(choice, None, None), None, "not again");
        assert_eq!(
            state.texts().t("headset-menu"),
            "Menu",
            "the language it has"
        );
        // What the system thread loaded (from the file it read back then).
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.ftl"),
            "language-name = Deutsch\nheadset-menu = Menü\n",
        )
        .unwrap();
        let loaded = loader.load(de);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(state.loaded(loaded));
        assert_eq!(state.texts().t("headset-menu"), "Menü");
        assert_eq!(state.wanted(choice, None, None), None, "shown");
        // Back to English: asked again; German, loaded late, is not shown.
        let en = Language::Fixed(Lang::FALLBACK);
        assert_eq!(state.wanted(en, None, None), Some(Lang::FALLBACK));
        assert!(!state.loaded(builtin("de")));
        assert_eq!(state.texts().t("headset-menu"), "Menü");
    }

    /// The list is made again only when a file was added, removed or changed; a file
    /// that cannot be read is no language.
    #[test]
    fn the_list_is_read_again_only_when_files_change() {
        let dir = std::env::temp_dir().join(format!("prostir-headset-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut loader = Loader::new(Some(dir.clone()));
        let codes =
            |known: &[Known]| -> Vec<String> { known.iter().map(|k| k.lang.to_string()).collect() };
        assert_eq!(codes(&loader.languages()), ["en", "uk"]);
        assert_eq!(loader.languages_if_changed(), None, "nothing changed");
        std::fs::write(dir.join("de.ftl"), "language-name = Deutsch\n").unwrap();
        std::fs::write(dir.join("fr.ftl"), b"language-name = \xff\n").unwrap();
        let known = loader.languages_if_changed().expect("a file was added");
        assert_eq!(codes(&known), ["de", "en", "uk"], "French is not readable");
        assert_eq!(loader.languages_if_changed(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A language whose file has lines that cannot be read says so.
    #[test]
    fn a_broken_file_is_told() {
        let dir =
            std::env::temp_dir().join(format!("prostir-headset-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.ftl"),
            "language-name = Deutsch\nheadset-menu = {\n",
        )
        .unwrap();
        let mut loader = Loader::new(Some(dir.clone()));
        let de = loader.load(Lang::new("de").unwrap());
        assert!(
            de.warning().is_some_and(|w| w.contains("de")),
            "{:?}",
            de.warning()
        );
        assert_eq!(loader.load(Lang::FALLBACK).warning(), None);
        let gone = loader.load(Lang::new("pl").unwrap());
        assert!(gone.warning().is_some(), "no file for it");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The pseudo-language has every message, half as long again, with the same
    /// placeholders.
    #[test]
    fn the_pseudo_language_is_longer() {
        let qps = pseudo(EN);
        let r = prostir_lang::check(EN, &qps);
        assert!(r.is_clean(), "{r:?}");
        let (en, _) = prostir_lang::messages(EN);
        let (long, _) = prostir_lang::messages(&qps);
        // Without the placeholders, which stay as they are.
        let words = |t: &str| {
            let mut depth = 0;
            t.chars()
                .filter(|c| {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => return depth == 0,
                    }
                    false
                })
                .count()
        };
        for (id, text) in en.iter().filter(|(id, _)| id.starts_with(PREFIX)) {
            let n = words(text);
            let m = words(&long[id]);
            assert!(
                m * 2 >= n * 3 || id == prostir_lang::NAME_ID,
                "{id}: {text:?} {:?}",
                long[id]
            );
        }
    }
}
