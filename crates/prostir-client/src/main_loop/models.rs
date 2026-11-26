//! The runtime's models of the controllers and hands (step 8.6), asked for when they
//! come and put on the GPU once: a hand's at the start, a controller's when a side's
//! interaction profile changes to a controller (and a few times more while the
//! runtime has none for it yet: it may say so for a moment as the controller wakes).
//! A frame only compares the profiles it already has; the core decides what is drawn.

use std::time::{Duration, Instant};

use log::{info, warn};
use openxr as xr;

use super::start::Xr;
use crate::model::glb::{ImageKind, Model};
use crate::platform::meta::models as runtime;
use crate::platform::msft;
use crate::platform::openxr::input::Controls;
use crate::platform::openxr::overlay::Overlay;

/// How often a side without a model asks again, and how many times.
const RETRY_EVERY: Duration = Duration::from_secs(2);
const RETRIES: u8 = 5;

/// What each side's controller was asked for.
#[derive(Debug, Default)]
pub struct Models {
    /// The profile (and whether it is a hand) the side's model was last asked for.
    asked_for: [Option<(Option<xr::Path>, bool)>; 2],
    /// When a side without a model asks again, and how many times it still may.
    again: [Option<(Instant, u8)>; 2],
}

impl Models {
    /// The hands' models, once at the start, onto `overlay`.
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
    /// it has none), with pictures it cannot decode tinted `tint` (linear).
    pub fn refresh(
        &mut self,
        now: Instant,
        xr: &Xr,
        controls: &Controls,
        overlay: &mut Overlay,
        tint: [f32; 3],
    ) {
        for side in 0..2 {
            let profile = controls.profile(side);
            let changed = self.asked_for[side] != Some(profile);
            let retry = self.again[side].is_some_and(|(at, _)| now >= at);
            if !changed && !retry {
                continue;
            }
            self.asked_for[side] = Some(profile);
            let (path, is_hand) = profile;
            let found = if path.is_none() || is_hand {
                None
            } else {
                controller(xr, controls.hand_path(side), side, tint)
            };
            let have = found.is_some();
            if let Err(e) =
                overlay.set_controller(&xr.gl, side, found.as_ref().map(|(key, m)| (*key, &m.mesh)))
            {
                warn!("controller model: {e:#}");
            }
            self.again[side] = match (have, changed, self.again[side]) {
                // A controller without a model yet: a few more times.
                (false, true, _) if path.is_some() && !is_hand => {
                    Some((now + RETRY_EVERY, RETRIES))
                }
                (false, false, Some((_, left))) if left > 1 => Some((now + RETRY_EVERY, left - 1)),
                _ => None,
            };
        }
    }
}

/// The model of the controller in hand `hand` (side `side`), read and coloured: from
/// `XR_FB_render_model`, else `XR_MSFT_controller_model`.
fn controller(xr: &Xr, hand: xr::Path, side: usize, tint: [f32; 3]) -> Option<(u64, Model)> {
    let (key, bytes) = match runtime::controller(&xr.session, side) {
        Some((key, _, bytes)) => (key, bytes),
        None => msft::controller(&xr.session, hand)?,
    };
    let mut model = match Model::read(&bytes) {
        Ok(m) => m,
        Err(e) => {
            warn!("controller model: {e}");
            return None;
        }
    };
    for i in 0..model.images.len() {
        let image = &model.images[i];
        let format = match image.kind {
            ImageKind::Png => Some(image::ImageFormat::Png),
            ImageKind::Jpeg => Some(image::ImageFormat::Jpeg),
            ImageKind::Ktx2 | ImageKind::Other => None,
        };
        let decoded = format.and_then(|f| {
            image::load_from_memory_with_format(&image.bytes, f)
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
    Some((key, model))
}
