//! The presets, a new monitor and "Line up" (the code the PC runs too, in
//! `prostir_layout`), and how an arrangement is turned, carried and refitted.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use prostir_layout::circle::{self, Limits};
use prostir_layout::space::{Frame, Monitor};
use prostir_layout::view::Room;
use prostir_layout::{place, rows, tidy};

use super::{Key, OVERHEAD_PITCH, PixelSize, Placement};
use crate::settings::Preset;

/// The layout's name for `preset`.
fn shape(preset: Preset) -> circle::Preset {
    match preset {
        Preset::Arc => circle::Preset::Arc,
        Preset::Wall => circle::Preset::Wall,
        Preset::Overhead => circle::Preset::Overhead,
    }
}

/// `(id, width, height)` as the layout takes them.
fn pixel_sizes<K: Key>(monitors: &[(K, u32, u32)]) -> Vec<(K, prostir_layout::space::PixelSize)> {
    monitors
        .iter()
        .map(|&(id, w, h)| (id, prostir_layout::space::PixelSize::new(w, h)))
        .collect()
}

/// How far from the head `preset` puts `monitors` (a wall's distance): the circle
/// grows when they do not fit around the user (see `prostir_layout::circle`).
pub fn preset_radius<K: Key>(preset: Preset, monitors: &[(K, u32, u32)]) -> f32 {
    circle::radius_of(shape(preset), &pixel_sizes(monitors), &Limits::default())
}

/// Places every monitor for a preset, around the origin facing -Z (see
/// `prostir_layout::circle`). `monitors` and `mirror` hold `(id, width_px,
/// height_px)`; the monitors go left to right in the given order (as on the desktop),
/// in one row while it fits around the user, else in rows; the laptop mirror goes under
/// the primary monitor. `curve`: the monitors are drawn curved, on a cylinder this many
/// times the arrangement's radius (`None`: flat); the mirror then continues the
/// cylinder of the monitor above it.
pub fn arrange<K: Key>(
    preset: Preset,
    monitors: &[(K, u32, u32)],
    primary: Option<K>,
    mirror: Option<(K, u32, u32)>,
    curve: Option<f32>,
) -> HashMap<K, Placement> {
    circle::arrange(
        shape(preset),
        &pixel_sizes(monitors),
        primary,
        mirror.map(|m| pixel_sizes(&[m])[0]),
        curve,
        &Limits::default(),
    )
    .into_iter()
    .collect()
}

/// Where the arrangement is seen from and how it is turned: from `center` (eye, yaw),
/// tilted up by `tilt`; lying down, along the band tilted up overhead (seen from below,
/// left and right would make no sense). The tilt is the arrangement's (see [`tilt`]).
pub fn frame(center: (Vec3, f32), tilt: f32) -> Frame {
    Frame::tilted(center.0, center.1, tilt)
}

/// How far up an arrangement `preset` lays out is tilted, in radians: lying down, the
/// band overhead.
pub fn tilt(preset: Preset) -> f32 {
    if preset == Preset::Overhead {
        OVERHEAD_PITCH
    } else {
        0.0
    }
}

/// Every one of `monitors` (id, pixel size) that has a placement.
pub(super) fn placed<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
) -> Vec<Monitor<K>> {
    pixel_sizes(monitors)
        .into_iter()
        .filter_map(|(id, px)| {
            Some(Monitor {
                id,
                px,
                placement: *placements.get(&id)?,
            })
        })
        .collect()
}

/// The rows of `monitors` as seen from `frame`, top to bottom and each left to right:
/// monitors side by side as the eye sees them. Two neighbours in `previous` (the rows
/// the PC has) stay neighbours until one is clearly moved away, so a hand's tremor
/// never moves a monitor from one row to another (see `prostir_layout::rows`).
pub(super) fn rows_seen<K: Key>(
    room: &Room<K>,
    monitors: &[Monitor<K>],
    previous: &[Vec<K>],
) -> Vec<Vec<K>> {
    let ids: Vec<K> = monitors.iter().map(|m| m.id).collect();
    rows::rows_of(&ids, room, Some(previous))
}

