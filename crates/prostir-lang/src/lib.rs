//! Translations as data: Fluent (`.ftl`) files, one per language.
//!
//! Every program of Prostir that shows text on the PC (the settings window now, the
//! tray, notifications and `ctl` later) uses the same loader with its own *domain*:
//! the files built into the program, and more files dropped into a folder:
//!
//! - `$XDG_DATA_HOME/prostir/lang/<domain>/<code>.ftl` (`~/.local/share/...`),
//! - `<dir>/prostir/lang/<domain>/<code>.ftl` for each of `$XDG_DATA_DIRS`.
//!
//! A dropped file for a new code adds a language; one for a built-in code overrides
//! that language message by message. Whatever the chosen language lacks comes from
//! English, which is built in and complete; an id English lacks too shows as itself.
//! The file names its language in a message of its own, `language-name`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use fluent_syntax::ast::Entry;
use unic_langid::LanguageIdentifier;
use unicode_segmentation::UnicodeSegmentation;

/// The language every other one falls back to; always built in.
pub const FALLBACK: &str = "en";

/// The message each file names its own language with, in that language.
pub const NAME_ID: &str = "language-name";

/// A translation file larger than this is not read: no language needs a megabyte.
pub const FILE_MAX: u64 = 1 << 20;

/// The bytes of `path` if it is a regular file of at most `max` bytes: a dropped
/// file that is a device, a pipe or huge never stalls or fills a program.
pub fn read_capped(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Read};
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    let too_big = || Error::new(ErrorKind::InvalidData, format!("larger than {max} bytes"));
    if meta.len() > max {
        return Err(too_big());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // The file may grow while it is read.
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(too_big());
    }
    Ok(bytes)
}

/// A value for a placeholder: text, or a number (a number also picks plural forms).
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Text(String),
    Number(f64),
}

impl From<&str> for Arg {
    fn from(s: &str) -> Self {
        Arg::Text(s.to_owned())
    }
}

impl From<String> for Arg {
    fn from(s: String) -> Self {
        Arg::Text(s)
    }
}

impl From<&String> for Arg {
    fn from(s: &String) -> Self {
        Arg::Text(s.clone())
    }
}

macro_rules! number_arg {
    ($($t:ty),*) => {$(
        impl From<$t> for Arg {
            #[allow(trivial_numeric_casts, reason = "one macro for every number type, f64 too")]
            fn from(n: $t) -> Self {
                Arg::Number(n as f64)
            }
        }
    )*};
}
number_arg!(u8, u16, u32, u64, usize, i32, i64, f32, f64);

/// A language there is a file for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    /// The file's name without `.ftl`: `uk`, `en`, `pt-BR`.
    pub code: String,
    /// What the language calls itself (`Українська`), or the code if the file does not say.
    pub name: String,
}

/// What went wrong reading one file; the rest of the file is used anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub what: String,
}

/// All translations of one program: the built-in files and the folders to look in.
pub struct Catalog {
    builtin: Vec<(&'static str, &'static str)>,
    dirs: Vec<PathBuf>,
}

impl Catalog {
    /// `builtin`: `(code, file text)` pairs, English among them. The folders are the
    /// XDG data folders for `domain`.
    pub fn new(domain: &str, builtin: &[(&'static str, &'static str)]) -> Self {
        Self::with_dirs(builtin, data_dirs(domain))
    }

    /// The same with the folders given (tests).
    pub fn with_dirs(builtin: &[(&'static str, &'static str)], dirs: Vec<PathBuf>) -> Self {
        assert!(
            builtin.iter().any(|(code, _)| *code == FALLBACK),
            "English is built in"
        );
        Self {
            builtin: builtin.to_vec(),
            dirs,
        }
    }

