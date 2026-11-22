//! Renders the headset menu to a PNG for checking the design on the desktop.
//! cargo run -p prostir-client --example panel_preview -- panel.png [hover_x hover_y]
//! Environment: WRIST=1 (the wrist menu), TAB=monitors|look|sound|language|pc|help (default: quick), LANG_UI=en|uk|qps (qps: the longer pseudo-language), PAIR=show|type|failed, OFFLINE=1, KEYPAD=…, NOTE=done|failed|waiting|saved (what the page says under a button pressed on it)

use prostir::ids::{DisplayKey, PcId};
use prostir::lang::{BUILTIN, Known, Lang, Language, Loader, Texts};
use prostir::note::Note;
use prostir::settings::Settings;
use prostir::store::{NoStore, Store as _};
use prostir::ui::{
    Action, MonitorEntry, OfferWhy, PairingView, Panel, Place, Section, Tab, UiState,
};

/// NOTE=`kind`: what the page of `s` says under one of its buttons, as after pressing
/// it: done, failed (with the PC's detail where the PC says one), waiting for the PC;
/// `saved`: a profile saved, on the Monitors page.
fn note(kind: &str, s: &UiState) -> Option<Note> {
    let l = &s.texts;
    let (on, done, failed, detail) = match (s.tab, kind) {
        (Tab::Monitors, "saved") => {
            let name = l.f("profile-n", &[("n", 2.into())]);
            let text = l.f("toast-profile-saved", &[("name", name.into())]);
            return Some(Note::done(Some(Action::SaveProfile(1)), &text));
        }
        (Tab::Quick, _) => (
            Action::SetAudio(false),
            l.t("note-sound-pc"),
            l.t("failure-sound"),
            None,
        ),
        (Tab::Monitors, _) => (
            Action::Add {
                width: 2560,
                height: 1440,
            },
            l.t("note-monitor-added"),
            l.t("failure-limit"),
            Some("at most 6 virtual displays"),
        ),
        (Tab::Settings(Section::Sound), _) => (
            Action::SetMic(true),
            l.t("note-mic-headset"),
            l.t("note-mic-denied"),
            None,
        ),
        (Tab::Settings(Section::Language), _) => (
            Action::Language(Language::LikeSystem),
            l.t("note-language"),
            l.t("failure-not-connected"),
            None,
        ),
        (Tab::Settings(Section::Pc), _) => {
            let text = l.f("note-pc-forgotten", &[("pc", "laptop".into())]);
            return Some(Note::done(Some(Action::ForgetPc(1)), &text));
        }
        _ => return None,
    };
    let on = Some(on);
    Some(match kind {
        "failed" => Note {
            detail: detail.map(Into::into),
            ..Note::failed(on, failed)
        },
        "waiting" => Note::waiting(on, l),
        _ => Note::done(on, done),
    })
}

/// Display `n` of the PC of the preview.
fn k(n: u32) -> DisplayKey {
    DisplayKey::new(PcId([1; 8]), prostir_proto::state::DisplayId(n))
}

fn questmon_pairing(mode: &str) -> PairingView {
    PairingView {
        host: "workstation".into(),
        code: (mode == "show").then(|| "482 913".into()),
        typed: if mode == "show" {
            String::new()
        } else {
            "4829".into()
        },
        failed: mode == "failed",
    }
}

