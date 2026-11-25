//! The presets, a new monitor and "Line up" (the code the PC runs too, in
//! `prostir_layout`), and how an arrangement is turned, carried and refitted.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use log::debug;
use prostir_layout::circle::{self, Limits};
use prostir_layout::size::Sizes;
use prostir_layout::space::{Frame, Monitor, azimuth, covers, cuts, see};
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

/// How far from the head `preset` puts `monitors` (a wall's distance), `primary` in
/// the main row: the circle grows when they do not fit around the user (see
/// `prostir_layout::circle`).
pub fn preset_radius<K: Key>(
    preset: Preset,
    monitors: &[(K, u32, u32)],
    primary: Option<K>,
) -> f32 {
    preset_radius_sized(preset, monitors, primary, Sizes::default())
}

/// [`preset_radius`] with the monitors made as large as `sizes` says.
pub fn preset_radius_sized<K: Key>(
    preset: Preset,
    monitors: &[(K, u32, u32)],
    primary: Option<K>,
    sizes: Sizes,
) -> f32 {
    circle::radius_of(
        shape(preset),
        &pixel_sizes(monitors),
        primary,
        &Limits {
            sizes,
            ..Limits::default()
        },
    )
}

/// The circle new monitors go on: its radius, and how large they are made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle {
    pub radius: f32,
    pub sizes: Sizes,
}

/// A circle of this radius, the monitors of the standard size.
impl From<f32> for Circle {
    fn from(radius: f32) -> Self {
        Self {
            radius,
            sizes: Sizes::default(),
        }
    }
}

impl Circle {
    fn limits(self) -> Limits {
        Limits {
            radius: self.radius,
            sizes: self.sizes,
            ..Limits::default()
        }
    }
}

/// The largest scale at which `preset` lays out `monitors` (and the PC's own displays
/// `physical`) on its own circle with every one in view, each made by `sized(scale)`
/// (see `prostir_layout::circle::largest_scale`); `None` where not even the smallest
/// does.
pub fn largest_scale<K: Key>(
    preset: Preset,
    monitors: &[(K, u32, u32)],
    primary: Option<K>,
    physical: &[Physical<K>],
    curve: Option<f32>,
    sized: impl Fn(f32) -> Sizes,
) -> Option<f32> {
    let group = physical
        .first()
        .map(|first| (first.0, group_size(physical)));
    circle::largest_scale(
        shape(preset),
        &pixel_sizes(monitors),
        primary,
        group.map(|(id, (w, h))| (id, prostir_layout::space::PixelSize::new(w, h))),
        curve,
        &Limits::default(),
        sized,
    )
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
    arrange_sized(preset, monitors, primary, physical, curve, Sizes::default())
}

/// [`arrange`] with the monitors made as large as `sizes` says.
pub fn arrange_sized<K: Key>(
    preset: Preset,
    monitors: &[(K, u32, u32)],
    primary: Option<K>,
    physical: &[Physical<K>],
    curve: Option<f32>,
    sizes: Sizes,
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
        &Limits {
            sizes,
            ..Limits::default()
        },
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
    line_up_keeping(
        placements,
        monitors,
        center,
        tilt,
        primary,
        previous,
        lid,
        &[],
        None,
    )
}

/// [`line_up`], each of `glued` kept glued as well (see
/// `prostir_layout::tidy::line_up_keeping`): pairs [`glued_pairs`] gave before the
/// monitors were carried away from the eye. The pairs it keeps glued go into `kept`.
#[allow(clippy::too_many_arguments)]
pub fn line_up_keeping<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    center: (Vec3, f32),
    tilt: f32,
    primary: Option<K>,
    previous: &[Vec<K>],
    lid: Option<K>,
    glued: &[(K, K)],
    kept: Option<&mut Vec<(K, K)>>,
) -> HashMap<K, Placement> {
    let all = placed(placements, monitors);
    let frame = frame(center, tilt);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    tidy::line_up_keeping(&all, &frame, primary, Some(&rows), lid, glued, kept)
        .into_iter()
        .collect()
}

