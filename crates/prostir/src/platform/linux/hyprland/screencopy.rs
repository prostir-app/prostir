//! Captures an output with `wlr-screencopy` (Hyprland, Sway) and feeds the frames to
//! the encoder's `appsrc`, with the pointer drawn in or left out (`overlay_cursor`)
//! while `pointer.rs` tells it apart.
//!
//! Frames go into GPU buffers (dmabuf) allocated with a layout (modifier) the VA-API
//! converter can read directly, so nothing is copied through the CPU. Without such a
//! layout (or with `--software`), frames go through shared memory instead, copied
//! into buffers of a pool that come back once the encoder is done with them.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::encoder::{Input, Pause};
use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer_allocators::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use log::{debug, info, warn};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// The rate Prostir's outputs run at. Told to the encoder: with an unknown rate VA-API
/// budgets every frame as if there were 30 a second, which on a busy screen means about
/// twice the chosen bitrate. Frames still come only when something changes.
const FRAME_RATE: i32 = 60;

/// Buffers captured into in turn; the encoder holds at most a few at a time.
const SLOTS: usize = 4;
/// A capture that waited for its turn at the gate and whose frame was ready within
/// this had a change waiting: one frame held by the gate.
const HELD_READY: Duration = Duration::from_millis(25);
/// With nothing changing on screen, the last frame is sent again this often, as
/// PipeWire in KDE does: the session needs none of it (pings keep it), but it pushes
/// out the one keyframe a still screen gets, and each costs an encoding here and a
/// decoding in the headset.
const KEEPALIVE: Duration = Duration::from_secs(2);

/// The encoder input for output `name`: an `appsrc` the capture pushes frames into.
pub fn input(name: &str) -> Input {
    Input {
        // Caps are set by the capture once it knows the frame size and format.
        source: "appsrc name=src is-live=true format=time do-timestamp=true \
                 max-buffers=2 leaky-type=downstream"
            .to_owned(),
        label: name.to_owned(),
        frame_rate: None,
        live: false,
    }
}

/// A running capture of one output.
pub struct Capture {
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    /// Its encoder's pause, which the capture sleeps on while paused.
    paused: Arc<Pause>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Starts capturing `output` (its compositor name) into `appsrc`. While `paused`
    /// is set nothing is captured; while `apart` is, frames leave the pointer out (its
    /// session tells it, see `pointer.rs`), taken anew for each frame.
    pub fn start(
        output: String,
        appsrc: gst_app::AppSrc,
        paused: Arc<Pause>,
        dmabuf: bool,
        apart: Arc<AtomicBool>,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let woken = paused.clone();
        let thread = {
            let (stop, closed) = (stop.clone(), closed.clone());
            std::thread::Builder::new()
                .name(format!("capture-{output}"))
                .spawn(move || {
                    if let Err(e) = run(&output, &appsrc, &paused, &stop, dmabuf, &apart) {
                        warn!("capture of {output} stopped: {e:#}");
                    }
                    closed.store(true, Ordering::Relaxed);
                })?
        };
        Ok(Self {
            stop,
            closed,
            paused: woken,
            thread: Some(thread),
        })
    }

    /// The output is gone or cannot be captured any more.
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.paused.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the compositor offers for the frame being captured.
#[derive(Default)]
struct Frame {
    shm: Option<(wl_shm::Format, u32, u32, u32)>,
    dmabuf: Option<(u32, u32, u32)>,
    described: bool,
    y_invert: bool,
    ready: bool,
    failed: bool,
    /// The compositor listed the parts that changed (`copy_with_damage`), and how
    /// many pixels they cover: none listed, all of it counts as changed.
    damaged: bool,
    damage_area: u64,
}

#[derive(Default)]
struct State {
    /// Outputs by registry name: proxy and connector name.
    outputs: HashMap<u32, (wl_output::WlOutput, Option<String>)>,
    frame: Frame,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
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

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = state.outputs.get_mut(global)
        {
            entry.1 = Some(name);
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        let f = &mut state.frame;
        match event {
            Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => f.shm = Some((format, width, height, stride)),
            Event::LinuxDmabuf {
                format,
                width,
                height,
            } => f.dmabuf = Some((format, width, height)),
            Event::BufferDone => f.described = true,
            Event::Flags {
                flags: WEnum::Value(flags),
            } => f.y_invert = flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert),
            Event::Damage { width, height, .. } => {
                f.damaged = true;
                f.damage_area += u64::from(width) * u64::from(height);
            }
            Event::Ready { .. } => f.ready = true,
            Event::Failed => f.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_buffer_params_v1::Event::Failed = event {
            warn!("the compositor refused a capture buffer");
        }
    }
}

delegate_noop!(State: ignore ZwlrScreencopyManagerV1);
delegate_noop!(State: ignore ZwpLinuxDmabufV1);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);

