//! The window driven by name and by keyboard, the way a screen reader or a keyboard
//! user would, on the mock service. Nothing is drawn, so no graphics card is needed.

mod common;

use common::{Setup, open, press, press_as, window};
use eframe::egui::{self, Key, Role};
use egui_kittest::kittest::Queryable;
use prostir_window::api::{ClientError, Counting, Request, ThemeMode};
use prostir_window::app::{Conn, Page, Start};
use prostir_window::mock::Mock;

#[test]
fn pairing_by_name_and_keyboard() {
    // As step 6.8 brings it: pairing opened from the window.
    let mock = Mock::connected()
        .pairing()
        .edit(|s| s.pairing = None)
        .live();
    let (mut h, handle) = window(mock, Setup::default());
    open(&mut h, Page::Headsets);
    press(&mut h, "Сполучити новий шолом");
    assert_eq!(handle.calls(), [Request::PairNewHeadset]);
    assert!(h.query_by_label("Зупинити сполучення").is_some());
    let left = h.state().ui.pair.ends - h.ctx.input(|i| i.time);
    assert!((170.0..=180.0).contains(&left), "{left}");

    // The headset shows a code; it is typed here and sent with Enter.
    let (mut h, handle) = window(Mock::connected().pairing(), Setup::default());
    open(&mut h, Page::Headsets);
    h.get_by_label("Код із шолома").focus();
    h.run_steps(1);
    h.get_by_label("Код із шолома").type_text("48a2 913");
    h.run_steps(2);
    assert_eq!(h.state().ui.code, "482913");
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert_eq!(
        handle.calls(),
        [Request::PairingCode {
            code: "482913".into()
        }]
    );
    // Pairing closed with a new headset: the window says who was paired.
    assert!(handle.state().pairing.is_none());
    h.get_by_label("Quest 3S (2) сполучено. Далі він підключатиметься сам.");
    assert_eq!(handle.state().headsets.len(), 3);
}

/// The headset's profiles: one is renamed by name and keyboard (its new name typed and
/// sent with Enter, the profile named by its number, as it has no name), one by its
/// name is given back its number with an empty name; a number only is never sent as
/// a name; the one in use says so, on one row even where two have its name.
#[test]
fn a_profile_is_renamed() {
    let mock = Mock::connected().edit(|s| {
        let session = s.headsets[0].session.as_mut().unwrap();
        session.profiles = vec![String::new(), "Робота".into(), "Кіно".into()];
        session.profile = Some("Робота".into());
    });
    let (mut h, handle) = window(mock, Setup::default());
    open(&mut h, Page::Monitors);
    assert_eq!(h.query_all_by_label("Увімкнено зараз").count(), 1);
    press(&mut h, "Перейменувати Профіль 1");
    h.get_by_label("Нова назва для Профіль 1").focus();
    h.run_steps(1);
    h.get_by_label("Нова назва для Профіль 1").type_text("12");
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert!(handle.calls().is_empty(), "a number only is not a name");
    h.get_by_label("Назва не може бути лише числом: число означає місце профілю.");
    press(&mut h, "Скасувати");
    press(&mut h, "Перейменувати Профіль 1");
    assert!(handle.calls().is_empty(), "not before it is saved");
    h.get_by_label("Нова назва для Профіль 1").focus();
    h.run_steps(1);
    h.get_by_label("Нова назва для Профіль 1")
        .type_text("  Ігри ");
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert_eq!(
        handle.calls(),
        [Request::RenameProfile {
            profile: "1".into(),
            name: "Ігри".into()
        }]
    );
    h.get_by_label("Перейменувати Ігри");
    let session = handle.state().headsets[0].session.clone().unwrap();
    assert_eq!(session.profiles, ["Ігри", "Робота", "Кіно"]);
    press_as(&mut h, Some(Role::Button), "Перейменувати Робота");
    h.get_by_label("Нова назва для Робота").focus();
    h.run_steps(1);
    for _ in 0.."Робота".chars().count() {
        h.key_press(Key::Backspace);
    }
    h.run_steps(1);
    press(&mut h, "Зберегти");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::RenameProfile {
            profile: "Робота".into(),
            name: String::new()
        })
    );
    h.get_by_label("Перейменувати Профіль 2");

    // Two of one name: one of them in use, one of them renamed.
    let mock = Mock::connected().edit(|s| {
        let session = s.headsets[0].session.as_mut().unwrap();
        session.profiles = vec!["Робота".into(), "Робота".into()];
        session.profile = Some("Робота".into());
    });
    let (mut h, _) = window(mock, Setup::default());
    open(&mut h, Page::Monitors);
    assert_eq!(h.query_all_by_label("Увімкнено зараз").count(), 1);
    // Under the arrangement, further down the page.
    h.query_all_by_label("Перейменувати Робота")
        .next()
        .unwrap()
        .scroll_to_me();
    h.run_steps(4);
    h.query_all_by_label("Перейменувати Робота")
        .next()
        .unwrap()
        .click();
    h.run_steps(2);
    assert_eq!(h.query_all_by_label("Нова назва для Робота").count(), 1);
}

