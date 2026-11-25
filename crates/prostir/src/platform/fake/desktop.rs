//! The fake desktop and its monitors: outputs with a size and a place, whose picture
//! (see `screen.rs`) the encoder reads like a KWin output.
//!
//! Other programs act on the desktop through `$XDG_RUNTIME_DIR/fake-desktop`, read and
//! removed once written, one command a line:
//! - `resize Fake-2 320x240`: another program's stream of the output makes it that
//!   size, as sharing a monitor in Discord does on KDE; starting Prostir's own stream
//!   over puts its size back;
//! - `resize-sticky Fake-2 320x240`: the same, but only making the output anew does;
//! - `refuse-scale`: setting the next monitor's scale fails, as `kscreen-doctor` can;
//! - `scale Fake-2 150`: the person gives the output another scale in the desktop's
//!   display settings;
//! - `unplug eDP-1`, `plug eDP-1`: a display of the PC goes away (the lid closed, the
//!   cable out) and comes back;
//! - `volume 40`, `volume 40 muted`: the slider of the headset's sound device moved;
//!   `drag 0 100 1000`: the slider dragged from 0 to 100 in 1000 ms, a step of 2 at a
//!   time;
//! - `record on`, `record off`: a program records from the headset's microphone;
//! - `sound-events stop`: the sound system's events end, as when `pactl subscribe`
//!   dies;
//! - `sound-gone`: the sound system loses Prostir's devices (the sound device and
//!   the microphone), as when PipeWire restarts;
//! - `close Fake-2`: the desktop closes that output, as KWin may, and its stream
//!   ends (as KWin's PipeWire node goes);
//! - `break-encoder Fake-2 3`: the next three fake hardware encoders of that output
//!   (the one running now among them) break once they have sent a frame, as a
//!   driver that fails in the middle of a stream; `break-encoder Fake-2 3 any`: the
//!   next three of any encoder;
//! - `fail-source Fake-3 2`: the next two starts of that output's picture source fail
//!   (it may not be made yet), as when KWin's PipeWire node of an output is gone;
//! - `cursor-shape 32 32 4 2 200`: the mouse pointer looks so (32x32 pixels, pointing
//!   at 4,2, every pixel of grey 200, opaque); `cursor-move Fake-1 100 50`: it is at
//!   that pixel of the output's picture; `cursor-hide`: it is hidden (until the next
//!   look). Only with `PROSTIR_FAKE_CURSOR=1`, where the desktop tells the pointer
//!   apart from the pictures; said to the core only while it asks for that;
//! - `hang make 20`, `hang place 20`, `hang encoder-stop 20`, `hang sound-start 20`:
//!   the next monitor made, layout placed, encoder stopped or sound to the headset
//!   started takes that many seconds, as a desktop, a driver or a sound system that
//!   hangs.
//!
//! As KWin, it keeps each output's scale by name: an output made anew has the scale
//! its name had last, until the platform layer sets another.
//!
//! The PC's own displays are `PROSTIR_FAKE_PHYSICAL`: `CONNECTOR=EDID@WxH` separated
//! by `;` (an empty EDID: the display says nothing), all plugged in at the start.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::{info, warn};

use super::screen::{self, Picture, Screen};
use crate::encoder::{Encoder, Input};
use crate::platform::{
    AddModel, Cursor, CursorEvent, CursorImage, CursorSink, CursorSource, DeskPlan, Desktop,
    DesktopCaps, FrameSource, Geometry, GpuDevices, Memory, Origin, Output, OutputHealth,
    OutputKey, Outputs, PhysicalDisplay, Pointer, Scales, VirtualSpec, Windows, needs_scale,
    scale_of,
};

/// A display of the fake PC itself, and whether it is plugged in.
struct FakePhysical {
    display: PhysicalDisplay,
    plugged: bool,
}

/// The fake PC's own displays, shared with the thread that carries out the test's
/// commands.
static PHYSICAL: Mutex<Vec<FakePhysical>> = Mutex::new(Vec::new());