/// Reads pending events, waiting at most `timeout` for new ones.
fn wait(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    timeout: Duration,
) -> Result<()> {
    queue.dispatch_pending(state)?;
    conn.flush()?;
    if let Some(guard) = queue.prepare_read() {
        let mut pfd = libc::pollfd {
            fd: guard.connection_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let n = unsafe { libc::poll(&raw mut pfd, 1, timeout.as_millis() as i32) };
        if n > 0 {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    queue.dispatch_pending(state)?;
    Ok(())
}

/// Binds every output and waits (up to 3 s, it may have just been created) for the
/// one called `name`.
fn find_output(
    conn: &Connection,
    globals: &GlobalList,
    queue: &mut EventQueue<State>,
    state: &mut State,
    name: &str,
) -> Result<wl_output::WlOutput> {
    let qh = queue.handle();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" && g.version >= 4 && !state.outputs.contains_key(&g.name)
            {
                let output = globals
                    .registry()
                    .bind::<wl_output::WlOutput, _, _>(g.name, 4, &qh, g.name);
                state.outputs.insert(g.name, (output, None));
            }
        }
        queue.roundtrip(state)?;
        if let Some((output, _)) = state
            .outputs
            .values()
            .find(|(_, n)| n.as_deref() == Some(name))
        {
            return Ok(output.clone());
        }
        if Instant::now() > deadline {
            bail!("output {name} did not appear");
        }
        wait(conn, queue, state, Duration::from_millis(100))?;
    }
}

/// `XR24` and the like, from a DRM fourcc code.
fn fourcc_name(code: u32) -> String {
    code.to_le_bytes().iter().map(|&b| b as char).collect()
}

/// DRM formats and modifiers `vapostproc` imports, e.g. `("AR24", 0x0100000000000009)`.
fn va_dmabuf_formats() -> Vec<(String, u64)> {
    let Some(factory) = gst::ElementFactory::find("vapostproc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for template in factory.static_pad_templates() {
        if template.direction() != gst::PadDirection::Sink {
            continue;
        }
        let caps = template.caps();
        for (s, features) in caps.iter_with_features() {
            if !features.contains("memory:DMABuf") {
                continue;
            }
            let Ok(value) = s.value("drm-format") else {
                continue;
            };
            let mut names = Vec::new();
            if let Ok(list) = value.get::<gst::List>() {
                names.extend(list.iter().filter_map(|v| v.get::<String>().ok()));
            } else if let Ok(one) = value.get::<String>() {
                names.push(one);
            }
            for n in names {
                let (fourcc, modifier) = match n.split_once(':') {
                    Some((f, m)) => (
                        f,
                        u64::from_str_radix(m.trim_start_matches("0x"), 16).unwrap_or(0),
                    ),
                    None => (n.as_str(), 0),
                };
                found.push((fourcc.to_owned(), modifier));
            }
        }
    }
    found
}

/// The modifiers the encoder can read for frames of `fourcc`, with the name to give
/// the format in the caps. A format without alpha is read as its alpha twin (same
/// bytes; the converter ignores alpha).
fn modifiers_for(fourcc: &str, available: &[(String, u64)]) -> Vec<(String, u64)> {
    let twin = fourcc.replacen('X', "A", 1);
    available
        .iter()
        .filter(|(f, _)| f == fourcc || *f == twin)
        .cloned()
        .collect()
}

fn render_node() -> Result<File> {
    let path = match std::env::var("PROSTIR_RENDER_NODE") {
        Ok(p) => p,
        Err(_) => {
            let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")?
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("renderD"))
                })
                .collect();
            nodes.sort();
            nodes
                .first()
                .context("no GPU render node in /dev/dri")?
                .to_string_lossy()
                .into_owned()
        }
    };
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {path}"))
}

struct DmaSlot {
    buffer: wl_buffer::WlBuffer,
    memory: gst::Memory,
    offset: usize,
    stride: i32,
    _bo: gbm::BufferObject<()>,
}

