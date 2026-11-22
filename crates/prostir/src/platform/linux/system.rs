//! Linux system bits that do not depend on the desktop: files, the single instance,
//! memory; programs run with a time limit (shared, in `platform/process.rs`).

use std::net::Ipv4Addr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::platform::audio;
use crate::platform::linux::hyprland;

/// Whether the laptop lid is open (true on machines without a lid).
pub fn lid_open() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc/acpi/button/lid") else {
        return true;
    };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("state")).ok())
        .all(|state| !state.contains("closed"))
}

/// A directory only this user can use: `$XDG_RUNTIME_DIR`, or `~/.cache/prostir`
/// without it (never the shared `/tmp`, where another user could plant files).
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cache/prostir");
    let _ = std::fs::create_dir_all(&dir);
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir
}

/// Where per-user settings go: `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

/// One Prostir per user: a second one would fight over the monitors (and on
/// Hyprland, its start-up clean-up would remove the first one's), and the port is one
/// per PC anyway. The lock file is held for the life of the process and says who
/// holds it (`role`: `watcher` or `serve`) and its process id.
pub fn single_instance(role: &str) -> Result<crate::platform::Lock> {
    crate::platform::lock::take(&runtime_dir().join("prostir.lock"), role)
        .context("Prostir is already running for this user")
}

/// What says which desktop the program runs on, from the user's systemd when it
/// started Prostir before the desktop told it (a headset that asked at login): the
/// variables the service lacks. Without them, the log says how to pass them.
///
/// Called while no other thread reads the environment (the light phase, before the
/// full program starts any).
pub fn import_session_environment() {
    const NEEDED: [&str; 5] = [
        "WAYLAND_DISPLAY",
        "XDG_CURRENT_DESKTOP",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "DISPLAY",
        "XDG_SESSION_TYPE",
    ];
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return;
    }
    let shown = run(
        std::process::Command::new("systemctl").args(["--user", "show-environment"]),
        std::time::Duration::from_secs(5),
    );
    let text = shown
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=')
            && NEEDED.contains(&key)
            && std::env::var_os(key).is_none()
        {
            log::info!("from the user's systemd: {key}");
            // SAFETY: no other thread reads or writes the environment now (see above;
            // `run`'s readers only read pipes).
            unsafe { std::env::set_var(key, value) };
        }
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        log::warn!(
            "no desktop for Prostir: the user's systemd has no WAYLAND_DISPLAY. A desktop \
             that does not pass it (Hyprland) needs this in its config: \
             dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP \
             HYPRLAND_INSTANCE_SIGNATURE"
        );
    }
}

/// Where the time of the last notice of a new headset is kept, for every program of
/// this user (see `door::notice_due`).
pub fn notice_mark() -> PathBuf {
    runtime_dir().join("prostir-notice")
}

/// The sockets' watcher of this user, if one runs: its process id.
pub fn watcher() -> Option<u32> {
    crate::platform::lock::holder(&runtime_dir().join("prostir.lock"), "watcher")
}

/// Removes what a run that crashed left behind: Hyprland's monitors (without the
/// headset nobody sees their windows) and the sound devices.
pub fn clean_up_after_crash() {
    if hyprland::running() {
        hyprland::remove_leftovers();
    }
    audio::restore_after_crash();
}

/// After a session the freed buffers would stay with the process: hand them back to
/// the system, so Prostir waiting in the background stays small.
pub fn give_memory_back() {
    // SAFETY: plain glibc call; only releases memory that is already free.
    unsafe { libc::malloc_trim(0) };
}

/// Where per-user caches go: `$XDG_CACHE_HOME`, else `~/.cache`.
pub fn cache_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
}

/// What decides what the video encoders do, besides GStreamer: the graphics cards
/// (each render node's vendor, device and kernel driver), the kernel, the NVIDIA
/// driver's version, the VA-API drivers (each file's size and time), and the variables
/// that pick a VA driver. A probe of the encoders is kept until one of these changes.
pub fn encoder_drivers() -> Vec<String> {
    let mut parts = vec![
        std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default(),
        std::fs::read_to_string("/sys/module/nvidia/version").unwrap_or_default(),
    ];
    parts.extend(graphics_cards());
    for var in [
        "LIBVA_DRIVER_NAME",
        "LIBVA_DRIVERS_PATH",
        "GST_VA_ALL_DRIVERS",
    ] {
        parts.push(format!("{var}={}", std::env::var(var).unwrap_or_default()));
    }
    let dirs: Vec<PathBuf> = match std::env::var_os("LIBVA_DRIVERS_PATH") {
        Some(p) => std::env::split_paths(&p).collect(),
        None => [
            "/usr/lib/dri",
            "/usr/lib64/dri",
            "/usr/lib/x86_64-linux-gnu/dri",
            "/usr/lib/aarch64-linux-gnu/dri",
            "/run/opengl-driver/lib/dri",
        ]
        .iter()
        .map(PathBuf::from)
        .collect(),
    };
    let mut drivers: Vec<String> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("_drv_video.so"))
        })
        .map(|p| crate::encoder::file_identity(&p))
        .collect();
    drivers.sort();
    parts.extend(drivers);
    parts
}

/// Each render node with the card behind it: `renderD128 0x8086 0x56a0 xe`.
fn graphics_cards() -> Vec<String> {
    let read = |path: PathBuf| {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut cards: Vec<String> = std::fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .map(|e| {
            let device = e.path().join("device");
            let driver = std::fs::read_link(device.join("driver"))
                .ok()
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            format!(
                "{} {} {} {driver}",
                e.file_name().to_string_lossy(),
                read(device.join("vendor")),
                read(device.join("device")),
            )
        })
        .collect();
    cards.sort();
    cards
}

/// The PC's name, for the headset's list of PCs.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map_or_else(|_| "linux".into(), |s| s.trim().to_string())
}

/// IPv4 broadcast addresses of all interfaces that are up.
pub fn broadcast_addresses() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, which is walked read-only and freed once.
    unsafe {
        if libc::getifaddrs(&raw mut list) != 0 {
            return out;
        }
        let mut cur = list;
        while let Some(ifa) = cur.as_ref() {
            let flags = ifa.ifa_flags;
            let up = flags & libc::IFF_UP as u32 != 0
                && flags & libc::IFF_BROADCAST as u32 != 0
                && flags & libc::IFF_LOOPBACK as u32 == 0;
            let addr = ifa.ifa_ifu; // broadcast address when IFF_BROADCAST is set
            if up && !addr.is_null() && i32::from((*addr).sa_family) == libc::AF_INET {
                let sin = &*(addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
    }
    out
}

pub use crate::platform::process::run;
