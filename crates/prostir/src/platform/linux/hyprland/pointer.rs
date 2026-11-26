//! The pointer apart from the frames on wlroots desktops (Sway 1.11 and newer, and so
//! Wayfire and labwc on wlroots 0.19; Hyprland once it gives the pointer's pictures,
//! see `mod.rs`): a pointer cursor session of `ext-image-copy-capture-v1` per output,
//! told to the core as [`CursorEvent`]s (`CursorSource`), while the frames are captured
//! without the pointer (`screencopy.rs`, `overlay_cursor` 0).
//!
//! A session says when the pointer's picture touches the output (`enter`, `leave`),
//! where its hotspot is (`position`, also off the output's picture) and the hotspot in
//! its picture (`hotspot`, in effect with the next picture); the picture itself comes
//! as a small frame, captured into shared memory, whose capture waits until the
//! picture changes. A clear picture is a hidden pointer. None of it touches the
//! output's frames or their gate: the pointer moving gives no frame to encode.
//!
//! All outputs' sessions are on one thread and one Wayland connection
//! ([`PointerHub`]), so what the core hears of them comes in the order the compositor
//! said it (the pointer leaving one output before it is on the next). Each output's
//! frames and its session agree through its [`Overlay`]: the session asks the pointer
//! out once its first picture came, the capture switches with its next frame (at once,
//! the frame waiting for a change given up), and the core hears the pointer on the
//! output only once frames without it went out, and off it only once frames with it
//! did. A session the compositor stops (Sway stops one opened while the pointer is
//! hidden) is opened again later and later, the pointer in the frames meanwhile.
//!
//! Hyprland differs from the protocol, which [`Units`] and [`CursorTap`] take in: it
//! says the place and the hotspot in logical pixels; it draws the pointer's picture at
//! the highest scale of its outputs; and a picture captured while the pointer is off
//! the output is black (not taken as the pointer's look: the session starts over once
//! the pointer is on it, its first picture coming at once). Hyprland up to 0.57 could
//! crash when a client dropped a cursor frame waiting for a change while its session
//! lived on: a frame here is dropped only once done, or after its session.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
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

use crate::encoder::Pause;
use crate::platform::linux::straighten;
use crate::platform::{CursorEvent, CursorImage, CursorSink, OutputKey};

/// Where the core's sink is while it asks for the pointer apart; none otherwise. One
/// for the desktop, shared with the pointer sessions.
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

/// Wakes a thread that polls it (an eventfd).
pub struct Waker(OwnedFd);