/// The scale of each output by name, kept as KWin keeps it: an output made anew starts
/// at it.
static SCALES: Mutex<BTreeMap<String, f64>> = Mutex::new(BTreeMap::new());

/// The desktop's layout, for the commands of other programs.
static LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);

/// The mouse pointer of the fake desktop, as the test's commands make it: how it looks
/// and where it is, and the core's sink while it asks for it apart from the pictures.
#[derive(Default)]
struct FakePointer {
    image: Option<CursorImage>,
    hidden: bool,
    at: Option<(OutputKey, i32, i32, (u32, u32))>,
    sink: Option<CursorSink>,
}

impl FakePointer {
    /// What the core is told of it as it is now.
    fn now(&self) -> Vec<CursorEvent> {
        let look = match (&self.image, self.hidden) {
            (Some(image), false) => CursorEvent::Shape(image.clone()),
            _ => CursorEvent::Hidden,
        };
        let at = self
            .at
            .clone()
            .map(|(output, x, y, size)| CursorEvent::Moved { output, x, y, size });
        std::iter::once(look).chain(at).collect()
    }
}

static POINTER: Mutex<Option<FakePointer>> = Mutex::new(None);

/// Tells the core `event` (while it asks for the pointer apart), after keeping it.
fn pointer_event(event: CursorEvent) -> Result<()> {
    let mut pointer = POINTER.lock().unwrap();
    let p = pointer
        .as_mut()
        .context("the fake desktop tells no pointer (PROSTIR_FAKE_CURSOR)")?;
    match &event {
        CursorEvent::Shape(image) => {
            p.image = Some(image.clone());
            p.hidden = false;
        }
        CursorEvent::Moved { output, x, y, size } => {
            p.at = Some((output.clone(), *x, *y, *size));
        }
        CursorEvent::Hidden => p.hidden = true,
    }
    if let Some(sink) = &p.sink {
        sink.send(event);
    }
    Ok(())
}

/// The fake desktop's side of the pointer apart from the pictures. Its pictures never
/// have a pointer drawn in, so leaving it out changes nothing in them.
struct FakeCursor;

impl CursorSource for FakeCursor {
    fn set_apart(&mut self, sink: Option<CursorSink>) {
        let mut pointer = POINTER.lock().unwrap();
        let Some(p) = pointer.as_mut() else {
            return;
        };
        info!(
            "fake desktop: the pointer {}",
            if sink.is_some() {
                "apart from the pictures"
            } else {
                "in the pictures"
            }
        );
        if let Some(sink) = &sink {
            for event in p.now() {
                sink.send(event);
            }
        }
        p.sink = sink;
    }
}

/// The next scale the platform layer sets is not set (`refuse-scale`), as when
/// `kscreen-doctor` fails.
static REFUSE_SCALE: AtomicBool = AtomicBool::new(false);

/// Gives output `name` (`pixels` large) the scale `factor` on the desktop, and keeps
/// it for the name.
fn set_scale(layout: &Layout, name: &str, pixels: (u32, u32), factor: f64) {
    if let Some(g) = layout.lock().unwrap().get_mut(name) {
        let at = |px: u32| (f64::from(px) / factor).round() as i32;
        (g.w, g.h) = (at(pixels.0), at(pixels.1));
    }
    SCALES.lock().unwrap().insert(name.to_owned(), factor);
    info!("fake desktop: {name} at scale {factor}");
}