    /// The folders searched for dropped files, the most important first.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Every file for `code`, in the order they apply: built in, then the folders from
    /// the least important to the most.
    fn sources(&self, code: &str) -> Vec<(String, Cow<'static, str>)> {
        let mut out = Vec::new();
        if let Some((_, text)) = self.builtin.iter().find(|(c, _)| *c == code) {
            out.push((format!("{code}.ftl (built in)"), Cow::Borrowed(*text)));
        }
        for dir in self.dirs.iter().rev() {
            let path = dir.join(format!("{code}.ftl"));
            match read_capped(&path, FILE_MAX).map(String::from_utf8) {
                Ok(Ok(text)) => out.push((path.display().to_string(), Cow::Owned(text))),
                Ok(Err(_)) => log::warn!("{}: not UTF-8 text, not used", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("{}: not used: {e}", path.display()),
            }
        }
        out
    }

    /// Every language there is a readable file for, by code. A file that cannot be
    /// read (too big, not UTF-8) adds no language: it would only be English again
    /// under another code.
    pub fn languages(&self) -> Vec<Language> {
        let mut codes: Vec<String> = self.builtin.iter().map(|(c, _)| (*c).to_owned()).collect();
        for dir in &self.dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "ftl")
                    && let Some(code) = path.file_stem().and_then(|s| s.to_str())
                    && code.parse::<LanguageIdentifier>().is_ok()
                {
                    codes.push(code.to_owned());
                }
            }
        }
        codes.sort();
        codes.dedup();
        codes
            .into_iter()
            .filter_map(|code| {
                let strings = self.load(&code);
                if strings.code() != code {
                    log::warn!("{code}.ftl: nothing readable in it, not listed");
                    return None;
                }
                let name = strings.own(NAME_ID).unwrap_or_else(|| code.clone());
                Some(Language { code, name })
            })
            .collect()
    }

    /// The strings for `code`, English behind them.
    pub fn load(&self, code: &str) -> Strings {
        let mut problems = Vec::new();
        let fallback = self.bundle(FALLBACK, &mut problems);
        let primary = (code != FALLBACK)
            .then(|| self.bundle(code, &mut problems))
            .filter(|b| b.has_any);
        Strings {
            code: if primary.is_some() { code } else { FALLBACK }.to_owned(),
            primary: primary.map(|b| b.bundle),
            fallback: fallback.bundle,
            problems,
        }
    }

    fn bundle(&self, code: &str, problems: &mut Vec<Problem>) -> Loaded {
        let id: LanguageIdentifier = code.parse().unwrap_or_default();
        let mut bundle = FluentBundle::new_concurrent(vec![id]);
        // No Unicode isolation marks around placeholders: a text renderer without
        // bidirectional text draws them as boxes, and no language here needs them.
        bundle.set_use_isolating(false);
        // NUMBER(), for a file that formats a number its own way.
        if let Err(e) = bundle.add_builtins() {
            log::warn!("Fluent's built-in functions: {e}");
        }
        let mut has_any = false;
        for (file, text) in self.sources(code) {
            let resource = match FluentResource::try_new(text.into_owned()) {
                Ok(r) => r,
                Err((r, errors)) => {
                    for e in errors {
                        problems.push(Problem {
                            file: file.clone(),
                            what: e.to_string(),
                        });
                    }
                    r
                }
            };
            has_any = true;
            bundle.add_resource_overriding(resource);
        }
        Loaded { bundle, has_any }
    }
}

struct Loaded {
    bundle: FluentBundle<FluentResource>,
    has_any: bool,
}

/// The messages of one language, with English behind them.
pub struct Strings {
    code: String,
    primary: Option<FluentBundle<FluentResource>>,
    fallback: FluentBundle<FluentResource>,
    problems: Vec<Problem>,
}

impl Strings {
    /// The language these are, after falling back (`en` when `code` had no file).
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Lines that could not be read in this language's files and English.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The text of `id`.
    pub fn t(&self, id: &str) -> String {
        self.f(id, &[])
    }

    /// The text of `id` with its placeholders (`{ $name }`) filled in.
    pub fn f(&self, id: &str, args: &[(&str, Arg)]) -> String {
        let args = (!args.is_empty()).then(|| {
            let mut a = FluentArgs::new();
            for (k, v) in args {
                match v {
                    Arg::Text(s) => a.set(*k, FluentValue::from(s.clone())),
                    Arg::Number(n) => a.set(*k, FluentValue::from(*n)),
                }
            }
            a
        });
        for bundle in self.primary.iter().chain(std::iter::once(&self.fallback)) {
            if let Some(text) = format(bundle, id, args.as_ref()) {
                return text;
            }
        }
        id.to_owned()
    }

    /// The text of `id` in this language only, without falling back.
    fn own(&self, id: &str) -> Option<String> {
        format(self.primary.as_ref().unwrap_or(&self.fallback), id, None)
    }
}

