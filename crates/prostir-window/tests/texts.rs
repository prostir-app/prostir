//! The window's code and themes use no long dash. That every language has every message
//! and that the code names exactly the messages there are is checked for all programs
//! together by `prostir-lang`'s `tests/files.rs`.

/// The code, for the dash check.
const SOURCES: &[(&str, &str)] = &[
    ("app.rs", include_str!("../src/app.rs")),
    ("pages/mod.rs", include_str!("../src/pages/mod.rs")),
    (
        "pages/overview.rs",
        include_str!("../src/pages/overview.rs"),
    ),
    (
        "pages/monitors.rs",
        include_str!("../src/pages/monitors.rs"),
    ),
    (
        "pages/headsets.rs",
        include_str!("../src/pages/headsets.rs"),
    ),
    ("pages/sound.rs", include_str!("../src/pages/sound.rs")),
    ("pages/look.rs", include_str!("../src/pages/look.rs")),
    (
        "pages/first_run.rs",
        include_str!("../src/pages/first_run.rs"),
    ),
    ("map.rs", include_str!("../src/map.rs")),
    ("widgets.rs", include_str!("../src/widgets.rs")),
];

/// All the code, for the dash check.
const ALL_CODE: &[(&str, &str)] = &[
    ("api.rs", include_str!("../src/api.rs")),
    ("dbus.rs", include_str!("../src/dbus.rs")),
    ("fonts.rs", include_str!("../src/fonts.rs")),
    ("instance.rs", include_str!("../src/instance.rs")),
    ("launch.rs", include_str!("../src/launch.rs")),
    ("lib.rs", include_str!("../src/lib.rs")),
    ("link.rs", include_str!("../src/link.rs")),
    ("run.rs", include_str!("../src/run.rs")),
    ("mock.rs", include_str!("../src/mock.rs")),
    ("portal.rs", include_str!("../src/portal.rs")),
    ("snap.rs", include_str!("../src/snap.rs")),
    ("theme.rs", include_str!("../src/theme.rs")),
];

#[test]
fn no_long_dashes_anywhere() {
    let themes = [
        ("ink.toml", prostir_window::theme::INK),
        ("graphite.toml", prostir_window::theme::GRAPHITE),
    ];
    for (name, text) in themes.iter().chain(SOURCES).chain(ALL_CODE) {
        for (n, line) in text.lines().enumerate() {
            assert!(
                !line.contains('\u{2014}') && !line.contains('\u{2013}'),
                "{name}:{}: a long dash: {line}",
                n + 1
            );
        }
    }
}
