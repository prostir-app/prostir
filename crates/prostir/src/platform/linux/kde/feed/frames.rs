//! KWin's buffers as GStreamer buffers, with nothing copied, and the last picture
//! sent again while the desktop is still.
//!
//! As in `pipewiresrc`: each of the stream's buffers gets GStreamer memory once, when
//! PipeWire adds it, over a duplicate of its descriptor (GPU buffers through the
//! DMA-BUF allocator, which VA-API imports; shared memory through the descriptor
//! allocator, which the CPU encoders map). Each frame then shares that memory for the
//! part the frame fills, with the layout it says (`VideoMeta`), and holds the buffer
//! until GStreamer lets the frame go; only then does KWin get it back. The memory
//! outlives the buffer (its own descriptor), so a frame GStreamer still holds when
//! PipeWire removes the buffers stays readable.
//!
//! A frame takes no memory of Rust's: the buffer's holder (a GStreamer buffer the
//! frame keeps as its parent, as `pipewiresrc` keeps its own) is made once with the
//! buffer and comes back to life each time a frame lets it go, which it says through
//! a flag and the feed's wake-up file.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::glib::translate::IntoGlib;
use gstreamer_allocators::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use log::warn;
use pipewire::spa::sys as ss;
use pipewire::sys as pw;

use crate::encoder::Gate as FrameGate;

/// Whether a frame of a buffer came back, shared by the buffer's holder (on whatever
/// thread GStreamer lets the frame go) and the feed.
struct Back {
    gate: Mutex<Gate>,
    /// The feed's wake-up file.
    wake: Arc<OwnedFd>,
}

struct Gate {
    /// The buffer is still the stream's: its holder comes back to life when let go.
    alive: bool,
    /// Its holder came back to life; the feed takes it again.
    returned: bool,
}

/// The key under which a holder keeps its [`Back`].
fn back_key() -> glib::Quark {
    static KEY: OnceLock<glib::Quark> = OnceLock::new();
    *KEY.get_or_init(|| glib::Quark::from_str("prostir-feed-back"))
}

/// Frees a holder's [`Back`] with the holder.
unsafe extern "C" fn free_back(data: glib::ffi::gpointer) {
    // SAFETY: the box `Slot::new` gave the holder, freed once, with it.
    drop(unsafe { Box::from_raw(data.cast::<Arc<Back>>()) });
}

/// A holder's last reference went: while its buffer is the stream's, it comes back
/// to life (as `pipewiresrc`'s pool buffers do) and the feed is woken to give the
/// buffer back to PipeWire; else it is freed. Takes no memory.
unsafe extern "C" fn holder_let_go(object: *mut gst::ffi::GstMiniObject) -> glib::ffi::gboolean {
    // SAFETY: GStreamer calls it with the holder, whose key holds its `Back`.
    let back = unsafe { gst::ffi::gst_mini_object_get_qdata(object, back_key().into_glib()) }
        .cast::<Arc<Back>>();
    if back.is_null() {
        return glib::ffi::GTRUE;
    }
    // SAFETY: freed only with the holder, which is alive in this call.
    let back = unsafe { &*back };
    let mut gate = back.gate.lock().unwrap_or_else(PoisonError::into_inner);
    if !gate.alive {
        return glib::ffi::GTRUE;
    }
    gate.returned = true;
    // SAFETY: back to life with one reference, which the feed takes.
    unsafe { gst::ffi::gst_mini_object_ref(object) };
    drop(gate);
    wake(&back.wake);
    glib::ffi::GFALSE
}

/// Wakes the feed (its eventfd).
pub fn wake(fd: &OwnedFd) {
    let one = 1u64;
    // SAFETY: an eventfd takes 8 bytes, which live through the call.
    unsafe { libc::write(fd.as_raw_fd(), (&raw const one).cast(), 8) };
}

/// The GStreamer memory of one of the stream's buffers.
struct Slot {
    /// One for each of its planes; none for memory of the stream's own process
    /// (copied each frame instead).
    memories: Vec<Option<gst::Memory>>,
    /// The holder a frame keeps as its parent; out while a frame of it is.
    holder: Option<gst::Buffer>,
    /// The holder's address, to take it again when it came back.
    raw: *mut gst::ffi::GstBuffer,
    back: Arc<Back>,
    /// A frame of it is in GStreamer's hands (until its holder is taken again).
    lent: bool,
}

