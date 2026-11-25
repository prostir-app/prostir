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

use std::collections::HashMap;
use std::os::fd::{BorrowedFd, IntoRawFd};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer_allocators::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use log::warn;
use pipewire::spa::sys as ss;
use pipewire::sys as pw;

/// The GStreamer memory of one of the stream's buffers.
struct Slot {
    /// Tells this buffer from one PipeWire adds later at the same address.
    id: u64,
    /// One for each of its planes; none for memory of the stream's own process
    /// (copied each frame instead).
    memories: Vec<Option<gst::Memory>>,
    /// A frame of it is in GStreamer's hands.
    lent: bool,
}

/// The stream's buffers, by address.
#[derive(Default)]
pub struct Slots {
    slots: HashMap<usize, Slot>,
    next: u64,
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

/// Whether a buffer carries a picture: KWin sends buffers with the pointer alone (its
/// metadata, every plane empty), which do not go to the encoder.
///
/// # Safety
/// As [`datas`].
pub unsafe fn has_picture(buffer: *mut pw::pw_buffer) -> bool {
    // SAFETY: as the caller says.
    unsafe { datas(buffer) }
        .iter()
        // SAFETY: a plane's chunk is valid with it.
        .any(|d| !d.chunk.is_null() && unsafe { (*d.chunk).size } > 0)
}

/// Sends a buffer back to PipeWire once the frame made of it is let go: kept with the
/// frame (a parent buffer of it), it calls `back` when GStreamer frees it.
struct Holder(Option<Box<dyn FnOnce() + Send>>);

impl AsRef<[u8]> for Holder {
    fn as_ref(&self) -> &[u8] {
        &[]
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(back) = self.0.take() {
            back();
        }
    }
}

impl Slots {
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
        self.next += 1;
        self.slots.insert(
            buffer as usize,
            Slot {
                id: self.next,
                memories,
                lent: false,
            },
        );
    }

    /// PipeWire removes `buffer`: whether a frame of it is still out (it is then given
    /// back now, as `pipewiresrc` does; the frame keeps its memory).
    pub fn remove(&mut self, buffer: *mut pw::pw_buffer) -> bool {
        self.slots
            .remove(&(buffer as usize))
            .is_some_and(|s| s.lent)
    }

    /// GStreamer let go of the frame of buffer `at` (`id`): whether it goes back to
    /// PipeWire now (not when PipeWire removed it meanwhile).
    pub fn returned(&mut self, at: usize, id: u64) -> bool {
        match self.slots.get_mut(&at) {
            Some(s) if s.id == id && s.lent => {
                s.lent = false;
                true
            }
            _ => false,
        }
    }