#[test]
fn forgetting_a_headset_asks_first() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Headsets);
    press(&mut h, "Забути Quest 3S");
    assert!(handle.calls().is_empty());
    h.get_by_label("Скасувати");
    press(&mut h, "Забути Quest 3S");
    assert_eq!(
        handle.calls(),
        [Request::ForgetHeadset {
            headset: "c81e07".into()
        }]
    );
}

#[test]
fn monitors_by_name_and_keyboard() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Monitors);
    press(&mut h, "1920 × 1080");
    assert!(matches!(
        handle.calls()[..],
        [Request::AddMonitor {
            w: 1920,
            h: 1080,
            ..
        }]
    ));
    // Select a monitor with the keyboard and move it with an arrow key.
    h.get_by_label("Prostir-3").focus();
    h.run_steps(1);
    h.key_press(Key::ArrowDown);
    h.run_steps(3);
    let calls = handle.calls();
    assert!(
        matches!(calls.last(), Some(Request::Place { id: 3, x: 5120, y }) if *y > 0),
        "{calls:?}"
    );
    assert_eq!(h.state().ui.selected, Some(3));
    // The selected monitor's controls.
    press(&mut h, "125\u{a0}%");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::SetScale { id: 3, scale: 125 })
    );
    press_as(&mut h, Some(Role::Switch), "Головний монітор");
    assert_eq!(handle.calls().last(), Some(&Request::SetPrimary { id: 3 }));
    press(&mut h, "Прибрати монітор");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::RemoveMonitor { id: 3 })
    );
    assert!(h.state().ui.selected.is_none());
}

#[test]
fn the_pages_are_reached_with_the_keyboard() {
    let (mut h, _) = window(Mock::connected(), Setup::default());
    // Tab from nothing lands on the first page in the sidebar, then the next.
    h.key_press(Key::Tab);
    h.run_steps(1);
    assert!(h.get_by_role_and_label(Role::Tab, "Огляд").is_focused());
    h.key_press(Key::Tab);
    h.run_steps(1);
    assert!(h.get_by_role_and_label(Role::Tab, "Монітори").is_focused());
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().ui.page, Page::Monitors);
    h.key_press(Key::Tab);
    h.key_press(Key::Tab);
    h.run_steps(1);
    assert!(h.get_by_role_and_label(Role::Tab, "Звук").is_focused());
    h.key_press(Key::Space);
    h.run_steps(2);
    assert_eq!(h.state().ui.page, Page::Sound);
}

#[test]
fn the_look_is_chosen_and_kept_by_the_service() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Look);
    press(&mut h, "Світла");
    press(&mut h, "Нейтральна");
    press(&mut h, "English");
    let window = handle.state().window;
    assert_eq!(window.theme, ThemeMode::Light);
    assert_eq!(window.palette, "graphite");
    assert_eq!(window.language.as_deref(), Some("en"));
    // The window follows: the texts are English now.
    h.get_by_label("Theme");
}

/// A look chosen while Prostir does not run holds for the window, and once Prostir
/// starts it is kept there too, not replaced by the look Prostir had.
#[test]
fn a_look_chosen_before_prostir_runs_is_kept_once_it_does() {
    let (mut h, handle) = window(Mock::not_running(), Setup::default());
    open(&mut h, Page::Look);
    press(&mut h, "Світла");
    assert_eq!(h.state().look().theme, ThemeMode::Light);
    handle.set_running(true);
    h.run_steps(4);
    assert!(matches!(h.state().conn, Conn::Live { .. }));
    assert_eq!(h.state().look().theme, ThemeMode::Light);
    assert_eq!(handle.state().window.theme, ThemeMode::Light);
}

