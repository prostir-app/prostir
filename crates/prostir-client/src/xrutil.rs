//! OpenXR/Android plumbing shared by the main loop.

use std::num::NonZeroU32;
use std::ptr;

use android_activity::AndroidApp;
use anyhow::{Context, Result, bail};
use glam::{Quat, Vec3};
use glow::HasContext;
use log::{info, warn};
use ndk::native_window::NativeWindow;
use openxr as xr;
use openxr::sys::Handle as _;

use crate::layout::Placement;

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
    // Meta's runtime rejects surface swapchains with a format or non-zero counts
    // ("info format validation failure"): MediaCodec decides those.
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

/// Keeps Wi-Fi out of power save while the app runs. Without it the headset's
/// radio naps between beacons and downlink frames wait at the router.
pub fn acquire_low_latency_wifi(
    app: &AndroidApp,
    vm: &jni::JavaVM,
) -> Result<jni::refs::Global<jni::objects::JObject<'static>>> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};
    /// WifiManager.WIFI_MODE_FULL_LOW_LATENCY (API 29).
    const WIFI_MODE_FULL_LOW_LATENCY: i32 = 4;
    // A Java exception on the way is caught and comes back as the error.
    let lock = vm.attach_current_thread(|env| -> jni::errors::Result<_> {
        let raw: jni::sys::jobject = app.activity_as_ptr().cast();
        // SAFETY: the activity reference stays valid while the app runs; the cast only
        // borrows it, it never deletes it.
        let activity = unsafe { env.as_cast_raw::<JObject<'static>>(&raw) }?;
        let context = env
            .call_method(
                &activity,
                jni_str!("getApplicationContext"),
                jni_sig!("()Landroid/content/Context;"),
                &[],
            )?
            .l()?;
        let service = env.new_string("wifi")?;
        let wifi = env
            .call_method(
                &context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&service)],
            )?
            .l()?;
        let tag = env.new_string("prostir")?;
        let lock = env
            .call_method(
                &wifi,
                jni_str!("createWifiLock"),
                jni_sig!("(ILjava/lang/String;)Landroid/net/wifi/WifiManager$WifiLock;"),
                &[
                    JValue::Int(WIFI_MODE_FULL_LOW_LATENCY),
                    JValue::Object(&tag),
                ],
            )?
            .l()?;
        env.call_method(
            &lock,
            jni_str!("setReferenceCounted"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(false)],
        )?;
        env.call_method(&lock, jni_str!("acquire"), jni_sig!("()V"), &[])?;
        env.new_global_ref(lock)
    })?;
    Ok(lock)
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
        Ok(Self {
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
        if self.is_static && self.uploaded {
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
        // Row by row (each a plain copy) into the buffer kept from the last upload.
        let mut flipped = std::mem::take(&mut self.flipped);
        flipped.clear();
        flipped.reserve_exact(rgba.len());
        for line in rgba.chunks_exact(row).rev() {
            flipped.extend_from_slice(line);
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
                glow::PixelUnpackData::Slice(Some(&flipped)),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.flush();
        }
        self.swapchain.release_image()?;
        self.uploaded = true;
        // A static image is uploaded once: its copy is not kept.
        if !self.is_static {
            self.flipped = flipped;
            #[cfg(all(feature = "selftest", not(feature = "store")))]
            crate::selftest::uploaded(began);
        }
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

/// How urgent a thread of this app is to the scheduler, as Android ranks its own
/// threads of the same kind.
#[derive(Debug, Clone, Copy)]
pub enum Priority {
    /// Sound in and out (`THREAD_PRIORITY_AUDIO`).
    Audio,
    /// Pictures and the network that brings them (`THREAD_PRIORITY_VIDEO`).
    Video,
}

/// Raises the calling thread to `priority`, so a busy moment elsewhere on the headset
/// delays it less. Only logged if the system says no.
pub fn raise_thread_priority(priority: Priority) {
    let nice = match priority {
        Priority::Audio => -16,
        Priority::Video => -10,
    };
    // SAFETY: plain system call; on Linux `PRIO_PROCESS` with `who` 0 is the calling
    // thread alone.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } != 0 {
        warn!(
            "thread priority {nice}: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// Tells the runtime that the calling thread is the app's main thread
/// (`XR_KHR_android_thread_settings`), which it then schedules as such.
pub fn mark_main_thread(instance: &xr::Instance, session: &xr::Session<Gfx>) {
    let Some(ext) = instance.exts().khr_android_thread_settings.as_ref() else {
        return;
    };
    // SAFETY: a live session and the calling thread's own id.
    let result = unsafe {
        (ext.set_android_application_thread)(
            session.as_raw(),
            xr::sys::AndroidThreadTypeKHR::APPLICATION_MAIN,
            libc::gettid() as u32,
        )
    };
    if result.into_raw() < 0 {
        warn!("marking the main thread: {result:?}");
    } else {
        info!("main thread marked for the runtime");
    }
}

/// Whether the left hand makes the menu gesture (palm towards the face, pinch), which
/// the Quest reports through `XR_FB_hand_tracking_aim`. The plain hand profile has no
/// menu button, so without this a user with hands only cannot open the menu when no
/// menu button is in view.
pub fn hand_menu_gesture(
    instance: &xr::Instance,
    tracker: &xr::HandTracker,
    space: &xr::Space,
    time: xr::Time,
) -> bool {
    let Some(ext) = instance.exts().ext_hand_tracking.as_ref() else {
        return false;
    };
    let mut aim = xr::sys::HandTrackingAimStateFB {
        ty: xr::sys::HandTrackingAimStateFB::TYPE,
        next: ptr::null_mut(),
        status: xr::sys::HandTrackingAimFlagsFB::EMPTY,
        aim_pose: xr::Posef::IDENTITY,
        pinch_strength_index: 0.0,
        pinch_strength_middle: 0.0,
        pinch_strength_ring: 0.0,
        pinch_strength_little: 0.0,
    };
    let mut joints = [xr::sys::HandJointLocationEXT::default(); 26];
    let mut locations = xr::sys::HandJointLocationsEXT {
        ty: xr::sys::HandJointLocationsEXT::TYPE,
        next: (&raw mut aim).cast(),
        is_active: xr::sys::FALSE,
        joint_count: joints.len() as u32,
        joint_locations: joints.as_mut_ptr(),
    };
    let info = xr::sys::HandJointsLocateInfoEXT {
        ty: xr::sys::HandJointsLocateInfoEXT::TYPE,
        next: ptr::null(),
        base_space: space.as_raw(),
        time,
    };
    // SAFETY: live tracker and space; `locations` points at 26 joints and chains a
    // properly typed aim state, all of which outlive the call.
    let result =
        unsafe { (ext.locate_hand_joints)(tracker.as_raw(), &raw const info, &raw mut locations) };
    result.into_raw() >= 0
        && locations.is_active == xr::sys::TRUE
        && aim
            .status
            .contains(xr::sys::HandTrackingAimFlagsFB::MENU_PRESSED)
}
