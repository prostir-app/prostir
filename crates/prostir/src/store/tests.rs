use super::*;

fn spec(s: &str) -> MonitorSpec {
    s.parse().unwrap()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prostir-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every monitors.txt 0.1 ever wrote moves over: sizes, scales, rows, where each was
/// seen, the primary one, and the choices.
#[test]
fn every_old_monitors_txt_moves_over() {
    // Before rows were saved: one row.
    let (_, set) = migrate("2560x1440\n1920x1080@1.5\n");
    assert_eq!(set.specs.len(), 2);
    assert_eq!(set.specs[&ScreenId::saved(1)].scale, 1.5);
    assert_eq!(set.rows.len(), 1);
    // Before vertical positions (0.1.0).
    let (s, set) = migrate("row\n2560x1440 at -1300 *\n1920x1080\nmirror off\nbitrate 30\n");
    assert_eq!(set.primary, Some(ScreenId::saved(0)));
    assert_eq!(
        set.rows,
        vec![vec![
            Entry {
                id: ScreenId::saved(0),
                center: Some(-1300),
                middle: None
            },
            Entry::new(ScreenId::saved(1)),
        ]]
    );
    assert!(!s.mirror && s.bitrate == 30);
    // 0.1.1 and 0.1.2: rows, middles, `next`, every choice.
    let (s, set) = migrate(
        "row\n2560x1440 at -1300 200\nrow\n1440x2560 at next -40 *\nmirror on\naudio off\n\
         mic off\nbitrate 40\nsharp on\ncodec h264\nsoftware on\n",
    );
    assert_eq!(set.rows.len(), 2);
    assert_eq!(set.rows[1][0].center, None);
    assert_eq!(set.rows[1][0].middle, Some(-40));
    assert_eq!(set.primary, Some(ScreenId::saved(1)));
    assert_eq!(
        s,
        Settings {
            bitrate: 40,
            sharp: Some(true),
            codec: Codec::H264,
            software: true,
            audio: false,
            mirror: true,
            count: Counting::Unasked,
            autostart: None,
            window: prostir_api::WindowLook::default(),
            access: BTreeMap::new(),
        }
    );
}

/// The settings window's look is kept, and only when it is not the default; a
/// value that does not read stays as an unknown line.
#[test]
fn the_window_look_is_kept() {
    let (s, kept) = parse_settings("bitrate 30\n");
    assert!(!settings_text(&s, &kept).contains("window-"));
    let (s, kept) =
        parse_settings("window-theme dark\nwindow-palette graphite\nwindow-language uk\n");
    assert_eq!(
        s.window,
        prostir_api::WindowLook {
            theme: prostir_api::ThemeMode::Dark,
            palette: "graphite".into(),
            language: Some("uk".into()),
        }
    );
    let text = settings_text(&s, &kept);
    assert!(
        text.contains("window-theme dark\nwindow-palette graphite\nwindow-language uk\n"),
        "{text}"
    );
    assert_eq!(parse_settings(&text).0, s);
    let (s, kept) = parse_settings("window-theme purple\nwindow-palette a b\n");
    assert_eq!(s.window, prostir_api::WindowLook::default());
    assert_eq!(kept.unknown.len(), 2);
}

/// Clearer text is not chosen by default (on where it fits in a frame's time), and a
/// `sharp off` line is no choice before version 3, where it was the default written
/// for everyone: from version 3 on, on or off is a choice, written as such, and no
/// line is none.
#[test]
fn clearer_text_by_default() {
    assert_eq!(Settings::default().sharp, None);
    assert!(Settings::default().clear_text());
    for (text, sharp) in [
        ("bitrate 30\n", None),
        ("version 2\nsharp off\n", None),
        ("version 2\nsharp on\n", Some(true)),
        // 0.1's monitors.txt, which has no version.
        ("sharp off\n", None),
        ("version 3\nsharp off\n", Some(false)),
        ("version 3\nsharp on\n", Some(true)),
        ("version 3\n", None),
        // A newer version's choice too.
        ("version 4\nsharp off\n", Some(false)),
    ] {
        let (s, kept) = parse_settings(text);
        assert_eq!(s.sharp, sharp, "{text}");
        // Written as it holds, and read back so.
        let again = settings_text(&s, &kept);
        assert!(again.starts_with(&format!("version {}\n", kept.version.max(3))));
        assert_eq!(again.contains("sharp"), sharp.is_some(), "{again}");
        assert_eq!(parse_settings(&again).0.sharp, sharp, "{again}");
    }
    assert_eq!(migrate("2560x1440\nsharp off\n").0.sharp, None);
    // Chosen only when what shows changes: a headset that sends what it shows keeps
    // it by size.
    let mut s = Settings::default();
    s.choose_clear_text(true);
    assert_eq!(s.sharp, None);
    s.choose_clear_text(false);
    assert_eq!(s.sharp, Some(false));
    s.choose_clear_text(true);
    assert_eq!(s.sharp, Some(true));
}

/// The choice to start with the session is kept once made, and written only then:
/// none before the first start.
#[test]
fn the_autostart_choice_is_kept() {
    let (s, kept) = parse_settings("bitrate 30\n");
    assert_eq!(s.autostart, None);
    assert!(!settings_text(&s, &kept).contains("autostart"));
    for (line, on) in [("autostart off\n", false), ("autostart on\n", true)] {
        let (s, kept) = parse_settings(line);
        assert_eq!(s.autostart, Some(on));
        let text = settings_text(&s, &kept);
        assert!(text.contains(line), "{text}");
        assert_eq!(parse_settings(&text).0.autostart, Some(on));
    }
}

/// The answer about counting is one of three and written as it is; nothing read is
/// taken for a yes. `on` and `off` (earlier builds of 0.2, which counted until turned
/// off): off stays no, on is no answer.
#[test]
fn the_counting_answer_is_kept() {
    let (s, kept) = parse_settings("bitrate 30\n");
    assert_eq!(s.count, Counting::Unasked);
    assert!(settings_text(&s, &kept).contains("count unasked\n"));
    for (line, count) in [
        ("count yes", Counting::Yes),
        ("count no", Counting::No),
        ("count unasked", Counting::Unasked),
    ] {
        let (s, kept) = parse_settings(line);
        assert_eq!(s.count, count, "{line}");
        let text = settings_text(&s, &kept);
        assert!(text.contains(&format!("{line}\n")), "{text}");
        assert_eq!(parse_settings(&text).0.count, count);
    }
    for (line, count, written) in [
        ("count off", Counting::No, "count no\n"),
        ("count on", Counting::Unasked, "count unasked\n"),
    ] {
        let (s, kept) = parse_settings(line);
        assert_eq!(s.count, count, "{line}");
        assert!(settings_text(&s, &kept).contains(written));
    }
    // A value that does not read is kept as it was and counts as no answer.
    let (s, kept) = parse_settings("count maybe\n");
    assert_eq!(s.count, Counting::Unasked);
    assert_eq!(kept.unknown, ["count maybe"]);
}

/// 0.1 kept the counter's switch in `count`, written only when the person used it:
/// an off stays no, also where 0.1 kept no monitors; an on, like no file at all, is
/// no answer, so the question is asked once after the update. Once `settings.txt`
/// has the answer, 0.1's notice goes, and its switch unless it is off (a downgrade
/// to 0.1 reads that).
#[test]
fn the_counting_answer_of_0_1() {
    for monitors in [true, false] {
        for (file, count) in [
            (None, Counting::Unasked),
            (Some("on\n"), Counting::Unasked),
            (Some("off\n"), Counting::No),
        ] {
            let dir = temp_dir("count-0.1");
            if monitors {
                std::fs::write(dir.join("monitors.txt"), "row\n2560x1440\n").unwrap();
            }
            std::fs::write(dir.join("count-notice"), "2026-11-18\n").unwrap();
            if let Some(text) = file {
                std::fs::write(dir.join("count"), text).unwrap();
            }
            let store = Store::open(Some(dir.clone()), Run::default());
            assert_eq!(store.wanted().count, count, "{file:?} {monitors}");
            store.close();
            let text = std::fs::read_to_string(dir.join("settings.txt")).unwrap_or_default();
            let store = Store::open(Some(dir.clone()), Run::default());
            assert_eq!(store.wanted().count, count, "{text}");
            store.close();
            if !text.is_empty() {
                assert!(!dir.join("count-notice").exists());
                assert_eq!(dir.join("count").exists(), count == Counting::No);
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The six monitors of 2026-11-19, as 0.1.2 saved them.
#[test]
fn the_six_monitors_move_over() {
    let (s, set) = migrate(
        "row\n3440x1440 at -2872 -72\nrow\n2560x1440 at -1280 1368\n1440x2560 at 720 1280 *\n\
         3440x1440 at -2264 6768\n2560x1440 at -2000 5328\nrow\n3440x1440 at -1800 2808\n\
         mirror on\naudio on\nmic on\nbitrate 30\nsharp on\ncodec h265\nsoftware off\n",
    );
    assert_eq!(set.specs.len(), 6);
    let lens: Vec<usize> = set.rows.iter().map(Vec::len).collect();
    assert_eq!(lens, [1, 4, 1]);
    assert_eq!(set.primary, Some(ScreenId::saved(2)));
    assert!(s.sharp == Some(true) && s.mirror);
    // Read back from the new file as it was moved over.
    let text = screens_text(&set, &Kept::default(), FIRST_ID);
    assert_eq!(parse_screens(&text).0, set);
}

#[test]
fn files_round_trip_and_keep_unknown_lines() {
    let settings = Settings {
        bitrate: 50,
        codec: Codec::H264,
        count: Counting::No,
        ..Settings::default()
    };
    let kept = Kept {
        version: 3,
        unknown: vec!["theme dark".into()],
        ..Kept::default()
    };
    let text = settings_text(&settings, &kept);
    assert_eq!(parse_settings(&text), (settings, kept.clone()));

    let mut set = SavedScreens::of(&[spec("2560x1440"), spec("1440x2560@1.25")]);
    set.rows[0][1].center = Some(720);
    set.rows[0][1].middle = Some(-40);
    let laptop = ScreenId::saved(100);
    let hdmi = ScreenId::saved(117);
    set.physical.insert(
        laptop,
        Physical {
            key: PhysicalKey {
                edid: Some("BOE 0x0a1c 0".into()),
                connector: "eDP-1".into(),
            },
            builtin: true,
            shown: true,
            seen: 1_795_000_000,
        },
    );
    set.physical.insert(
        hdmi,
        Physical {
            key: PhysicalKey {
                edid: None,
                connector: "HDMI-A-1".into(),
            },
            builtin: false,
            shown: false,
            seen: 0,
        },
    );
    set.rows.push(vec![Entry {
        id: laptop,
        center: None,
        middle: Some(2200),
    }]);
    set.primary = Some(ScreenId::saved(1));
    let text = screens_text(&set, &kept, 133);
    assert!(text.contains("next 133\n"), "{text}");
    assert!(text.contains("screen 1 1440x2560@1.25 at 720 -40 primary\n"));
    assert!(text.contains(
        "physical 100 edid \"BOE 0x0a1c 0\" connector eDP-1 builtin shown seen 1795000000\n"
    ));
    assert!(text.contains("physical 117 connector HDMI-A-1\n"));
    assert!(text.contains("place 100 at next 2200\n"));
    assert_eq!(parse_screens(&text), (set, kept, Some(133)));
    // The laptop screen of version 1 is number 100.
    let (old, _, next) = parse_screens("row\nscreen 0 2560x1440\nmirror at 10 20\n");
    assert_eq!(old.rows[0][1].id, laptop);
    assert_eq!(old.rows[0][1].middle, Some(20));
    assert_eq!(next, None);
}

/// Each headset's rights: a line for a headset with other rights than all; one
/// without a line is the main one with every right.
#[test]
fn rights_are_kept_by_headset() {
    let key = [0xab; 32];
    let line = format!("access {} guest input mic", "AB".repeat(32));
    let (s, kept) = parse_settings(&line);
    assert!(kept.unknown.is_empty());
    let a = s.access_of(&key);
    assert!(!a.is_main() && a.may(perms::INPUT | perms::MIC) && !a.may(perms::CHANGES));
    assert_eq!(s.access_of(&[1; 32]), Access::MAIN);
    assert!(
        settings_text(&s, &kept).contains(&format!("access {} guest input mic\n", "ab".repeat(32)))
    );
    // A key that is not one: kept as it was, and nothing more.
    let (s, kept) = parse_settings("access 12 main");
    assert!(s.access.is_empty() && kept.unknown == ["access 12 main"]);
    // A role or right mistyped: that headset gets no rights at all (not every
    // right, as without a line), and the line stays as it was to put right.
    for bad in [
        "access {k} boss",
        "access {k} main fly",
        "access {k} guest input mike",
        "access {k}",
    ] {
        let bad = bad.replace("{k}", &"ab".repeat(32));
        let (s, kept) = parse_settings(&bad);
        let a = s.access_of(&key);
        assert!(!a.is_main() && a.perms == 0, "{bad}: {a:?}");
        assert_eq!(kept.unknown, std::slice::from_ref(&bad));
        let text = settings_text(&s, &kept);
        assert_eq!(text.matches("access ").count(), 1, "{text}");
        assert_eq!(parse_settings(&text).0.access_of(&key), a, "the same again");
    }
    // A mistyped line beside a good one for the same headset, in either order:
    // no rights while the mistyped one is there, and both lines kept as they were.
    let good = format!("access {} guest input", "ab".repeat(32));
    let typo = format!("access {} guest inptu", "ab".repeat(32));
    for text in [format!("{good}\n{typo}\n"), format!("{typo}\n{good}\n")] {
        let (s, kept) = parse_settings(&text);
        assert_eq!(s.access_of(&key).perms, 0, "{text}");
        let saved = settings_text(&s, &kept);
        assert!(saved.contains(&format!("{good}\n")), "{saved}");
        assert!(saved.contains(&format!("{typo}\n")), "{saved}");
        let (again, kept_again) = parse_settings(&saved);
        assert_eq!(again.access_of(&key).perms, 0);
        assert_eq!(settings_text(&again, &kept_again), saved, "stable");
    }
}

/// Numbers never come back: the next block is on disk before its first number is
/// given out, so after a crash the counter goes on after it (a gap, never a number
/// twice). It starts above every number earlier versions gave out.
#[test]
fn numbers_never_come_back() {
    let dir = temp_dir("counter");
    std::fs::write(
        dir.join("screens.txt"),
        "row\nscreen 0 2560x1440\nscreen 5 1920x1080\n",
    )
    .unwrap();
    let mut store = Store::open(Some(dir.clone()), Run::default());
    let first = store.new_id();
    assert_eq!(
        first,
        ScreenId::saved(FIRST_ID),
        "above 0..5, 10..12 and 100"
    );
    let on_disk = std::fs::read_to_string(dir.join("screens.txt")).unwrap();
    assert!(
        on_disk.contains(&format!("next {}\n", FIRST_ID + ID_BLOCK)),
        "{on_disk}"
    );
    let given: Vec<ScreenId> = (0..20).map(|_| store.new_id()).collect();
    // Crashed: nothing more written.
    drop(store);
    let mut again = Store::open(Some(dir.clone()), Run::default());
    let after = again.new_id();
    assert!(
        given.iter().all(|g| *g < after),
        "{after:?} after {given:?}"
    );
    assert_eq!(
        after,
        ScreenId::saved(FIRST_ID + 2 * ID_BLOCK),
        "the gap of a block"
    );
    again.close();
    // Saved numbers above the counter push it up.
    std::fs::write(
        dir.join("screens.txt"),
        "next 150\nrow\nscreen 400 2560x1440\n",
    )
    .unwrap();
    let mut high = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(high.new_id(), ScreenId::saved(401));
    high.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// A quality out of the range the PC offers, written into `settings.txt` by hand or
/// given as `--bitrate`, holds at the nearest end of it: as kbit/s it would not fit.
#[test]
fn the_quality_stays_in_its_range() {
    for (line, mbps) in [
        ("bitrate 4294967\n", 100),
        ("bitrate 101\n", 100),
        ("bitrate 0\n", 5),
        ("bitrate 4\n", 5),
        ("bitrate 50\n", 50),
    ] {
        assert_eq!(parse_settings(line).0.bitrate, mbps, "{line}");
    }
    let store = Store::open(
        None,
        Run {
            bitrate: Some(u32::MAX),
            ..Run::default()
        },
    );
    assert_eq!(store.wanted().bitrate, 100);
    let store = Store::open(
        None,
        Run {
            bitrate: Some(1),
            ..Run::default()
        },
    );
    assert_eq!(store.wanted().bitrate, 5);
}

/// Bad lines are skipped and kept; a monitor listed twice counts once.
#[test]
fn bad_lines_are_skipped() {
    let (set, kept, _) =
        parse_screens("row\nscreen 0 2560x1440\nscreen 0 1920x1080\nscreen x 1x1\nscreen 1 9x9\n");
    assert_eq!(set.specs.len(), 1);
    assert_eq!(kept.unknown.len(), 3);
}

#[test]
fn the_command_line_holds_for_this_run_only() {
    let saved = SavedScreens::of(&[spec("2560x1440"), spec("2560x1440")]);
    assert_eq!(startup_set(&saved, &[]), None, "the saved set");
    let set = startup_set(&saved, &[spec("1920x1080")]).unwrap();
    assert_eq!(set.specs.len(), 1);
    assert_eq!(
        startup_set(&SavedScreens::default(), &[]),
        Some(SavedScreens::first())
    );

    let dir = temp_dir("run");
    let run = Run {
        bitrate: Some(8),
        codec: Some(Codec::H264),
        software: Some(true),
        screens: Some(set.clone()),
    };
    let mut store = Store::open(Some(dir.clone()), run);
    store.set_screens(saved.clone(), true);
    store.close();
    // A first start: the defaults and the chosen set are saved, not the options.
    let mut store = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(store.settings, Settings::default());
    assert_eq!(store.screens(), &saved);
    // Changed while running: it holds over the command line, and is saved.
    store.run.bitrate = Some(8);
    store.set_bitrate(15);
    assert_eq!(store.wanted().bitrate, 15);
    // A layout change to a set from the command line stays in this run.
    store.run.screens = Some(set.clone());
    let mut moved = set;
    moved.rows[0][0].center = Some(100);
    store.set_screens(moved, false);
    store.close();
    let store = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(store.settings.bitrate, 15);
    assert_eq!(store.screens(), &saved);
    store.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// A first start writes no empty set, so the next start does not take it for a
/// broken file.
#[test]
fn a_first_start_leaves_nothing_broken() {
    let dir = temp_dir("first");
    let mut store = Store::open(Some(dir.clone()), Run::default());
    store.start_with(&[]);
    assert_eq!(store.screens(), &SavedScreens::first());
    store.change(|s| s.audio = false);
    store.close();
    assert!(!dir.join("screens.txt").exists());
    let mut store = Store::open(Some(dir.clone()), Run::default());
    assert!(!store.settings.audio);
    // Arranged in the headset: kept.
    store.start_with(&[]);
    let mut moved = SavedScreens::first();
    moved.rows[0][0].middle = Some(10);
    store.set_screens(moved.clone(), false);
    store.close();
    let store = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(store.screens(), &moved);
    assert!(!dir.join("screens.txt.bad").exists());
    store.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// The codec the person chose stays chosen, whatever the graphics card made.
#[test]
fn the_wanted_codec_stays() {
    let dir = temp_dir("codec");
    let mut store = Store::open(Some(dir.clone()), Run::default());
    store.set_encoding(30, false, Codec::H265, false);
    store.close();
    let store = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(store.settings.codec, Codec::H265);
    store.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// 0.1's files move over once, and stay for a downgrade; a cut-short write is
/// removed; a file with nothing readable is kept aside.
#[test]
fn moving_over_and_broken_files() {
    let dir = temp_dir("migrate");
    let old = "row\n2560x1440 at -1300 200\n1920x1080 *\nmirror off\n";
    std::fs::write(dir.join("monitors.txt"), old).unwrap();
    std::fs::write(dir.join("count"), "off\n").unwrap();
    std::fs::write(dir.join("screens.txt.tmp"), "half a fi").unwrap();
    std::fs::write(dir.join("settings.txt.tmp.4321.7"), "half a fi").unwrap();
    let store = Store::open(Some(dir.clone()), Run::default());
    assert!(!store.settings.mirror);
    assert_eq!(store.settings.count, Counting::No);
    assert_eq!(store.screens().specs.len(), 2);
    assert!(!dir.join("screens.txt.tmp").exists());
    assert!(!dir.join("settings.txt.tmp.4321.7").exists());
    store.close();
    assert_eq!(
        std::fs::read_to_string(dir.join("monitors.txt")).unwrap(),
        old
    );
    assert!(dir.join("settings.txt").exists() && dir.join("screens.txt").exists());
    // The new files win from now on.
    std::fs::write(dir.join("monitors.txt"), "3840x2160\n").unwrap();
    let store = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(store.screens().specs.len(), 2);
    store.close();
    // Nothing readable: the default, and the file kept aside.
    std::fs::write(dir.join("screens.txt"), "garbage\n").unwrap();
    let store = Store::open(Some(dir.clone()), Run::default());
    assert!(store.screens().specs.is_empty());
    assert!(dir.join("screens.txt.bad").exists());
    store.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// When each paired headset was last seen, with its app, is kept between runs;
/// a forgotten headset's goes, and lines that cannot be read are left out.
#[test]
fn when_each_headset_was_seen_is_kept() {
    let dir = temp_dir("seen");
    let (a, b) = ([0xa1; 32], [0xb2; 32]);
    let mut store = Store::open(Some(dir.clone()), Run::default());
    assert_eq!(store.last_seen(&a), None);
    store.headset_seen(&a, 100, Some("0.2.0-dev+p6"));
    store.headset_seen(&b, 200, None);
    store.headset_seen(&a, 300, None);
    store.close();
    let mut store = Store::open(Some(dir.clone()), Run::default());
    let seen = |at, app: &str| Seen {
        at,
        app: app.into(),
    };
    assert_eq!(store.last_seen(&a), Some(&seen(300, "0.2.0-dev+p6")));
    assert_eq!(store.last_seen(&b), Some(&seen(200, "")));
    store.keep_seen_of(&[a]);
    assert_eq!(store.last_seen(&b), None);
    store.close();
    let text = std::fs::read_to_string(dir.join("headsets.txt")).unwrap();
    assert_eq!(text, format!("seen {} 300 0.2.0-dev+p6\n", hex(&a)));
    let odd = format!("seen nothex 1\nseen {} soon\nwhatever\n", hex(&a));
    assert!(parse_seen_headsets(&odd).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