/// A look chosen before Prostir ran, then another once it answers: the later one
/// holds, never the earlier one sent again.
#[test]
fn the_look_chosen_last_holds() {
    let (mut h, handle) = window(Mock::not_running(), Setup::default());
    open(&mut h, Page::Look);
    press(&mut h, "Світла");
    handle.set_running_quietly(true);
    press(&mut h, "Темна");
    handle.set_running(true);
    h.run_steps(4);
    assert!(matches!(h.state().conn, Conn::Live { .. }));
    assert_eq!(handle.state().window.theme, ThemeMode::Dark);
    assert_eq!(h.state().look().theme, ThemeMode::Dark);
}

#[test]
fn starting_prostir_from_the_window() {
    let setup = Setup::default();
    let starts = setup.starts.clone();
    let (mut h, handle) = window(Mock::not_running(), setup);
    assert!(matches!(h.state().conn, Conn::Absent { .. }));
    press(&mut h, "Запустити Prostir");
    assert_eq!(*starts.lock().unwrap(), 1);
    assert!(matches!(
        h.state().conn,
        Conn::Absent {
            start: Start::Waiting { .. },
            ..
        }
    ));
    h.get_by_label("Запуск Prostir");
    handle.set_running(true);
    h.run_steps(2);
    assert!(matches!(h.state().conn, Conn::Live { .. }));
    // And it going away again.
    handle.set_running(false);
    h.run_steps(2);
    // In the sidebar and on the page.
    assert_eq!(h.get_all_by_label("Prostir не запущено").count(), 2);
}

#[test]
fn starting_gives_up_after_ten_seconds() {
    let (mut h, _) = window(
        Mock::not_running(),
        Setup {
            step: 0.5,
            ..Setup::default()
        },
    );
    press(&mut h, "Запустити Prostir");
    h.run_steps(18);
    assert!(matches!(
        h.state().conn,
        Conn::Absent {
            start: Start::Waiting { .. },
            ..
        }
    ));
    h.run_steps(4);
    assert!(matches!(
        h.state().conn,
        Conn::Absent {
            start: Start::TimedOut,
            ..
        }
    ));
    h.get_by_label("Prostir не запустився");
}

#[test]
fn a_prostir_that_runs_already_is_not_started_again() {
    let setup = Setup {
        started: prostir_window::app::Started::AlreadyRunning,
        ..Setup::default()
    };
    let (mut h, handle) = window(Mock::not_running(), setup);
    press(&mut h, "Запустити Prostir");
    h.get_by_label("Ця версія Prostir старіша за вікно");
    assert!(h.query_by_label("Запустити Prostir").is_none());
    // Once it answers, "Try again" finds it.
    handle.set_running_quietly(true);
    press(&mut h, "Спробувати ще раз");
    assert!(matches!(h.state().conn, Conn::Live { .. }));
}

#[test]
fn the_code_can_be_pasted() {
    let (mut h, handle) = window(Mock::connected().pairing(), Setup::default());
    open(&mut h, Page::Headsets);
    h.get_by_label("Код із шолома").focus();
    h.run_steps(1);
    h.event(egui::Event::Paste("482 913\n".into()));
    h.run_steps(2);
    assert_eq!(h.state().ui.code, "482913");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(
        handle.calls(),
        [Request::PairingCode {
            code: "482913".into()
        }]
    );
}

