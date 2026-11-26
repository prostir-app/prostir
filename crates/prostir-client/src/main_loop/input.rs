//! A frame's input, as the core takes it: where the head is, the controllers and
//! the tracked hands, the menu's gesture, and the wall clock for the menu.

use std::ptr;
use std::time::{Duration, Instant};

use anyhow::Result;
use glam::Vec3;
use openxr as xr;

use super::Kept;
use super::shell::Shell;
use super::start::{Stage, Xr};
use crate::interaction::{FrameInput, Head};
use crate::lang::Moment;
use crate::platform::meta;
use crate::platform::openxr::overlay;
use crate::platform::openxr::{self as xrutil, vec3};
use crate::settings::HandMode;

/// How often the wall clock is read for the menu's clock.
const WALL_EVERY: Duration = Duration::from_secs(1);

/// The headset's local time, broken down.
fn local_time() -> Option<libc::tm> {
    // SAFETY: plain libc time calls on a zeroed `tm`.
    unsafe {
        let now = libc::time(ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&raw const now, &raw mut tm).is_null()).then_some(tm)
    }
}

/// A moment of the local time, as the menu writes it.
fn moment(tm: &libc::tm) -> Moment {
    let small = |v: libc::c_int, max: u8| v.clamp(0, libc::c_int::from(max)) as u8;
    Moment {
        year: tm.tm_year + 1900,
        month: small(tm.tm_mon, 11),
        day: small(tm.tm_mday, 31),
        weekday: small(tm.tm_wday, 6),
        hour: small(tm.tm_hour, 23),
        minute: small(tm.tm_min, 59),
    }
}

/// The menu's clock, about once a second.
pub fn wall_clock(now: Instant, kept: &mut Kept) {
    if kept
        .wall_read
        .is_none_or(|at| now.saturating_duration_since(at) >= WALL_EVERY)
    {
        kept.wall_read = Some(now);
        kept.wall = local_time().map(|t| moment(&t));
    }
}

/// This frame's input at `time`, `dt` seconds after the last frame's.
pub fn read(
    xr: &Xr,
    stage: &Stage,
    shell: &mut Shell<'_>,
    kept: &mut Kept,
    now: Instant,
    time: xr::Time,
    dt: f32,
) -> Result<FrameInput> {
    let session = shell.session;
    let local = &stage.local;
    let hand_mode = shell.model.settings().hands;
    shell.controls.hands_enabled = hand_mode == HandMode::Control;
    shell.controls.feel = *shell.model.feel();
    // A profile changed without its event (a runtime does not always say): let go
    // of what was held, as for the event.
    if shell.controls.sync(session, local, time)? && kept.hands.busy() {
        kept.hands.cancel_all(&mut shell.model);
        shell.apply(now);
    }
    if !kept.focused {
        shell.controls.state = Default::default();
    }
    let (head, forward, right) =
        stage
            .view
            .locate(local, time)
            .map_or((Vec3::ZERO, -Vec3::Z, Vec3::X), |l| {
                let turn = xrutil::quat(l.pose.orientation);
                (vec3(l.pose.position), turn * -Vec3::Z, turn * Vec3::X)
            });
    // Tracked hands: joints for drawing, fingertips for touching the menu.
    let joints: [Option<[(Vec3, f32); 26]>; 2] = match &stage.hand_trackers {
        Some(trackers) if kept.focused && hand_mode != HandMode::Hidden => {
            std::array::from_fn(|h| {
                local
                    .locate_hand_joints(&trackers[h], time)
                    .ok()
                    .flatten()
                    .and_then(|l| overlay::joints(&l))
            })
        }
        _ => [None, None],
    };
    shell.controls.hands_from_joints(&joints, head, forward);
    // The left hand's menu gesture counts as the menu button.
    let menu_gesture = match &stage.hand_trackers {
        Some(t) if xr.exts.fb_hand_tracking_aim => {
            meta::hand_menu_gesture(&xr.instance, &t[0], local, time)
        }
        _ => false,
    };
    Ok(FrameInput {
        head: Some(Head {
            position: head,
            forward,
            right,
        }),
        dt,
        wall: kept.wall,
        focused: kept.focused,
        hands: shell.controls.state,
        joints,
        menu_button: shell.controls.menu_pressed(session),
        menu_gesture,
        align: shell.controls.align_pressed(session),
    })
}
