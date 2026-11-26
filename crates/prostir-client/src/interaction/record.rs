//! Recordings of the input (step 8, section 8): every frame's [`FrameInput`] for a
//! while, with what stood in the room at the start, so what the hands did can be played
//! again on the desktop and written down as a journal of what they asked of the room.
//!
//! In the preview build "Record the input, 30 s" in "For development" fills a
//! [`Recorder`] made at once for all of it (no memory is taken in a frame) and hands
//! its bytes to the shell at the end, which writes `input-<date>.rec` into the app's
//! external folder (taken with `adb pull`). On the desktop [`replay`] plays a file
//! against a [`Stage`] made from its own snapshot and gives the journal; the tests keep
//! journals next to their scripted recordings (`tests/input/*.expected`), and
//! `cargo run -p prostir-client --example replay -- file.rec` prints one.
//!
//! The file: text lines up to `frames` (the format, the app's version, the numbers of
//! `feel.txt` that differ from the defaults, the stage), then the frames, binary, little
//! endian (see [`Recorder::push`]).

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use glam::{Quat, Vec2, Vec3};

use super::{
    Button, FrameInput, HandState, Head, Joints, Menu, Menus, Modes, MouseButton, PcInput, Scene,
    Take, TouchPhase, Touched,
};
use crate::ids::{DisplayKey, Target, Thing};
use crate::layout::{Hit, Placement, Ray, facing};
use crate::ui::{self, Action};
use crate::world::{panel_height, panel_width, wrist_menu_height};

use super::feel::Feel;

/// The first line of a recording.
pub const FORMAT: &str = "prostir-input 1";
/// How long "Record the input" records.
pub const RECORD_FOR: Duration = Duration::from_secs(30);
/// The most frames a second it keeps room for.
const RATE_MAX: u32 = 120;
/// The most bytes one frame takes.
const FRAME_MAX: usize = 4 + 1 + 4 + 36 + 2 * (2 + 24 + 8 + 26 * 16);

// ---- What stood in the room ----

/// Something a ray can point at, as it stood.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    pub thing: Thing,
    pub place: Placement,
    pub height: f32,
    /// A detached window (it shows "Attach").
    pub window: bool,
}

/// What stood in the room at the start of a recording, and what playing it changes:
/// the menu, the monitors and the keyboard window, all flat (a curve is not kept).
/// It is the [`Scene`] a recording is played against, writing down what it is asked
/// (see [`Stage::journal`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub menu: Placement,
    pub menu_open: bool,
    pub large_menu: bool,
    pub left_handed: bool,
    /// What the hands could do beyond the input before step 8.
    pub modes: Modes,
    pub pieces: Vec<Piece>,
    /// The numbers played with (the recording's).
    feel: Feel,
    wrist: Option<Placement>,
    held: Vec<Thing>,
    resizing: Option<DisplayKey>,
    start: Option<Instant>,
    now: Option<Instant>,
    journal: Vec<Line>,
}

/// One line of the journal: from when to when (several of the same in a row are one
/// line), what, how many, and how it ended.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    from: Duration,
    to: Duration,
    what: String,
    count: u32,
    tail: String,
}

impl Stage {
    pub fn new(menu: Placement, menu_open: bool, pieces: Vec<Piece>) -> Self {
        Self {
            menu,
            menu_open,
            large_menu: false,
            left_handed: false,
            modes: Modes::default(),
            pieces,
            feel: Feel::DEFAULT,
            wrist: None,
            held: Vec::new(),
            resizing: None,
            start: None,
            now: None,
            journal: Vec::new(),
        }
    }

    /// The time of the next frame played.
    fn at(&mut self, start: Instant, now: Instant) {
        self.start = Some(start);
        self.now = Some(now);
    }

    fn since(&self) -> Duration {
        match (self.start, self.now) {
            (Some(start), Some(now)) => now.saturating_duration_since(start),
            _ => Duration::ZERO,
        }
    }

    /// `what` was asked (`tail`: how it ended, for a line that repeats).
    fn note(&mut self, what: String, tail: String) {
        let at = self.since();
        if let Some(last) = self.journal.last_mut()
            && last.what == what
            && !tail.is_empty()
        {
            last.to = at;
            last.count += 1;
            last.tail = tail;
            return;
        }
        self.journal.push(Line {
            from: at,
            to: at,
            what,
            count: 1,
            tail,
        });
    }

