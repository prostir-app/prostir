//! The runtime's models of the controllers and hands (step 8.6): a hand's once at the
//! start, a controller's when a side's interaction profile changes to a controller
//! (and a few times more while the runtime has none for it yet: it may say so for a
//! moment as the controller wakes). A controller's model is asked for, read and made
//! ready for the GPU on a thread of its own (the runtime takes a few milliseconds and
//! the file is read and its pictures decoded), so a controller coming or going never
//! holds up a frame; the frame only compares the profiles, takes what came without
//! waiting and puts it on the GPU. The core decides what is drawn.

use std::io::Cursor;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{info, warn};
use openxr as xr;

use super::start::Xr;
use crate::model::glb::{ImageKind, Model};
use crate::platform::meta::models as runtime;
use crate::platform::msft;
use crate::platform::openxr::Gfx;
use crate::platform::openxr::input::Controls;
use crate::platform::openxr::overlay::{Corners, Overlay};

/// How often a side without a model asks again, and how many times.
const RETRY_EVERY: Duration = Duration::from_secs(2);
const RETRIES: u8 = 5;
/// The largest picture of a model decoded, on a side, and the memory decoding takes.
const PICTURE_SIDE_MAX: u32 = 4096;
const PICTURE_BYTES_MAX: u64 = 64 << 20;

/// A controller's model to ask for: which side and which ask of it, the hand's path,
/// the colour of a picture that cannot be decoded (linear).
struct Ask {
    side: usize,
    ask: u32,
    hand: xr::Path,
    tint: [f32; 3],
}

/// What came of an ask (`None`: no model).
struct Came {
    side: usize,
    ask: u32,
    model: Option<Corners>,
}

/// The thread that asks for the controllers' models.
struct Loader {
    asks: Option<Sender<Ask>>,
    came: Receiver<Came>,
    thread: Option<JoinHandle<()>>,
}

impl Loader {
    fn start(session: &xr::Session<Gfx>) -> Option<Self> {
        let session = session.clone();
        let (asks, asked) = mpsc::channel::<Ask>();
        let (tell, came) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("models".into())
            .spawn(move || {
                // Until the frame loop lets go of its end.
                while let Ok(a) = asked.recv() {
                    let model = controller(&session, a.hand, a.side, a.tint);
                    let came = Came {
                        side: a.side,
                        ask: a.ask,
                        model,
                    };
                    if tell.send(came).is_err() {
                        break;
                    }
                }
            })
            .inspect_err(|e| warn!("controller models: no thread: {e}"))
            .ok()?;
        Some(Self {
            asks: Some(asks),
            came,
            thread: Some(thread),
        })
    }
}