/// The pairs of monitors [`line_up`] keeps glued (one over or under the other, their
/// seam closed as seen), the one of the row nearer the main one first, in order.
pub fn glued_pairs<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    (center, tilt): ((Vec3, f32), f32),
    primary: Option<K>,
    previous: &[Vec<K>],
    lid: Option<K>,
) -> Vec<(K, K)> {
    let all = placed(placements, monitors);
    let frame = frame(center, tilt);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    tidy::glued_pairs(&all, &frame, primary, Some(&rows), lid)
}

/// Where a new monitor of `size` pixels goes next to the placed ones of `monitors`
/// (it is not among them), which do not move: at the end of the longest row with
/// room, the end nearest the gaze (a direction in the room); over the top row or under
/// the bottom one only when no row has room; covering nothing (see
/// `prostir_layout::place`). The first one goes straight ahead on `circle` (a radius,
/// or one with the size new monitors are made). `previous`: the rows the PC has.
#[allow(clippy::too_many_arguments)]
pub fn place_new<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    size: (u32, u32),
    center: (Vec3, f32),
    tilt: f32,
    gaze: Vec3,
    previous: &[Vec<K>],
    circle: impl Into<Circle>,
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
        &circle.into().limits(),
    )
}

/// [`place_new`], but where that place is out of view (see
/// `prostir_layout::space::in_view`: a monitor too wide for any row, over the head; or
/// over a row of tall ones): one too wide for the row's circle in the main row instead,
/// at the seam nearest the gaze (see `prostir_layout::place::into_row`): then `true`,
/// and the neighbours it covers there are to be pushed aside along the row (see
/// [`make_room`]). Another where it was, further away (see [`back_into_view`]), where
/// that is in view; else, with `any`, into the main row as well. With no room in the
/// row (it would go all the way around), a monitor too wide for the circle goes over or
/// under a row on a wider circle (see [`FAR_ONE`]) where that is in view.
#[allow(clippy::too_many_arguments)]
pub fn place_in_view<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    size: (u32, u32),
    center: (Vec3, f32),
    tilt: f32,
    gaze: Vec3,
    previous: &[Vec<K>],
    circle: impl Into<Circle>,
    any: bool,
) -> (Placement, bool) {
    let circle = circle.into();
    let at = place_new(
        placements, monitors, size, center, tilt, gaze, previous, circle,
    );
    if in_view(&at, (center, tilt)) {
        return (at, false);
    }
    let frame = frame(center, tilt);
    let all = placed(placements, monitors);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    let px = prostir_layout::space::PixelSize::new(size.0, size.1);
    let new = circle.sizes.others;
    if let Some(p) = place::into_row(&all, &frame, gaze, px, Some(&rows), false, new) {
        return (p, true);
    }
    if let Some(p) = back_into_view(at, px, &all, &frame) {
        return (p, false);
    }
    if any && let Some(p) = place::into_row(&all, &frame, gaze, px, Some(&rows), true, new) {
        return (p, true);
    }
    // No room in the row: over or under one, further away, so seen lower.
    let limits = circle.limits();
    let far = place::place_within(&all, &frame, gaze, px, Some(&rows), &limits, FAR_ONE);
    (
        if in_view(&far, (center, tilt)) {
            far
        } else {
            at
        },
        false,
    )
}

