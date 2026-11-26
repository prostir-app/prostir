//! The headset's app: shows each monitor of the PC as an OpenXR layer, on any headset
//! with OpenXR on Android.
//!
//! Video frames are decoded by MediaCodec directly into Android surfaces owned by the
//! headset's compositor (XR_KHR_android_surface_swapchain), so pixels never pass
//! through this app's GPU code and text stays as sharp as the compositor can make it.
//!
//! The core (the model and its rules, what the menu shows) builds and is tested on any
//! system; the shell (`main_loop`, `platform`, the decoders) is Android's.

// Every unsafe block says why it is sound.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod app;
pub mod autoscale;
pub mod background;
pub mod backoff;
pub mod caps;
pub mod clarity;
pub mod commands;
pub mod connecting;
pub mod cursors;
pub mod device;
pub mod frames;
pub mod ids;
pub mod interaction;
pub mod lang;
pub mod layers;
pub mod layout;
#[cfg(feature = "preview")]
pub mod metrics;
pub mod model;
pub mod net;
pub mod note;
pub mod outbox;
pub mod palette;
pub mod pending;
pub mod persist;
pub mod physical;
pub mod platform;
pub mod presence;
pub mod profiles;
#[cfg(all(feature = "selftest", not(feature = "store")))]
pub mod selftest;
pub mod session;
pub mod settings;
pub mod ui;
pub mod video;
pub mod view;
pub mod world;

/// The preview build (`PROSTIR_PREVIEW=1 scripts/build-apk.sh`, the feature `preview`):
/// the menu has its "For development" part and the numbers next to the monitor looked
/// at. Never the store's build: that code is not even built there.
pub const PREVIEW: bool = cfg!(feature = "preview");

#[cfg(all(feature = "preview", feature = "store"))]
compile_error!("the preview build is never a store build: `preview` and `store` together");

#[cfg(target_os = "android")]
mod alloc_count;
#[cfg(target_os = "android")]
mod main_loop;