    /// The journal, one line each: `1.234 take menu`, or for a run of the same
    /// `1.245..1.600 drag menu x33 at (0.10, 1.32, -0.60)`.
    pub fn journal(&self) -> Vec<String> {
        self.journal
            .iter()
            .map(|l| {
                let mut s = format!("{:.3}", l.from.as_secs_f32());
                if l.count > 1 {
                    let _ = write!(s, "..{:.3}", l.to.as_secs_f32());
                }
                let _ = write!(s, " {}", l.what);
                if l.count > 1 {
                    let _ = write!(s, " x{}", l.count);
                }
                if !l.tail.is_empty() {
                    let _ = write!(s, " {}", l.tail);
                }
                s
            })
            .collect()
    }

    fn piece(&self, thing: Thing) -> Option<&Piece> {
        self.pieces.iter().find(|p| p.thing == thing)
    }

    fn piece_mut(&mut self, thing: Thing) -> Option<&mut Piece> {
        self.pieces.iter_mut().find(|p| p.thing == thing)
    }

    // ---- As text, in a recording's head ----

    fn write(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "menu {} {}",
            u8::from(self.menu_open),
            placement_text(&self.menu)
        );
        let _ = writeln!(out, "large-menu {}", u8::from(self.large_menu));
        let _ = writeln!(out, "left-handed {}", u8::from(self.left_handed));
        let m = self.modes;
        let take = match m.take {
            Take::Fist => "fist",
            Take::Middle => "middle",
            Take::Hold => "hold",
        };
        let (monitors, hands) = (u8::from(m.monitors), u8::from(m.hands));
        let _ = writeln!(out, "modes {monitors} {take} {hands}");
        for p in &self.pieces {
            let what = match p.thing {
                Thing::Display(id) => format!("display {id}"),
                Thing::Keyboard => "keyboard".to_owned(),
                _ => continue,
            };
            let _ = writeln!(
                out,
                "{what} {} {} {}",
                u8::from(p.window),
                p.height,
                placement_text(&p.place)
            );
        }
    }

    fn read<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Self, String> {
        let mut stage = Stage::new(
            Placement {
                position: Vec3::new(0.0, 1.3, -0.6),
                rotation: Quat::IDENTITY,
                width: panel_width(false),
            },
            false,
            Vec::new(),
        );
        for line in lines {
            let mut words = line.split_whitespace();
            let bad = || format!("stage: \"{line}\"");
            match words.next() {
                Some("menu") => {
                    stage.menu_open = words.next() == Some("1");
                    stage.menu = read_placement(&mut words).ok_or_else(bad)?;
                }
                Some("large-menu") => stage.large_menu = words.next() == Some("1"),
                Some("left-handed") => stage.left_handed = words.next() == Some("1"),
                Some("modes") => {
                    stage.modes.monitors = words.next() == Some("1");
                    stage.modes.take = match words.next() {
                        Some("fist") => Take::Fist,
                        Some("middle") => Take::Middle,
                        _ => Take::Hold,
                    };
                    stage.modes.hands = words.next() == Some("1");
                }
                Some(kind @ ("display" | "keyboard")) => {
                    let thing = if kind == "display" {
                        Thing::Display(words.next().and_then(DisplayKey::parse).ok_or_else(bad)?)
                    } else {
                        Thing::Keyboard
                    };
                    let window = words.next() == Some("1");
                    let height = words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?;
                    let place = read_placement(&mut words).ok_or_else(bad)?;
                    stage.pieces.push(Piece {
                        thing,
                        place,
                        height,
                        window,
                    });
                }
                // A line a later version added.
                _ => {}
            }
        }
        Ok(stage)
    }
}

fn placement_text(p: &Placement) -> String {
    let (q, v) = (p.rotation, p.position);
    format!(
        "{} {} {} {} {} {} {} {}",
        v.x, v.y, v.z, q.x, q.y, q.z, q.w, p.width
    )
}

fn read_placement<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<Placement> {
    let mut n = [0.0f32; 8];
    for v in &mut n {
        *v = words.next()?.parse().ok()?;
    }
    Some(Placement {
        position: Vec3::new(n[0], n[1], n[2]),
        rotation: Quat::from_xyzw(n[3], n[4], n[5], n[6]),
        width: n[7],
    })
}

