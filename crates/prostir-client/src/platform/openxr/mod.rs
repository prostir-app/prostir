//! Standard OpenXR and its `KHR` and `EXT` extensions, which any runtime may have: the
//! extensions turned on, the controllers' and hands' input ([`input`], by the table of
//! `platform::controllers`), the images the app draws itself and their layers, the
//! hands and rays ([`overlay`]), the surfaces the streams are decoded into, the
//! performance levels. The maker's extensions are `platform::meta`.

pub mod egl;
pub mod input;
pub mod overlay;

use std::num::NonZeroU32;
use std::ptr;

use anyhow::{Context, Result, bail};
use glam::{Quat, Vec3};
use glow::HasContext;
use log::{info, warn};
use ndk::native_window::NativeWindow;
use openxr as xr;
use openxr::sys::Handle as _;

use crate::layout::Placement;
use crate::platform::controllers::Needs;
use crate::ui::Area;

/// The extensions the app turns on: those it cannot do without, and (unless
/// `minimal`, which checks the app on a headset without any other) every optional one
/// the runtime has, the maker's among them (`platform::meta`).
pub fn extensions(available: &xr::ExtensionSet, minimal: bool) -> xr::ExtensionSet {
    let mut on = xr::ExtensionSet::default();
    on.khr_android_create_instance = true;
    on.khr_opengl_es_enable = true;
    on.khr_android_surface_swapchain = true;
    if minimal {
        return on;
    }
    on.khr_composition_layer_cylinder = available.khr_composition_layer_cylinder;
    on.khr_composition_layer_equirect2 = available.khr_composition_layer_equirect2;
    on.khr_composition_layer_color_scale_bias = available.khr_composition_layer_color_scale_bias;
    on.khr_android_thread_settings = available.khr_android_thread_settings;
    on.ext_hand_tracking = available.ext_hand_tracking;
    on.ext_hand_interaction = available.ext_hand_interaction;
    // Taken off, said at once by the headset's sensor (the session may stay shown
    // until the system sleeps).
    on.ext_user_presence = available.ext_user_presence;
    on.ext_performance_settings = available.ext_performance_settings;
    // The controllers of the profiles in the table.
    on.bd_controller_interaction = available.bd_controller_interaction;
    on.htc_vive_focus3_controller_interaction = available.htc_vive_focus3_controller_interaction;
    crate::platform::meta::extensions(available, &mut on);
    crate::platform::htc::extensions(available, &mut on);
    // A controller's model by its own module, where the runtime has no
    // `XR_FB_render_model`.
    if !on.fb_render_model {
        crate::platform::msft::extensions(available, &mut on);
    }
    on
}

/// Whether the extension a profile of the controllers' table needs is on.
pub fn has(on: &xr::ExtensionSet, needs: Needs) -> bool {
    match needs {
        Needs::Core => true,
        Needs::HandInteraction => on.ext_hand_interaction,
        Needs::BdControllers => on.bd_controller_interaction,
        Needs::HtcFocus3 => on.htc_vive_focus3_controller_interaction,
    }
}

/// A performance level for the runtime to hold the processor and the graphics at
/// (`XR_EXT_performance_settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfLevel {
    /// Steady and low: the battery first.
    Low,
    /// Steady and high, within what the headset keeps up without warming too much.
    High,
}

/// Asks the runtime for `level` on the processor and on the graphics; whether it took
/// it (without the extension, never).
pub fn set_perf_level(session: &xr::Session<Gfx>, level: PerfLevel) -> bool {
    let Some(ext) = session.instance().exts().ext_performance_settings.as_ref() else {
        return false;
    };
    let level_raw = match level {
        PerfLevel::Low => xr::sys::PerfSettingsLevelEXT::SUSTAINED_LOW,
        PerfLevel::High => xr::sys::PerfSettingsLevelEXT::SUSTAINED_HIGH,
    };
    let mut took = true;
    for domain in [
        xr::sys::PerfSettingsDomainEXT::CPU,
        xr::sys::PerfSettingsDomainEXT::GPU,
    ] {
        // SAFETY: a live session of the instance the extension's function is from.
        let result = unsafe {
            (ext.perf_settings_set_performance_level)(session.as_raw(), domain, level_raw)
        };
        if result.into_raw() < 0 {
            warn!("performance level {level:?} for {domain:?}: {result:?}");
            took = false;
        }
    }
    if took {
        info!("performance level {level:?}");
    }
    took
}

