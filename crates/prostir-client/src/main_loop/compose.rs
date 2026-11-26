//! The frame's layers as OpenXR takes them, and the frame's end: what `layers::plan`
//! says is shown, in its order, each made of the picture the shell has for it (the
//! room or the panorama, the window onto the keyboard, a monitor's stream, a picture
//! drawn on the CPU, the hands and rays).

use std::ptr;
use std::time::Instant;

use anyhow::Result;
use glam::{Quat, Vec3};
use log::warn;
use openxr as xr;
use openxr::sys::Handle as _;

use super::pictures::Pictures;
use super::shell::Shell;
use super::start::{Stage, Xr};
use super::{Gpu, Kept};
use crate::app::{Event, PlatformEvent};
use crate::clarity::Quality;
use crate::ids::{DisplayKey, PcId};
use crate::layers::{self, Layer, Plan, Quad};
use crate::layout::Cylinder;
use crate::platform::meta::{self, passthrough};
use crate::platform::openxr::overlay;
use crate::platform::openxr::{self as xrutil, Gfx, ImageLayer, as_layer, pose, posef};
use crate::platform::video::SurfaceVideo;
use crate::view::fx::Effects;

/// The side of the black layer over a dimmed room, in metres, a metre in front of
/// the eyes: wider than any field of view.
const DIM_SIZE: f32 = 40.0;

/// Buffers of a frame's layers, kept from frame to frame and filled again.
#[derive(Default)]
pub struct Buffers {
    /// Where the eyes are, for the hands and rays.
    views: [xr::View; 2],
    /// The hands, rays and effects of the frame.
    pub effects: Effects,
    /// Where the controllers' and hands' models go this frame.
    pub poses: overlay::Poses,
    /// What is shown, where, in which order.
    pub plan: Plan,
    /// Each monitor's colour scale, in the plan's order.
    dims: Vec<xr::sys::CompositionLayerColorScaleBiasKHR>,
    /// Each PC pointer's colour scale (its monitor's), in the plan's order.
    pointer_dims: Vec<xr::sys::CompositionLayerColorScaleBiasKHR>,
    /// The flat layers and the curved ones, in the plan's order.
    quads: Vec<xr::sys::CompositionLayerQuad>,
    cylinders: Vec<xr::sys::CompositionLayerCylinderKHR>,
    /// Where each layer of the plan was made, in its order.
    made: Vec<Made>,
    /// The layers' list for `end`: its memory only, empty between frames (see
    /// [`reuse`]).
    layers: Vec<&'static xr::CompositionLayerBase<'static, Gfx>>,
}

/// Where a layer of the plan was made.
#[derive(Debug, Clone, Copy)]
enum Made {
    Quad(usize),
    Cylinder(usize),
    Background,
    KeyboardWindow,
    Hands,
    /// Its picture went in the meantime: left out.
    Nothing,
}

/// An empty list of references in the memory of `v` (emptied): a list kept from frame
/// to frame whose items borrow from one frame only. Collected into the same layout,
/// nothing is allocated.
fn reuse<'a, 'b, T, U: 'b>(mut v: Vec<&'a T>) -> Vec<&'b U> {
    v.clear();
    v.into_iter().filter_map(|_| None).collect()
}

/// What the shell has a picture for, for the plan.
struct Have<'a> {
    background: bool,
    keyboard_window: bool,
    hands: bool,
    video: &'a SurfaceVideo,
    pictures: &'a Pictures,
}

impl layers::Ready for Have<'_> {
    fn background(&self) -> bool {
        self.background
    }

    fn keyboard_window(&self) -> bool {
        self.keyboard_window
    }

    fn picture(&self, id: DisplayKey) -> Option<(u32, u32)> {
        self.video.image(id).map(|m| (m.width, m.height))
    }

    fn trouble(&self, id: DisplayKey) -> bool {
        self.pictures.trouble_layers.contains_key(&id)
    }

    fn placeholder(&self) -> bool {
        self.pictures.placeholder.is_some()
    }

    fn menu(&self) -> bool {
        self.pictures.panel_layer.ready()
    }

    fn palm(&self) -> bool {
        self.pictures.wrist_layer.ready()
    }

    fn attach(&self) -> bool {
        self.pictures.attach_drawn.is_some()
    }

    #[cfg(feature = "preview")]
    fn metrics(&self) -> bool {
        self.pictures.metrics.ready()
    }

    fn hands(&self) -> bool {
        self.hands
    }

    fn pc_cursor(&self, pc: PcId, shape: u32) -> bool {
        self.pictures.pc_cursors.iter().any(|p| p.shows(pc, shape))
    }
}