struct ShmSlot {
    buffer: wl_buffer::WlBuffer,
    offset: usize,
}

enum Slots {
    Dma(Vec<DmaSlot>),
    Shm {
        slots: Vec<ShmSlot>,
        map: *mut u8,
        len: usize,
        stride: usize,
        /// The buffers frames are copied into, and their rows: where they start and
        /// how many bytes of pixels each has.
        frames: gst::BufferPool,
        frame_stride: usize,
        row: usize,
        _pool: wl_shm_pool::WlShmPool,
        _file: File,
    },
}

/// The buffer descriptions a frame came with: dmabuf and shared memory.
type Offer = (
    Option<(u32, u32, u32)>,
    Option<(wl_shm::Format, u32, u32, u32)>,
);

/// The buffers of one frame size and format.
struct Pool {
    /// What the compositor offered when this pool was made.
    key: Offer,
    width: u32,
    height: u32,
    slots: Slots,
}

impl Drop for Pool {
    fn drop(&mut self) {
        match &self.slots {
            Slots::Dma(slots) => slots.iter().for_each(|s| s.buffer.destroy()),
            Slots::Shm {
                slots,
                map,
                len,
                frames,
                _pool: pool,
                ..
            } => {
                let _ = frames.set_active(false);
                for s in slots {
                    s.buffer.destroy();
                }
                pool.destroy();
                // SAFETY: `map` came from mmap with this length and is not used after.
                unsafe { libc::munmap(map.cast(), *len) };
            }
        }
    }
}

struct Wayland<'a> {
    qh: &'a QueueHandle<State>,
    shm: &'a wl_shm::WlShm,
    dmabuf: Option<&'a ZwpLinuxDmabufV1>,
}