pub type Gfx = xr::OpenGlEs;

const GL_RGBA8: u32 = 0x8058;
const GL_SRGB8_ALPHA8: u32 = 0x8C43;

/// Views a raw `XrCompositionLayer*` struct as the base type `FrameStream::end` takes.
pub fn as_layer<T>(layer: &T) -> &xr::CompositionLayerBase<'_, Gfx> {
    // SAFETY: only called with XrCompositionLayer* structs, which all begin with the
    // common layer header; CompositionLayerBase is a transparent wrapper over it.
    unsafe { &*ptr::from_ref::<T>(layer).cast() }
}

pub fn vec3(v: xr::Vector3f) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

pub fn quat(q: xr::Quaternionf) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// A pose the compositor accepts: a unit rotation and a finite position. A broken one
/// (from a bad hand-tracking frame, say) would make it reject the whole frame and end
/// the session, so it is repaired and reported instead.
pub fn pose(position: Vec3, rotation: Quat) -> xr::Posef {
    let length = rotation.length_squared();
    let rotation = if rotation.is_finite() && (length - 1.0).abs() < 1e-3 {
        rotation
    } else if rotation.is_finite() && length > 1e-6 {
        rotation.normalize()
    } else {
        report_bad_pose(position, rotation);
        Quat::IDENTITY
    };
    let position = if position.is_finite() {
        position
    } else {
        report_bad_pose(position, rotation);
        Vec3::ZERO
    };
    xr::Posef {
        orientation: xr::Quaternionf {
            x: rotation.x,
            y: rotation.y,
            z: rotation.z,
            w: rotation.w,
        },
        position: xr::Vector3f {
            x: position.x,
            y: position.y,
            z: position.z,
        },
    }
}

/// Logs a broken pose, a few times only (it may come every frame).
fn report_bad_pose(position: Vec3, rotation: Quat) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static TOLD: AtomicU32 = AtomicU32::new(0);
    if TOLD.fetch_add(1, Ordering::Relaxed) < 5 {
        log::warn!("broken layer pose repaired: position {position:?}, rotation {rotation:?}");
    }
}

pub fn posef(p: &Placement) -> xr::Posef {
    pose(p.position, p.rotation)
}

/// A swapchain whose Android Surface MediaCodec decodes into.
pub fn create_surface_swapchain(
    session: &xr::Session<Gfx>,
    vm: &jni::JavaVM,
    width: u32,
    height: u32,
) -> Result<(xr::Swapchain<Gfx>, NativeWindow)> {
    let ext = session
        .instance()
        .exts()
        .khr_android_surface_swapchain
        .as_ref()
        .context("XR_KHR_android_surface_swapchain is not enabled")?;
    // No format and no counts: MediaCodec decides those, and a runtime may refuse a
    // surface swapchain that names them ("info format validation failure").
    let info = xr::sys::SwapchainCreateInfo {
        ty: xr::sys::SwapchainCreateInfo::TYPE,
        next: ptr::null(),
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::SAMPLED | xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
        format: 0,
        sample_count: 0,
        width,
        height,
        face_count: 0,
        array_size: 0,
        mip_count: 0,
    };
    // In a local frame of its own: the spec does not say whether the Surface the
    // runtime returns is a local or a global JNI reference. A local one goes with the
    // frame (this thread stays attached for the app's life, so it would otherwise be
    // kept until the app ends, one for each stream started); the window took its own
    // reference to the Surface by then.
    vm.attach_current_thread(|env| -> Result<_> {
        let mut handle = xr::sys::Swapchain::NULL;
        let mut surface = ptr::null_mut();
        // SAFETY: valid session and create info; out-pointers live on the stack.
        let result = unsafe {
            (ext.create_swapchain_android_surface)(
                session.as_raw(),
                &raw const info,
                &raw mut handle,
                &raw mut surface,
            )
        };
        if result != xr::sys::Result::SUCCESS {
            bail!("xrCreateSwapchainAndroidSurfaceKHR failed: {result:?}");
        }
        // SAFETY: the handle was just created for this session and is owned by the
        // wrapper.
        let swapchain = unsafe { xr::Swapchain::from_raw(session.clone(), handle) };
        // SAFETY: `surface` is a valid android.view.Surface reference returned by the
        // runtime, and `env` is this thread's.
        let window = unsafe { NativeWindow::from_surface(env.get_raw().cast(), surface.cast()) }
            .context("ANativeWindow_fromSurface failed")?;
        Ok((swapchain, window))
    })
}

