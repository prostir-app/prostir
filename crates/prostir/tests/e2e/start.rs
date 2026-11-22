//! Starting Prostir: the settings window is its own window and the tray icon the
//! program minimized, so starting it again by any way in (the menu's plain `prostir`,
//! `prostir serve` of an autostart line or a terminal) opens or brings forward the
//! window of the one running and ends well; a click on the icon does the same; the
//! very first start opens it; `--version` says the version, on every command.

use std::process::Command;
use std::time::{Duration, Instant};

use crate::pc::{Bus, Options, Pc, TestDir};
use crate::{SOON, TWO_MONITORS};

/// What the fake desktop logs where a real one opens the settings window.
const WINDOW: &str = "fake desktop: the settings window opens";

/// `-v` and `--version` print the version as the log says it, and nothing starts.
#[test]
fn the_version_from_every_way_in() {
    let expected = format!(
        "Prostir {}\n",
        prostir_proto::app_version(env!("CARGO_PKG_VERSION"))
    );
    for args in [
        &["--version"][..],
        &["-v"],
        &["serve", "--version"],
        &["ctl", "--version"],
        &["window", "-v"],
        &["window", "--version"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_prostir"))
            .args(args)
            .env_remove("PROSTIR_FAKE_DESKTOP")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{args:?}");
    }
}

/// While Prostir runs, starting it again by any way in opens its settings window and
/// ends with 0, never "already running"; so does a click on the tray icon.
#[test]
fn starting_again_opens_the_settings_window() {
    let dir = TestDir::new("start-again");
    dir.save_monitors(TWO_MONITORS);
    let mut pc = Pc::start(&dir, &Options::default());
    if pc.bus.is_none() {
        return;
    }
    assert_eq!(
        pc.count_log(WINDOW),
        0,
        "not at a start that is not the first"
    );
    for args in [&[][..], &["serve"], &["serve", "--stay"]] {
        let before = pc.count_log(WINDOW);
        let again = pc.again(args);
        assert_eq!(again.code, 0, "prostir {args:?}: {again:?}");
        assert!(!again.err.contains("already running"), "{again:?}");
        pc.wait_log_after(SOON, WINDOW, before);
    }
    let before = pc.count_log(WINDOW);
    pc.tray("activate");
    pc.wait_log_after(SOON, WINDOW, before);
    pc.wait_log(SOON, "fake tray: activate");
}

/// Asleep in its sockets' watcher, `prostir` (the menu) wakes it for the window.
#[test]
fn starting_again_wakes_the_watcher_for_the_window() {
    let dir = TestDir::new("start-again-asleep");
    dir.save_monitors(TWO_MONITORS);
    let options = Options {
        watcher: true,
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    if pc.bus.is_none() {
        return;
    }
    let again = pc.again(&[]);
    assert_eq!(again.code, 0, "{again:?}");
    pc.wait_log(SOON, WINDOW);
}

/// The very first start opens the settings window; the next does not.
#[test]
fn the_first_start_opens_the_settings_window() {
    let dir = TestDir::new("first-start");
    let mut pc = Pc::start(&dir, &Options::default());
    pc.wait_log(SOON, "the first start: the settings window opens");
    pc.wait_log(SOON, WINDOW);
    pc.quit();
    let pc = Pc::start(&dir, &Options::default());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(pc.count_log(WINDOW), 0);
}

/// 0.1's options without a command (`prostir --linger 5`) are `prostir serve` with
/// them, which says so in its log: while Prostir runs, the running one opens its window.
#[test]
fn serve_options_without_a_command_are_serve() {
    let dir = TestDir::new("old-options");
    dir.save_monitors(TWO_MONITORS);
    let mut pc = Pc::start(&dir, &Options::default());
    if pc.bus.is_none() {
        return;
    }
    let before = pc.count_log(WINDOW);
    let again = pc.again(&["--linger", "5"]);
    assert_eq!(again.code, 0, "{again:?}");
    assert!(again.err.contains("0.1's way"), "{again:?}");
    assert!(again.err.contains("Prostir runs already"), "{again:?}");
    pc.wait_log_after(SOON, WINDOW, before);
}

/// A bare `prostir` started with the session (0.1's autostart line, which an update
/// left) runs as `prostir serve`, not as the window's `prostir`: an autostart's id from
/// the session manager, or Hyprland's `exec-once` right after Hyprland started, which
/// also says what to change. Prostir already running, serve opens its window, as ever.
#[test]
fn a_bare_prostir_at_login_is_serve() {
    let dir = TestDir::new("bare-at-login");
    dir.save_monitors(TWO_MONITORS);
    let mut pc = Pc::start(&dir, &Options::default());
    if pc.bus.is_none() {
        return;
    }
    let before = pc.count_log(WINDOW);
    let again = pc.again_with(&[], &[("DESKTOP_AUTOSTART_ID", "10d8f2c1a3")]);
    assert_eq!(again.code, 0, "{again:?}");
    assert!(
        again.err.contains("started with the session (Autostart)"),
        "{again:?}"
    );
    assert!(again.err.contains("Prostir runs already"), "{again:?}");
    pc.wait_log_after(SOON, WINDOW, before);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let hyprland = |started: u64| format!("4520b30d498daca8079365bdb909a8de_{started}_1981813508");
    let before = pc.count_log(WINDOW);
    let again = pc.again_with(&[], &[("HYPRLAND_INSTANCE_SIGNATURE", &hyprland(now))]);
    assert_eq!(again.code, 0, "{again:?}");
    assert!(
        again.err.contains("started with the session (Hyprland)"),
        "{again:?}"
    );
    assert!(again.err.contains("exec-once = prostir serve"), "{again:?}");
    pc.wait_log_after(SOON, WINDOW, before);

    // Long after Hyprland started: by hand, the window's `prostir`.
    let again = pc.again_with(
        &[],
        &[("HYPRLAND_INSTANCE_SIGNATURE", &hyprland(now - 600))],
    );
    assert_eq!(again.code, 0, "{again:?}");
    assert!(!again.err.contains("started with the session"), "{again:?}");
}

/// The processes whose environment has `mark` (`NAME=value`), by process id.
fn marked(mark: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let environ = std::fs::read(entry.path().join("environ")).unwrap_or_default();
        if environ.split(|b| *b == 0).any(|v| v == mark.as_bytes()) {
            found.push(pid);
        }
    }
    found
}

/// Prostir runs, but on another session bus than the one `prostir` (or `prostir
/// serve`) is started on, from another session say: the one started says so, once,
/// and ends, starting no other program (never a chain of them).
#[test]
fn a_prostir_on_another_bus_is_not_started_again_and_again() {
    let dir = TestDir::new("start-other-bus");
    dir.save_monitors(TWO_MONITORS);
    let pc = Pc::start(&dir, &Options::default());
    if pc.bus.is_none() {
        return;
    }
    let other = Bus::start(&dir.path().join("home")).expect("a second session bus");
    for (n, args) in [&[][..], &["serve"]].into_iter().enumerate() {
        let value = format!("{}-{n}", std::process::id());
        let mark = format!("PROSTIR_E2E_MARK={value}");
        let mut child = pc
            .again_command(
                args,
                &[
                    ("DBUS_SESSION_BUS_ADDRESS", other.address.as_str()),
                    ("PROSTIR_E2E_MARK", &value),
                ],
            )
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut most = 0;
        let ended = loop {
            most = most.max(marked(&mark).len());
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= until || most > 20 {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        for pid in marked(&mark) {
            // SAFETY: a signal to a process this test started (it carries its mark).
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            most <= 2,
            "prostir {args:?}: {most} processes at once; {err}"
        );
        let ended = ended.unwrap_or_else(|| panic!("prostir {args:?} did not end: {err}"));
        assert_eq!(ended.code(), Some(2), "prostir {args:?}: {err}");
        let said = err
            .lines()
            .filter(|l| l.starts_with("prostir: ") && l.contains("not on this session bus"))
            .count();
        assert_eq!(said, 1, "prostir {args:?}: said once: {err}");
        // From the menu (a bare `prostir`), a notification says it too.
        let noticed = err.contains("notification: Prostir: Prostir is already running");
        assert_eq!(noticed, args.is_empty(), "prostir {args:?}: {err}");
        assert!(
            !err.contains("ended before it started"),
            "prostir {args:?}: {err}"
        );
        assert_eq!(most, 1, "prostir {args:?}: nothing started: {err}");
    }
    pc.quit();
}
