//! The automatic arrangement (only on request: a preset, or the first start): the
//! widening circle. One row around the user at 1.1 m while it fits in [`ARC`]; else the
//! smallest radius that fits; past [`MAX_RADIUS`], two rows, then three, the main row at
//! eye level and the others above it. A row over the main one never leaves out of view
//! ([`in_view`]) a monitor alone in it or one too wide for the circle it starts from
//! ([`RADIUS`]): then one row fewer, on the larger radius that needs.
//!
//! A flat monitor `w` wide with both vertical edges on a cylinder of radius `R` around
//! the eye takes the angle `a(w, R) = 2 asin(w / 2R)` (needs `R >= w / 2`). A row fits
//! when the sum of its angles is at most [`ARC`]. Every `a` falls as `R` grows, so the
//! smallest radius that fits is unique and found by bisection.

// Rows are ranges of the monitors in order; a single row is a list of one range.
#![allow(clippy::single_range_in_vec_init)]

use std::collections::BTreeMap;
use std::ops::Range;

use glam::{Quat, Vec3};

use crate::space::{METRES_PER_PIXEL, PixelSize, Placement, facing, in_view};

/// Distance from the eye to the monitors' edges by default.
pub const RADIUS: f32 = 1.1;
/// The widest a row may be, around the user: 210°.
pub const ARC: f32 = 7.0 * std::f32::consts::PI / 6.0;
/// Past this radius text gets too small: the monitors go into more rows instead.
pub const MAX_RADIUS: f32 = 1.5;
/// At most this many rows.
pub const MAX_ROWS: usize = 3;
/// The main row sits a little below eye height.
pub const EYE_DROP: f32 = 0.08;
/// How far up the overhead arrangement is tilted, for lying down.
pub const OVERHEAD_PITCH: f32 = 0.95;
/// The laptop screen is shown at most this wide.
pub const MIRROR_MAX_WIDTH: f32 = 1.0;
/// A wall row fits while it is seen at most this far to either side (75°).
pub const WALL_HALF_ANGLE: f32 = 75.0 * std::f32::consts::PI / 180.0;
/// No monitor alone takes more than this (120°): without it a monitor more than twice
/// the radius wide would get the radius of half its width and its flat face would pass
/// through the eye. Below 1.9 m (3840 pixels at the default size) it changes nothing
/// at 1.1 m.
pub const MAX_ONE: f32 = 2.0 * std::f32::consts::PI / 3.0;

/// Which arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Around the user, every monitor facing them.
    Arc,
    /// A flat wall, all monitors in one plane facing the same way.
    Wall,
    /// Around the user and tilted up overhead, for lying down.
    Overhead,
}

/// What an automatic arrangement keeps to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// The radius to start from (and the distance of a wall).
    pub radius: f32,
    /// The widest arc of a row.
    pub arc: f32,
    /// Past this radius, more rows.
    pub max_radius: f32,
    /// At most this many rows.
    pub max_rows: usize,
    /// Space between neighbours in a row, metres (as a chord between their edges).
    pub gap: f32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            radius: RADIUS,
            arc: ARC,
            max_radius: MAX_RADIUS,
            max_rows: MAX_ROWS,
            gap: 0.0,
        }
    }
}

/// The angle a flat monitor `width` wide takes with both edges on a cylinder of
/// `radius` around the eye. Needs `radius >= width / 2`. The same operations as the
/// headset's `angular_width` where that one does not clamp.
pub fn angular_width(width: f32, radius: f32) -> f32 {
    2.0 * (width / 2.0 / radius).asin()
}

/// The angle a row of `widths` takes at `radius`, with `gap` between neighbours.
pub fn row_arc(widths: &[f32], radius: f32, gap: f32) -> f32 {
    let monitors: f32 = widths.iter().map(|&w| angular_width(w, radius)).sum();
    if gap > 0.0 && widths.len() > 1 {
        monitors + (widths.len() - 1) as f32 * angular_width(gap, radius)
    } else {
        monitors
    }
}