impl Pool {
    fn dmabuf(
        wl: &Wayland<'_>,
        gpu: &gbm::Device<File>,
        va: &[(String, u64)],
        format: u32,
        width: u32,
        height: u32,
    ) -> Result<(Self, gst::Caps)> {
        let dmabuf = wl.dmabuf.context("no zwp_linux_dmabuf_v1")?;
        let fourcc = fourcc_name(format);
        let usable = modifiers_for(&fourcc, va);
        if usable.is_empty() {
            bail!("the encoder reads no layout of {fourcc}");
        }
        let gbm_format =
            gbm::Format::try_from(format).map_err(|_| anyhow!("unknown format {fourcc}"))?;
        let allocator = gstreamer_allocators::DmaBufAllocator::new();
        let mut slots = Vec::new();
        let mut caps_name = None;
        for _ in 0..SLOTS {
            let bo = gpu.create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                gbm_format,
                usable.iter().map(|(_, m)| gbm::Modifier::from(*m)),
                gbm::BufferObjectFlags::RENDERING,
            )?;
            if bo.plane_count() != 1 {
                bail!("{} planes", bo.plane_count());
            }
            let modifier = u64::from(bo.modifier());
            let (name, _) = usable
                .iter()
                .find(|(_, m)| *m == modifier)
                .context("GBM picked a layout the encoder cannot read")?;
            caps_name = Some(if modifier == 0 {
                name.clone()
            } else {
                format!("{name}:{modifier:#018x}")
            });
            let offset = bo.offset(0);
            let stride = bo.stride();
            let fd: OwnedFd = bo.fd()?;
            let params = dmabuf.create_params(wl.qh, ());
            params.add(
                fd.as_fd(),
                0,
                offset,
                stride,
                (modifier >> 32) as u32,
                modifier as u32,
            );
            let buffer = params.create_immed(
                width as i32,
                height as i32,
                format,
                zwp_linux_buffer_params_v1::Flags::empty(),
                wl.qh,
                (),
            );
            params.destroy();
            let mut file = File::from(bo.fd()?);
            let size = file.seek(SeekFrom::End(0))? as usize;
            // SAFETY: `file` is a dmabuf of `size` bytes that GStreamer now owns.
            let memory = unsafe { allocator.alloc_dmabuf(file, size)? };
            slots.push(DmaSlot {
                buffer,
                memory,
                offset: offset as usize,
                stride: stride as i32,
                _bo: bo,
            });
        }
        let caps = gst::Caps::builder("video/x-raw")
            .features(["memory:DMABuf"])
            .field("format", "DMA_DRM")
            .field("drm-format", caps_name.unwrap_or_default())
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", gst::Fraction::new(FRAME_RATE, 1))
            .build();
        Ok((
            Self {
                key: (None, None),
                width,
                height,
                slots: Slots::Dma(slots),
            },
            caps,
        ))
    }

    fn shm(
        wl: &Wayland<'_>,
        format: wl_shm::Format,
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<(Self, gst::Caps)> {
        let video_format = match format {
            wl_shm::Format::Xrgb8888 => gst_video::VideoFormat::Bgrx,
            wl_shm::Format::Argb8888 => gst_video::VideoFormat::Bgra,
            wl_shm::Format::Xbgr8888 => gst_video::VideoFormat::Rgbx,
            wl_shm::Format::Abgr8888 => gst_video::VideoFormat::Rgba,
            other => bail!("unsupported shared-memory format {other:?}"),
        };
        let frame = stride as usize * height as usize;
        let len = frame * SLOTS;
        // SAFETY: a fresh anonymous file; the name is a valid C string.
        let fd = unsafe { libc::memfd_create(c"prostir-capture".as_ptr(), libc::MFD_CLOEXEC) };
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
        let pool = wl.shm.create_pool(file.as_fd(), len as i32, wl.qh, ());
        let slots = (0..SLOTS)
            .map(|i| ShmSlot {
                buffer: pool.create_buffer(
                    (i * frame) as i32,
                    width as i32,
                    height as i32,
                    stride as i32,
                    format,
                    wl.qh,
                    (),
                ),
                offset: i * frame,
            })
            .collect();
        let info = gst_video::VideoInfo::builder(video_format, width, height)
            .fps(gst::Fraction::new(FRAME_RATE, 1))
            .build()?;
        let caps = info.to_caps()?;
        // Enough buffers for those the encoder holds; more are made only if it holds
        // more, and every one comes back when it is done with it.
        let frames = gst_video::VideoBufferPool::new().upcast::<gst::BufferPool>();
        let mut config = frames.config();
        config.set_params(Some(&caps), info.size() as u32, SLOTS as u32, 0);
        config.add_option(gst_video::BUFFER_POOL_OPTION_VIDEO_META);
        frames.set_config(config)?;
        frames.set_active(true)?;
        Ok((
            Self {
                key: (None, None),
                width,
                height,
                slots: Slots::Shm {
                    slots,
                    map: map.cast(),
                    len,
                    stride: stride as usize,
                    frames,
                    frame_stride: info.stride()[0] as usize,
                    row: width as usize * 4,
                    _pool: pool,
                    _file: file,
                },
            },
            caps,
        ))
    }

    fn buffer(&self, slot: usize) -> &wl_buffer::WlBuffer {
        match &self.slots {
            Slots::Dma(s) => &s[slot].buffer,
            Slots::Shm { slots, .. } => &slots[slot].buffer,
        }
    }

    /// A GStreamer buffer showing what `slot` holds.
    fn frame(&self, slot: usize, y_invert: bool) -> Result<gst::Buffer> {
        let buffer = match &self.slots {
            Slots::Dma(slots) => {
                let s = &slots[slot];
                let mut buffer = gst::Buffer::new();
                let b = buffer.get_mut().expect("new buffer");
                b.append_memory(s.memory.clone());
                gst_video::VideoMeta::add_full(
                    b,
                    gst_video::VideoFrameFlags::empty(),
                    gst_video::VideoFormat::DmaDrm,
                    self.width,
                    self.height,
                    &[s.offset],
                    &[s.stride],
                )?;
                buffer
            }
            Slots::Shm {
                slots,
                map,
                stride,
                frames,
                frame_stride,
                row,
                ..
            } => {
                let size = stride * self.height as usize;
                // SAFETY: the slot lies inside the mapping, and the compositor is done
                // writing it (the frame is ready).
                let src = unsafe { std::slice::from_raw_parts(map.add(slots[slot].offset), size) };
                let mut buffer = frames
                    .acquire_buffer(None)
                    .map_err(|e| anyhow!("no buffer for a frame: {e:?}"))?;
                {
                    let b = buffer
                        .get_mut()
                        .context("a buffer of the pool still in use")?;
                    let mut out = b.map_writable()?;
                    copy_rows(src, *stride, &mut out, *frame_stride, *row, y_invert);
                }
                buffer
            }
        };
        Ok(buffer)
    }
}

