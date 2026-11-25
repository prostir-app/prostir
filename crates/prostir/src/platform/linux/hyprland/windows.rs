//! Hyprland's windows, over its IPC socket: the active one, moving it, the mouse.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use log::warn;
use serde_json::Value;

use super::request;
use crate::platform::linux::screenshot;
use crate::platform::{ActiveWindow, AppWindow, OutputKey, Pointer, WindowList, Windows};

/// Hyprland's windows and mouse, for the core.
pub struct HyprWindows {
    list: Arc<HyprWindowList>,
    dispatch: Dispatcher,
}

impl HyprWindows {
    pub fn new() -> Self {
        Self {
            list: Arc::new(HyprWindowList),
            dispatch: Dispatcher::new(),
        }
    }
}

/// Every window, asked of Hyprland from any thread.
struct HyprWindowList;

/// Only adapts the core's types to the Hyprland calls below.
impl Windows for HyprWindows {
    fn active(&mut self) -> Option<ActiveWindow> {
        active_window()
    }

    /// Hyprland's focused monitor: the one with the mouse.
    fn active_output(&mut self) -> Option<OutputKey> {
        super::focused().map(OutputKey::new)
    }

    fn move_to(&mut self, w: &ActiveWindow, output: &OutputKey, fill: bool) -> Result<()> {
        move_window(&mut self.dispatch, w, output.as_str(), fill)
    }

    fn put_back(&mut self, w: &ActiveWindow, output: &OutputKey) -> Result<()> {
        put_window_back(&mut self.dispatch, w, output.as_str())
    }

    /// Hyprland is asked directly which panels have no window left.
    fn vacated(&mut self, panels: &[OutputKey]) -> Vec<OutputKey> {
        let names: Vec<String> = panels.iter().map(OutputKey::to_string).collect();
        vacated_panels(&names)
            .into_iter()
            .map(OutputKey::new)
            .collect()
    }

    fn screenshot(&mut self, output: Option<&OutputKey>) -> Result<PathBuf> {
        let mut cmd = std::process::Command::new("grim");
        if let Some(o) = output {
            cmd.args(["-o", o.as_str()]);
        }
        screenshot::take("grim", cmd)
    }

    fn list(&self) -> Arc<dyn WindowList> {
        self.list.clone()
    }
}

/// Only adapts the core's types to the Hyprland calls below.
impl Pointer for HyprWindows {
    fn position(&mut self) -> Option<(i32, i32)> {
        cursor()
    }

    fn move_to(&mut self, x: i32, y: i32) -> bool {
        move_cursor(&mut self.dispatch, x, y)
    }
}

impl WindowList for HyprWindowList {
    fn all(&self) -> Vec<AppWindow> {
        all_windows()
    }
}

fn active_window() -> Option<ActiveWindow> {
    let json = request("j/activewindow").ok()?;
    let v: Value = serde_json::from_str(&json).ok()?;
    let size = v.get("size")?.as_array()?;
    let monitor = v.get("monitor").and_then(Value::as_i64);
    let on = monitors()
        .into_iter()
        .find(|(id, _, _)| Some(*id) == monitor);
    Some(ActiveWindow {
        id: v.get("address")?.as_str()?.to_owned(),
        width: size.first()?.as_i64()? as i32,
        height: size.get(1)?.as_i64()? as i32,
        scale: on.as_ref().map_or(1.0, |m| m.2),
        output: on.map(|m| OutputKey::new(m.1)),
    })
}

/// Where the mouse is on the desktop (logical pixels).
fn cursor() -> Option<(i32, i32)> {
    let json = request("j/cursorpos").ok()?;
    let v: Value = serde_json::from_str(&json).ok()?;
    Some((v.get("x")?.as_i64()? as i32, v.get("y")?.as_i64()? as i32))
}