impl Slot {
    fn new(memories: Vec<Option<gst::Memory>>, wake: Arc<OwnedFd>) -> Self {
        let back = Arc::new(Back {
            gate: Mutex::new(Gate {
                alive: true,
                returned: false,
            }),
            wake,
        });
        let holder = gst::Buffer::new();
        let raw = holder.as_ptr().cast_mut();
        let object = raw.cast::<gst::ffi::GstMiniObject>();
        // SAFETY: a new buffer, ours alone: its `Back` goes with it, and it comes back
        // to life when let go (see `holder_let_go`).
        unsafe {
            gst::ffi::gst_mini_object_set_qdata(
                object,
                back_key().into_glib(),
                Box::into_raw(Box::new(back.clone())).cast(),
                Some(free_back),
            );
            (*object).dispose = Some(holder_let_go);
        }
        Self {
            memories,
            holder: Some(holder),
            raw,
            back,
            lent: false,
        }
    }

    /// Takes the holder again if it came back; whether it did.
    fn take_back(&mut self) -> bool {
        let came = std::mem::take(
            &mut self
                .back
                .gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .returned,
        );
        if came {
            // SAFETY: the one reference it came back to life with, the feed's now.
            self.holder = Some(unsafe { gst::Buffer::from_glib_full(self.raw) });
            self.lent = false;
        }
        came
    }
}

impl Drop for Slot {
    /// The buffer is no longer the stream's: its holder is freed, now or when the
    /// frame that keeps it lets it go.
    fn drop(&mut self) {
        let came = {
            let mut gate = self
                .back
                .gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            gate.alive = false;
            std::mem::take(&mut gate.returned)
        };
        if came {
            // SAFETY: as in `take_back`; dropped at once, it is freed.
            drop(unsafe { gst::Buffer::from_glib_full(self.raw) });
        }
    }
}

/// The stream's buffers, by address.
pub struct Slots {
    slots: HashMap<usize, Slot>,
    wake: Arc<OwnedFd>,
    /// A frame was dropped already (said once).
    dropped: bool,
}

/// The planes of a buffer.
///
/// # Safety
/// `buffer` is a valid `pw_buffer` with a valid `spa_buffer`.
unsafe fn datas<'a>(buffer: *mut pw::pw_buffer) -> &'a [ss::spa_data] {
    // SAFETY: valid, the caller says.
    let b = unsafe { &*(*buffer).buffer };
    if b.datas.is_null() {
        return &[];
    }
    // SAFETY: `datas` points to `n_datas` of them.
    unsafe { std::slice::from_raw_parts(b.datas, b.n_datas as usize) }
}

