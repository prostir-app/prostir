//! The controllers' models by `XR_MSFT_controller_model`, for a runtime that has it
//! and not `XR_FB_render_model` (`platform::meta::models`): the glTF binary of the
//! controller in each hand, asked once when the controllers come. The model's nodes
//! that the runtime moves with the buttons are drawn where the file puts them (step
//! 8.6: no moving buttons yet).

use std::ptr;

use log::{info, warn};
use openxr as xr;

use crate::model::MAX_BYTES;
use crate::platform::openxr::Gfx;

/// Turns the extension on in `on` if `available` has it.
pub fn extensions(available: &xr::ExtensionSet, on: &mut xr::ExtensionSet) {
    on.msft_controller_model = available.msft_controller_model;
}

/// The glTF binary of the controller now in hand `hand` (its `/user/hand/...` path),
/// with its key (the same key is the same model). `None`: the extension is off, no
/// controller is there, or the runtime has no model of it.
pub fn controller(session: &xr::Session<Gfx>, hand: xr::Path) -> Option<(u64, Vec<u8>)> {
    let ext = session.instance().exts().msft_controller_model.as_ref()?;
    let mut key = xr::sys::ControllerModelKeyStateMSFT {
        ty: xr::sys::ControllerModelKeyStateMSFT::TYPE,
        next: ptr::null_mut(),
        model_key: xr::sys::ControllerModelKeyMSFT::NULL,
    };
    // SAFETY: a live session and a path of its instance; `key` is typed and alive.
    let result = unsafe { (ext.get_controller_model_key)(session.as_raw(), hand, &raw mut key) };
    if result.into_raw() < 0 || key.model_key == xr::sys::ControllerModelKeyMSFT::NULL {
        if result.into_raw() < 0 {
            warn!("controller model key: {result:?}");
        }
        return None;
    }
    let mut size = 0;
    // SAFETY: a key the runtime just gave; no room yet, the size comes back.
    let result = unsafe {
        (ext.load_controller_model)(
            session.as_raw(),
            key.model_key,
            0,
            &raw mut size,
            ptr::null_mut(),
        )
    };
    if result.into_raw() < 0 || size == 0 || size as usize > MAX_BYTES {
        warn!("controller model: {result:?}");
        return None;
    }
    let mut bytes = vec![0u8; size as usize];
    // SAFETY: as above, `bytes` has room for the size the runtime said.
    let result = unsafe {
        (ext.load_controller_model)(
            session.as_raw(),
            key.model_key,
            size,
            &raw mut size,
            bytes.as_mut_ptr(),
        )
    };
    if result.into_raw() < 0 {
        warn!("controller model: {result:?}");
        return None;
    }
    bytes.truncate(size as usize);
    info!(
        "controller model {:#x}: {} bytes",
        key.model_key.into_raw(),
        bytes.len()
    );
    Some((key.model_key.into_raw(), bytes))
}
