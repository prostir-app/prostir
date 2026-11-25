//! The extensions of HTC (`XR_HTC_*`), each by its own check that the runtime has it:
//! the cameras' picture of the whole room (`XR_HTC_passthrough`, planar), for the
//! "Room" backgrounds on headsets that have it and not `XR_FB_passthrough`.
//!
//! The passthrough has no pause of its own: it is made while the room is shown and
//! destroyed when it is not, so the cameras do not run for nothing.

use std::ptr;

use anyhow::{Context, Result, bail};
use openxr as xr;
use openxr::sys::Handle as _;

/// Turns on in `on` each of these extensions that `available` has.
pub fn extensions(available: &xr::ExtensionSet, on: &mut xr::ExtensionSet) {
    on.htc_passthrough = available.htc_passthrough;
}

/// The room through the cameras (`XR_HTC_passthrough`, the planar form).
pub struct Passthrough {
    fp: xr::raw::PassthroughHTC,
    session: xr::sys::Session,
    /// While the room is shown.
    made: Option<xr::sys::PassthroughHTC>,
    /// How much of the room shows (1: all; less darkens it towards black).
    opacity: f32,
}

impl Passthrough {
    /// Ready to show the room in `session` (nothing made until it is shown).
    pub fn new<G: xr::Graphics>(session: &xr::Session<G>) -> Result<Self> {
        let fp = *session
            .instance()
            .exts()
            .htc_passthrough
            .as_ref()
            .context("XR_HTC_passthrough is not enabled")?;
        Ok(Self {
            fp,
            session: session.as_raw(),
            made: None,
            opacity: 1.0,
        })
    }

    /// Shows the room (`on`), as much of it as `opacity` says, or stops the cameras.
    pub fn set_room(&mut self, on: bool, opacity: f32) -> Result<()> {
        self.opacity = opacity.clamp(0.0, 1.0);
        match (on, self.made) {
            (true, None) => {
                let info = xr::sys::PassthroughCreateInfoHTC {
                    ty: xr::sys::PassthroughCreateInfoHTC::TYPE,
                    next: ptr::null(),
                    form: xr::sys::PassthroughFormHTC::PLANAR,
                };
                let mut handle = xr::sys::PassthroughHTC::NULL;
                // SAFETY: a live session, a valid create info and an out-pointer on the
                // stack.
                let result = unsafe {
                    (self.fp.create_passthrough)(self.session, &raw const info, &raw mut handle)
                };
                if result.into_raw() < 0 {
                    bail!("xrCreatePassthroughHTC failed: {result:?}");
                }
                self.made = Some(handle);
            }
            (false, Some(handle)) => {
                self.made = None;
                // SAFETY: made above for this session, destroyed once.
                unsafe { (self.fp.destroy_passthrough)(handle) };
            }
            _ => {}
        }
        Ok(())
    }

    /// The room is shown.
    pub fn shown(&self) -> bool {
        self.made.is_some()
    }

    /// The room's layer for the frame, while it is shown (the first layer, under all).
    pub fn layer(&self, space: &xr::Space) -> Option<xr::sys::CompositionLayerPassthroughHTC> {
        let passthrough = self.made?;
        Some(xr::sys::CompositionLayerPassthroughHTC {
            ty: xr::sys::CompositionLayerPassthroughHTC::TYPE,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: space.as_raw(),
            passthrough,
            color: xr::sys::PassthroughColorHTC {
                ty: xr::sys::PassthroughColorHTC::TYPE,
                next: ptr::null(),
                alpha: self.opacity,
            },
        })
    }
}

impl Drop for Passthrough {
    fn drop(&mut self) {
        if let Some(handle) = self.made.take() {
            // SAFETY: made for this session, destroyed once.
            unsafe { (self.fp.destroy_passthrough)(handle) };
        }
    }
}