/// `CONNECTOR=EDID@WxH;…` (see the top of this file).
fn parse_physical(text: &str) -> Result<Vec<FakePhysical>> {
    text.split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            let (connector, rest) = t.split_once('=').context("CONNECTOR=EDID@WxH")?;
            let (edid, size) = rest.rsplit_once('@').context("CONNECTOR=EDID@WxH")?;
            let (w, h) = size.split_once('x').context("a size is WxH")?;
            Ok(FakePhysical {
                display: PhysicalDisplay {
                    key: OutputKey::new(connector),
                    // Its EDID's words as its maker and model, as a desktop says them.
                    label: crate::platform::display_label(edid, "", connector),
                    edid: (!edid.is_empty()).then(|| edid.to_owned()),
                    // A laptop's panel, by its connector, as on Linux.
                    builtin: ["eDP", "LVDS", "DSI"]
                        .iter()
                        .any(|p| connector.starts_with(p)),
                    size: Some((w.parse()?, h.parse()?)),
                    default: false,
                },
                plugged: true,
            })
        })
        .collect()
}

/// Whether display `name` of the fake PC is plugged in.
fn plugged(name: &str) -> bool {
    PHYSICAL
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.plugged && p.display.key.as_str() == name)
}

/// Name prefix of the fake outputs.
const PREFIX: &str = "Fake-";

/// Where each output is on the fake desktop, shared by the desktop and its outputs.
type Layout = Arc<Mutex<BTreeMap<String, Geometry>>>;

/// Connects to the fake desktop (its pictures are in plain memory: no graphics
/// devices to share).
pub fn connect(_gpu: &GpuDevices) -> Result<Box<dyn Desktop>> {
    Ok(Box::new(FakeDesktop::connect()?))
}

pub struct FakeDesktop {
    caps: DesktopCaps,
    layout: Layout,
    /// Picture changes per second on every output (0: still).
    fps: u32,
    picture: Picture,
    /// How long making a monitor takes (KScreen can take seconds).
    slow: Duration,
    /// Monitors (by number, from 1) that cannot be made.
    broken: Vec<u32>,
    /// The pointer told apart from the pictures (`PROSTIR_FAKE_CURSOR=1`).
    cursor: Option<FakeCursor>,
}

impl FakeDesktop {
    fn connect() -> Result<Self> {
        if let Ok(ms) = std::env::var("PROSTIR_FAKE_SLOW_CONNECT_MS") {
            let ms: u64 = ms
                .parse()
                .context("PROSTIR_FAKE_SLOW_CONNECT_MS: a whole number")?;
            std::thread::sleep(Duration::from_millis(ms));
        }
        let fps = match std::env::var("PROSTIR_FAKE_FPS") {
            Ok(v) => v.parse().context("PROSTIR_FAKE_FPS: a whole number")?,
            Err(_) => 0,
        };
        let picture = match std::env::var("PROSTIR_FAKE_PICTURE").as_deref() {
            Ok("noise") => Picture::Noise,
            Ok("bands") | Err(_) => Picture::Bands,
            Ok(other) => anyhow::bail!("PROSTIR_FAKE_PICTURE: bands or noise, not {other:?}"),
        };
        let slow = match std::env::var("PROSTIR_FAKE_SLOW_MONITORS_MS") {
            Ok(v) => v
                .parse()
                .context("PROSTIR_FAKE_SLOW_MONITORS_MS: a whole number")?,
            Err(_) => 0,
        };
        let broken = std::env::var("PROSTIR_FAKE_BROKEN_MONITORS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        let physical = parse_physical(&std::env::var("PROSTIR_FAKE_PHYSICAL").unwrap_or_default())
            .context("PROSTIR_FAKE_PHYSICAL")?;
        info!(
            "desktop: fake, the picture ({picture:?}) changes {fps} times a second; {} display(s) of its own",
            physical.len()
        );
        *PHYSICAL.lock().unwrap() = physical;
        let cursor = std::env::var_os("PROSTIR_FAKE_CURSOR").is_some_and(|v| v == "1");
        *POINTER.lock().unwrap() = cursor.then(FakePointer::default);
        let layout = Layout::default();
        *LAYOUT.lock().unwrap() = Some(layout.clone());
        static WATCHED: Once = Once::new();
        WATCHED.call_once(watch_commands);
        Ok(Self {
            caps: DesktopCaps {
                name: "fake",
                virtual_monitors: Ok(AddModel::OneByOne),
                has_primary: false,
                origin_is_primary: false,
                scales: Scales::Any { min: 50, max: 400 },
                refresh: vec![60],
                // With `PROSTIR_FAKE_SCREENSHOT=stream`, as on KDE: the picture of
                // one monitor comes from its own stream.
                screenshot_one_output: std::env::var_os("PROSTIR_FAKE_SCREENSHOT")
                    .is_none_or(|v| v != "stream"),
                moves_pointer: false,
                cleans_up_after_crash: true,
            },
            layout,
            fps,
            picture,
            slow: Duration::from_millis(slow),
            broken,
            cursor: cursor.then_some(FakeCursor),
        })
    }
}

impl Desktop for FakeDesktop {
    fn caps(&self) -> &DesktopCaps {
        &self.caps
    }

