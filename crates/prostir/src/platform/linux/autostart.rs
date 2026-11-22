//! Starting with the session: systemd's `prostir.socket` where the user's systemd
//! runs and the package installed the units (nothing runs until a headset comes), else
//! an XDG autostart entry (`prostir serve`: the small sockets' watcher). Hyprland
//! starts Prostir from its own config (`exec-once = prostir serve`, which XDG autostart
//! does not reach without uwsm), so it is not set from here there.
//!
//! It is on by default: the first start of Prostir for a user turns it on
//! ([`first_start`]), and the control loop keeps that it did (`autostart on` in
//! `settings.txt`), so turning it off later (the tray, `prostir ctl autostart off`,
//! the settings window) holds for good, through every update. Turning it on with
//! systemd enables the socket and removes the XDG entry of 0.1, so the two never fight
//! over the port; it takes effect at the next login when Prostir already runs
//! otherwise now (it holds the port, so the socket is not started now). Turning it off
//! disables the socket and removes the entry; Prostir running now runs on until it
//! quits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::{info, warn};

use super::system::{config_home, run};
use crate::i18n::t;

/// The socket unit.
const SOCKET: &str = "prostir.socket";

/// How Prostir starts with the session on this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    /// systemd's socket: nothing runs until a headset comes.
    Socket,
    /// An XDG autostart entry.
    Entry,
    /// Not from here (Hyprland's own config).
    None,
}

/// The way for a PC where the user's systemd runs (`systemd`) with the units
/// installed (`units`), on Hyprland or not.
fn pick(systemd: bool, units: bool, hyprland: bool) -> Way {
    match (systemd && units, hyprland) {
        (_, true) => Way::None,
        (true, false) => Way::Socket,
        (false, false) => Way::Entry,
    }
}

/// Where autostart lives on this PC (the tests make their own).
struct Places {
    /// `$XDG_CONFIG_HOME`: the XDG entry, the user's units and their links.
    config: Option<PathBuf>,
    /// Where the user's systemd looks for units, the user's own first.
    unit_dirs: Vec<PathBuf>,
    /// The user's systemd runs.
    systemd: bool,
    hyprland: bool,
    /// The program that tells it (`systemctl`).
    systemctl: PathBuf,
    /// The program the XDG entry starts.
    exe: String,
}