fn format(
    bundle: &FluentBundle<FluentResource>,
    id: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let pattern = bundle.get_message(id)?.value()?;
    let mut errors = Vec::new();
    Some(
        bundle
            .format_pattern(pattern, args, &mut errors)
            .into_owned(),
    )
}

/// The XDG data folders for `domain`, the most important first.
pub fn data_dirs(domain: &str) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let user = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".local/share")));
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    user.into_iter()
        .chain(
            system
                .split(':')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        )
        .map(|d| d.join("prostir/lang").join(domain))
        .collect()
}

/// The desktop's language among `available`: the first of `LANGUAGE` (a list),
/// `LC_ALL`, `LC_MESSAGES` and `LANG` that is set, as gettext reads them; English if
/// none matches.
pub fn system_language(available: &[&str]) -> String {
    let vars = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"].map(|v| std::env::var(v).ok());
    pick_language(&vars, available)
}

fn pick_language(vars: &[Option<String>], available: &[&str]) -> String {
    let wanted: Vec<&str> = match vars.iter().flatten().find(|s| !s.is_empty()) {
        Some(s) => s.split(':').collect(),
        None => Vec::new(),
    };
    for w in wanted {
        // uk_UA.UTF-8, pt_BR@euro: language and region, without encoding and modifier.
        let tag = w.split(['.', '@']).next().unwrap_or("").replace('_', "-");
        if tag.is_empty() || tag == "C" || tag == "POSIX" {
            continue;
        }
        if let Some(code) = available.iter().find(|c| c.eq_ignore_ascii_case(&tag)) {
            return (*code).to_owned();
        }
        let lang = tag.split('-').next().unwrap_or("");
        if let Some(code) = available.iter().find(|c| c.eq_ignore_ascii_case(lang)) {
            return (*code).to_owned();
        }
    }
    FALLBACK.to_owned()
}

/// What a translation lacks or has extra against a complete one (English): the
/// check behind a test of each built-in file, and later `prostir ctl lang check`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Lines that do not parse.
    pub errors: Vec<String>,
    /// Ids the reference has and this file does not.
    pub missing: Vec<String>,
    /// Ids this file has and the reference does not (typos, old ids).
    pub extra: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.missing.is_empty() && self.extra.is_empty()
    }
}

/// Compares the ids of `file` with those of `reference`, both `.ftl` text.
pub fn check(reference: &str, file: &str) -> Report {
    let (ref_ids, _) = ids(reference);
    let (own, errors) = ids(file);
    Report {
        errors,
        missing: ref_ids
            .keys()
            .filter(|k| !own.contains_key(*k))
            .cloned()
            .collect(),
        extra: own
            .keys()
            .filter(|k| !ref_ids.contains_key(*k))
            .cloned()
            .collect(),
    }
}

/// The message ids of an `.ftl` text with their texts, and its parse errors.
pub fn messages(text: &str) -> (BTreeMap<String, String>, Vec<String>) {
    ids(text)
}

fn ids(text: &str) -> (BTreeMap<String, String>, Vec<String>) {
    let (resource, errors) = match FluentResource::try_new(text.to_owned()) {
        Ok(r) => (r, Vec::new()),
        Err((r, e)) => (r, e.iter().map(ToString::to_string).collect()),
    };
    let ids: Vec<String> = resource
        .entries()
        .filter_map(|e| match e {
            Entry::Message(m) => Some(m.id.name.to_owned()),
            _ => None,
        })
        .collect();
    let mut bundle = FluentBundle::new_concurrent(vec![LanguageIdentifier::default()]);
    bundle.set_use_isolating(false);
    let _ = bundle.add_builtins();
    bundle.add_resource_overriding(resource);
    let mut out = BTreeMap::new();
    for id in ids {
        let Some(msg) = bundle.get_message(&id) else {
            continue;
        };
        let mut texts: Vec<String> = Vec::new();
        let mut errs = Vec::new();
        if let Some(p) = msg.value() {
            texts.push(bundle.format_pattern(p, None, &mut errs).into_owned());
        }
        for a in msg.attributes() {
            texts.push(
                bundle
                    .format_pattern(a.value(), None, &mut errs)
                    .into_owned(),
            );
        }
        out.insert(id, texts.join("\n"));
    }
    (out, errors)
}

