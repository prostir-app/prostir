//! Hyprland: virtual monitors are headless outputs (`output create headless`), placed
//! with monitor rules, and captured with `wlr-screencopy` (see `screencopy.rs`); the
//! pointer apart from the frames through `ext-image-copy-capture` (`pointer.rs`, Hyprland
//! 0.54 and newer).
//!
//! Everything goes through Hyprland's IPC socket, the same one `hyprctl` uses.
//! Monitor rules are set with Lua (`eval`) on Lua configs (Hyprland 0.53 and newer)
//! and with `keyword` on `hyprland.conf` ones.

pub mod pointer;
pub mod screencopy;
pub mod windows;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{info, warn};
use serde_json::Value;

use crate::encoder::Encoder;
use crate::platform::linux::{is_panel, named_virtual};
use crate::platform::{
    AddModel, Cursor, CursorSink, CursorSource, DeskPlan, Desktop, DesktopCaps, FrameSource,
    Geometry, GpuDevices, Memory, Origin, Output, OutputHealth, OutputKey, Outputs,
    PhysicalDisplay, Pointer, Scales, VirtualSpec, Windows,
};
use pointer::{PointerSession, SinkSlot, Units};
use screencopy::Capture;
use windows::HyprWindows;

/// Names of Prostir's outputs; anything else is a real display.
pub const PREFIX: &str = "Prostir-";

/// Whether Prostir runs inside Hyprland.
pub fn running() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

fn socket_path() -> Option<PathBuf> {
    socket("socket")
}

/// `.socket.sock` takes requests, `.socket2.sock` streams events.
fn socket(kind: &str) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(
        PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(format!(".{kind}.sock")),
    )
}

/// One IPC request, e.g. `j/monitors all` or `output create headless Prostir-1`.
pub fn request(command: &str) -> Result<String> {
    let path = socket_path().context("not running inside Hyprland")?;
    let mut socket =
        UnixStream::connect(&path).with_context(|| format!("cannot reach Hyprland at {path:?}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.write_all(command.as_bytes())?;
    let mut reply = String::new();
    socket.read_to_string(&mut reply)?;
    Ok(reply)
}

/// An output as Hyprland reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub name: String,
    /// Mode in pixels.
    pub width: i32,
    pub height: i32,
    pub refresh: f64,
    /// Logical position.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub focused: bool,
    pub disabled: bool,
    /// What the display says it is (its EDID), each empty when it says nothing.
    pub make: String,
    pub model: String,
    pub serial: String,
}

impl Monitor {
    fn from_json(v: &Value) -> Option<Self> {
        let int = |k: &str| v.get(k)?.as_i64().map(|n| n as i32);
        let text = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        Some(Self {
            name: v.get("name")?.as_str()?.to_owned(),
            width: int("width")?,
            height: int("height")?,
            refresh: v.get("refreshRate").and_then(Value::as_f64).unwrap_or(60.0),
            x: int("x")?,
            y: int("y")?,
            scale: v.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
            focused: v.get("focused").and_then(Value::as_bool).unwrap_or(false),
            disabled: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            make: text("make"),
            model: text("model"),
            serial: text("serial"),
        })
    }

    /// Size on the desktop, after scaling.
    pub fn logical(&self) -> (i32, i32) {
        (
            (f64::from(self.width) / self.scale).round() as i32,
            (f64::from(self.height) / self.scale).round() as i32,
        )
    }
}

pub fn parse_monitors(json: &str) -> Vec<Monitor> {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| {
            v.as_array()
                .map(|a| a.iter().filter_map(Monitor::from_json).collect())
        })
        .unwrap_or_default()
}

/// All outputs, including disabled ones.
pub fn monitors() -> Vec<Monitor> {
    request("j/monitors all")
        .map(|json| parse_monitors(&json))
        .unwrap_or_default()
}

/// The output with the mouse (Hyprland's focused monitor).
fn focused() -> Option<String> {
    let json = request("j/monitors").ok()?;
    parse_monitors(&json)
        .into_iter()
        .find(|m| m.focused)
        .map(|m| m.name)
}

fn geometries() -> Outputs {
    monitors()
        .into_iter()
        .map(|m| {
            let (w, h) = m.logical();
            let g = Geometry {
                x: m.x,
                y: m.y,
                w,
                h,
                priority: None,
                enabled: !m.disabled,
                is_virtual: is_virtual(&m.name),
            };
            (OutputKey::new(m.name), g)
        })
        .collect()
}