impl Places {
    fn here() -> Self {
        Self {
            config: config_home(),
            unit_dirs: unit_dirs(),
            systemd: std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|d| Path::new(&d).join("systemd/private").exists()),
            hyprland: super::hyprland::running(),
            systemctl: "systemctl".into(),
            // A wrapper (Nix) names the program to start; else this very file (not
            // "... (deleted)" once an update replaced it).
            exe: prostir_window::launch::this_program()
                .map_or_else(|_| "prostir".into(), |p| p.display().to_string()),
        }
    }

    fn way(&self) -> Way {
        pick(
            self.systemd,
            self.unit_dirs.iter().any(|d| d.join(SOCKET).is_file()),
            self.hyprland,
        )
    }

    fn entry(&self) -> Option<PathBuf> {
        Some(
            self.config
                .as_ref()?
                .join("autostart")
                .join("prostir.desktop"),
        )
    }

    /// Whether the socket is enabled: a link in a `sockets.target.wants` of the user's
    /// systemd (the user's own, or one for every user).
    fn socket_enabled(&self) -> bool {
        [
            self.config.as_ref().map(|c| c.join("systemd/user")),
            Some("/etc/systemd/user".into()),
        ]
        .into_iter()
        .flatten()
        .any(|d| d.join("sockets.target.wants").join(SOCKET).exists())
    }

    fn available(&self) -> bool {
        match self.way() {
            Way::Socket => true,
            Way::Entry => self.entry().is_some(),
            Way::None => false,
        }
    }

    fn enabled(&self) -> bool {
        self.entry().is_some_and(|p| p.exists())
            || (self.way() == Way::Socket && self.socket_enabled())
    }

    fn set(&self, on: bool) {
        match self.way() {
            Way::Socket => {
                // The entry of 0.1 would start a watcher that takes the port first.
                self.remove_entry();
                let _ = self.systemctl(&[if on { "enable" } else { "disable" }, SOCKET]);
            }
            Way::Entry if on => self.write_entry(),
            Way::Entry => self.remove_entry(),
            Way::None => {}
        }
    }

    /// The first start for this user: on, where it is set from here; whether it is.
    fn first_start(&self) -> bool {
        match self.way() {
            Way::None => false,
            _ => {
                self.set(true);
                true
            }
        }
    }

    fn systemctl(&self, args: &[&str]) -> bool {
        let done = run(
            Command::new(&self.systemctl).arg("--user").args(args),
            Duration::from_secs(10),
        );
        match done {
            Ok(out) if out.status.success() => {
                info!("systemctl --user {}", args.join(" "));
                return true;
            }
            Ok(out) => warn!(
                "systemctl --user {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => warn!("systemctl --user {}: {e}", args.join(" ")),
        }
        false
    }

    fn write_entry(&self) {
        let Some(path) = self.entry() else { return };
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=Prostir\n\
             Comment=Virtual monitors for Meta Quest\nExec={} serve\nIcon=prostir\n\
             Terminal=false\nX-KDE-autostart-phase=2\n",
            desktop_arg(&self.exe)
        );
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, text));
        if let Err(e) = written {
            warn!("cannot change autostart ({}): {e}", path.display());
        }
    }

    fn remove_entry(&self) {
        let Some(path) = self.entry() else { return };
        match std::fs::remove_file(&path) {
            Ok(()) => info!("autostart entry removed: {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("cannot remove {}: {e}", path.display()),
        }
    }

    /// See [`migrate`].
    fn migrate(&self) -> bool {
        let Some(path) = self.entry() else {
            return false;
        };
        let old = std::fs::read_to_string(&path).is_ok_and(|text| without_serve(&text));
        if old {
            self.write_entry();
        }
        old
    }
}

/// Where the user's systemd looks for units, the user's own first.
fn unit_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    dirs.extend(config_home().map(|c| c.join("systemd/user")));
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    dirs.extend(data_home.map(|d| d.join("systemd/user")));
    dirs.push("/etc/systemd/user".into());
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(d).join("systemd/user")),
    );
    dirs.extend(
        ["/usr/local/lib/systemd/user", "/usr/lib/systemd/user"]
            .iter()
            .map(PathBuf::from),
    );
    dirs
}

/// Whether this PC can start Prostir with the session from here (Hyprland without
/// uwsm ignores XDG autostart: its own config starts it).
pub fn available() -> bool {
    Places::here().available()
}

/// Whether Prostir starts with the session.
pub fn enabled() -> bool {
    Places::here().enabled()
}

/// Turns starting with the session on or off.
pub fn set(on: bool) {
    Places::here().set(on);
}

/// The first start of Prostir for this user (no choice kept yet): starting with the
/// session goes on. On Hyprland, where its own config starts Prostir, a notification
/// says how, once.
pub fn first_start() {
    let places = Places::here();
    if places.first_start() {
        info!("starting with the session: on (the first start; the tray turns it off)");
    } else if places.hyprland {
        crate::platform::dialogs::notify(
            t("Запуск разом із Hyprland", "Starting with Hyprland"),
            t(
                "Щоб Prostir чекав на шолом після входу, додайте в конфіг Hyprland рядок: exec-once = prostir serve",
                "For Prostir to wait for the headset after login, add this line to the Hyprland config: exec-once = prostir serve",
            ),
        );
    }
}

/// An XDG entry of 0.1 (`Exec=prostir`, which now opens the window) runs `prostir
/// serve` from now on, wherever it is (also where systemd's socket will take over: the
/// first start removes it then, and until then it says that autostart was on). Whether
/// one was changed. Called first thing on a bare `prostir` and on `prostir serve`.
pub fn migrate() -> bool {
    Places::here().migrate()
}

/// Whether a desktop entry starts the program without `serve` (0.1's).
fn without_serve(entry: &str) -> bool {
    entry
        .lines()
        .filter_map(|l| l.strip_prefix("Exec="))
        .any(|exec| !exec.split_whitespace().any(|word| word == "serve"))
}

