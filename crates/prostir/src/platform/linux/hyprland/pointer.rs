//! The pointer apart from the frames on wlroots desktops (Hyprland; Sway, Wayfire and
//! labwc with wlroots 0.19 alike): a pointer cursor session of `ext-image-copy-capture-v1`
//! per output, told to the core as [`CursorEvent`]s (`CursorSource`), while the frames
//! are captured without the pointer (`screencopy.rs`, `overlay_cursor` 0).
//!
//! The session says when the pointer's picture touches the output (`enter`, `leave`),
//! where its hotspot is (`position`, also off the output's picture) and the hotspot in
//! its picture (`hotspot`); the picture itself comes as a small frame, captured into
//! shared memory, whose capture waits until the picture changes. Neither touches the
//! output's frames or their gate: the pointer moving gives no frame to encode.
//!
//! A clear picture is not taken for a hidden pointer: the output's frames then have
//! the pointer drawn in again (a hidden one shows there as nothing all the same), and
//! the core hears it left the output, until a picture with something in it comes. So
//! a compositor that cannot give the picture never leaves the person without a
//! pointer: Hyprland 0.54 to 0.57 at least gives clear ones for the pointers of its
//! own themes (hyprcursor, and any on a hardware cursor plane; hyprwm/Hyprland#15895),
//! real ones only for those programs draw themselves (Xwayland).
//!
//! Hyprland differs from the protocol in more ways, which [`Units`] and [`CursorTap`]
//! take in: it says the place and the hotspot in logical pixels, not in the pixels of
//! the output's picture and of the pointer's; the pointer's picture is drawn at the
//! highest scale of its outputs, whatever the scale of this one; it says the hotspot
//! with the output's next frame, after the picture; and a picture captured while the
//! pointer is off the output is black (not taken as the pointer's look: the session
//! starts over once the pointer is on it, its first picture coming at once). Hyprland
//! before its fix of 2026-11 could crash when a client dropped a cursor frame waiting
//! for a change while its session lived on: a frame here is dropped only once done,
//! or after its session.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_shm_pool;
use wayland_client::protocol::{wl_buffer, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::platform::linux::straighten;
use crate::platform::{CursorEvent, CursorImage, CursorSink, OutputKey};

/// Where the core's sink is while it asks for the pointer apart; none otherwise. One
/// for the desktop, shared with every output's session.
pub type SinkSlot = Arc<Mutex<Option<CursorSink>>>;

/// The interfaces a pointer session needs, as the registry names them.
const NEEDED: [&str; 3] = [
    "ext_image_copy_capture_manager_v1",
    "ext_output_image_capture_source_manager_v1",
    "wl_seat",
];

/// Whether a compositor whose registry lists `interfaces` offers pointer sessions.
pub fn offers_sessions<'a>(interfaces: impl IntoIterator<Item = &'a str>) -> bool {
    let listed: Vec<&str> = interfaces.into_iter().collect();
    NEEDED.iter().all(|n| listed.contains(n))
}

/// Whether the compositor of this session offers pointer sessions (one look at its
/// registry, on a connection of its own).
pub fn offered() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<Probe>(&conn) else {
        return false;
    };
    let list = globals.contents().clone_list();
    offers_sessions(list.iter().map(|g| g.interface.as_str()))
}

struct Probe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// How the compositor counts what it says of the pointer, in the pixels of the
/// output's picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Units {
    /// Pixels per unit of `position`: 1 as the protocol has it, the output's scale on
    /// Hyprland (logical pixels).
    pub place: f64,
    /// Pixels per unit of `hotspot`: 1 as the protocol has it (the pointer's picture
    /// is in the output's pixels), the output's scale on Hyprland.
    pub hotspot: f64,
    /// Pixels per pixel of the pointer's picture: 1 as the protocol has it; on
    /// Hyprland the output's scale over the one the picture is drawn at.
    pub image: f64,
}

impl Units {
    /// As `ext-image-copy-capture-v1` says (wlroots).
    pub const PROTOCOL: Self = Self {
        place: 1.0,
        hotspot: 1.0,
        image: 1.0,
    };

    /// Hyprland's, for an output at `scale` while the highest scale of its outputs is
    /// `highest` (its own pointers are drawn at it).
    pub fn hyprland(scale: f64, highest: f64) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        Self {
            place: scale,
            hotspot: scale,
            image: scale / highest.max(scale),
        }
    }
}

/// A picture of the pointer as the compositor captured it: `w`x`h` pixels, rows
/// `stride` bytes apart, in `format` (premultiplied, as Wayland's shared memory is).
#[derive(Clone, Copy)]
pub struct Captured<'a> {
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub format: wl_shm::Format,
    pub pixels: &'a [u8],
}

/// A hotspot said within this after a picture came goes with that picture (Hyprland
/// says it with the output's next frame, after the picture); later, with the next.
const HOT_LATE: Duration = Duration::from_millis(100);

/// What one output's pointer session told the core of the pointer, to tell it only
/// what changed: the compositor's events in, [`CursorEvent`]s out.
#[derive(Debug)]
pub struct CursorTap {
    output: OutputKey,
    /// The output's picture, in pixels, and how the compositor counts.
    size: (u32, u32),
    units: Units,
    /// The pointer's picture touches the output (`enter`, until `leave`).
    entered: bool,
    /// Its hotspot's place, as said.
    place: Option<(i32, i32)>,
    /// The hotspot in its picture, as said and in effect; the one said for the next
    /// picture.
    hot: (i32, i32),
    hot_next: Option<(i32, i32)>,
    /// When the last picture came, while a hotspot said soon after goes with it.
    pictured: Option<Instant>,
    /// Its look in the output's pixels (straight RGBA), none before a picture came.
    look: Option<Look>,
    /// A picture came while the pointer was off this output (black on Hyprland): the
    /// look may be old, and a fresh session is asked for once it is on it.
    pictured_off: bool,
    fresh_asked: bool,
    /// As told: on this output, at, the hotspot told with the look (none: the look
    /// not told yet).
    on: bool,
    at: Option<(i32, i32)>,
    told: Option<(i32, i32)>,
    /// Kept for each picture: nothing is allocated while the look stays.
    scratch: Vec<u8>,
    scaled: Vec<u8>,
}

