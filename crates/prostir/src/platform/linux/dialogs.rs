//! Talking to the person at the PC: a dialog for the pairing code (kdialog, zenity or
//! a dmenu-style launcher) and desktop notifications (notify-send).

use std::io::ErrorKind;
use std::process::Command;
use std::time::Duration;

use log::warn;

use crate::i18n::t;
use crate::platform::{NoCode, Said, run, run_until};

/// Asks for the code the headset shows, in a small dialog on the PC, closed when
/// `gone` says the headset is gone.
pub fn ask_code(gone: &dyn Fn() -> bool) -> Result<String, NoCode> {
    let text = t("pc-dialogs-a-headset-wants-to-connect-type");
    // Launchers in dmenu mode (common on Hyprland and Sway) print what was typed; they
    // show one line, so the prompt is shorter.
    let prompt = t("pc-dialogs-code-from-the-headset");
    let attempts: [(&str, Vec<&str>); 5] = [
        ("kdialog", vec!["--title", "Prostir", "--inputbox", text]),
        ("zenity", vec!["--entry", "--title=Prostir", "--text", text]),
        ("fuzzel", vec!["--dmenu", "--lines=0", "--prompt", prompt]),
        ("wofi", vec!["--dmenu", "--lines=1", "--prompt", prompt]),
        ("rofi", vec!["-dmenu", "-l", "0", "-p", prompt]),
    ];
    for (program, args) in attempts {
        // Nobody at the PC: the dialog goes away rather than holding the PC.
        let asked = run_until(
            Command::new(program).args(&args),
            Duration::from_secs(120),
            gone,
        );
        let out = match asked {
            Ok(out) => out,
            Err(e) if e.kind() == ErrorKind::NotFound => continue, // not installed
            Err(e) if e.kind() == ErrorKind::Interrupted => return Err(NoCode::Gone),
            Err(_) => return Err(NoCode::Cancelled), // timed out
        };
        if !out.status.success() {
            return Err(NoCode::Cancelled);
        }
        let code: String = String::from_utf8_lossy(&out.stdout)
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        return if code.is_empty() {
            Err(NoCode::Cancelled)
        } else {
            Ok(code)
        };
    }
    warn!(
        "no dialog program (kdialog, zenity, fuzzel, wofi, rofi): cannot ask for the pairing code"
    );
    Err(NoCode::NoDialog)
}

/// Desktop notification, best effort.
pub fn notify(title: &str, body: &str) {
    if cfg!(test) {
        return;
    }
    let child = Command::new("notify-send")
        .args(["--app-name=Prostir", "--icon=prostir", title, body])
        .spawn();
    // Reaped in the background, so finished notify-send processes do not pile up.
    if let Ok(mut child) = child {
        let _ = std::thread::Builder::new()
            .name("notify".into())
            .spawn(move || child.wait());
    }
}

/// Desktop notification, waiting until it is handed over; `true` when it was shown.
pub fn notify_and_wait(title: &str, body: &str) -> bool {
    run(
        Command::new("notify-send").args(["--app-name=Prostir", "--icon=prostir", title, body]),
        Duration::from_secs(10),
    )
    .is_ok_and(|out| out.status.success())
}

/// A desktop notification with buttons, while it is up (notify-send with actions,
/// which says the action's name when a button is pressed and ends when the
/// notification closes). Dropping it stops waiting for a button.
pub struct Notice {
    child: std::process::Child,
    out: std::process::ChildStdout,
    said: Vec<u8>,
    /// Shown instead when this notify-send has no buttons (before libnotify 0.7.10).
    plain: (String, String),
}

/// Shows `title` and `body` with `buttons`; `plain` is the body to show instead where
/// notifications cannot have buttons. `None` without notify-send.
pub fn notice(title: &str, body: &str, buttons: &[&str], plain: &str) -> Option<Notice> {
    let actions = buttons
        .iter()
        .enumerate()
        .map(|(i, label)| format!("--action=b{i}={label}"));
    let mut child = Command::new("notify-send")
        .args(["--app-name=Prostir", "--icon=prostir"])
        .args(actions)
        .args([title, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .inspect_err(|e| warn!("cannot show a notification: {e}"))
        .ok()?;
    let out = child.stdout.take()?;
    Some(Notice {
        child,
        out,
        said: Vec::new(),
        plain: (title.to_owned(), plain.to_owned()),
    })
}

impl Notice {
    /// What to wait on: readable once a button is pressed or the notice closed.
    pub fn fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.out.as_raw_fd()
    }

    /// Once [`Self::fd`] is readable: which button was pressed, or that the notice
    /// closed without one; `None` not yet.
    pub fn check(&mut self) -> Option<Said> {
        use std::io::Read;
        let mut buf = [0u8; 64];
        match self.out.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                self.said.extend_from_slice(&buf[..n]);
                if self.said.len() > 256 {
                    self.said.drain(..128);
                }
                let said = String::from_utf8_lossy(&self.said);
                return said
                    .lines()
                    .find_map(|l| l.trim().strip_prefix('b')?.parse().ok())
                    .map(Said::Button);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => return None,
            Err(_) => {}
        }
        // It ended: without buttons in this notify-send, the plain notice instead.
        if self.child.wait().is_ok_and(|s| !s.success()) {
            notify(&self.plain.0, &self.plain.1);
        }
        Some(Said::Closed)
    }
}

impl Drop for Notice {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
