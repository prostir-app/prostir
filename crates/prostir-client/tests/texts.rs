//! The headset's texts: no code uses a long dash, and the texts name no desktop, store or
//! headset. That both languages have every message with the same placeholders
//! and that the code names exactly the messages there are is checked for all programs
//! together by `prostir-lang`'s `tests/files.rs`.

const EN: &str = prostir_lang::BUILTIN[0].1;
const UK: &str = prostir_lang::BUILTIN[1].1;

/// The code, for the dash check.
const SOURCES: &[(&str, &str)] = &[
    ("ui.rs", include_str!("../src/ui.rs")),
    ("note.rs", include_str!("../src/note.rs")),
    ("app/mod.rs", include_str!("../src/app/mod.rs")),
    ("app/session.rs", include_str!("../src/app/session.rs")),
    ("app/hands.rs", include_str!("../src/app/hands.rs")),
    (
        "app/actions/mod.rs",
        include_str!("../src/app/actions/mod.rs"),
    ),
    (
        "app/actions/look.rs",
        include_str!("../src/app/actions/look.rs"),
    ),
    (
        "app/actions/monitors.rs",
        include_str!("../src/app/actions/monitors.rs"),
    ),
    (
        "app/actions/pc.rs",
        include_str!("../src/app/actions/pc.rs"),
    ),
    (
        "app/actions/profiles.rs",
        include_str!("../src/app/actions/profiles.rs"),
    ),
    (
        "app/actions/settings.rs",
        include_str!("../src/app/actions/settings.rs"),
    ),
    ("main_loop.rs", include_str!("../src/main_loop.rs")),
    ("view.rs", include_str!("../src/view.rs")),
    (
        "interaction/mod.rs",
        include_str!("../src/interaction/mod.rs"),
    ),
    ("net/mod.rs", include_str!("../src/net/mod.rs")),
    ("net/link.rs", include_str!("../src/net/link.rs")),
    ("net/discovery.rs", include_str!("../src/net/discovery.rs")),
    ("physical.rs", include_str!("../src/physical.rs")),
    ("session/mod.rs", include_str!("../src/session/mod.rs")),
    ("lang.rs", include_str!("../src/lang.rs")),
    (
        "platform/store/mod.rs",
        include_str!("../src/platform/store/mod.rs"),
    ),
    (
        "platform/store/meta.rs",
        include_str!("../src/platform/store/meta.rs"),
    ),
];

/// The rest of the code, for the dash check.
const OTHER_CODE: &[(&str, &str)] = &[
    ("alloc_count.rs", include_str!("../src/alloc_count.rs")),
    ("app/tests.rs", include_str!("../src/app/tests.rs")),
    ("frame_allocations.rs", include_str!("frame_allocations.rs")),
    ("neutrality.rs", include_str!("neutrality.rs")),
    (
        "interaction/tests.rs",
        include_str!("../src/interaction/tests.rs"),
    ),
    (
        "platform/sound/audio.rs",
        include_str!("../src/platform/sound/audio.rs"),
    ),
    ("background.rs", include_str!("../src/background.rs")),
    ("caps.rs", include_str!("../src/caps.rs")),
    ("commands.rs", include_str!("../src/commands.rs")),
    (
        "platform/video/decoder.rs",
        include_str!("../src/platform/video/decoder.rs"),
    ),
    (
        "platform/video/mod.rs",
        include_str!("../src/platform/video/mod.rs"),
    ),
    ("video.rs", include_str!("../src/video.rs")),
    ("device.rs", include_str!("../src/device.rs")),
    (
        "platform/openxr/egl.rs",
        include_str!("../src/platform/openxr/egl.rs"),
    ),
    ("ids.rs", include_str!("../src/ids.rs")),
    (
        "platform/openxr/input.rs",
        include_str!("../src/platform/openxr/input.rs"),
    ),
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
    (
        "platform/sound/mod.rs",
        include_str!("../src/platform/sound/mod.rs"),
    ),
    (
        "platform/sound/mic.rs",
        include_str!("../src/platform/sound/mic.rs"),
    ),
    ("net/tests.rs", include_str!("../src/net/tests.rs")),
    ("outbox.rs", include_str!("../src/outbox.rs")),
    (
        "platform/openxr/overlay.rs",
        include_str!("../src/platform/openxr/overlay.rs"),
    ),
    (
        "platform/meta/passthrough.rs",
        include_str!("../src/platform/meta/passthrough.rs"),
    ),
    ("pending.rs", include_str!("../src/pending.rs")),
    ("platform/mod.rs", include_str!("../src/platform/mod.rs")),
    (
        "platform/controllers.rs",
        include_str!("../src/platform/controllers.rs"),
    ),
    (
        "platform/android/mod.rs",
        include_str!("../src/platform/android/mod.rs"),
    ),
    (
        "platform/android/codecs.rs",
        include_str!("../src/platform/android/codecs.rs"),
    ),
    (
        "platform/meta/mod.rs",
        include_str!("../src/platform/meta/mod.rs"),
    ),
    ("presence.rs", include_str!("../src/presence.rs")),
    ("profiles.rs", include_str!("../src/profiles.rs")),
    ("selftest.rs", include_str!("../src/selftest.rs")),
    (
        "selftest/device.rs",
        include_str!("../src/selftest/device.rs"),
    ),
    ("session/tests.rs", include_str!("../src/session/tests.rs")),
    ("settings.rs", include_str!("../src/settings.rs")),
    (
        "platform/android/system.rs",
        include_str!("../src/platform/android/system.rs"),
    ),
    ("world/mod.rs", include_str!("../src/world/mod.rs")),
    ("world/tests.rs", include_str!("../src/world/tests.rs")),
    (
        "platform/openxr/mod.rs",
        include_str!("../src/platform/openxr/mod.rs"),
    ),
    (
        "panel_preview.rs",
        include_str!("../examples/panel_preview.rs"),
    ),
];

#[test]
fn no_long_dashes_anywhere() {
    for (name, text) in SOURCES.iter().chain(OTHER_CODE) {
        for (n, line) in text.lines().enumerate() {
            assert!(
                !line.contains('\u{2014}') && !line.contains('\u{2013}'),
                "{name}:{}: a long dash: {line}",
                n + 1
            );
        }
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
        // The headset's messages are the last part of the shared file.
        let first = file
            .lines()
            .position(|l| l.starts_with("### Headset app"))
            .expect("the headset's part of the file");
        for (n, line) in file.lines().enumerate().skip(first) {
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