    fn make(&mut self, spec: &VirtualSpec) -> Result<Box<dyn Output>> {
        let number = spec.number;
        std::thread::sleep(self.slow);
        super::hang("make");
        anyhow::ensure!(
            !self.broken.contains(&number),
            "fake monitor {number} is broken"
        );
        let name = format!("{PREFIX}{number}");
        // At the scale its name had last, as KWin makes it.
        let kept = SCALES.lock().unwrap().get(&name).copied().unwrap_or(1.0);
        let output = FakeOutput::start(
            name.clone(),
            (spec.w, spec.h, kept),
            self.fps,
            self.picture,
            self.layout.clone(),
            true,
        )?;
        info!("fake desktop: created {name} ({spec})");
        let listed = self.layout.lock().unwrap().get(&name).map(|g| (g.w, g.h));
        if needs_scale(spec, listed) {
            if REFUSE_SCALE.swap(false, Ordering::Relaxed) {
                warn!("fake desktop: setting the scale of {name} failed");
            } else {
                set_scale(&self.layout, &name, (spec.w, spec.h), spec.factor());
            }
        }
        Ok(Box::new(output))
    }

    /// An external display before the laptop's panel, as on Hyprland, is the default.
    fn physical(&self) -> Vec<PhysicalDisplay> {
        let mut found: Vec<PhysicalDisplay> = PHYSICAL
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.plugged)
            .map(|p| p.display.clone())
            .collect();
        let default = found
            .iter()
            .min_by_key(|d| (d.builtin, d.key.clone()))
            .map(|d| d.key.clone());
        for d in &mut found {
            d.default = Some(&d.key) == default.as_ref();
        }
        found
    }