#[test]
fn a_theme_font_that_is_not_a_font_does_not_stop_the_window() {
    let dir = std::env::temp_dir().join(format!("prostir-window-font-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let text = dir.join("words.ttf");
    std::fs::write(&text, "not a font at all").unwrap();
    let path = dir.join("window-theme.toml");
    std::fs::write(
        &path,
        format!(
            "[type]\nfont = \"{}\"\nfont_medium = \"/dev/zero\"\n",
            text.display()
        ),
    )
    .unwrap();
    let (mut h, _) = window(
        Mock::connected(),
        Setup {
            theme_file: path.clone(),
            ..Setup::default()
        },
    );
    open(&mut h, Page::Look);
    assert_eq!(h.get_all_by_label_contains("шрифт не читається").count(), 2);
    // The same when the file changes while the window is open.
    std::fs::write(&path, "[type]\nfont = \"/dev/zero\"\n").unwrap();
    h.state_mut().ui.page = Page::Overview;
    h.run_steps(90);
    open(&mut h, Page::Look);
    assert_eq!(h.get_all_by_label_contains("шрифт не читається").count(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn going_on_in_the_first_run_opens_pairing() {
    let (mut h, handle) = window(Mock::no_headsets(), Setup::default());
    assert_eq!(h.state().ui.page, Page::FirstRun);
    press(&mut h, "Далі");
    assert_eq!(handle.calls(), [Request::PairNewHeadset]);
}

/// A monitor just added stands nowhere until the headset placed it: a line under the
/// map says so and can remove it; placed, it is on the map instead.
#[test]
fn a_monitor_being_placed_is_named_under_the_map() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Monitors);
    press(&mut h, "1920 × 1080");
    h.run_steps(2);
    h.get_by_label("Prostir-4: розставляється…");
    assert!(h.query_by_label("Prostir-4").is_none(), "not on the map");
    press(&mut h, "Прибрати Prostir-4");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::RemoveMonitor { id: 5 })
    );
    assert!(h.query_by_label("Prostir-4: розставляється…").is_none());

    press(&mut h, "2560 × 1440");
    h.run_steps(2);
    h.get_by_label("Prostir-4: розставляється…");
    handle.place_new();
    h.run_steps(3);
    assert!(h.query_by_label("Prostir-4: розставляється…").is_none());
    h.get_by_label("Prostir-4");
}

#[test]
fn a_refused_request_says_why_until_closed() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Monitors);
    handle.refuse_next(ClientError::Refused {
        code: prostir_window::api::ApiError::Desktop,
        message: "KWin не відповів.".into(),
    });
    press(&mut h, "2560 × 1440");
    h.get_by_label("Не вдалося: KWin не відповів.");
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("Не вдалося: KWin не відповів.").is_none());
}

#[test]
fn a_service_that_does_not_answer_is_asked_again() {
    let mock = Mock::connected();
    mock.handle().set_silent(true);
    let (mut h, handle) = window(mock, Setup::default());
    assert_eq!(h.get_all_by_label("Prostir не відповідає").count(), 2);
    assert!(h.query_by_label("Запустити Prostir").is_none());
    handle.set_silent(false);
    press(&mut h, "Спробувати ще раз");
    assert!(matches!(h.state().conn, Conn::Live { .. }));
}

#[test]
fn the_first_run_says_when_pairing_is_closed() {
    let (mut h, handle) = window(Mock::no_headsets(), Setup::default());
    h.state_mut().ui.fr_step = 1;
    h.run_steps(3);
    h.get_by_label("Сполучення закрито, тож новий шолом не знайде цей ПК.");
    assert!(h.query_by_label("Пошук шолома в мережі").is_none());
    press(&mut h, "Відкрити знову");
    assert_eq!(handle.calls(), [Request::PairNewHeadset]);
}

#[test]
fn a_new_theme_file_pins_nothing() {
    let dir = std::env::temp_dir().join(format!("prostir-window-new-{}", std::process::id()));
    let path = dir.join("window-theme.toml");
    let (mut h, _) = window(
        Mock::connected(),
        Setup {
            theme_file: path.clone(),
            ..Setup::default()
        },
    );
    open(&mut h, Page::Look);
    press(&mut h, "Створити файл");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("format = 1"));
    h.run_steps(90);
    // The file is in use, clean, and the palette still changes the colours.
    h.get_by_label("Застосовано. Зміни у файлі видно одразу, перезапуск не потрібен.");
    press(&mut h, "Нейтральна");
    h.run_steps(30);
    let graphite =
        prostir_window::theme::Theme::builtin(prostir_window::theme::PaletteName::Graphite);
    assert_eq!(h.state().theme().dark, graphite.dark);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tab_meets_back_before_next_as_seen() {
    let (mut h, _) = window(Mock::no_headsets(), Setup::default());
    h.state_mut().ui.fr_step = 1;
    h.run_steps(3);
    let back = h.get_by_label("Назад").rect();
    let next = h.get_by_label("Далі").rect();
    assert!(back.right() < next.left(), "Back is left of Next");
    h.get_by_label("Назад").focus();
    h.run_steps(1);
    h.key_press(Key::Tab);
    h.run_steps(1);
    assert!(h.get_by_label("Далі").is_focused());
}