/// On each start: an XDG entry names this install's program again (a Nix store path
/// moves with each upgrade). Never turns anything on.
pub fn refresh() {
    let places = Places::here();
    if places.way() == Way::Entry && places.entry().is_some_and(|p| p.exists()) {
        places.write_entry();
    }
}

/// Asked to quit while systemd's socket started Prostir: the socket stops too, until
/// the next login (without waiting: stopping it stops this very service).
pub fn quit_until_login() {
    let _ = Places::here().systemctl(&["stop", "--no-block", SOCKET]);
}

/// Starts Prostir through systemd, when its socket waits for the headset: whether it
/// did (`prostir`, Prostir not running).
pub fn start_service() -> bool {
    socket_waits() && Places::here().systemctl(&["start", "--no-block", "prostir.service"])
}

/// The desktop session's variables a Prostir started elsewhere needs, passed on where
/// this program has them.
const SESSION_VARS: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// Starts this program with `args` as a unit of the user's systemd manager
/// (`systemd-run --user`), so nothing of it writes to this program's stdio and its log
/// goes to the journal: whether it did. Not without a systemd user manager.
pub fn start_in_user_manager(args: &[&str]) -> bool {
    let places = Places::here();
    if !places.systemd {
        return false;
    }
    let mut cmd = Command::new("systemd-run");
    cmd.args(["--user", "--quiet", "--collect", "--no-block"]);
    for var in SESSION_VARS {
        if std::env::var_os(var).is_some() {
            // `NAME` alone: the value of systemd-run's own environment.
            cmd.arg(format!("--setenv={var}"));
        }
    }
    cmd.arg("--").arg(&places.exe).args(args);
    match run(&mut cmd, Duration::from_secs(10)) {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            warn!(
                "systemd-run: {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            warn!("systemd-run: {e}");
            false
        }
    }
}

/// Whether systemd's socket waits for the headset now (Prostir asleep).
pub fn socket_waits() -> bool {
    Places::here().systemd
        && run(
            Command::new("systemctl").args(["--user", "--quiet", "is-active", SOCKET]),
            Duration::from_secs(5),
        )
        .is_ok_and(|out| out.status.success())
}