#[derive(Debug)]
struct Look {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    /// Not a pixel shows: hidden, or a picture the compositor could not give.
    clear: bool,
}

impl CursorTap {
    pub fn new(output: OutputKey) -> Self {
        Self {
            output,
            size: (0, 0),
            units: Units::PROTOCOL,
            entered: false,
            place: None,
            hot: (0, 0),
            hot_next: None,
            pictured: None,
            look: None,
            pictured_off: false,
            fresh_asked: false,
            on: false,
            at: None,
            told: None,
            scratch: Vec::new(),
            scaled: Vec::new(),
        }
    }

    /// The output's picture is `size` pixels, counted in `units`.
    pub fn geometry(&mut self, size: (u32, u32), units: Units, tell: impl FnMut(CursorEvent)) {
        if (size, units) == (self.size, self.units) {
            return;
        }
        let rescaled = units.image != self.units.image;
        (self.size, self.units) = (size, units);
        if rescaled && self.look.is_some() {
            // The look is of the old scale: anew with the next picture.
            self.look = None;
            self.pictured_off = true;
            self.told = None;
        }
        self.update(tell);
    }

    pub fn enter(&mut self, tell: impl FnMut(CursorEvent)) {
        self.entered = true;
        self.update(tell);
    }

    pub fn leave(&mut self, tell: impl FnMut(CursorEvent)) {
        self.entered = false;
        self.place = None;
        self.update(tell);
    }

    pub fn position(&mut self, x: i32, y: i32, tell: impl FnMut(CursorEvent)) {
        self.place = Some((x, y));
        self.update(tell);
    }

    /// The hotspot, in effect with the next picture, or with the one that came just
    /// before (`now`).
    pub fn hotspot(&mut self, x: i32, y: i32, now: Instant, tell: impl FnMut(CursorEvent)) {
        if self
            .pictured
            .take()
            .is_some_and(|t| now.saturating_duration_since(t) < HOT_LATE)
        {
            self.hot = (x, y);
            self.update(tell);
        } else {
            self.hot_next = Some((x, y));
        }
    }

    /// The compositor has no picture of the pointer at all (of no size).
    pub fn no_picture(&mut self, tell: impl FnMut(CursorEvent)) {
        self.look = Some(Look {
            w: 0,
            h: 0,
            rgba: Vec::new(),
            clear: true,
        });
        self.update(tell);
    }

    /// A picture of the pointer came (at `now`).
    pub fn picture(&mut self, shot: Captured<'_>, now: Instant, tell: impl FnMut(CursorEvent)) {
        if let Some(hot) = self.hot_next.take() {
            self.hot = hot;
        }
        self.pictured = Some(now);
        if self.inside().is_none() {
            // Not its look (black on Hyprland): kept as it was.
            self.pictured_off = true;
            return;
        }
        self.pictured_off = false;
        self.fresh_asked = false;
        if !premultiplied_rgba(shot, &mut self.scratch) {
            warn!(
                "{}: the pointer's picture is {:?}",
                self.output, shot.format
            );
            return;
        }
        let (mut w, mut h) = (shot.w, shot.h);
        let factor = self.units.image;
        if (factor - 1.0).abs() > 1e-3 && w > 0 && h > 0 {
            let side = |n: u32| ((f64::from(n) * factor).round() as u32).max(1);
            let (nw, nh) = (side(w), side(h));
            resample(&self.scratch, (w, h), &mut self.scaled, (nw, nh));
            std::mem::swap(&mut self.scratch, &mut self.scaled);
            (w, h) = (nw, nh);
        }
        straighten(&mut self.scratch);
        let same = self
            .look
            .as_ref()
            .is_some_and(|l| (l.w, l.h) == (w, h) && l.rgba == self.scratch);
        if !same {
            let clear = self.scratch.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
            let look = self.look.get_or_insert_with(|| Look {
                w,
                h,
                rgba: Vec::new(),
                clear,
            });
            (look.w, look.h, look.clear) = (w, h, clear || w == 0 || h == 0);
            look.rgba.clear();
            look.rgba.extend_from_slice(&self.scratch);
            self.told = None;
        }
        self.update(tell);
    }

    /// Whether the headset draws the pointer for this output: its look is known and
    /// shows something (else the output's frames have it drawn in).
    pub fn shows(&self) -> bool {
        self.look.as_ref().is_some_and(|l| !l.clear)
    }

    /// Whether the session should start over for a picture taken now: the pointer is
    /// on this output, and the picture that came last was taken while it was off it.
    /// Asked once each time.
    pub fn wants_fresh(&mut self) -> bool {
        if self.pictured_off && !self.fresh_asked && self.inside().is_some() {
            self.fresh_asked = true;
            return true;
        }
        false
    }

    /// The session is over (the output gone, the pointer drawn in again): off this
    /// output, if it was on it.
    pub fn end(&mut self, tell: impl FnMut(CursorEvent)) {
        self.entered = false;
        self.place = None;
        self.update(tell);
    }