/// How the journal names a thing.
fn name(thing: Thing) -> String {
    match thing {
        Thing::Display(id) => format!("display {}", id.id.0),
        Thing::Adding { .. } => "placeholder".to_owned(),
        Thing::Menu => "menu".to_owned(),
        Thing::MenuButton => "menu-button".to_owned(),
        Thing::Keyboard => "keyboard".to_owned(),
        Thing::Wrist => "palm-menu".to_owned(),
        Thing::Attach(id) => format!("attach {}", id.id.0),
    }
}

fn at(v: Vec3) -> String {
    format!("at ({:.2}, {:.2}, {:.2})", v.x, v.y, v.z)
}

/// Where `ray` meets the flat rectangle `place`, `height` high, from in front.
fn flat_hit(ray: Ray, place: &Placement, height: f32) -> Option<Hit> {
    let (t, local) = crate::layout::hit(ray, place, height)?;
    Some(Hit {
        t,
        local,
        point: ray.origin + ray.dir * t,
        normal: place.rotation * Vec3::Z,
    })
}

impl Scene for Stage {
    fn pick(&self, ray: Ray, attach: Option<DisplayKey>) -> Option<Target> {
        let mut best: Option<Target> = None;
        let mut consider = |thing: Thing, place: &Placement, height: f32| {
            if let Some(hit) = flat_hit(ray, place, height)
                && best.is_none_or(|b| hit.t < b.hit.t)
            {
                best = Some(Target { thing, hit });
            }
        };
        if self.menu_open {
            consider(Thing::Menu, &self.menu, panel_height(self.large_menu));
        }
        if let Some(w) = self.wrist {
            consider(Thing::Wrist, &w, wrist_menu_height());
        }
        for p in &self.pieces {
            consider(p.thing, &p.place, p.height);
        }
        // "Attach" is a strip under its window.
        if let Some(id) = attach
            && let Some(p) = self.piece(Thing::Display(id))
        {
            let mut under = p.place;
            under.position -= p.place.rotation * Vec3::Y * (p.height / 2.0 + 0.04);
            under.width = 0.2;
            if let Some(hit) = flat_hit(ray, &under, 0.06)
                && best.is_none_or(|b| hit.t < b.hit.t)
            {
                best = Some(Target {
                    thing: Thing::Attach(id),
                    hit,
                });
            }
        }
        best
    }
    fn shows(&self, id: DisplayKey) -> bool {
        self.piece(Thing::Display(id)).is_some()
    }
    fn is_window(&self, id: DisplayKey) -> bool {
        self.piece(Thing::Display(id)).is_some_and(|p| p.window)
    }
    fn menu(&self) -> Option<Placement> {
        self.menu_open.then_some(self.menu)
    }
    fn large_menu(&self) -> bool {
        self.large_menu
    }
    fn left_handed(&self) -> bool {
        self.left_handed
    }
    fn wrist(&self) -> Option<Placement> {
        self.wrist
    }
    fn place(&self, thing: Thing) -> Option<Placement> {
        match thing {
            Thing::Menu => Some(self.menu),
            _ => self.piece(thing).map(|p| p.place),
        }
    }
    fn holds(&self, thing: Thing) -> bool {
        self.held.contains(&thing)
    }
    fn resizing(&self) -> Option<DisplayKey> {
        self.resizing
    }
    fn feel(&self) -> &Feel {
        &self.feel
    }
    fn modes(&self) -> Modes {
        self.modes
    }
    fn touchable(&self, at: Vec3, only: Option<DisplayKey>) -> Option<Touched> {
        let mut best: Option<Touched> = None;
        for p in &self.pieces {
            let Thing::Display(id) = p.thing else {
                continue;
            };
            if p.window || only.is_some_and(|o| o != id) {
                continue;
            }
            let Some(a) = crate::layout::against(&p.place, p.height, None, at) else {
                continue;
            };
            if only.is_none() && a.z < -self.feel.touch_through_m {
                continue;
            }
            if best.is_none_or(|b| a.z.abs() < b.z.abs()) {
                best = Some(Touched {
                    id,
                    local: a.local,
                    z: a.z,
                    point: a.point,
                    normal: a.normal,
                    uv: crate::layout::image_uv(&p.place, p.height, None, a.local),
                });
            }
        }
        best
    }
    fn monitor_uv(&self, id: DisplayKey, local: Vec2) -> Option<Vec2> {
        let p = self.piece(Thing::Display(id))?;
        Some(crate::layout::image_uv(&p.place, p.height, None, local))
    }
    fn palm(&mut self, place: Option<Placement>) {
        if place.is_some() != self.wrist.is_some() {
            let shown = if place.is_some() { "shown" } else { "gone" };
            self.note(format!("palm-menu {shown}"), String::new());
        }
        self.wrist = place;
    }
    fn press_menu(&mut self, _now: Instant, action: Action) {
        self.note(format!("press-menu {action:?}"), String::new());
    }
    fn press(&mut self, _now: Instant, action: Action) {
        self.note(format!("press {action:?}"), String::new());
    }
    fn toggle_menu(&mut self) {
        self.menu_open = !self.menu_open;
        let open = if self.menu_open { "open" } else { "closed" };
        self.note(format!("menu {open}"), String::new());
    }
    fn align(&mut self, _now: Instant) {
        self.note("line-up".to_owned(), String::new());
    }
    fn move_menu(&mut self, position: Vec3, head: Vec3) {
        self.menu.position = position;
        self.menu.rotation = facing(position, head);
        self.note("move-menu".to_owned(), at(position));
    }
    fn hold(&mut self, thing: Thing) -> Option<Placement> {
        let at = self.place(thing)?;
        self.held.push(thing);
        self.note(format!("take {}", name(thing)), String::new());
        Some(at)
    }
    fn drag(&mut self, thing: Thing, place: Placement, _head: Vec3) {
        match thing {
            Thing::Menu => self.menu = place,
            _ => {
                if let Some(p) = self.piece_mut(thing) {
                    p.place = place;
                }
            }
        }
        self.note(format!("drag {}", name(thing)), at(place.position));
    }
    fn let_go(&mut self, _now: Instant, thing: Thing) -> bool {
        self.held.retain(|t| *t != thing);
        if self.resizing.is_some_and(|id| Thing::Display(id) == thing) {
            self.resizing = None;
        }
        self.note(format!("let-go {}", name(thing)), String::new());
        false
    }
    fn resize_start(&mut self, id: DisplayKey) -> Option<f32> {
        let width = self.piece(Thing::Display(id))?.place.width;
        self.resizing = Some(id);
        self.note(
            format!("resize-start {}", name(Thing::Display(id))),
            String::new(),
        );
        Some(width)
    }
    fn resize(&mut self, width: f32) {
        if let Some(id) = self.resizing
            && let Some(p) = self.piece_mut(Thing::Display(id))
        {
            p.place.width = width;
        }
        self.note("resize".to_owned(), format!("to {width:.3}"));
    }
    fn resize_end(&mut self) {
        self.resizing = None;
        self.note("resize-end".to_owned(), String::new());
    }
    fn cancel_holds(&mut self) {
        self.held.clear();
        self.resizing = None;
        self.note("cancel-holds".to_owned(), String::new());
    }
    fn haptic(&mut self, hand: usize) {
        let side = if hand == 0 { "left" } else { "right" };
        self.note(format!("haptic {side}"), String::new());
    }
    fn pc(&mut self, _now: Instant, input: PcInput) {
        let uv = |v: Vec2| format!("at ({:.3}, {:.3})", v.x, v.y);
        let display = |id: DisplayKey| name(Thing::Display(id));
        match input {
            PcInput::Move { id, uv: at } => self.note(format!("mouse {}", display(id)), uv(at)),
            PcInput::Button {
                id,
                uv: at,
                button,
                down,
            } => {
                let button = match button {
                    MouseButton::Left => "left",
                    MouseButton::Right => "right",
                };
                let way = if down { "down" } else { "up" };
                self.note(
                    format!("{button} {way} {} {}", display(id), uv(at)),
                    String::new(),
                );
            }
            PcInput::Scroll { id, dx, dy } => {
                self.note(
                    format!("scroll {}", display(id)),
                    format!("by ({dx}, {dy})"),
                );
            }
            PcInput::Touch {
                id,
                slot,
                uv: at,
                phase,
            } => {
                let phase = match phase {
                    TouchPhase::Down => "down",
                    TouchPhase::Move => "move",
                    TouchPhase::Up => "up",
                    TouchPhase::Cancel => "cancel",
                };
                let what = format!("touch {phase} {} slot {slot}", display(id));
                if phase == "move" {
                    self.note(what, uv(at));
                } else {
                    self.note(format!("{what} {}", uv(at)), String::new());
                }
            }
        }
    }
}