    fn show(&mut self, key: &OutputKey) -> Result<Box<dyn Output>> {
        let name = key.as_str();
        let size = PHYSICAL
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.plugged && p.display.key == *key)
            .and_then(|p| p.display.size)
            .with_context(|| format!("the fake desktop has no display {name}"))?;
        let output = FakeOutput::start(
            name.to_owned(),
            (size.0, size.1, 1.0),
            self.fps,
            self.picture,
            self.layout.clone(),
            false,
        )?;
        info!("fake desktop: mirroring {name}");
        Ok(Box::new(output))
    }

    fn outputs(&self) -> Outputs {
        let mut layout = self.layout.lock().unwrap();
        // The PC's own displays are on the desktop while plugged in.
        for p in PHYSICAL.lock().unwrap().iter() {
            let name = p.display.key.as_str();
            match (p.plugged, layout.contains_key(name)) {
                (true, false) => {
                    let (w, h) = p.display.size.unwrap_or((1920, 1080));
                    let g = Geometry {
                        x: 0,
                        y: 0,
                        w: w as i32,
                        h: h as i32,
                        priority: None,
                        enabled: true,
                        is_virtual: false,
                    };
                    layout.insert(name.to_owned(), g);
                }
                (false, true) => {
                    layout.remove(name);
                }
                _ => {}
            }
        }
        layout
            .iter()
            .map(|(name, g)| {
                let mut g = *g;
                // Resized by another program: at the same scale, the size it has now.
                if let Some(screen) = Screen::find(name)
                    && let Some((w, h)) = screen.resized()
                {
                    let (made_w, made_h) = screen.size();
                    let scaled = |now: u32, made: u32, logical: i32| {
                        (f64::from(now) * f64::from(logical) / f64::from(made.max(1))).round()
                            as i32
                    };
                    (g.w, g.h) = (scaled(w, made_w, g.w), scaled(h, made_h, g.h));
                }
                (OutputKey::new(name.as_str()), g)
            })
            .collect()
    }

    fn place(&mut self, plan: &DeskPlan, _current: &Outputs) -> Result<()> {
        super::hang("place");
        let mut layout = self.layout.lock().unwrap();
        for (key, x, y) in &plan.positions {
            if let Some(g) = layout.get_mut(key.as_str()) {
                (g.x, g.y) = (*x, *y);
            }
        }
        let placed: Vec<String> = plan
            .positions
            .iter()
            .map(|(key, x, y)| format!("{key} {x},{y}"))
            .collect();
        info!("fake desktop: placed {}", placed.join(", "));
        Ok(())
    }

    fn windows(&mut self) -> Option<&mut dyn Windows> {
        None
    }

    fn pointer(&mut self) -> Option<&mut dyn Pointer> {
        None
    }

    fn cursor(&mut self) -> Option<&mut dyn CursorSource> {
        match &mut self.cursor {
            Some(c) => Some(c),
            None => None,
        }
    }
}

/// A fake monitor. Its picture changes `fps` times a second (never with 0) until it
/// is dropped.
pub struct FakeOutput {
    key: OutputKey,
    name: String,
    screen: Arc<Screen>,
    stop: Arc<AtomicBool>,
    changer: Option<JoinHandle<()>>,
    layout: Layout,
    /// Made here (and gone with it), not a display of the PC streamed as it is.
    owned: bool,
}

impl FakeOutput {
    /// `mode`: pixel width and height, and scale.
    fn start(
        name: String,
        mode: (u32, u32, f64),
        fps: u32,
        picture: Picture,
        layout: Layout,
        owned: bool,
    ) -> Result<Self> {
        let (width, height, scale) = mode;
        let screen = Screen::add(&name, width, height, picture)?;
        let stop = Arc::new(AtomicBool::new(false));
        let changer = if fps > 0 {
            let (screen, stop) = (screen.clone(), stop.clone());
            let every = Duration::from_secs(1) / fps;
            Some(
                std::thread::Builder::new()
                    .name(format!("fake-{name}"))
                    .spawn(move || {
                        let mut next = Instant::now();
                        while !stop.load(Ordering::Relaxed) {
                            next += every;
                            std::thread::sleep(next.saturating_duration_since(Instant::now()));
                            screen.change();
                        }
                    })?,
            )
        } else {
            None
        };
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        if owned {
            layout.lock().unwrap().insert(
                name.clone(),
                Geometry {
                    x: 0,
                    y: 0,
                    w: logical(width),
                    h: logical(height),
                    priority: None,
                    enabled: true,
                    is_virtual: true,
                },
            );
        }
        Ok(Self {
            key: OutputKey::new(name.as_str()),
            name,
            screen,
            stop,
            changer,
            layout,
            owned,
        })
    }
}

impl Drop for FakeOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.changer.take() {
            let _ = t.join();
        }
        self.screen.remove();
        if self.owned {
            self.layout.lock().unwrap().remove(&self.name);
            info!("fake desktop: removed {}", self.name);
        } else {
            info!("fake desktop: stopped mirroring {}", self.name);
        }
    }
}

impl Output for FakeOutput {
    fn key(&self) -> &OutputKey {
        &self.key
    }

