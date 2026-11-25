//! The frame's layers, back to front, as OpenXR takes them, and the frame's end:
//! the room or the panorama, the window onto the keyboard, the monitors, what is
//! wrong with their pictures, the placeholder, the menu's button, the menu, the
//! toast, "Attach", the palm menu, the hands and rays, the pointer dots.

use std::ptr;
use std::time::Instant;

use anyhow::Result;
use glam::{Quat, Vec3};
use log::warn;
use openxr as xr;
use openxr::sys::Handle as _;

use super::Kept;
use super::pictures::TOAST_PX;
use super::shell::Shell;
use super::start::{Stage, Xr};
use crate::app::{Event, PlatformEvent, TOAST_WIDTH};
use crate::ids::Target;
use crate::layout::{self, Cylinder};
use crate::platform::meta::{self, passthrough};
use crate::platform::openxr::overlay::{self, Sphere};
use crate::platform::openxr::{self as xrutil, Gfx, as_layer, pose, posef};
use crate::world::{
    ATTACH_WIDTH, BUTTON_SIZE, KEYBOARD_ASPECT, WRIST_MENU_WIDTH, attach_height, panel_height,
    panel_width, wrist_menu_height,
};

/// The side of the black layer over a dimmed room, in metres, a metre in front of
/// the eyes: wider than any field of view.
const DIM_SIZE: f32 = 40.0;

/// Buffers of a frame's layers, kept from frame to frame and filled again.
#[derive(Default)]
pub struct Buffers {
    /// Where the eyes are, for the hands and rays.
    views: [xr::View; 2],
    pub spheres: Vec<Sphere>,
    dims: Vec<xr::sys::CompositionLayerColorScaleBiasKHR>,
    quads: Vec<xr::sys::CompositionLayerQuad>,
    cylinders: Vec<xr::sys::CompositionLayerCylinderKHR>,
    cursors: Vec<xr::sys::CompositionLayerQuad>,
    /// What is wrong with a monitor's picture, over it or in its place.
    troubles: Vec<xr::sys::CompositionLayerQuad>,
    /// The layers' list for `end`: its memory only, empty between frames (see
    /// [`reuse`]).
    layers: Vec<&'static xr::CompositionLayerBase<'static, Gfx>>,
}

/// An empty list of references in the memory of `v` (emptied): a list kept from frame
/// to frame whose items borrow from one frame only. Collected into the same layout,
/// nothing is allocated.
fn reuse<'a, 'b, T, U: 'b>(mut v: Vec<&'a T>) -> Vec<&'b U> {
    v.clear();
    v.into_iter().filter_map(|_| None).collect()
}