/// Whether Hyprland's output `name` is virtual: ours, or another tool's headless one
/// (`HEADLESS-N`, from Sunshine or wayvnc), which the layout leaves where it is.
fn is_virtual(name: &str) -> bool {
    named_virtual(name) || name.starts_with("HEADLESS-")
}

/// The display to show by default: an external monitor if one is on (the one in front
/// of you, as in KDE where the laptop panel goes off with the lid), else the laptop
/// panel, but never the panel of a closed laptop.
fn pick_physical(monitors: &[Monitor], lid_open: bool) -> Option<String> {
    let mut names: Vec<&str> = real(monitors, lid_open).map(|m| m.name.as_str()).collect();
    names.sort_by_key(|n| (is_panel(n), *n));
    names.first().map(|n| (*n).to_owned())
}

/// The displays of the PC itself that are on: not Prostir's or other tools' virtual
/// outputs, nor the panel of a closed laptop.
fn real(monitors: &[Monitor], lid_open: bool) -> impl Iterator<Item = &Monitor> {
    monitors
        .iter()
        .filter(|m| !m.disabled && !is_virtual(&m.name))
        .filter(move |m| lid_open || !is_panel(&m.name))
}

/// The displays of the PC itself connected and on now, and the one shown by default.
fn physical_displays() -> Vec<PhysicalDisplay> {
    let monitors = monitors();
    let lid_open = crate::platform::linux::system::lid_open();
    let default = pick_physical(&monitors, lid_open);
    real(&monitors, lid_open)
        .map(|m| {
            let said: Vec<&str> = [&m.make, &m.model, &m.serial]
                .into_iter()
                .map(String::as_str)
                .filter(|w| !w.is_empty())
                .collect();
            PhysicalDisplay {
                key: OutputKey::new(m.name.clone()),
                label: crate::platform::display_label(&m.make, &m.model, &m.name),
                edid: (!said.is_empty()).then(|| said.join(" ")),
                builtin: is_panel(&m.name),
                size: u32::try_from(m.width)
                    .ok()
                    .zip(u32::try_from(m.height).ok()),
                default: default.as_ref() == Some(&m.name),
            }
        })
        .collect()
}

/// A monitor rule: mode, logical position and scale of one output.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub refresh: f64,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
}

impl Rule {
    fn mode(&self) -> String {
        let refresh = format!("{:.3}", self.refresh);
        let refresh = refresh.trim_end_matches('0').trim_end_matches('.');
        format!("{}x{}@{refresh}", self.width, self.height)
    }

    pub fn lua(&self) -> String {
        format!(
            "hl.monitor({{output=\"{}\", mode=\"{}\", position=\"{}x{}\", scale={}}})",
            self.name,
            self.mode(),
            self.x,
            self.y,
            self.scale
        )
    }

    pub fn keyword(&self) -> String {
        format!(
            "keyword monitor {},{},{}x{},{}",
            self.name,
            self.mode(),
            self.x,
            self.y,
            self.scale
        )
    }
}

fn ok(reply: &str) -> bool {
    reply.trim() == "ok" || reply.lines().all(|l| l.trim() == "ok")
}

/// What Prostir keeps of Hyprland's monitor rules while it is the desktop: shared by
/// the desktop, its outputs (which forget their rules when removed) and the thread that
/// puts the rules back after a config reload.
#[derive(Default)]
struct Rules {
    /// Every rule Prostir has set, by output: a config reload drops runtime rules, and
    /// these are put back afterwards.
    applied: Mutex<BTreeMap<String, Rule>>,
    /// Which way monitor rules are accepted: 0 not known yet, 1 Lua, 2 keyword.
    dialect: AtomicU8,
    /// Where real displays stood before Prostir moved them, to put them back after.
    original: Mutex<BTreeMap<String, (i32, i32)>>,
    /// The reload watcher's event stream, closed when the desktop goes so the watcher
    /// ends.
    watch: Mutex<Watch>,
}

/// The reload watcher, as [`Rules::stop_watching`] finds it.
#[derive(Default)]
struct Watch {
    /// The event stream it reads now.
    events: Option<UnixStream>,
    /// The desktop is gone: the watcher keeps no stream and ends.
    stopped: bool,
}

/// The rules of the Hyprland desktop connected now, for the clean-up when quitting
/// hangs (see [`remove_leftovers`]), which runs on another thread while the desktop
/// still exists. Weak: the desktop owns its rules.
static LIVE: Mutex<Weak<Rules>> = Mutex::new(Weak::new());