/// `arg` as one argument of an `Exec` line of a desktop entry: quoted where it has a
/// character the line would read otherwise (a space, a quote, `$`...), with the
/// escapes the quoting and then the string value need; `%` doubled.
fn desktop_arg(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let arg: String = arg
        .chars()
        .filter(|c| *c != '\n')
        .collect::<String>()
        .replace('%', "%%");
    if !arg.chars().any(|c| RESERVED.contains(c)) {
        return arg;
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            // Escaped for the quoting (a backslash), that backslash escaped again for
            // the string value.
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The socket where systemd runs with the units, an entry elsewhere, nothing from
    /// here on Hyprland (it keeps `exec-once`).
    #[test]
    fn the_way_to_start_with_the_session() {
        assert_eq!(pick(true, true, false), Way::Socket);
        assert_eq!(pick(true, false, false), Way::Entry, "no units installed");
        assert_eq!(pick(false, true, false), Way::Entry, "no systemd");
        assert_eq!(pick(true, true, true), Way::None);
        assert_eq!(pick(false, false, true), Way::None);
    }

    /// A program's path goes into the entry as one argument, whatever it holds.
    #[test]
    fn paths_are_quoted_for_the_entry() {
        assert_eq!(desktop_arg("/usr/bin/prostir"), "/usr/bin/prostir");
        assert_eq!(
            desktop_arg("/home/a b/.local/bin/prostir"),
            "\"/home/a b/.local/bin/prostir\""
        );
        assert_eq!(desktop_arg("/x/$HOME/p"), "\"/x/\\\\$HOME/p\"");
        assert_eq!(desktop_arg("/x/a\"b"), "\"/x/a\\\\\"b\"");
        assert_eq!(desktop_arg("/x/a\\b"), "\"/x/a\\\\\\\\b\"");
        assert_eq!(desktop_arg("/x/100%/p"), "/x/100%%/p");
    }

    /// A PC of the test's own: a config directory, a directory of units (with the
    /// socket when `units`), and a `systemctl` that writes what it was asked.
    fn places(name: &str, systemd: bool, units: bool) -> (Places, PathBuf) {
        let dir = std::env::temp_dir().join(format!("prostir-auto-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let unit_dir = dir.join("units");
        std::fs::create_dir_all(&unit_dir).unwrap();
        if units {
            std::fs::write(unit_dir.join(SOCKET), "[Socket]\n").unwrap();
        }
        let systemctl = dir.join("systemctl");
        let script = format!("#!/bin/sh\necho \"$@\" >> {}/asked\n", dir.display());
        crate::platform::process::write_program(&systemctl, &script);
        let p = Places {
            config: Some(dir.join("config")),
            unit_dirs: vec![unit_dir],
            systemd,
            hyprland: false,
            systemctl,
            exe: "/opt/my apps/prostir".into(),
        };
        (p, dir)
    }

    /// The first start with systemd and the units: the socket is enabled (not started:
    /// the running Prostir holds the port) and the entry of 0.1 goes.
    #[test]
    fn the_first_start_enables_the_socket() {
        let (p, dir) = places("socket", true, true);
        let entry = p.entry().unwrap();
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, "[Desktop Entry]\nExec=prostir\n").unwrap();
        assert!(p.first_start());
        let asked = std::fs::read_to_string(dir.join("asked")).unwrap();
        assert_eq!(asked, "--user enable prostir.socket\n");
        assert!(!entry.exists(), "the entry of 0.1 removed");
        // Off: disabled, nothing else.
        p.set(false);
        let asked = std::fs::read_to_string(dir.join("asked")).unwrap();
        assert!(
            asked.ends_with("--user disable prostir.socket\n"),
            "{asked}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An entry of 0.1 (a bare `prostir`, which now opens the window) runs `prostir
    /// serve` from now on, with this program's path; one that does already is left.
    #[test]
    fn an_entry_of_0_1_runs_serve() {
        assert!(without_serve("[Desktop Entry]\nExec=prostir\n"));
        assert!(without_serve(
            "[Desktop Entry]\nExec=/usr/bin/prostir --linger 5\n"
        ));
        assert!(!without_serve(
            "[Desktop Entry]\nExec=/usr/bin/prostir serve\n"
        ));
        assert!(!without_serve(
            "[Desktop Entry]\nExec=\"/a b/prostir\" serve --stay\n"
        ));
        let (p, dir) = places("migrate", true, true);
        assert!(!p.migrate(), "no entry");
        let entry = p.entry().unwrap();
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, "[Desktop Entry]\nType=Application\nExec=prostir\n").unwrap();
        assert!(p.migrate());
        let text = std::fs::read_to_string(&entry).unwrap();
        assert!(
            text.contains("\nExec=\"/opt/my apps/prostir\" serve\n"),
            "{text}"
        );
        assert!(!p.migrate(), "once");
        assert!(!dir.join("asked").exists(), "systemctl left alone");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Without systemd (or without the units): an entry that runs `prostir serve`,
    /// with the program's path quoted; off removes it. Hyprland: nothing from here.
    #[test]
    fn without_systemd_an_entry_and_on_hyprland_nothing() {
        let (p, dir) = places("entry", false, true);
        assert!(p.first_start());
        let text = std::fs::read_to_string(p.entry().unwrap()).unwrap();
        assert!(
            text.contains("\nExec=\"/opt/my apps/prostir\" serve\n"),
            "{text}"
        );
        assert!(p.enabled());
        assert!(!dir.join("asked").exists(), "systemctl left alone");
        p.set(false);
        assert!(!p.entry().unwrap().exists());
        assert!(!p.enabled());
        let hyprland = Places {
            hyprland: true,
            ..places("hyprland", true, true).0
        };
        assert!(!hyprland.first_start());
        assert!(!hyprland.available());
        std::fs::remove_dir_all(&dir).unwrap();
        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("prostir-auto-hyprland-{}", std::process::id())),
        );
    }
}