/// Copies a picture whose rows of `row` bytes lie `from_stride` apart in `from` into
/// `to`, with its rows `to_stride` apart, upside down with `flip`: at once when both
/// lay their rows out alike, else row by row.
fn copy_rows(
    from: &[u8],
    from_stride: usize,
    to: &mut [u8],
    to_stride: usize,
    row: usize,
    flip: bool,
) {
    if !flip && from_stride == to_stride && to.len() >= from.len() {
        to[..from.len()].copy_from_slice(from);
        return;
    }
    let rows = from.chunks_exact(from_stride);
    let into = to.chunks_mut(to_stride);
    if flip {
        for (to, from) in into.zip(rows.rev()) {
            to[..row].copy_from_slice(&from[..row]);
        }
    } else {
        for (to, from) in into.zip(rows) {
            to[..row].copy_from_slice(&from[..row]);
        }
    }
}

fn run(
    name: &str,
    appsrc: &gst_app::AppSrc,
    paused: &Pause,
    stop: &AtomicBool,
    want_dmabuf: bool,
    apart: &AtomicBool,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("no Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: ZwlrScreencopyManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .context("the compositor has no wlr-screencopy")?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ())?;
    let dmabuf: Option<ZwpLinuxDmabufV1> = globals.bind(&qh, 3..=4, ()).ok();
    let mut state = State::default();
    let output = find_output(&conn, &globals, &mut queue, &mut state, name)?;
    let wl = Wayland {
        qh: &qh,
        shm: &shm,
        dmabuf: dmabuf.as_ref(),
    };
    let va = if want_dmabuf {
        va_dmabuf_formats()
    } else {
        Vec::new()
    };
    let gpu = if va.is_empty() {
        None
    } else {
        match render_node().and_then(|f| Ok(gbm::Device::new(f)?)) {
            Ok(d) => Some(d),
            Err(e) => {
                warn!("no GPU buffers for capture, using shared memory: {e:#}");
                None
            }
        }
    };
    let mut dmabuf_broken = gpu.is_none();

    let mut pool: Option<Pool> = None;
    let mut next = 0;
    // The frame pushed last, pushed again on a still screen (the same buffer: nothing
    // is copied, and the encoder sees it is the picture before).
    let mut last: Option<gst::Buffer> = None;
    let mut last_push = Instant::now();
    // When a new picture went last (a repeat does not count for the gate).
    let mut last_new: Option<Instant> = None;
    let mut failures = 0;
    let gate = paused.gate();
    while !stop.load(Ordering::Relaxed) {
        if paused.is_on() {
            // Asleep until resumed or stopped (see `Capture`'s drop).
            paused.wait(stop);
            continue;
        }
        // The next frame is captured when the gate lets it go (see `encoder/gate.rs`):
        // then it is the newest picture. It is asked for a frame of the monitor before
        // its turn, as the compositor copies with its next frame. Asked again
        // meanwhile, the last one goes; opened (a keyframe asked for), it goes now.
        let mut waited = false;
        let mut opened = gate.take_open();
        while !opened {
            let lead = gate.period();
            let since = last_new.filter(|_| last.is_some());
            let Some(at) = gate.due(since, Instant::now() + lead) else {
                break;
            };
            if stop.load(Ordering::Relaxed) || paused.is_on() {
                break;
            }
            waited = true;
            paused.nap(at - lead, stop);
            opened = gate.take_open();
            if paused.take_again()
                && let Some(l) = &last
            {
                let _ = appsrc.push_buffer(l.clone());
                last_push = Instant::now();
            }
        }
        if stop.load(Ordering::Relaxed) || paused.is_on() {
            continue;
        }
        state.frame = Frame::default();
        let asked = Instant::now();
        // The pointer drawn in, or left out while its session tells it.
        let overlay = i32::from(!apart.load(Ordering::Relaxed));
        let frame = manager.capture_output(overlay, &output, &qh, ());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !(state.frame.described || state.frame.failed) && Instant::now() < deadline {
            wait(&conn, &mut queue, &mut state, Duration::from_millis(100))?;
            // Version 1 and 2 frames describe one buffer and send no `buffer_done`.
            if manager_version(&manager) < 3 && state.frame.shm.is_some() {
                state.frame.described = true;
            }
        }
        if !state.frame.described {
            frame.destroy();
            failures += 1;
            if failures >= 5 {
                bail!("the compositor stopped answering");
            }
            continue;
        }
        let key = (state.frame.dmabuf, state.frame.shm);
        if pool.as_ref().is_none_or(|p| p.key != key) {
            last = None;
            // Free the old buffers before making new ones.
            drop(pool.take());
            let made = match (&gpu, state.frame.dmabuf) {
                (Some(gpu), Some((format, w, h))) if !dmabuf_broken => {
                    Pool::dmabuf(&wl, gpu, &va, format, w, h)
                        .inspect_err(|e| {
                            warn!("{name}: GPU buffers unusable, using shared memory: {e:#}");
                        })
                        .ok()
                }
                _ => None,
            };
            let made = match made {
                Some(m) => m,
                None => {
                    dmabuf_broken = true;
                    let (format, w, h, stride) =
                        state.frame.shm.context("no shared-memory buffer offered")?;
                    Pool::shm(&wl, format, w, h, stride)?
                }
            };
            info!(
                "capturing {name} at {}x{} via {}",
                made.0.width,
                made.0.height,
                if matches!(made.0.slots, Slots::Dma(_)) {
                    "GPU buffers"
                } else {
                    "shared memory"
                }
            );
            debug!("{name}: caps {}", made.1);
            appsrc.set_caps(Some(&made.1));
            let mut made = made.0;
            made.key = key;
            pool = Some(made);
        }
        let p = pool.as_ref().expect("pool made above");
        let slot = next;
        next = (next + 1) % SLOTS;
        frame.copy_with_damage(p.buffer(slot));
        while !(state.frame.ready || state.frame.failed) && !stop.load(Ordering::Relaxed) {
            wait(&conn, &mut queue, &mut state, Duration::from_millis(100))?;
            // Again now when a keyframe was asked of the still screen.
            let again = paused.take_again();
            if (again || last_push.elapsed() >= KEEPALIVE)
                && !paused.is_on()
                && let Some(l) = &last
            {
                let _ = appsrc.push_buffer(l.clone());
                last_push = Instant::now();
            }
        }
        frame.destroy();
        // The compositor says nothing changed: the last picture stands for it, unless
        // a keyframe was asked for (the gate open).
        let unchanged =
            state.frame.damaged && state.frame.damage_area == 0 && last.is_some() && !opened;
        if state.frame.ready && unchanged {
            failures = 0;
            gate.unchanged();
        } else if state.frame.ready {
            failures = 0;
            if waited && asked.elapsed() < HELD_READY {
                gate.held();
            }
            let frame = p.frame(slot, state.frame.y_invert)?;
            let _ = appsrc.push_buffer(frame.clone());
            last = Some(frame);
            last_push = Instant::now();
            last_new = Some(last_push);
        } else if state.frame.failed {
            failures += 1;
            if failures >= 5 {
                bail!("{name} cannot be captured (gone?)");
            }
            if matches!(p.slots, Slots::Dma(_)) && failures >= 2 {
                warn!("{name}: the compositor rejects GPU buffers, using shared memory");
                dmabuf_broken = true;
                pool = None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

fn manager_version(m: &ZwlrScreencopyManagerV1) -> u32 {
    wayland_client::Proxy::version(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame is copied whole, or row by row where the rows are laid out apart or
    /// the picture is upside down.
    #[test]
    fn copies_rows() {
        // Two rows of 3 bytes, 4 apart.
        let from = [1, 2, 3, 0, 4, 5, 6, 0];
        let mut to = [9; 8];
        copy_rows(&from, 4, &mut to, 4, 3, false);
        assert_eq!(to, from);
        let mut to = [9; 6];
        copy_rows(&from, 4, &mut to, 3, 3, false);
        assert_eq!(to, [1, 2, 3, 4, 5, 6]);
        let mut to = [9; 8];
        copy_rows(&from, 4, &mut to, 4, 3, true);
        assert_eq!(to, [4, 5, 6, 9, 1, 2, 3, 9]);
    }

    #[test]
    fn names_fourcc_codes() {
        assert_eq!(fourcc_name(0x3432_5258), "XR24");
    }

    #[test]
    fn reads_alpha_twin_layouts() {
        let va = vec![
            ("AR24".to_owned(), 0x0100_0000_0000_0009),
            ("NV12".to_owned(), 0),
            ("XB24".to_owned(), 0x0100_0000_0000_0009),
        ];
        assert_eq!(
            modifiers_for("XR24", &va),
            vec![("AR24".to_owned(), 0x0100_0000_0000_0009)]
        );
        assert_eq!(modifiers_for("XB24", &va).len(), 1);
        assert!(modifiers_for("AR30", &va).is_empty());
    }
}