/// The menus' pictures as a recording knows them: the main menu's lower quarter is an
/// empty place (it drags the menu), the rest a button ("Recenter"); the palm menu is one
/// button ("Recenter" too).
struct StageMenus;

impl Menus for StageMenus {
    fn hover(&mut self, _menu: Menu, _at: Option<(f32, f32)>) {}
    fn press(&mut self, menu: Menu, at: (f32, f32)) -> Option<Action> {
        match menu {
            Menu::Main => (at.1 < ui::HEIGHT as f32 * 0.75).then_some(Action::Recenter),
            Menu::Palm => Some(Action::Recenter),
        }
    }
}

// ---- A recording ----

/// A recording read back: its head and its frames.
#[derive(Debug, Clone)]
pub struct Recording {
    pub app: String,
    pub feel: Feel,
    pub stage: Stage,
    /// Each frame, by how long after the first it came.
    pub frames: Vec<(Duration, FrameInput)>,
}

/// The head of a recording: the format, the app, the numbers, the stage.
pub fn head(app: &str, feel: &Feel, stage: &Stage) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{FORMAT}");
    let _ = writeln!(out, "app {app}");
    out.push_str("feel\n");
    feel.write_changed(&mut out);
    out.push_str("end\nstage\n");
    stage.write(&mut out);
    out.push_str("end\nframes\n");
    out
}