/// "Line up" (the A button): the crookedness taken out of every row (a monitor turned
/// or hanging crooked, gaps, steps) and nothing more. How each row stands (tilted back,
/// overhead, turned) and where, the rows themselves and the gaps between them stay (see
/// `prostir_layout::tidy`). `previous`: the rows the PC has. `lid`: the laptop screen,
/// which is not lined up and moves only down out of the way of a monitor lined up over
/// it.
pub fn line_up<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    center: (Vec3, f32),
    tilt: f32,
    primary: Option<K>,
    previous: &[Vec<K>],
    lid: Option<K>,
) -> HashMap<K, Placement> {
    let all = placed(placements, monitors);
    let frame = frame(center, tilt);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    tidy::line_up(&all, &frame, primary, Some(&rows), lid)
        .into_iter()
        .collect()
}

/// Where a new monitor of `size` pixels goes next to the placed ones of `monitors`
/// (it is not among them), which do not move: at the end of the longest row with
/// room, the end nearest the gaze (a direction in the room); over the top row or under
/// the bottom one only when no row has room; covering nothing (see
/// `prostir_layout::place`). The first one goes straight ahead on a circle of
/// `radius`. `previous`: the rows the PC has.
#[allow(clippy::too_many_arguments)]
pub fn place_new<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    size: (u32, u32),
    center: (Vec3, f32),
    tilt: f32,
    gaze: Vec3,
    previous: &[Vec<K>],
    radius: f32,
) -> Placement {
    let all = placed(placements, monitors);
    let frame = frame(center, tilt);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    place::place_new(
        &all,
        &frame,
        gaze,
        prostir_layout::space::PixelSize::new(size.0, size.1),
        Some(&rows),
        &Limits {
            radius,
            ..Limits::default()
        },
    )
}

/// `p` for a stream now `now` pixels in size instead of `was`: the same size of a
/// pixel, centred where it was. A monitor that comes back with another resolution
/// under the same number (a profile, a rotated monitor) would otherwise keep its old
/// width in metres and look bigger or smaller than its neighbours.
pub fn refit(p: Placement, was: PixelSize, now: PixelSize) -> Placement {
    if was.0 == 0 || was == now {
        return p;
    }
    Placement {
        width: p.width * now.0 as f32 / was.0 as f32,
        ..p
    }
}

/// Which way an arrangement faces from `eye`: the mean direction of its monitors, in
/// the yaw convention of `center` (0 is -Z, positive turns towards -X). Summed in the
/// order of their numbers, so the same arrangement gives the same yaw to the bit.
pub fn facing_yaw<K: Key>(placements: &HashMap<K, Placement>, eye: Vec3) -> Option<f32> {
    let (sin, cos) = super::saved::ascending(placements)
        .filter_map(|k| placements.get(&k))
        .map(|p| {
            let d = p.position - eye;
            (-d.x).atan2(-d.z)
        })
        .fold((0.0f32, 0.0f32), |(s, c), a| (s + a.sin(), c + a.cos()));
    (sin.abs() > 1e-6 || cos.abs() > 1e-6).then(|| sin.atan2(cos))
}

/// `p`, laid out around `from` (eye and yaw, as the headset's `center`), standing the
/// same way around `to`: turned about the vertical by the difference of the yaws, the
/// eye carried to the eye. What is worked out as seen from the eye (rows, the desktop,
/// "Line up", the magnet) sees it from `to` as it saw it from `from`.
pub fn carried(p: &Placement, from: (Vec3, f32), to: (Vec3, f32)) -> Placement {
    let turn = Quat::from_rotation_y(to.1 - from.1);
    Placement {
        position: to.0 + turn * (p.position - from.0),
        rotation: (turn * p.rotation).normalize(),
        width: p.width,
    }
}

/// The yaw "In front" brings straight ahead, seen from `eye`: the `primary` monitor's
/// direction, else the mean direction of `monitors` (what the user arranged around
/// them, not the layout's old forward, which placing by hand has long left behind).
pub fn ahead<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[K],
    primary: Option<K>,
    eye: Vec3,
) -> Option<f32> {
    let direction = |p: &Placement| {
        let d = p.position - eye;
        (-d.x).atan2(-d.z)
    };
    primary
        .and_then(|id| placements.get(&id))
        .map(direction)
        .or_else(|| {
            let (sin, cos) = monitors
                .iter()
                .filter_map(|id| placements.get(id))
                .map(direction)
                .fold((0.0f32, 0.0f32), |(s, c), a| (s + a.sin(), c + a.cos()));
            (sin != 0.0 || cos != 0.0).then(|| sin.atan2(cos))
        })
}