/// The menu's language: a built-in one, or `qps`, every text half as long again.
#[expect(
    clippy::disallowed_methods,
    reason = "a preview on the desktop, built-in files only"
)]
fn texts(code: &str) -> (Texts, Vec<Known>) {
    let qps: &'static str = Box::leak(prostir::lang::pseudo(BUILTIN[0].1).into_boxed_str());
    let mut loader = Loader::with(&[BUILTIN[0], BUILTIN[1], ("qps", qps)], Vec::new());
    let texts = loader.load(Lang::new(code).unwrap_or(Lang::FALLBACK));
    // The pseudo-language is listed only while it is shown.
    let mut languages = loader.languages();
    languages.retain(|k| k.lang.as_str() != "qps" || code == "qps");
    (texts, languages)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "panel.png".into());
    let hover = match (args.next(), args.next()) {
        (Some(x), Some(y)) => Some((x.parse().unwrap(), y.parse().unwrap())),
        _ => None,
    };
    let code = std::env::var("LANG_UI").unwrap_or_else(|_| "uk".into());
    let (texts, languages) = texts(&code);
    let day = prostir::lang::Moment::EXAMPLE;
    let mut state = UiState {
        mic_denied: std::env::var("MIC_DENIED").is_ok(),
        confirm_save: None,
        unreachable: std::env::var("BLOCKED").ok(),
        can_move_mouse: std::env::var("NO_MOUSE").is_err(),
        paired: vec!["workstation".into(), "laptop".into()],
        battery: Some((72, false)),
        clock: texts.clock(false, &day).into(),
        date: texts
            .date(prostir::settings::DateStyle::LikeMenu, &day)
            .into(),
        confirm_forget: None,
        windows: if std::env::var("WINDOWS").is_ok() {
            vec![MonitorEntry {
                id: k(10),
                label: "5".into(),
                width: 1200,
                height: 800,
                place: Place::default(),
                seen: None,
            }]
        } else {
            Vec::new()
        },
        limits: Some(prostir_proto::state::Limits {
            virtual_max: 6,
            windows_max: 3,
            physical_max: 1,
            streams_max: 16,
            min: (640, 480),
            max: (5120, 2880),
            sizes: [
                (1920, 1080),
                (2560, 1440),
                (3840, 2160),
                (3440, 1440),
                (5120, 1440),
                (1440, 2560),
            ]
            .into_iter()
            .map(|(w, h)| prostir_proto::state::Mode::new(w, h))
            .collect(),
            scales: vec![100, 125, 150],
        }),
        pc: Some(prostir_proto::state::Encoding {
            wanted: prostir_proto::state::Wanted {
                bitrate: 30,
                codec: Some(prostir_proto::Codec::H265),
                software: false,
                sharp: false,
            },
            using: Some((prostir_proto::Codec::H265, true)),
            choices: vec![
                (prostir_proto::Codec::H265, true),
                (prostir_proto::Codec::H264, true),
                (prostir_proto::Codec::H264, false),
            ],
        }),
        pair_offer: std::env::var("OFFER").ok().map(|v| {
            let why = match v.as_str() {
                "failed" => OfferWhy::WrongCodes,
                "forgotten" => OfferWhy::Forgotten,
                _ => OfferWhy::New,
            };
            ("workstation 192.168.1.10".into(), why)
        }),
        pairing: std::env::var("PAIR")
            .ok()
            .map(|mode| questmon_pairing(&mode)),
        host: std::env::var("OFFLINE")
            .is_err()
            .then(|| "workstation".into()),
        // With a hint (below), no PC answered.
        found: if std::env::var("HINT").is_ok() {
            Vec::new()
        } else {
            vec![
                ("workstation".into(), "192.168.1.10".into()),
                ("media-pc".into(), "192.168.1.20".into()),
            ]
        },
        ip_input: std::env::var("KEYPAD").ok(),
        // HINT=new or HINT=silent (with OFFLINE): what the search screen says while
        // no PC answers.
        hint: match std::env::var("HINT").as_deref() {
            Ok("new") => Some(prostir::session::Hint::NewPc),
            Ok("silent") => Some(prostir::session::Hint::Silent),
            _ => None,
        },
        monitors: vec![
            MonitorEntry {
                id: k(0),
                label: "1".into(),
                width: 2560,
                height: 1440,
                place: Place::default(),
                seen: Some([-1.0, 0.0, 0.5, 0.28]),
            },
            MonitorEntry {
                id: k(1),
                label: "2".into(),
                width: 2560,
                height: 1440,
                place: Place::default(),
                seen: Some([0.0, 0.0, 0.5, 0.28]),
            },
            MonitorEntry {
                id: k(2),
                label: "3".into(),
                width: 1440,
                height: 2560,
                place: Place::default(),
                seen: Some([0.78, 0.12, 0.28, 0.5]),
            },
        ],
        primary: Some(k(1)),
        cinema_screen: None,
        selected: None,
        mirror_seen: Some([0.0, -0.55, 0.35, 0.22]),
        mirror_wanted: true,
        audio_wanted: true,
        mic_wanted: true,
        mirror_name: Some(texts.f("pc-display-named", &[("name", "HDMI-A-1".into())])),
        settings: Settings::default(),
        room_available: true,
        panorama_available: true,
        custom_available: false,
        curve_available: true,
        keyboard_available: true,
        error: None,
        confirm_remove: std::env::var("CONFIRM").is_ok().then_some(k(2)),
        intro: std::env::var("INTRO").is_ok(),
        update: std::env::var("UPDATE")
            .ok()
            .map(|v| (v == "pc", "0.2.0".into())),
        tab: match std::env::var("TAB").as_deref() {
            Ok("monitors") => Tab::Monitors,
            Ok("look") => Tab::Settings(Section::Look),
            Ok("sound") => Tab::Settings(Section::Sound),
            Ok("language") => Tab::Settings(Section::Language),
            Ok("pc") => Tab::Settings(Section::Pc),
            Ok("help") => Tab::Settings(Section::Help),
            Ok("controls") => Tab::Settings(Section::Controls),
            _ => Tab::Quick,
        },
        saved_profiles: [true, false, false],
        active_profile: Some(0),
        note: None,
        texts,
        languages: languages.into(),
        language_page: 0,
        update_from: NoStore.update_hint(),
    };
    state.note = std::env::var("NOTE")
        .ok()
        .and_then(|kind| note(&kind, &state));
    // TOAST="text": a toast instead.
    if let Ok(message) = std::env::var("TOAST") {
        let mut panel = Panel::new();
        let (w, h) = (1024, 132);
        let rgba = panel.toast_image(&message, w, h);
        let pixmap =
            tiny_skia::Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(w, h).unwrap()).unwrap();
        let mut bg = tiny_skia::Pixmap::new(w, h).unwrap();
        bg.fill(tiny_skia::Color::from_rgba8(90, 96, 110, 255));
        bg.draw_pixmap(
            0,
            0,
            pixmap.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
        bg.save_png(&out).unwrap();
        println!("{out}");
        return;
    }
    // WRIST=1: the wrist menu instead.
    let wrist = std::env::var("WRIST").is_ok();
    let (w, h) = if wrist {
        (prostir::ui::WRIST_WIDTH, prostir::ui::WRIST_HEIGHT)
    } else {
        (prostir::ui::WIDTH, prostir::ui::HEIGHT)
    };
    let mut panel = if wrist { Panel::wrist() } else { Panel::new() };
    panel.set_state(&state);
    panel.hover(hover);
    let rgba = panel.render_if_dirty().unwrap().to_vec();
    let pixmap =
        tiny_skia::Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(w, h).unwrap()).unwrap();
    // Composite over a dark grey so translucent parts are visible in the preview.
    let mut bg = tiny_skia::Pixmap::new(w, h).unwrap();
    bg.fill(tiny_skia::Color::from_rgba8(40, 44, 52, 255));
    bg.draw_pixmap(
        0,
        0,
        pixmap.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    bg.save_png(&out).unwrap();
    println!("{out}");
}