/// `name` shortened with an ellipsis until `fits` (a name on a tile too small for it:
/// the PC's map of displays, the headset's map of monitors): its ending kept where that tells
/// similar names apart (`Pro…-3`, a number or a short last word), else its start
/// (`Екран ноу…`). None where not even a letter and the ellipsis fit.
/// Letters are whole graphemes: a letter with its accent stays one.
pub fn shortened(name: &str, fits: impl Fn(&str) -> bool) -> Option<String> {
    let letters: Vec<&str> = name.graphemes(true).collect();
    let n = letters.len();
    let tail = name
        .rfind(['-', ' '])
        .map(|i| name[i..].graphemes(true).count())
        .filter(|t| (2..=5).contains(t) && *t < n);
    // The longest start of at most `max` letters (at least one) that `fits` as `make`
    // makes it, halving: a longer start never fits where a shorter one does not.
    let longest = |make: &dyn Fn(&str) -> String, max: usize| {
        let at = |k: usize| make(&letters[..k].concat());
        if max == 0 || !fits(&at(1)) {
            return None;
        }
        let (mut lo, mut hi) = (1, max);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(&at(mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(at(lo))
    };
    tail.and_then(|t| {
        let end = letters[n - t..].concat();
        longest(&|start| format!("{start}…{end}"), n - t - 1)
    })
    .or_else(|| longest(&|start| format!("{start}…"), n - 1))
}

/// A folder's path shown to a person: `~` for the home folder.
pub fn short_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortened_name_keeps_what_tells_it_apart() {
        let fits = |max: usize| move |s: &str| s.chars().count() <= max;
        assert_eq!(shortened("Prostir-3", fits(6)).as_deref(), Some("Pro…-3"));
        assert_eq!(shortened("Prostir-12", fits(5)).as_deref(), Some("P…-12"));
        // No room for the ending: the start.
        assert_eq!(shortened("Prostir-12", fits(3)).as_deref(), Some("Pr…"));
        assert_eq!(
            shortened("Екран ноутбука", fits(10)).as_deref(),
            Some("Екран ноу…")
        );
        assert_eq!(shortened("Prostir-3", fits(1)), None);
        // A letter with a combining accent stays whole.
        let letters = |max: usize| move |s: &str| s.graphemes(true).count() <= max;
        assert_eq!(
            shortened("Pre\u{301}sent-2", letters(6)).as_deref(),
            Some("Pre\u{301}…-2")
        );
        assert_eq!(
            shortened("Cafe\u{301} du coin", letters(5)).as_deref(),
            Some("Cafe\u{301}…")
        );
    }

    const EN: &str = "language-name = English\nhello = Hello, { $name }\nonly-en = Only here\n\
        files = { $n ->\n    [one] { $n } file\n   *[other] { $n } files\n}\n";
    const UK: &str = "language-name = Українська\nhello = Привіт, { $name }\n\
        files = { $n ->\n    [one] { $n } файл\n    [few] { $n } файли\n   *[many] { $n } файлів\n}\n";

    fn catalog(dirs: Vec<PathBuf>) -> Catalog {
        Catalog::with_dirs(&[("en", EN), ("uk", UK)], dirs)
    }

    #[test]
    fn falls_back_to_english_message_by_message() {
        let s = catalog(vec![]).load("uk");
        assert_eq!(s.code(), "uk");
        assert_eq!(s.f("hello", &[("name", "Олю".into())]), "Привіт, Олю");
        assert_eq!(s.t("only-en"), "Only here");
        assert_eq!(s.t("nowhere"), "nowhere");
    }

    #[test]
    fn plurals_follow_the_language() {
        let s = catalog(vec![]).load("uk");
        assert_eq!(s.f("files", &[("n", 1.into())]), "1 файл");
        assert_eq!(s.f("files", &[("n", 3.into())]), "3 файли");
        assert_eq!(s.f("files", &[("n", 5.into())]), "5 файлів");
        let s = catalog(vec![]).load("en");
        assert_eq!(s.f("files", &[("n", 1.into())]), "1 file");
    }

    #[test]
    fn an_unknown_language_is_english() {
        let s = catalog(vec![]).load("de");
        assert_eq!(s.code(), "en");
        assert_eq!(s.t("only-en"), "Only here");
    }

    #[test]
    fn a_dropped_file_adds_a_language_and_overrides_a_built_in_one() {
        let dir = std::env::temp_dir().join(format!("prostir-lang-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.ftl"),
            "language-name = Deutsch\nhello = Hallo, { $name }\n",
        )
        .unwrap();
        std::fs::write(dir.join("uk.ftl"), "only-en = Тепер і тут\nbroken = {\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a language").unwrap();
        let c = catalog(vec![dir.clone()]);
        let langs = c.languages();
        let names: Vec<_> = langs
            .iter()
            .map(|l| (l.code.as_str(), l.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [("de", "Deutsch"), ("en", "English"), ("uk", "Українська")]
        );
        let de = c.load("de");
        assert_eq!(de.f("hello", &[("name", "Jo".into())]), "Hallo, Jo");
        assert_eq!(de.t("only-en"), "Only here");
        let uk = c.load("uk");
        assert_eq!(uk.t("only-en"), "Тепер і тут");
        assert_eq!(uk.f("hello", &[("name", "Jo".into())]), "Привіт, Jo");
        assert_eq!(uk.problems().len(), 1, "{:?}", uk.problems());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file that cannot be read adds no language (it was listed as a second
    /// English).
    #[test]
    fn an_unreadable_file_adds_no_language() {
        let dir = std::env::temp_dir().join(format!("prostir-lang-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.ftl"),
            b"language-name = Deutsch\nhello = \xff\xfe\n",
        )
        .unwrap();
        let big = format!("language-name = Big\n{}", "x = y\n".repeat(200_000));
        std::fs::write(dir.join("fr.ftl"), big).unwrap();
        std::fs::write(dir.join("pl.ftl"), "language-name = Polski\n").unwrap();
        let langs = catalog(vec![dir.clone()]).languages();
        let codes: Vec<_> = langs.iter().map(|l| l.code.as_str()).collect();
        assert_eq!(codes, ["en", "pl", "uk"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plurals_can_use_number() {
        let c = Catalog::with_dirs(
            &[(
                "en",
                "n = { NUMBER($n) ->\n    [one] one\n   *[other] more\n}\n",
            )],
            Vec::new(),
        );
        assert_eq!(c.load("en").f("n", &[("n", 1.into())]), "one");
        assert_eq!(c.load("en").f("n", &[("n", 3.into())]), "more");
    }

    #[test]
    fn picks_the_desktop_language() {
        let v = |s: &str| vec![None, None, None, Some(s.to_owned())];
        assert_eq!(pick_language(&v("uk_UA.UTF-8"), &["en", "uk"]), "uk");
        assert_eq!(pick_language(&v("de_DE.UTF-8"), &["en", "uk"]), "en");
        assert_eq!(pick_language(&v("pt_BR"), &["en", "pt-BR", "pt"]), "pt-BR");
        assert_eq!(pick_language(&v("C"), &["en", "uk"]), "en");
        let list = vec![
            Some("de:uk".to_owned()),
            None,
            None,
            Some("en_US".to_owned()),
        ];
        assert_eq!(pick_language(&list, &["en", "uk"]), "uk");
        assert_eq!(
            pick_language(&[None, None, None, None], &["en", "uk"]),
            "en"
        );
    }

    #[test]
    fn only_small_regular_files_are_read() {
        let dir = std::env::temp_dir().join(format!("prostir-lang-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.ftl");
        std::fs::write(&small, "a = b\n").unwrap();
        assert_eq!(read_capped(&small, 100).unwrap(), b"a = b\n");
        assert!(read_capped(&small, 3).is_err());
        assert!(read_capped(&dir, 100).is_err(), "a folder");
        assert!(
            read_capped(Path::new("/dev/zero"), 100).is_err(),
            "a device"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn home_is_shown_as_a_tilde() {
        let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
            return;
        };
        let home = PathBuf::from(home);
        assert_eq!(
            short_path(&home.join(".config/prostir")),
            "~/.config/prostir"
        );
        assert_eq!(short_path(&home), "~");
        // A folder that only starts with the same letters is someone else's.
        let other = PathBuf::from(format!("{}x/y", home.display()));
        assert_eq!(short_path(&other), other.display().to_string());
    }

    #[test]
    fn check_finds_missing_and_extra_ids() {
        let r = check(EN, UK);
        assert_eq!(r.missing, ["only-en"]);
        assert!(r.extra.is_empty() && r.errors.is_empty());
        let r = check(UK, "hello = x\nmore = y\nbad = {\n");
        assert_eq!(r.missing, ["files", "language-name"]);
        assert_eq!(r.extra, ["more"]);
        assert_eq!(r.errors.len(), 1);
    }
}