/// The hands and rays drawn (`kept.buffers.spheres`, filled), the layers made and the
/// frame at `time` ended: how many layers it wanted.
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
    let spheres = &mut buffers.spheres;
    let views = (!spheres.is_empty()
        && xrutil::locate_views(session, time, local, &mut buffers.views))
    .then_some(&buffers.views[..]);
    if let Some(ov) = &mut stage.overlay {
        ov.render(gl, views.unwrap_or(&[]), spheres)?;
    }
    let projection_views = stage
        .overlay
        .as_ref()
        .zip(views)
        .and_then(|(o, v)| o.layer_views(v));
    let hands_layer = projection_views
        .as_ref()
        .map(|v| overlay::projection_layer(local, v));

    // ---- Layers, back to front ----
    // Supersampled and sharpened, and the stream's picture the right way up, where
    // the runtime can.
    let quality = meta::sharper();
    let quality_next: *const std::ffi::c_void = if exts.fb_composition_layer_settings {
        (&raw const quality).cast()
    } else {
        ptr::null()
    };
    let flip = meta::flipped(quality_next);
    let video_next = if exts.fb_composition_layer_image_layout {
        (&raw const flip).cast()
    } else {
        quality_next
    };

    // The keyboard window: placed in front of the user at desk height the first
    // time, then wherever it was moved.
    let mut keyboard_failed = false;
    if let Some(pt) = shell.room.fb().filter(|pt| pt.window_supported()) {
        let shown = shell
            .model
            .keyboard_window()
            .map(|p| (posef(&p), p.width, p.width * KEYBOARD_ASPECT));
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
        super::Gpu {
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
    let world = model.world();
    let settings = *model.settings();
    let pano = pictures
        .pano_layer
        .as_ref()
        .filter(|(b, _)| *b == model.settings().background)
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

    // Far monitors first so nearer (and grabbed) ones draw on top.
    let curve = world.curve_radius(&settings);
    let order = world.draw_list();
    let dims = &mut buffers.dims;
    dims.clear();
    dims.extend(order.iter().map(|(_, id)| {
        // Fading in: premultiplied, so colour and alpha scale together.
        let t = world.appear(*id);
        let t = t * t * (3.0 - 2.0 * t);
        // Focus dimming, the brightness setting and fading in, together.
        let b = world.brightness(*id) * t * f32::from(settings.brightness) / 100.0;
        xr::sys::CompositionLayerColorScaleBiasKHR {
            ty: xr::sys::CompositionLayerColorScaleBiasKHR::TYPE,
            next: video_next,
            color_scale: xr::Color4f {
                r: b,
                g: b,
                b,
                a: t,
            },
            color_bias: xr::Color4f {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        }
    }));
    let (quads, cylinders) = (&mut buffers.quads, &mut buffers.cylinders);
    quads.clear();
    cylinders.clear();
    for (dim, (_, id)) in dims.iter().zip(order) {
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
        let (Some(m), Some(p)) = (shell.video.image(*id), world.shown(*id)) else {
            continue;
        };
        let height = p.width * m.height as f32 / m.width.max(1) as f32;
        let sub_image = xr::sys::SwapchainSubImage {
            swapchain: m.swapchain.as_raw(),
            image_rect: xr::Rect2Di {
                offset: xr::Offset2Di { x: 0, y: 0 },
                extent: xr::Extent2Di {
                    width: m.width as i32,
                    height: m.height as i32,
                },
            },
            image_array_index: 0,
        };
        // Window panels stay flat.
        if let Some(radius) = curve.filter(|_| !world.is_window(*id)) {
            let c = Cylinder::new(p, radius);
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
        } else {
            quads.push(xr::sys::CompositionLayerQuad {
                ty: xr::sys::CompositionLayerQuad::TYPE,
                next,
                layer_flags,
                space: local.as_raw(),
                eye_visibility: xr::EyeVisibility::BOTH,
                sub_image,
                pose: posef(p),
                size: xr::Extent2Df {
                    width: p.width,
                    height,
                },
            });
        }
    }

    let button = world.button().map(|b| {
        pictures
            .button_layer
            .quad(local, posef(&b), BUTTON_SIZE, BUTTON_SIZE)
    });
    let panel_quad = (model.menu().open && pictures.panel_layer.ready()).then(|| {
        pictures.panel_layer.quad(
            local,
            posef(&world.menu()),
            panel_width(settings.large_menu),
            panel_height(settings.large_menu),
        )
    });
    let cursors = &mut buffers.cursors;
    cursors.clear();
    cursors.extend(hands.picks().iter().flatten().map(|&Target { hit, .. }| {
        let size = 0.011 * hit.t.clamp(0.3, 4.0);
        let at = hit.point + hit.normal * 0.004;
        pictures.cursor_layer.quad(
            local,
            pose(at, layout::facing(at, at + hit.normal)),
            size,
            size,
        )
    }));
    // Under a fingertip near the menu: a dot that shrinks as the finger gets closer.
    for (_, at, z, _) in hands.pokes() {
        let place = &world.menu();
        let normal = place.rotation * Vec3::Z;
        let point = place.position + place.rotation * Vec3::new(at.x, at.y, 0.0) + normal * 0.002;
        let size = 0.006 + z.max(0.0) * 0.15;
        cursors.push(pictures.cursor_layer.quad(
            local,
            pose(point, layout::facing(point, point + normal)),
            size,
            size,
        ));
    }

    let mut layers = reuse(std::mem::take(&mut buffers.layers));
    if let Some(l) = &passthrough_layer {
        layers.push(as_layer(l));
    } else if let Some(l) = &htc_layer {
        layers.push(as_layer(l));
    } else if let Some(l) = &dim_quad {
        layers.push(as_layer(l));
    } else if let Some(l) = pano.as_ref().filter(|_| !shell.room.shown()) {
        layers.push(as_layer(l));
    }
    layers.extend(keyboard_layer.iter().map(as_layer));
    layers.extend(cylinders.iter().map(as_layer));
    layers.extend(quads.iter().map(as_layer));
    let troubles = &mut buffers.troubles;
    troubles.clear();
    for (_, id) in order {
        if let (Some((_, _, l)), Some(p)) = (pictures.trouble_layers.get(id), world.shown(*id)) {
            troubles.push(l.quad(local, posef(p), p.width, world.height_of(*id)));
        }
    }
    layers.extend(troubles.iter().map(as_layer));
    let placeholder_quad = world.adding().and_then(|a| {
        let (layer, _, _) = pictures.placeholder.as_ref()?;
        Some(layer.quad(local, posef(&a.place), a.place.width, a.height()))
    });
    layers.extend(placeholder_quad.iter().map(as_layer));
    layers.extend(button.iter().map(as_layer));
    layers.extend(panel_quad.iter().map(as_layer));
    let toast_quad = model.toast().and_then(|t| t.place).map(|p| {
        pictures.toast_layer.quad(
            local,
            posef(&p),
            TOAST_WIDTH,
            TOAST_WIDTH * TOAST_PX.1 as f32 / TOAST_PX.0 as f32,
        )
    });
    layers.extend(toast_quad.iter().map(as_layer));
    let attach_quad = hands
        .attach_for()
        .filter(|_| pictures.attach_drawn.is_some())
        .and_then(|id| world.attach_place(id))
        .map(|p| {
            pictures
                .attach_layer
                .quad(local, posef(&p), ATTACH_WIDTH, attach_height())
        });
    layers.extend(attach_quad.iter().map(as_layer));
    let wrist_quad = world
        .wrist()
        .filter(|_| pictures.wrist_layer.ready())
        .map(|w| {
            pictures
                .wrist_layer
                .quad(local, posef(&w), WRIST_MENU_WIDTH, wrist_menu_height())
        });
    layers.extend(wrist_quad.iter().map(as_layer));
    layers.extend(hands_layer.iter().map(as_layer));
    layers.extend(cursors.iter().map(as_layer));
    let layers_wanted = layers.len();
    // As many layers as the compositor takes.
    layers.truncate(stage.layers_max);
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
    Ok(layers_wanted)
}