impl Waker {
    pub fn new() -> Result<Self> {
        // SAFETY: a new eventfd; owned below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: `fd` was just made and belongs to nobody else.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub fn wake(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: writes 8 bytes to the eventfd owned here.
        unsafe { libc::write(self.0.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }

    /// Takes the wakes so far.
    fn drain(&self) {
        let mut n = [0u8; 8];
        // SAFETY: reads at most 8 bytes into `n` from the eventfd owned here (it does
        // not block).
        unsafe { libc::read(self.0.as_raw_fd(), n.as_mut_ptr().cast(), n.len()) };
    }

    fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// What the frames pushed last had: none pushed (the capture paused or not running).
const NOT_PUSHED: u8 = 0;
const PUSHED_IN: u8 = 1;
const PUSHED_APART: u8 = 2;

/// How one output's frames have the pointer, which its pointer session and its
/// capture agree through: the session asks the pointer out or in, the capture
/// switches with its next frame and says what it pushed.
#[derive(Default)]
pub struct Overlay {
    /// The pointer left out of the frames, as asked.
    apart: AtomicBool,
    /// What the frames pushed last had (`PUSHED_*`).
    pushed: AtomicU8,
    /// The capture's pause, opened to switch at once.
    capture: Mutex<Option<Arc<Pause>>>,
    /// The pointer sessions' thread, woken when the frames switched.
    hub: Mutex<Option<Arc<Waker>>>,
}

impl Overlay {
    /// The pointer left out of the frames (`apart`) or drawn in: the capture switches
    /// at once (a frame waiting for a change is given up).
    pub fn ask(&self, apart: bool) {
        if self.apart.swap(apart, Ordering::Relaxed) != apart
            && let Some(pause) = &*self.capture.lock().unwrap()
        {
            pause.open_gate();
        }
    }

    /// Whether the next frame leaves the pointer out (the capture asks for each).
    pub fn asked(&self) -> bool {
        self.apart.load(Ordering::Relaxed)
    }

    /// A frame went out with the pointer left out (`apart`) or drawn in.
    pub fn pushed(&self, apart: bool) {
        self.set_pushed(if apart { PUSHED_APART } else { PUSHED_IN });
    }

    /// The capture stopped or paused: no frame goes out, the next one as asked.
    pub fn not_pushing(&self) {
        self.set_pushed(NOT_PUSHED);
    }

    fn set_pushed(&self, pushed: u8) {
        if self.pushed.swap(pushed, Ordering::Relaxed) != pushed
            && let Some(hub) = &*self.hub.lock().unwrap()
        {
            hub.wake();
        }
    }

    /// Whether the frames leave the pointer out now: those that went out last, or,
    /// with none going out, the next one.
    pub fn frames_apart(&self) -> bool {
        match self.pushed.load(Ordering::Relaxed) {
            PUSHED_APART => true,
            PUSHED_IN => false,
            _ => self.asked(),
        }
    }

    /// The capture that runs now (its pause), none when it ended.
    pub fn capture(&self, pause: Option<Arc<Pause>>) {
        *self.capture.lock().unwrap() = pause;
    }

    fn hub(&self, waker: Option<Arc<Waker>>) {
        *self.hub.lock().unwrap() = waker;
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

/// What one output's pointer session told the core of the pointer, to tell it only
/// what changed: the compositor's events in, [`CursorEvent`]s out.
#[derive(Debug)]
pub struct CursorTap {
    output: OutputKey,
    /// The output's picture, in pixels, and how the compositor counts.
    size: (u32, u32),
    units: Units,
    /// The output's frames leave the pointer out: only then is it told on it.
    frames_apart: bool,
    /// The pointer's picture touches the output (`enter`, until `leave`).
    entered: bool,
    /// Its hotspot's place, as said.
    place: Option<(i32, i32)>,
    /// The hotspot in its picture, in effect; the one said for the next picture.
    hot: (i32, i32),
    hot_next: Option<(i32, i32)>,
    /// Its look in the output's pixels (straight RGBA), none before a picture came.
    look: Option<Look>,
    /// A picture came while the pointer was off this output (black on Hyprland): the
    /// look may be old, and a fresh session is asked for once it is on it.
    pictured_off: bool,
    fresh_asked: bool,
    /// As told: on this output, at, the hotspot told with the look (none: the look
    /// not told yet), hidden.
    on: bool,
    at: Option<(i32, i32)>,
    told: Option<(i32, i32)>,
    hidden: bool,
    /// Kept for each picture: nothing is allocated while the look stays.
    scratch: Vec<u8>,
    scaled: Vec<u8>,
}

#[derive(Debug)]
struct Look {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    /// Not a pixel shows: hidden.
    clear: bool,
}

impl CursorTap {
    pub fn new(output: OutputKey) -> Self {
        Self {
            output,
            size: (0, 0),
            units: Units::PROTOCOL,
            frames_apart: false,
            entered: false,
            place: None,
            hot: (0, 0),
            hot_next: None,
            look: None,
            pictured_off: false,
            fresh_asked: false,
            on: false,
            at: None,
            told: None,
            hidden: false,
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

    /// The output's frames leave the pointer out (`apart`) or have it drawn in: the
    /// pointer is told on the output only while they leave it out.
    pub fn frames(&mut self, apart: bool, tell: impl FnMut(CursorEvent)) {
        if std::mem::replace(&mut self.frames_apart, apart) != apart {
            self.update(tell);
        }
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

    /// The hotspot, in effect with the next picture.
    pub fn hotspot(&mut self, x: i32, y: i32) {
        self.hot_next = Some((x, y));
    }

    /// The compositor has no picture of the pointer at all (of no size): hidden.
    pub fn no_picture(&mut self, tell: impl FnMut(CursorEvent)) {
        self.look = Some(Look {
            w: 0,
            h: 0,
            rgba: Vec::new(),
            clear: true,
        });
        self.update(tell);
    }

    /// A picture of the pointer came.
    pub fn picture(&mut self, shot: Captured<'_>, tell: impl FnMut(CursorEvent)) {
        if let Some(hot) = self.hot_next.take() {
            self.hot = hot;
            self.told = None;
        }
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
            let clear =
                w == 0 || h == 0 || self.scratch.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
            let look = self.look.get_or_insert_with(|| Look {
                w,
                h,
                rgba: Vec::new(),
                clear,
            });
            (look.w, look.h, look.clear) = (w, h, clear);
            look.rgba.clear();
            look.rgba.extend_from_slice(&self.scratch);
            self.told = None;
        }
        self.update(tell);
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
        let Some((x, y)) = self.inside().filter(|_| self.frames_apart) else {
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
        if let Some(look) = &self.look {
            if look.clear {
                if !std::mem::replace(&mut self.hidden, true) {
                    tell(CursorEvent::Hidden);
                }
            } else if self.told != Some(hot) || self.hidden {
                let side = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
                let within = |v: i32, n: u32| u16::try_from(v.clamp(0, n as i32 - 1)).unwrap_or(0);
                self.told = Some(hot);
                self.hidden = false;
                tell(CursorEvent::Shape(CursorImage {
                    w: side(look.w),
                    h: side(look.h),
                    hot_x: within(hot.0, look.w),
                    hot_y: within(hot.1, look.h),
                    rgba: look.rgba.clone(),
                }));
            }
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

/// The pointer sessions of every output asked apart, on one thread and one Wayland
/// connection (see the top of this file). Dropping it ends them all.
pub struct PointerHub {
    asks: mpsc::Sender<Ask>,
    waker: Arc<Waker>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// What an output asks of the hub.
enum Ask {
    /// The pointer apart on output `name`, its frames following `overlay`.
    Watch(String, Arc<Overlay>),
    /// The pointer in its frames again (or the output going).
    Unwatch(String),
}

impl PointerHub {
    /// Starts the hub: it tells `sink` (while that holds one) of the pointer on the
    /// outputs asked, counted in the `units` it gives for each (at the start and on
    /// each change of the outputs).
    pub fn start(sink: SinkSlot, units: Box<dyn Fn(&str) -> Units + Send>) -> Result<Self> {
        let waker = Arc::new(Waker::new()?);
        let stop = Arc::new(AtomicBool::new(false));
        let (asks, asked) = mpsc::channel();
        let thread = {
            let (waker, stop) = (waker.clone(), stop.clone());
            std::thread::Builder::new()
                .name("pointer".into())
                .spawn(move || {
                    let mut state = State::new(sink, waker.clone());
                    if let Err(e) = run(&mut state, &asked, &*units, &stop) {
                        warn!("the pointer goes into the frames again: {e:#}");
                    }
                    state.close_all();
                })?
        };
        Ok(Self {
            asks,
            waker,
            stop,
            thread: Some(thread),
        })
    }

    /// The pointer apart from the frames of output `name` (its capture following
    /// `overlay`), once its session gave the pointer's first picture.
    pub fn watch(&self, name: &str, overlay: Arc<Overlay>) {
        let _ = self.asks.send(Ask::Watch(name.to_owned(), overlay));
        self.waker.wake();
    }

    /// The pointer in the frames of output `name` again: the core hears it is off it.
    pub fn unwatch(&self, name: &str) {
        let _ = self.asks.send(Ask::Unwatch(name.to_owned()));
        self.waker.wake();
    }
}

impl Drop for PointerHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.waker.wake();
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

/// What the objects of one session carry: the output's number in the hub and the
/// session's own, so nothing said to a session closed is taken for its successor's.
type Key = (u32, u32);

/// One open pointer session of an output.
struct Session {
    key: Key,
    cursor: ExtImageCopyCaptureCursorSessionV1,
    capture: ExtImageCopyCaptureSessionV1,
    opened: Instant,
    /// The constraints of the batch said last (`done`), and of the one being said.
    size: (u32, u32),
    formats: Vec<wl_shm::Format>,
    batch_size: (u32, u32),
    batch_formats: Vec<wl_shm::Format>,
    described: bool,
    stopped: bool,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    /// Captures that failed in a row, and when to try again after one.
    failures: u32,
    retry_at: Option<Instant>,
}

impl Session {
    /// Ends it: the cursor session and its capture session first, then a frame still
    /// waiting (see the top of this file).
    fn close(self) {
        self.cursor.destroy();
        self.capture.destroy();
        if let Some(f) = self.frame {
            f.destroy();
        }
    }
}

/// One output asked apart.
struct Watched {
    name: String,
    overlay: Arc<Overlay>,
    tap: CursorTap,
    /// Its `wl_output` (by registry name) and capture source, once found.
    global: Option<u32>,
    source: Option<ExtImageCaptureSourceV1>,
    session: Option<Session>,
    /// The buffer its pictures are captured into, kept across sessions.
    shm: Option<Shm>,
    /// Sessions stopped in a row (no picture between), and when to open the next.
    stops: u32,
    reopen_at: Instant,
}

/// How long after its `stops`-th stop in a row a session is opened again: from 1 s,
/// twice as long each time, at most 5 s (a pointer shown again after a video is apart
/// again within that).
fn backoff(stops: u32) -> Duration {
    (Duration::from_millis(500) * (1 << stops.min(4))).min(Duration::from_secs(5))
}

/// What a watched output's session is due for.
enum Due {
    Open,
    End,
    Capture,
    Nothing,
}

/// A session that says nothing of the pointer's picture within this is taken for
/// stopped (Hyprland gives none to a client without the permission).
const ANSWER_WITHIN: Duration = Duration::from_secs(3);

struct State {
    sink: SinkSlot,
    waker: Arc<Waker>,
    outputs: HashMap<u32, (wl_output::WlOutput, OutputInfo)>,
    /// An output came, went or said all it had to say: sizes and units anew.
    outputs_done: bool,
    watched: HashMap<u32, Watched>,
    next: u32,
    shm_global: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    /// The seat has a pointer (else asking for it is a protocol error on Sway), and
    /// the one taken.
    seat_pointer: bool,
    pointer: Option<wl_pointer::WlPointer>,
}

/// The size of the picture of output `global`: its mode, turned as it is.
fn output_size(
    outputs: &HashMap<u32, (wl_output::WlOutput, OutputInfo)>,
    global: Option<u32>,
) -> (u32, u32) {
    let Some((_, info)) = global.and_then(|g| outputs.get(&g)) else {
        return (0, 0);
    };
    let (w, h) = (info.mode.0.max(0) as u32, info.mode.1.max(0) as u32);
    if info.turned { (h, w) } else { (w, h) }
}

/// The core's sink as it is now, to tell `f`'s events to.
fn telling(sink: &SinkSlot) -> impl FnMut(CursorEvent) + use<> {
    let sink = sink.lock().unwrap().clone();
    move |e| {
        if let Some(sink) = &sink {
            sink.send(e);
        }
    }
}

impl State {
    fn new(sink: SinkSlot, waker: Arc<Waker>) -> Self {
        Self {
            sink,
            waker,
            outputs: HashMap::new(),
            outputs_done: true,
            watched: HashMap::new(),
            next: 0,
            shm_global: None,
            seat: None,
            seat_pointer: false,
            pointer: None,
        }
    }

    fn watch(&mut self, name: String, overlay: Arc<Overlay>) {
        if self.watched.values().any(|w| w.name == name) {
            return;
        }
        overlay.hub(Some(self.waker.clone()));
        self.next += 1;
        let tap = CursorTap::new(OutputKey::new(name.clone()));
        self.watched.insert(
            self.next,
            Watched {
                name,
                overlay,
                tap,
                global: None,
                source: None,
                session: None,
                shm: None,
                stops: 0,
                reopen_at: Instant::now(),
            },
        );
        self.outputs_done = true;
    }

    /// Ends what was opened for output `id`: the pointer in its frames, and the core
    /// hears it is off it.
    fn unwatch(&mut self, id: u32) {
        let Some(mut w) = self.watched.remove(&id) else {
            return;
        };
        if let Some(s) = w.session.take() {
            s.close();
        }
        if let Some(s) = w.source.take() {
            s.destroy();
        }
        w.overlay.ask(false);
        w.overlay.hub(None);
        w.tap.end(telling(&self.sink));
    }

    fn close_all(&mut self) {
        let ids: Vec<u32> = self.watched.keys().copied().collect();
        for id in ids {
            self.unwatch(id);
        }
    }

    /// The watched output and its open session that `key` names, if still so.
    fn session(&mut self, key: Key) -> Option<(&mut Watched, &SinkSlot)> {
        let w = self.watched.get_mut(&key.0)?;
        w.session.as_ref().filter(|s| s.key == key)?;
        Some((w, &self.sink))
    }

    /// One look at every output watched: its output found, its session opened,
    /// ended or opened again as due, and the core told what the frames allow.
    fn step(
        &mut self,
        manager: &ExtImageCopyCaptureManagerV1,
        sources: &ExtOutputImageCaptureSourceManagerV1,
        qh: &QueueHandle<Self>,
    ) {
        let now = Instant::now();
        if self.pointer.is_none()
            && self.seat_pointer
            && let Some(seat) = &self.seat
        {
            self.pointer = Some(seat.get_pointer(qh, ()));
        }
        // Fields apart, so nothing is collected to go through them.
        let Self {
            sink,
            outputs,
            outputs_done,
            watched,
            next,
            shm_global,
            pointer,
            ..
        } = self;
        for (&id, w) in watched.iter_mut() {
            if w.global.is_none()
                && let Some((global, (output, _))) = outputs
                    .iter()
                    .find(|(_, (_, i))| i.name.as_deref() == Some(w.name.as_str()))
            {
                w.global = Some(*global);
                w.source = Some(sources.create_source(output, qh, ()));
                *outputs_done = true;
            }
            let due = match &w.session {
                None if now >= w.reopen_at && w.source.is_some() && pointer.is_some() => Due::Open,
                None => Due::Nothing,
                Some(s)
                    if s.stopped
                        || s.failures >= 5
                        || (!s.described && now.duration_since(s.opened) > ANSWER_WITHIN) =>
                {
                    Due::End
                }
                Some(s) if s.retry_at.is_some_and(|t| now >= t) => Due::Capture,
                Some(_) => Due::Nothing,
            };
            match due {
                Due::Open => {
                    let (Some(source), Some(pointer)) = (&w.source, &*pointer) else {
                        continue;
                    };
                    *next += 1;
                    let key = (id, *next);
                    let cursor = manager.create_pointer_cursor_session(source, pointer, qh, key);
                    let capture = cursor.get_capture_session(qh, key);
                    w.session = Some(Session {
                        key,
                        cursor,
                        capture,
                        opened: now,
                        size: (0, 0),
                        formats: Vec::new(),
                        batch_size: (0, 0),
                        batch_formats: Vec::new(),
                        described: false,
                        stopped: false,
                        frame: None,
                        failures: 0,
                        retry_at: None,
                    });
                }
                Due::End => {
                    // Again later and later, the pointer in the frames meanwhile.
                    w.stops += 1;
                    w.reopen_at = now + backoff(w.stops);
                    debug!(
                        "{}: the pointer session stopped ({} in a row), again in {:?}",
                        w.name,
                        w.stops,
                        backoff(w.stops)
                    );
                    if let Some(s) = w.session.take() {
                        s.close();
                    }
                    w.overlay.ask(false);
                }
                Due::Capture => {
                    if let Some(s) = &mut w.session {
                        s.retry_at = None;
                    }
                    capture(w, shm_global.as_ref(), qh);
                }
                Due::Nothing => {
                    if w.session.is_some() && w.tap.wants_fresh() {
                        // A picture taken now, the pointer on this output.
                        if let Some(s) = w.session.take() {
                            s.close();
                        }
                        w.reopen_at = now;
                    }
                }
            }
            let apart = w.overlay.frames_apart();
            w.tap.frames(apart, telling(sink));
        }
    }

    /// When the next step is due at the latest (a session to open again or to
    /// capture again).
    fn next_due(&self) -> Option<Instant> {
        self.watched
            .values()
            .filter_map(|w| match &w.session {
                None => Some(w.reopen_at),
                Some(s) => s.retry_at,
            })
            .min()
    }
}

/// Captures the pointer's next picture on `w`'s session, unless one is on the way or
/// the constraints do not allow one yet.
fn capture(w: &mut Watched, shm: Option<&wl_shm::WlShm>, qh: &QueueHandle<State>) {
    let Some(s) = &mut w.session else {
        return;
    };
    if s.frame.is_some() || !s.described {
        return;
    }
    let (width, height) = s.size;
    if width == 0 || height == 0 {
        return;
    }
    let read = [
        wl_shm::Format::Argb8888,
        wl_shm::Format::Abgr8888,
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Xbgr8888,
    ];
    let Some(format) = read.into_iter().find(|f| s.formats.contains(f)) else {
        warn!(
            "{}: no pointer picture format read in {:?}",
            w.name, s.formats
        );
        s.stopped = true;
        return;
    };
    if w.shm
        .as_ref()
        .is_none_or(|b| (b.w, b.h, b.format) != (width, height, format))
    {
        w.shm = None;
        let Some(global) = shm else {
            return;
        };
        match Shm::new(global, qh, (width, height), format) {
            Ok(b) => w.shm = Some(b),
            Err(e) => {
                warn!("{}: no memory for the pointer's picture: {e:#}", w.name);
                s.stopped = true;
                return;
            }
        }
    }
    let b = w.shm.as_ref().expect("made above");
    let frame = s.capture.create_frame(qh, s.key);
    frame.attach_buffer(&b.buffer);
    frame.damage_buffer(0, 0, width as i32, height as i32);
    frame.capture();
    s.frame = Some(frame);
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" && version >= 4 => {
                let o = registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, name);
                state.outputs.insert(name, (o, OutputInfo::default()));
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.outputs.remove(&name).is_some() {
                    state.outputs_done = true;
                }
                // Its output gone: so is its session.
                for w in state
                    .watched
                    .values_mut()
                    .filter(|w| w.global == Some(name))
                {
                    w.global = None;
                    if let Some(s) = w.session.take() {
                        s.close();
                    }
                    if let Some(s) = w.source.take() {
                        s.destroy();
                    }
                    w.overlay.ask(false);
                    w.tap.end(telling(&state.sink));
                }
            }
            _ => {}
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

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(c),
        } = event
        {
            state.seat_pointer |= c.contains(wl_seat::Capability::Pointer);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, Key> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        key: &Key,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_cursor_session_v1::Event;
        let Some((w, sink)) = state.session(*key) else {
            return;
        };
        let tell = telling(sink);
        match event {
            Event::Enter => w.tap.enter(tell),
            Event::Leave => w.tap.leave(tell),
            Event::Position { x, y } => w.tap.position(x, y, tell),
            Event::Hotspot { x, y } => w.tap.hotspot(x, y),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, Key> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        key: &Key,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let shm = state.shm_global.clone();
        let Some((w, sink)) = state.session(*key) else {
            return;
        };
        let tell = telling(sink);
        let s = w.session.as_mut().expect("found above");
        match event {
            Event::BufferSize { width, height } => s.batch_size = (width, height),
            Event::ShmFormat {
                format: WEnum::Value(f),
            } => s.batch_formats.push(f),
            Event::Done => {
                s.size = s.batch_size;
                std::mem::swap(&mut s.formats, &mut s.batch_formats);
                s.batch_formats.clear();
                s.described = true;
                if s.size.0 == 0 || s.size.1 == 0 {
                    // No picture at all: hidden.
                    w.tap.no_picture(tell);
                    w.stops = 0;
                    w.overlay.ask(true);
                }
                // A frame on the way fails if it no longer fits, and goes again then.
                capture(w, shm.as_ref(), qh);
            }
            Event::Stopped => s.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, Key> for State {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        key: &Key,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::{Event, FailureReason};
        let shm = state.shm_global.clone();
        let Some((w, sink)) = state.session(*key) else {
            return;
        };
        let tell = telling(sink);
        let s = w.session.as_mut().expect("found above");
        match event {
            Event::Ready => {
                frame.destroy();
                s.frame = None;
                s.failures = 0;
                if let Some(b) = &w.shm {
                    let shot = Captured {
                        w: b.w,
                        h: b.h,
                        stride: b.w * 4,
                        format: b.format,
                        pixels: b.pixels(),
                    };
                    w.tap.picture(shot, tell);
                    // The session works: the pointer out of the frames.
                    w.stops = 0;
                    w.overlay.ask(true);
                }
                // The next one comes when the picture changes.
                capture(w, shm.as_ref(), qh);
            }
            Event::Failed { reason } => {
                frame.destroy();
                s.frame = None;
                s.failures += 1;
                match reason {
                    WEnum::Value(FailureReason::Stopped) => s.stopped = true,
                    // The constraints changed: again at once with the new ones.
                    WEnum::Value(FailureReason::BufferConstraints) if s.failures < 5 => {
                        capture(w, shm.as_ref(), qh);
                    }
                    _ => {
                        s.retry_at = Some(Instant::now() + Duration::from_millis(200) * s.failures);
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
delegate_noop!(State: ignore wl_pointer::WlPointer);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);

/// Reads pending events, waiting at most `timeout` for new ones or for the waker.
fn wait(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    timeout: Duration,
) -> Result<()> {
    queue.dispatch_pending(state)?;
    conn.flush()?;
    if let Some(guard) = queue.prepare_read() {
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: state.waker.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid pollfds for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout.as_millis() as i32) };
        if n > 0 && fds[1].revents != 0 {
            state.waker.drain();
        }
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
    conn.flush()?;
    Ok(())
}

fn run(
    state: &mut State,
    asked: &mpsc::Receiver<Ask>,
    units: &dyn Fn(&str) -> Units,
    stop: &AtomicBool,
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
    state.seat = Some(globals.bind(&qh, 1..=8, ()).context("no seat")?);
    state.shm_global = Some(globals.bind(&qh, 1..=1, ())?);
    for g in globals.contents().clone_list() {
        if g.interface == "wl_output" && g.version >= 4 {
            let o = globals
                .registry()
                .bind::<wl_output::WlOutput, _, _>(g.name, 4, &qh, g.name);
            state.outputs.insert(g.name, (o, OutputInfo::default()));
        }
    }
    queue.roundtrip(state)?;
    info!("the pointer sessions run");
    loop {
        let now = Instant::now();
        let timeout = state
            .next_due()
            .map_or(Duration::from_millis(250), |t| {
                t.saturating_duration_since(now)
            })
            .min(Duration::from_millis(250));
        wait(&conn, &mut queue, state, timeout)?;
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        while let Ok(ask) = asked.try_recv() {
            match ask {
                Ask::Watch(name, overlay) => state.watch(name, overlay),
                Ask::Unwatch(name) => {
                    let id = state
                        .watched
                        .iter()
                        .find(|(_, w)| w.name == name)
                        .map(|(id, _)| *id);
                    if let Some(id) = id {
                        state.unwatch(id);
                    }
                }
            }
        }
        state.step(&manager, &sources, &qh);
        if std::mem::take(&mut state.outputs_done) {
            for w in state.watched.values_mut() {
                let size = output_size(&state.outputs, w.global);
                w.tap.geometry(size, units(&w.name), telling(&state.sink));
            }
        }
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

    /// A tap of a 640x360 output whose frames leave the pointer out.
    fn tap() -> CursorTap {
        let mut tap = CursorTap::new(OutputKey::new("HEADLESS-1"));
        tap.geometry((640, 360), Units::PROTOCOL, |_| {});
        tap.frames(true, |_| {});
        tap
    }

    /// On the output with a look: the core heard it.
    fn on(tap: &mut CursorTap, x: i32, y: i32, look: &[u8]) {
        heard(tap, |t, tell| t.enter(tell));
        heard(tap, |t, tell| t.position(x, y, tell));
        heard(tap, |t, tell| t.picture(shot(look, 2, 2), tell));
    }

    /// As the protocol has it (wlroots): enter, the hotspot, the place, the picture;
    /// the place, the look once (straight RGBA, its hotspot), each move, off the
    /// picture once, hidden by a clear picture, shown by the next.
    #[test]
    fn a_pointer_session_as_the_core_hears_it() {
        let mut tap = tap();
        assert!(heard(&mut tap, |t, tell| t.enter(tell)).is_empty());
        tap.hotspot(1, 1);
        let events = heard(&mut tap, |t, tell| t.position(10, 20, tell));
        assert!(
            matches!(
                events[..],
                [CursorEvent::Moved {
                    x: 10,
                    y: 20,
                    size: (640, 360),
                    ..
                }]
            ),
            "{events:?}"
        );
        // B, G, R, A = 0, 50, 100, 128: straight R 199, G 100.
        let look = argb([0, 50, 100, 128]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), tell));
        let [CursorEvent::Shape(image)] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!((image.w, image.h, image.hot_x, image.hot_y), (2, 2, 1, 1));
        assert_eq!(image.rgba[..4], [199, 100, 0, 128]);
        // The same picture again: nothing new.
        assert!(heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), tell)).is_empty());
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
        assert!(matches!(
            heard(&mut tap, |t, tell| t.position(5, 5, tell))[..],
            [CursorEvent::Moved { x: 5, .. }]
        ));
        let clear = argb([0, 0, 0, 0]);
        assert!(matches!(
            heard(&mut tap, |t, tell| t.picture(shot(&clear, 2, 2), tell))[..],
            [CursorEvent::Hidden]
        ));
        assert!(matches!(
            heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), tell))[..],
            [CursorEvent::Shape(_)]
        ));
        assert!(matches!(
            heard(&mut tap, |t, tell| t.no_picture(tell))[..],
            [CursorEvent::Hidden]
        ));
        // The session ends: off this output.
        assert!(matches!(
            heard(&mut tap, |t, tell| t.end(tell))[..],
            [CursorEvent::Left { .. }]
        ));
    }

    /// The core hears the pointer on the output only while its frames leave it out:
    /// once they do, all of it at once; once they have it again, that it left.
    #[test]
    fn the_core_hears_the_pointer_only_while_the_frames_leave_it_out() {
        let mut tap = CursorTap::new(OutputKey::new("HEADLESS-1"));
        tap.geometry((640, 360), Units::PROTOCOL, |_| {});
        let look = argb([0, 0, 255, 255]);
        heard(&mut tap, |t, tell| t.enter(tell));
        assert!(heard(&mut tap, |t, tell| t.position(5, 5, tell)).is_empty());
        assert!(heard(&mut tap, |t, tell| t.picture(shot(&look, 2, 2), tell)).is_empty());
        let events = heard(&mut tap, |t, tell| t.frames(true, tell));
        assert!(
            matches!(
                events[..],
                [CursorEvent::Shape(_), CursorEvent::Moved { x: 5, .. }]
            ),
            "{events:?}"
        );
        assert!(heard(&mut tap, |t, tell| t.frames(true, tell)).is_empty());
        assert!(matches!(
            heard(&mut tap, |t, tell| t.frames(false, tell))[..],
            [CursorEvent::Left { .. }]
        ));
        assert!(heard(&mut tap, |t, tell| t.position(6, 6, tell)).is_empty());
    }

    /// The hotspot is in effect with the next picture, as the protocol says.
    #[test]
    fn the_hotspot_goes_with_the_next_picture() {
        let mut tap = tap();
        on(&mut tap, 10, 10, &argb([0, 0, 0, 255]));
        tap.hotspot(1, 0);
        let beam = argb([255, 255, 255, 255]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&beam, 2, 2), tell));
        assert!(
            matches!(&events[..], [CursorEvent::Shape(i)] if (i.hot_x, i.hot_y) == (1, 0)),
            "{events:?}"
        );
        // A new hotspot for the same picture: told with it.
        tap.hotspot(0, 1);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&beam, 2, 2), tell));
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
        tap.frames(true, |_| {});
        heard(&mut tap, |t, tell| t.enter(tell));
        tap.hotspot(2, 3);
        let events = heard(&mut tap, |t, tell| t.position(100, 50, tell));
        assert!(matches!(
            events[..],
            [CursorEvent::Moved { x: 100, y: 50, .. }]
        ));
        // A 4x4 picture at scale 2: 2x2 here; its top left pixel opaque only.
        let mut big = argb([0, 0, 0, 0]).repeat(4);
        big[..4].copy_from_slice(&[0, 0, 200, 255]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&big, 4, 4), tell));
        let [CursorEvent::Shape(image)] = &events[..] else {
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
        tap.frames(true, |_| {});
        on(&mut tap, 0, 0, &argb([0, 0, 0, 255]));
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
        let black = argb([0, 0, 0, 255]);
        assert!(heard(&mut tap, |t, tell| t.picture(shot(&black, 2, 2), tell)).is_empty());
        assert!(!tap.wants_fresh(), "not on it");
        heard(&mut tap, |t, tell| t.enter(tell));
        heard(&mut tap, |t, tell| t.position(3, 3, tell));
        assert!(tap.wants_fresh());
        assert!(!tap.wants_fresh(), "once");
        let arrow = argb([0, 0, 255, 255]);
        let events = heard(&mut tap, |t, tell| t.picture(shot(&arrow, 2, 2), tell));
        assert!(
            matches!(&events[..], [CursorEvent::Shape(i)] if i.rgba[..4] == [255, 0, 0, 255]),
            "{events:?}"
        );
        assert!(!tap.wants_fresh());
    }

    /// Only the output whose picture has the hotspot has the pointer, as on KDE: at
    /// the edge of two side by side, the pointer's picture touches both.
    #[test]
    fn only_the_output_with_the_hotspot_has_the_pointer() {
        let (mut left, mut right) = (tap(), tap());
        let arrow = argb([0, 0, 0, 255]);
        on(&mut left, 638, 10, &arrow);
        heard(&mut right, |t, tell| t.enter(tell));
        assert!(heard(&mut right, |t, tell| t.position(-2, 10, tell)).is_empty());
        assert!(heard(&mut right, |t, tell| t.picture(shot(&arrow, 2, 2), tell)).is_empty());
        assert!(matches!(
            heard(&mut left, |t, tell| t.position(641, 10, tell))[..],
            [CursorEvent::Left { .. }]
        ));
        let events = heard(&mut right, |t, tell| t.position(1, 10, tell));
        assert!(
            matches!(events[..], [CursorEvent::Moved { x: 1, .. }]),
            "{events:?}"
        );
        assert!(right.wants_fresh(), "its picture came while off it");
        let events = heard(&mut right, |t, tell| t.picture(shot(&arrow, 2, 2), tell));
        assert!(matches!(events[..], [CursorEvent::Shape(_)]), "{events:?}");
    }

    /// The frames as their capture pushed them, and the hub woken when that changes;
    /// with none going out, the next one as asked. Asking switches the capture at
    /// once (its gate opens).
    #[test]
    fn an_overlay_follows_the_frames_pushed() {
        let overlay = Overlay::default();
        let waker = Arc::new(Waker::new().unwrap());
        overlay.hub(Some(waker.clone()));
        let woken = || {
            let mut n = [0u8; 8];
            // SAFETY: reads at most 8 bytes from the test's eventfd (not blocking).
            unsafe { libc::read(waker.fd(), n.as_mut_ptr().cast(), 8) == 8 }
        };
        assert!(!overlay.frames_apart() && !overlay.asked());
        let pause = Arc::new(Pause::default());
        overlay.capture(Some(pause.clone()));
        overlay.ask(true);
        assert!(overlay.asked());
        assert!(pause.gate().take_open(), "the capture switches at once");
        assert!(
            overlay.frames_apart(),
            "no frame went out: the next one as asked"
        );
        overlay.pushed(false);
        assert!(woken());
        assert!(
            !overlay.frames_apart(),
            "a frame with the pointer went out last"
        );
        overlay.pushed(true);
        assert!(woken() && overlay.frames_apart());
        overlay.pushed(true);
        assert!(!woken(), "no change");
        overlay.ask(false);
        assert!(
            overlay.frames_apart(),
            "until a frame with the pointer goes out"
        );
        overlay.not_pushing();
        assert!(woken() && !overlay.frames_apart());
    }

    /// A session the compositor stops is opened again later and later, at most 5 s.
    #[test]
    fn stopped_sessions_back_off() {
        let s: Vec<u64> = (1..=6).map(|n| backoff(n).as_millis() as u64).collect();
        assert_eq!(s, [1000, 2000, 4000, 5000, 5000, 5000]);
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
        let arrow = argb([0, 0, 255, 255]);
        on(&mut tap, 3, 3, &arrow);
        let kept = |t: &CursorTap| (t.scratch.as_ptr(), t.look.as_ref().map(|l| l.rgba.as_ptr()));
        let before = kept(&tap);
        for x in 4..8 {
            assert!(heard(&mut tap, |t, tell| t.picture(shot(&arrow, 2, 2), tell)).is_empty());
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

    /// By hand, against the compositor of this session: what the pointer sessions of
    /// outputs `PROSTIR_POINTER_OUTPUTS` (comma-separated) tell for 10 s, in one hub,
    /// and whether their frames would leave the pointer out (`cargo test -p prostir --
    /// --ignored --nocapture pointer_session_live`). With `PROSTIR_POINTER_MOVE`, a
    /// virtual pointer moves across them (Sway with no input devices).
    #[test]
    #[ignore = "needs a wlroots compositor"]
    fn pointer_session_live() {
        let Ok(names) = std::env::var("PROSTIR_POINTER_OUTPUTS") else {
            return;
        };
        println!("pointer sessions offered: {}", offered());
        let mover = std::env::var_os("PROSTIR_POINTER_MOVE").map(|_| {
            let m = std::thread::spawn(move_a_virtual_pointer);
            std::thread::sleep(Duration::from_secs(1));
            m
        });
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
        let hub = PointerHub::start(sink, Box::new(|_| Units::PROTOCOL)).unwrap();
        let overlays: Vec<(String, Arc<Overlay>)> = names
            .split(',')
            .map(|n| (n.to_owned(), Arc::new(Overlay::default())))
            .collect();
        for (name, overlay) in &overlays {
            hub.watch(name, overlay.clone());
        }
        let mut was = vec![None; overlays.len()];
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            for ((name, overlay), was) in overlays.iter().zip(&mut was) {
                let now = overlay.asked();
                if *was != Some(now) {
                    println!("{name}: apart from the frames: {now}");
                    *was = Some(now);
                }
            }
        }
        drop(hub);
        if let Some(m) = mover {
            m.join().unwrap();
        }
    }

    struct Mover;

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Mover {
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

    use wayland_protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    };
    delegate_noop!(Mover: ignore wl_seat::WlSeat);
    delegate_noop!(Mover: ignore ZwlrVirtualPointerManagerV1);
    delegate_noop!(Mover: ignore ZwlrVirtualPointerV1);

    /// A virtual pointer (wlr-virtual-pointer) at 1700,100 of two 1920x1080 outputs
    /// side by side, then 20 px right every 100 ms, 40 times (over the edge).
    fn move_a_virtual_pointer() {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, mut queue) = registry_queue_init::<Mover>(&conn).unwrap();
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
        let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());
        pointer.motion_absolute(0, 1700, 100, 3840, 1080);
        pointer.frame();
        queue.roundtrip(&mut Mover).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        for i in 1..=40 {
            pointer.motion(i * 100, 20.0, 0.0);
            pointer.frame();
            queue.roundtrip(&mut Mover).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        pointer.destroy();
        queue.roundtrip(&mut Mover).unwrap();
    }
}