impl Recording {
    /// A recording's bytes as [`Recorder`] wrote them.
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let marker = b"\nframes\n";
        let split = bytes
            .windows(marker.len())
            .position(|w| w == marker)
            .ok_or("no \"frames\" line")?;
        let text = std::str::from_utf8(&bytes[..split]).map_err(|_| "the head is not text")?;
        let mut lines = text.lines();
        if lines.next() != Some(FORMAT) {
            return Err(format!("not \"{FORMAT}\""));
        }
        let mut app = String::new();
        let mut feel_text = String::new();
        let mut stage_lines = Vec::new();
        let mut part = "";
        for line in lines {
            match (part, line) {
                ("", l) if l.starts_with("app ") => l[4..].clone_into(&mut app),
                ("", "feel") => part = "feel",
                ("", "stage") => part = "stage",
                (_, "end") => part = "",
                ("feel", l) => {
                    feel_text.push_str(l);
                    feel_text.push('\n');
                }
                ("stage", l) => stage_lines.push(l),
                _ => {}
            }
        }
        let (feel, _) = Feel::parse(&feel_text);
        let stage = Stage::read(stage_lines.into_iter())?;
        let mut r = Reader {
            bytes: &bytes[split + marker.len()..],
        };
        let mut frames = Vec::new();
        while !r.bytes.is_empty() {
            frames.push(r.frame().ok_or("a frame cut short")?);
        }
        Ok(Self {
            app,
            feel,
            stage,
            frames,
        })
    }
}

/// The frames of a recording as they are read.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.bytes.split_first_chunk::<N>()?;
        self.bytes = rest;
        Some(*head)
    }
    fn f32(&mut self) -> Option<f32> {
        self.take::<4>().map(f32::from_le_bytes)
    }
    fn vec3(&mut self) -> Option<Vec3> {
        Some(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }

    fn frame(&mut self) -> Option<(Duration, FrameInput)> {
        let t = Duration::from_micros(u64::from(u32::from_le_bytes(self.take()?)));
        let [flags] = self.take()?;
        let dt = self.f32()?;
        let head = if flags & 1 != 0 {
            Some(Head {
                position: self.vec3()?,
                forward: self.vec3()?,
                right: self.vec3()?,
            })
        } else {
            None
        };
        let mut input = FrameInput::hidden(None);
        input.head = head;
        input.dt = dt;
        input.focused = flags & 2 != 0;
        input.menu_button = flags & 4 != 0;
        input.menu_gesture = flags & 8 != 0;
        input.align = flags & 16 != 0;
        for h in 0..2 {
            let bits = u16::from_le_bytes(self.take()?);
            let button = |shift: u16| Button {
                down: bits >> shift & 1 != 0,
                pressed: bits >> (shift + 1) & 1 != 0,
                released: bits >> (shift + 2) & 1 != 0,
            };
            let ray = if bits & 1 != 0 {
                Some(Ray {
                    origin: self.vec3()?,
                    dir: self.vec3()?,
                })
            } else {
                None
            };
            let stick = Vec2::new(self.f32()?, self.f32()?);
            input.hands[h] = HandState {
                ray,
                select: button(2),
                grab: button(5),
                stick,
                is_hand: bits & 2 != 0,
            };
            if bits & (1 << 8) != 0 {
                let mut j: Joints = [(Vec3::ZERO, 0.0); 26];
                for joint in &mut j {
                    *joint = (self.vec3()?, self.f32()?);
                }
                input.joints[h] = Some(j);
            }
        }
        Some((t, input))
    }
}