/// GL function loader for the EGL context the session was created with.
pub fn load_gl(egl: &khronos_egl::Instance<khronos_egl::Static>) -> glow::Context {
    // SAFETY: the EGL context is current on this thread for the app's lifetime.
    unsafe {
        glow::Context::from_loader_function(|name| {
            egl.get_proc_address(name)
                .map_or(ptr::null(), |f| f as *const std::ffi::c_void)
        })
    }
}

/// Picks sRGB storage when the runtime offers it, so CPU-drawn sRGB pixels show right.
pub fn color_format(session: &xr::Session<Gfx>) -> u32 {
    let formats = session.enumerate_swapchain_formats().unwrap_or_default();
    if formats.contains(&GL_SRGB8_ALPHA8) {
        GL_SRGB8_ALPHA8
    } else {
        GL_RGBA8
    }
}

/// A CPU-drawn image (menu, cursor, panorama) in a GL swapchain.
pub struct ImageLayer {
    swapchain: xr::Swapchain<Gfx>,
    images: Vec<u32>,
    pub width: u32,
    pub height: u32,
    is_static: bool,
    uploaded: bool,
    /// The pixels bottom row first, as GL takes them: kept between uploads of an image
    /// that changes, so turning it over allocates nothing after the first.
    flipped: Vec<u8>,
    /// For each image of the swapchain, what changed since it was last written: an
    /// image that was shown while a part changed gets that part too.
    stale: Vec<Area>,
}

