//! Quest 3 client: shows each virtual monitor of the PC as an OpenXR quad layer.
//!
//! Video frames are decoded by MediaCodec directly into Android surfaces owned
//! by the Quest compositor (XR_KHR_android_surface_swapchain), so pixels never
//! pass through this app's GPU code and text stays as sharp as the compositor
//! can make it.

// Every unsafe block says why it is sound.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod background;
pub mod commands;
pub mod device;
pub mod ids;
pub mod lang;
pub mod layout;
pub mod outbox;
pub mod pending;
pub mod persist;
pub mod physical;
pub mod profiles;
#[cfg(all(feature = "selftest", not(feature = "store")))]
pub mod selftest;
pub mod settings;
pub mod store;
pub mod ui;
pub mod world;

#[cfg(target_os = "android")]
mod alloc_count;
#[cfg(target_os = "android")]
mod app;
#[cfg(target_os = "android")]
mod audio;
#[cfg(target_os = "android")]
mod decoder;
#[cfg(target_os = "android")]
mod egl;
#[cfg(target_os = "android")]
mod input;
#[cfg(target_os = "android")]
mod mic;
#[cfg(target_os = "android")]
mod net;
#[cfg(target_os = "android")]
mod overlay;
#[cfg(target_os = "android")]
mod passthrough;
#[cfg(target_os = "android")]
mod system;
#[cfg(target_os = "android")]
mod xrutil;