#[test]
fn pairing_this_prostir_does_not_open_says_what_to_do_instead() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Headsets);
    press(&mut h, "Сполучити новий шолом");
    assert_eq!(handle.calls(), [Request::PairNewHeadset]);
    // No failure: the button gives way to what to do.
    assert!(h.query_by_label_contains("Не вдалося").is_none());
    assert!(h.query_by_label("Сполучити новий шолом").is_none());
    h.get_by_label_contains("ПК покаже тут код");
}

#[test]
fn a_code_shown_on_the_pc_has_no_countdown_and_never_runs_out() {
    let (mut h, handle) = window(Mock::connected().pc_code("482913"), Setup::default());
    open(&mut h, Page::Headsets);
    h.get_by_label("Введіть цей код у шоломі:");
    h.get_by_label("482 913");
    assert!(h.query_by_label_contains("ще ").is_none(), "no countdown");
    // The code goes away (paired with another headset or closed): no "time is over".
    handle.change(|s| s.pairing = None);
    h.run_steps(3);
    assert!(h.query_by_label_contains("Час сполучення минув").is_none());
}

#[test]
fn monitors_kept_for_the_next_headset() {
    let (mut h, handle) = window(Mock::waiting(), Setup::default());
    open(&mut h, Page::Monitors);
    h.get_by_label("Чекають на шолом");
    // Added without a headset: kept too.
    press(&mut h, "1920 × 1080");
    let added = handle
        .state()
        .displays
        .into_iter()
        .find(|d| d.name == "Prostir-4")
        .unwrap();
    assert_eq!(added.state, prostir_window::api::DisplayState::Kept);
    press(&mut h, "Прибрати Prostir-1");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::RemoveMonitor { id: 1 })
    );
    open(&mut h, Page::Overview);
    assert!(h.get_all_by_label("Чекає на шолом").count() >= 3);
}

#[test]
fn the_microphone_is_shown_not_switched_and_the_volume_waits() {
    let (mut h, _) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Sound);
    assert!(
        h.query_by_role_and_label(Role::Switch, "Мікрофон шолома на ПК")
            .is_none()
    );
    h.get_by_label("Вмикається й вимикається в шоломі.");
    h.get_by_label("Змінюється кнопками шолома.");
}

/// With a headset that shares its volume: a click and a drag on the slider each set it
/// once, when let go, and the keys move it in steps of 5.
#[test]
fn the_volume_slider_sets_the_volume() {
    let mock = Mock::connected().edit(|s| {
        s.sound.volume = Some(prostir_window::api::Volume {
            level: 40,
            muted: false,
        });
    });
    let (mut h, handle) = window(mock, Setup::default());
    open(&mut h, Page::Sound);
    h.get_by_label("Та сама, що на кнопках шолома і в системі.");
    press_as(&mut h, Some(Role::Slider), "Гучність");
    let Some(Request::Volume { level }) = handle.calls().last().cloned() else {
        panic!("no volume asked: {:?}", handle.calls());
    };
    assert!((45..=55).contains(&level), "{level}");

    let rect = h.get_by_role_and_label(Role::Slider, "Гучність").rect();
    let button = |h: &mut egui_kittest::Harness<'static, prostir_window::App>, at, pressed| {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    };
    let calls = handle.calls().len();
    let from = rect.center();
    h.hover_at(from);
    h.run_steps(1);
    button(&mut h, from, true);
    h.run_steps(1);
    for i in 1..=10 {
        h.hover_at(from + egui::vec2(rect.width() * 0.04 * i as f32, 0.0));
        h.run_steps(1);
    }
    assert_eq!(handle.calls().len(), calls, "nothing asked while dragged");
    let to = from + egui::vec2(rect.width() * 0.4, 0.0);
    button(&mut h, to, false);
    h.run_steps(3);
    assert_eq!(handle.calls().len(), calls + 1, "{:?}", handle.calls());
    let Some(Request::Volume { level }) = handle.calls().last().cloned() else {
        panic!("no volume asked: {:?}", handle.calls());
    };
    assert!(level >= 85, "{level}");

    h.get_by_role_and_label(Role::Slider, "Гучність").focus();
    h.run_steps(2);
    let calls = handle.calls().len();
    h.key_press(Key::ArrowLeft);
    h.run_steps(3);
    assert_eq!(handle.calls().len(), calls + 1, "{:?}", handle.calls());
    let Some(Request::Volume { level: by_key }) = handle.calls().last().cloned() else {
        panic!("no volume asked: {:?}", handle.calls());
    };
    assert_eq!(by_key % 5, 0);
    assert!(by_key < level, "{by_key} after {level}");
}

