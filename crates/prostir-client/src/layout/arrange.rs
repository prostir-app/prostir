//! The presets, a new monitor and "Line up" (the code the PC runs too, in
//! `prostir_layout`), and how an arrangement is turned, carried and refitted.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use prostir_layout::circle::{self, Limits};
use prostir_layout::space::{Frame, Monitor, Seen, azimuth, covers, cuts, see};
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

/// A display of the PC itself shown: its stream's size in pixels, and where it is on
/// the PC's desktop (`x`, `y`, width, height in the desktop's pixels), where known.
pub type Physical<K> = (K, u32, u32, Option<[i32; 4]>);

/// Places every monitor for a preset, around the origin facing -Z (see
/// `prostir_layout::circle`). `monitors` hold `(id, width_px, height_px)`; they go left
/// to right in the given order (as on the desktop), in one row while it fits around the
/// user, else in rows. The PC's own displays shown (`physical`) go under the primary
/// monitor as one group, standing to each other as they stand on the PC's desktop (see
/// [`spread`]). `curve`: the monitors are drawn curved, on a cylinder this many times
/// the arrangement's radius (`None`: flat); the group then continues the cylinder of
/// the monitor above it.
pub fn arrange<K: Key>(
    preset: Preset,
    monitors: &[(K, u32, u32)],
    primary: Option<K>,
    physical: &[Physical<K>],
    curve: Option<f32>,
) -> HashMap<K, Placement> {
    let group = physical
        .first()
        .map(|first| (first.0, group_size(physical)));
    let mut out: HashMap<K, Placement> = circle::arrange(
        shape(preset),
        &pixel_sizes(monitors),
        primary,
        group.map(|(id, (w, h))| (id, prostir_layout::space::PixelSize::new(w, h))),
        curve,
        &Limits::default(),
    )
    .into_iter()
    .collect();
    if let Some((id, _)) = group
        && let Some(at) = out.remove(&id)
    {
        out.extend(spread(physical, at));
    }
    out
}