/// Every window.
fn all_windows() -> Vec<AppWindow> {
    let monitors = monitors();
    let Ok(json) = request("j/clients") else {
        return Vec::new();
    };
    let Ok(Value::Array(clients)) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    clients
        .iter()
        .filter_map(|c| {
            let pid = c.get("pid")?.as_i64()? as u32;
            let class = c.get("class")?.as_str()?.to_owned();
            let monitor = c.get("monitor")?.as_i64()?;
            let output = monitors.iter().find(|m| m.0 == monitor)?.1.clone();
            Some(AppWindow {
                pid,
                class,
                output: OutputKey::new(output),
            })
        })
        .collect()
}

/// Puts the mouse at `x`,`y` (logical desktop pixels).
fn move_cursor(d: &mut Dispatcher, x: i32, y: i32) -> bool {
    d.send(&Cmd::MoveCursor { x, y })
}

/// Moves window `w` to `output`; `fill`: full screen there (a window panel).
fn move_window(d: &mut Dispatcher, w: &ActiveWindow, output: &str, fill: bool) -> Result<()> {
    // Focus it first: the dispatchers act on the active window.
    d.send(&Cmd::Focus(&w.id));
    if !d.send(&Cmd::MoveWindow(output)) {
        bail!("Hyprland did not move the window");
    }
    if fill {
        d.send(&Cmd::Fullscreen);
    }
    Ok(())
}

/// Puts window `w` back on `output` as an ordinary window (out of full screen).
fn put_window_back(d: &mut Dispatcher, w: &ActiveWindow, output: &str) -> Result<()> {
    d.send(&Cmd::Focus(&w.id));
    d.send(&Cmd::ExitFullscreen);
    move_window(d, w, output, false)
}

/// Panel outputs (of `panels`) with no window left on them.
fn vacated_panels(panels: &[String]) -> Vec<String> {
    if panels.is_empty() {
        return Vec::new();
    }
    let Ok(json) = request("j/clients") else {
        return Vec::new();
    };
    let Ok(Value::Array(clients)) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    let monitors = monitors();
    panels
        .iter()
        .filter(|name| {
            let Some(id) = monitors.iter().find(|m| &m.1 == *name).map(|m| m.0) else {
                return true;
            };
            !clients.iter().any(|c| {
                c.get("monitor").and_then(Value::as_i64) == Some(id)
                    && c.get("mapped").and_then(Value::as_bool) != Some(false)
            })
        })
        .cloned()
        .collect()
}

/// (id, name, scale) of every Hyprland output.
fn monitors() -> Vec<(i64, String, f64)> {
    let Ok(json) = request("j/monitors all") else {
        return Vec::new();
    };
    let Ok(Value::Array(all)) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    all.iter()
        .filter_map(|m| {
            Some((
                m.get("id")?.as_i64()?,
                m.get("name")?.as_str()?.to_owned(),
                m.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
            ))
        })
        .collect()
}

/// How Hyprland spells its dispatchers: the Lua config (0.55 and later) or the classic one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    Lua,
    Classic,
}

impl Dialect {
    fn other(self) -> Self {
        match self {
            Self::Lua => Self::Classic,
            Self::Classic => Self::Lua,
        }
    }
}

/// The dialects to try, in order: the known one first, else the newer one first.
fn order(known: Option<Dialect>) -> [Dialect; 2] {
    let first = known.unwrap_or(Dialect::Lua);
    [first, first.other()]
}

