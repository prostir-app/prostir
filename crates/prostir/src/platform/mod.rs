//! The door from the core to the operating system: the core (`main.rs`, `session/`,
//! `listen.rs`, `displays/`, `worker/`, `layout.rs`, `encoder.rs`, `pairing.rs`, `outbox.rs`,
//! `usage.rs`, `i18n.rs`) reaches the desktop, the sound system and the user's files
//! only through this module. What the platform's threads see (the tray's items, a
//! signal to quit) goes to the control loop as events (`crate::events`).
//!
//! The desktop is a contract (`contract.rs`): the traits [`Desktop`], [`Output`],
//! [`Windows`] and [`Pointer`], and the data that says what differs ([`DesktopCaps`],
//! [`FrameSource`], [`OutputHealth`], [`OutputKey`]). It has an implementation per
//! desktop, picked once when a session starts ([`connect`]), and the core holds it as
//! a `Box<dyn Desktop>`. Everything else of the platform has one implementation per
//! build: each operating system has a module of its own (`linux/`, picked with `cfg`)
//! whose public items are re-exported here, so the core writes `platform::tray::spawn`
//! and never names the OS.
//!
//! # A new operating system (Windows)
//!
//! Add `platform/windows/mod.rs`, select it below next to `linux`, and provide what
//! `linux/mod.rs` exports, with the same names and signatures:
//!
//! - `connect`: the desktop, as a [`Desktop`] with its [`Output`]s, [`Windows`] and
//!   [`Pointer`]: making and removing virtual monitors, showing the PC's own displays,
//!   placing them, the primary output, and the frame source for the encoder
//!   ([`FrameSource`]: GStreamer source elements, or an `appsrc` named `src` that
//!   [`Output::attach`] feeds). How the contract says what Windows does differently is
//!   in `contract.rs`.
//! - `audio`: `AudioOut` (PC sound to the headset, as a default output device while it
//!   exists: its `route` and the `device` it is captured from), `MicIn` (the headset
//!   microphone as the default input: `push`, `set_recording`), `SinkVolume` (the
//!   level of the headset's device: `open`, `set`, `level`, `outdated`; none of them
//!   waits for the sound system), `watch` (a `Watcher` that sends the sound system's
//!   changes as events, without polling), `prepare` (finds out at start, off the
//!   control loop, whether the volume can be shared), `restore_after_crash`. The
//!   types they share are in `sound.rs`.
//! - `sound_source::Watcher` (`start`, `output`): the output whose window plays sound.
//! - `notifications::watch`: the PC's desktop notifications, as text.
//! - `tray`: `spawn`, `Handle`, `set_status`, `set_battery`, `set_settings`,
//!   `set_pairing`; the tray menu also turns autostart on and off.
//! - `autostart`: `available`, `enabled`, `set` (starting with the session), `first_start`
//!   (on by default), `refresh`,
//!   `quit_until_login`, `start_service`, `socket_waits` (what holds the sockets
//!   while Prostir sleeps, where the system does: systemd's socket on Linux).
//! - The local API's transport (`local_api` on Linux: D-Bus): `serve`, and a
//!   `Client` for `prostir ctl`.
//! - `dialogs`: `ask_code` (the pairing code typed on the PC), `notify` and
//!   `notify_and_wait` (desktop notifications), `notice` (a notification with a
//!   button, as a `Notice` whose descriptor the watcher sleeps on).
//! - System: `config_home`, `single_instance` (with who holds it) and `watcher`,
//!   `clean_up_after_crash`, `give_memory_back`, `hostname`, `local_addresses`,
//!   `screenshot_path`, `import_session_environment`. The private runtime dir, and
//!   running a program with a time limit, stay inside the platform.
//! - `sockets` (`take`, `open`, `filter_queries`, `readable`, `start_with`): the
//!   sockets passed from outside or opened here, and starting the full program with
//!   them. Windows passes inherited sockets in `PROSTIR_SOCKETS` instead of
//!   `LISTEN_FDS`, and starts the program without `fork`.
//! - `keep_adb_reverse`: a headset on USB reaches the PC through 127.0.0.1. A service
//!   of the operating system started once, not part of the desktop (see `contract.rs`).
//!
//! The network belongs to the operating system, not to the desktop: `net.rs` holds
//! it for every desktop, the fake one too, with a part for each operating system. So
//! do the signals that ask the program to quit (`signals.rs`).
//!
//! The compiler checks the list: the core does not build until everything is there.
//! `fake/` is the smallest complete example: a desktop that exists only in memory,
//! which the end-to-end tests build in place of the real platform.
//!
//! # A new Linux desktop (GNOME)
//!
//! Linux picks the desktop at run time (`linux/desktop.rs`: KDE Plasma or Hyprland).
//! Add a `linux/gnome/` module next to `linux/kde/` and `linux/hyprland/` with a
//! struct implementing [`Desktop`] (making monitors, streaming them, placing them), an
//! [`Output`] for its outputs, and [`Windows`] and [`Pointer`] where it has them, and
//! pick it in `linux/desktop.rs`. Sound, notifications, the tray, dialogs and the
//! system part are the same on every Linux desktop.

