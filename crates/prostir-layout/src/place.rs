//! A new monitor next to the arrangement, moving nothing else.
//!
//! The candidates: past either end of every row, on that row's surface (the cylinder
//! around the eye its end monitor stands on, or that monitor's plane on a wall), its
//! middle level with the end monitor's; and over every monitor of the top row and under
//! every monitor of the bottom row, on its surface. A candidate that would cover a
//! monitor (or go through one in the room), or take its row past the widest arc, is
//! dropped; of the rest, the one seen nearest the gaze wins. If none is left, it goes
//! over everything (or under, or behind the user), where nothing can be.

use glam::{DVec3, Vec3};

use crate::circle::{EYE_DROP, Limits, clear_of, on_cylinder};
use crate::rows::rows_of;
use crate::space::{
    Frame, METRES_PER_PIXEL, Monitor, PixelSize, Placement, Seen, azimuth, covers, cuts,
    seen_local, unwrap,
};
use crate::view::Room;

/// Seen overlaps smaller than this fraction of the smaller monitor are a shared edge.
const SLACK: f64 = 0.01;

/// Where a new monitor of stream size `px` goes next to `monitors`, seen from `frame`:
/// at an end of a row, the longest row with room first (the main one) and its end
/// nearest the gaze (`gaze`: a direction in the room); over the top row or under the
/// bottom one, nearest the gaze, only when no row has room for it. A new row is the
/// user's to make, by hand: one looks at the menu or a monitor when adding, not where
/// the new one should go. `rows`: the rows if known (else worked out from what is
/// seen). Nothing else moves.
pub fn place_new<K: Copy + Ord>(
    monitors: &[Monitor<K>],
    frame: &Frame,
    gaze: Vec3,
    px: PixelSize,
    rows: Option<&[Vec<K>]>,
    limits: &Limits,
) -> Placement {
    if monitors.is_empty() {
        let w = px.w as f32 * METRES_PER_PIXEL;
        return frame.place(on_cylinder(
            0.0,
            -EYE_DROP,
            w,
            limits.radius.max(w / 2.0 * 1.001),
        ));
    }
    let room = Room::new(frame, monitors);
    let seen = room.seen();
    let rows: Vec<Vec<K>> = match rows {
        Some(r) => r.to_vec(),
        None => {
            let ids: Vec<K> = monitors.iter().map(|m| m.id).collect();
            rows_of(&ids, &room, None)
        }
    };
    let local: Vec<(K, Placement, f32)> = monitors
        .iter()
        .map(|m| (m.id, frame.unplace(m.placement), m.height()))
        .collect();
    let find = |k: K| local.iter().find(|m| m.0 == k).copied();
    let seen_of = |k: K| seen.iter().find(|s| s.id == k).copied();

    // Candidates in the frame, each with whether it extends a row (and which).
    let mut candidates: Vec<(Placement, Option<usize>)> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let ends = [(row.first(), -1.0f32), (row.last(), 1.0)];
        for (end, side) in ends {
            let Some((_, e, eh)) = end.and_then(|&k| find(k)) else {
                continue;
            };
            let width = px.w as f32 * e.width / px_w_of(monitors, end.copied());
            let c = beside(&e, eh, width, side);
            candidates.extend(c.map(|p| (p, Some(r))));
        }
    }
    let rings = [(rows.first(), 1.0f32), (rows.last(), -1.0)];
    for (row, up) in rings {
        for &k in row.into_iter().flatten() {
            let Some((_, e, eh)) = find(k) else { continue };
            let width = px.w as f32 * e.width / px_w_of(monitors, Some(k));
            let height = width * px.aspect();
            candidates.extend(over(&e, eh, width, height, up).map(|p| (p, None)));
        }
    }

    let gaze_local = (frame.turn.inverse() * gaze)
        .as_dvec3()
        .normalize_or(DVec3::NEG_Z);
    let fits = |p: &Placement, row: Option<usize>| -> bool {
        if !(p.position.is_finite() && p.rotation.is_finite() && p.width.is_finite()) {
            return false;
        }
        let height = p.width * px.aspect();
        let mine = p.corners(height);
        // Seen apart by a hair is not enough: nearer the eye than its neighbour, a
        // monitor seen next to it can still go through it in the room.
        if local
            .iter()
            .any(|(_, o, oh)| covers(&o.corners(*oh), &mine, SLACK) || cuts(o, *oh, p, height))
        {
            return false;
        }
        let s = seen_local(p, px, 0.0);
        // A row it extends stays within the widest arc.
        row.is_none_or(|r| {
            let members: Vec<Seen<K>> = rows[r].iter().filter_map(|&k| seen_of(k)).collect();
            let left = members.iter().map(|m| m.left).fold(f64::INFINITY, f64::min);
            let right = members
                .iter()
                .map(|m| m.right)
                .fold(f64::NEG_INFINITY, f64::max);
            let c = unwrap(s.center_x(), (left + right) / 2.0);
            let (l, rr) = (c - s.width() / 2.0, c + s.width() / 2.0);
            rr.max(right) - l.min(left) <= f64::from(limits.arc) + 1e-6
        })
    };
    // Extending a row before making a new one, a longer row before a shorter one.
    let rank = |row: Option<usize>| match row {
        Some(r) => (0, usize::MAX - rows[r].len()),
        None => (1, 0),
    };
    let best = candidates
        .iter()
        .filter(|(p, row)| fits(p, *row))
        .min_by(|a, b| {
            let d = |p: &Placement| -> f64 {
                let dir = p.position.as_dvec3().normalize_or(DVec3::NEG_Z);
                dir.dot(gaze_local).clamp(-1.0, 1.0).acos()
            };
            rank(a.1)
                .cmp(&rank(b.1))
                .then_with(|| d(&a.0).total_cmp(&d(&b.0)))
        })
        .map(|c| c.0);
    let chosen = best.unwrap_or_else(|| fallback(&local, &seen, gaze_local, px, limits));
    frame.place(chosen)
}