/// A recording being made: room for all of it is taken at the start, so a frame takes
/// no memory (see the module).
#[derive(Debug)]
pub struct Recorder {
    bytes: Vec<u8>,
    start: Instant,
    frames: u32,
}

impl Recorder {
    /// A recording of [`RECORD_FOR`] from `now`, its head `head` (see [`head`]).
    pub fn start(now: Instant, head: &str) -> Self {
        let frames = RECORD_FOR.as_secs() as usize * RATE_MAX as usize;
        let mut bytes = Vec::with_capacity(head.len() + frames * FRAME_MAX);
        bytes.extend_from_slice(head.as_bytes());
        Self {
            bytes,
            start: now,
            frames: 0,
        }
    }

    /// When it started.
    pub fn started(&self) -> Instant {
        self.start
    }

    /// The recording is over (its time, or its room, is up).
    pub fn over(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= RECORD_FOR
            || self.bytes.len() + FRAME_MAX > self.bytes.capacity()
    }

    /// How many frames it has.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// This frame's input, at `now`: the time from the start in microseconds (u32),
    /// flags (head, focused, menu button, menu gesture, "Line up"), `dt` (f32), the
    /// head (9 f32), then each hand: its bits (ray, a tracked hand, select's and grab's
    /// down, pressed, released, joints; u16), the ray (6 f32), the stick (2 f32), the
    /// joints (26 times position and radius, f32). Nothing once it is over.
    pub fn push(&mut self, now: Instant, input: &FrameInput) {
        if self.over(now) {
            return;
        }
        self.frames += 1;
        let t = now.saturating_duration_since(self.start).as_micros();
        let b = &mut self.bytes;
        b.extend_from_slice(&u32::try_from(t).unwrap_or(u32::MAX).to_le_bytes());
        let flags = u8::from(input.head.is_some())
            | u8::from(input.focused) << 1
            | u8::from(input.menu_button) << 2
            | u8::from(input.menu_gesture) << 3
            | u8::from(input.align) << 4;
        b.push(flags);
        b.extend_from_slice(&input.dt.to_le_bytes());
        let v = |v: Vec3, b: &mut Vec<u8>| {
            for x in v.to_array() {
                b.extend_from_slice(&x.to_le_bytes());
            }
        };
        if let Some(h) = input.head {
            v(h.position, b);
            v(h.forward, b);
            v(h.right, b);
        }
        for (hand, joints) in input.hands.iter().zip(&input.joints) {
            let button = |x: Button, shift: u16| {
                u16::from(x.down) << shift
                    | u16::from(x.pressed) << (shift + 1)
                    | u16::from(x.released) << (shift + 2)
            };
            let bits = u16::from(hand.ray.is_some())
                | u16::from(hand.is_hand) << 1
                | button(hand.select, 2)
                | button(hand.grab, 5)
                | u16::from(joints.is_some()) << 8;
            b.extend_from_slice(&bits.to_le_bytes());
            if let Some(r) = hand.ray {
                v(r.origin, b);
                v(r.dir, b);
            }
            b.extend_from_slice(&hand.stick.x.to_le_bytes());
            b.extend_from_slice(&hand.stick.y.to_le_bytes());
            if let Some(j) = joints {
                for &(p, r) in j {
                    v(p, b);
                    b.extend_from_slice(&r.to_le_bytes());
                }
            }
        }
    }

    /// The recording's bytes, to be written.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// `rec` played from `start` with the hands of this build: the journal of what they
/// asked of the room.
pub fn replay(rec: &Recording, start: Instant) -> Vec<String> {
    let mut hands = super::Hands::new();
    let mut stage = rec.stage.clone();
    stage.held.clear();
    stage.journal.clear();
    stage.feel = rec.feel;
    let mut menus = StageMenus;
    for (t, input) in &rec.frames {
        let now = start + *t;
        stage.at(start, now);
        hands.buttons(now, input, &mut stage);
        hands.step(now, input, &mut stage, &mut menus);
    }
    stage.journal()
}

#[cfg(test)]
mod tests;