/// `at`, a place over (or under) a row out of view for a monitor of stream size `px`,
/// carried away from the eye as if its circle were larger (see [`further`]), its
/// bottom (top) edge seen where it was, on the row: seen nearer eye level, and smaller.
/// The nearest such place where its middle is in view and it covers and goes through
/// none of `monitors`, its edges no further than `circle::VIEW_RADIUS` from the eye.
fn back_into_view<K: Key>(
    at: Placement,
    px: prostir_layout::space::PixelSize,
    monitors: &[Monitor<K>],
    frame: &Frame,
) -> Option<Placement> {
    let local = frame.unplace(at);
    let height = local.width * px.aspect();
    let up = if local.position.y >= 0.0 { 1.0 } else { -1.0 };
    let edge = local.position.y - up * height / 2.0;
    let along = Vec3::new(local.position.x, 0.0, local.position.z).length();
    let reach = (along * along + local.width * local.width / 4.0).sqrt();
    if along < 1e-6 || reach >= circle::VIEW_RADIUS {
        return None;
    }
    let others: Vec<(Placement, f32)> = monitors
        .iter()
        .map(|m| (frame.unplace(m.placement), m.height()))
        .collect();
    // Under a row, the same with heights turned over.
    let flip = |p: Placement| Placement {
        position: Vec3::new(p.position.x, up * p.position.y, p.position.z),
        ..p
    };
    let flipped: Vec<(Placement, f32)> = others.iter().map(|&(o, h)| (flip(o), h)).collect();
    // Carried `k` times as far, resting on the row as seen (see `circle::clear_of`).
    let at_k = |k: f32| {
        let mut p = frame.unplace(further(at, frame, k, false));
        let s = Vec3::new(p.position.x, 0.0, p.position.z).length() / along;
        let rest = up * edge * s;
        p.position.y = up * (rest + height / 2.0);
        let bottom = circle::clear_of(&[(flip(p), height)], &flipped).map_or(rest, |c| c.max(rest));
        p.position.y = up * (bottom + height / 2.0);
        p
    };
    let seen = |p: &Placement| prostir_layout::space::in_view(p);
    let most = circle::VIEW_RADIUS / reach;
    if !seen(&at_k(most)) {
        return None;
    }
    let (mut lo, mut hi) = (1.0f32, most);
    while hi - lo > 1e-3 {
        let mid = (lo + hi) / 2.0;
        if seen(&at_k(mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let clear = |p: &Placement| {
        let mine = p.corners(height);
        others
            .iter()
            .all(|(o, oh)| !covers(&o.corners(*oh), &mine, SLACK) && !cuts(o, *oh, p, height))
    };
    (0..=20)
        .map(|i| hi + (most - hi) * i as f32 / 20.0)
        .map(at_k)
        .find(|p| clear(p))
        .map(|p| frame.place(p))
}

/// Where the placeholder of a new monitor of `size` pixels stands while its picture is
/// on the way, when [`place_in_view`] puts it into the main row at `into` (its
/// neighbours make room there only once it comes): where it goes through nothing, in
/// view. Over or under a row on a wider circle (see [`FAR_ONE`]) where that is in view
/// (it covers nothing there either); else at `into`, carried away from the eye (see
/// [`further`]) until it goes through none of them, seen where it will be, behind its
/// neighbours. The other arguments are those of [`place_in_view`].
#[allow(clippy::too_many_arguments)]
pub fn place_waiting<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    size: (u32, u32),
    center: (Vec3, f32),
    tilt: f32,
    gaze: Vec3,
    previous: &[Vec<K>],
    circle: impl Into<Circle>,
    into: Placement,
) -> Placement {
    let frame = frame(center, tilt);
    let all = placed(placements, monitors);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    let px = prostir_layout::space::PixelSize::new(size.0, size.1);
    let limits = circle.into().limits();
    let far = place::place_within(&all, &frame, gaze, px, Some(&rows), &limits, FAR_ONE);
    if in_view(&far, (center, tilt)) {
        return far;
    }
    let height = into.width * px.aspect();
    let clear = |p: &Placement| {
        all.iter()
            .all(|m| !cuts(&m.placement, m.height(), p, height))
    };
    (1..=40)
        .map(|i| further(into, &frame, 1.0 + 0.05 * i as f32, false))
        .find(|p| clear(p))
        .unwrap_or_else(|| further(into, &frame, 3.0, false))
}

/// The most a monitor too wide for every row's circle takes around the user, over or
/// under a row, where the row has no room for it (90°): further away than where it takes
/// `circle::MAX_ONE`, and so seen nearer eye level.
const FAR_ONE: f32 = std::f32::consts::FRAC_PI_2;

/// The monitors among `monitors` (placed in `placements`) that are alone in a row whose
/// middle is out of view from `center` tilted up by `tilt` (see
/// `prostir_layout::space::in_view`), in the order of `monitors`. `previous`: the rows
/// the PC has.
pub fn alone_out_of_view<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    (center, tilt): ((Vec3, f32), f32),
    previous: &[Vec<K>],
) -> Vec<K> {
    let all = placed(placements, monitors);
    let frame = frame(center, tilt);
    let rows = rows_seen(&Room::new(&frame, &all), &all, previous);
    monitors
        .iter()
        .map(|m| m.0)
        .filter(|k| {
            rows.iter().any(|r| r.as_slice() == [*k])
                && placements
                    .get(k)
                    .is_some_and(|p| !in_view(p, (center, tilt)))
        })
        .collect()
}

/// Whether the middle of `p` is in view from `center` tilted up by `tilt` (see
/// `prostir_layout::space::in_view`).
pub fn in_view(p: &Placement, (center, tilt): ((Vec3, f32), f32)) -> bool {
    prostir_layout::space::in_view(&frame(center, tilt).unplace(*p))
}

/// `p` carried straight away from the up axis through the eye of `frame`, at the same
/// height and turned the same way, as if the circle it stands on were `k` times as
/// large: its vertical edges `k` times as far from the axis (a row of monitors of any
/// widths on one circle stays on one circle). Where it was around the user, seen nearer
/// eye level and smaller. On a wall (`wall`) its middle goes `k` times as far, so the
/// wall stays one plane.
pub fn further(p: Placement, frame: &Frame, k: f32, wall: bool) -> Placement {
    let mut local = frame.unplace(p);
    let along = Vec3::new(local.position.x, 0.0, local.position.z).length();
    let scale = if wall || along < 1e-6 {
        k
    } else {
        let half = local.width / 2.0;
        let edge = (along * along + half * half).sqrt() * k;
        (edge * edge - half * half).max(0.0).sqrt() / along
    };
    local.position.x *= scale;
    local.position.z *= scale;
    frame.place(local)
}

/// The direction at eye level from `center` (tilted up by `tilt`) towards where `p`
/// is seen around the user: where to look for a monitor's new place near its old one.
pub fn level_towards(p: &Placement, (center, tilt): ((Vec3, f32), f32)) -> Vec3 {
    let frame = frame(center, tilt);
    let az = azimuth(frame.to_local(p.position).as_dvec3()) as f32;
    frame.turn * Vec3::new(az.sin(), 0.0, -az.cos())
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

/// Whether two of the placed ones of `monitors` go through each other in the room.
pub fn cut_any<K: Key>(placements: &HashMap<K, Placement>, monitors: &[(K, u32, u32)]) -> bool {
    let all = placed(placements, monitors);
    all.iter().enumerate().any(|(i, a)| {
        all[i + 1..]
            .iter()
            .any(|b| cuts(&a.placement, a.height(), &b.placement, b.height()))
    })
}

/// How far apart each of `pairs` (one over or under the other, as [`glued_pairs`] gives
/// them) is seen from `center` tilted up by `tilt`, in the middle of where the edges they
/// face each other with run along each other: radians up or down, above 0 apart, below
/// overlapping. None for a pair not both placed or whose edges do not run along each
/// other.
pub fn seam_gaps<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    (center, tilt): ((Vec3, f32), f32),
    pairs: &[(K, K)],
) -> Vec<Option<f32>> {
    let frame = frame(center, tilt);
    let local = |k: K| {
        let m = monitor_of(placements, monitors, k)?;
        Some((frame.unplace(m.placement), m.height()))
    };
    let az = |v: Vec3| v.x.atan2(-v.z);
    let around = |a: f32, c: f32| {
        c + (a - c + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
    };
    // Where the edge from `a` to `b` is seen up or down at azimuth `at`.
    let seen_at = |(a, b): (Vec3, Vec3), at: f32| {
        let d = b - a;
        let (c, s) = (at.cos(), at.sin());
        let across = d.x * c + d.z * s;
        let t = if across.abs() < 1e-9 {
            0.5
        } else {
            -(a.x * c + a.z * s) / across
        };
        let q = a + d * t.clamp(0.0, 1.0);
        q.y.atan2(Vec3::new(q.x, 0.0, q.z).length())
    };
    pairs
        .iter()
        .map(|&(p, u)| {
            let (Some((pp, ph)), Some((up, uh))) = (local(p), local(u)) else {
                return None;
            };
            let over = up.position.y > pp.position.y;
            let [ptl, ptr, pbr, pbl] = pp.corners(ph);
            let [utl, utr, ubr, ubl] = up.corners(uh);
            let (ue, pe) = if over {
                ((ubl, ubr), (ptl, ptr))
            } else {
                ((utl, utr), (pbl, pbr))
            };
            let c = az(up.position);
            let span = |(a, b): (Vec3, Vec3)| {
                let (x, y) = (around(az(a), c), around(az(b), c));
                (x.min(y), x.max(y))
            };
            let ((ul, ur), (pl, pr)) = (span(ue), span(pe));
            let (lo, hi) = (ul.max(pl), ur.min(pr));
            if hi <= lo {
                return None;
            }
            let mid = (lo + hi) / 2.0;
            let (at_u, at_p) = (seen_at(ue, mid), seen_at(pe, mid));
            Some(if over { at_u - at_p } else { at_p - at_u })
        })
        .collect()
}

/// Every two of the placed ones of `monitors`, one over or under the other, seen to meet
/// within `within` (radians up or down, apart or overlapping; see [`seam_gaps`]), in
/// both orders: the seams that are to stay closed, whether or not "Line up" took them
/// for glued.
pub fn meeting_pairs<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    at: ((Vec3, f32), f32),
    within: f32,
) -> Vec<(K, K)> {
    let ids: Vec<K> = placed(placements, monitors).iter().map(|m| m.id).collect();
    let mut pairs = Vec::new();
    for (i, &a) in ids.iter().enumerate() {
        for &b in &ids[i + 1..] {
            let gap = seam_gaps(placements, monitors, at, &[(a, b)]);
            if gap
                .first()
                .copied()
                .flatten()
                .is_some_and(|g| g.abs() <= within)
            {
                pairs.push((a, b));
                pairs.push((b, a));
            }
        }
    }
    pairs
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
/// resolution, a place swapped): a monitor beside it in its row (seen at its height
/// for at least half of the lower one) that it now covers as seen is pushed clear
/// along the row, just as far as it reaches into it (they meet edge to edge), and so on
/// for what that one then covers: only the chain that touches moves, not monitors
/// further along or behind the user. Around the eye; on a wall (`wall`) along its
/// plane. Then what any of them goes through over or under it moves up or down out of
/// its way (see [`clear_above_and_below`]). The displays of `group` (the PC's own,
/// shown as one group) move as one piece, all the same way.
pub fn make_room<K: Key>(
    placements: &mut HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    (center, tilt): ((Vec3, f32), f32),
    wall: bool,
    group: &[K],
    id: K,
) {
    let frame = frame(center, tilt);
    let piece = |k: K| -> Vec<K> {
        if group.contains(&k) {
            group.to_vec()
        } else {
            vec![k]
        }
    };
    // Which side of a monitor another is on, as they stood before any was pushed: a
    // long push (a wide monitor put into the row) may carry a monitor past the middle
    // of the next one.
    let before = placements.clone();
    let mut moved = piece(id);
    let mut work = vec![(id, 1.0f32), (id, -1.0)];
    // Each push goes further out along the row: a bound only against a float gone wrong.
    let mut left = 8 * monitors.len().max(1);
    while let Some((m, side)) = work.pop() {
        if left == 0 {
            debug!(
                "making room along the row: out of passes, {} left",
                work.len() + 1
            );
            break;
        }
        left -= 1;
        let Some(mine) = monitor_of(placements, monitors, m) else {
            continue;
        };
        let pushed_with = piece(m);
        for &(o, ..) in monitors {
            if pushed_with.contains(&o) {
                continue;
            }
            let Some(other) = monitor_of(placements, monitors, o) else {
                continue;
            };
            let pair = see(&frame, &[mine, other]);
            let (me, it) = (&pair[0], &pair[1]);
            let reach = if side > 0.0 {
                me.right - it.left
            } else {
                it.right - me.left
            };
            let stood = |k: K, now: Monitor<K>| Monitor {
                placement: before.get(&k).copied().unwrap_or(now.placement),
                ..now
            };
            let was = see(&frame, &[stood(m, mine), stood(o, other)]);
            if it.vertical_overlap(me) < 0.5
                || (was[1].center_x() > was[0].center_x()) != (side > 0.0)
                || it.horizontal_overlap(me) <= 0.0
                || reach <= SEAM
            {
                continue;
            }
            // How far its near edge goes to meet this one's.
            let (to, to_x) = edge(&mine.placement, &frame, side);
            let (from, from_x) = edge(&other.placement, &frame, -side);
            let (angle, metres) = (to - from, to_x - from_x);
            // Every display of a piece moved pushes on: a monitor beyond the group's
            // far end is pushed by the display it now covers.
            for k in piece(o) {
                if let Some(p) = placements.get_mut(&k) {
                    *p = along_row(*p, &frame, wall, angle, metres);
                    if !moved.contains(&k) {
                        moved.push(k);
                    }
                }
                work.push((k, side));
            }
        }
    }
    clear_above_and_below(placements, monitors, &frame, group, moved);
}

/// Monitor `k` of `monitors` where it stands, if it has a place.
fn monitor_of<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    k: K,
) -> Option<Monitor<K>> {
    let &(_, w, h) = monitors.iter().find(|m| m.0 == k)?;
    Some(Monitor {
        id: k,
        px: prostir_layout::space::PixelSize::new(w, h),
        placement: *placements.get(&k)?,
    })
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
/// still) are carried up or down out of their way, just far enough, and so on for the
/// monitors those then go through, until none goes through another (those moved are
/// looked at again). The displays of `group` move as one piece. A row glued over a row
/// may be seen over it a little, as "Line up" leaves it: only going through it is
/// undone.
fn clear_above_and_below<K: Key>(
    placements: &mut HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    frame: &Frame,
    group: &[K],
    moved: Vec<K>,
) {
    let height = |k: K, p: &Placement| {
        monitors
            .iter()
            .find(|m| m.0 == k)
            .map(|&(_, w, h)| p.width * h as f32 / w.max(1) as f32)
    };
    let piece = |k: K| -> Vec<K> {
        if group.contains(&k) {
            group.to_vec()
        } else {
            vec![k]
        }
    };
    let mut work = moved;
    // Each lift goes further up or down: a bound only against a float gone wrong.
    let mut left = 8 * monitors.len().max(1) + work.len();
    while let Some(m) = work.pop() {
        if left == 0 {
            debug!(
                "clearing over and under: out of passes, {} left",
                work.len() + 1
            );
            break;
        }
        left -= 1;
        let Some(mp) = placements.get(&m).copied() else {
            continue;
        };
        let Some(mh) = height(m, &mp) else { continue };
        let with = piece(m);
        for &(o, ..) in monitors {
            if with.contains(&o) {
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
            // The whole piece, as far as the member that needs it most.
            let members = piece(o);
            let t = members
                .iter()
                .filter_map(|&k| {
                    let p = *placements.get(&k)?;
                    Some(clear_along_up(p, height(k, &p)?, &[(mp, mh)], frame, dir))
                })
                .fold(0.0f32, f32::max);
            for k in members {
                if let Some(p) = placements.get_mut(&k) {
                    *p = lifted(*p, frame, dir * t);
                    work.push(k);
                }
            }
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