/// The stream width of `k` (pixels), to keep the new monitor's pixels the size of its
/// neighbour's.
fn px_w_of<K: Copy + PartialEq>(monitors: &[Monitor<K>], k: Option<K>) -> f32 {
    k.and_then(|k| monitors.iter().find(|m| m.id == k))
        .map_or(1.0, |m| m.px.w.max(1) as f32)
}

/// Whether `e` faces the vertical axis through the eye (it stands on a cylinder around
/// the user), else it is part of a flat wall.
fn on_a_cylinder(e: &Placement) -> bool {
    let n = e.normal();
    let to_axis = Vec3::new(-e.position.x, 0.0, -e.position.z).normalize_or_zero();
    let flat = Vec3::new(n.x, 0.0, n.z).normalize_or_zero();
    flat.dot(to_axis) > 10f32.to_radians().cos()
}

/// Beside `e` (`eh` high), on `side` (-1 left, 1 right), `width` wide, middles level,
/// on `e`'s surface.
fn beside(e: &Placement, eh: f32, width: f32, side: f32) -> Option<Placement> {
    if on_a_cylinder(e) {
        let corners = e.corners(eh);
        let (top, bottom) = if side > 0.0 {
            (corners[1], corners[2])
        } else {
            (corners[0], corners[3])
        };
        let edge = (top + bottom) / 2.0;
        let radius = Vec3::new(edge.x, 0.0, edge.z).length();
        if width >= 2.0 * radius {
            return None;
        }
        let edge_az = azimuth(edge.as_dvec3()) as f32;
        let a = 2.0 * (width / 2.0 / radius).asin();
        let yaw = edge_az + side * a / 2.0;
        Some(on_cylinder(yaw, e.position.y, width, radius))
    } else {
        let right = e.rotation * Vec3::X;
        Some(Placement {
            position: e.position + right * side * (e.width + width) / 2.0,
            rotation: e.rotation,
            width,
        })
    }
}

/// Over (`up` 1) or under (`up` -1) `e`, centred on it, on its surface; on a cylinder
/// resting on it as seen (see [`clear_of`]: a flat monitor's edge bulges towards the
/// eye in its middle).
fn over(e: &Placement, eh: f32, width: f32, height: f32, up: f32) -> Option<Placement> {
    if on_a_cylinder(e) {
        let corners = e.corners(eh);
        let radius = Vec3::new(corners[0].x, 0.0, corners[0].z)
            .length()
            .max(width / 2.0 * 1.001);
        let az = azimuth(e.position.as_dvec3()) as f32;
        let edge_y = e.position.y + up * eh / 2.0;
        let at = |edge: f32| on_cylinder(az, edge + up * height / 2.0, width, radius);
        // Going down, the same with heights turned over.
        let flip = |p: Placement| Placement {
            position: Vec3::new(p.position.x, up * p.position.y, p.position.z),
            ..p
        };
        let edge =
            clear_of(&[(flip(at(edge_y)), height)], &[(flip(*e), eh)]).map_or(edge_y, |c| up * c);
        Some(at(if up > 0.0 {
            edge.max(edge_y)
        } else {
            edge.min(edge_y)
        }))
    } else {
        let upv = e.rotation * Vec3::Y;
        Some(Placement {
            position: e.position + upv * up * (eh + height) / 2.0,
            rotation: e.rotation,
            width,
        })
    }
}

/// Over everything seen, at the gaze's azimuth; else under everything; else behind the
/// user at eye level. Nothing is seen there.
fn fallback<K>(
    local: &[(K, Placement, f32)],
    seen: &[Seen<K>],
    gaze: DVec3,
    px: PixelSize,
    limits: &Limits,
) -> Placement {
    let mut radii: Vec<f32> = local
        .iter()
        .map(|m| Vec3::new(m.1.position.x, 0.0, m.1.position.z).length())
        .collect();
    radii.sort_by(f32::total_cmp);
    let width = px.w as f32 * METRES_PER_PIXEL;
    let height = width * px.aspect();
    let radius = radii[radii.len() / 2]
        .max(limits.radius)
        .max(width / 2.0 * 1.001);
    let az = azimuth(gaze) as f32;
    let top = seen.iter().map(|s| s.top).fold(f64::NEG_INFINITY, f64::max);
    let bottom = seen.iter().map(|s| s.bottom).fold(f64::INFINITY, f64::min);
    let limit = 75f64.to_radians();
    // On the cylinder the corners stand at `radius`: their elevation is atan(y / radius).
    let (y, step) = if top < limit - 0.2 {
        (
            (radius as f64 * (top + 0.01).tan()) as f32 + height / 2.0,
            0.02,
        )
    } else if bottom > -limit + 0.2 {
        (
            (radius as f64 * (bottom - 0.01).tan()) as f32 - height / 2.0,
            -0.02,
        )
    } else {
        return on_cylinder(az + std::f32::consts::PI, 0.0, width, radius);
    };
    // The seen rectangles are corner means: up (down) until it covers nothing.
    let clear = |p: &Placement| {
        let mine = p.corners(height);
        local
            .iter()
            .all(|(_, o, oh)| !covers(&o.corners(*oh), &mine, 0.0))
    };
    let mut p = on_cylinder(az, y, width, radius);
    for k in 1..=200 {
        if clear(&p) {
            break;
        }
        p = on_cylinder(az, y + step * k as f32, width, radius);
    }
    p
}