/// Whether two desktop rectangles (x, y, width, height) cover some of the same pixels.
fn cover_each_other(a: [i32; 4], b: [i32; 4]) -> bool {
    let (a, b) = (a.map(i64::from), b.map(i64::from));
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

/// Where each of `physical` is on one plane, in the desktop's pixels, in their order:
/// as on the PC's desktop when each says where it is there and none covers another;
/// else side by side, tops level (in their streams' pixels). Displays that show the
/// same (one desktop cloned onto both) would stand on top of each other: side by
/// side, each can be seen.
fn desk_rects<K: Key>(physical: &[Physical<K>]) -> Vec<(K, [f32; 4])> {
    let rects: Option<Vec<[i32; 4]>> = physical
        .iter()
        .map(|p| p.3.filter(|r| r[2] > 0 && r[3] > 0))
        .collect();
    let apart = |rects: &[[i32; 4]]| {
        rects
            .iter()
            .enumerate()
            .all(|(i, a)| rects[i + 1..].iter().all(|b| !cover_each_other(*a, *b)))
    };
    if let Some(rects) = rects.filter(|r| apart(r)) {
        return physical
            .iter()
            .zip(rects)
            .map(|(p, [x, y, w, h])| (p.0, [x as f32, y as f32, w as f32, h as f32]))
            .collect();
    }
    let mut x = 0.0;
    physical
        .iter()
        .map(|&(id, w, h, _)| {
            let r = [x, 0.0, w.max(1) as f32, h.max(1) as f32];
            x += w.max(1) as f32;
            (id, r)
        })
        .collect()
}

/// The box around `rects`: left, top, right, bottom.
fn bounds<K>(rects: &[(K, [f32; 4])]) -> [f32; 4] {
    rects.iter().fold(
        [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
        |[l, t, r, b], (_, [x, y, w, h])| [l.min(*x), t.min(*y), r.max(x + w), b.max(y + h)],
    )
}

/// How many of a stream's pixels a pixel of the desktop is, for the group: as for the
/// largest display in it (which then looks as large as it would alone).
fn stream_per_desk<K: Key>(physical: &[Physical<K>], rects: &[(K, [f32; 4])]) -> f32 {
    physical
        .iter()
        .zip(rects)
        .max_by_key(|(p, _)| u64::from(p.1) * u64::from(p.2))
        .map_or(1.0, |(p, (_, r))| p.1.max(1) as f32 / r[2].max(1.0))
}

/// The group of `physical` as one display: its size in a stream's pixels.
fn group_size<K: Key>(physical: &[Physical<K>]) -> (u32, u32) {
    let rects = desk_rects(physical);
    let [l, t, r, b] = bounds(&rects);
    let k = stream_per_desk(physical, &rects);
    let size = |v: f32| (v * k).round().clamp(1.0, 1e6) as u32;
    (size(r - l), size(b - t))
}

/// The gap between two displays of the PC glued to each other, in the desktop's
/// pixels: neighbours on the desktop never overlap where they meet.
const GLUE_GAP: f32 = 2.0;

/// Each of `physical` placed within the group placed at `at` (its box, the size
/// [`group_size`] says): side by side and above one another as on the PC's desktop,
/// on the group's plane, a little apart ([`GLUE_GAP`]); each in the middle of its
/// cell, as large as its stream's shape lets it be within the cell.
pub fn spread<K: Key>(physical: &[Physical<K>], at: Placement) -> Vec<(K, Placement)> {
    let rects = desk_rects(physical);
    let [l, t, r, b] = bounds(&rects);
    let per = at.width / (r - l).max(1.0);
    let (mx, my) = ((l + r) / 2.0, (t + b) / 2.0);
    rects
        .iter()
        .zip(physical)
        .map(|(&(id, [x, y, w, h]), &(_, sw, sh, _))| {
            let offset = Vec3::new((x + w / 2.0 - mx) * per, -(y + h / 2.0 - my) * per, 0.0);
            let (cw, ch) = ((w - GLUE_GAP).max(1.0), (h - GLUE_GAP).max(1.0));
            let aspect = sw.max(1) as f32 / sh.max(1) as f32;
            let p = Placement {
                position: at.position + at.rotation * offset,
                rotation: at.rotation,
                width: cw.min(ch * aspect) * per,
            };
            (id, p)
        })
        .collect()
}

/// `p`, which stood so to `from`, standing the same way to `to`: turned, carried
/// and made as much larger or smaller as `to` is than `from`. A display of the PC
/// keeps its place in the group when another one moves it (see `World::place_missing`
/// and `World::tidy_up`).
pub fn kept_to(p: Placement, from: Placement, to: Placement) -> Placement {
    let k = to.width / from.width.max(1e-6);
    let turn = to.rotation * from.rotation.inverse();
    Placement {
        position: to.position + turn * (p.position - from.position) * k,
        rotation: (turn * p.rotation).normalize(),
        width: p.width * k,
    }
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

/// Seen overlaps smaller than this share of the smaller monitor are a shared edge (as
/// for a new monitor, see `prostir_layout::place`).
const SLACK: f64 = 0.01;

/// Whether a monitor of `size` pixels at `p` would cover one of the placed ones of
/// `monitors` (other than itself, `id`) as seen from `center` tilted up by `tilt`, or go
/// through one in the room: no place for it.
pub fn covers_any<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    id: K,
    size: (u32, u32),
    p: &Placement,
    (center, tilt): ((Vec3, f32), f32),
) -> bool {
    let frame = frame(center, tilt);
    let height = p.width * size.1 as f32 / size.0.max(1) as f32;
    let mine = frame.unplace(*p);
    let corners = mine.corners(height);
    placed(placements, monitors)
        .iter()
        .filter(|m| m.id != id)
        .any(|m| {
            let o = frame.unplace(m.placement);
            let oh = m.height();
            covers(&o.corners(oh), &corners, SLACK) || cuts(&o, oh, &mine, height)
        })
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

/// `p` moved along the row it stands in: turned about the eye by `angle` (radians,
/// positive to the right as seen), or on a wall (`wall`) carried along its plane by
/// `metres`.
fn along_row(p: Placement, frame: &Frame, wall: bool, angle: f32, metres: f32) -> Placement {
    if wall {
        return Placement {
            position: p.position + frame.turn * Vec3::X * metres,
            ..p
        };
    }
    let turn = frame.turn * Quat::from_rotation_y(-angle) * frame.turn.inverse();
    Placement {
        position: frame.eye + turn * (p.position - frame.eye),
        rotation: (turn * p.rotation).normalize(),
        width: p.width,
    }
}

/// Where the middle of `p`'s left (`side` -1) or right (1) edge is seen, around the eye
/// of `frame` (radians, positive to the right), and along a wall in its plane.
fn edge(p: &Placement, frame: &Frame, side: f32) -> (f32, f32) {
    let at = frame.to_local(p.position + p.rotation * Vec3::X * (side * p.width / 2.0));
    (azimuth(at.as_dvec3()) as f32, at.x)
}

/// `p` moved along its row so its `side` edge (-1 left, 1 right) is where `slot`'s
/// is: a monitor taking another's place at its own width keeps that place's edge.
pub fn edge_to(p: Placement, slot: &Placement, side: f32, frame: &Frame, wall: bool) -> Placement {
    let (to, to_x) = edge(slot, frame, side);
    let (from, from_x) = edge(&p, frame, side);
    along_row(p, frame, wall, to - from, to_x - from_x)
}

/// Whether `a` is seen left of `b` from `frame`'s eye.
pub fn left_of(a: &Placement, b: &Placement, frame: &Frame) -> bool {
    let az = |p: &Placement| azimuth(frame.to_local(p.position).as_dvec3());
    az(a) < az(b)
}

/// How far apart two monitors side by side may be seen to overlap before one is
/// pushed clear of the other (radians: a hundredth of a millimetre a metre away), what
/// the last bits of a float leave of a seam.
const SEAM: f64 = 1e-5;

/// Monitor `id` of `monitors` is larger, or stands elsewhere, than before (another
/// resolution, a place swapped): the monitors beside it in its row (seen at its height
/// for at least half of the lower one) that it now covers as seen are pushed clear
/// along the row, each side as one piece with all beyond it, just as far as it
/// reaches into them: they meet it edge to edge. Around the eye; on a wall (`wall`)
/// along its plane. The rest stays.
pub fn make_room<K: Key>(
    placements: &mut HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    (center, tilt): ((Vec3, f32), f32),
    wall: bool,
    id: K,
) {
    let frame = frame(center, tilt);
    let all = placed(placements, monitors);
    let seen = see(&frame, &all);
    let (Some(me), Some(mine)) = (
        seen.iter().find(|s| s.id == id).copied(),
        placements.get(&id).copied(),
    ) else {
        return;
    };
    // It, and every monitor pushed along the row: what may go through another now.
    let mut moved = vec![id];
    for side in [1.0f32, -1.0] {
        let beyond: Vec<&Seen<K>> = seen
            .iter()
            .filter(|s| s.id != id && s.vertical_overlap(&me) >= 0.5)
            .filter(|s| (s.center_x() > me.center_x()) == (side > 0.0))
            .collect();
        let reach = |s: &Seen<K>| {
            if side > 0.0 {
                me.right - s.left
            } else {
                s.right - me.left
            }
        };
        let Some(into) = beyond
            .iter()
            .filter(|s| s.horizontal_overlap(&me) > 0.0)
            .copied()
            .max_by(|a, b| reach(a).total_cmp(&reach(b)))
            .filter(|s| reach(s) > SEAM)
        else {
            continue;
        };
        let Some(other) = placements.get(&into.id).copied() else {
            continue;
        };
        // How far its near edge goes to meet this one's.
        let (to, to_x) = edge(&mine, &frame, side);
        let (from, from_x) = edge(&other, &frame, -side);
        let (angle, metres) = (to - from, to_x - from_x);
        for s in beyond {
            if let Some(p) = placements.get_mut(&s.id) {
                *p = along_row(*p, &frame, wall, angle, metres);
                moved.push(s.id);
            }
        }
    }
    clear_above_and_below(placements, monitors, &frame, moved);
}

/// `p` carried up (`up` above 0) or down along the frame's up by `up` metres.
pub fn lifted(p: Placement, frame: &Frame, up: f32) -> Placement {
    Placement {
        position: p.position + frame.turn * Vec3::Y * up,
        ..p
    }
}

/// How far `p` (`ph` high) must go along the frame's up (`dir` 1) or down (-1) to cut
/// into none of `others` (place, height), to a tenth of a millimetre: none of them
/// further than both heights together.
pub fn clear_along_up(
    p: Placement,
    ph: f32,
    others: &[(Placement, f32)],
    frame: &Frame,
    dir: f32,
) -> f32 {
    let cut_any = |t: f32| {
        let at = lifted(p, frame, dir * t);
        others.iter().any(|(o, oh)| cuts(&at, ph, o, *oh))
    };
    if !cut_any(0.0) {
        return 0.0;
    }
    let mut hi = ph + others.iter().map(|o| o.1).fold(0.0, f32::max);
    while cut_any(hi) && hi < 100.0 {
        hi *= 2.0;
    }
    let mut lo = 0.0;
    while hi - lo > 1e-4 {
        let mid = (lo + hi) / 2.0;
        if cut_any(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// The monitors those of `moved` go through now (over or under them, or beside them
/// still) are carried up or down out of their way, and so on for the monitors those
/// then go through. A row glued over a row may be seen over it a little, as "Line up" leaves
/// it: only going through it is undone.
fn clear_above_and_below<K: Key>(
    placements: &mut HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    frame: &Frame,
    moved: Vec<K>,
) {
    let height = |k: K, p: &Placement| {
        monitors
            .iter()
            .find(|m| m.0 == k)
            .map(|&(_, w, h)| p.width * h as f32 / w.max(1) as f32)
    };
    let mut fixed = moved;
    let mut i = 0;
    while let Some(&m) = fixed.get(i) {
        i += 1;
        let Some(mp) = placements.get(&m).copied() else {
            continue;
        };
        let Some(mh) = height(m, &mp) else { continue };
        for &(o, ..) in monitors {
            if fixed.contains(&o) {
                continue;
            }
            let Some(op) = placements.get(&o).copied() else {
                continue;
            };
            let Some(oh) = height(o, &op) else { continue };
            if !cuts(&mp, mh, &op, oh) {
                continue;
            }
            let up = frame.to_local(op.position).y >= frame.to_local(mp.position).y;
            let dir = if up { 1.0 } else { -1.0 };
            let t = clear_along_up(op, oh, &[(mp, mh)], frame, dir);
            placements.insert(o, lifted(op, frame, dir * t));
            fixed.push(o);
        }
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