/// GStreamer memory over a duplicate of a buffer's descriptor: GPU buffers (mapped once
/// where PipeWire says they can be, as `pipewiresrc` does) and shared memory, kept
/// mapped (mapped and unmapped for each frame, a 1080p frame took 2 ms more from the
/// node to the encoder).
fn wrap(d: &ss::spa_data) -> Option<gst::Memory> {
    let fd = i32::try_from(d.fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: PipeWire keeps the descriptor open while the buffer exists, through
    // this call.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .ok()?
        .into_raw_fd();
    let size = d.mapoffset as usize + d.maxsize as usize;
    let mapped =
        d.flags & ss::SPA_DATA_FLAG_MAPPABLE != 0 && d.flags & ss::SPA_DATA_FLAG_READABLE != 0;
    // SAFETY: `fd` is a descriptor of `size` bytes or more, the memory's own from here.
    let memory = unsafe {
        match d.type_ {
            ss::SPA_DATA_DmaBuf => gstreamer_allocators::DmaBufAllocator::new()
                .alloc_dmabuf_with_flags(
                    fd,
                    size,
                    if mapped {
                        gstreamer_allocators::FdMemoryFlags::KEEP_MAPPED
                    } else {
                        gstreamer_allocators::FdMemoryFlags::NONE
                    },
                ),
            _ => gstreamer_allocators::FdAllocator::new().alloc_fd(
                fd,
                size,
                gstreamer_allocators::FdMemoryFlags::KEEP_MAPPED,
            ),
        }
    };
    let mut memory = match memory {
        Ok(m) => m,
        Err(e) => {
            warn!("a buffer of the stream cannot be read: {e}");
            return None;
        }
    };
    memory
        .get_mut()
        .expect("new memory")
        .resize(d.mapoffset as usize..size);
    if d.type_ == ss::SPA_DATA_DmaBuf && mapped && memory.map_readable().is_err() {
        warn!("a buffer of the stream cannot be mapped");
    }
    Some(memory)
}

/// Whether a buffer carries a picture. When only the pointer moved, KWin sends a
/// buffer with the pointer's metadata alone: KWin 6.8 marks it corrupted
/// (`SPA_CHUNK_FLAG_CORRUPTED`) and leaves its size at the whole buffer's. Such a
/// buffer, one the header (`header`, `SPA_META_Header`) says is corrupted, and one
/// with every plane empty do not go to the encoder.
///
/// # Safety
/// As [`datas`].
pub unsafe fn has_picture(buffer: *mut pw::pw_buffer, header: Option<&[u8]>) -> bool {
    let corrupted = header
        .filter(|h| h.len() >= size_of::<ss::spa_meta_header>())
        // SAFETY: the metadata holds a header (checked), plain numbers.
        .map(|h| unsafe { h.as_ptr().cast::<ss::spa_meta_header>().read_unaligned() })
        .is_some_and(|h| h.flags & ss::SPA_META_HEADER_FLAG_CORRUPTED != 0);
    if corrupted {
        return false;
    }
    let mut any = false;
    // SAFETY: as the caller says.
    for d in unsafe { datas(buffer) } {
        if d.chunk.is_null() {
            return false;
        }
        // SAFETY: a plane's chunk is valid with it.
        let chunk = unsafe { &*d.chunk };
        if chunk.flags as u32 & ss::SPA_CHUNK_FLAG_CORRUPTED != 0 {
            return false;
        }
        any |= chunk.size > 0;
    }
    any
}

impl Slots {
    /// The stream's buffers, which wake the feed through `wake` when a frame of one is
    /// let go.
    pub fn new(wake: Arc<OwnedFd>) -> Self {
        Self {
            slots: HashMap::new(),
            wake,
            dropped: false,
        }
    }

    /// PipeWire added `buffer` to the stream.
    ///
    /// # Safety
    /// As [`datas`].
    pub unsafe fn add(&mut self, buffer: *mut pw::pw_buffer) {
        // SAFETY: as the caller says.
        let memories = unsafe { datas(buffer) }
            .iter()
            .map(|d| match d.type_ {
                ss::SPA_DATA_DmaBuf | ss::SPA_DATA_MemFd => wrap(d),
                _ => None,
            })
            .collect();
        self.slots
            .insert(buffer as usize, Slot::new(memories, self.wake.clone()));
    }

    /// PipeWire removes `buffer`: whether a frame of it is still out (it is then given
    /// back now, as `pipewiresrc` does; the frame keeps its memory).
    pub fn remove(&mut self, buffer: *mut pw::pw_buffer) -> bool {
        self.slots
            .remove(&(buffer as usize))
            .is_some_and(|s| s.lent)
    }

    /// The buffers whose frames GStreamer let go since, each given to `back` (to go
    /// back to PipeWire). Takes no memory.
    pub fn take_back(&mut self, mut back: impl FnMut(*mut pw::pw_buffer)) {
        for (at, slot) in &mut self.slots {
            if slot.take_back() {
                back(*at as *mut pw::pw_buffer);
            }
        }
    }

    /// Says once that a frame could not be made of a buffer.
    fn drop_frame(&mut self, why: &str) -> Option<gst::Buffer> {
        if !std::mem::replace(&mut self.dropped, true) {
            warn!("a frame of the stream was dropped: {why} (said once)");
        }
        None
    }

    /// The picture of `buffer` as a frame in the layout `info`, which gives the buffer
    /// back (see [`Slots::take_back`]) once GStreamer lets it go; none when it has no
    /// picture (the buffer then stays the caller's). Takes no memory of Rust's.
    ///
    /// # Safety
    /// `buffer` is a valid `pw_buffer` of this stream, dequeued, and stays valid until
    /// it is given back.
    pub unsafe fn frame(
        &mut self,
        buffer: *mut pw::pw_buffer,
        info: &gst_video::VideoInfo,
        crop: Option<Crop>,
    ) -> Option<gst::Buffer> {
        // SAFETY: as the caller says.
        let datas = unsafe { datas(buffer) };
        let Some(slot) = self.slots.get_mut(&(buffer as usize)) else {
            return self.drop_frame("a buffer PipeWire never added");
        };
        if slot.holder.is_none() {
            return self.drop_frame("a buffer still in the encoder's hands");
        }
        let mut frame = gst::Buffer::new();
        let f = frame.get_mut().expect("new buffer");
        let planes = (info.n_planes() as usize).min(4);
        let (mut offsets, mut strides) = ([0usize; 4], [0i32; 4]);
        offsets[..planes].copy_from_slice(&info.offset()[..planes]);
        strides[..planes].copy_from_slice(&info.stride()[..planes]);
        let mut size = 0;
        for (i, d) in datas.iter().enumerate() {
            if d.chunk.is_null() {
                return self.drop_frame("a plane with no chunk");
            }
            // SAFETY: a plane's chunk is valid with it.
            let chunk = unsafe { &*d.chunk };
            if i < planes {
                offsets[i] = size;
                strides[i] = chunk.stride;
            }
            size += chunk.size as usize;
        }
        for (i, d) in datas.iter().enumerate() {
            // SAFETY: as above.
            let chunk = unsafe { &*d.chunk };
            let (start, len) = (chunk.offset as usize, chunk.size as usize);
            if len == 0 {
                continue;
            }
            let memory = match slot.memories.get(i).and_then(Option::as_ref) {
                Some(m) if start + len <= m.size() => m.share(start..start + len),
                Some(_) => return self.drop_frame("a chunk past the end of its memory"),
                // Memory of the stream's own process: copied (KWin sends none).
                None if d.type_ == ss::SPA_DATA_MemPtr
                    && !d.data.is_null()
                    && start + len <= d.maxsize as usize =>
                {
                    // SAFETY: the plane's memory is `maxsize` bytes, the chunk
                    // within it.
                    let bytes =
                        unsafe { std::slice::from_raw_parts(d.data.cast::<u8>().add(start), len) };
                    gst::Memory::from_slice(bytes.to_vec())
                }
                None => return self.drop_frame("a plane of memory that cannot be read"),
            };
            f.append_memory(memory);
        }
        if f.size() == 0 {
            return None;
        }
        if gst_video::VideoMeta::add_full(
            f,
            gst_video::VideoFrameFlags::empty(),
            info.format(),
            info.width(),
            info.height(),
            &offsets[..planes],
            &strides[..planes],
        )
        .is_err()
        {
            return self.drop_frame("its layout");
        }
        if let Some(rect) = crop {
            gst_video::VideoCropMeta::add(f, rect);
        }
        let holder = slot.holder.take().expect("checked above");
        gst::ParentBufferMeta::add(f, &holder);
        // The frame's parent meta holds it from here.
        drop(holder);
        slot.lent = true;
        Some(frame)
    }
}

/// A crop of the picture: x, y, width, height.
pub type Crop = (u32, u32, u32, u32);

/// The crop a `spa_meta_region` of `SPA_META_VideoCrop` says; none when it is empty
/// (a producer that does not fill it in: VA-API's converter fails on a crop to
/// nothing).
pub fn read_crop(meta: &[u8]) -> Option<Crop> {
    if meta.len() < size_of::<ss::spa_meta_region>() {
        return None;
    }
    // SAFETY: the metadata holds a region (checked), plain numbers.
    let r: ss::spa_meta_region =
        unsafe { meta.as_ptr().cast::<ss::spa_meta_region>().read_unaligned() };
    if r.region.size.width == 0 || r.region.size.height == 0 {
        return None;
    }
    Some((
        r.region.position.x.max(0) as u32,
        r.region.position.y.max(0) as u32,
        r.region.size.width,
        r.region.size.height,
    ))
}

/// The pictures pushed into the encoder: the last one, sent again every `every` while
/// nothing new comes (`pipewiresrc`'s `keepalive-time`), and at once when asked. A new
/// one goes when its encoder's gate lets it (see `encoder/gate.rs`): until then it
/// waits as the last one, and a newer one takes its place.
pub struct Pictures {
    appsrc: gst_app::AppSrc,
    every: Duration,
    gate: Arc<FrameGate>,
    last: Option<gst::Buffer>,
    /// When the last one goes again; none while paused.
    due: Option<Instant>,
    /// When a picture last went to the encoder.
    sent: Option<Instant>,
    /// The last one waits for its turn at the gate, until then.
    held: Option<Instant>,
}

impl Pictures {
    pub fn new(appsrc: gst_app::AppSrc, every: Duration, gate: Arc<FrameGate>) -> Self {
        Self {
            appsrc,
            every,
            gate,
            last: None,
            due: None,
            sent: None,
            held: None,
        }
    }

    /// A new picture, at `now`: it goes now, or waits for its turn at the gate.
    pub fn push(&mut self, frame: gst::Buffer, now: Instant) {
        self.last = Some(frame);
        match self.gate.turn(self.sent, now) {
            None => self.send(now),
            Some(at) => {
                self.gate.held();
                self.held = Some(at);
            }
        }
    }

    /// Whether a picture is there to send again (a frame of nothing new is not
    /// encoded only then: the first one of a stream always is).
    pub fn has_last(&self) -> bool {
        self.last.is_some()
    }

    /// The last picture to the encoder now.
    fn send(&mut self, now: Instant) {
        if let Some(last) = self.last.clone() {
            let _ = self.appsrc.push_buffer(last);
        }
        self.sent = Some(now);
        self.held = None;
        self.due = Some(now + self.every);
    }

    /// The last picture again, the same buffer (the encoder tells it is a repeat by
    /// its memory), whatever the gate says (it was asked for); whether there was one.
    pub fn again(&mut self, now: Instant) -> bool {
        if self.last.is_none() {
            return false;
        }
        self.send(now);
        true
    }

    /// Sends the picture that waited at the gate once its turn came (or the gate
    /// opened), and the last picture again when its time came.
    pub fn tick(&mut self, now: Instant) {
        if self.held.is_some() {
            if self.gate.turn(self.sent, now).is_none() {
                self.send(now);
            }
            return;
        }
        if self.due.is_some_and(|due| now >= due) {
            self.again(now);
        }
    }

    /// How long until a picture goes: the one that waits at the gate, or the last one
    /// again; none: not before something happens. A gate that opened: now.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if self.held.is_some() && self.gate.is_open() {
            return Some(Duration::ZERO);
        }
        self.held
            .or(self.due)
            .map(|due| due.saturating_duration_since(now))
    }

    /// Nothing is sent again until a new picture comes or one is asked for (paused).
    pub fn rest(&mut self) {
        self.due = None;
        self.held = None;
    }

    /// The last picture is let go: the stream starts over.
    pub fn forget(&mut self) {
        self.last = None;
        self.due = None;
        self.sent = None;
        self.held = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// An `appsrc` as the encoder's, and the memory of each frame that came out of it.
    fn pipeline() -> (gst::Pipeline, gst_app::AppSrc, Arc<Mutex<Vec<usize>>>) {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "appsrc name=src is-live=true format=time do-timestamp=true \
             caps=video/x-raw,format=GRAY8,width=4,height=1,framerate=0/1 \
             ! appsink name=sink sync=false",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let appsrc = pipeline
            .by_name("src")
            .unwrap()
            .downcast::<gst_app::AppSrc>()
            .unwrap();
        let sink = pipeline
            .by_name("sink")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let memory = sample.buffer().unwrap().peek_memory(0).as_ptr() as usize;
                    kept.lock().unwrap().push(memory);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing).unwrap();
        (pipeline, appsrc, seen)
    }

    fn came(seen: &Mutex<Vec<usize>>, n: usize) -> Vec<usize> {
        let until = Instant::now() + Duration::from_secs(10);
        while seen.lock().unwrap().len() < n && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        seen.lock().unwrap().clone()
    }

    /// The last picture goes again `every` after the one before, the same memory (a
    /// repeat to the encoder), at once when asked, and not while resting.
    #[test]
    fn the_last_picture_goes_again_while_still() {
        let (pipeline, appsrc, seen) = pipeline();
        let every = Duration::from_millis(2000);
        let mut pictures = Pictures::new(appsrc, every, Arc::default());
        let t0 = Instant::now();
        assert_eq!(pictures.wait(t0), None, "nothing to send again");
        assert!(!pictures.again(t0));
        pictures.push(gst::Buffer::from_slice([1u8, 2, 3, 4]), t0);
        assert_eq!(pictures.wait(t0), Some(every));
        pictures.tick(t0 + Duration::from_millis(1999));
        assert_eq!(came(&seen, 1).len(), 1, "not yet");
        pictures.tick(t0 + every);
        let frames = came(&seen, 2);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], frames[1], "the same memory: a repeat");
        assert_eq!(pictures.wait(t0 + every), Some(every), "from the repeat on");

        // Asked for at once.
        assert!(pictures.again(t0 + every + Duration::from_millis(10)));
        assert_eq!(came(&seen, 3).len(), 3);

        // A new picture: new memory, and the time counts from it.
        let t1 = t0 + Duration::from_secs(3);
        pictures.push(gst::Buffer::from_slice([5u8, 6, 7, 8]), t1);
        let frames = came(&seen, 4);
        assert_ne!(frames[3], frames[2]);
        assert_eq!(
            pictures.wait(t1 + Duration::from_millis(500)),
            Some(Duration::from_millis(1500))
        );

        // Paused: nothing until a picture comes or is asked for.
        pictures.rest();
        assert_eq!(pictures.wait(t1), None);
        pictures.tick(t1 + Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(seen.lock().unwrap().len(), 4);
        // Started over: the last one is gone.
        pictures.forget();
        assert!(!pictures.again(t1));
        pipeline.set_state(gst::State::Null).unwrap();
    }

    /// Slowed by the gate, a picture that came too soon waits, a newer one takes its
    /// place, and the newest goes at its turn; a keyframe asked for (the gate opened)
    /// lets it go at once, and the last picture asked again goes whatever the gate.
    #[test]
    fn the_newest_picture_waits_for_its_turn() {
        let (pipeline, appsrc, seen) = pipeline();
        let gate = Arc::new(FrameGate::default());
        gate.set_level(crate::encoder::Level::Side);
        let spacing = gate.spacing();
        let mut pictures = Pictures::new(appsrc, Duration::from_secs(2), gate.clone());
        let t0 = Instant::now();
        let picture = |n: u8| gst::Buffer::from_slice([n; 4]);
        pictures.push(picture(1), t0);
        assert_eq!(came(&seen, 1).len(), 1, "the first goes at once");
        let ms = Duration::from_millis(1);
        pictures.push(picture(2), t0 + ms);
        pictures.push(picture(3), t0 + 2 * ms);
        assert_eq!(
            pictures.wait(t0 + 2 * ms),
            Some(spacing - 2 * ms),
            "its turn"
        );
        pictures.tick(t0 + 3 * ms);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(seen.lock().unwrap().len(), 1, "not yet");
        pictures.tick(t0 + spacing);
        let frames = came(&seen, 2);
        assert_eq!(frames.len(), 2, "the newest went at its turn");
        assert_eq!(gate.take_counts().held, 2);
        assert_eq!(pictures.wait(t0 + spacing), Some(Duration::from_secs(2)));

        // A keyframe asked for: the one waiting goes at once.
        let t1 = t0 + spacing + ms;
        pictures.push(picture(4), t1);
        gate.open();
        assert_eq!(pictures.wait(t1), Some(Duration::ZERO));
        pictures.tick(t1);
        assert_eq!(came(&seen, 3).len(), 3);
        // Asked again: at once, whatever the gate.
        assert!(pictures.again(t1 + ms));
        assert_eq!(came(&seen, 4).len(), 4);
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn a_crop_region() {
        // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
        let mut r: ss::spa_meta_region = unsafe { std::mem::zeroed() };
        r.region.position.x = 2;
        r.region.size.width = 100;
        r.region.size.height = 50;
        // SAFETY: a plain struct of numbers, read as its bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&r).cast::<u8>(),
                size_of::<ss::spa_meta_region>(),
            )
        };
        assert_eq!(read_crop(bytes), Some((2, 0, 100, 50)));
        assert_eq!(read_crop(&bytes[..4]), None);
        // Not filled in: no crop.
        assert_eq!(read_crop(&[0; size_of::<ss::spa_meta_region>()]), None);
    }

    /// A buffer of shared memory as PipeWire hands it over, its parts boxed so they
    /// stay where they are.
    struct Given {
        _file: std::fs::File,
        chunk: Box<ss::spa_chunk>,
        _data: Box<ss::spa_data>,
        spa: Box<ss::spa_buffer>,
        pwb: Box<pw::pw_buffer>,
    }

    impl Given {
        /// One plane of `stride * rows` bytes, each 7.
        fn new(stride: usize, rows: usize) -> Self {
            use std::os::fd::AsRawFd;
            let size = stride * rows;
            let file = tempfile_with(&vec![7u8; size]);
            let mut chunk = Box::new(ss::spa_chunk {
                offset: 0,
                size: size as u32,
                stride: stride as i32,
                flags: 0,
            });
            // SAFETY: one of SPA's structs of numbers and pointers, for which all zero
            // is a value.
            let mut data: Box<ss::spa_data> = Box::new(unsafe { std::mem::zeroed() });
            data.type_ = ss::SPA_DATA_MemFd;
            data.flags = ss::SPA_DATA_FLAG_READABLE | ss::SPA_DATA_FLAG_MAPPABLE;
            data.fd = i64::from(file.as_raw_fd());
            data.maxsize = size as u32;
            data.chunk = &raw mut *chunk;
            // SAFETY: as above.
            let mut spa: Box<ss::spa_buffer> = Box::new(unsafe { std::mem::zeroed() });
            spa.n_datas = 1;
            spa.datas = &raw mut *data;
            // SAFETY: as above.
            let mut pwb: Box<pw::pw_buffer> = Box::new(unsafe { std::mem::zeroed() });
            pwb.buffer = &raw mut *spa;
            Self {
                _file: file,
                chunk,
                _data: data,
                spa,
                pwb,
            }
        }

        fn ptr(&mut self) -> *mut pw::pw_buffer {
            &raw mut *self.pwb
        }
    }

    /// The feed's wake-up file.
    fn wake_file() -> Arc<OwnedFd> {
        use std::os::fd::FromRawFd;
        // SAFETY: a new eventfd; checked below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(fd >= 0);
        // SAFETY: a descriptor just made, owned by nobody else.
        Arc::new(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Whether the wake-up file was written to (and clears it).
    fn woken(fd: &OwnedFd) -> bool {
        let mut n = 0u64;
        // SAFETY: 8 bytes to fill, alive through the call.
        unsafe { libc::read(fd.as_raw_fd(), (&raw mut n).cast(), 8) == 8 }
    }

    /// A buffer of shared memory: a frame shares its memory with the part the chunk
    /// says, in the layout it says, and the buffer goes back only once GStreamer let
    /// go of the frame, once, waking the feed; again and again.
    #[test]
    fn a_shared_memory_buffer_as_a_frame() {
        gst::init().unwrap();
        let (w, h, stride) = (4u32, 2u32, 16usize);
        let mut given = Given::new(stride, h as usize);
        let b = given.ptr();
        let wake = wake_file();
        let mut slots = Slots::new(wake.clone());
        // SAFETY: the buffer and its parts live through the test.
        unsafe { slots.add(b) };
        // SAFETY: as above.
        assert!(unsafe { has_picture(b, None) });
        let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgrx, w, h)
            .build()
            .unwrap();
        let back = |slots: &mut Slots| {
            let mut given = Vec::new();
            slots.take_back(|b| given.push(b));
            given
        };
        for _ in 0..3 {
            // SAFETY: as above.
            let frame = unsafe { slots.frame(b, &info, None) }.unwrap();
            assert_eq!(frame.size(), stride * h as usize);
            let meta = frame.meta::<gst_video::VideoMeta>().unwrap();
            assert_eq!(
                (meta.width(), meta.height(), meta.stride()[0]),
                (w, h, stride as i32)
            );
            assert_eq!(
                frame.map_readable().unwrap()[0],
                7,
                "the file's bytes, not a copy of them"
            );
            assert!(
                // SAFETY: as above.
                unsafe { slots.frame(b, &info, None) }.is_none(),
                "lent: no second frame of it"
            );
            let again = frame.clone();
            drop(frame);
            assert!(back(&mut slots).is_empty(), "still held");
            assert!(!woken(&wake));
            drop(again);
            assert!(woken(&wake), "the feed is woken");
            assert_eq!(back(&mut slots), [b]);
            assert!(back(&mut slots).is_empty(), "once");
        }

        // A frame out when PipeWire removes the buffer: given back then; the frame
        // still reads its memory, and letting it go later wakes nobody.
        // SAFETY: as above.
        let frame = unsafe { slots.frame(b, &info, None) }.unwrap();
        assert!(slots.remove(b), "lent");
        assert_eq!(frame.map_readable().unwrap()[0], 7);
        drop(frame);
        assert!(!woken(&wake));
        assert!(back(&mut slots).is_empty());
    }

    /// A buffer with the pointer alone carries no picture: KWin 6.8 marks it
    /// corrupted and leaves its size; one the header says is corrupted neither, nor
    /// one with every plane empty.
    #[test]
    fn a_buffer_with_the_pointer_alone_has_no_picture() {
        let mut given = Given::new(16, 2);
        let b = given.ptr();
        // SAFETY: the buffer and its parts live through the test.
        assert!(unsafe { has_picture(b, None) });
        given.chunk.flags = ss::SPA_CHUNK_FLAG_CORRUPTED as i32;
        // SAFETY: as above.
        assert!(!unsafe { has_picture(b, None) }, "corrupted, its size kept");
        given.chunk.flags = 0;
        // SAFETY: one of SPA's structs of numbers, for which all zero is a value.
        let mut header: ss::spa_meta_header = unsafe { std::mem::zeroed() };
        header.flags = ss::SPA_META_HEADER_FLAG_CORRUPTED;
        // SAFETY: a plain struct of numbers, read as its bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&header).cast::<u8>(),
                size_of::<ss::spa_meta_header>(),
            )
        };
        assert!(
            // SAFETY: as above.
            !unsafe { has_picture(b, Some(bytes)) },
            "the header says so"
        );
        given.chunk.size = 0;
        // SAFETY: as above.
        assert!(!unsafe { has_picture(b, None) }, "empty");
    }

    /// A buffer as KWin 6.8 sends it when only the pointer moved, made with the pointer
    /// apart: its plane marked corrupted (size left at the whole buffer's), the
    /// pointer's metadata with a new look (RGBA, premultiplied). It never reaches the
    /// encoder, and the core hears the look (straight, its hotspot) and the hotspot's
    /// place in the output's pixels.
    #[test]
    fn a_kwin_pointer_buffer_tells_the_core_and_not_the_encoder() {
        use crate::platform::{CursorEvent, OutputKey};
        let mut given = Given::new(16, 2);
        given.chunk.flags = ss::SPA_CHUNK_FLAG_CORRUPTED as i32;
        // Two pixels, RGBA premultiplied as KWin 6.8 sends them: blue opaque, red at half.
        let pixels = [0, 0, 255, 255, 128, 0, 0, 128];
        let mut cursor = super::super::meta::tests::cursor_meta(
            1,
            (100, 50),
            (1, 0),
            Some((ss::SPA_VIDEO_FORMAT_RGBA, 2, 1, &pixels)),
        );
        let mut metas = [ss::spa_meta {
            type_: ss::SPA_META_Cursor,
            size: cursor.len() as u32,
            data: cursor.as_mut_ptr().cast(),
        }];
        given.spa.n_metas = 1;
        given.spa.metas = metas.as_mut_ptr();
        let b = given.ptr();
        // SAFETY: the buffer, its parts and its metadata live through the test.
        let found = unsafe { super::super::meta::metas((*b).buffer) };
        // SAFETY: as above.
        let picture = unsafe { has_picture(b, found.header) };
        assert!(!picture, "the pointer alone");
        let meta = super::super::meta::read_cursor(found.cursor.unwrap()).unwrap();
        let mut tap = super::super::pointer::CursorTap::new(
            OutputKey::new("Virtual-Prostir-1"),
            std::sync::Arc::default(),
        );
        let mut heard = Vec::new();
        tap.events(&meta, (2560, 1440), |e| heard.push(e));
        let [
            CursorEvent::Shape(image),
            CursorEvent::Moved { output, x, y, size },
        ] = &heard[..]
        else {
            panic!("{heard:?}");
        };
        assert_eq!((image.w, image.h, image.hot_x, image.hot_y), (2, 1, 1, 0));
        assert_eq!(image.rgba, [0, 0, 255, 255, 255, 0, 0, 128]);
        assert_eq!(output.as_str(), "Virtual-Prostir-1");
        assert_eq!((*x, *y, *size), (100, 50, (2560, 1440)));
    }

    /// A frame of the feed takes no memory of Rust's (only GStreamer's, as
    /// `pipewiresrc`'s did): made of a buffer, pushed to the encoder, its pointer and
    /// changed parts seen, the buffer taken back; counted on this thread.
    #[test]
    fn a_frame_takes_no_memory() {
        let (pipeline, appsrc, seen) = pipeline();
        let wake = wake_file();
        let mut slots = Slots::new(wake);
        let mut given = [Given::new(4, 1), Given::new(4, 1)];
        let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Gray8, 4, 1)
            .build()
            .unwrap();
        for g in &mut given {
            // SAFETY: the buffers and their parts live through the test.
            unsafe { slots.add(g.ptr()) };
        }
        let mut pictures = Pictures::new(appsrc, Duration::from_secs(1), Arc::default());
        let sightings = super::super::meta::Sightings::default();
        let cursor = super::super::meta::tests::cursor_meta(1, (3, 4), (0, 0), None);
        let damage = super::super::meta::tests::damage_meta(&[(0, 0, 2, 1)], 4);
        let mut count = 0;
        let mut taken = 0u64;
        for i in 0..40 {
            let b = given[i % 2].ptr();
            // Waits for the encoder to let go of this buffer's frame of two frames ago.
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                let before = allocations();
                let mut back = 0;
                slots.take_back(|_| back += 1);
                taken += allocations() - before;
                count += back;
                // SAFETY: the buffers and their parts live through the test.
                let free = unsafe { slots.frame(b, &info, None) };
                if let Some(frame) = free {
                    let before = allocations();
                    sightings.saw_frame(Some(&cursor), Some(&damage), true);
                    pictures.push(frame, Instant::now());
                    if i >= 4 {
                        taken += allocations() - before;
                    }
                    break;
                }
                assert!(Instant::now() < until, "buffer {} never came back", i % 2);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert!(count >= 30, "{count} buffers back");
        assert!(seen.lock().unwrap().len() >= 30);
        assert_eq!(taken, 0, "allocations in the feed's frames");
        drop(pictures);
        pipeline.set_state(gst::State::Null).unwrap();
    }

    thread_local! {
        /// Allocations made on this thread (other tests run on threads of their own).
        static ALLOCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// The system's allocator, counting this thread's allocations for
    /// `a_frame_takes_no_memory` (the only allocator of the program's tests).
    struct Counting;

    fn count() {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
    }

    // SAFETY: every call is passed on to the system's allocator as it came; counting
    // touches only a thread-local number, which needs no memory.
    unsafe impl std::alloc::GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            count();
            // SAFETY: as the caller promised for `alloc`.
            unsafe { std::alloc::System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            // SAFETY: as the caller promised for `dealloc`.
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
            count();
            // SAFETY: as the caller promised for `realloc`.
            unsafe { std::alloc::System.realloc(ptr, layout, size) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    fn allocations() -> u64 {
        ALLOCS.with(std::cell::Cell::get)
    }

    /// A file of `bytes` in memory.
    fn tempfile_with(bytes: &[u8]) -> std::fs::File {
        use std::io::Write as _;
        use std::os::fd::FromRawFd;
        // SAFETY: a fresh anonymous file; the name is a valid C string.
        let fd = unsafe { libc::memfd_create(c"prostir-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0);
        // SAFETY: a descriptor just made, owned by nobody else.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(bytes).unwrap();
        file
    }
}