    /// The hotspot's place in the output's pixels, when on its picture.
    fn inside(&self) -> Option<(i32, i32)> {
        let (x, y) = self.place.filter(|_| self.entered)?;
        let px = |v: i32| (f64::from(v) * self.units.place).round() as i32;
        let (x, y) = (px(x), px(y));
        let (w, h) = self.size;
        ((0..w as i32).contains(&x) && (0..h as i32).contains(&y)).then_some((x, y))
    }

    fn update(&mut self, mut tell: impl FnMut(CursorEvent)) {
        let look = self.look.as_ref().filter(|l| !l.clear);
        let Some(((x, y), look)) = self.inside().zip(look) else {
            if std::mem::take(&mut self.on) {
                self.at = None;
                tell(CursorEvent::Left {
                    output: self.output.clone(),
                });
            }
            return;
        };
        let px = |v: i32| (f64::from(v) * self.units.hotspot).round() as i32;
        let hot = (px(self.hot.0), px(self.hot.1));
        if self.told != Some(hot) {
            let side = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
            let within = |v: i32, n: u32| u16::try_from(v.clamp(0, n as i32 - 1)).unwrap_or(0);
            self.told = Some(hot);
            tell(CursorEvent::Shape(CursorImage {
                w: side(look.w),
                h: side(look.h),
                hot_x: within(hot.0, look.w),
                hot_y: within(hot.1, look.h),
                rgba: look.rgba.clone(),
            }));
        }
        if !self.on || self.at != Some((x, y)) {
            self.on = true;
            self.at = Some((x, y));
            tell(CursorEvent::Moved {
                output: self.output.clone(),
                x,
                y,
                size: self.size,
            });
        }
    }
}

/// `shot` as premultiplied RGBA rows packed into `out`; whether its format is one read.
fn premultiplied_rgba(shot: Captured<'_>, out: &mut Vec<u8>) -> bool {
    use wl_shm::Format;
    // Where red, green, blue and alpha are in each pixel (little-endian formats);
    // none for alpha: opaque.
    let (r, g, b, a) = match shot.format {
        Format::Argb8888 => (2, 1, 0, Some(3)),
        Format::Xrgb8888 => (2, 1, 0, None),
        Format::Abgr8888 => (0, 1, 2, Some(3)),
        Format::Xbgr8888 => (0, 1, 2, None),
        _ => return false,
    };
    let (w, h, stride) = (shot.w as usize, shot.h as usize, shot.stride as usize);
    out.clear();
    if w * 4 > stride || shot.pixels.len() < stride * h.saturating_sub(1) + w * 4 {
        return h == 0 || w == 0;
    }
    out.reserve(w * h * 4);
    for row in shot.pixels.chunks(stride).take(h) {
        for p in row[..w * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[p[r], p[g], p[b], a.map_or(255, |a| p[a])]);
        }
    }
    true
}

/// RGBA `from` of `size` pixels made `to_size` into `to`, each pixel the mean of those
/// it covers (premultiplied: the colours of clear pixels do not bleed in).
fn resample(from: &[u8], size: (u32, u32), to: &mut Vec<u8>, to_size: (u32, u32)) {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let (nw, nh) = (to_size.0 as usize, to_size.1 as usize);
    to.clear();
    to.resize(nw * nh * 4, 0);
    let span = |i: usize, n: usize, of: usize| {
        let start = i * of / n;
        let end = ((i + 1) * of).div_ceil(n).clamp(start + 1, of);
        start..end
    };
    for y in 0..nh {
        let rows = span(y, nh, h);
        for x in 0..nw {
            let cols = span(x, nw, w);
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for sy in rows.clone() {
                for sx in cols.clone() {
                    let p = &from[(sy * w + sx) * 4..][..4];
                    for (s, c) in sum.iter_mut().zip(p) {
                        *s += u32::from(*c);
                    }
                    n += 1;
                }
            }
            let out = &mut to[(y * nw + x) * 4..][..4];
            for (o, s) in out.iter_mut().zip(sum) {
                *o = ((s + n / 2) / n.max(1)) as u8;
            }
        }
    }
}