/// The smallest radius at which the widest monitor takes no more than [`MAX_ONE`].
pub fn least_radius(widest: f32) -> f32 {
    let mut r = widest / (2.0 * (MAX_ONE / 2.0).sin());
    while widest > 0.0 && angular_width(widest, r) > MAX_ONE {
        r = r.next_up();
    }
    r
}

/// The smallest radius, at least `limits.radius` and [`least_radius`], at which the
/// row fits in `limits.arc` (in `f32`, as it will be laid out).
pub fn fit_radius(widths: &[f32], limits: &Limits) -> f32 {
    let widest = widths.iter().copied().fold(0.0f32, f32::max);
    let start = limits.radius.max(least_radius(widest));
    if widths.is_empty() || row_arc(widths, start, limits.gap) <= limits.arc {
        return start;
    }
    let arc = f64::from(limits.arc);
    let gap = f64::from(limits.gap);
    let over = |r: f64| {
        let monitors: f64 = widths
            .iter()
            .map(|&w| 2.0 * (f64::from(w) / 2.0 / r).min(1.0).asin())
            .sum();
        let gaps = (widths.len().saturating_sub(1)) as f64 * 2.0 * (gap / 2.0 / r).min(1.0).asin();
        monitors + gaps - arc
    };
    // x <= asin x <= πx/2: the answer lies between Σw / arc and π Σw / (2 arc).
    let total: f64 =
        widths.iter().map(|&w| f64::from(w)).sum::<f64>() + gap * (widths.len() - 1) as f64;
    let mut lo = f64::from(start).max(total / arc);
    let mut hi = (std::f64::consts::PI * total / (2.0 * arc)).max(lo);
    while over(hi) > 0.0 {
        hi *= 2.0;
    }
    if over(lo) <= 0.0 {
        hi = lo;
    }
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if mid <= lo || mid >= hi {
            break;
        }
        if over(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut r = (hi as f32).max(start);
    while row_arc(widths, r, limits.gap) > limits.arc {
        r = r.next_up();
    }
    r
}

/// Monitors of `widths` (in this order) into rows: one row while it fits within
/// `limits.max_radius`, else the fewest rows (up to `limits.max_rows`) that do, the
/// order kept and the cuts chosen so the largest radius any row needs is the smallest.
/// Every row then stands on that radius. Ties: the row arcs most even, then the
/// earliest cuts. With `max_rows` rows still too wide, those rows on a larger radius.
pub fn split(widths: &[f32], limits: &Limits) -> (Vec<Range<usize>>, f32) {
    let n = widths.len();
    let one = fit_radius(widths, limits);
    if n <= 1 || one <= limits.max_radius {
        return (vec![0..n], one);
    }
    let mut best = None;
    for k in 2..=limits.max_rows.min(n) {
        best = Some(cut_into(widths, k, limits));
        if best.as_ref().is_some_and(|b| b.1 <= limits.max_radius) {
            break;
        }
    }
    best.expect("at least two monitors")
}

/// Monitors of `widths` (in this order) in `k` rows (`1..=widths.len()`), cut so the
/// largest radius any row needs is the smallest (ties: the row arcs most even, then the
/// earliest cuts), and that radius.
fn cut_into(widths: &[f32], k: usize, limits: &Limits) -> (Vec<Range<usize>>, f32) {
    let n = widths.len();
    let mut best: Option<(f32, f32, Vec<Range<usize>>)> = None;
    for cuts in cut_sets(n, k) {
        let rows: Vec<Range<usize>> = std::iter::once(0)
            .chain(cuts.iter().copied())
            .zip(cuts.iter().copied().chain(std::iter::once(n)))
            .map(|(a, b)| a..b)
            .collect();
        let radius = rows
            .iter()
            .map(|r| fit_radius(&widths[r.clone()], limits))
            .fold(0.0f32, f32::max);
        let widest_arc = rows
            .iter()
            .map(|r| row_arc(&widths[r.clone()], radius, limits.gap))
            .fold(0.0f32, f32::max);
        let better = match &best {
            None => true,
            Some((r, a, _)) => radius < *r || (radius == *r && widest_arc < *a),
        };
        if better {
            best = Some((radius, widest_arc, rows));
        }
    }
    let (radius, _, rows) = best.unwrap_or_else(|| (fit_radius(widths, limits), 0.0, vec![0..n]));
    (rows, radius)
}

/// The rows of `sorted` around the user and their radius, as [`split`] cuts them; but
/// while a row stacked over the main one has a monitor whose middle is out of view and
/// that is alone there or too wide for the circle at `limits.radius` (a flat monitor
/// far wider than the others comes close to the eye, and so high, over them), one row
/// fewer, on the larger radius that needs (one row is always in view).
fn rows_in_view<K: Copy + Ord>(
    sorted: &[Item<K>],
    primary: Option<K>,
    y: f32,
    limits: &Limits,
) -> (Vec<Range<usize>>, f32) {
    let widths: Vec<f32> = sorted.iter().map(|m| m.1).collect();
    let main = primary
        .and_then(|p| sorted.iter().position(|m| m.0 == p))
        .unwrap_or_else(|| sorted.len().saturating_sub(1) / 2);
    let (mut rows, mut radius) = split(&widths, limits);
    while rows.len() > 1 {
        let mut out = BTreeMap::new();
        let lay =
            |items: &[Item<K>], row_y: &dyn Fn(usize) -> f32, out: &mut BTreeMap<K, Placement>| {
                arc_row(items, row_y, radius, limits.gap, out);
            };
        stack_rows(sorted, &rows, primary, y, lay, &mut out);
        let unseen = rows.iter().filter(|r| !r.contains(&main)).any(|r| {
            r.clone().any(|i| {
                let wide = least_radius(widths[i]) > limits.radius;
                (r.len() == 1 || wide) && out.get(&sorted[i].0).is_some_and(|p| !in_view(p))
            })
        });
        if !unseen {
            break;
        }
        (rows, radius) = cut_into(&widths, rows.len() - 1, limits);
    }
    (rows, radius)
}

/// Every way to cut `0..n` into `k` non-empty runs: the `k - 1` cut points, in
/// increasing order.
fn cut_sets(n: usize, k: usize) -> Vec<Vec<usize>> {
    fn go(from: usize, n: usize, left: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if left == 0 {
            out.push(cur.clone());
            return;
        }
        for c in from..=n - left {
            cur.push(c);
            go(c + 1, n, left - 1, cur, out);
            cur.pop();
        }
    }
    let mut out = Vec::new();
    if k >= 1 && k <= n {
        go(1, n, k - 1, &mut Vec::new(), &mut out);
    }
    out
}

/// Whether a wall row `width` wide fits at `distance`: seen at most
/// [`WALL_HALF_ANGLE`] to either side.
fn wall_fits(width: f32, distance: f32) -> bool {
    width <= 2.0 * distance * WALL_HALF_ANGLE.tan()
}

/// The radius [`arrange`] puts `monitors` on for `preset` (a wall's distance), which a
/// curved monitor's cylinder grows with.
/// `primary`: as for [`arrange`].
pub fn radius_of<K: Copy + Ord>(
    preset: Preset,
    monitors: &[(K, PixelSize)],
    primary: Option<K>,
    limits: &Limits,
) -> f32 {
    rows(preset, monitors, primary, limits).1
}

/// The rows [`arrange`] puts `monitors` (in this order) in for `preset`, `primary` in
/// the main one, as ranges of them, and the radius (a wall's distance): as [`split`]
/// cuts them around the user, but with none stacked out of view where fewer rows on a
/// larger radius can help it (see `rows_in_view`).
pub fn rows<K: Copy + Ord>(
    preset: Preset,
    monitors: &[(K, PixelSize)],
    primary: Option<K>,
    limits: &Limits,
) -> (Vec<Range<usize>>, f32) {
    let sorted = items(monitors);
    match preset {
        Preset::Arc | Preset::Overhead => rows_in_view(&sorted, primary, row_level(preset), limits),
        Preset::Wall => {
            let widths: Vec<f32> = sorted.iter().map(|m| m.1).collect();
            split_wall(&widths, limits, wall_fits)
        }
    }
}

/// `monitors` as the arrangement takes them: key, width and height in metres.
fn items<K: Copy>(monitors: &[(K, PixelSize)]) -> Vec<Item<K>> {
    monitors
        .iter()
        .map(|&(id, px)| {
            (
                id,
                px.w as f32 * METRES_PER_PIXEL,
                px.h as f32 * METRES_PER_PIXEL,
            )
        })
        .collect()
}

/// The height of the main row's middle around the user: a little below the eye
/// sitting, at the eye lying down (the whole is tilted up then).
fn row_level(preset: Preset) -> f32 {
    if preset == Preset::Arc {
        -EYE_DROP
    } else {
        0.0
    }
}

/// An upright monitor on the cylinder of `radius` around the eye, its edges on the
/// cylinder, centred at azimuth `yaw` (0 straight ahead, positive right) and height
/// `y`. The headset's `on_cylinder` with the radius as a parameter.
pub fn on_cylinder(yaw: f32, y: f32, width: f32, radius: f32) -> Placement {
    let half = (width / 2.0 / radius).asin();
    let r = radius * half.cos();
    let position = Vec3::new(r * yaw.sin(), y, -r * yaw.cos());
    Placement {
        position,
        rotation: facing(position, Vec3::new(0.0, y, 0.0)),
        width,
    }
}

fn yaw_of(p: &Placement) -> f32 {
    p.position.x.atan2(-p.position.z)
}

/// A monitor to arrange: key, width and height in metres.
type Item<K> = (K, f32, f32);

/// Places every monitor for `preset`, in the frame (eye at the origin, -Z ahead).
/// `monitors` go left to right in the order given (as on the desktop), into one row
/// around the user while it fits (see the module's description), else into rows; the
/// laptop screen `mirror` goes under the primary monitor. `curve`: the monitors are
/// drawn curved with a cylinder `curve` times the arrangement's radius (`None` flat);
/// the laptop screen then continues the cylinder of the monitor above it.
///
/// Every set that fits in one row at [`RADIUS`] goes through the same operations as
/// the headset's `arrange_curved` of 0.2.0 (`curve` there being `curve * RADIUS`), so
/// it comes out bit for bit the same.
pub fn arrange<K: Copy + Ord>(
    preset: Preset,
    monitors: &[(K, PixelSize)],
    primary: Option<K>,
    mirror: Option<(K, PixelSize)>,
    curve: Option<f32>,
    limits: &Limits,
) -> BTreeMap<K, Placement> {
    let sorted = items(monitors);
    let height_of = |id: K| sorted.iter().find(|m| m.0 == id).map_or(0.0, |m| m.2);
    let widths: Vec<f32> = sorted.iter().map(|m| m.1).collect();

    let mut out = BTreeMap::new();
    let radius = match preset {
        Preset::Arc | Preset::Overhead => {
            let y = row_level(preset);
            let (rows, radius) = rows_in_view(&sorted, primary, y, limits);
            let lay = |items: &[Item<K>],
                       row_y: &dyn Fn(usize) -> f32,
                       out: &mut BTreeMap<K, Placement>| {
                arc_row(items, row_y, radius, limits.gap, out);
            };
            stack_rows(&sorted, &rows, primary, y, lay, &mut out);
            radius
        }
        Preset::Wall => {
            let (rows, distance) = split_wall(&widths, limits, wall_fits);
            let lay = |items: &[Item<K>],
                       row_y: &dyn Fn(usize) -> f32,
                       out: &mut BTreeMap<K, Placement>| {
                wall_row(items, row_y, distance, out);
            };
            stack_rows(&sorted, &rows, primary, -EYE_DROP, lay, &mut out);
            distance
        }
    };

    if let Some((id, px)) = mirror {
        let (mut w, mut h) = (
            px.w as f32 * METRES_PER_PIXEL,
            px.h as f32 * METRES_PER_PIXEL,
        );
        if w > MIRROR_MAX_WIDTH {
            h *= MIRROR_MAX_WIDTH / w;
            w = MIRROR_MAX_WIDTH;
        }
        let anchor = primary
            .and_then(|p| out.get(&p).map(|pl: &Placement| (*pl, height_of(p))))
            .or_else(|| {
                let mid = sorted.get(sorted.len().saturating_sub(1) / 2)?;
                Some((*out.get(&mid.0)?, mid.2))
            });
        let yaw = anchor.map_or(0.0, |(p, _)| yaw_of(&p));
        // Below every monitor it could cover: those whose arc overlaps its own (on a wall,
        // whose span across the wall does).
        let reach = angular_width(w, radius) / 2.0;
        let lid_x = anchor.map_or(0.0, |(p, _)| p.position.x);
        let bottom = sorted
            .iter()
            .filter_map(|m| {
                let p = out.get(&m.0)?;
                let overlaps = if preset == Preset::Wall {
                    (p.position.x - lid_x).abs() < (w + m.1) / 2.0
                } else {
                    (yaw_of(p) - yaw).abs() < reach + angular_width(m.1, radius) / 2.0
                };
                overlaps.then_some(p.position.y - m.2 / 2.0)
            })
            .fold(f32::INFINITY, f32::min);
        let bottom = if bottom.is_finite() {
            bottom
        } else {
            -EYE_DROP
        };
        // Curved: the laptop screen continues the cylinder of the monitor above it (on a
        // cylinder row, and when that monitor fits within its curve; a wall, or a
        // monitor wider than its curve, gets the flat hinge).
        let curved = curve.zip(anchor).filter(|(factor, (above, _))| {
            preset != Preset::Wall && above.width / 2.0 < factor * radius * 0.999
        });
        if let Some((factor, (above, _))) = curved {
            let curve_radius = factor * radius;
            let normal = above.rotation * Vec3::Z;
            let half_above = (above.width / 2.0).min(curve_radius * 0.999);
            let axis = above.position
                + normal * (curve_radius * curve_radius - half_above * half_above).sqrt();
            let half = (w / 2.0).min(curve_radius * 0.999);
            let mut position = axis - normal * (curve_radius * curve_radius - half * half).sqrt();
            position.y = bottom - h / 2.0;
            out.insert(
                id,
                Placement {
                    position,
                    rotation: above.rotation,
                    width: w,
                },
            );
            return finish(preset, out);
        }
        // Flat: hinged at its top edge to the bottom edge of the monitor above and
        // tilted back towards the eyes like a laptop lid.
        let hinge_at = |bottom: f32| {
            let top = match anchor {
                Some((p, _)) => Vec3::new(p.position.x, bottom, p.position.z),
                None => on_cylinder(yaw, bottom, w, radius).position,
            };
            // Facing the eye; on a wall turned only up towards it, its hinge along the
            // wall's bottom edge.
            let towards = |p: Vec3| {
                if preset == Preset::Wall {
                    Vec3::new(p.x, 0.0, 0.0)
                } else {
                    Vec3::ZERO
                }
            };
            let mut position = top - Vec3::Y * h / 2.0;
            let mut rotation = facing(position, towards(position));
            for _ in 0..4 {
                position = top + rotation * Vec3::new(0.0, -h / 2.0, 0.0);
                rotation = facing(position, towards(position));
            }
            Placement {
                position,
                rotation,
                width: w,
            }
        };
        // Lower, a centimetre at a time, while it is seen covering a monitor (a wide flat
        // neighbour's bottom edge bulges down towards the eye); as before when it does not.
        let mut lid = hinge_at(bottom);
        for step in 1..=60 {
            let covered = sorted.iter().any(|m| {
                out.get(&m.0)
                    .is_some_and(|p| crate::space::covers(&p.corners(m.2), &lid.corners(h), 0.001))
            });
            if !covered {
                break;
            }
            lid = hinge_at(bottom - 0.01 * step as f32);
        }
        out.insert(id, lid);
    }
    finish(preset, out)
}

/// The rows of `sorted` stacked: the one with the primary monitor (else the middle
/// monitor) at `y`, the others above it in their order from the top down. One row:
/// every monitor centred at `y`, as before rows. More: the main row's tops lined up
/// (the primary centred at `y`), every other row standing on the highest top of the
/// row under it, bottoms lined up, so each row is a shelf on the one below and the
/// mouse goes up wherever they meet. `lay` puts a row's monitors in place with their
/// centres at the heights given.
fn stack_rows<K: Copy + Ord>(
    sorted: &[Item<K>],
    rows: &[Range<usize>],
    primary: Option<K>,
    y: f32,
    lay: impl Fn(&[Item<K>], &dyn Fn(usize) -> f32, &mut BTreeMap<K, Placement>),
    out: &mut BTreeMap<K, Placement>,
) {
    if rows.len() <= 1 {
        lay(sorted, &|_| y, out);
        return;
    }
    let main = primary
        .and_then(|p| sorted.iter().position(|m| m.0 == p))
        .unwrap_or_else(|| sorted.len().saturating_sub(1) / 2);
    let main_row = rows.iter().position(|r| r.contains(&main)).unwrap_or(0);
    // The main row: tops lined up with the primary's (the tallest's without one in it).
    let items = &sorted[rows[main_row].clone()];
    let reference = items
        .iter()
        .find(|m| Some(m.0) == primary)
        .or_else(|| items.iter().max_by(|a, b| a.2.total_cmp(&b.2)))
        .expect("a row has monitors");
    let top = y + reference.2 / 2.0;
    lay(items, &|i| top - items[i].2 / 2.0, out);
    // The others from the bottom up: those after the main row's in reverse, then those
    // before it in reverse, so from the top down they read in their order.
    let mut below: Vec<usize> = vec![main_row];
    let order: Vec<usize> = (0..rows.len()).rev().filter(|&r| r != main_row).collect();
    let order: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&r| r > main_row)
        .chain(order.iter().copied().filter(|&r| r < main_row))
        .collect();
    for r in order {
        let items = &sorted[rows[r].clone()];
        // The shelf: the highest top of the row under it.
        let under = *below.last().expect("the main row");
        let shelf = sorted[rows[under].clone()]
            .iter()
            .filter_map(|m| out.get(&m.0).map(|p| p.position.y + m.2 / 2.0))
            .fold(f32::NEG_INFINITY, f32::max);
        lay(items, &|i| shelf + items[i].2 / 2.0, out);
        // Seen, not only in height: up as far as keeps it clear of the rows under it.
        let with_height = |m: &Item<K>| out.get(&m.0).map(|p| (*p, m.2));
        let upper: Vec<(Placement, f32)> = items.iter().filter_map(with_height).collect();
        let lower: Vec<(Placement, f32)> = below
            .iter()
            .flat_map(|&b| sorted[rows[b].clone()].iter())
            .filter_map(with_height)
            .collect();
        if let Some(clear) = clear_of(&upper, &lower)
            && clear > shelf
        {
            lay(items, &|i| clear + items[i].2 / 2.0, out);
        }
        below.push(r);
    }
}

