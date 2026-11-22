//! Pictures of the window drawn without a screen (kittest with wgpu): every page in
//! light and dark with both palettes, and the states each page has. They need a
//! graphics card or lavapipe.
//!
//! Pictures live in tests/snapshots; `UPDATE_SNAPSHOTS=1 cargo test -p prostir-window`
//! writes them anew after a deliberate change. Before every picture kittest checks that
//! each control has an accessible name.

mod common;

use common::{Setup, open, press, window};
use eframe::egui::Key;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use prostir_window::App;
use prostir_window::api::{ClientError, Pairing, Request, ThemeMode};
use prostir_window::app::Page;
use prostir_window::mock::Mock;

/// Setup for a picture.
fn shot() -> Setup {
    Setup {
        pictures: true,
        ..Setup::default()
    }
}

/// Pictures of several windows in one test, checked together at its end.
type Pictures = SnapshotResults;

fn picture(h: &mut Harness<'static, App>, all: &mut Pictures, name: &str) {
    // Text is drawn slightly differently by different GPUs and lavapipe: a few
    // pixels may differ, a changed layout may not.
    h.snapshot_options(
        name,
        &SnapshotOptions::new().threshold(1.5).max_failed_pixels(400),
    );
    all.extend_harness(h);
}

const PAGES: [(Page, &str); 7] = [
    (Page::Overview, "overview"),
    (Page::Monitors, "monitors"),
    (Page::Headsets, "headsets"),
    (Page::Sound, "sound"),
    (Page::Quality, "quality"),
    (Page::Look, "look"),
    (Page::FirstRun, "first-run"),
];

#[test]
fn every_page_in_both_themes_and_palettes() {
    let mut all = Pictures::default();
    for (page, name) in PAGES {
        for (theme, t) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            for palette in ["ink", "graphite"] {
                let mock = if page == Page::FirstRun {
                    Mock::no_headsets()
                } else {
                    Mock::connected()
                };
                let (mut h, _) = window(mock.look(theme, palette), shot());
                if page == Page::Monitors {
                    h.state_mut().ui.selected = Some(2);
                }
                open(&mut h, page);
                picture(&mut h, &mut all, &format!("{name}-{t}-{palette}"));
            }
        }
    }
    all.unwrap();
}

#[test]
fn each_state_of_the_overview() {
    let mut all = Pictures::default();
    let dark = |m: Mock| m.look(ThemeMode::Dark, "ink");
    for (mock, name) in [
        (dark(Mock::connecting()), "overview-connecting"),
        (dark(Mock::waiting()), "overview-waiting"),
        (dark(Mock::failed_monitor()), "overview-failed"),
        (dark(Mock::not_running()), "overview-not-running"),
        (
            dark({
                let m = Mock::connected();
                m.handle().set_old(true);
                m
            }),
            "overview-old",
        ),
        (
            dark(Mock::connected().edit(|s| {
                let session = s.headsets[0].session.as_mut().unwrap();
                session.worn = false;
                s.problems.push(prostir_window::api::Problem {
                    code: prostir_window::api::ApiError::Desktop,
                    text: "KWin не відповідає: нові монітори чекають на нього.".into(),
                });
            })),
            "overview-taken-off",
        ),
    ] {
        let (mut h, _) = window(mock, shot());
        open(&mut h, Page::Overview);
        picture(&mut h, &mut all, name);
    }
    // No headset paired: the window opens on the first run.
    let (mut h, _) = window(dark(Mock::no_headsets()), shot());
    h.run_steps(2);
    assert_eq!(h.state().ui.page, Page::FirstRun);
    open(&mut h, Page::Overview);
    picture(&mut h, &mut all, "overview-no-headsets");
    // In English.
    let (mut h, _) = window(
        Mock::connected().look(ThemeMode::Light, "ink"),
        Setup {
            language: "en",
            ..shot()
        },
    );
    open(&mut h, Page::Overview);
    picture(&mut h, &mut all, "overview-en");
    // "Count this PC among users?" until it is answered: on the first run, where the
    // first start opens, and on the overview.
    let unasked = |s: &mut prostir_window::api::State| {
        s.counting = Some(prostir_window::api::Counting::Unasked);
    };
    let (mut h, _) = window(dark(Mock::no_headsets().edit(unasked)), shot());
    h.run_steps(2);
    picture(&mut h, &mut all, "first-run-question");
    let (mut h, _) = window(
        Mock::connected()
            .edit(unasked)
            .look(ThemeMode::Light, "ink"),
        Setup {
            language: "en",
            ..shot()
        },
    );
    open(&mut h, Page::Overview);
    picture(&mut h, &mut all, "overview-question-en");
    all.unwrap();
}