#[test]
fn an_older_prostir_is_told_apart_from_none() {
    let mock = Mock::connected();
    mock.handle().set_old(true);
    let (mut h, handle) = window(mock, Setup::default());
    h.get_by_label("Ця версія Prostir старіша за вікно");
    assert!(h.query_by_label("Запустити Prostir").is_none());
    h.get_by_label("Prostir старіший за це вікно");
    handle.set_old(false);
    press(&mut h, "Спробувати ще раз");
    assert!(matches!(h.state().conn, Conn::Live { .. }));
}

#[test]
fn quality_by_name() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Quality);
    press(&mut h, "50");
    press(&mut h, "H.264");
    press_as(&mut h, Some(Role::Switch), "Чіткіший текст");
    press_as(&mut h, Some(Role::Switch), "Кодувати процесором");
    assert_eq!(
        handle.calls(),
        [
            Request::SetQuality { mbit: 50 },
            Request::SetCodec {
                codec: prostir_window::api::CodecChoice::H264,
                cpu: false
            },
            // On by default: switched off.
            Request::SetSharp { on: false },
            Request::SetCodec {
                codec: prostir_window::api::CodecChoice::H264,
                cpu: true
            },
        ]
    );
    let e = handle.state().encoding;
    assert_eq!((e.bitrate, e.sharp, e.cpu), (50, false, true));
    assert!(!e.sharp_by_size, "chosen");
}

#[test]
fn what_the_service_does_not_do_is_disabled_quietly() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Quality);
    handle.refuse_next(ClientError::Refused {
        code: prostir_window::api::ApiError::Unsupported,
        message: "Not on this PC.".into(),
    });
    press_as(&mut h, Some(Role::Switch), "Чіткіший текст");
    assert!(h.query_by_label_contains("Не вдалося").is_none());
    assert!(h.state().ui.unsupported.contains("set_sharp"));
    // Disabled now: a press does nothing.
    press_as(&mut h, Some(Role::Switch), "Чіткіший текст");
    assert_eq!(handle.calls().len(), 1);
}

#[test]
fn a_move_holds_until_the_state_with_it_is_in() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Monitors);
    h.get_by_label("Prostir-3").focus();
    h.run_steps(1);
    h.key_press(Key::ArrowDown);
    h.run_steps(3);
    assert!(matches!(handle.calls().last(), Some(Request::Place { .. })));
    // The answer came with the state that has the move: nothing is held any more.
    assert_eq!(h.state().ui.pending_place, None);
}