impl Rules {
    /// Output `name` is at `scale` from now on, given with Hyprland's own means: the
    /// rule put back after a config reload has it.
    fn take_scale(&self, name: &str, scale: f64) {
        if let Some(rule) = self.applied.lock().unwrap().get_mut(name) {
            rule.scale = scale;
        }
    }

    /// Applies monitor rules in one request and remembers them.
    fn apply(&self, rules: &[Rule]) -> bool {
        let mut applied = self.applied.lock().unwrap();
        for r in rules {
            applied.insert(r.name.clone(), r.clone());
        }
        drop(applied);
        self.send(rules)
    }

    /// Hyprland re-reads its config whenever the file is saved, which resets our
    /// outputs to its own rules. Watches for that and puts Prostir's layout back, until
    /// the rules are dropped (see [`Rules::stop_watching`]).
    fn keep_after_reloads(self: &Arc<Self>) {
        let rules = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("hyprland-events".into())
            .spawn(move || {
                loop {
                    if let Some(path) = socket("socket2")
                        && let Ok(stream) = UnixStream::connect(path)
                    {
                        let Some(alive) = rules.upgrade() else {
                            return;
                        };
                        // Under the same lock as `stop_watching`: a desktop gone in
                        // between is seen here, and the stream is not kept.
                        let mut watch = alive.watch.lock().unwrap();
                        if watch.stopped {
                            return;
                        }
                        watch.events = stream.try_clone().ok();
                        drop(watch);
                        drop(alive);
                        for line in BufReader::new(stream).lines() {
                            let Ok(line) = line else { break };
                            if rules.strong_count() == 0 {
                                return;
                            }
                            if line.starts_with("configreloaded>>") {
                                std::thread::sleep(Duration::from_millis(200));
                                let Some(alive) = rules.upgrade() else {
                                    return;
                                };
                                alive.reapply();
                            }
                        }
                    }
                    let stopped = rules
                        .upgrade()
                        .is_none_or(|alive| alive.watch.lock().unwrap().stopped);
                    if stopped {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            });
        if let Err(e) = spawned {
            warn!("cannot watch Hyprland events: {e}");
        }
    }

    /// Ends the reload watcher: its event stream closes under it.
    fn stop_watching(&self) {
        let mut watch = self.watch.lock().unwrap();
        watch.stopped = true;
        if let Some(stream) = watch.events.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Puts back the remembered rules: always for our outputs, and for real displays
    /// only while they are on (the config may have just switched one off, e.g. a closed
    /// lid).
    fn reapply(&self) {
        let present = monitors();
        let rules: Vec<Rule> = self
            .applied
            .lock()
            .unwrap()
            .values()
            .filter(|r| {
                r.name.starts_with(PREFIX)
                    || present.iter().any(|m| m.name == r.name && !m.disabled)
            })
            .cloned()
            .collect();
        if !rules.is_empty() {
            info!(
                "Hyprland reloaded its config: restoring {} monitor rules",
                rules.len()
            );
            let _ = self.send(&rules);
        }
    }

    /// Sends `rules` to Hyprland; whether it took them.
    fn send(&self, rules: &[Rule]) -> bool {
        if rules.is_empty() {
            return true;
        }
        let lua = || {
            format!(
                "eval {}",
                rules.iter().map(Rule::lua).collect::<Vec<_>>().join(" ")
            )
        };
        let keyword = || {
            let all: Vec<String> = rules.iter().map(Rule::keyword).collect();
            format!("[[BATCH]]{}", all.join(";"))
        };
        let attempt = |dialect: u8| {
            let command = if dialect == 1 { lua() } else { keyword() };
            match request(&command) {
                Ok(reply) if ok(&reply) => true,
                Ok(reply) => {
                    if self.dialect.load(Ordering::Relaxed) != 0 {
                        warn!("Hyprland refused {command:?}: {}", reply.trim());
                    }
                    false
                }
                Err(e) => {
                    warn!("Hyprland: {e:#}");
                    false
                }
            }
        };
        match self.dialect.load(Ordering::Relaxed) {
            0 => {
                for dialect in [1, 2] {
                    if attempt(dialect) {
                        self.dialect.store(dialect, Ordering::Relaxed);
                        return true;
                    }
                }
                warn!("Hyprland accepted no monitor rule for {rules:?}");
                false
            }
            d => attempt(d),
        }
    }

    /// Creates a headless output (or takes over one left from an earlier run) and
    /// gives it the size of `spec`. Placement comes later, with the whole layout.
    fn create(&self, spec: &VirtualSpec) -> Result<String> {
        let number = spec.number;
        let name = output_name(number);
        if !monitors().iter().any(|m| m.name == name) {
            let reply = request(&format!("output create headless {name}"))?;
            if !ok(&reply) {
                bail!("Hyprland cannot create {name}: {}", reply.trim());
            }
        }
        // Out of the way until the layout is applied, so it overlaps nothing.
        self.apply(&[Rule {
            name: name.clone(),
            width: spec.w as i32,
            height: spec.h as i32,
            refresh: f64::from(spec.refresh),
            x: 100_000 + 10_000 * number as i32,
            y: 0,
            scale: spec.factor(),
        }]);
        info!("created {name} ({spec})");
        Ok(name)
    }

    fn remove(&self, name: &str) {
        self.applied.lock().unwrap().remove(name);
        match request(&format!("output remove {name}")) {
            Ok(reply) if ok(&reply) => info!("removed {name}"),
            Ok(reply) => warn!("Hyprland did not remove {name}: {}", reply.trim()),
            Err(e) => warn!("cannot remove {name}: {e:#}"),
        }
    }

    /// Removes outputs a previous run left behind (Hyprland keeps headless outputs
    /// after the program that made them is gone), so the windows on them come back.
    fn remove_leftovers(&self) {
        self.restore_real_outputs();
        for m in monitors() {
            if m.name.starts_with(PREFIX) {
                self.remove(&m.name);
            }
        }
    }

    /// Puts the real displays back where they were and forgets the rules for them, so
    /// a later config reload does not move them again.
    fn restore_real_outputs(&self) {
        let original = std::mem::take(&mut *self.original.lock().unwrap());
        {
            let mut applied = self.applied.lock().unwrap();
            for name in original.keys() {
                applied.remove(name);
            }
        }
        let current = monitors();
        let rules: Vec<Rule> = original
            .into_iter()
            .filter_map(|(name, (x, y))| {
                let m = current.iter().find(|m| m.name == name && !m.disabled)?;
                ((m.x, m.y) != (x, y)).then_some(Rule {
                    name,
                    width: m.width,
                    height: m.height,
                    refresh: m.refresh,
                    x,
                    y,
                    scale: m.scale,
                })
            })
            .collect();
        if !rules.is_empty() {
            info!("putting {} display(s) back where they were", rules.len());
            let _ = self.send(&rules);
        }
    }

    /// Moves outputs to new logical positions, keeping their modes, and their scales
    /// but for those of `scales` (outputs of ours: their positions are for the new
    /// size); whether Hyprland took them.
    fn place(&self, positions: &[(String, i32, i32)], scales: &[(String, f64)]) -> bool {
        let current = monitors();
        {
            let mut original = self.original.lock().unwrap();
            for (name, _, _) in positions {
                if !name.starts_with(PREFIX)
                    && let Some(m) = current.iter().find(|m| &m.name == name)
                {
                    original.entry(name.clone()).or_insert((m.x, m.y));
                }
            }
        }
        let new_scale = |name: &str| {
            scales
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, scale)| *scale)
        };
        let mut rules: Vec<Rule> = positions
            .iter()
            .filter_map(|(name, x, y)| {
                let m = current.iter().find(|m| &m.name == name)?;
                let scale = new_scale(name).unwrap_or(m.scale);
                ((m.x, m.y) != (*x, *y) || scale != m.scale).then(|| Rule {
                    name: name.clone(),
                    width: m.width,
                    height: m.height,
                    refresh: m.refresh,
                    x: *x,
                    y: *y,
                    scale,
                })
            })
            .collect();
        // A new scale for an output the plan does not move.
        for (name, scale) in scales {
            if rules.iter().any(|r| &r.name == name) {
                continue;
            }
            if let Some(m) = current.iter().find(|m| &m.name == name) {
                rules.push(Rule {
                    name: name.clone(),
                    width: m.width,
                    height: m.height,
                    refresh: m.refresh,
                    x: m.x,
                    y: m.y,
                    scale: *scale,
                });
            }
        }
        self.apply_without_overlaps(&current, &rules)
    }

    /// Applies the rules; if doing so one by one would overlap outputs for a moment
    /// (Hyprland warns about that), first moves the outputs involved out of the way.
    fn apply_without_overlaps(&self, current: &[Monitor], rules: &[Rule]) -> bool {
        if overlaps_on_the_way(current, rules) {
            let aside: Vec<Rule> = rules
                .iter()
                .enumerate()
                .map(|(i, r)| Rule {
                    x: 200_000 + 10_000 * i as i32,
                    y: 0,
                    ..r.clone()
                })
                .collect();
            let _ = self.send(&aside);
        }
        self.apply(rules)
    }
}

/// Our output's name for monitor number `number` (1-based).
pub fn output_name(number: u32) -> String {
    format!("{PREFIX}{number}")
}

/// Removes the outputs a run that crashed left behind (see
/// `system::clean_up_after_crash`): at the start, before any desktop is connected, and
/// when quitting hangs, with the rules of the desktop still connected, so the real
/// displays it moved go back too.
pub fn remove_leftovers() {
    let live = LIVE.lock().unwrap().upgrade();
    live.unwrap_or_default().remove_leftovers();
}

/// Logical rectangle `(x, y, w, h)` of a rule.
fn rect(r: &Rule) -> (i32, i32, i32, i32) {
    (
        r.x,
        r.y,
        (f64::from(r.width) / r.scale).round() as i32,
        (f64::from(r.height) / r.scale).round() as i32,
    )
}

fn overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

/// Whether applying `rules` one after another (as Hyprland does) passes through a
/// moment where two outputs overlap.
fn overlaps_on_the_way(current: &[Monitor], rules: &[Rule]) -> bool {
    let mut at: Vec<(String, (i32, i32, i32, i32))> = current
        .iter()
        .filter(|m| !m.disabled)
        .map(|m| {
            let (w, h) = m.logical();
            (m.name.clone(), (m.x, m.y, w, h))
        })
        .collect();
    for r in rules {
        let moved = rect(r);
        if at
            .iter()
            .any(|(n, other)| n != &r.name && overlap(moved, *other))
        {
            return true;
        }
        match at.iter_mut().find(|(n, _)| n == &r.name) {
            Some(entry) => entry.1 = moved,
            None => at.push((r.name.clone(), moved)),
        }
    }
    false
}

/// Hyprland as the desktop.
pub struct Hyprland {
    caps: DesktopCaps,
    windows: HyprWindows,
    rules: Arc<Rules>,
    /// The pointer apart from the frames, where Hyprland offers pointer sessions.
    cursor: Option<HyprCursor>,
    /// The graphics devices to share (wlr-screencopy hands buffers over: none taken
    /// yet).
    _gpu: GpuDevices,
}

impl Hyprland {
    /// Takes over the desktop: removes what an earlier run left and keeps Prostir's
    /// monitor rules across config reloads.
    pub fn connect(gpu: GpuDevices) -> Self {
        let rules = Arc::new(Rules::default());
        rules.remove_leftovers();
        rules.keep_after_reloads();
        *LIVE.lock().unwrap() = Arc::downgrade(&rules);
        Self {
            caps: DesktopCaps {
                name: "Hyprland",
                virtual_monitors: Ok(AddModel::OneByOne),
                has_primary: false,
                origin_is_primary: false,
                scales: Scales::Any { min: 50, max: 400 },
                // A monitor rule with another scale, taken live.
                scales_in_place: true,
                refresh: vec![60],
                screenshot_one_output: true,
                moves_pointer: true,
                // Hyprland keeps headless outputs after the program that made them.
                cleans_up_after_crash: false,
            },
            windows: HyprWindows::new(),
            rules,
            cursor: cursor_apart().then(HyprCursor::default),
            _gpu: gpu,
        }
    }
}

/// Whether the pointer can go apart from the frames: Hyprland offers pointer sessions
/// (0.54 and newer), and gives Prostir the pointer's place without asking (with
/// `ecosystem:enforce_permissions` it may not: its pictures would be black and the
/// pointer gone). Says why not, once.
fn cursor_apart() -> bool {
    let enforced =
        request("j/getoption ecosystem:enforce_permissions").is_ok_and(|reply| option_on(&reply));
    let why = if !pointer::offered() {
        "no pointer sessions (ext-image-copy-capture, Hyprland 0.54 and newer)"
    } else if enforced {
        "Hyprland enforces permissions (ecosystem:enforce_permissions)"
    } else {
        info!("Hyprland: the pointer can go apart from the frames");
        return true;
    };
    info!("Hyprland: the pointer stays in the frames: {why}");
    false
}

/// Whether `getoption`'s answer (JSON) says the option is on.
fn option_on(reply: &str) -> bool {
    serde_json::from_str::<Value>(reply)
        .ok()
        .and_then(|v| v.get("int").and_then(Value::as_i64))
        .is_some_and(|n| n != 0)
}

/// How Hyprland counts what it says of the pointer on output `name` (see
/// [`Units::hyprland`]): its scale, and the highest one of the outputs that are on,
/// at least 1 (Hyprland's own pointers are drawn at it).
fn units_of(name: &str) -> Units {
    let all = monitors();
    let on = all.iter().filter(|m| !m.disabled);
    let highest = on.clone().map(|m| m.scale).fold(1.0, f64::max);
    let scale = on.clone().find(|m| m.name == name).map_or(1.0, |m| m.scale);
    Units::hyprland(scale, highest)
}

/// The pointer apart from the frames: each output's pointer session tells the core's
/// sink, kept here. Switching costs nothing: each frame is captured with the pointer
/// in or out as asked then.
#[derive(Default)]
struct HyprCursor {
    sink: SinkSlot,
}

impl CursorSource for HyprCursor {
    fn set_apart(&mut self, sink: Option<CursorSink>) {
        *self.sink.lock().unwrap() = sink;
    }
}

impl Drop for Hyprland {
    fn drop(&mut self) {
        // Real displays moved for the layout go back where they were.
        self.rules.restore_real_outputs();
        self.rules.stop_watching();
    }
}

impl Desktop for Hyprland {
    fn caps(&self) -> &DesktopCaps {
        &self.caps
    }