impl Drop for Loader {
    /// The thread ends once it finished what it was asking (a few milliseconds).
    fn drop(&mut self) {
        self.asks = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What each side's controller was asked for.
#[derive(Default)]
pub struct Models {
    /// The profile (and whether it is a hand) the side's model was last asked for.
    asked_for: [Option<(Option<xr::Path>, bool)>; 2],
    /// Each side's last ask: an answer to an earlier one is too late and left.
    ask: [u32; 2],
    /// When a side without a model asks again, and how many times it still may.
    again: [Option<(Instant, u8)>; 2],
    loader: Option<Loader>,
}

impl Models {
    /// The controllers' models asked for on a thread of the session `session`'s.
    pub fn new(session: &xr::Session<Gfx>) -> Self {
        Self {
            loader: Loader::start(session),
            ..Self::default()
        }
    }

    /// The hands' models, once at the start (before the first frame), onto `overlay`.
    pub fn hands(xr: &Xr, trackers: Option<&[xr::HandTracker; 2]>, overlay: &mut Overlay) {
        let Some(trackers) = trackers else {
            return;
        };
        for (h, tracker) in trackers.iter().enumerate() {
            let mesh = runtime::hand(&xr.instance, tracker);
            if let Err(e) = overlay.set_hand(&xr.gl, h, mesh) {
                warn!("hand model: {e:#}");
            }
        }
    }

    /// Asks for a side's controller model when its profile changed (or again, while
    /// it has none), with pictures it cannot decode tinted `tint` (linear); puts on
    /// `overlay` what came. Never waits.
    pub fn refresh(
        &mut self,
        now: Instant,
        gl: &glow::Context,
        controls: &Controls,
        overlay: &mut Overlay,
        tint: [f32; 3],
    ) {
        let Some(loader) = &self.loader else {
            return;
        };
        while let Ok(c) = loader.came.try_recv() {
            if c.ask != self.ask[c.side] {
                continue;
            }
            let have = c.model.is_some();
            if let Err(e) = overlay.set_controller(gl, c.side, c.model.as_ref()) {
                warn!("controller model: {e:#}");
            }
            if have {
                self.again[c.side] = None;
            }
        }
        for side in 0..2 {
            let profile = controls.profile(side);
            let changed = self.asked_for[side] != Some(profile);
            let retry = !changed && self.again[side].is_some_and(|(at, _)| now >= at);
            if !changed && !retry {
                continue;
            }
            self.asked_for[side] = Some(profile);
            let (path, is_hand) = profile;
            let controller = path.is_some() && !is_hand;
            self.again[side] = match (controller, changed, self.again[side]) {
                // A controller: asked again a few times until it has a model (an
                // answer with one ends it).
                (true, true, _) => Some((now + RETRY_EVERY, RETRIES)),
                (true, false, Some((_, left))) if left > 1 => Some((now + RETRY_EVERY, left - 1)),
                _ => None,
            };
            if !controller {
                // Put down, or tracked hands: no model now, whatever comes late.
                self.ask[side] = self.ask[side].wrapping_add(1);
                if let Err(e) = overlay.set_controller(gl, side, None) {
                    warn!("controller model: {e:#}");
                }
                continue;
            }
            self.ask[side] = self.ask[side].wrapping_add(1);
            let ask = Ask {
                side,
                ask: self.ask[side],
                hand: controls.hand_path(side),
                tint,
            };
            if let Some(asks) = &loader.asks
                && asks.send(ask).is_err()
            {
                warn!("controller models: the thread ended");
            }
        }
    }
}

/// The model of the controller in hand `hand` (side `side`), read, coloured and made
/// ready for the GPU: from `XR_FB_render_model`, else `XR_MSFT_controller_model`.
fn controller(
    session: &xr::Session<Gfx>,
    hand: xr::Path,
    side: usize,
    tint: [f32; 3],
) -> Option<Corners> {
    let (key, bytes) = match runtime::controller(session, side) {
        Some((key, _, bytes)) => (key, bytes),
        None => msft::controller(session, hand)?,
    };
    let mut model = match Model::read(&bytes) {
        Ok(m) => m,
        Err(e) => {
            warn!("controller model: {e}");
            return None;
        }
    };
    for i in 0..model.images.len() {
        let format = match model.images[i].kind {
            ImageKind::Png => Some(image::ImageFormat::Png),
            ImageKind::Jpeg => Some(image::ImageFormat::Jpeg),
            ImageKind::Ktx2 | ImageKind::Other => None,
        };
        let decoded = format.and_then(|f| {
            decode(model.image_bytes(&bytes, i), f)
                .inspect_err(|e| warn!("controller model's picture {i}: {e}"))
                .ok()
        });
        match decoded {
            Some(picture) => {
                let rgba = picture.to_rgba8();
                model.paint(i, rgba.width(), rgba.height(), rgba.as_raw());
            }
            // A picture not decoded here (KTX2): one colour instead.
            None => model.tint(i, tint),
        }
    }
    info!(
        "controller model {key:#x}: {} corners, {} triangles",
        model.mesh.positions.len(),
        model.mesh.triangles()
    );
    Some(Corners::of(key, model.into_mesh()))
}

/// A picture of `format` decoded, no larger than the limits.
fn decode(bytes: &[u8], format: image::ImageFormat) -> image::ImageResult<image::DynamicImage> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(PICTURE_SIDE_MAX);
    limits.max_image_height = Some(PICTURE_SIDE_MAX);
    limits.max_alloc = Some(PICTURE_BYTES_MAX);
    reader.limits(limits);
    reader.decode()
}
