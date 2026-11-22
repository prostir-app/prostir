//! Linux: KDE Plasma or Hyprland, picked at run time.

pub mod audio;
pub mod autostart;
mod desktop;
pub mod dialogs;
mod hyprland;
mod kde;
pub mod notifications;
mod screenshot;
pub mod sound_source;
mod system;
pub mod tray;
mod usb;

pub use desktop::connect;
pub use screenshot::{has_picture, screenshot_dir, screenshot_path};
pub use system::{cache_home, encoder_drivers};
pub use system::{
    clean_up_after_crash, config_home, give_memory_back, hostname, import_session_environment,
    local_addresses, notice_mark, run, run_until, serving, single_instance, watcher,
};
pub use usb::{keep_adb_reverse, usb_waiting};

/// Whether a connector is a laptop's own panel.
fn is_panel(connector: &str) -> bool {
    ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|p| connector.starts_with(p))
}

/// Whether output `name` is virtual (Prostir's on KDE or Hyprland, or KWin's for
/// another program), not a display of the PC.
fn named_virtual(name: &str) -> bool {
    name.starts_with("Virtual-") || name.starts_with(hyprland::PREFIX)
}
