//! The person's keyboard as the headset tracks it (`XR_FB_keyboard_tracking`, step
//! 8.7): whether the runtime tracks one, which, and where it lies each frame. The
//! runtime needs no permission of the app for the question to be asked (the manifest
//! names its tracked-keyboard feature, not required); if it says it needs one (or
//! does not do it), the tracking is given up for the run ([`Poll::Unavailable`]) and
//! the window onto the keyboard is the one placed by hand.
//!
//! Not `XR_META_dynamic_object_keyboard`, though it is the newer way: it is a preview
//! extension (an app with it must say it uses experimental features, which the store
//! refuses), and its keyboard is found by a spatial-entity query, which takes the
//! scene permission (`USE_SCENE`), a new one the person would be asked for.
//!
//! Which keyboard is asked about once in a while, never in every frame; a frame only
//! locates the space of the one found, and allocates nothing.

use std::ptr;
use std::time::{Duration, Instant};

use log::{info, warn};
use openxr as xr;
use openxr::sys::Handle as _;

use crate::keyboard::Seen;
use crate::platform::openxr::{Gfx, quat, vec3};

/// How often the runtime is asked which keyboard it tracks, while none is found.
const ASK_EVERY: Duration = Duration::from_secs(1);
/// And while one is, to notice another taking its place.
const ASK_AGAIN: Duration = Duration::from_secs(10);

/// Turns on `XR_FB_keyboard_tracking` in `on` when `available` has it.
pub fn extensions(available: &xr::ExtensionSet, on: &mut xr::ExtensionSet) {
    on.fb_keyboard_tracking = available.fb_keyboard_tracking;
}

/// Whether the extension is on and the system says it tracks a keyboard.
pub fn supported(instance: &xr::Instance, system: xr::SystemId) -> bool {
    if instance.exts().fb_keyboard_tracking.is_none() {
        return false;
    }
    let mut keyboard = xr::sys::SystemKeyboardTrackingPropertiesFB {
        ty: xr::sys::SystemKeyboardTrackingPropertiesFB::TYPE,
        next: ptr::null_mut(),
        supports_keyboard_tracking: xr::sys::FALSE,
    };
    // SAFETY: a zeroed properties block with its type is how the crate starts one; the
    // chained struct is typed and outlives the call.
    let mut props: xr::sys::SystemProperties = unsafe { std::mem::zeroed() };
    props.ty = xr::sys::SystemProperties::TYPE;
    props.next = (&raw mut keyboard).cast();
    // SAFETY: a live instance and system, a typed out block.
    let result =
        unsafe { (instance.fp().get_system_properties)(instance.as_raw(), system, &raw mut props) };
    result.into_raw() >= 0 && keyboard.supports_keyboard_tracking == xr::sys::TRUE
}

/// What a pass found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Poll {
    /// The keyboard now (`None`: none tracked, or not in view).
    Seen(Option<Seen>),
    /// The runtime will not track one for this app: not asked again.
    Unavailable,
}

/// The keyboard the runtime tracks, and the space it is located by.
pub struct Tracker {
    session: xr::Session<Gfx>,
    /// The id of the keyboard `space` is of, and the sides it said.
    found: Option<Found>,
    asked: Option<Instant>,
    unavailable: bool,
}

struct Found {
    id: u64,
    width: f32,
    depth: f32,
    space: xr::Space,
}

impl Tracker {
    pub fn new(session: &xr::Session<Gfx>) -> Self {
        Self {
            session: session.clone(),
            found: None,
            asked: None,
            unavailable: false,
        }
    }