/// One output's pointer session, on a thread and a Wayland connection of its own
/// (never waiting for the output's frames, nor they for it). Dropping it ends it: the
/// pointer is drawn into the output's frames again.
pub struct PointerSession {
    stop: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl PointerSession {
    /// Starts telling the pointer on output `name` to `sink` (while it holds one),
    /// counted in the `units` it gives at the start and on each change of the
    /// outputs. `apart` is set while the headset draws the pointer for this output
    /// (its look known and showing, see [`CursorTap::shows`]), which the capture reads
    /// to leave the pointer out of the frames, and cleared when the session ends.
    pub fn start(
        name: String,
        sink: SinkSlot,
        apart: Arc<AtomicBool>,
        units: Box<dyn Fn() -> Units + Send>,
    ) -> Result<Self> {
        // SAFETY: a new eventfd; owned below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: `fd` was just made and belongs to nobody else.
        let stop = unsafe { OwnedFd::from_raw_fd(fd) };
        let woken = stop.try_clone()?;
        let thread = std::thread::Builder::new()
            .name(format!("pointer-{name}"))
            .spawn(move || {
                let key = OutputKey::new(name.clone());
                let mut state = State::new(CursorTap::new(key), sink);
                match run(&name, &mut state, &apart, &*units, woken.as_raw_fd()) {
                    Ok(()) => info!("{name}: the pointer session ended"),
                    Err(e) => warn!("{name}: the pointer goes into the frames again: {e:#}"),
                }
                apart.store(false, Ordering::Relaxed);
                state.with_tap(|t, tell| t.end(tell));
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for PointerSession {
    fn drop(&mut self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: writes 8 bytes to the eventfd owned here.
        unsafe { libc::write(self.stop.as_raw_fd(), one.as_ptr().cast(), one.len()) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One shared-memory buffer the pointer's picture is captured into, made anew only
/// when its size or format changes.
struct Shm {
    buffer: wl_buffer::WlBuffer,
    pool: wl_shm_pool::WlShmPool,
    map: *mut u8,
    len: usize,
    w: u32,
    h: u32,
    format: wl_shm::Format,
    _file: File,
}

impl Shm {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        (w, h): (u32, u32),
        format: wl_shm::Format,
    ) -> Result<Self> {
        let len = w as usize * h as usize * 4;
        // SAFETY: a fresh anonymous file; the name is a valid C string.
        let fd = unsafe { libc::memfd_create(c"prostir-pointer".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: `fd` was just created and is owned by nobody else.
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(len as u64)?;
        // SAFETY: maps the whole file, which stays open as long as the mapping.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().into());
        }
        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, w as i32 * 4, format, qh, ());
        Ok(Self {
            buffer,
            pool,
            map: map.cast(),
            len,
            w,
            h,
            format,
            _file: file,
        })
    }

    fn pixels(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes, and the compositor is done writing it
        // (the frame is ready).
        unsafe { std::slice::from_raw_parts(self.map, self.len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        // SAFETY: `map` came from mmap with this length and is not used after.
        unsafe { libc::munmap(self.map.cast(), self.len) };
    }
}

/// An output as its `wl_output` says it.
#[derive(Default)]
struct OutputInfo {
    name: Option<String>,
    mode: (i32, i32),
    turned: bool,
}

struct State {
    tap: CursorTap,
    sink: SinkSlot,
    outputs: HashMap<u32, (wl_output::WlOutput, OutputInfo)>,
    /// Ours, by its registry name, once found; gone from the registry.
    ours: Option<u32>,
    gone: bool,
    /// An output said all it had to say (`done`): the units may have changed.
    outputs_done: bool,
    shm_global: Option<wl_shm::WlShm>,
    /// The capture session of the pointer and its constraints: the batch said last
    /// (`done`), and the one being said.
    session: Option<ExtImageCopyCaptureSessionV1>,
    size: (u32, u32),
    formats: Vec<wl_shm::Format>,
    batch_size: (u32, u32),
    batch_formats: Vec<wl_shm::Format>,
    described: bool,
    stopped: bool,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    shm: Option<Shm>,
    /// Captures that failed in a row, and when to try again after one.
    failures: u32,
    retry_at: Option<Instant>,
    /// Why no capture can be made.
    broken: Option<String>,
}

impl State {
    fn new(tap: CursorTap, sink: SinkSlot) -> Self {
        Self {
            tap,
            sink,
            outputs: HashMap::new(),
            ours: None,
            gone: false,
            outputs_done: true,
            shm_global: None,
            session: None,
            size: (0, 0),
            formats: Vec::new(),
            batch_size: (0, 0),
            batch_formats: Vec::new(),
            described: false,
            stopped: false,
            frame: None,
            shm: None,
            failures: 0,
            retry_at: None,
            broken: None,
        }
    }

    /// Calls `f` on the tap with the core's sink, if it holds one.
    fn with_tap(&mut self, f: impl FnOnce(&mut CursorTap, &mut dyn FnMut(CursorEvent))) {
        let sink = self.sink.lock().unwrap().clone();
        let mut tell = |e: CursorEvent| {
            if let Some(sink) = &sink {
                sink.send(e);
            }
        };
        f(&mut self.tap, &mut tell);
    }

    /// The size of our output's picture: its mode, turned as it is.
    fn output_size(&self) -> (u32, u32) {
        let Some((_, info)) = self.ours.and_then(|g| self.outputs.get(&g)) else {
            return (0, 0);
        };
        let (w, h) = (info.mode.0.max(0) as u32, info.mode.1.max(0) as u32);
        if info.turned { (h, w) } else { (w, h) }
    }

    /// Captures the pointer's next picture, unless one is on the way or the
    /// constraints do not allow one yet.
    fn capture(&mut self, qh: &QueueHandle<Self>) {
        if self.frame.is_some() || !self.described || self.broken.is_some() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let read = [
            wl_shm::Format::Argb8888,
            wl_shm::Format::Abgr8888,
            wl_shm::Format::Xrgb8888,
            wl_shm::Format::Xbgr8888,
        ];
        let Some(format) = read.into_iter().find(|f| self.formats.contains(f)) else {
            self.broken = Some(format!("no picture format read in {:?}", self.formats));
            return;
        };
        if self
            .shm
            .as_ref()
            .is_none_or(|s| (s.w, s.h, s.format) != (w, h, format))
        {
            self.shm = None;
            let Some(global) = &self.shm_global else {
                return;
            };
            match Shm::new(global, qh, (w, h), format) {
                Ok(shm) => self.shm = Some(shm),
                Err(e) => {
                    self.broken = Some(format!("no memory for the picture: {e:#}"));
                    return;
                }
            }
        }
        let shm = self.shm.as_ref().expect("made above");
        let frame = session.create_frame(qh, ());
        frame.attach_buffer(&shm.buffer);
        frame.damage_buffer(0, 0, w as i32, h as i32);
        frame.capture();
        self.frame = Some(frame);
    }

    /// Ends the session: the cursor session and its capture session first, then a
    /// frame still waiting (see the top of this file).
    fn close(&mut self, cursor: Option<ExtImageCopyCaptureCursorSessionV1>, conn: &Connection) {
        if let Some(c) = cursor {
            c.destroy();
        }
        if let Some(s) = self.session.take() {
            s.destroy();
        }
        if let Some(f) = self.frame.take() {
            f.destroy();
        }
        let _ = conn.flush();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::GlobalRemove { name } = event {
            if state.ours == Some(name) {
                state.gone = true;
            }
            if state.outputs.remove(&name).is_some() {
                state.outputs_done = true;
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, info)) = state.outputs.get_mut(global) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => info.name = Some(name),
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => info.mode = (width, height),
            wl_output::Event::Geometry {
                transform: WEnum::Value(t),
                ..
            } => {
                use wl_output::Transform as T;
                info.turned = matches!(t, T::_90 | T::_270 | T::Flipped90 | T::Flipped270);
            }
            wl_output::Event::Done => state.outputs_done = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_cursor_session_v1::Event;
        let now = Instant::now();
        match event {
            Event::Enter => state.with_tap(|t, tell| t.enter(tell)),
            Event::Leave => state.with_tap(|t, tell| t.leave(tell)),
            Event::Position { x, y } => state.with_tap(|t, tell| t.position(x, y, tell)),
            Event::Hotspot { x, y } => state.with_tap(|t, tell| t.hotspot(x, y, now, tell)),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => state.batch_size = (width, height),
            Event::ShmFormat {
                format: WEnum::Value(f),
            } => state.batch_formats.push(f),
            Event::Done => {
                state.size = state.batch_size;
                std::mem::swap(&mut state.formats, &mut state.batch_formats);
                state.batch_formats.clear();
                state.described = true;
                if state.size.0 == 0 || state.size.1 == 0 {
                    state.with_tap(|t, tell| t.no_picture(tell));
                }
                // A frame on the way fails if it no longer fits, and goes again then.
                state.capture(qh);
            }
            Event::Stopped => state.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::{Event, FailureReason};
        match event {
            Event::Ready => {
                frame.destroy();
                state.frame = None;
                state.failures = 0;
                if let Some(shm) = state.shm.take() {
                    let shot = Captured {
                        w: shm.w,
                        h: shm.h,
                        stride: shm.w * 4,
                        format: shm.format,
                        pixels: shm.pixels(),
                    };
                    let now = Instant::now();
                    let sink = state.sink.lock().unwrap().clone();
                    state.tap.picture(shot, now, |e| {
                        if let Some(sink) = &sink {
                            sink.send(e);
                        }
                    });
                    state.shm = Some(shm);
                }
                // The next one comes when the picture changes.
                state.capture(qh);
            }
            Event::Failed { reason } => {
                frame.destroy();
                state.frame = None;
                state.failures += 1;
                match reason {
                    WEnum::Value(FailureReason::Stopped) => state.stopped = true,
                    // The constraints changed: again at once with the new ones.
                    WEnum::Value(FailureReason::BufferConstraints) if state.failures < 5 => {
                        state.capture(qh);
                    }
                    _ => {
                        state.retry_at =
                            Some(Instant::now() + Duration::from_millis(200) * state.failures);
                    }
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ignore ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ignore ExtImageCaptureSourceV1);
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore wl_pointer::WlPointer);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);

/// Reads pending events, waiting at most `timeout` for new ones or for `stop`; whether
/// to stop.
fn wait(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    stop: i32,
    timeout: Duration,
) -> Result<bool> {
    queue.dispatch_pending(state)?;
    conn.flush()?;
    let mut stopped = false;
    if let Some(guard) = queue.prepare_read() {
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid pollfds for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout.as_millis() as i32) };
        stopped = n > 0 && fds[1].revents != 0;
        if n > 0 && fds[0].revents != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    queue.dispatch_pending(state)?;
    Ok(stopped)
}

/// A session that says nothing of the pointer's picture within this is taken for
/// none (Hyprland gives none to a client without the permission).
const ANSWER_WITHIN: Duration = Duration::from_secs(3);
/// A session the compositor stops is opened again, unless it stopped this many times.
const STOPS_AT_MOST: u32 = 3;

fn run(
    name: &str,
    state: &mut State,
    apart: &AtomicBool,
    units: &dyn Fn() -> Units,
    stop: i32,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("no Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: ExtImageCopyCaptureManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("the compositor has no ext-image-copy-capture")?;
    let sources: ExtOutputImageCaptureSourceManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("the compositor captures no outputs with ext-image-capture-source")?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=8, ()).context("no seat")?;
    state.shm_global = Some(globals.bind(&qh, 1..=1, ())?);
    // Our output, by its name (it may have just been made).
    let deadline = Instant::now() + Duration::from_secs(3);
    let output = loop {
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" && g.version >= 4 && !state.outputs.contains_key(&g.name)
            {
                let o = globals
                    .registry()
                    .bind::<wl_output::WlOutput, _, _>(g.name, 4, &qh, g.name);
                state.outputs.insert(g.name, (o, OutputInfo::default()));
            }
        }
        queue.roundtrip(state)?;
        let found = state
            .outputs
            .iter()
            .find(|(_, (_, i))| i.name.as_deref() == Some(name))
            .map(|(g, (o, _))| (*g, o.clone()));
        if let Some((global, output)) = found {
            state.ours = Some(global);
            break output;
        }
        if Instant::now() > deadline {
            bail!("output {name} did not appear");
        }
        if wait(&conn, &mut queue, state, stop, Duration::from_millis(100))? {
            return Ok(());
        }
    };
    let source = sources.create_source(&output, &qh, ());
    let pointer = seat.get_pointer(&qh, ());
    let mut stops = 0;
    loop {
        let cursor = manager.create_pointer_cursor_session(&source, &pointer, &qh, ());
        state.session = Some(cursor.get_capture_session(&qh, ()));
        state.described = false;
        state.stopped = false;
        state.failures = 0;
        state.retry_at = None;
        let opened = Instant::now();
        let ended = loop {
            if wait(&conn, &mut queue, state, stop, Duration::from_millis(250))? {
                break Ok(true);
            }
            if state.gone {
                break Ok(true);
            }
            if std::mem::take(&mut state.outputs_done) {
                let (size, units) = (state.output_size(), units());
                state.with_tap(|t, tell| t.geometry(size, units, tell));
            }
            if let Some(why) = state.broken.take() {
                break Err(anyhow::anyhow!(why));
            }
            if !state.described && opened.elapsed() > ANSWER_WITHIN {
                break Err(anyhow::anyhow!(
                    "the compositor says nothing of the pointer"
                ));
            }
            // The frames leave the pointer out while the headset draws it.
            let shows = state.tap.shows();
            if apart.swap(shows, Ordering::Relaxed) != shows {
                debug!(
                    "{name}: the pointer {} the frames",
                    if shows { "apart from" } else { "in" }
                );
            }
            if state.stopped {
                break Ok(false);
            }
            if state.failures >= 5 {
                break Err(anyhow::anyhow!("the pointer's picture cannot be captured"));
            }
            if state.retry_at.is_some_and(|t| Instant::now() >= t) {
                state.retry_at = None;
                state.capture(&qh);
            }
            if state.tap.wants_fresh() {
                break Ok(false);
            }
        };
        state.close(Some(cursor), &conn);
        match ended {
            Ok(true) => break,
            Ok(false) if state.stopped => {
                stops += 1;
                if stops >= STOPS_AT_MOST {
                    release(&pointer);
                    bail!("the compositor stopped the pointer session {stops} times");
                }
            }
            Ok(false) => {}
            Err(e) => {
                release(&pointer);
                return Err(e);
            }
        }
    }
    release(&pointer);
    source.destroy();
    let _ = conn.flush();
    Ok(())
}

/// `wl_pointer.release` where the seat is new enough (version 3).
fn release(pointer: &wl_pointer::WlPointer) {
    if wayland_client::Proxy::version(pointer) >= 3 {
        pointer.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(
        tap: &mut CursorTap,
        f: impl FnOnce(&mut CursorTap, &mut dyn FnMut(CursorEvent)),
    ) -> Vec<CursorEvent> {
        let mut out = Vec::new();
        f(tap, &mut |e| out.push(e));
        out
    }

    /// A 2x2 picture in Wayland's ARGB8888 (bytes B, G, R, A), premultiplied.
    fn argb(pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat(4)
    }

    fn shot(pixels: &[u8], w: u32, h: u32) -> Captured<'_> {
        Captured {
            w,
            h,
            stride: w * 4,
            format: wl_shm::Format::Argb8888,
            pixels,
        }
    }

    fn tap() -> CursorTap {
        let mut tap = CursorTap::new(OutputKey::new("Prostir-1"));
        tap.geometry((640, 360), Units::PROTOCOL, |_| {});
        tap
    }

    /// On the output with a look: the core hears it.
    fn on(tap: &mut CursorTap, x: i32, y: i32, look: &[u8], now: Instant) {
        heard(tap, |t, tell| t.enter(tell));
        heard(tap, |t, tell| t.position(x, y, tell));
        heard(tap, |t, tell| t.picture(shot(look, 2, 2), now, tell));
    }

    /// As the protocol has it (wlroots): enter, the hotspot, the place, the picture;
    /// the look (straight RGBA, its hotspot) and the place once the look is known,
    /// each move, off the picture once, back on it.
    #[test]
    fn a_pointer_session_as_the_core_hears_it() {
        let mut tap = tap();
        let now = Instant::now();
        assert!(heard(&mut tap, |t, tell| t.enter(tell)).is_empty());
        assert!(heard(&mut tap, |t, tell| t.hotspot(1, 1, now, tell)).is_empty());
        assert!(
            heard(&mut tap, |t, tell| t.position(10, 20, tell)).is_empty(),
            "no look yet"
        );
        assert!(!tap.shows());
        // B, G, R, A = 0, 50, 100, 128: straight R 199, G 100.
        let look = argb([0, 50, 100, 128]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), now, tell));
        let [
            CursorEvent::Shape(image),
            CursorEvent::Moved {
                x: 10,
                y: 20,
                size: (640, 360),
                ..
            },
        ] = &events[..]
        else {
            panic!("{events:?}");
        };
        assert!(tap.shows());
        assert_eq!((image.w, image.h, image.hot_x, image.hot_y), (2, 2, 1, 1));
        assert_eq!(image.rgba[..4], [199, 100, 0, 128]);
        // The same picture again: nothing new.
        assert!(heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), now, tell)).is_empty());
        assert!(matches!(
            heard(&mut tap, |t, tell| t.position(11, 20, tell))[..],
            [CursorEvent::Moved { x: 11, .. }]
        ));
        // The hotspot off the picture (the pointer's picture still touching it).
        assert!(matches!(
            heard(&mut tap, |t, tell| t.position(-1, 20, tell))[..],
            [CursorEvent::Left { .. }]
        ));
        assert!(heard(&mut tap, |t, tell| t.leave(tell)).is_empty(), "once");
        heard(&mut tap, |t, tell| t.enter(tell));
        let events = heard(&mut tap, |t, tell| t.position(5, 5, tell));
        assert!(
            matches!(events[..], [CursorEvent::Moved { x: 5, .. }]),
            "the look known: {events:?}"
        );
        // The session ends: off this output.
        assert!(matches!(
            heard(&mut tap, |t, tell| t.end(tell))[..],
            [CursorEvent::Left { .. }]
        ));
    }

    /// A clear picture (hidden, or Hyprland's for its own themes): the pointer goes
    /// into the frames (`shows` false) and the core hears it left; with a picture
    /// that shows, it is apart again.
    #[test]
    fn a_clear_picture_puts_the_pointer_into_the_frames() {
        let mut tap = tap();
        let now = Instant::now();
        let clear = argb([0, 0, 0, 0]);
        heard(&mut tap, |t, tell| t.enter(tell));
        heard(&mut tap, |t, tell| t.position(5, 5, tell));
        assert!(heard(&mut tap, |t, tell| t.picture(shot(&clear, 2, 2), now, tell)).is_empty());
        assert!(!tap.shows());
        let look = argb([0, 0, 255, 255]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), now, tell));
        assert!(
            matches!(
                events[..],
                [CursorEvent::Shape(_), CursorEvent::Moved { .. }]
            ),
            "{events:?}"
        );
        assert!(tap.shows());
        assert!(matches!(
            heard(&mut tap, |t, tell| t.picture(shot(&clear, 2, 2), now, tell))[..],
            [CursorEvent::Left { .. }]
        ));
        assert!(!tap.shows());
        assert!(heard(&mut tap, |t, tell| t.position(6, 6, tell)).is_empty());
        // No picture at all (of no size): the same.
        let mut tap = self::tap();
        on(&mut tap, 5, 5, &look, now);
        assert!(matches!(
            heard(&mut tap, |t, tell| t.no_picture(tell))[..],
            [CursorEvent::Left { .. }]
        ));
        assert!(!tap.shows());
    }

    /// The hotspot is in effect with the next picture, as the protocol says, or with
    /// the one just before it (Hyprland says it with the output's next frame).
    #[test]
    fn the_hotspot_goes_with_its_picture() {
        let mut tap = tap();
        let now = Instant::now();
        on(&mut tap, 10, 10, &argb([0, 0, 0, 255]), now);
        // Long after: for the next picture.
        let later = now + Duration::from_secs(1);
        assert!(heard(&mut tap, |t, tell| t.hotspot(1, 0, later, tell)).is_empty());
        let beam = argb([255, 255, 255, 255]);
        let events = heard(&mut tap, |t, tell| {
            t.picture(shot(&beam, 2, 2), later, tell);
        });
        assert!(
            matches!(&events[..], [CursorEvent::Shape(i)] if (i.hot_x, i.hot_y) == (1, 0)),
            "{events:?}"
        );
        // Right after a picture: with it.
        let soon = later + Duration::from_millis(5);
        let events = heard(&mut tap, |t, tell| t.hotspot(0, 1, soon, tell));
        assert!(
            matches!(&events[..], [CursorEvent::Shape(i)] if (i.hot_x, i.hot_y) == (0, 1)),
            "{events:?}"
        );
    }

    /// Hyprland: the place and the hotspot in logical pixels (times the output's
    /// scale), the picture drawn at the highest scale of the outputs (made the size it
    /// is on this one).
    #[test]
    fn hyprlands_logical_pixels() {
        let mut tap = CursorTap::new(OutputKey::new("Prostir-1"));
        tap.geometry((1920, 1080), Units::hyprland(1.0, 2.0), |_| {});
        let now = Instant::now();
        heard(&mut tap, |t, tell| t.enter(tell));
        heard(&mut tap, |t, tell| t.hotspot(2, 3, now, tell));
        heard(&mut tap, |t, tell| t.position(100, 50, tell));
        // A 4x4 picture at scale 2: 2x2 here; its top left pixel opaque only.
        let mut big = argb([0, 0, 0, 0]).repeat(4);
        big[..4].copy_from_slice(&[0, 0, 200, 255]);
        let later = now + Duration::from_secs(1);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&big, 4, 4), later, tell));
        let [
            CursorEvent::Shape(image),
            CursorEvent::Moved { x: 100, y: 50, .. },
        ] = &events[..]
        else {
            panic!("{events:?}");
        };
        assert_eq!(
            (image.w, image.h, image.hot_x, image.hot_y),
            (2, 2, 1, 1),
            "the hotspot within the picture"
        );
        // A quarter of the 2x2 block was opaque red: premultiplied mean, then straight.
        assert_eq!(image.rgba[..4], [199, 0, 0, 64]);
        // At 125 %: logical 100 is pixel 125.
        let mut tap = CursorTap::new(OutputKey::new("Prostir-1"));
        tap.geometry((2400, 1350), Units::hyprland(1.25, 1.25), |_| {});
        on(&mut tap, 0, 0, &argb([0, 0, 0, 255]), now);
        let events = heard(&mut tap, |t, tell| t.position(100, 40, tell));
        assert!(
            matches!(events[..], [CursorEvent::Moved { x: 125, y: 50, .. }]),
            "{events:?}"
        );
        // Another scale: the look is taken anew from a fresh session.
        tap.geometry((2400, 1350), Units::hyprland(1.25, 2.0), |_| {});
        assert!(tap.wants_fresh());
    }