    fn outputs(&self) -> Outputs {
        geometries()
    }

    fn physical(&self) -> Vec<PhysicalDisplay> {
        physical_displays()
    }

    fn make(&mut self, spec: &VirtualSpec) -> Result<Box<dyn Output>> {
        let name = self.rules.create(spec)?;
        Ok(Box::new(HyprOutput::new(
            name,
            Some(self.rules.clone()),
            self.cursor.as_ref().map(|c| c.sink.clone()),
        )))
    }

    fn show(&mut self, key: &OutputKey) -> Result<Box<dyn Output>> {
        Ok(Box::new(HyprOutput::new(
            key.to_string(),
            None,
            self.cursor.as_ref().map(|c| c.sink.clone()),
        )))
    }

    /// Scales of whole logical pixels only (see [`whole_pixel_scales`]).
    fn scales_for(&self, w: u32, h: u32) -> Scales {
        whole_pixel_scales(w, h)
    }

    /// Hyprland has no primary output. New scales go in the monitor rules with the
    /// places, live.
    fn place(&mut self, plan: &DeskPlan, _current: &Outputs) -> Result<()> {
        let positions: Vec<(String, i32, i32)> = plan
            .positions
            .iter()
            .map(|(key, x, y)| (key.to_string(), *x, *y))
            .collect();
        let scales: Vec<(String, f64)> = plan
            .scales
            .iter()
            .map(|(key, percent)| (key.to_string(), f64::from(*percent) / 100.0))
            .collect();
        anyhow::ensure!(
            self.rules.place(&positions, &scales),
            "Hyprland refused the layout"
        );
        Ok(())
    }

