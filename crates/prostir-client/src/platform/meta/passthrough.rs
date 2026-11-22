//! XR_FB_passthrough driven through the raw entry points: the whole room as a
//! background, and a window onto the real keyboard (the camera image projected onto a
//! rectangle lying where the keyboard is, via XR_FB_triangle_mesh).
//!
//! Hiding a passthrough layer keeps the cameras and the reconstruction pipeline running
//! and the battery draining, so the feature itself is paused when neither is shown.
//! (`openxr::Passthrough::start` calls xrPassthroughPauseFB, hence the raw calls.)

use std::ptr;

use anyhow::{Context, Result, bail};
use openxr as xr;
use openxr::sys::Handle as _;

pub struct Passthrough {
    fp: xr::raw::PassthroughFB,
    mesh_fp: Option<xr::raw::TriangleMeshFB>,
    session: xr::sys::Session,
    feature: xr::sys::PassthroughFB,
    /// The whole room (the "room" background).
    room: xr::sys::PassthroughLayerFB,
    window: Option<Window>,
    cameras: bool,
    room_shown: bool,
}

/// The keyboard window: a projected layer with one rectangle in it.
struct Window {
    layer: xr::sys::PassthroughLayerFB,
    mesh: xr::sys::TriangleMeshFB,
    instance: xr::sys::GeometryInstanceFB,
    shown: bool,
}

fn check(result: xr::sys::Result, what: &str) -> Result<()> {
    if result.into_raw() < 0 {
        bail!("{what} failed: {result:?}");
    }
    Ok(())
}

impl Passthrough {
    pub fn new<G: xr::Graphics>(session: &xr::Session<G>, room: bool) -> Result<Self> {
        let exts = session.instance().exts();
        let fp = *exts
            .fb_passthrough
            .as_ref()
            .context("XR_FB_passthrough is not enabled")?;
        let flags = if room {
            xr::PassthroughFlagsFB::IS_RUNNING_AT_CREATION
        } else {
            xr::PassthroughFlagsFB::EMPTY
        };

        let info = xr::sys::PassthroughCreateInfoFB {
            ty: xr::sys::PassthroughCreateInfoFB::TYPE,
            next: ptr::null(),
            flags,
        };
        let mut feature = xr::sys::PassthroughFB::NULL;
        // SAFETY: valid session, create info and out-pointer.
        let created =
            unsafe { (fp.create_passthrough)(session.as_raw(), &raw const info, &raw mut feature) };
        check(created, "xrCreatePassthroughFB")?;
        let mut this = Self {
            fp,
            mesh_fp: exts.fb_triangle_mesh,
            session: session.as_raw(),
            feature,
            room: xr::sys::PassthroughLayerFB::NULL,
            window: None,
            cameras: room,
            room_shown: room,
        };
        // On failure, `Drop` destroys what was created.
        this.room = this.create_layer(xr::PassthroughLayerPurposeFB::RECONSTRUCTION, flags)?;
        Ok(this)
    }

    fn create_layer(
        &self,
        purpose: xr::PassthroughLayerPurposeFB,
        flags: xr::PassthroughFlagsFB,
    ) -> Result<xr::sys::PassthroughLayerFB> {
        let info = xr::sys::PassthroughLayerCreateInfoFB {
            ty: xr::sys::PassthroughLayerCreateInfoFB::TYPE,
            next: ptr::null(),
            passthrough: self.feature,
            flags,
            purpose,
        };
        let mut layer = xr::sys::PassthroughLayerFB::NULL;
        // SAFETY: `feature` belongs to this session.
        let created = unsafe {
            (self.fp.create_passthrough_layer)(self.session, &raw const info, &raw mut layer)
        };
        check(created, "xrCreatePassthroughLayerFB")?;
        Ok(layer)
    }

    /// Whether the keyboard window can be shown (the runtime projects passthrough onto
    /// meshes).
    pub fn window_supported(&self) -> bool {
        self.mesh_fp.is_some()
    }

