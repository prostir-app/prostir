//! The headset's platform, in the parts any headset on OpenXR and Android splits into
//! (step 7, 4.4): standard OpenXR ([`openxr`]), the extensions of one maker
//! ([`meta`], each by its own check), what every headset on Android shares
//! ([`android`]), the sound ([`sound`]) and the store a build comes from ([`store`]).
//! The core sees none of it: what the headset can do comes to it as [`crate::caps::Caps`],
//! made here at the start ([`caps`]).
//!
//! The controllers' table and the stores build on any system (they are data and an
//! interface); the rest only for Android.

pub mod controllers;
pub mod store;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "android")]
pub mod meta;
#[cfg(target_os = "android")]
pub mod openxr;
#[cfg(target_os = "android")]
pub mod sound;

#[cfg(target_os = "android")]
pub use self::found::caps;

#[cfg(target_os = "android")]
mod found {
    use log::{info, warn};
    use openxr as xr;

    use super::meta::passthrough::Passthrough;
    use super::openxr::Gfx;
    use super::{android, meta};
    use crate::caps::{self, Caps, RoomCaps};

    /// What this headset can do: from the extensions turned on (`on`; with
    /// `caps=minimal` only those it cannot do without), what the runtime says of the
    /// system and the display, the cameras made (`passthrough`), and what Android says
    /// of its decoders and its volume. Asked once, at the start.
    pub fn caps(
        on: &xr::ExtensionSet,
        instance: &xr::Instance,
        system: xr::SystemId,
        session: &xr::Session<Gfx>,
        passthrough: Option<&Passthrough>,
        hands: bool,
        model: String,
    ) -> Caps {
        let layers_max = match instance.system_properties(system) {
            Ok(p) if p.graphics_properties.max_layer_count > 0 => {
                let layers = p.graphics_properties.max_layer_count;
                info!("{} takes {layers} layers", p.system_name);
                u8::try_from(layers).unwrap_or(u8::MAX)
            }
            _ => caps::LAYERS_AT_LEAST,
        };
        let (decoders, streams_max) = match android::codecs::decoders() {
            Some(found) if !found.is_empty() => {
                let streams = caps::streams_max(found.iter().map(|(_, n)| *n), layers_max);
                (found.into_iter().map(|(d, _)| d).collect(), streams)
            }
            _ => {
                warn!("the system's decoders unknown: the ones taken for granted before");
                let streams = caps::streams_max([], layers_max);
                (caps::DECODERS_UNSAID.to_vec(), streams)
            }
        };
        let volume = android::volume();
        info!("volume: {volume:?} (steps above silence, fixed)");
        let volume_steps = caps::volume_steps(volume.map(|(s, _)| s), volume.is_some_and(|v| v.1));
        let mut c = Caps::without_extensions(model, decoders, streams_max, volume_steps);
        c.layers_max = layers_max;
        c.refresh_rates = meta::display_rates(session).0;
        c.perf_levels = on.ext_performance_settings;
        c.cylinder = on.khr_composition_layer_cylinder;
        c.equirect = on.khr_composition_layer_equirect2;
        c.color_scale = on.khr_composition_layer_color_scale_bias;
        c.room = passthrough.map(|p| RoomCaps {
            keyboard_window: p.window_supported(),
        });
        c.hands = hands;
        c.hand_aim = on.ext_hand_interaction || (hands && on.fb_hand_tracking_aim);
        c.menu_gesture = hands && on.fb_hand_tracking_aim;
        info!("headset can: {} off; {c:?}", c.missing());
        c
    }
}