/// Three monitors streaming and a monitor of the PC hidden from the headset, with the
/// laptop's panel not connected: the map has all four on the desktop, the hidden one
/// with its size, and no display says a size of 0 × 0.
#[test]
fn the_map_has_every_display_on_the_desktop() {
    use prostir_window::api::{DeskRect, Display, DisplayKind, DisplayState, Mode};
    let monitor = |id: u32, name: &str, x: i32| Display {
        id,
        kind: DisplayKind::Virtual,
        name: name.into(),
        output: format!("Virtual-{name}"),
        shown: true,
        mode: Mode {
            w: 2560,
            h: 1440,
            scale: 100,
        },
        desk: Some(DeskRect {
            x,
            y: 560,
            w: 2560,
            h: 1440,
        }),
        state: DisplayState::Running,
        ..Display::default()
    };
    let mock = Mock::connected().edit(|s| {
        s.displays = vec![
            monitor(1, "Prostir-4", 0),
            monitor(2, "Prostir-2", 2560),
            monitor(3, "Prostir-1", 5120),
            Display {
                id: 117,
                kind: DisplayKind::Physical,
                name: "HDMI-A-1".into(),
                output: "HDMI-A-1".into(),
                shown: false,
                mode: Mode {
                    w: 1920,
                    h: 1080,
                    scale: 100,
                },
                desk: Some(DeskRect {
                    x: 2560,
                    y: 2000,
                    w: 1920,
                    h: 1080,
                }),
                state: DisplayState::Running,
                ..Display::default()
            },
            Display {
                id: 100,
                kind: DisplayKind::Physical,
                name: "eDP-1".into(),
                output: "eDP-1".into(),
                builtin: true,
                shown: true,
                mode: Mode {
                    w: 0,
                    h: 0,
                    scale: 100,
                },
                state: DisplayState::Absent,
                ..Display::default()
            },
        ];
        s.primary = Some(2);
    });
    let (mut h, _handle) = window(mock, Setup::default());
    open(&mut h, Page::Monitors);
    for name in ["Prostir-4", "Prostir-2", "Prostir-1", "HDMI-A-1"] {
        h.get_by_role_and_label(Role::Button, name);
    }
    assert!(h.query_by_label_contains("0 × 0").is_none());
    h.get_by_role_and_label(Role::Button, "HDMI-A-1").focus();
    h.run_steps(2);
    h.get_by_label_contains("1920 × 1080");
    open(&mut h, Page::Overview);
    assert!(h.query_by_label_contains("0 × 0").is_none());
    assert!(h.query_by_label("Монітор не додано").is_none());
}

/// A monitor that could not be made: the overview names it once, its reason a
/// sentence of its own before the rest.
#[test]
fn a_failed_monitor_is_named_once() {
    let mock = Mock::failed_monitor().edit(|s| {
        let d = s
            .displays
            .iter_mut()
            .find(|d| d.name == "Prostir-4")
            .unwrap();
        d.failure.as_mut().unwrap().text = "2560x1440 більше, ніж шолом декодує в HEVC".into();
    });
    let (mut h, _handle) = window(mock, Setup::default());
    open(&mut h, Page::Overview);
    h.get_by_label(
        "Prostir-4: 2560x1440 більше, ніж шолом декодує в HEVC. Решта моніторів працює.",
    );
}

/// "На ПК" keeps the PC's sound on the PC, "У шоломі" sends it to the headset.
#[test]
fn the_sound_goes_where_the_button_says() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    open(&mut h, Page::Sound);
    assert!(handle.state().sound.to_headset);
    press_as(&mut h, Some(Role::RadioButton), "На ПК");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::SoundToHeadset { on: false })
    );
    h.run_steps(2);
    press_as(&mut h, Some(Role::RadioButton), "У шоломі");
    assert_eq!(
        handle.calls().last(),
        Some(&Request::SoundToHeadset { on: true })
    );
    assert_eq!(handle.calls().len(), 2);
}

/// "Вийти з Prostir" quits the whole program as the tray's "Quit" does; it is there
/// only while Prostir runs.
#[test]
fn the_window_quits_prostir() {
    let (mut h, handle) = window(Mock::connected(), Setup::default());
    press(&mut h, "Вийти з Prostir");
    assert_eq!(handle.calls(), [Request::Quit]);
    let (h, _) = window(Mock::not_running(), Setup::default());
    assert!(h.query_by_label("Вийти з Prostir").is_none());
}

