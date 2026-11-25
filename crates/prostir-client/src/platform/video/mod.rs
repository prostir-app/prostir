//! The streams' video (step 7 note, 4.4): each stream of the PC decoded by MediaCodec
//! straight into an Android surface the compositor owns
//! (`XR_KHR_android_surface_swapchain`), by generation. [`SurfaceVideo`] makes and ends
//! each stream's surface and decoder thread, makes the surface again when the decoder
//! asks for it, and says what the core needs to know ([`VideoEvent`]): trouble, a
//! picture again, a lasting failure. The rules are the core's (`crate::video`).

pub mod decoder;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use log::{error, warn};
use openxr as xr;
use prostir_proto::Codec;
use prostir_proto::state::Gen;

use self::decoder::{DecoderJob, End, Status, Stopped, Stream};
use crate::frames::{Frames, Received};
use crate::ids::DisplayKey;
use crate::net::Feedback;
use crate::platform::openxr::{self as xrutil, Gfx};
#[cfg(all(feature = "selftest", not(feature = "store")))]
use crate::video::Fault;
use crate::video::{Happened, VideoEvent, Watch, Why};

/// One stream: the compositor's surface it is decoded into and its decoder thread.
struct Monitor {
    generation: Gen,
    width: u32,
    height: u32,
    decoder: Option<JoinHandle<Stopped>>,
    stop: Arc<AtomicBool>,
    status: Arc<Status>,
    watch: Watch,
    /// `None` once it failed for good (nothing is shown, nothing tried again).
    swapchain: Option<xr::Swapchain<Gfx>>,
    /// What came of its stream (the numbers of "For development").
    received: Arc<Received>,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // The decoder must stop writing into the surface before the swapchain goes away.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.decoder.take() {
            // It sleeps until there is work: woken, it sees `stop` at once.
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// What a layer shows of a stream: its swapchain and its size in pixels.
pub struct Image<'a> {
    pub swapchain: &'a xr::Swapchain<Gfx>,
    pub width: u32,
    pub height: u32,
}

/// The streams shown, each with its surface and decoder (see the module).
#[derive(Default)]
pub struct SurfaceVideo {
    monitors: HashMap<DisplayKey, Monitor>,
}

/// What a stream is: its display and generation, codec and size.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub id: DisplayKey,
    pub generation: Gen,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// The largest picture the headset said the codec decodes (`None`: none of it).
    pub said: Option<(u32, u32)>,
}