    /// Where the keyboard is at `time` in `base`, asking which one it is when it is
    /// time to.
    pub fn poll(&mut self, now: Instant, base: &xr::Space, time: xr::Time) -> Poll {
        if self.unavailable {
            return Poll::Unavailable;
        }
        let every = if self.found.is_some() {
            ASK_AGAIN
        } else {
            ASK_EVERY
        };
        if self
            .asked
            .is_none_or(|at| now.saturating_duration_since(at) >= every)
        {
            self.asked = Some(now);
            if !self.ask() {
                self.unavailable = true;
                self.found = None;
                return Poll::Unavailable;
            }
        }
        let Some(found) = &self.found else {
            return Poll::Seen(None);
        };
        let Ok(at) = found.space.locate(base, time) else {
            return Poll::Seen(None);
        };
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        if !at.location_flags.contains(valid) {
            return Poll::Seen(None);
        }
        Poll::Seen(Some(Seen {
            position: vec3(at.pose.position),
            rotation: quat(at.pose.orientation),
            width: found.width,
            depth: found.depth,
        }))
    }

    /// Asks which keyboard is tracked (the one of a PC first, then the one of the
    /// headset), and makes its space when it is another. `false`: the runtime will not.
    fn ask(&mut self) -> bool {
        let Some(ext) = self.session.instance().exts().fb_keyboard_tracking else {
            return false;
        };
        let mut seen = None;
        for flags in [
            xr::sys::KeyboardTrackingQueryFlagsFB::REMOTE,
            xr::sys::KeyboardTrackingQueryFlagsFB::LOCAL,
        ] {
            let query = xr::sys::KeyboardTrackingQueryFB {
                ty: xr::sys::KeyboardTrackingQueryFB::TYPE,
                next: ptr::null_mut(),
                flags,
            };
            let mut keyboard = xr::sys::KeyboardTrackingDescriptionFB {
                tracked_keyboard_id: 0,
                size: xr::Vector3f {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                flags: xr::sys::KeyboardTrackingFlagsFB::EMPTY,
                name: [0; xr::sys::MAX_KEYBOARD_TRACKING_NAME_SIZE_FB],
            };
            // SAFETY: a live session, a typed query and a block to fill.
            let result = unsafe {
                (ext.query_system_tracked_keyboard)(
                    self.session.as_raw(),
                    &raw const query,
                    &raw mut keyboard,
                )
            };
            if matches!(
                result,
                xr::sys::Result::ERROR_PERMISSION_INSUFFICIENT
                    | xr::sys::Result::ERROR_FEATURE_UNSUPPORTED
                    | xr::sys::Result::ERROR_FUNCTION_UNSUPPORTED
            ) {
                warn!("keyboard tracking: {result:?}; the window stays where it was put");
                return false;
            }
            if result.into_raw() >= 0
                && keyboard
                    .flags
                    .contains(xr::sys::KeyboardTrackingFlagsFB::EXISTS)
            {
                seen = Some(keyboard);
                break;
            }
        }
        let Some(keyboard) = seen else {
            self.found = None;
            return true;
        };
        if self
            .found
            .as_ref()
            .is_some_and(|f| f.id == keyboard.tracked_keyboard_id)
        {
            return true;
        }
        let info = xr::sys::KeyboardSpaceCreateInfoFB {
            ty: xr::sys::KeyboardSpaceCreateInfoFB::TYPE,
            next: ptr::null_mut(),
            tracked_keyboard_id: keyboard.tracked_keyboard_id,
        };
        let mut handle = xr::sys::Space::NULL;
        // SAFETY: a live session, a typed create info and an out handle.
        let result = unsafe {
            (ext.create_keyboard_space)(self.session.as_raw(), &raw const info, &raw mut handle)
        };
        if result.into_raw() < 0 {
            if result == xr::sys::Result::ERROR_PERMISSION_INSUFFICIENT {
                warn!("keyboard tracking: {result:?}; the window stays where it was put");
                return false;
            }
            self.found = None;
            return true;
        }
        info!(
            "keyboard tracked: {} x {} m",
            keyboard.size.x, keyboard.size.z
        );
        // SAFETY: the handle was just made for this session, and is owned from here.
        let space = unsafe { xr::Space::reference_from_raw(self.session.clone(), handle) };
        self.found = Some(Found {
            id: keyboard.tracked_keyboard_id,
            width: keyboard.size.x,
            depth: keyboard.size.z,
            space,
        });
        true
    }
}