    fn windows(&mut self) -> Option<&mut dyn Windows> {
        Some(&mut self.windows)
    }

    fn pointer(&mut self) -> Option<&mut dyn Pointer> {
        Some(&mut self.windows)
    }

    fn cursor(&mut self) -> Option<&mut dyn CursorSource> {
        match &mut self.cursor {
            Some(c) => Some(c),
            None => None,
        }
    }
}

/// The scales, in steps of 5 % from 50 % to 400 %, that Hyprland gives an output of
/// `w`x`h` pixels: those that make it a whole number of logical pixels both ways
/// (2560x1440 at 150 % would be 1706.67 wide, which Hyprland does not take; 125 % and
/// 200 % it does). A step of 5 % is a whole number of KWin's and Wayland's 1/120.
pub fn whole_pixel_scales(w: u32, h: u32) -> Scales {
    let whole = |px: u32, percent: u32| (px * 100).is_multiple_of(percent);
    Scales::Steps(
        (10..=80)
            .map(|fives| fives * 5)
            .filter(|&p| whole(w, p) && whole(h, p))
            .filter_map(|p| u16::try_from(p).ok())
            .collect(),
    )
}

/// How long the scale Hyprland said for an output of ours is taken as it is.
const SCALE_READ: Duration = Duration::from_secs(2);

/// A headless output of ours, or a display of the PC, captured with wlr-screencopy.
///
/// While the pointer is apart (and it is no window panel), its pointer session tells
/// the pointer and has the capture leave it out of the frames; it runs while the
/// output does, whatever its encoder does.
struct HyprOutput {
    key: OutputKey,
    // Field order matters: the pointer session ends before the capture.
    pointer: Option<PointerSession>,
    capture: Option<Capture>,
    /// Ours (removed when dropped, with its rule), not a display of the PC shown as it
    /// is.
    owned: Option<Arc<Rules>>,
    /// When Hyprland was last asked for its scale, and what it said.
    scale_read: Cell<(Option<Instant>, Option<u16>)>,
    /// The core's sink for the pointer, where Hyprland offers pointer sessions.
    cursor: Option<SinkSlot>,
    /// The frames leave the pointer out (its session says so while the headset draws
    /// it), read for each frame.
    apart: Arc<AtomicBool>,
}

impl HyprOutput {
    fn new(name: String, owned: Option<Arc<Rules>>, cursor: Option<SinkSlot>) -> Self {
        Self {
            key: OutputKey::new(name),
            pointer: None,
            capture: None,
            owned,
            scale_read: Cell::new((None, None)),
            cursor,
            apart: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for HyprOutput {
    fn drop(&mut self) {
        // Stop capturing before the output goes away.
        self.pointer.take();
        self.capture.take();
        if let Some(rules) = &self.owned {
            rules.remove(self.key.as_str());
        }
    }
}

impl Output for HyprOutput {
    fn key(&self) -> &OutputKey {
        &self.key
    }

    /// Asked of Hyprland's socket at most every [`SCALE_READ`], for an output of ours.
    fn desk_scale(&self) -> Option<u16> {
        self.owned.as_ref()?;
        let (at, said) = self.scale_read.get();
        if at.is_some_and(|t| t.elapsed() < SCALE_READ) {
            return said;
        }
        let said = monitors()
            .iter()
            .find(|m| m.name == self.key.as_str())
            .map(|m| (m.scale * 100.0).round() as u16);
        self.scale_read.set((Some(Instant::now()), said));
        said
    }

    fn scale_taken(&self, percent: u16) {
        if let Some(rules) = &self.owned {
            rules.take_scale(self.key.as_str(), f64::from(percent) / 100.0);
        }
    }

    /// Its outputs change size only through monitor rules, not through capture.
    fn health(&self) -> OutputHealth {
        if self.capture.as_ref().is_some_and(Capture::closed) {
            OutputHealth::Closed
        } else {
            OutputHealth::Live
        }
    }

    fn source(&self) -> FrameSource {
        FrameSource {
            input: screencopy::input(self.key.as_str()),
            origin: Origin::Capture,
            // GPU buffers for an encoder that takes them, else shared memory.
            memory: Memory::DmaBuf,
            cursor: if self.pointer.is_some() {
                Cursor::Separate
            } else {
                Cursor::InFrame
            },
        }
    }

    fn attach(&mut self, encoder: &Encoder) -> Result<()> {
        let appsrc = encoder
            .appsrc()
            .ok_or_else(|| anyhow::anyhow!("the encoder takes no pushed frames"))?;
        self.capture = Some(Capture::start(
            self.key.to_string(),
            appsrc,
            encoder.paused(),
            encoder.takes_dmabuf(),
            self.apart.clone(),
        )?);
        Ok(())
    }

    /// At once, also for the running capture: its session starts or ends (without
    /// pointer sessions the pointer stays in the frames).
    fn set_cursor_apart(&mut self, apart: bool) {
        let Some(sink) = self.cursor.clone().filter(|_| apart) else {
            self.pointer = None;
            return;
        };
        if self.pointer.is_some() {
            return;
        }
        let name = self.key.to_string();
        let units = {
            let name = name.clone();
            Box::new(move || units_of(&name))
        };
        self.pointer = PointerSession::start(name, sink, self.apart.clone(), units)
            .inspect_err(|e| warn!("{}: no pointer session: {e:#}", self.key))
            .ok();
    }

    fn detach(&mut self) {
        self.capture = None;
    }

    /// Hyprland has no such thing.
    fn make_primary(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hyprland's scales are those of whole logical pixels: 150 % gives 1920x1080
    /// and 3840x2160 but not 2560x1440 (1706.67) nor its portrait; 125 % and 200 %
    /// give every size Prostir offers.
    #[test]
    fn scales_of_whole_logical_pixels() {
        let allows = |w, h, p| whole_pixel_scales(w, h).allows(p);
        for (w, h) in crate::publish::SIZES {
            assert!(allows(w, h, 100) && allows(w, h, 125) && allows(w, h, 200));
        }
        assert!(allows(1920, 1080, 150) && allows(3840, 2160, 150));
        assert!(!allows(2560, 1440, 150) && !allows(1440, 2560, 150));
        assert!(!allows(1920, 1080, 175) && !allows(2560, 1440, 175));
        assert!(!allows(2560, 1440, 133) && allows(2560, 1440, 160));
    }

    const MONITORS: &str = r#"[{"id":0,"name":"eDP-1","width":2880,"height":1800,
        "refreshRate":120.00000,"x":0,"y":0,"scale":2.0,"focused":false,"disabled":false},
        {"id":1,"name":"Prostir-1","width":2560,"height":1440,"refreshRate":60.0,
        "x":1440,"y":0,"scale":1.0,"focused":true,"disabled":false}]"#;

    /// `getoption` as Hyprland answers it.
    #[test]
    fn reads_whether_an_option_is_on() {
        assert!(option_on(
            r#"{"option": "ecosystem:enforce_permissions", "int": 1, "set": true}"#
        ));
        assert!(!option_on(
            r#"{"option": "ecosystem:enforce_permissions", "int": 0, "set": false}"#
        ));
        assert!(!option_on("no such option"));
    }

    /// Without pointer sessions the pointer stays in the frames: asked apart, an
    /// output starts none and its frames keep the pointer.
    #[test]
    fn without_pointer_sessions_the_pointer_stays_in_the_frames() {
        let mut output = HyprOutput::new(output_name(1), None, None);
        output.set_cursor_apart(true);
        assert!(output.pointer.is_none());
        assert_eq!(output.source().cursor, Cursor::InFrame);
        assert!(!output.apart.load(Ordering::Relaxed));
    }

    #[test]
    fn other_tools_headless_outputs_are_virtual() {
        assert!(is_virtual("HEADLESS-2"));
        assert!(is_virtual(&output_name(1)));
        assert!(!is_virtual("eDP-1"));
        assert!(!is_virtual("HDMI-A-1"));
    }

    #[test]
    fn reads_monitors() {
        let m = parse_monitors(MONITORS);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].logical(), (1440, 900));
        assert!(m[1].focused);
        assert_eq!(m[1].name, output_name(1));
    }

