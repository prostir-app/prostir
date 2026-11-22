//! The headset's code names no desktop of the PC, no sound system of the PC, and no
//! maker, headset or store outside the places that are about them (the step 7 note,
//! 13.1): the maker's extensions, the store's implementation, the controllers' table.
//! Each such place is a line of `neutrality-allow.txt`, so a new one shows in review.
//!
//! Words are matched whole (letters, digits and `_` make a word, any case), as
//! `grep -wi` would: `XR_FB_passthrough` is one word, `meta::` is the word `meta`.

#![expect(
    clippy::disallowed_methods,
    reason = "a test reads the crate's own sources"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The names: desktops and sound systems of the PC, makers, headsets and stores.
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
    "pactl",
    "wasapi",
    "meta",
    "oculus",
    "quest",
    "horizon",
    "pico",
    "vive",
    "htc",
    "bytedance",
    "samsung",
    "valve",
    "steamvr",
];

/// The folders looked through, from the crate's own.
const LOOKED_AT: &[&str] = &["src", "assets", "lang", "tests"];

/// The allowed places: (path, word); a path ending in `/` stands for all below it, the
/// word `*` for every name.
fn allowed(root: &Path) -> Vec<(String, String)> {
    let text =
        std::fs::read_to_string(root.join("neutrality-allow.txt")).expect("neutrality-allow.txt");
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (place, why) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("neutrality-allow.txt:{}: no reason", n + 1));
        assert!(
            !why.trim().is_empty(),
            "neutrality-allow.txt:{}: no reason",
            n + 1
        );
        let mut parts = place.split_whitespace();
        let (Some(path), Some(word), None) = (parts.next(), parts.next(), parts.next()) else {
            panic!("neutrality-allow.txt:{}: not `<path> <word>: <why>`", n + 1);
        };
        out.push((path.to_owned(), word.to_lowercase()));
    }
    out
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn words(line: &str) -> impl Iterator<Item = String> + '_ {
    line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

#[test]
fn no_desktop_maker_or_store_outside_their_places() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed = allowed(root);
    let mut used = BTreeSet::new();
    let mut found = Vec::new();
    let mut all = Vec::new();
    for dir in LOOKED_AT {
        files(&root.join(dir), &mut all);
    }
    all.sort();
    for path in all {
        // Fonts and clips are not text.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .expect("below the crate")
            .to_string_lossy()
            .replace('\\', "/");
        for (n, line) in text.lines().enumerate() {
            for word in words(line) {
                if !NAMES.contains(&word.as_str()) {
                    continue;
                }
                let place = allowed.iter().position(|(p, w)| {
                    (*w == word || w == "*")
                        && (rel == *p || (p.ends_with('/') && rel.starts_with(p.as_str())))
                });
                match place {
                    Some(i) => {
                        used.insert(i);
                    }
                    None => found.push(format!("{rel}:{}: {word}: {}", n + 1, line.trim())),
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "names outside their places (or add a line with the reason to \
         neutrality-allow.txt):\n{}",
        found.join("\n")
    );
    // A line nothing needs any more goes, so the list stays the true one.
    let stale: Vec<_> = allowed
        .iter()
        .enumerate()
        .filter(|(i, (p, _))| !used.contains(i) && LOOKED_AT.iter().any(|d| p.starts_with(d)))
        .map(|(_, (p, w))| format!("{p} {w}"))
        .collect();
    assert!(
        stale.is_empty(),
        "lines in neutrality-allow.txt nothing needs: {stale:?}"
    );
}