/// The picture of a planned flat layer (not a monitor's).
fn picture_of<'p>(pictures: &'p Pictures, layer: &Layer) -> Option<(&'p ImageLayer, Quad)> {
    Some(match *layer {
        Layer::Trouble { id, quad } => (&pictures.trouble_layers.get(&id)?.2, quad),
        Layer::Placeholder(quad) => (&pictures.placeholder.as_ref()?.0, quad),
        #[cfg(feature = "preview")]
        Layer::Metrics(quad) => (&pictures.metrics_layer, quad),
        Layer::MenuButton(quad) => (&pictures.button_layer, quad),
        Layer::Menu(quad) => (&pictures.panel_layer, quad),
        Layer::Toast(quad) => (&pictures.toast_layer, quad),
        Layer::Attach(quad) => (&pictures.attach_layer, quad),
        Layer::Palm(quad) => (&pictures.wrist_layer, quad),
        Layer::Cursor(quad) => (&pictures.cursor_layer, quad),
        Layer::Background
        | Layer::KeyboardWindow
        | Layer::Monitor(_)
        | Layer::Hands
        | Layer::PcCursor { .. } => {
            return None;
        }
    })
}

/// The hands, rays and effects drawn (`kept.buffers.effects`, filled), the layers planned and
/// made, and the frame at `time` ended: how many layers it wanted.
pub fn end(
    xr: &Xr,
    stage: &mut Stage,
    shell: &mut Shell<'_>,
    kept: &mut Kept,
    now: Instant,
    time: xr::Time,
) -> Result<usize> {
    let (gl, exts) = (&xr.gl, &xr.exts);
    let session = shell.session;
    let local = &stage.local;
    let view = &stage.view;
    let format = stage.format;
    let buffers = &mut kept.buffers;
    let hands = &kept.hands;
    let pictures = &mut stage.pictures;
    let effects = &mut buffers.effects;
    let views = (!effects.is_empty()
        && xrutil::locate_views(session, time, local, &mut buffers.views))
    .then_some(&buffers.views[..]);
    if let Some(ov) = &mut stage.overlay {
        ov.render(gl, views.unwrap_or(&[]), effects.list(), &buffers.poses)?;
    }
    let projection_views = stage
        .overlay
        .as_ref()
        .zip(views)
        .and_then(|(o, v)| o.layer_views(v));
    let hands_layer = projection_views
        .as_ref()
        .map(|v| overlay::projection_layer(local, v));

    // Supersampled and sharpened as each monitor's scale says, and the stream's
    // picture the right way up, where the runtime can: one chain for each of the four
    // ways, made on the stack.
    let qualities = Quality::ALL.map(meta::sharper);
    let quality_next = |i: usize| -> *const std::ffi::c_void {
        if exts.fb_composition_layer_settings {
            (&raw const qualities[i]).cast()
        } else {
            ptr::null()
        }
    };
    let flips = [0, 1, 2, 3].map(|i| meta::flipped(quality_next(i)));
    let video_next = |quality: Quality| -> *const std::ffi::c_void {
        let i = quality.index();
        if exts.fb_composition_layer_image_layout {
            (&raw const flips[i]).cast()
        } else {
            quality_next(i)
        }
    };

    // The keyboard window: placed in front of the user at desk height the first
    // time, then wherever it was moved.
    let mut keyboard_failed = false;
    if let Some(pt) = shell.room.fb().filter(|pt| pt.window_supported()) {
        let shown = shell
            .model
            .keyboard_window()
            .map(|p| (posef(&p), p.width, shell.model.keyboard_depth()));
        if let Err(e) = pt.set_window(local, time, shown) {
            warn!("keyboard window: {e:#}");
            keyboard_failed = true;
        }
    }
    if keyboard_failed {
        shell.handle(now, Event::Platform(PlatformEvent::KeyboardWindowFailed));
    }
    let passthrough_layer = |handle: Option<xr::sys::PassthroughLayerFB>| {
        handle.map(|layer_handle| xr::sys::CompositionLayerPassthroughFB {
            ty: xr::sys::CompositionLayerPassthroughFB::TYPE,
            next: ptr::null(),
            flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: xr::sys::Space::NULL,
            layer_handle,
        })
    };
    let keyboard_layer = passthrough_layer(
        shell
            .room
            .fb_ref()
            .and_then(passthrough::Passthrough::window_layer),
    );
    let passthrough_layer = passthrough_layer(
        shell
            .room
            .fb_ref()
            .and_then(passthrough::Passthrough::room_layer),
    );
    let htc_layer = shell.room.htc_passthrough().and_then(|p| p.layer(local));
    // The room blended with the frame and dimmed: a black layer over it, letting
    // part of it through (made once for each darkness).
    let dim = shell.room.dim();
    pictures.dim(
        Gpu {
            session,
            gl,
            format,
        },
        dim,
    )?;
    let dim_quad = dim.and(pictures.dim_layer.as_ref()).map(|(_, l)| {
        l.quad(
            view,
            pose(Vec3::new(0.0, 0.0, -1.0), Quat::IDENTITY),
            DIM_SIZE,
            DIM_SIZE,
        )
    });
    let model = &shell.model;
    let pano = pictures
        .pano_layer
        .as_ref()
        .filter(|(b, _)| *b == model.settings().background && !shell.room.shown())
        .map(|(_, l)| xr::sys::CompositionLayerEquirect2KHR {
            ty: xr::sys::CompositionLayerEquirect2KHR::TYPE,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space: local.as_raw(),
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: l.sub_image(),
            pose: xr::Posef::IDENTITY,
            radius: 0.0,
            central_horizontal_angle: std::f32::consts::TAU,
            upper_vertical_angle: std::f32::consts::FRAC_PI_2,
            lower_vertical_angle: -std::f32::consts::FRAC_PI_2,
        });
    // Behind everything: the cameras' room, by whichever way this headset shows it,
    // else its dimming, else the panorama.
    let background: Option<&xr::CompositionLayerBase<'_, Gfx>> = passthrough_layer
        .as_ref()
        .map(as_layer)
        .or_else(|| htc_layer.as_ref().map(as_layer))
        .or_else(|| dim_quad.as_ref().map(as_layer))
        .or_else(|| pano.as_ref().map(as_layer));

    // What is shown, where, and in which order.
    let have = Have {
        background: background.is_some(),
        keyboard_window: keyboard_layer.is_some(),
        hands: hands_layer.is_some(),
        video: &shell.video,
        pictures,
    };
    let plan = &mut buffers.plan;
    layers::plan(model, hands, &have, plan);

    // The monitors' colour scales first: their layers point at them.
    let dims = &mut buffers.dims;
    dims.clear();
    dims.extend(plan.layers().iter().filter_map(|l| match l {
        Layer::Monitor(m) => Some(xr::sys::CompositionLayerColorScaleBiasKHR {
            ty: xr::sys::CompositionLayerColorScaleBiasKHR::TYPE,
            next: video_next(m.quality),
            color_scale: xr::Color4f {
                r: m.bright,
                g: m.bright,
                b: m.bright,
                a: m.alpha,
            },
            color_bias: xr::Color4f {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        }),
        _ => None,
    }));
    // The PCs' pointers', their monitors' brightness and fading.
    let pointer_dims = &mut buffers.pointer_dims;
    pointer_dims.clear();
    pointer_dims.extend(plan.layers().iter().filter_map(|l| match *l {
        Layer::PcCursor { bright, alpha, .. } => Some(xr::sys::CompositionLayerColorScaleBiasKHR {
            ty: xr::sys::CompositionLayerColorScaleBiasKHR::TYPE,
            next: ptr::null(),
            color_scale: xr::Color4f {
                r: bright,
                g: bright,
                b: bright,
                a: alpha,
            },
            color_bias: xr::Color4f {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        }),
        _ => None,
    }));
    let mut pointer_dims = pointer_dims.iter();
    let (quads, cylinders) = (&mut buffers.quads, &mut buffers.cylinders);
    quads.clear();
    cylinders.clear();
    let made = &mut buffers.made;
    made.clear();
    let mut dims = dims.iter();
    for layer in plan.layers() {
        let m = match layer {
            Layer::Background => Made::Background,
            Layer::KeyboardWindow => Made::KeyboardWindow,
            Layer::Hands => Made::Hands,
            Layer::PcCursor {
                pc, shape, quad, ..
            } => {
                let dim = pointer_dims.next();
                let picture = pictures.pc_cursors.iter().find(|p| p.shows(*pc, *shape));
                match (picture, dim) {
                    (Some(picture), Some(dim)) => match picture.layer.as_ref() {
                        Some(layer) => {
                            let Quad { at, height } = *quad;
                            let mut q = layer.quad(local, posef(&at), at.width, height);
                            let scale = dim.color_scale;
                            if exts.khr_composition_layer_color_scale_bias
                                && (scale.r < 1.0 || scale.a < 1.0)
                            {
                                q.next = ptr::from_ref(dim).cast();
                            }
                            quads.push(q);
                            Made::Quad(quads.len() - 1)
                        }
                        None => Made::Nothing,
                    },
                    _ => Made::Nothing,
                }
            }
            Layer::Monitor(m) => match dims.next() {
                Some(dim) => monitor(
                    m,
                    dim,
                    video_next(m.quality),
                    MonitorLayers {
                        exts,
                        video: &shell.video,
                        local,
                        quads,
                        cylinders,
                    },
                ),
                None => Made::Nothing,
            },
            _ => match picture_of(pictures, layer) {
                Some((picture, Quad { at, height })) => {
                    quads.push(picture.quad(local, posef(&at), at.width, height));
                    Made::Quad(quads.len() - 1)
                }
                None => Made::Nothing,
            },
        };
        made.push(m);
    }

    // The list in the plan's order, each from where it was made.
    let mut layers = reuse(std::mem::take(&mut buffers.layers));
    for m in made.iter() {
        layers.extend(match *m {
            Made::Quad(i) => quads.get(i).map(as_layer),
            Made::Cylinder(i) => cylinders.get(i).map(as_layer),
            Made::Background => background,
            Made::KeyboardWindow => keyboard_layer.as_ref().map(as_layer),
            Made::Hands => hands_layer.as_ref().map(as_layer),
            Made::Nothing => None,
        });
    }
    let ended = stage.stream.end(time, shell.room.blend_mode(), &layers);
    buffers.layers = reuse(layers);
    match ended {
        // One frame the compositor refused is not worth closing the app over.
        Err(
            e @ (xr::sys::Result::ERROR_POSE_INVALID
            | xr::sys::Result::ERROR_LAYER_INVALID
            | xr::sys::Result::ERROR_SWAPCHAIN_RECT_INVALID),
        ) => {
            warn!("frame refused: {e}");
        }
        other => other?,
    }
    // The person hears when monitors do not fit, or fit again.
    let wanted = buffers.plan.wanted();
    if let Some(hidden) = buffers.plan.news() {
        shell.handle(now, Event::Platform(PlatformEvent::LayersHidden(hidden)));
    }
    Ok(wanted)
}

/// Where the monitors' layers are made.
struct MonitorLayers<'a> {
    exts: &'a xr::ExtensionSet,
    video: &'a SurfaceVideo,
    local: &'a xr::Space,
    quads: &'a mut Vec<xr::sys::CompositionLayerQuad>,
    cylinders: &'a mut Vec<xr::sys::CompositionLayerCylinderKHR>,
}

/// Monitor `m`'s layer, its colour scale `dim`, its stream's picture after
/// `video_next`, made in `to`.
fn monitor(
    m: &layers::Monitor,
    dim: &xr::sys::CompositionLayerColorScaleBiasKHR,
    video_next: *const std::ffi::c_void,
    to: MonitorLayers<'_>,
) -> Made {
    let MonitorLayers {
        exts,
        video,
        local,
        quads,
        cylinders,
    } = to;
    let scale = dim.color_scale;
    let fading = exts.khr_composition_layer_color_scale_bias && scale.a < 1.0;
    let layer_flags = if fading {
        xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA
    } else {
        xr::CompositionLayerFlags::EMPTY
    };
    let next = if exts.khr_composition_layer_color_scale_bias && scale.r < 1.0 {
        ptr::from_ref(dim).cast()
    } else {
        video_next
    };
    // Planned only with a picture, which stays until the next pass.
    let Some(image) = video.image(m.id) else {
        return Made::Nothing;
    };
    let sub_image = xr::sys::SwapchainSubImage {
        swapchain: image.swapchain.as_raw(),
        image_rect: xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di {
                width: image.width as i32,
                height: image.height as i32,
            },
        },
        image_array_index: 0,
    };
    let Quad { at: p, height } = m.quad;
    if let Some(radius) = m.curve {
        let c = Cylinder::new(&p, radius);
        cylinders.push(xr::sys::CompositionLayerCylinderKHR {
            ty: xr::sys::CompositionLayerCylinderKHR::TYPE,
            next,
            layer_flags,
            space: local.as_raw(),
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image,
            pose: pose(c.center, p.rotation),
            radius: c.radius,
            central_angle: c.central_angle,
            aspect_ratio: c.arc_length() / height,
        });
        Made::Cylinder(cylinders.len() - 1)
    } else {
        quads.push(xr::sys::CompositionLayerQuad {
            ty: xr::sys::CompositionLayerQuad::TYPE,
            next,
            layer_flags,
            space: local.as_raw(),
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image,
            pose: posef(&p),
            size: xr::Extent2Df {
                width: p.width,
                height,
            },
        });
        Made::Quad(quads.len() - 1)
    }
}