#[test]
fn each_state_of_the_other_pages() {
    let mut all = Pictures::default();
    let dark = |m: Mock| m.look(ThemeMode::Dark, "ink");
    // Monitors around you, and with no headset (only the PC's screen).
    let (mut h, _) = window(dark(Mock::connected()), shot());
    h.state_mut().ui.arc_view = true;
    h.state_mut().ui.selected = Some(2);
    open(&mut h, Page::Monitors);
    picture(&mut h, &mut all, "monitors-arc");
    let (mut h, _) = window(dark(Mock::waiting()), shot());
    open(&mut h, Page::Monitors);
    picture(&mut h, &mut all, "monitors-offline");
    // Five monitors at mixed scales, upright ones among them: every tile's text inside
    // it, the scale on the size line; in English too.
    for (language, name) in [("uk", "monitors-mixed"), ("en", "monitors-mixed-en")] {
        let (mut h, _) = window(dark(Mock::many_monitors()), Setup { language, ..shot() });
        h.state_mut().ui.selected = Some(2);
        open(&mut h, Page::Monitors);
        picture(&mut h, &mut all, name);
    }
    // A monitor just added, not placed by the headset yet: a line under the map.
    let (mut h, _) = window(dark(Mock::connected()), shot());
    open(&mut h, Page::Monitors);
    press(&mut h, "1920 × 1080");
    h.run_steps(2);
    picture(&mut h, &mut all, "monitors-placing");

    // Pairing: a headset shows a code; then a wrong code; a code from the PC; waiting.
    let (mut h, _) = window(dark(Mock::connected().pairing()), shot());
    open(&mut h, Page::Headsets);
    picture(&mut h, &mut all, "headsets-pairing");
    h.get_by_label("Код із шолома").focus();
    h.run_steps(1);
    h.get_by_label("Код із шолома")
        .type_text(prostir_window::mock::WRONG_CODE);
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(4);
    picture(&mut h, &mut all, "headsets-pairing-wrong");
    // The code the PC shows, as the service does now: no time limit.
    let (mut h, _) = window(dark(Mock::connected().pc_code("482913")), shot());
    open(&mut h, Page::Headsets);
    picture(&mut h, &mut all, "headsets-pc-code");
    // Pairing from the window (step 6.8), open and waiting for a headset.
    let waiting = |s: &mut prostir_window::api::State| {
        s.pairing = Some(Pairing {
            seconds_left: 95,
            seconds_total: 180,
            ..Default::default()
        });
        s.nearby.clear();
        s.pc.addresses = vec!["192.168.1.10".into()];
    };
    let (mut h, _) = window(dark(Mock::connected().pairing().edit(waiting)), shot());
    open(&mut h, Page::Headsets);
    picture(&mut h, &mut all, "headsets-pairing-waiting");
    // A Prostir that does not open pairing from the window: what to do instead.
    let (mut h, _) = window(dark(Mock::connected()), shot());
    open(&mut h, Page::Headsets);
    press(&mut h, "Сполучити новий шолом");
    picture(&mut h, &mut all, "headsets-pair-hint");
    let (mut h, _) = window(dark(Mock::no_headsets()), shot());
    open(&mut h, Page::Headsets);
    picture(&mut h, &mut all, "headsets-empty");

    // Sound with no headset.
    let (mut h, _) = window(dark(Mock::waiting()), shot());
    open(&mut h, Page::Sound);
    picture(&mut h, &mut all, "sound-offline");

    // A theme file with mistakes: what was not used and what is hard to read.
    let dir = std::env::temp_dir().join(format!("prostir-window-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("window-theme.toml");
    std::fs::write(
        &path,
        "format = 1\n[dark]\ntext = \"white\"\ntext_muted = \"#3A4560\"\nglow = \"#FFFFFF\"\n[shape]\nradius = 4\n",
    )
    .unwrap();
    let (mut h, _) = window(
        dark(Mock::connected()),
        Setup {
            theme_file: path,
            ..shot()
        },
    );
    open(&mut h, Page::Look);
    if let Some(last) = h.get_all_by_label_contains("text_muted").last() {
        last.scroll_to_me();
    }
    h.run_steps(3);
    let folder = prostir_lang::short_path(&dir);
    let r = h.get_by_label(&folder).rect();
    h.mask(r);
    picture(&mut h, &mut all, "look-bad-theme");
    std::fs::remove_dir_all(&dir).unwrap();

    // The look chosen without the service holds for the window.
    let (mut h, _) = window(dark(Mock::not_running()), shot());
    open(&mut h, Page::Look);
    picture(&mut h, &mut all, "look-not-running");
    all.unwrap();
}

#[test]
fn first_run_steps() {
    let mut all = Pictures::default();
    let (mut h, handle) = window(Mock::no_headsets().look(ThemeMode::Dark, "ink"), shot());
    assert_eq!(h.state().ui.page, Page::FirstRun);
    press(&mut h, "Далі");
    // Going on to "open Prostir in the headset" opens pairing, so the headset finds
    // the PC.
    assert_eq!(handle.calls(), [Request::PairNewHeadset]);
    picture(&mut h, &mut all, "first-run-2");
    h.state_mut().ui.fr_step = 2;
    h.run_steps(3);
    picture(&mut h, &mut all, "first-run-3");
    h.state_mut().ui.fr_step = 3;
    h.run_steps(3);
    picture(&mut h, &mut all, "first-run-4");
    all.unwrap();
}

#[test]
fn a_failed_request_says_why() {
    let mut all = Pictures::default();
    let (mut h, handle) = window(Mock::connected().look(ThemeMode::Dark, "ink"), shot());
    open(&mut h, Page::Monitors);
    handle.refuse_next(ClientError::Refused {
        code: prostir_window::api::ApiError::Desktop,
        message: "KWin не відповів.".into(),
    });
    press(&mut h, "2560 × 1440");
    h.get_by_label("Не вдалося: KWin не відповів.");
    picture(&mut h, &mut all, "monitors-refused");
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("Не вдалося: KWin не відповів.").is_none());
    all.unwrap();
}