/// The first time the window closes while Prostir runs, a note says it goes on in the
/// tray and how to open the window again; never again after that.
#[test]
fn the_first_close_says_prostir_goes_on() {
    use prostir_window::app::CloseNote;
    use std::sync::{Arc, Mutex};
    let dir = std::env::temp_dir().join(format!("prostir-close-note-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let shown = dir.join("prostir/window-closed-note");
    let said: Arc<Mutex<Vec<String>>> = Arc::default();
    let note = |said: Arc<Mutex<Vec<String>>>| CloseNote {
        shown: shown.clone(),
        notify: Box::new(move |summary, body| {
            said.lock().unwrap().push(format!("{summary}: {body}"));
            true
        }),
    };
    let setup = || Setup {
        close_note: Some(note(said.clone())),
        ..Setup::default()
    };
    let (mut h, _) = window(Mock::not_running(), setup());
    h.state_mut().window_closes();
    assert!(said.lock().unwrap().is_empty(), "nothing goes on");
    let (mut h, _) = window(Mock::connected(), setup());
    h.state_mut().window_closes();
    assert_eq!(
        *said.lock().unwrap(),
        [
            "Prostir працює далі: Prostir працює далі у треї. Відкрити знову: значок Prostir у меню або в треї."
        ]
    );
    assert!(shown.exists());
    h.state_mut().window_closes();
    let (mut h, _) = window(Mock::connected(), setup());
    h.state_mut().window_closes();
    assert_eq!(said.lock().unwrap().len(), 1, "once only");
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Count this PC among users?" on the first start: at the top of the first run and
/// the overview, with two equal answers and neither chosen, while the rest of the
/// window works. An answer is sent once and the question goes; the overview's
/// switch then shows the answer and changes it.
#[test]
fn the_counting_question_and_both_answers() {
    const QUESTION: &str = "Рахувати цей ПК серед користувачів Prostir?";
    const SWITCH: &str = "Рахувати цей ПК серед користувачів";
    let unasked = |s: &mut prostir_window::api::State| s.counting = Some(Counting::Unasked);
    for (answer, counting, other) in [
        ("Так, рахувати цей ПК", Counting::Yes, Counting::No),
        ("Ні, не рахувати цей ПК", Counting::No, Counting::Yes),
    ] {
        let (mut h, handle) = window(Mock::no_headsets().edit(unasked), Setup::default());
        assert_eq!(h.state().ui.page, Page::FirstRun);
        h.get_by_label(QUESTION);
        // Neither answer is favoured: buttons of the same size, Yes first, neither
        // focused; and the question comes before the page's own title.
        let yes = h.get_by_role_and_label(Role::Button, "Так, рахувати цей ПК");
        let no = h.get_by_role_and_label(Role::Button, "Ні, не рахувати цей ПК");
        assert_eq!(yes.rect().size(), no.rect().size());
        assert!(yes.rect().right() < no.rect().left());
        assert!(!yes.is_focused() && !no.is_focused());
        let question = h.get_by_label(QUESTION).rect();
        assert!(question.bottom() < h.get_by_label("Крок 1 з 4").rect().top());
        // The first run goes on meanwhile.
        press(&mut h, "Далі");
        assert_eq!(handle.calls(), [Request::PairNewHeadset]);
        h.get_by_label(QUESTION);
        // The overview asks too, and has no switch until the answer.
        open(&mut h, Page::Overview);
        h.get_by_label(QUESTION);
        assert!(h.query_by_role_and_label(Role::Switch, SWITCH).is_none());
        press(&mut h, answer);
        assert_eq!(
            handle.calls(),
            [Request::PairNewHeadset, Request::SetCounting { counting }]
        );
        assert_eq!(handle.state().counting, Some(counting));
        assert!(h.query_by_label(QUESTION).is_none());
        open(&mut h, Page::FirstRun);
        assert!(h.query_by_label(QUESTION).is_none());
        open(&mut h, Page::Overview);
        press_as(&mut h, Some(Role::Switch), SWITCH);
        assert_eq!(
            handle.calls().last(),
            Some(&Request::SetCounting { counting: other })
        );
    }
    // Where this build or run never counts, nothing is asked or shown; nor for an
    // answer this window does not know (an older Prostir's `true`).
    for counting in [None, Some(Counting::Other)] {
        let (mut h, _) = window(
            Mock::connected().edit(|s| s.counting = counting),
            Setup::default(),
        );
        open(&mut h, Page::Overview);
        assert!(h.query_by_label(QUESTION).is_none());
        assert!(h.query_by_role_and_label(Role::Switch, SWITCH).is_none());
    }
}

/// The question's link opens the privacy policy in the window's language.
#[test]
fn the_counting_question_links_the_privacy_policy() {
    let unasked = |s: &mut prostir_window::api::State| s.counting = Some(Counting::Unasked);
    let (mut h, handle) = window(
        Mock::connected().edit(unasked),
        Setup {
            language: "en",
            ..Setup::default()
        },
    );
    h.get_by_label("Count this PC among Prostir's users?");
    let link = h
        .get_by_role_and_label(Role::Link, "Privacy policy")
        .rect()
        .center();
    h.hover_at(link);
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: link,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(1);
    let opened: Vec<String> = h
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::OpenUrl(u) => Some(u.url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened,
        ["https://prostir.ishawyha.dev/privacy.html#counting"]
    );
    assert!(handle.calls().is_empty());
}