use anyhow::{Result, bail};

mod contract;
pub use contract::*;

// The sound part of the contract, and following the sound system's changes (the same
// loop for PipeWire's `pactl subscribe` and the fake desktop's). Windows (step 11c)
// hears of its devices' changes from the system itself (`IMMNotificationClient`,
// `IAudioEndpointVolumeCallback`) and needs none of the `unix` modules: the loop of
// `sound_events` reads lines from a pipe, `pulse` runs `pactl`, `process` other
// programs.
#[cfg(unix)]
mod process;
#[cfg(unix)]
mod pulse;
pub mod sound;
#[cfg(unix)]
mod sound_events;

#[cfg(all(
    target_os = "linux",
    feature = "desktop",
    not(feature = "fake-desktop")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    feature = "desktop",
    not(feature = "fake-desktop")
))]
pub use linux::*;

// A desktop inside the process, for the end-to-end tests only (see `fake/mod.rs`). It
// replaces the whole platform, so a test build never touches the real desktop.
#[cfg(feature = "fake-desktop")]
mod fake;
#[cfg(feature = "fake-desktop")]
pub use fake::*;

// The connection to the headset, the same with the fake desktop as with a real one.
mod net;
pub use net::*;

// The PC's own Wi-Fi, for diagnostics (the fake desktop's from its environment).
#[cfg(target_os = "linux")]
pub mod wifi;

// Signals that ask the program to quit, the same on every Linux desktop and the fake one.
pub mod signals;
pub use signals::quit_on_signals;

// The sockets a headset reaches the PC through, opened here or passed from outside
// (systemd, the sockets' watcher), the same with the fake desktop as with a real one.
#[cfg(unix)]
pub mod sockets;

// The lock file of the one Prostir of a user.
#[cfg(unix)]
mod lock;
#[cfg(unix)]
pub use lock::{Lock, held_by, is_held as runs_already};