    /// From the layout in memory, as KWin's events tell it.
    fn desk_scale(&self) -> Option<u16> {
        let g = *self.layout.lock().unwrap().get(&self.name)?;
        scale_of(self.screen.size(), (g.w, g.h))
    }

    /// A display of the PC is closed once it is unplugged.
    fn health(&self) -> OutputHealth {
        if (!self.owned && !plugged(&self.name)) || self.screen.is_closed() {
            OutputHealth::Closed
        } else if let Some((w, h)) = self.screen.resized() {
            OutputHealth::Resized { w, h }
        } else {
            OutputHealth::Live
        }
    }

    fn make_primary(&self) {}

    fn source(&self) -> FrameSource {
        FrameSource {
            input: Input {
                source: format!("{} screen={}", screen::ELEMENT, self.name),
                label: self.name.clone(),
                frame_rate: None,
                live: false,
            },
            origin: if self.owned {
                Origin::Monitor
            } else {
                Origin::Capture
            },
            memory: Memory::System,
            cursor: Cursor::InFrame,
        }
    }

    fn detach(&mut self) {}

    /// The encoder reads the picture itself.
    fn attach(&mut self, _encoder: &Encoder) -> Result<()> {
        Ok(())
    }
}

/// Carries out what other programs do on the desktop, as the test writes it to
/// `$XDG_RUNTIME_DIR/fake-desktop` (see the top of this file).
fn watch_commands() {
    let watched = super::watch_file("fake-desktop", |text| {
        for line in text.lines() {
            if let Err(e) = command(line.trim()) {
                warn!("fake desktop: {line:?}: {e:#}");
            }
        }
        true
    });
    if !watched {
        warn!("fake desktop: no commands");
    }
}