    /// The picture of `buffer` as a frame in the layout `info`, which calls `back` with
    /// the buffer's address and id once GStreamer lets it go; none when it has no
    /// picture (the buffer then stays the caller's).
    ///
    /// # Safety
    /// `buffer` is a valid `pw_buffer` of this stream, dequeued, and stays valid until
    /// it is given back.
    pub unsafe fn frame(
        &mut self,
        buffer: *mut pw::pw_buffer,
        info: &gst_video::VideoInfo,
        crop: Option<Crop>,
        back: impl FnOnce(usize, u64) + Send + 'static,
    ) -> Option<gst::Buffer> {
        let at = buffer as usize;
        let slot = self.slots.get_mut(&at)?;
        // SAFETY: as the caller says.
        let datas = unsafe { datas(buffer) };
        let mut frame = gst::Buffer::new();
        {
            let f = frame.get_mut().expect("new buffer");
            let planes = info.n_planes() as usize;
            let mut offsets = info.offset()[..planes].to_vec();
            let mut strides = info.stride()[..planes].to_vec();
            let mut size = 0;
            for (i, d) in datas.iter().enumerate() {
                if d.chunk.is_null() {
                    return None;
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
                    Some(_) => return None,
                    // Memory of the stream's own process: copied.
                    None if d.type_ == ss::SPA_DATA_MemPtr
                        && !d.data.is_null()
                        && start + len <= d.maxsize as usize =>
                    {
                        // SAFETY: the plane's memory is `maxsize` bytes, the chunk
                        // within it.
                        let bytes = unsafe {
                            std::slice::from_raw_parts(d.data.cast::<u8>().add(start), len)
                        };
                        gst::Memory::from_slice(bytes.to_vec())
                    }
                    None => return None,
                };
                f.append_memory(memory);
                if chunk.flags as u32 & ss::SPA_CHUNK_FLAG_CORRUPTED != 0 {
                    f.set_flags(gst::BufferFlags::CORRUPTED);
                }
            }
            if f.size() == 0 {
                return None;
            }
            gst_video::VideoMeta::add_full(
                f,
                gst_video::VideoFrameFlags::empty(),
                info.format(),
                info.width(),
                info.height(),
                &offsets,
                &strides,
            )
            .ok()?;
            if let Some(rect) = crop {
                gst_video::VideoCropMeta::add(f, rect);
            }
            let id = slot.id;
            let holder = gst::Buffer::from_slice(Holder(Some(Box::new(move || back(at, id)))));
            gst::ParentBufferMeta::add(f, &holder);
        }
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
/// nothing new comes (`pipewiresrc`'s `keepalive-time`), and at once when asked.
pub struct Pictures {
    appsrc: gst_app::AppSrc,
    every: Duration,
    last: Option<gst::Buffer>,
    /// When the last one goes again; none while paused.
    due: Option<Instant>,
}

impl Pictures {
    pub fn new(appsrc: gst_app::AppSrc, every: Duration) -> Self {
        Self {
            appsrc,
            every,
            last: None,
            due: None,
        }
    }

    /// A new picture, at `now`.
    pub fn push(&mut self, frame: gst::Buffer, now: Instant) {
        let _ = self.appsrc.push_buffer(frame.clone());
        self.last = Some(frame);
        self.due = Some(now + self.every);
    }

    /// The last picture again, the same buffer (the encoder tells it is a repeat by
    /// its memory); whether there was one.
    pub fn again(&mut self, now: Instant) -> bool {
        let Some(last) = self.last.clone() else {
            return false;
        };
        let _ = self.appsrc.push_buffer(last);
        self.due = Some(now + self.every);
        true
    }

    /// Sends the last picture again when its time came.
    pub fn tick(&mut self, now: Instant) {
        if self.due.is_some_and(|due| now >= due) {
            self.again(now);
        }
    }

    /// How long until the last picture goes again; none: not before something
    /// happens.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.due.map(|due| due.saturating_duration_since(now))
    }

    /// Nothing is sent again until a new picture comes or one is asked for (paused).
    pub fn rest(&mut self) {
        self.due = None;
    }

    /// The last picture is let go: the stream starts over.
    pub fn forget(&mut self) {
        self.last = None;
        self.due = None;
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
        let mut pictures = Pictures::new(appsrc, every);
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

    /// A buffer of shared memory, as PipeWire hands it over: one frame shares its
    /// memory with the part the chunk says, in the layout it says, and the buffer goes
    /// back only once GStreamer let go of the frame, once.
    #[test]
    fn a_shared_memory_buffer_as_a_frame() {
        use std::io::Write as _;
        use std::os::fd::AsRawFd;
        gst::init().unwrap();
        let (w, h, stride) = (4u32, 2u32, 16usize);
        let mut file = tempfile_with(&[7u8; 64]);
        file.flush().unwrap();
        let mut chunk = ss::spa_chunk {
            offset: 0,
            size: (stride * h as usize) as u32,
            stride: stride as i32,
            flags: 0,
        };
        // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
        let mut data: ss::spa_data = unsafe { std::mem::zeroed() };
        data.type_ = ss::SPA_DATA_MemFd;
        data.flags = ss::SPA_DATA_FLAG_READABLE | ss::SPA_DATA_FLAG_MAPPABLE;
        data.fd = i64::from(file.as_raw_fd());
        data.maxsize = 64;
        data.chunk = &raw mut chunk;
        // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
        let mut spa: ss::spa_buffer = unsafe { std::mem::zeroed() };
        spa.n_datas = 1;
        spa.datas = &raw mut data;
        // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
        let mut pwb: pw::pw_buffer = unsafe { std::mem::zeroed() };
        pwb.buffer = &raw mut spa;
        let b = &raw mut pwb;

        let mut slots = Slots::default();
        // SAFETY: the buffer and its parts live through the test.
        unsafe { slots.add(b) };
        // SAFETY: the buffer and its parts live through the test.
        assert!(unsafe { has_picture(b) });
        let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgrx, w, h)
            .build()
            .unwrap();
        let back = Arc::new(Mutex::new(Vec::new()));
        let told = back.clone();
        // SAFETY: as above.
        let frame = unsafe {
            slots.frame(b, &info, None, move |at, id| {
                told.lock().unwrap().push((at, id));
            })
        }
        .unwrap();
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
        let again = frame.clone();
        drop(frame);
        assert!(back.lock().unwrap().is_empty(), "still held");
        drop(again);
        let given = back.lock().unwrap().clone();
        assert_eq!(given.len(), 1);
        assert!(slots.returned(given[0].0, given[0].1));
        assert!(!slots.returned(given[0].0, given[0].1), "once");

        // A buffer with the pointer alone carries no picture.
        // SAFETY: the chunk the plane points to, alive through the test.
        unsafe { (*data.chunk).size = 0 };
        // SAFETY: the buffer and its parts live through the test.
        assert!(!unsafe { has_picture(b) });
        // SAFETY: as above.
        assert!(unsafe { slots.frame(b, &info, None, |_, _| {}) }.is_none());
        assert!(!slots.remove(b), "not lent");
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