    /// The room layer, while it is shown.
    pub fn room_layer(&self) -> Option<xr::sys::PassthroughLayerFB> {
        self.room_shown.then_some(self.room)
    }

    /// The keyboard window layer, while it is shown.
    pub fn window_layer(&self) -> Option<xr::sys::PassthroughLayerFB> {
        self.window.as_ref().filter(|w| w.shown).map(|w| w.layer)
    }

    /// Shows or hides the room background.
    pub fn set_room(&mut self, on: bool) -> Result<()> {
        if on == self.room_shown {
            return Ok(());
        }
        if on {
            self.set_cameras(true)?;
        }
        // SAFETY: the layer is alive for the lifetime of `self`.
        unsafe {
            if on {
                check(
                    (self.fp.passthrough_layer_resume)(self.room),
                    "xrPassthroughLayerResumeFB",
                )?;
            } else {
                check(
                    (self.fp.passthrough_layer_pause)(self.room),
                    "xrPassthroughLayerPauseFB",
                )?;
            }
        }
        self.room_shown = on;
        self.update_cameras()
    }

    /// How much of the room shows through (1: all; less darkens it towards black).
    pub fn set_room_opacity(&mut self, opacity: f32) -> Result<()> {
        let style = xr::sys::PassthroughStyleFB {
            ty: xr::sys::PassthroughStyleFB::TYPE,
            next: ptr::null(),
            texture_opacity_factor: opacity.clamp(0.0, 1.0),
            edge_color: xr::Color4f {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        };
        // SAFETY: the layer is alive for the lifetime of `self`.
        check(
            unsafe { (self.fp.passthrough_layer_set_style)(self.room, &raw const style) },
            "xrPassthroughLayerSetStyleFB",
        )
    }

    /// Shows the keyboard window at `pose` (a rectangle `width` × `depth` metres, lying
    /// in the pose's XY plane), or hides it with `None`.
    pub fn set_window(
        &mut self,
        space: &xr::Space,
        time: xr::Time,
        place: Option<(xr::Posef, f32, f32)>,
    ) -> Result<()> {
        let Some((pose, width, depth)) = place else {
            if let Some(w) = self.window.as_mut().filter(|w| w.shown) {
                // SAFETY: the layer is alive for the lifetime of `self`.
                let paused = unsafe { (self.fp.passthrough_layer_pause)(w.layer) };
                check(paused, "xrPassthroughLayerPauseFB")?;
                w.shown = false;
            }
            return self.update_cameras();
        };
        let scale = xr::Vector3f {
            x: width,
            y: depth,
            z: 1.0,
        };
        if self.window.is_none() {
            self.window = Some(self.create_window(space, pose, scale)?);
        }
        self.set_cameras(true)?;
        let w = self.window.as_mut().expect("created above");
        if !w.shown {
            // SAFETY: the layer is alive for the lifetime of `self`.
            let resumed = unsafe { (self.fp.passthrough_layer_resume)(w.layer) };
            check(resumed, "xrPassthroughLayerResumeFB")?;
            w.shown = true;
        }
        let transform = xr::sys::GeometryInstanceTransformFB {
            ty: xr::sys::GeometryInstanceTransformFB::TYPE,
            next: ptr::null(),
            base_space: space.as_raw(),
            time,
            pose,
            scale,
        };
        // SAFETY: the instance is alive; the transform outlives the call.
        check(
            unsafe { (self.fp.geometry_instance_set_transform)(w.instance, &raw const transform) },
            "xrGeometryInstanceSetTransformFB",
        )
    }

    fn create_window(
        &self,
        space: &xr::Space,
        pose: xr::Posef,
        scale: xr::Vector3f,
    ) -> Result<Window> {
        let mesh_fp = self
            .mesh_fp
            .as_ref()
            .context("XR_FB_triangle_mesh is not enabled")?;
        let layer = self.create_layer(
            xr::PassthroughLayerPurposeFB::PROJECTED,
            xr::PassthroughFlagsFB::EMPTY,
        )?;
        // A unit rectangle, scaled by the instance.
        let vertices = [
            xr::Vector3f {
                x: -0.5,
                y: -0.5,
                z: 0.0,
            },
            xr::Vector3f {
                x: 0.5,
                y: -0.5,
                z: 0.0,
            },
            xr::Vector3f {
                x: 0.5,
                y: 0.5,
                z: 0.0,
            },
            xr::Vector3f {
                x: -0.5,
                y: 0.5,
                z: 0.0,
            },
        ];
        let indices: [u32; 6] = [0, 1, 2, 2, 3, 0];
        let info = xr::sys::TriangleMeshCreateInfoFB {
            ty: xr::sys::TriangleMeshCreateInfoFB::TYPE,
            next: ptr::null(),
            flags: xr::TriangleMeshFlagsFB::EMPTY,
            winding_order: xr::WindingOrderFB::CCW,
            vertex_count: vertices.len() as u32,
            vertex_buffer: vertices.as_ptr(),
            triangle_count: 2,
            index_buffer: indices.as_ptr(),
        };
        let mut mesh = xr::sys::TriangleMeshFB::NULL;
        // SAFETY: the buffers outlive the call; the runtime copies them.
        let made = check(
            unsafe { (mesh_fp.create_triangle_mesh)(self.session, &raw const info, &raw mut mesh) },
            "xrCreateTriangleMeshFB",
        );
        if let Err(e) = made {
            // SAFETY: destroying the layer created above, never used again.
            unsafe { (self.fp.destroy_passthrough_layer)(layer) };
            return Err(e);
        }
        let info = xr::sys::GeometryInstanceCreateInfoFB {
            ty: xr::sys::GeometryInstanceCreateInfoFB::TYPE,
            next: ptr::null(),
            layer,
            mesh,
            base_space: space.as_raw(),
            pose,
            scale,
        };
        let mut instance = xr::sys::GeometryInstanceFB::NULL;
        // SAFETY: layer, mesh and space are alive.
        let made = check(
            unsafe {
                (self.fp.create_geometry_instance)(self.session, &raw const info, &raw mut instance)
            },
            "xrCreateGeometryInstanceFB",
        );
        if let Err(e) = made {
            // SAFETY: destroying the handles created above, never used again.
            unsafe {
                (mesh_fp.destroy_triangle_mesh)(mesh);
                (self.fp.destroy_passthrough_layer)(layer);
            }
            return Err(e);
        }
        Ok(Window {
            layer,
            mesh,
            instance,
            shown: false,
        })
    }

    /// Cameras run only while some passthrough is shown.
    fn update_cameras(&mut self) -> Result<()> {
        let needed = self.room_shown || self.window.as_ref().is_some_and(|w| w.shown);
        self.set_cameras(needed)
    }

    fn set_cameras(&mut self, on: bool) -> Result<()> {
        if on == self.cameras {
            return Ok(());
        }
        // SAFETY: the feature is alive for the lifetime of `self`.
        unsafe {
            if on {
                check(
                    (self.fp.passthrough_start)(self.feature),
                    "xrPassthroughStartFB",
                )?;
            } else {
                check(
                    (self.fp.passthrough_pause)(self.feature),
                    "xrPassthroughPauseFB",
                )?;
            }
        }
        self.cameras = on;
        Ok(())
    }
}

impl Drop for Passthrough {
    fn drop(&mut self) {
        // SAFETY: handles are owned by `self` and destroyed exactly once, children first.
        unsafe {
            if let Some(w) = &self.window {
                (self.fp.destroy_geometry_instance)(w.instance);
                if let Some(m) = &self.mesh_fp {
                    (m.destroy_triangle_mesh)(w.mesh);
                }
                (self.fp.destroy_passthrough_layer)(w.layer);
            }
            if self.room != xr::sys::PassthroughLayerFB::NULL {
                (self.fp.destroy_passthrough_layer)(self.room);
            }
            (self.fp.destroy_passthrough)(self.feature);
        }
    }
}
