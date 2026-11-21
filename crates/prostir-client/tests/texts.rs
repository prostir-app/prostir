//! The headset's texts: both built-in languages have every message with the same
//! placeholders, every message the code names exists (and every message is named
//! somewhere), and no text or code uses a long dash.

use std::collections::{BTreeMap, BTreeSet};

const EN: &str = include_str!("../lang/en.ftl");
const UK: &str = include_str!("../lang/uk.ftl");

/// The code that names messages.
const SOURCES: &[(&str, &str)] = &[
    ("ui.rs", include_str!("../src/ui.rs")),
    ("app.rs", include_str!("../src/app.rs")),
    ("net.rs", include_str!("../src/net.rs")),
    ("physical.rs", include_str!("../src/physical.rs")),
    ("lang.rs", include_str!("../src/lang.rs")),
    ("store/mod.rs", include_str!("../src/store/mod.rs")),
    ("store/meta.rs", include_str!("../src/store/meta.rs")),
];

/// The rest of the code, for the dash check.
const OTHER_CODE: &[(&str, &str)] = &[
    ("alloc_count.rs", include_str!("../src/alloc_count.rs")),
    ("audio.rs", include_str!("../src/audio.rs")),
    ("background.rs", include_str!("../src/background.rs")),
    ("commands.rs", include_str!("../src/commands.rs")),
    ("decoder.rs", include_str!("../src/decoder.rs")),
    ("device.rs", include_str!("../src/device.rs")),
    ("egl.rs", include_str!("../src/egl.rs")),
    ("ids.rs", include_str!("../src/ids.rs")),
    ("input.rs", include_str!("../src/input.rs")),
    (
        "layout/arrange.rs",
        include_str!("../src/layout/arrange.rs"),
    ),
    ("layout/cinema.rs", include_str!("../src/layout/cinema.rs")),
    (
        "layout/desktop.rs",
        include_str!("../src/layout/desktop.rs"),
    ),
    ("layout/draw.rs", include_str!("../src/layout/draw.rs")),
    ("layout/grab.rs", include_str!("../src/layout/grab.rs")),
    ("layout/hit.rs", include_str!("../src/layout/hit.rs")),
    ("layout/mod.rs", include_str!("../src/layout/mod.rs")),
    ("layout/saved.rs", include_str!("../src/layout/saved.rs")),
    ("layout/stick.rs", include_str!("../src/layout/stick.rs")),
    ("layout/tests.rs", include_str!("../src/layout/tests.rs")),
    ("lib.rs", include_str!("../src/lib.rs")),
    ("mic.rs", include_str!("../src/mic.rs")),
    ("outbox.rs", include_str!("../src/outbox.rs")),
    ("overlay.rs", include_str!("../src/overlay.rs")),
    ("passthrough.rs", include_str!("../src/passthrough.rs")),
    ("pending.rs", include_str!("../src/pending.rs")),
    ("profiles.rs", include_str!("../src/profiles.rs")),
    ("selftest.rs", include_str!("../src/selftest.rs")),
    (
        "selftest/device.rs",
        include_str!("../src/selftest/device.rs"),
    ),
    ("settings.rs", include_str!("../src/settings.rs")),
    ("system.rs", include_str!("../src/system.rs")),
    ("world/mod.rs", include_str!("../src/world/mod.rs")),
    ("world/tests.rs", include_str!("../src/world/tests.rs")),
    ("xrutil.rs", include_str!("../src/xrutil.rs")),
    (
        "panel_preview.rs",
        include_str!("../examples/panel_preview.rs"),
    ),
];

/// Quoted strings in the code that look like ids but are not messages.
const NOT_MESSAGES: &[&str] = &[
    // Threads.
    "net-writer",
    "selftest-exit",
    // Names and codes in tests.
    "a-much-longer-laptop-name",
    "workstation-with-a-long-name",
    "much-too-long-a-code",
    "no-such-message",
];

/// Messages the code names by a pattern (`weekday-{i}`): named if the pattern is.
const PATTERNS: &[(&str, &str)] = &[("weekday-", "weekday-{i}"), ("month-", "month-{i}")];

#[test]
fn both_languages_have_every_message() {
    for (name, file) in [("en", EN), ("uk", UK)] {
        let r = prostir_lang::check(EN, file);
        assert!(r.is_clean(), "{name}.ftl: {r:?}");
    }
}