impl SurfaceVideo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Generation `spec.generation` of a stream starts: an older one of the same
    /// display stops first (its decoder, then its surface), then its surface and
    /// decoder are made. A surface that cannot be made is a lasting failure, said in
    /// `out`.
    pub fn start(
        &mut self,
        session: &xr::Session<Gfx>,
        vm: &jni::JavaVM,
        spec: Spec,
        frames: Frames,
        feedback: Feedback,
        out: &mut Vec<VideoEvent>,
    ) {
        let Spec {
            id,
            generation,
            codec,
            width,
            height,
            said,
        } = spec;
        self.monitors.remove(&id);
        let received = frames.received();
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Status::new());
        let mut monitor = Monitor {
            generation,
            width,
            height,
            decoder: None,
            stop: stop.clone(),
            status: status.clone(),
            watch: Watch::default(),
            swapchain: None,
            received,
        };
        match xrutil::create_surface_swapchain(session, vm, width, height) {
            Ok((swapchain, window)) => {
                monitor.decoder = Some(decoder::spawn(DecoderJob {
                    stream: Stream {
                        id: id.id,
                        generation,
                    },
                    codec,
                    width,
                    height,
                    window,
                    frames,
                    feedback,
                    stop,
                    status,
                    failures: 0,
                    said,
                }));
                monitor.swapchain = Some(swapchain);
            }
            Err(e) => {
                error!("monitor {id}: {e:#}");
                out.push(VideoEvent {
                    id,
                    generation,
                    what: Happened::Failed(Why::NoSurface),
                });
            }
        }
        self.monitors.insert(id, monitor);
    }

    /// Display `id`'s stream ends (or the display went): its decoder stops, then its
    /// surface goes.
    pub fn end(&mut self, id: DisplayKey) {
        self.monitors.remove(&id);
    }

    /// The bytes of every frame of display `id`'s stream so far, and of its last
    /// keyframe (see `frames::Received`).
    pub fn received(&self, id: DisplayKey) -> Option<(u64, Option<u32>)> {
        self.monitors.get(&id).map(|m| m.received.get())
    }

    /// What a layer of display `id` shows (`None`: nothing to show).
    pub fn image(&self, id: DisplayKey) -> Option<Image<'_>> {
        let m = self.monitors.get(&id)?;
        Some(Image {
            swapchain: m.swapchain.as_ref()?,
            width: m.width,
            height: m.height,
        })
    }

    /// Looked at once a pass: a decoder thread that ended while its stream goes on is
    /// made again with a new surface where it asks, or its lasting failure is said; and
    /// what changed of each stream's trouble (see `video::Watch`). Into `out`; nothing
    /// is made unless a thread ended.
    pub fn poll(
        &mut self,
        now: Instant,
        session: &xr::Session<Gfx>,
        vm: &jni::JavaVM,
        out: &mut Vec<VideoEvent>,
    ) {
        for (id, m) in &mut self.monitors {
            let finished = !m.stop.load(Ordering::Relaxed)
                && m.decoder.as_ref().is_some_and(JoinHandle::is_finished);
            if finished && let Some(thread) = m.decoder.take() {
                let failed = match thread.join() {
                    // The stream ended: its end comes from the network.
                    Ok(Stopped {
                        end: End::Ended, ..
                    }) => None,
                    Ok(Stopped {
                        job,
                        end: End::NewSurface,
                    }) => Self::new_surface(*id, m, job, session, vm).err(),
                    Ok(Stopped {
                        end: End::Failed(why),
                        ..
                    }) => Some(why),
                    Err(_) => {
                        error!("monitor {id}: its decoder panicked");
                        Some(Why::Stopped)
                    }
                };
                if let Some(why) = failed {
                    m.swapchain = None;
                    out.push(VideoEvent {
                        id: *id,
                        generation: m.generation,
                        what: Happened::Failed(why),
                    });
                    continue;
                }
            }
            if m.swapchain.is_none() {
                continue;
            }
            if let Some(what) = m.watch.look(now, m.status.failing_since()) {
                out.push(VideoEvent {
                    id: *id,
                    generation: m.generation,
                    what,
                });
            }
        }
    }

    /// The decoder of `m` asked for a new surface: the old one goes (its thread has
    /// ended), a new one is made, and the thread again with it.
    fn new_surface(
        id: DisplayKey,
        m: &mut Monitor,
        mut job: DecoderJob,
        session: &xr::Session<Gfx>,
        vm: &jni::JavaVM,
    ) -> Result<(), Why> {
        m.swapchain = None;
        match xrutil::create_surface_swapchain(session, vm, m.width, m.height) {
            Ok((swapchain, window)) => {
                job.window = window;
                m.swapchain = Some(swapchain);
                m.decoder = Some(decoder::spawn(job));
                warn!("monitor {id}: a new surface");
                Ok(())
            }
            Err(e) => {
                error!("monitor {id}: no new surface: {e:#}");
                Err(Why::NoSurface)
            }
        }
    }

    /// Causes `fault` in display `id`'s decoder, on purpose (the selftest's hook, and
    /// checks by hand).
    #[cfg(all(feature = "selftest", not(feature = "store")))]
    pub fn inject(&self, id: DisplayKey, fault: Fault) -> bool {
        let Some(m) = self.monitors.get(&id) else {
            return false;
        };
        m.status.cause(fault);
        if let Some(t) = &m.decoder {
            t.thread().unpark();
        }
        true
    }
}