/// The lowest height for the bottom edges of the upright monitors `upper` at which none
/// of them is seen below the top edge of any of `lower` it stands over (each with its
/// height; frame coordinates, eye at the origin). `None` when none stands over another.
///
/// A flat monitor's middle is nearer the eye than its ends: where a wide one is under a
/// narrow one at the same height, its top edge is seen above the narrow one's bottom
/// edge. Seen from the eye, an upright plane at distance `d` from the vertical axis,
/// facing azimuth `t`, is `d / cos(a - t)` away horizontally at azimuth `a`; an edge at
/// height `y` is seen at elevation `atan(y / that)`. So the upper bottom `y_u` clears
/// the lower top `y_p` at `a` when `y_u >= y_p * d_u(a) / d_p(a)`; the largest over the
/// azimuths both cover is the answer. On a wall every plane is the same and this is the
/// lower top itself.
pub fn clear_of(upper: &[(Placement, f32)], lower: &[(Placement, f32)]) -> Option<f32> {
    // (azimuth the plane faces away to, its distance from the axis, the azimuths it
    // covers as seen).
    let plane = |p: &Placement| {
        let away = -p.normal();
        let facing = away.x.atan2(-away.z);
        let flat = Vec3::new(away.x, 0.0, away.z).normalize_or(Vec3::NEG_Z);
        let distance = p.position.dot(flat).max(1e-4);
        let right = p.rotation * Vec3::X * (p.width / 2.0);
        let (l, r) = (p.position - right, p.position + right);
        let a = (l.x.atan2(-l.z), r.x.atan2(-r.z));
        let c = p.position.x.atan2(-p.position.z);
        let unwrap = |x: f32| {
            c + (x - c + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI
        };
        let (a0, a1) = (unwrap(a.0), unwrap(a.1));
        (facing, distance, a0.min(a1), a0.max(a1), c)
    };
    let mut best: Option<f32> = None;
    for (u, _) in upper {
        let (tu, du, ul, ur, uc) = plane(u);
        for (p, ph) in lower {
            let (tp, dp, pl, pr, pc) = plane(p);
            // The lower one's span moved by whole turns to lie around the upper one.
            let shift = {
                let d = uc - pc;
                (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI
                    - d
            };
            let (pl, pr) = (pl - shift, pr - shift);
            let (lo, hi) = (ul.max(pl), ur.min(pr));
            if hi - lo <= 1e-4 {
                continue;
            }
            let top = p.position.y + ph / 2.0;
            for i in 0..=32 {
                let a = lo + (hi - lo) * i as f32 / 32.0;
                let ratio = (du / (a - tu).cos()) / (dp / (a - tp + shift).cos());
                let need = top * ratio;
                best = Some(best.map_or(need, |b: f32| b.max(need)));
            }
        }
    }
    best
}

/// One row around the user on the cylinder of `radius`, side by side, centred straight
/// ahead, monitor `i` centred at height `y(i)`. The headset's `row` with the radius and
/// heights as parameters.
fn arc_row<K: Copy + Ord>(
    items: &[Item<K>],
    y: &dyn Fn(usize) -> f32,
    radius: f32,
    gap: f32,
    out: &mut BTreeMap<K, Placement>,
) {
    let widths: Vec<f32> = items.iter().map(|m| m.1).collect();
    let total = row_arc(&widths, radius, gap);
    let mut yaw = -total / 2.0;
    for (i, &(id, w, _)) in items.iter().enumerate() {
        if i > 0 && gap > 0.0 {
            yaw += angular_width(gap, radius);
        }
        let a = angular_width(w, radius);
        out.insert(id, on_cylinder(yaw + a / 2.0, y(i), w, radius));
        yaw += a;
    }
}

/// One row of a wall at `distance`, side by side, centred straight ahead.
fn wall_row<K: Copy + Ord>(
    items: &[Item<K>],
    y: &dyn Fn(usize) -> f32,
    distance: f32,
    out: &mut BTreeMap<K, Placement>,
) {
    let total: f32 = items.iter().map(|m| m.1).sum();
    let mut x = -total / 2.0;
    for (i, &(id, w, _)) in items.iter().enumerate() {
        out.insert(
            id,
            Placement {
                position: Vec3::new(x + w / 2.0, y(i), -distance),
                rotation: Quat::IDENTITY,
                width: w,
            },
        );
        x += w;
    }
}

/// Rows of a wall: one while it is seen within [`WALL_HALF_ANGLE`] to either side at
/// `limits.radius`, else the fewest rows (up to `limits.max_rows`) that are, cut so the
/// widest row is the narrowest; still too wide, the wall moves further away.
fn split_wall(
    widths: &[f32],
    limits: &Limits,
    fits: impl Fn(f32, f32) -> bool,
) -> (Vec<Range<usize>>, f32) {
    let n = widths.len();
    let total: f32 = widths.iter().sum();
    if n <= 1 || fits(total, limits.radius) {
        let distance = limits.radius.max(total / (2.0 * WALL_HALF_ANGLE.tan()));
        return (vec![0..n], if n <= 1 { distance } else { limits.radius });
    }
    let mut best: Option<(f32, Vec<Range<usize>>)> = None;
    for k in 2..=limits.max_rows.min(n) {
        best = None;
        for cuts in cut_sets(n, k) {
            let rows: Vec<Range<usize>> = std::iter::once(0)
                .chain(cuts.iter().copied())
                .zip(cuts.iter().copied().chain(std::iter::once(n)))
                .map(|(a, b)| a..b)
                .collect();
            let widest = rows
                .iter()
                .map(|r| widths[r.clone()].iter().sum::<f32>())
                .fold(0.0f32, f32::max);
            if best.as_ref().is_none_or(|(w, _)| widest < *w) {
                best = Some((widest, rows));
            }
        }
        if best.as_ref().is_some_and(|(w, _)| fits(*w, limits.radius)) {
            break;
        }
    }
    let (widest, rows) = best.expect("at least two monitors");
    let distance = limits.radius.max(widest / (2.0 * WALL_HALF_ANGLE.tan()));
    (rows, distance)
}

/// Tilts everything up for the overhead preset.
fn finish<K: Ord>(preset: Preset, mut out: BTreeMap<K, Placement>) -> BTreeMap<K, Placement> {
    if preset == Preset::Overhead {
        let tilt = Quat::from_rotation_x(OVERHEAD_PITCH);
        for p in out.values_mut() {
            p.position = tilt * p.position;
            p.rotation = tilt * p.rotation;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths(px: &[u32]) -> Vec<f32> {
        px.iter().map(|&p| p as f32 * METRES_PER_PIXEL).collect()
    }

    #[test]
    fn three_2560_stay_at_1_1_m_in_one_row() {
        let (rows, r) = split(&widths(&[2560, 2560, 2560]), &Limits::default());
        assert_eq!((rows, r), (vec![0..3], RADIUS));
    }

    #[test]
    fn the_six_of_2026_11_19_make_two_rows_at_about_1_35_m() {
        // 3440, 2560, 1440 (portrait), 3440, 2560, 3440 in the saved order.
        let w = widths(&[3440, 2560, 1440, 3440, 2560, 3440]);
        let limits = Limits::default();
        assert!(
            fit_radius(&w, &limits) > 2.2,
            "one row would need {}",
            fit_radius(&w, &limits)
        );
        let (rows, r) = split(&w, &limits);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!((1.3..1.4).contains(&r), "{r}");
        for row in rows {
            assert!(row_arc(&w[row], r, 0.0) <= ARC);
        }
    }

    #[test]
    fn the_radius_is_the_smallest_that_fits() {
        let w = widths(&[2560, 2560, 2560, 2560]);
        let r = fit_radius(&w, &Limits::default());
        assert!(row_arc(&w, r, 0.0) <= ARC);
        assert!(row_arc(&w, r * 0.999, 0.0) > ARC, "{r}");
    }
}