/// One command from the file.
fn command(line: &str) -> Result<()> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let device = |level: u8, muted: bool| {
        super::audio::moved(level, muted);
        info!(
            "fake desktop: volume moved to {level}{}",
            if muted { " muted" } else { "" }
        );
    };
    match words[..] {
        ["volume", level] => {
            device(level.parse()?, false);
            return Ok(());
        }
        ["volume", level, "muted"] => {
            device(level.parse()?, true);
            return Ok(());
        }
        ["drag", from, to, ms] => {
            let (from, to, ms): (u8, u8, u64) = (from.parse()?, to.parse()?, ms.parse()?);
            let steps = u64::from(from.abs_diff(to) / 2).max(1);
            std::thread::spawn(move || {
                // How long the drag stood still at most between two steps (a busy
                // machine may hold this thread up), for tests to know what the PC saw.
                let mut longest = Duration::ZERO;
                let mut last: Option<Instant> = None;
                for i in 0..=steps {
                    let level = if to >= from {
                        from + (u64::from(to - from) * i / steps) as u8
                    } else {
                        from - (u64::from(from - to) * i / steps) as u8
                    };
                    let now = Instant::now();
                    if let Some(t) = last {
                        longest = longest.max(now - t);
                    }
                    last = Some(now);
                    device(level, false);
                    std::thread::sleep(Duration::from_millis(ms / steps));
                }
                info!(
                    "fake desktop: drag done, longest pause {} ms",
                    longest.as_millis()
                );
            });
            return Ok(());
        }
        ["record", on @ ("on" | "off")] => {
            super::audio::record(on == "on");
            info!("fake desktop: a program records: {on}");
            return Ok(());
        }
        ["sound-events", "stop"] => {
            super::audio::stop_events();
            info!("fake desktop: the sound system's events stopped");
            return Ok(());
        }
        ["sound-gone"] => {
            super::audio::gone();
            info!("fake desktop: the sound system lost Prostir's devices");
            return Ok(());
        }
        _ => {}
    }
    if let ["cursor-shape", w, h, hot_x, hot_y, grey] = words[..] {
        let (w, h): (u16, u16) = (w.parse()?, h.parse()?);
        let grey: u8 = grey.parse()?;
        let image = CursorImage {
            w,
            h,
            hot_x: hot_x.parse()?,
            hot_y: hot_y.parse()?,
            rgba: [grey, grey, grey, 255].repeat(usize::from(w) * usize::from(h)),
        };
        info!("fake desktop: the pointer looks {image:?}");
        return pointer_event(CursorEvent::Shape(image));
    }
    if let ["cursor-move", name, x, y] = words[..] {
        info!("fake desktop: the pointer at {x},{y} of {name}");
        return pointer_event(CursorEvent::Moved {
            output: OutputKey::new(name),
            x: x.parse()?,
            y: y.parse()?,
            // The output's picture as it makes it; one it does not have, as a usual screen.
            size: Screen::find(name).map_or((1920, 1080), |s| s.size()),
        });
    }
    if let ["cursor-hide"] = words[..] {
        info!("fake desktop: the pointer hidden");
        return pointer_event(CursorEvent::Hidden);
    }
    if let ["close", name] = words[..] {
        Screen::find(name)
            .with_context(|| format!("no output {name}"))?
            .close();
        info!("fake desktop: closed {name}");
        return Ok(());
    }
    if let ["break-encoder", name, n] = words[..] {
        super::break_encoders(name, n.parse()?, false);
        info!("fake desktop: the next {n} fake hardware encoders of {name} break");
        return Ok(());
    }
    if let ["break-encoder", name, n, "any"] = words[..] {
        super::break_encoders(name, n.parse()?, true);
        info!("fake desktop: the next {n} encoders of {name} break");
        return Ok(());
    }
    if let ["fail-source", name, n] = words[..] {
        screen::FAIL_STARTS
            .lock()
            .unwrap()
            .insert(name.to_owned(), n.parse()?);
        info!("fake desktop: the next {n} source starts of {name} fail");
        return Ok(());
    }
    if let [
        "hang",
        point @ ("make" | "place" | "encoder-stop" | "sound-start"),
        secs,
    ] = words[..]
    {
        super::hang_next(point, Duration::from_secs(secs.parse()?));
        info!("fake desktop: the next {point} hangs for {secs} s");
        return Ok(());
    }
    if let [verb @ ("plug" | "unplug"), name] = words[..] {
        let mut physical = PHYSICAL.lock().unwrap();
        let p = physical
            .iter_mut()
            .find(|p| p.display.key.as_str() == name)
            .with_context(|| format!("no display {name}"))?;
        p.plugged = verb == "plug";
        info!("fake desktop: {name} {verb}ged");
        return Ok(());
    }
    if let ["change", name, times] = words[..] {
        let screen = Screen::find(name).with_context(|| format!("no output {name}"))?;
        for _ in 0..times.parse::<u32>()? {
            screen.change();
            std::thread::sleep(Duration::from_millis(30));
        }
        info!("fake desktop: {name} changed {times} times");
        return Ok(());
    }
    if let ["refuse-scale"] = words[..] {
        REFUSE_SCALE.store(true, Ordering::Relaxed);
        info!("fake desktop: the next scale set fails");
        return Ok(());
    }
    if let ["scale", name, percent] = words[..] {
        let percent: u16 = percent.parse()?;
        let screen = Screen::find(name).with_context(|| format!("no output {name}"))?;
        let layout = LAYOUT.lock().unwrap().clone().context("no desktop")?;
        set_scale(&layout, name, screen.size(), f64::from(percent) / 100.0);
        return Ok(());
    }
    let [verb @ ("resize" | "resize-sticky"), name, size] = words[..] else {
        anyhow::bail!("not a command");
    };
    let (w, h) = size.split_once('x').context("a size is WxH")?;
    let size = (w.parse()?, h.parse()?);
    let screen = Screen::find(name).with_context(|| format!("no output {name}"))?;
    screen.resize(size, verb == "resize-sticky");
    info!("fake desktop: another program resized {name} to {w}x{h}");
    Ok(())
}