impl ImageLayer {
    /// `is_static` images are uploaded once and never change.
    pub fn new(
        session: &xr::Session<Gfx>,
        format: u32,
        width: u32,
        height: u32,
        is_static: bool,
    ) -> Result<Self> {
        let swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
            create_flags: if is_static {
                xr::SwapchainCreateFlags::STATIC_IMAGE
            } else {
                xr::SwapchainCreateFlags::EMPTY
            },
            usage_flags: xr::SwapchainUsageFlags::SAMPLED
                | xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                | xr::SwapchainUsageFlags::TRANSFER_DST,
            format,
            sample_count: 1,
            width,
            height,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        })?;
        let images = swapchain.enumerate_images()?;
        let whole = Area {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        Ok(Self {
            stale: vec![whole; images.len()],
            swapchain,
            images,
            width,
            height,
            is_static,
            uploaded: false,
            flipped: Vec::new(),
        })
    }

    /// Whether the compositor has something to show.
    pub fn ready(&self) -> bool {
        self.uploaded
    }

    /// Uploads RGBA8 pixels given top row first (GL images are stored bottom row first).
    pub fn upload(&mut self, gl: &glow::Context, rgba: &[u8]) -> Result<()> {
        if !self.is_static {
            return self.upload_part(gl, rgba, None);
        }
        if self.uploaded {
            return Ok(());
        }
        let row = self.width as usize * 4;
        if rgba.len() != row * self.height as usize {
            bail!(
                "image is {} bytes, expected {}",
                rgba.len(),
                row * self.height as usize
            );
        }
        #[cfg(all(feature = "selftest", not(feature = "store")))]
        let began = crate::selftest::upload_started();
        // Row by row (each a plain copy) into the buffer kept from the last upload,
        // which stays the layer's whatever fails below.
        self.flipped.clear();
        self.flipped.reserve_exact(rgba.len());
        for line in rgba.chunks_exact(row).rev() {
            self.flipped.extend_from_slice(line);
        }
        let index = self.swapchain.acquire_image()? as usize;
        self.swapchain.wait_image(xr::Duration::INFINITE)?;
        let texture = NonZeroU32::new(self.images[index]).context("null GL texture")?;
        // SAFETY: the texture belongs to the acquired swapchain image; the context is current.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(texture)));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                self.width as i32,
                self.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&self.flipped)),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.flush();
        }
        self.swapchain.release_image()?;
        self.uploaded = true;
        // A static image is uploaded once: its copy is not kept.
        self.flipped = Vec::new();
        #[cfg(all(feature = "selftest", not(feature = "store")))]
        let _ = began;
        Ok(())
    }

    /// Uploads `part` of RGBA8 pixels given top row first (`None`: all of them) into
    /// an image that changes (the menu: only the buttons pointed at, see
    /// `ui::Panel::damage`). The swapchain's image written gets what changed since it
    /// was last written, so it ends up as `rgba` whichever image the runtime hands out.
    pub fn upload_part(
        &mut self,
        gl: &glow::Context,
        rgba: &[u8],
        part: Option<Area>,
    ) -> Result<()> {
        let row = self.width as usize * 4;
        if rgba.len() != row * self.height as usize {
            bail!(
                "image is {} bytes, expected {}",
                rgba.len(),
                row * self.height as usize
            );
        }
        let whole = Area {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        };
        // A part within the image; all of it the first time (nothing kept yet).
        let part = match part {
            _ if self.flipped.len() != rgba.len() => whole,
            None => whole,
            Some(p) => Area {
                x: p.x.min(self.width),
                y: p.y.min(self.height),
                w: p.w.min(self.width.saturating_sub(p.x)),
                h: p.h.min(self.height.saturating_sub(p.y)),
            },
        };
        if part.is_empty() && self.stale.iter().all(Area::is_empty) {
            return Ok(());
        }
        #[cfg(all(feature = "selftest", not(feature = "store")))]
        let began = crate::selftest::upload_started();
        // Its rows turned over into the copy kept from the last upload (each a plain
        // copy; nothing allocated after the first).
        if part == whole {
            self.flipped.clear();
            self.flipped.reserve_exact(rgba.len());
            for line in rgba.chunks_exact(row).rev() {
                self.flipped.extend_from_slice(line);
            }
        } else {
            let (x, w) = (part.x as usize * 4, part.w as usize * 4);
            for y in part.y as usize..(part.y + part.h) as usize {
                let to = (self.height as usize - 1 - y) * row + x;
                let from = y * row + x;
                self.flipped[to..to + w].copy_from_slice(&rgba[from..from + w]);
            }
        }
        for s in &mut self.stale {
            *s = s.union(part);
        }
        let index = self.swapchain.acquire_image()? as usize;
        self.swapchain.wait_image(xr::Duration::INFINITE)?;
        let texture = NonZeroU32::new(self.images[index]).context("null GL texture")?;
        let todo = self.stale.get(index).copied().unwrap_or(whole);
        if !todo.is_empty() {
            // Rows from the bottom in GL; the rest of each row skipped.
            let bottom = (self.height - (todo.y + todo.h)) as i32;
            // SAFETY: the texture belongs to the acquired swapchain image; the context is
            // current; the rows read lie within `flipped` (`todo` is within the image).
            unsafe {
                gl.bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(texture)));
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, self.width as i32);
                gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, todo.x as i32);
                gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, bottom);
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    todo.x as i32,
                    bottom,
                    todo.w as i32,
                    todo.h as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&self.flipped)),
                );
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
                gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
                gl.bind_texture(glow::TEXTURE_2D, None);
                gl.flush();
            }
        }
        if let Some(s) = self.stale.get_mut(index) {
            *s = Area::EMPTY;
        }
        self.swapchain.release_image()?;
        self.uploaded = true;
        #[cfg(all(feature = "selftest", not(feature = "store")))]
        crate::selftest::uploaded(began);
        Ok(())
    }

    pub fn sub_image(&self) -> xr::sys::SwapchainSubImage {
        xr::sys::SwapchainSubImage {
            swapchain: self.swapchain.as_raw(),
            image_rect: xr::Rect2Di {
                offset: xr::Offset2Di { x: 0, y: 0 },
                extent: xr::Extent2Di {
                    width: self.width as i32,
                    height: self.height as i32,
                },
            },
            image_array_index: 0,
        }
    }

    /// A camera-facing quad of this image, alpha-blended over what is behind it.
    pub fn quad(
        &self,
        space: &xr::Space,
        pose: xr::Posef,
        width: f32,
        height: f32,
    ) -> xr::sys::CompositionLayerQuad {
        xr::sys::CompositionLayerQuad {
            ty: xr::sys::CompositionLayerQuad::TYPE,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: space.as_raw(),
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: self.sub_image(),
            pose,
            size: xr::Extent2Df { width, height },
        }
    }
}

