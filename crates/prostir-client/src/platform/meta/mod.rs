//! The extensions of one maker (`XR_FB_*`, `XR_META_*`), each turned on and used by its
//! own check that the runtime has it, not by who made the headset: another runtime with
//! one of them gets it too. What they give: the cameras ([`passthrough`]), the display
//! rate, sharper layers, the stream's picture the right way up, the left hand's menu
//! gesture.

pub mod passthrough;

use std::ptr;

use log::{info, warn};
use openxr as xr;

use crate::platform::openxr::Gfx;

/// Turns on in `on` each of these extensions that `available` has.
pub fn extensions(available: &xr::ExtensionSet, on: &mut xr::ExtensionSet) {
    on.fb_passthrough = available.fb_passthrough;
    on.fb_triangle_mesh = available.fb_triangle_mesh;
    on.fb_display_refresh_rate = available.fb_display_refresh_rate;
    on.fb_composition_layer_settings = available.fb_composition_layer_settings;
    on.fb_composition_layer_image_layout = available.fb_composition_layer_image_layout;
    on.fb_hand_tracking_aim = available.fb_hand_tracking_aim;
}

/// The display rates the runtime offers (`XR_FB_display_refresh_rate`; none without
/// it), and the one it shows now.
pub fn display_rates(session: &xr::Session<Gfx>) -> (Vec<f32>, Option<f32>) {
    if session.instance().exts().fb_display_refresh_rate.is_none() {
        return (Vec::new(), None);
    }
    let offered = session
        .enumerate_display_refresh_rates()
        .inspect_err(|e| warn!("display rates: {e}"))
        .unwrap_or_default();
    info!("display rates offered: {offered:?}");
    (offered, display_rate(session))
}

/// The display rate now (`None` without `XR_FB_display_refresh_rate`).
pub fn display_rate(session: &xr::Session<Gfx>) -> Option<f32> {
    session.instance().exts().fb_display_refresh_rate.as_ref()?;
    session.get_display_refresh_rate().ok()
}

/// Asks for the display rate `hz` (one the runtime offered).
pub fn set_display_rate(session: &xr::Session<Gfx>, hz: f32) {
    if session.instance().exts().fb_display_refresh_rate.is_none() {
        return;
    }
    match session.request_display_refresh_rate(hz) {
        Ok(()) => info!("display at {hz} Hz"),
        Err(e) => warn!("display at {hz} Hz: {e}"),
    }
}

/// Supersampling and sharpening for a layer (`XR_FB_composition_layer_settings`):
/// the desktop's text is denser than the display, and plain sampling shimmers.
pub fn sharper() -> xr::sys::CompositionLayerSettingsFB {
    xr::sys::CompositionLayerSettingsFB {
        ty: xr::sys::CompositionLayerSettingsFB::TYPE,
        next: ptr::null(),
        layer_flags: xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SUPER_SAMPLING
            | xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SHARPENING,
    }
}

/// The layer's picture turned over (`XR_FB_composition_layer_image_layout`): an Android
/// surface keeps its rows top down, while the compositor reads them bottom up.
pub fn flipped(next: *const std::ffi::c_void) -> xr::sys::CompositionLayerImageLayoutFB {
    xr::sys::CompositionLayerImageLayoutFB {
        ty: xr::sys::CompositionLayerImageLayoutFB::TYPE,
        next: next.cast_mut(),
        flags: xr::sys::CompositionLayerImageLayoutFlagsFB::VERTICAL_FLIP,
    }
}

/// Whether the left hand makes the system's menu gesture (palm towards the face,
/// pinch), as `XR_FB_hand_tracking_aim` reports it. The hands' own profile has no menu
/// button, so without this a user with hands only cannot open the menu when no menu
/// button is in view.
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