    /// A picture taken while the pointer was off the output (black on Hyprland) is not
    /// its look: once the pointer is on it, a fresh session is asked for, once.
    #[test]
    fn a_picture_taken_off_the_output_is_not_its_look() {
        let mut tap = tap();
        let now = Instant::now();
        let black = argb([0, 0, 0, 255]);
        assert!(heard(&mut tap, |t, tell| t.picture(shot(&black, 2, 2), now, tell)).is_empty());
        assert!(!tap.wants_fresh(), "not on it");
        heard(&mut tap, |t, tell| t.enter(tell));
        assert!(heard(&mut tap, |t, tell| t.position(3, 3, tell)).is_empty());
        assert!(!tap.shows());
        assert!(tap.wants_fresh());
        assert!(!tap.wants_fresh(), "once");
        let arrow = argb([0, 0, 255, 255]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&arrow, 2, 2), now, tell));
        assert!(
            matches!(&events[..], [CursorEvent::Shape(i), CursorEvent::Moved { .. }] if i.rgba[..4] == [255, 0, 0, 255]),
            "{events:?}"
        );
        assert!(!tap.wants_fresh());
    }

    /// Only the output whose picture has the hotspot has the pointer, as on KDE: at
    /// the edge of two side by side, the pointer's picture touches both.
    #[test]
    fn only_the_output_with_the_hotspot_has_the_pointer() {
        let (mut left, mut right) = (tap(), tap());
        let now = Instant::now();
        let arrow = argb([0, 0, 0, 255]);
        on(&mut left, 638, 10, &arrow, now);
        heard(&mut right, |t, tell| t.enter(tell));
        assert!(heard(&mut right, |t, tell| t.position(-2, 10, tell)).is_empty());
        assert!(
            heard(&mut right, |t, tell| t.picture(
                shot(&arrow, 2, 2),
                now,
                tell
            ))
            .is_empty()
        );
        assert!(matches!(
            heard(&mut left, |t, tell| t.position(641, 10, tell))[..],
            [CursorEvent::Left { .. }]
        ));
        assert!(heard(&mut right, |t, tell| t.position(1, 10, tell)).is_empty());
        assert!(right.wants_fresh(), "its picture came while off it");
        let events = heard(&mut right, |t, tell| {
            t.picture(shot(&arrow, 2, 2), now, tell);
        });
        assert!(
            matches!(
                events[..],
                [CursorEvent::Shape(_), CursorEvent::Moved { x: 1, .. }]
            ),
            "{events:?}"
        );
    }

    #[test]
    fn reads_the_formats_of_shared_memory() {
        let mut out = Vec::new();
        let pixel = [1, 2, 3, 4];
        let mut s = shot(&pixel, 1, 1);
        assert!(premultiplied_rgba(s, &mut out));
        assert_eq!(out, [3, 2, 1, 4]);
        s.format = wl_shm::Format::Xbgr8888;
        assert!(premultiplied_rgba(s, &mut out));
        assert_eq!(out, [1, 2, 3, 255]);
        s.format = wl_shm::Format::Rgb565;
        assert!(!premultiplied_rgba(s, &mut out));
        // Rows apart: only the pixels of each.
        let rows = [1, 2, 3, 4, 9, 9, 5, 6, 7, 8, 9, 9];
        let s = Captured {
            w: 1,
            h: 2,
            stride: 6,
            format: wl_shm::Format::Abgr8888,
            pixels: &rows,
        };
        assert!(premultiplied_rgba(s, &mut out));
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    /// Pictures of a pointer that keeps its look, and its moves, take no memory: the
    /// buffers stay, and only the place is told.
    #[test]
    fn the_same_look_again_takes_no_memory() {
        let mut tap = tap();
        let now = Instant::now();
        let arrow = argb([0, 0, 255, 255]);
        on(&mut tap, 3, 3, &arrow, now);
        let kept = |t: &CursorTap| (t.scratch.as_ptr(), t.look.as_ref().map(|l| l.rgba.as_ptr()));
        let before = kept(&tap);
        for x in 4..8 {
            assert!(heard(&mut tap, |t, tell| t.picture(shot(&arrow, 2, 2), now, tell)).is_empty());
            assert_eq!(heard(&mut tap, |t, tell| t.position(x, 3, tell)).len(), 1);
        }
        assert_eq!(before, kept(&tap));
    }

    #[test]
    fn needs_both_capture_interfaces_and_a_seat() {
        let all = [
            "wl_seat",
            "zwlr_screencopy_manager_v1",
            "ext_image_copy_capture_manager_v1",
            "ext_output_image_capture_source_manager_v1",
        ];
        assert!(offers_sessions(all));
        // Sway 1.10 and Hyprland before 0.54: wlr-screencopy only.
        assert!(!offers_sessions(["wl_seat", "zwlr_screencopy_manager_v1"]));
        assert!(!offers_sessions(all[1..].iter().copied()));
    }

    /// By hand, against the compositor of this session: what the pointer session of
    /// output `PROSTIR_POINTER_OUTPUT` tells for 10 s, and whether the frames would
    /// leave the pointer out (`cargo test -p prostir -- --ignored --nocapture
    /// pointer_session_live`).
    #[test]
    #[ignore = "needs a wlroots compositor"]
    fn pointer_session_live() {
        let Ok(name) = std::env::var("PROSTIR_POINTER_OUTPUT") else {
            return;
        };
        println!("pointer sessions offered: {}", offered());
        let sink: SinkSlot = Arc::new(Mutex::new(Some(CursorSink::new(|e| {
            if let CursorEvent::Shape(i) = &e {
                let alpha: u64 = i
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| u64::from(p[3]))
                    .sum();
                println!("{e:?}, alpha {alpha}");
            } else {
                println!("{e:?}");
            }
        }))));
        let apart = Arc::new(AtomicBool::new(false));
        let units: Box<dyn Fn() -> Units + Send> = if super::super::running() {
            let name = name.clone();
            Box::new(move || super::super::units_of(&name))
        } else {
            Box::new(|| Units::PROTOCOL)
        };
        let session = PointerSession::start(name, sink, apart.clone(), units).unwrap();
        let mut was = None;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            let now = apart.load(Ordering::Relaxed);
            if was != Some(now) {
                println!("apart from the frames: {now}");
                was = Some(now);
            }
        }
        drop(session);
    }
}