/// Where both eyes are at `time` in `space`, written into `views` (kept by the caller:
/// `Session::locate_views` makes two lists at each call, this nothing). Whether both
/// were found; a part of a pose the runtime says is not valid is left as the default,
/// as `Session::locate_views` leaves it.
pub fn locate_views(
    session: &xr::Session<Gfx>,
    time: xr::Time,
    space: &xr::Space,
    views: &mut [xr::View; 2],
) -> bool {
    let info = xr::sys::ViewLocateInfo {
        ty: xr::sys::ViewLocateInfo::TYPE,
        next: ptr::null(),
        view_configuration_type: xr::ViewConfigurationType::PRIMARY_STEREO,
        display_time: time,
        space: space.as_raw(),
    };
    let mut state = xr::sys::ViewState {
        ty: xr::sys::ViewState::TYPE,
        next: ptr::null_mut(),
        view_state_flags: xr::ViewStateFlags::EMPTY,
    };
    let empty = xr::sys::View {
        ty: xr::sys::View::TYPE,
        next: ptr::null_mut(),
        pose: xr::Posef::IDENTITY,
        fov: xr::Fovf::default(),
    };
    let mut raw = [empty; 2];
    let mut count = 0;
    // SAFETY: the session and the space are alive; `info` and `state` are valid for
    // the call, and `raw` has room for the two views it is said to have.
    let result = unsafe {
        (session.instance().fp().locate_views)(
            session.as_raw(),
            &raw const info,
            &raw mut state,
            2,
            &raw mut count,
            raw.as_mut_ptr(),
        )
    };
    if result.into_raw() < 0 || count < 2 {
        return false;
    }
    let flags = state.view_state_flags;
    for (view, raw) in views.iter_mut().zip(&raw) {
        view.pose = xr::Posef {
            orientation: if flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) {
                raw.pose.orientation
            } else {
                xr::Quaternionf::default()
            },
            position: if flags.contains(xr::ViewStateFlags::POSITION_VALID) {
                raw.pose.position
            } else {
                xr::Vector3f::default()
            },
        };
        view.fov = raw.fov;
    }
    true
}

/// Tells the runtime that the calling thread is the app's main thread and the one
/// that renders (`XR_KHR_android_thread_settings`): the frame loop does both, and the
/// runtime schedules it as such.
pub fn mark_main_thread(instance: &xr::Instance, session: &xr::Session<Gfx>) {
    let Some(ext) = instance.exts().khr_android_thread_settings.as_ref() else {
        return;
    };
    // SAFETY: plain system call returning the calling thread's id.
    let tid = unsafe { libc::gettid() } as u32;
    for (kind, what) in [
        (xr::sys::AndroidThreadTypeKHR::APPLICATION_MAIN, "main"),
        (xr::sys::AndroidThreadTypeKHR::RENDERER_MAIN, "rendering"),
    ] {
        // SAFETY: a live session and the calling thread's own id.
        let result = unsafe { (ext.set_android_application_thread)(session.as_raw(), kind, tid) };
        if result.into_raw() < 0 {
            warn!("marking the {what} thread: {result:?}");
        } else {
            info!("frame loop marked as the {what} thread for the runtime");
        }
    }
}