    #[test]
    fn writes_rules_in_both_dialects() {
        let r = Rule {
            name: "Prostir-2".into(),
            width: 1920,
            height: 1080,
            refresh: 60.0,
            x: -1920,
            y: 360,
            scale: 1.25,
        };
        assert_eq!(
            r.lua(),
            r#"hl.monitor({output="Prostir-2", mode="1920x1080@60", position="-1920x360", scale=1.25})"#
        );
        assert_eq!(
            r.keyword(),
            "keyword monitor Prostir-2,1920x1080@60,-1920x360,1.25"
        );
        let odd = Rule {
            refresh: 59.951,
            ..r
        };
        assert!(odd.lua().contains("1920x1080@59.951"));
    }

    #[test]
    fn mirrors_the_external_monitor_and_never_a_closed_lid() {
        let mut m = parse_monitors(MONITORS);
        m.push(Monitor {
            name: "HDMI-A-1".into(),
            ..m[0].clone()
        });
        assert_eq!(pick_physical(&m, true).as_deref(), Some("HDMI-A-1"));
        m.pop();
        assert_eq!(pick_physical(&m, true).as_deref(), Some("eDP-1"));
        assert_eq!(pick_physical(&m, false), None);
    }

    #[test]
    fn notices_overlaps_on_the_way() {
        let hdmi = Monitor {
            name: "HDMI-A-1".into(),
            width: 1920,
            height: 1080,
            refresh: 60.0,
            x: 0,
            y: 0,
            scale: 1.0,
            focused: false,
            disabled: false,
            make: String::new(),
            model: String::new(),
            serial: String::new(),
        };
        let rule = |name: &str, x, y| Rule {
            name: name.into(),
            width: 2560,
            height: 1440,
            refresh: 60.0,
            x,
            y,
            scale: 1.0,
        };
        let hdmi_down = Rule {
            name: "HDMI-A-1".into(),
            width: 1920,
            height: 1080,
            ..rule("", 320, 2000)
        };
        // Prostir-3 lands on the HDMI display before the HDMI display moves away.
        assert!(overlaps_on_the_way(
            std::slice::from_ref(&hdmi),
            &[rule("Prostir-3", 0, 560), hdmi_down.clone()]
        ));
        // The other way round there is no such moment.
        assert!(!overlaps_on_the_way(
            &[hdmi],
            &[hdmi_down, rule("Prostir-3", 0, 560)]
        ));
    }
}