#[test]
fn no_long_dashes_anywhere() {
    let files = [("en.ftl", EN), ("uk.ftl", UK)];
    for (name, text) in files.iter().chain(SOURCES).chain(OTHER_CODE) {
        for (n, line) in text.lines().enumerate() {
            assert!(
                !line.contains('\u{2014}') && !line.contains('\u{2013}'),
                "{name}:{}: a long dash: {line}",
                n + 1
            );
        }
    }
}

/// Every `"word-word"` literal in the code, and every `t("word")` and `f("word", ...)`.
fn named_ids() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, src) in SOURCES {
        let pieces: Vec<&str> = src.split('"').collect();
        for (i, piece) in pieces.iter().enumerate() {
            let called =
                i > 0 && (pieces[i - 1].ends_with(".t(") || pieces[i - 1].ends_with(".f("));
            let is_id = i % 2 == 1
                && (piece.contains('-') || called)
                && !piece.is_empty()
                && piece
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && piece.starts_with(|c: char| c.is_ascii_lowercase());
            if is_id && !NOT_MESSAGES.contains(piece) {
                out.insert((*piece).to_owned());
            }
        }
    }
    out
}

fn named_by_pattern(id: &str) -> bool {
    PATTERNS.iter().any(|(prefix, pattern)| {
        id.strip_prefix(prefix)
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            && SOURCES.iter().any(|(_, src)| src.contains(pattern))
    })
}

#[test]
fn the_code_and_the_messages_match() {
    let (messages, errors) = prostir_lang::messages(EN);
    assert!(errors.is_empty(), "{errors:?}");
    let named = named_ids();
    let missing: Vec<_> = named
        .iter()
        .filter(|id| !messages.contains_key(*id))
        .collect();
    assert!(
        missing.is_empty(),
        "named in the code, not in en.ftl: {missing:?}"
    );
    let unused: Vec<_> = messages
        .keys()
        .filter(|id| !named.contains(*id) && *id != prostir_lang::NAME_ID && !named_by_pattern(id))
        .collect();
    assert!(unused.is_empty(), "in en.ftl, never shown: {unused:?}");
}

#[test]
fn placeholders_are_the_same_in_every_language() {
    let vars = |text: &str| -> BTreeSet<String> {
        text.split("{ $")
            .skip(1)
            .filter_map(|s| s.split([' ', '}']).next())
            .map(str::to_owned)
            .collect()
    };
    let per_message = |file: &str| -> BTreeMap<String, BTreeSet<String>> {
        let mut out: Vec<(String, String)> = Vec::new();
        for line in file.lines() {
            match line.split_once(" = ").or_else(|| line.split_once(" =")) {
                Some((id, rest)) if !line.starts_with([' ', '#']) => {
                    out.push((id.trim().to_owned(), rest.to_owned()));
                }
                _ => {
                    if let Some(last) = out.last_mut() {
                        last.1.push_str(line);
                    }
                }
            }
        }
        out.into_iter().map(|(id, t)| (id, vars(&t))).collect()
    };
    let en = per_message(EN);
    for (id, v) in per_message(UK) {
        assert_eq!(Some(&v), en.get(&id), "uk.ftl {id}: placeholders differ");
    }
}

/// The texts name no desktop of the PC, no store, no headset: what the PC and the store
/// are called comes from them.
#[test]
fn the_texts_name_no_desktop_store_or_headset() {
    const NAMES: &[&str] = &[
        "kde",
        "plasma",
        "kwin",
        "kscreen",
        "hyprland",
        "gnome",
        "mutter",
        "sway",
        "pipewire",
        "pulseaudio",
        "meta",
        "oculus",
        "quest",
        "horizon",
        "pico",
        "vive",
        "htc",
    ];
    for (name, file) in [("en.ftl", EN), ("uk.ftl", UK)] {
        for (n, line) in file.lines().enumerate() {
            let words: Vec<String> = line
                .split(|c: char| !c.is_alphanumeric())
                .map(str::to_lowercase)
                .collect();
            for word in NAMES {
                assert!(
                    !words.iter().any(|w| w == word),
                    "{name}:{}: names {word}: {line}",
                    n + 1
                );
            }
        }
    }
}