/// A dispatcher call, spelled only once the dialect is known.
enum Cmd<'a> {
    MoveCursor { x: i32, y: i32 },
    Focus(&'a str),
    MoveWindow(&'a str),
    Fullscreen,
    ExitFullscreen,
}

/// Writes `cmd` in `dialect` into `buf` (cleared first), as an IPC request.
fn write_cmd(buf: &mut String, dialect: Dialect, cmd: &Cmd<'_>) {
    buf.clear();
    buf.push_str("dispatch ");
    let _ = match (dialect, cmd) {
        (Dialect::Lua, Cmd::MoveCursor { x, y }) => {
            write!(buf, "hl.dsp.cursor.move({{x = {x}, y = {y}}})")
        }
        (Dialect::Classic, Cmd::MoveCursor { x, y }) => write!(buf, "movecursor {x} {y}"),
        (Dialect::Lua, Cmd::Focus(address)) => {
            write!(buf, "hl.dsp.focus({{window = \"address:{address}\"}})")
        }
        (Dialect::Classic, Cmd::Focus(address)) => write!(buf, "focuswindow address:{address}"),
        (Dialect::Lua, Cmd::MoveWindow(output)) => {
            write!(buf, "hl.dsp.window.move({{monitor = \"{output}\"}})")
        }
        (Dialect::Classic, Cmd::MoveWindow(output)) => write!(buf, "movewindow mon:{output}"),
        (Dialect::Lua, Cmd::Fullscreen) => {
            write!(buf, "hl.dsp.window.fullscreen({{mode = \"fullscreen\"}})")
        }
        (Dialect::Classic, Cmd::Fullscreen) => write!(buf, "fullscreen 0"),
        (Dialect::Lua, Cmd::ExitFullscreen) => {
            write!(
                buf,
                "hl.dsp.window.fullscreen_state({{internal = 0, client = 0}})"
            )
        }
        (Dialect::Classic, Cmd::ExitFullscreen) => write!(buf, "fullscreenstate 0 0"),
    };
}

/// Sends Hyprland's dispatchers in the one dialect it understands, found on first use.
struct Dispatcher {
    /// Reused for every request.
    buf: String,
    /// The dialect that worked, with the Hyprland instance it worked on.
    known: Option<(Option<OsString>, Dialect)>,
    /// Whether the refusal in progress was already logged.
    warned: bool,
}

impl Dispatcher {
    fn new() -> Self {
        Self {
            buf: String::new(),
            known: None,
            warned: false,
        }
    }

    /// Runs `cmd` on the real Hyprland; whether it took it.
    fn send(&mut self, cmd: &Cmd<'_>) -> bool {
        let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
        self.send_with(instance, cmd, &mut |c| request(c))
    }

    /// Runs `cmd` through `ask`. The dialect that worked last is tried first, and the
    /// other one only after a refusal. A different Hyprland instance (it restarted)
    /// forgets the dialect, so it is found again.
    fn send_with(
        &mut self,
        instance: Option<OsString>,
        cmd: &Cmd<'_>,
        ask: &mut dyn FnMut(&str) -> Result<String>,
    ) -> bool {
        let known = match &self.known {
            Some((seen, dialect)) if *seen == instance => Some(*dialect),
            _ => None,
        };
        for dialect in order(known) {
            write_cmd(&mut self.buf, dialect, cmd);
            match ask(&self.buf) {
                Ok(reply) if reply.trim() == "ok" => {
                    self.known = Some((instance, dialect));
                    self.warned = false;
                    return true;
                }
                // The socket itself is gone: the other dialect would fail the same way.
                Err(_) => break,
                Ok(_) => {}
            }
        }
        if !self.warned {
            self.warned = true;
            write_cmd(&mut self.buf, Dialect::Classic, cmd);
            let spelled = self.buf.trim_start_matches("dispatch ");
            warn!("Hyprland refused {spelled:?}");
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    /// A pretend Hyprland that understands one dialect and records every request.
    struct Fake {
        speaks: Dialect,
        seen: Vec<String>,
    }

    impl Fake {
        fn new(speaks: Dialect) -> Self {
            Self {
                speaks,
                seen: Vec::new(),
            }
        }

        fn ask(&mut self, command: &str) -> Result<String> {
            self.seen.push(command.to_owned());
            let lua = command.starts_with("dispatch hl.");
            let ok = lua == (self.speaks == Dialect::Lua);
            Ok(if ok { "ok\n" } else { "unknown dispatcher" }.to_owned())
        }
    }

    fn sig(name: &str) -> Option<OsString> {
        Some(name.into())
    }

    #[test]
    fn newer_dialect_is_tried_first_when_nothing_is_known() {
        assert_eq!(order(None), [Dialect::Lua, Dialect::Classic]);
        assert_eq!(
            order(Some(Dialect::Classic)),
            [Dialect::Classic, Dialect::Lua]
        );
    }

    #[test]
    fn spells_each_dialect() {
        let mut buf = String::from("stale");
        write_cmd(&mut buf, Dialect::Lua, &Cmd::MoveCursor { x: 3, y: -4 });
        assert_eq!(buf, "dispatch hl.dsp.cursor.move({x = 3, y = -4})");
        write_cmd(&mut buf, Dialect::Classic, &Cmd::MoveCursor { x: 3, y: -4 });
        assert_eq!(buf, "dispatch movecursor 3 -4");
        write_cmd(&mut buf, Dialect::Lua, &Cmd::MoveWindow("DP-1"));
        assert_eq!(buf, "dispatch hl.dsp.window.move({monitor = \"DP-1\"})");
        write_cmd(&mut buf, Dialect::Classic, &Cmd::Focus("0xab"));
        assert_eq!(buf, "dispatch focuswindow address:0xab");
    }

    #[test]
    fn old_hyprland_costs_one_extra_request_only_once() {
        let mut fake = Fake::new(Dialect::Classic);
        let mut d = Dispatcher::new();
        for _ in 0..3 {
            assert!(d.send_with(sig("a"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        }
        // Lua refused once, then classic every time.
        assert_eq!(fake.seen.len(), 4);
        assert_eq!(
            fake.seen[0],
            "dispatch hl.dsp.window.fullscreen({mode = \"fullscreen\"})"
        );
        assert!(fake.seen[1..].iter().all(|c| c == "dispatch fullscreen 0"));
    }

    #[test]
    fn new_hyprland_takes_one_request_per_command() {
        let mut fake = Fake::new(Dialect::Lua);
        let mut d = Dispatcher::new();
        for _ in 0..3 {
            assert!(d.send_with(sig("a"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        }
        assert_eq!(fake.seen.len(), 3);
    }

    #[test]
    fn a_refused_known_dialect_is_detected_again() {
        let mut fake = Fake::new(Dialect::Lua);
        let mut d = Dispatcher::new();
        assert!(d.send_with(sig("a"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        fake.speaks = Dialect::Classic;
        assert!(d.send_with(sig("a"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        assert_eq!(d.known, Some((sig("a"), Dialect::Classic)));
        let before = fake.seen.len();
        assert!(d.send_with(sig("a"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        assert_eq!(fake.seen.len(), before + 1);
    }

    #[test]
    fn a_restarted_hyprland_is_detected_again() {
        let mut fake = Fake::new(Dialect::Classic);
        let mut d = Dispatcher::new();
        assert!(d.send_with(sig("old"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        fake.speaks = Dialect::Lua;
        fake.seen.clear();
        assert!(d.send_with(sig("new"), &Cmd::Fullscreen, &mut |c| fake.ask(c)));
        // Started with the newer dialect, not with the one the old instance used.
        assert!(fake.seen[0].starts_with("dispatch hl."));
        assert_eq!(fake.seen.len(), 1);
        assert_eq!(d.known, Some((sig("new"), Dialect::Lua)));
    }

    #[test]
    fn neither_dialect_works_warns_once() {
        let mut d = Dispatcher::new();
        let mut asked = 0;
        let mut refuse = |_: &str| {
            asked += 1;
            Ok("error".to_owned())
        };
        assert!(!d.send_with(sig("a"), &Cmd::Fullscreen, &mut refuse));
        assert!(d.warned);
        assert!(!d.send_with(sig("a"), &Cmd::Fullscreen, &mut refuse));
        assert!(d.warned);
        assert_eq!(d.known, None);
        assert_eq!(asked, 4);
        // Working again, it may warn again later.
        assert!(d.send_with(sig("a"), &Cmd::Fullscreen, &mut |_| Ok("ok".to_owned())));
        assert!(!d.warned);
    }

    #[test]
    fn a_dead_socket_is_not_retried_in_the_other_dialect() {
        let mut d = Dispatcher::new();
        let mut asked = 0;
        let mut gone = |_: &str| {
            asked += 1;
            Err(anyhow!("no socket"))
        };
        assert!(!d.send_with(sig("a"), &Cmd::Fullscreen, &mut gone));
        assert_eq!(asked, 1);
        assert_eq!(d.known, None);
    }
}