/// Ends the process at once, without the exit handlers of the libraries it loaded:
/// for a thread left inside one of them (a probe of an encoder that hangs in its
/// driver), which those handlers would tear down under it. What is kept is saved
/// before.
#[cfg(unix)]
pub fn end_now(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` ends the process; nothing of it runs after.
    unsafe { libc::_exit(code) }
}

// The local API on the session bus, the same on every Linux desktop and the fake one.
#[cfg(target_os = "linux")]
pub mod local_api;

#[cfg(not(any(target_os = "linux", feature = "fake-desktop")))]
compile_error!("Prostir has no platform layer for this operating system yet (see platform/mod.rs)");

#[cfg(not(any(feature = "desktop", feature = "fake-desktop")))]
compile_error!("Prostir needs a desktop: the default feature `desktop`, or `fake-desktop`");

/// What people call a display of the PC (see [`PhysicalDisplay::label`]): its `make`
/// and `model` as it says them (the model alone when it starts with the make, as
/// `Dell Inc.` and `DELL U2720Q` do), else its `connector`. Any desktop's.
pub fn display_label(make: &str, model: &str, connector: &str) -> String {
    fn said(w: &str) -> Option<&str> {
        let w = w.trim();
        (!w.is_empty() && !w.eq_ignore_ascii_case("unknown")).then_some(w)
    }
    let first = |w: &str| w.split_whitespace().next().unwrap_or("").to_lowercase();
    let make = said(make).map(|m| m.trim_end_matches(['.', ',']));
    match (make, said(model)) {
        (Some(make), Some(model)) if first(model).starts_with(&first(make)) => model.to_owned(),
        (Some(make), Some(model)) => format!("{make} {model}"),
        (Some(one), None) | (None, Some(one)) => one.to_owned(),
        (None, None) => connector.to_owned(),
    }
}

/// A virtual monitor to make: its mode and scale.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorSpec {
    /// Size in pixels of the encoded stream.
    pub width: u32,
    pub height: u32,
    /// Output scale (UI scaling on that monitor), in whole percent as the desktop gets
    /// it ([`VirtualSpec::scale`]).
    pub scale: f64,
}

impl MonitorSpec {
    pub fn validate(&self) -> Result<()> {
        if !(320..=7680).contains(&self.width)
            || !(240..=4320).contains(&self.height)
            || !(0.5..=4.0).contains(&self.scale)
        {
            bail!(
                "monitor {}x{}@{} is out of range",
                self.width,
                self.height,
                self.scale
            );
        }
        Ok(())
    }
}

/// `2560x1440` or `3840x2160@1.5`, as on the command line and in saved files.
impl std::str::FromStr for MonitorSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (size, scale) = match s.split_once('@') {
            Some((a, b)) => (
                a,
                whole_percent(b.parse::<f64>().map_err(|e| format!("bad scale: {e}"))?),
            ),
            None => (s, 1.0),
        };
        let (w, h) = size.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
        let spec = MonitorSpec {
            width: w.parse().map_err(|e| format!("bad width: {e}"))?,
            height: h.parse().map_err(|e| format!("bad height: {e}"))?,
            scale,
        };
        spec.validate().map_err(|e| e.to_string())?;
        Ok(spec)
    }
}

impl MonitorSpec {
    /// The scale in percent, as [`VirtualSpec::scale`] has it.
    pub fn percent(&self) -> u16 {
        (self.scale * 100.0).round() as u16
    }
}

/// `scale` rounded to whole percent, the steps the desktop is given scales in: a
/// monitor is planned with the scale it gets.
pub fn whole_percent(scale: f64) -> f64 {
    (scale * 100.0).round() / 100.0
}

impl std::fmt::Display for MonitorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)?;
        if (self.scale - 1.0).abs() > f64::EPSILON {
            write!(f, "@{}", self.scale)?;
        }
        Ok(())
    }
}

/// What the tray asks the control loop to do.
pub enum TrayCommand {
    SetBitrate(u32),
    /// Clearer text at about 2 ms more encoding per frame.
    SetSharp(bool),
    /// Count this PC among users (see `usage`).
    SetCounting(bool),
    /// Drop all paired headsets: each must pair again.
    ForgetHeadsets,
    /// Ask the headset to do something (bring the monitors in front, say).
    Headset(prostir_proto::request::Asked<prostir_proto::request::Command>),
    /// Open the settings window.
    ShowSettings,
    /// Start with the session (`true`) or not; the choice is kept.
    SetAutostart(bool),
    /// Open the pairing window: a new headset can pair (and is answered when it asks
    /// for the PC) for a while.
    PairNewHeadset,
    StopPairing,
    Quit,
}

impl TrayCommand {
    /// What a click on the icon itself does (its menu aside): the settings window, the
    /// program's own window, opens or comes forward (one window per user).
    pub fn clicked() -> Self {
        Self::ShowSettings
    }
}

/// What the tray shows.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TrayStatus {
    /// "Wi-Fi" or "USB" while a headset is connected (the main one, else the first).
    pub connected: Option<&'static str>,
    /// Other headsets connected besides it.
    pub others: usize,
    pub monitors: usize,
    pub mirror: bool,
    /// Connected, but the headset is not worn: streaming paused.
    pub paused: bool,
    /// The headset's "No limits", while one is connected that takes commands.
    pub no_limits: Option<bool>,
    /// The headset's microphone is on, but its system does not let it record.
    pub mic_denied: bool,
    /// A display is not on the best encoder for it, or encodes on the CPU: which, and
    /// why (see `Displays::encoding_line`).
    pub encoding: Option<String>,
    /// The pairing window is open.
    pub pairing: bool,
    /// The desktop does not respond (a call into it has hung for a while).
    pub desktop_stuck: bool,
}

/// Why there is no pairing code typed on the PC (see `dialogs::ask_code`).
pub enum NoCode {
    Cancelled,
    /// The PC has no program to ask with.
    NoDialog,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A display of the PC is called by its maker and model where it says them, by
    /// its connector where it does not.
    #[test]
    fn displays_of_the_pc_have_human_names() {
        assert_eq!(
            display_label("Samsung", "S24R35x", "HDMI-A-1"),
            "Samsung S24R35x"
        );
        assert_eq!(
            display_label("Dell Inc.", "DELL U2720Q", "DP-1"),
            "DELL U2720Q"
        );
        assert_eq!(display_label(" ", "unknown", "eDP-1"), "eDP-1");
        assert_eq!(display_label("BOE", "", "eDP-1"), "BOE");
        assert_eq!(display_label("", "", "HDMI-A-1"), "HDMI-A-1");
    }

    /// A monitor is planned with the scale its output gets: whole percent.
    #[test]
    fn scales_are_whole_percent() {
        let spec: MonitorSpec = "2560x1440@1.333333".parse().unwrap();
        assert_eq!(spec.scale, 1.33);
        assert_eq!(spec.percent(), 133);
        assert_eq!(spec.to_string(), "2560x1440@1.33");
        let plain: MonitorSpec = "1920x1080".parse().unwrap();
        assert_eq!(plain.percent(), 100);
        assert_eq!(whole_percent(1.25), 1.25);
    }
}
