//! Virtual input on Linux (see `Injector` in `contract.rs`), by desktop:
//!
//! - KDE Plasma (and GNOME): the RemoteDesktop portal with libei (`portal.rs`,
//!   `ei.rs`): the desktop asks the person once, and keeps the answer.
//! - Hyprland and other wlroots desktops: virtual pointers and keyboards (`wlr.rs`).
//! - Otherwise, where the user may write `/dev/uinput`: devices of the kernel
//!   (`uinput.rs`).
//!
//! Which there is is found when the desktop connects (quickly: no dialog); the
//! devices open on the input thread when first needed.

mod ei;
mod portal;
mod uinput;
mod wlr;

use std::sync::Arc;

use log::info;

use crate::platform::{Injects, OpenInput};

/// What there is: what it makes, and how to open it.
pub type Found = Option<(Injects, Arc<dyn OpenInput>)>;

const ALL: Injects = Injects {
    pointer: true,
    keys: true,
};

/// KDE Plasma's: the portal, else uinput.
pub fn for_kde() -> Found {
    if portal::available() {
        return Some((ALL, Arc::new(portal::PortalOpen)));
    }
    fallback()
}

/// A wlroots desktop's (Hyprland): its virtual pointers, else uinput.
pub fn for_wlroots() -> Found {
    if wlr::available() {
        return Some((ALL, Arc::new(wlr::WlrOpen)));
    }
    fallback()
}

fn fallback() -> Found {
    if uinput::available() {
        info!("input: through uinput, the desktop has no virtual input of its own");
        return Some((ALL, Arc::new(uinput::UinputOpen)));
    }
    info!("input: none (no virtual input on this desktop, and /dev/uinput is not writable)");
    None
}
