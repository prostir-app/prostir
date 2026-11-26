//! "Line up" (the A button): it takes the crookedness out of each row and nothing more.
//!
//! How a whole row stands is the user's choice and stays: tilted back for a reclined
//! pose, lying down overhead, turned or rolled a little as a whole, its monitors turned
//! to face a head that was lower. Each row is one surface fitted to it, and only each
//! monitor's own deviation from that surface goes.
//!
//! - The row's axis, a line through the eye. A monitor stands on its row without
//!   leaning when its top and bottom edges are square to the axis, so the axis is the
//!   direction square to the right edges of the most monitors, found robustly: every
//!   two monitors whose right edges point apart by [`PAIR`] or more propose the one
//!   direction square to both, and the proposal that leaves the least lean of all the
//!   monitors and the least difference of their tilts from the pair's own wins (equal
//!   ones averaged), refined over the monitors it fits. One monitor hanging crooked
//!   among three or more does not move it. A row of one or two monitors takes the axis
//!   fitted to all of them unless one of its monitors leans more than [`OWN_TILT`] off
//!   it; monitors all in one column (right edges parallel) take the median of their
//!   ups.
//! - The row's tilt: the median of how far its monitors face up or down along the axis
//!   (0 when they face it square). Monitors turned to face a lower head keep facing it.
//! - On that surface, in the row's own frame (the axis up): every monitor faces the
//!   axis, tilted by the row's tilt, without leaning, turned about its own centre; the
//!   monitors close up side by side in the order of their row, left to right, the mean
//!   of their directions where it was (the least movement of them all, and no drift
//!   towards the primary); tilted, they close up where their side edges come nearest the
//!   axis (flat monitors on a cone meet only there, and further out stand a little
//!   apart, rather than go into each other); their mid-lines on one circle, at the mean
//!   distance their
//!   side edges were from the axis; and one line up and down: tops, middles or bottoms,
//!   whichever the monitors are nearest to, where that line was on average.
//! - A flat row (monitors parallel and in one plane, a wall) stays in its plane: side
//!   by side in it, the mean of their centres where it was, one line up and down.
//!
//! The rows are those the eye sees, the laptop screen among them, and they stay so. They
//! keep their heights. Only where a row straightened would cover a monitor of the row
//! before it (outwards from the main one), or come so near in height that the two could
//! be taken for one row, is it moved along the axis of the whole far enough not to, and
//! [`CLEAR`] more, so that what decides it is not a matter of the last bits of a
//! number. A gap the user left between rows stays: the desktop closes it, the headset
//! shows it as it was.
//!
//! A row glued to the row before it stays glued (the magnet joins a monitor over or
//! under another edge to edge: see [`GLUED`]). Straightened, its edges are seen
//! otherwise (a monitor turned to face the axis, a flat one over a curved row, a row
//! fitted around an eye elsewhere), and pushed clear it would leave a gap in the middle
//! of the seam. So it moves along the axis of the whole to where the middle of each
//! seam it is glued along closes as the eye sees it, as near as the seam that would open
//! most needs; the ends of a flat monitor over a curved row then overlap a little as
//! seen, the nearer monitor in front, never deeper than [`OVERLAP_MAX`] into a monitor
//! it did not cover as it stood. Never into one row with the row before it. Where it
//! cannot, it is pushed clear as a row not glued is.
//!
//! A row already lined up, to within [`SNUG`] and [`SQUARE`] (far more than rounding
//! leaves, far less than the eye sees), is left exactly as it is. So lining up twice
//! changes nothing the second time, on any machine: what the first time left is lined
//! up whatever the last bits of its sines and square roots.
//!
//! Everything is worked out from the monitors themselves around the eye, so turning the
//! whole arrangement about the eye (any yaw, pitch or roll that leaves its rows the
//! same) and lining up gives what lining up and turning gives; a mirror-symmetric
//! arrangement stays symmetric.

use std::collections::BTreeMap;

use glam::{Mat3, Quat, Vec3};

use crate::circle::least_radius;
use crate::desk::TOUCH_GAP;
use crate::rows::rows_of;
use crate::space::Side;
use crate::space::{Frame, Monitor, PixelSize, Placement, covers, cuts, facing, unwrap};
use crate::view::{Room, reach_pair, relate_pair};

/// A row of one or two monitors keeps an axis of its own only past this angle from the
/// axis fitted to all monitors (10°); nearer, it is a monitor standing crooked.
pub const OWN_TILT: f32 = 10.0 * std::f32::consts::PI / 180.0;
/// Two monitors propose an axis when their right edges point apart by this much (10°):
/// nearer to parallel, the direction square to both is not well defined.
pub const PAIR: f32 = 10.0 * std::f32::consts::PI / 180.0;
/// Monitors whose faces point within this angle of each other (and that stand in one
/// plane) make a flat row, a wall (3°), however crooked one of them hangs.
const FLAT: f32 = 3.0 * std::f32::consts::PI / 180.0;
/// A monitor fits an axis when it leans off it by no more than this (1°).
const FITS: f32 = std::f32::consts::PI / 180.0;
/// An axis is within this angle of the up of every monitor that proposes it (60°).
const UPRIGHT: f32 = 60.0 * std::f32::consts::PI / 180.0;
/// A row is lined up already when its monitors lean, tilt apart and turn away from
/// facing the axis by no more than this (0.05°)...
pub const SQUARE: f32 = 0.05 * std::f32::consts::PI / 180.0;
/// ... and their seams, the circle of their mid-lines and their line up and down are
/// true to this (0.5 mm).
pub const SNUG: f32 = 0.0005;
/// A row moved apart from the one before it goes this much further than it needs (1 mm).
pub const CLEAR: f32 = 0.001;
/// A monitor over or under another is glued to it when the edges they face each other
/// with lie along one another in the room to within this much of the shorter one's
/// height (0.1 %, 0.7 mm on a 2560x1440 monitor): as the magnet leaves them.
pub const GLUED: f32 = 0.001;
/// ... but never further apart than this (a quarter of a millimetre), however big the
/// monitors.
pub const GLUED_MOST: f32 = CLEAR / 4.0;
/// ... or, its row lined up already, they meet as the eye sees them in the middle of the
/// edge they share to within this (50 micrometres: the gap as a share of the inner
/// one's height, times that height). Lining up closes a glued seam to a micrometre, and
/// pushes a row clear [`CLEAR`] further than it covers by: rarely nearer than this in the
/// middle of a seam.
pub const CLOSED: f32 = CLEAR / 20.0;
/// ... along at least this much of the shorter of the two edges (a tenth): a corner
/// meeting a corner is no seam.
pub const SHARES: f32 = 0.1;
/// A glued row overlaps a monitor of the row it is glued to, as seen, by at most this
/// much of the height of the one it reaches into (12 %; the owner's 3840x2160 over two
/// 2560x1440 on an arc needs about 11 at the far end): further, it is pushed clear.
pub const OVERLAP_MAX: f32 = 0.12;
/// Points along a seam where its overlap is measured.
const OVERLAP_SAMPLES: usize = 17;
/// Halving to find where a row meets or clears another stops here (a micrometre).
const HALVED: f32 = 1e-6;

/// Every monitor lined up (see the module's description), in the room. `main`: the
/// primary monitor, whose row keeps its place while the others make room.
/// `previous_rows`: the rows last worked out (see [`rows_of`]). `lid`: the laptop
/// screen, which hangs tilted under its monitor and is not lined up; it stays where it
/// is, or goes down just enough not to be covered by a monitor lined up over it.
pub fn line_up<K: Copy + Ord>(
    all: &[Monitor<K>],
    frame: &Frame,
    main: Option<K>,
    previous_rows: Option<&[Vec<K>]>,
    lid: Option<K>,
) -> BTreeMap<K, Placement> {
    line_up_keeping(all, frame, main, previous_rows, lid, &[], None)
}

/// [`line_up`], each of `glued` (pairs as [`glued_pairs`] gives them: the one of the
/// row nearer the main one first) kept glued as well, as if its seam were closed: for
/// monitors carried away from where they were glued (further from the eye, so their
/// seams open in the room though they met as seen), whose seams are to close again.
/// The pairs it keeps glued, as [`glued_pairs`] gives them, are added to `kept`.
pub fn line_up_keeping<K: Copy + Ord>(
    all: &[Monitor<K>],
    frame: &Frame,
    main: Option<K>,
    previous_rows: Option<&[Vec<K>]>,
    lid: Option<K>,
    glued: &[(K, K)],
    kept: Option<&mut Vec<(K, K)>>,
) -> BTreeMap<K, Placement> {
    // The rows as the eye sees them, the laptop screen among them: what is kept.
    let ids: Vec<K> = all.iter().map(|m| m.id).collect();
    let seen_rows = rows_of(&ids, &Room::new(frame, all), previous_rows);
    let monitors: Vec<Monitor<K>> = all.iter().filter(|m| Some(m.id) != lid).copied().collect();
    let screen = all.iter().find(|m| Some(m.id) == lid).copied();
    let mut lined = line_up_rows(&monitors, frame, main, &seen_rows, screen, glued, kept);
    if let Some(l) = screen {
        let mut p = frame.unplace(l.placement);
        // Only a monitor lined up over it pushes it down: one that covered it as it
        // stood (a curved monitor's flat chord bulging over it) leaves it be.
        let covered = |m: &Placement, h: f32, p: &Placement| {
            covers(&m.corners(h), &p.corners(l.height()), 0.001)
        };
        let others: Vec<[Vec3; 4]> = monitors
            .iter()
            .filter(|m| !covered(&frame.unplace(m.placement), m.height(), &p))
            .map(|m| frame.unplace(lined[&m.id]).corners(m.height()))
            .collect();
        let down = p.rotation * Vec3::Y;
        // Nor does it join a row it was not in.
        let in_row = |p: &Placement| -> Vec<K> {
            let mut now: Vec<Monitor<K>> = all
                .iter()
                .map(|m| Monitor {
                    placement: lined.get(&m.id).copied().unwrap_or(m.placement),
                    ..*m
                })
                .collect();
            if let Some(m) = now.iter_mut().find(|m| m.id == l.id) {
                m.placement = frame.place(*p);
            }
            let rows = rows_of(&ids, &Room::new(frame, &now), Some(&seen_rows));
            let mut row = rows
                .into_iter()
                .find(|r| r.contains(&l.id))
                .unwrap_or_default();
            row.sort();
            row
        };
        let mut was = seen_rows
            .iter()
            .find(|r| r.contains(&l.id))
            .cloned()
            .unwrap_or_default();
        was.sort();
        // A centimetre at a time: until nothing lined up covers it, then (30 cm at most)
        // until it is in no row it was not in.
        let mut joining = 0;
        for _ in 0..100 {
            let mine = p.corners(l.height());
            if others.iter().any(|o| covers(o, &mine, 0.001)) {
                p.position -= down * 0.01;
                continue;
            }
            if joining == 30 || !in_row(&p).iter().any(|k| !was.contains(k)) {
                break;
            }
            joining += 1;
            p.position -= down * 0.01;
        }
        lined.insert(l.id, frame.place(p));
    }
    lined
}

/// A monitor's edges as the fit sees them: where its centre is (from the eye), which
/// way its right edge, its up and its face point, and its size in metres.
#[derive(Debug, Clone, Copy)]
struct Pose {
    centre: Vec3,
    right: Vec3,
    up: Vec3,
    face: Vec3,
    width: f32,
    height: f32,
}

impl Pose {
    fn of(p: &Placement, px: PixelSize) -> Self {
        Self {
            centre: p.position,
            right: p.rotation * Vec3::X,
            up: p.rotation * Vec3::Y,
            face: p.rotation * Vec3::Z,
            width: p.width,
            height: p.width * px.aspect(),
        }
    }

    /// How far it leans off `axis` (its right edge out of square), radians.
    fn lean(&self, axis: Vec3) -> f32 {
        self.right.dot(axis).clamp(-1.0, 1.0).asin()
    }

    /// How far its face looks up along `axis`, radians.
    fn tilt(&self, axis: Vec3) -> f32 {
        self.face.dot(axis).clamp(-1.0, 1.0).asin()
    }
}

/// The median of `values` (the mean of the middle two for an even count).
fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// The axis fitted to `poses` (see the module's description), pointing to the side of
/// `up`; `None` when no two right edges point apart by [`PAIR`] around an up.
fn fit_axis(poses: &[Pose], up: Vec3) -> Option<Vec3> {
    let orient = |a: Vec3| if a.dot(up) < 0.0 { -a } else { a };
    // Right edges apart by a turn around the axis, not by a roll in one plane: two
    // monitors of a wall, one hanging crooked, would propose the wall's normal. The axis
    // is within [`UPRIGHT`] of both monitors' ups (tilted back 35°, they are within 35°).
    let proposal = |i: usize, j: usize| -> Option<Vec3> {
        let c = poses[i].right.cross(poses[j].right);
        let a = orient(c.normalize_or_zero());
        let upright = |p: &Pose| a.dot(p.up).abs() >= UPRIGHT.cos();
        (c.length() >= PAIR.sin() && upright(&poses[i]) && upright(&poses[j])).then_some(a)
    };
    // What a proposal of the pair `i`, `j` leaves: the lean of every monitor, and how far
    // each tilts from the pair's own tilt (a monitor that leans on its own then disagrees
    // with the others in both, so it never outvotes them).
    let cost = |a: Vec3, i: usize, j: usize| -> f32 {
        let tilt = (poses[i].tilt(a) + poses[j].tilt(a)) / 2.0;
        poses
            .iter()
            .map(|p| p.lean(a).abs() + (p.tilt(a) - tilt).abs())
            .sum()
    };
    let mut proposals: Vec<(f32, Vec3)> = Vec::new();
    for i in 0..poses.len() {
        for j in i + 1..poses.len() {
            if let Some(a) = proposal(i, j) {
                proposals.push((cost(a, i, j), a));
            }
        }
    }
    let least = proposals.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    if !least.is_finite() {
        return None;
    }
    // Equal proposals (a symmetric arrangement proposes mirror images) are averaged.
    let tied: Vec3 = proposals
        .iter()
        .filter(|p| p.0 <= least + 1e-5 * poses.len() as f32)
        .map(|p| p.1)
        .sum();
    let best = orient(tied.normalize_or(proposals[0].1));
    // Refined over the monitors it fits: every two of them, weighted by how well their
    // right edges define the direction square to both.
    let fit: Vec<&Pose> = poses
        .iter()
        .filter(|p| p.lean(best).abs() <= FITS)
        .collect();
    let mut refined = Vec3::ZERO;
    for i in 0..fit.len() {
        for j in i + 1..fit.len() {
            let c = fit[i].right.cross(fit[j].right);
            refined += if c.dot(best) < 0.0 { -c } else { c };
        }
    }
    Some(if refined.length() >= PAIR.sin() {
        orient(refined.normalize())
    } else {
        best
    })
}

/// The geometric median of unit vectors (the point least far from them all, Weiszfeld's
/// iteration from their mean), made unit: a monitor tilted on its own among three or
/// more does not move it.
fn median_direction(v: &[Vec3]) -> Vec3 {
    let mut m: Vec3 = v.iter().copied().sum::<Vec3>() / v.len().max(1) as f32;
    for _ in 0..50 {
        let (mut sum, mut weights) = (Vec3::ZERO, 0.0f32);
        for &x in v {
            let w = 1.0 / x.distance(m).max(1e-6);
            sum += x * w;
            weights += w;
        }
        let next = sum / weights.max(1e-12);
        if next.distance(m) < 1e-7 {
            m = next;
            break;
        }
        m = next;
    }
    m.normalize_or(Vec3::Y)
}

/// A row's own frame: `up` along +Y, its right as near the frame's right as can be.
/// Only directions relative to the row are used in it, so how its right is chosen does
/// not matter.
fn row_frame(up: Vec3) -> Quat {
    let y = up.normalize_or(Vec3::Y);
    let x = (Vec3::X - y * y.dot(Vec3::X))
        .try_normalize()
        .unwrap_or_else(|| (Vec3::NEG_Z - y * y.dot(Vec3::NEG_Z)).normalize());
    Quat::from_mat3(&Mat3::from_cols(x, y, x.cross(y))).normalize()
}

/// Whether the monitors of a row make a flat row: faces and right edges parallel, and
/// in one plane.
fn is_flat(poses: &[Pose]) -> bool {
    let Some(first) = poses.first() else {
        return false;
    };
    let depth = |p: &Pose| -p.centre.dot(first.face);
    poses.len() > 1
        && poses.iter().all(|p| {
            p.face.angle_between(first.face) < FLAT && (depth(p) - depth(first)).abs() < 0.02
        })
}

/// [`line_up`] of the monitors without the laptop screen, the pairs of `keep` glued
/// too (see [`line_up_keeping`]); the pairs it keeps glued added to `glued_out`.
fn line_up_rows<K: Copy + Ord>(
    monitors: &[Monitor<K>],
    frame: &Frame,
    main: Option<K>,
    seen_rows: &[Vec<K>],
    screen: Option<Monitor<K>>,
    keep: &[(K, K)],
    mut glued_out: Option<&mut Vec<(K, K)>>,
) -> BTreeMap<K, Placement> {
    let ids: Vec<K> = monitors.iter().map(|m| m.id).collect();
    // The rows as seen, without the laptop screen (which is not lined up).
    let rows: Vec<Vec<K>> = seen_rows
        .iter()
        .map(|r| {
            r.iter()
                .copied()
                .filter(|k| ids.contains(k))
                .collect::<Vec<K>>()
        })
        .filter(|r| !r.is_empty())
        .collect();
    // In the frame: the eye at the origin.
    let local: BTreeMap<K, (Placement, PixelSize)> = monitors
        .iter()
        .map(|m| (m.id, (frame.unplace(m.placement), m.px)))
        .collect();
    let screen = screen.map(|m| (m.id, (frame.unplace(m.placement), m.px)));
    let pose = |k: K| Pose::of(&local[&k].0, local[&k].1);
    let everyone: Vec<Pose> = ids.iter().map(|&k| pose(k)).collect();
    let all_up = median_direction(&everyone.iter().map(|p| p.up).collect::<Vec<_>>());
    // The axis of the whole: where rows of three or more fit their own, the median of
    // those (they stand square to them once lined up, so it is the same the second
    // time); else fitted to all monitors. Monitors all in one column (right edges
    // parallel) say nothing about it by their edges: then their median up, which three
    // or more keep from a crooked one.
    let own_axes: Vec<Vec3> = rows
        .iter()
        .filter(|r| r.len() >= 3)
        .filter_map(|r| {
            let poses: Vec<Pose> = r.iter().map(|&k| pose(k)).collect();
            if is_flat(&poses) {
                return None;
            }
            let up = median_direction(&poses.iter().map(|p| p.up).collect::<Vec<_>>());
            fit_axis(&poses, up)
        })
        .collect();
    let all_axis = if own_axes.is_empty() {
        fit_axis(&everyone, all_up).or_else(|| (everyone.len() >= 3).then_some(all_up))
    } else {
        Some(median_direction(&own_axes))
    };

    let mut out: BTreeMap<K, Placement> = BTreeMap::new();
    let mut axes: Vec<Vec3> = Vec::new();
    // Rows lined up already as they stood (left exactly as they are).
    let mut kept: Vec<bool> = Vec::new();
    for row in &rows {
        let poses: Vec<Pose> = row.iter().map(|&k| pose(k)).collect();
        let flat = is_flat(&poses);
        let row_up = median_direction(&poses.iter().map(|p| p.up).collect::<Vec<_>>());
        let own = if flat { None } else { fit_axis(&poses, row_up) };
        // Three or more fit their own axis; fewer take the axis of the whole unless one
        // of them leans more than OWN_TILT off it (a pair's own axis turns with either
        // monitor's lean, magnified, so it is no measure of a deliberate tilt).
        let whole = all_axis.filter(|b| poses.iter().all(|p| p.lean(*b).abs() < OWN_TILT));
        let mut axis = match (own, whole) {
            (Some(a), _) if poses.len() >= 3 => a,
            (_, Some(b)) => b,
            (Some(a), None) => a,
            (None, None) => all_axis.filter(|_| flat).unwrap_or(row_up),
        };
        if flat {
            // In the plane of the wall.
            let face = median_direction(&poses.iter().map(|p| p.face).collect::<Vec<_>>());
            axis = (axis - face * axis.dot(face)).normalize_or(row_up);
        }
        axes.push(axis);
        let turn = row_frame(axis);
        let in_row: Vec<(K, Placement, PixelSize)> = row
            .iter()
            .map(|&k| {
                let (p, px) = local[&k];
                let p = Placement {
                    position: turn.inverse() * p.position,
                    rotation: (turn.inverse() * p.rotation).normalize(),
                    width: p.width,
                };
                (k, p, px)
            })
            .collect();
        kept.push(lined_up(&in_row, flat));
        if kept[kept.len() - 1] {
            for &k in row {
                out.insert(k, local[&k].0);
            }
            continue;
        }
        let straight = if flat {
            straighten_flat(&in_row)
        } else {
            straighten(&in_row)
        };
        for (k, p) in straight {
            let p = Placement {
                position: turn * p.position,
                rotation: (turn * p.rotation).normalize(),
                width: p.width,
            };
            out.insert(k, p);
        }
    }

    // The rows outwards from the main one, moved along the axis of the whole (else the
    // main row's) and seen in its frame, so they keep apart however the whole stands.
    let main_row = main
        .and_then(|m| rows.iter().position(|r| r.contains(&m)))
        .unwrap_or(0);
    let stack = all_axis.unwrap_or_else(|| axes.get(main_row).copied().unwrap_or(Vec3::Y));
    let turn = row_frame(stack);
    let height = |k: K, p: &Placement| p.width * local[&k].1.aspect();
    let mut inner: Vec<K> = rows.get(main_row).cloned().unwrap_or_default();
    // The glued pairs of the row being placed, the same buffer for every row.
    let mut pairs: Vec<(K, K)> = Vec::new();
    let order: Vec<(usize, f32)> = (0..main_row)
        .rev()
        .map(|r| (r, 1.0))
        .chain((main_row + 1..rows.len()).map(|r| (r, -1.0)))
        .collect();
    for (r, sign) in order {
        let dir = stack * sign;
        let tallest = rows[r]
            .iter()
            .map(|&k| height(k, &out[&k]))
            .fold(0.0f32, f32::max);
        let ok = |shift: f32| {
            apart(
                &rows[r],
                &inner,
                &out,
                &local,
                screen,
                turn,
                dir * shift,
                seen_rows,
                true,
            )
        };
        // The least move, outwards or back (a row lined up over the next one from the
        // other side goes back), up to four times its height: a row that cannot be kept
        // apart nearer stays where lining it up put it, rather than far from everything.
        let least = |way: f32| -> Option<f32> {
            let step = tallest / 8.0;
            let mut hi = step;
            while !ok(way * hi) && hi < 4.0 * tallest {
                hi += step;
            }
            if !ok(way * hi) {
                return None;
            }
            let mut lo = hi - step;
            while hi - lo > HALVED {
                let mid = (lo + hi) / 2.0;
                if ok(way * mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            Some(hi + CLEAR)
        };
        let pushed: f32 = if ok(0.0) {
            0.0
        } else {
            match (least(1.0), least(-1.0)) {
                (Some(out), Some(back)) if back < out => -back,
                (Some(out), _) => out,
                (None, Some(back)) => -back,
                (None, None) => 0.0,
            }
        };
        // A row glued to the rows before it stays glued (see the module's description).
        let over = sign > 0.0;
        let stood = |k: K| local[&k].0;
        let lined = |k: K| out[&k];
        glued(&rows[r], &inner, &local, (turn, over, kept[r]), &mut pairs);
        for &(p, u) in keep {
            if inner.contains(&p) && rows[r].contains(&u) && !pairs.contains(&(p, u)) {
                pairs.push((p, u));
            }
        }
        let closing = |p: K, u: K| {
            close(
                &|s: f32| seam_gap(p, u, &lined, &local, turn, dir * s, over).map(|g| g.0),
                tallest,
            )
        };
        // Each glued seam closes; so does each other seam of a glued monitor with the
        // row it is glued to, along at least SHARES of the shorter edge, that needs the
        // row at most TOUCH_GAP of its height nearer (the far end of a flat monitor glued
        // over a curved row): as near as the seam that would open most needs.
        let mut target: Option<f32> = None;
        for &(p, u) in &pairs {
            if let Some(s) = closing(p, u) {
                target = Some(target.map_or(s, |t: f32| t.min(s)));
            }
        }
        if let Some(at) = target {
            let mut nearest = at;
            for (i, &(_, u)) in pairs.iter().enumerate() {
                if pairs[..i].iter().any(|q| q.1 == u) {
                    continue;
                }
                let under = rows
                    .iter()
                    .filter(|row| pairs.iter().any(|(p, _)| row.contains(p)))
                    .flatten();
                for &p in under {
                    let seam = !pairs.contains(&(p, u))
                        && seam_gap(p, u, &lined, &local, turn, Vec3::ZERO, over)
                            .is_some_and(|(_, share)| share >= SHARES);
                    let near = |s: &f32| *s >= at - TOUCH_GAP as f32 * tallest;
                    if let Some(s) = closing(p, u).filter(|_| seam).filter(near) {
                        nearest = nearest.min(s);
                    }
                }
            }
            target = Some(nearest);
        }
        // Never into one row with the rows before it, and never deeper into a monitor
        // of them than OVERLAP_MAX where it did not cover it as it stood.
        let target = target.filter(|&s| {
            let covered = |p: K, u: K| {
                let (a, b) = (
                    seen_in(p, &stood, &local, turn, Vec3::ZERO),
                    seen_in(u, &stood, &local, turn, Vec3::ZERO),
                );
                covers(
                    &a.placement.corners(a.height()),
                    &b.placement.corners(b.height()),
                    0.001,
                )
            };
            // Seen within the bound, a monitor of the row may still go through one of
            // the row before it in the room (one nearer the eye than the other).
            let through = |p: K, u: K| {
                let (a, b) = (
                    seen_in(p, &lined, &local, turn, Vec3::ZERO),
                    seen_in(u, &lined, &local, turn, dir * s),
                );
                cuts(&a.placement, a.height(), &b.placement, b.height())
            };
            let fits = || {
                inner.iter().all(|&p| {
                    rows[r].iter().all(|&u| {
                        (overlap(p, u, &lined, &local, turn, dir * s, over) <= OVERLAP_MAX
                            || covered(p, u))
                            && !through(p, u)
                    })
                })
            };
            apart(
                &rows[r],
                &inner,
                &out,
                &local,
                screen,
                turn,
                dir * s,
                seen_rows,
                false,
            ) && fits()
        });
        let shift = match target {
            Some(s) => {
                if let Some(kept_glued) = glued_out.as_deref_mut() {
                    kept_glued.extend_from_slice(&pairs);
                }
                // Closed already (but for the last bits of a number), it stays.
                if s.abs() > 1e-5 { s } else { 0.0 }
            }
            None => pushed,
        };
        if shift != 0.0 {
            for &k in &rows[r] {
                if let Some(p) = out.get_mut(&k) {
                    p.position += dir * shift;
                }
            }
        }
        inner.extend(rows[r].iter().copied());
    }
    out.into_iter().map(|(k, p)| (k, frame.place(p))).collect()
}

/// The pairs of monitors [`line_up`] keeps glued, the one of the row nearer the main
/// one first: what it closes the seam of rather than pushing apart (see the module's
/// description). For tests and reports: whether a nudge changed this decision.
#[doc(hidden)]
pub fn glued_pairs<K: Copy + Ord>(
    all: &[Monitor<K>],
    frame: &Frame,
    main: Option<K>,
    previous_rows: Option<&[Vec<K>]>,
    lid: Option<K>,
) -> Vec<(K, K)> {
    let ids: Vec<K> = all.iter().map(|m| m.id).collect();
    let seen_rows = rows_of(&ids, &Room::new(frame, all), previous_rows);
    let monitors: Vec<Monitor<K>> = all.iter().filter(|m| Some(m.id) != lid).copied().collect();
    let screen = all.iter().find(|m| Some(m.id) == lid).copied();
    let mut pairs = Vec::new();
    line_up_rows(
        &monitors,
        frame,
        main,
        &seen_rows,
        screen,
        &[],
        Some(&mut pairs),
    );
    pairs.sort();
    pairs
}

/// How a row's monitors line up when its steps go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lining {
    Top,
    Middle,
    Bottom,
}

/// The line a row's monitors are nearest to: tops, middles or bottoms, summed over
/// every two monitors of different heights (`spans` are `(top, bottom)`); middles when
/// all are of a height, and on a tie (to a tenth of a millimetre, so rounding never
/// decides it): then tops before bottoms.
fn lining(spans: &[(f32, f32)]) -> Lining {
    const TIE: f32 = 1e-4;
    let mut cost = [0.0f32; 3];
    for (i, &(ta, ba)) in spans.iter().enumerate() {
        for &(tb, bb) in &spans[i + 1..] {
            let taller = (ta - ba).max(tb - bb);
            if ((ta - ba) - (tb - bb)).abs() <= 0.02 * taller {
                continue;
            }
            cost[0] += (ta - tb).abs();
            cost[1] += ((ta + ba) - (tb + bb)).abs() / 2.0;
            cost[2] += (ba - bb).abs();
        }
    }
    if cost[1] <= cost[0] + TIE && cost[1] <= cost[2] + TIE {
        Lining::Middle
    } else if cost[0] <= cost[2] + TIE {
        Lining::Top
    } else {
        Lining::Bottom
    }
}

/// The height of every monitor's centre when the row's steps go: `spans` (top, bottom)
/// as they stand, the monitors' heights `tall` as they will stand.
fn one_line(spans: &[(f32, f32)], tall: &[f32]) -> Vec<f32> {
    let n = spans.len().max(1) as f32;
    match lining(spans) {
        Lining::Middle => {
            let line = spans.iter().map(|s| (s.0 + s.1) / 2.0).sum::<f32>() / n;
            tall.iter().map(|_| line).collect()
        }
        Lining::Top => {
            let line = spans.iter().map(|s| s.0).sum::<f32>() / n;
            tall.iter().map(|h| line - h / 2.0).collect()
        }
        Lining::Bottom => {
            let line = spans.iter().map(|s| s.1).sum::<f32>() / n;
            tall.iter().map(|h| line + h / 2.0).collect()
        }
    }
}

/// Whether a row, in its own frame (its axis along +Y through the eye at the origin;
/// `flat`: a wall), stands as lining it up would put it, to within [`SQUARE`] and
/// [`SNUG`]: no monitor leaning, all tilted alike, each facing the axis (or, turned
/// away from it, its own way, as lining up leaves such a one), side by side in the
/// row's order, their mid-lines on one circle (a wall: in one plane), on one line up
/// and down.
fn lined_up<K>(row: &[(K, Placement, PixelSize)], flat: bool) -> bool {
    let poses: Vec<Pose> = row.iter().map(|(_, p, px)| Pose::of(p, *px)).collect();
    let Some(first) = poses.first() else {
        return true;
    };
    let square = SQUARE.sin();
    let tilt = first.tilt(Vec3::Y);
    let upright = poses
        .iter()
        .all(|p| p.right.y.abs() <= square && (p.tilt(Vec3::Y) - tilt).abs() <= SQUARE);
    if !upright {
        return false;
    }
    let level = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
    // Mid-points of the side edges, on the mid-line; neighbours meet across (up and down
    // they stand on the row's line, below).
    let sides = |p: &Pose| {
        (
            p.centre - p.right * p.width / 2.0,
            p.centre + p.right * p.width / 2.0,
        )
    };
    // Tilted, they stand as far apart on the mid-line as `straighten` leaves them for
    // their edges nearer the axis to meet.
    let apart = |a: &Pose, b: &Pose| {
        if flat {
            return 0.0;
        }
        let radius = level(sides(first).0).length();
        let tallest = poses.iter().map(|p| p.height).fold(0.0f32, f32::max);
        let more =
            |w: f32| cone_half(radius, w, tallest, tilt) - (w / 2.0 / radius).min(1.0).asin();
        2.0 * radius * ((more(a.width) + more(b.width)) / 2.0).sin()
    };
    let together = poses.windows(2).all(|w| {
        level(sides(&w[0]).1).distance(level(sides(&w[1]).0)) <= apart(&w[0], &w[1]) + SNUG
    });
    let around = if flat {
        let depth = |p: &Pose| -p.centre.dot(first.face);
        poses.iter().all(|p| {
            p.face.angle_between(first.face) <= SQUARE && (depth(p) - depth(first)).abs() <= SNUG
        })
    } else {
        let radius = |p: &Pose| level(sides(p).0).length();
        let facing_axis = |p: &Pose| {
            let (c, f) = (
                level(p.centre).normalize_or_zero(),
                level(p.face).normalize_or_zero(),
            );
            c.cross(f).length() <= square
        };
        // On a circle wide enough for the widest (see `straighten`).
        let widest = poses.iter().map(|p| p.width).fold(0.0f32, f32::max);
        radius(first) >= least_radius(widest) - SNUG
            && poses.iter().all(|p| {
                facing_axis(p)
                    && (radius(p) - radius(first)).abs() <= SNUG
                    && (level(sides(p).1).length() - radius(first)).abs() <= SNUG
            })
    };
    if !together || !around {
        return false;
    }
    let tall: Vec<f32> = poses
        .iter()
        .map(|p| p.height * p.tilt(Vec3::Y).cos())
        .collect();
    let spans: Vec<(f32, f32)> = poses
        .iter()
        .zip(&tall)
        .map(|(p, h)| (p.centre.y + h / 2.0, p.centre.y - h / 2.0))
        .collect();
    one_line(&spans, &tall)
        .iter()
        .zip(&poses)
        .all(|(y, p)| (y - p.centre.y).abs() <= SNUG)
}

/// Half the turn round the axis a monitor `w` wide takes in a row whose side edges'
/// middles stand on a circle of `radius`, tilted by `tilt` on its cone, `tallest` the
/// tallest of the row: as seen at the end of its side edges nearer the axis (on a
/// smaller circle there, so it takes more of a turn), so that neighbours meet at those
/// ends and go into each other nowhere.
fn cone_half(radius: f32, w: f32, tallest: f32, tilt: f32) -> f32 {
    let half = (w / 2.0 / radius).min(1.0).asin();
    let middle = radius * half.cos();
    let near = (middle - tallest / 2.0 * tilt.sin().abs()).max(middle * 0.5);
    (w / 2.0).atan2(near)
}

/// One row straightened in its own frame (its axis along +Y through the eye at the
/// origin): see the module's description.
fn straighten<K: Copy + Ord>(row: &[(K, Placement, PixelSize)]) -> Vec<(K, Placement)> {
    let poses: Vec<Pose> = row.iter().map(|(_, p, px)| Pose::of(p, *px)).collect();
    let tilt = median(poses.iter().map(|p| p.tilt(Vec3::Y)).collect());
    // The circle: where the side edges would be with the centres where they are.
    let flat_distance = |v: Vec3| Vec3::new(v.x, 0.0, v.z).length();
    let radius = {
        let r = poses
            .iter()
            .map(|p| (flat_distance(p.centre).powi(2) + p.width.powi(2) / 4.0).sqrt())
            .sum::<f32>()
            / poses.len() as f32;
        // A monitor far wider than the others' circle would come right up to the eye
        // on it: the row goes out to where it takes no more than `MAX_ONE`.
        let widest = poses.iter().map(|p| p.width).fold(0.0f32, f32::max);
        r.max(widest / 2.0 * 1.0001).max(least_radius(widest))
    };
    // Tilted, the row stands on a cone: at the end of the monitors nearer the axis
    // their edges are on a smaller circle, so they go round it as wide apart as there
    // (else their corners there reach into each other).
    let tallest = poses.iter().map(|p| p.height).fold(0.0f32, f32::max);
    let half = |w: f32| f64::from((w / 2.0 / radius).min(1.0).asin());
    let half_in = |w: f32| f64::from(cone_half(radius, w, tallest, tilt));
    // Directions around the axis, each near the one before it in the row, so the row
    // keeps its own order and ends (as its rows say, left to right).
    let mut az: Vec<f64> = Vec::with_capacity(row.len());
    for p in &poses {
        let a = f64::from(p.centre.x).atan2(f64::from(-p.centre.z));
        az.push(az.last().map_or(a, |&before| unwrap(a, before)));
    }
    // Side by side from `start` in that order, the mean direction kept.
    let mut offsets = vec![0.0f64; row.len()];
    let mut x = 0.0f64;
    for (i, p) in poses.iter().enumerate() {
        offsets[i] = x + half_in(p.width);
        x += 2.0 * half_in(p.width);
    }
    let n = row.len() as f64;
    let start = az.iter().sum::<f64>() / n - offsets.iter().sum::<f64>() / n;
    // Up and down: one line, the heights as they will stand.
    let tall: Vec<f32> = poses.iter().map(|p| p.height * tilt.cos()).collect();
    let spans: Vec<(f32, f32)> = poses
        .iter()
        .zip(&tall)
        .map(|(p, h)| (p.centre.y + h / 2.0, p.centre.y - h / 2.0))
        .collect();
    let ys = one_line(&spans, &tall);
    let lean_back = Quat::from_rotation_x(-tilt);
    (0..row.len())
        .map(|i| {
            let yaw = (start + offsets[i]) as f32;
            let r = radius * half(poses[i].width).cos() as f32;
            let position = Vec3::new(r * yaw.sin(), ys[i], -r * yaw.cos());
            // Facing the axis; one that faced away from it (the surface does not suit
            // it) keeps facing its own way.
            let inward = Vec3::new(-position.x, 0.0, -position.z);
            let own = Vec3::new(poses[i].face.x, 0.0, poses[i].face.z);
            let towards = if own.dot(inward) < 0.0 {
                position + own
            } else {
                Vec3::new(0.0, ys[i], 0.0)
            };
            let upright = facing(position, towards);
            let p = Placement {
                position,
                rotation: (upright * lean_back).normalize(),
                width: poses[i].width,
            };
            (row[i].0, p)
        })
        .collect()
}

/// A flat row straightened in its own frame (its axis along +Y, the wall upright):
/// side by side in the wall's plane, the mean of the centres where it was, one line up
/// and down.
fn straighten_flat<K: Copy + Ord>(row: &[(K, Placement, PixelSize)]) -> Vec<(K, Placement)> {
    let poses: Vec<Pose> = row.iter().map(|(_, p, px)| Pose::of(p, *px)).collect();
    let face = {
        let f = median_direction(&poses.iter().map(|p| p.face).collect::<Vec<_>>());
        Vec3::new(f.x, 0.0, f.z).normalize_or(Vec3::Z)
    };
    let right = Vec3::Y.cross(face);
    let along: Vec<f32> = poses.iter().map(|p| p.centre.dot(right)).collect();
    let depth = poses.iter().map(|p| -p.centre.dot(face)).sum::<f32>() / poses.len() as f32;
    // Side by side in the row's own order.
    let mut offsets = vec![0.0f32; row.len()];
    let mut x = 0.0f32;
    for (i, p) in poses.iter().enumerate() {
        offsets[i] = x + p.width / 2.0;
        x += p.width;
    }
    let n = row.len() as f32;
    let start = along.iter().sum::<f32>() / n - offsets.iter().sum::<f32>() / n;
    let tall: Vec<f32> = poses.iter().map(|p| p.height).collect();
    let spans: Vec<(f32, f32)> = poses
        .iter()
        .map(|p| (p.centre.y + p.height / 2.0, p.centre.y - p.height / 2.0))
        .collect();
    let ys = one_line(&spans, &tall);
    let rotation = Quat::from_rotation_y(face.x.atan2(face.z));
    (0..row.len())
        .map(|i| {
            let position = right * (start + offsets[i]) - face * depth + Vec3::Y * ys[i];
            let p = Placement {
                position,
                rotation,
                width: poses[i].width,
            };
            (row[i].0, p)
        })
        .collect()
}

/// Whether `row` moved by `shift` keeps apart from the monitors `inner`: none covers
/// one of them as seen, and, seen in the frame `turn` (the whole's) with the laptop
/// `screen` where it stands and `rows` (as first seen) as what was, `row` is a row of its
/// own, so it is taken for one row with none of them (only that, without
/// `covering_counts`). Corners and rows are taken in that frame: where two
/// monitors are too far apart around the eye to be projected together, covering falls
/// back to angles, which must be the whole's own.
#[allow(clippy::too_many_arguments)]
fn apart<K: Copy + Ord>(
    row: &[K],
    inner: &[K],
    out: &BTreeMap<K, Placement>,
    local: &BTreeMap<K, (Placement, PixelSize)>,
    screen: Option<(K, (Placement, PixelSize))>,
    turn: Quat,
    shift: Vec3,
    rows: &[Vec<K>],
    covering_counts: bool,
) -> bool {
    let size = |k: K| match screen {
        Some((id, (_, px))) if id == k => px,
        _ => local[&k].1,
    };
    let placed = |k: K| {
        let mut p = match screen {
            Some((id, (p, _))) if id == k => p,
            _ => out[&k],
        };
        if row.contains(&k) {
            p.position += shift;
        }
        Placement {
            position: turn.inverse() * p.position,
            rotation: (turn.inverse() * p.rotation).normalize(),
            width: p.width,
        }
    };
    let height = |k: K| placed(k).width * size(k).aspect();
    let corners = |k: K| placed(k).corners(height(k));
    // Seen over it, or through it in the room: the middle of a flat monitor far wider
    // than the others bulges towards the eye beyond what its corners say.
    let covering = || {
        row.iter().any(|&u| {
            inner.iter().any(|&p| {
                covers(&corners(p), &corners(u), 0.001)
                    || cuts(&placed(p), height(p), &placed(u), height(u))
            })
        })
    };
    if covering_counts && covering() {
        return false;
    }
    let ids: Vec<K> = inner
        .iter()
        .chain(row)
        .copied()
        .chain(screen.map(|s| s.0))
        .collect();
    let monitors: Vec<Monitor<K>> = ids
        .iter()
        .map(|&k| Monitor {
            id: k,
            px: size(k),
            placement: placed(k),
        })
        .collect();
    let room = Room::new(&Frame::upright(Vec3::ZERO, 0.0), &monitors);
    let now = rows_of(&ids, &room, Some(rows));
    // Its monitors make one row there, with nobody else in it.
    let mut mine: Vec<K> = row.to_vec();
    mine.sort();
    now.iter().any(|r| {
        let mut r = r.clone();
        r.sort();
        r == mine
    })
}

/// How far the edge `a0`..`a1` is from lying along the edge `b0`..`b1` where they run
/// side by side: of the stretch of `b` that `a` reaches over (its ends seen square
/// from `b`), how far its two ends are from `a`. `None` when `a` reaches over none of
/// `b`. Two edges joined by the magnet along one line give 0; edges that only cross, or
/// one hanging crooked over the other, give how far one end of the stretch stands off.
/// With how long the stretch is, a share of the shorter edge.
fn along(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> Option<(f32, f32)> {
    let d = b1 - b0;
    let foot = |p: Vec3| (p - b0).dot(d) / d.length_squared().max(1e-12);
    let (t0, t1) = (foot(a0), foot(a1));
    let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
    if hi < lo {
        return None;
    }
    let shorter = d.length().min((a1 - a0).length()).max(1e-6);
    let share = (hi - lo) * d.length() / shorter;
    let to_a = |p: Vec3| {
        let e = a1 - a0;
        let s = ((p - a0).dot(e) / e.length_squared().max(1e-12)).clamp(0.0, 1.0);
        p.distance(a0 + e * s)
    };
    Some((to_a(b0 + d * lo).max(to_a(b0 + d * hi)), share))
}

/// The monitors of `row` glued to those of `inner` they stand over (`over`) or under, as
/// the monitors stood (`local`), into `out` as `(inner one, row one)`. Seen in the frame
/// `turn` (the whole's). Glued: the edges they face each other with run along each
/// other along at least [`SHARES`] of the shorter (a corner meeting a corner is no seam)
/// and lie along one another in the room to within [`GLUED`] of the shorter one's
/// height, at most [`GLUED_MOST`] (the magnet joins them along one line); or, the row
/// `lined` up already, meet as the eye sees them in the middle of the edge they share to
/// within [`CLOSED`] in the inner one's height (as lining up leaves a glued seam).
fn glued<K: Copy + Ord>(
    row: &[K],
    inner: &[K],
    local: &BTreeMap<K, (Placement, PixelSize)>,
    (turn, over, lined): (Quat, bool, bool),
    out: &mut Vec<(K, K)>,
) {
    let edges = |k: K| {
        let (p, px) = local[&k];
        let h = p.height(px);
        let [tl, tr, br, bl] = p.corners(h);
        (h, (tl, tr), (bl, br))
    };
    let stood = |k: K| local[&k].0;
    out.clear();
    for &u in row {
        let (hu, top_u, bottom_u) = edges(u);
        for &p in inner {
            let (hp, top_p, bottom_p) = edges(p);
            let ((a0, a1), (b0, b1)) = if over {
                (bottom_u, top_p)
            } else {
                (top_u, bottom_p)
            };
            let near = (GLUED * hu.min(hp)).min(GLUED_MOST);
            let in_room =
                along(a0, a1, b0, b1).is_some_and(|(off, share)| off <= near && share >= SHARES);
            let seen = lined
                && seam_gap(p, u, &stood, local, turn, Vec3::ZERO, over)
                    .is_some_and(|(gap, share)| gap.abs() * hp <= CLOSED && share >= SHARES);
            if in_room || seen {
                out.push((p, u));
            }
        }
    }
}

/// Monitor `k` where `at` puts it, moved by `shift`, in the frame `turn` (the whole's).
fn seen_in<K: Copy + Ord>(
    k: K,
    at: &dyn Fn(K) -> Placement,
    local: &BTreeMap<K, (Placement, PixelSize)>,
    turn: Quat,
    shift: Vec3,
) -> Monitor<K> {
    let q = at(k);
    Monitor {
        id: k,
        px: local[&k].1,
        placement: Placement {
            position: turn.inverse() * (q.position + shift),
            rotation: (turn.inverse() * q.rotation).normalize(),
            width: q.width,
        },
    }
}

/// How far apart `u` (of the row, moved by `shift`) is from `p` (of the rows before
/// it), both where `at` puts them, as the eye sees them in the middle of the edge they
/// share: a share of `p`'s height (negative where they overlap), with how much of the
/// shorter edge they share. Seen in the frame `turn` (the whole's), `u` over `p`
/// (`over`) or under it. `None` when they share none there.
fn seam_gap<K: Copy + Ord>(
    p: K,
    u: K,
    at: &dyn Fn(K) -> Placement,
    local: &BTreeMap<K, (Placement, PixelSize)>,
    turn: Quat,
    shift: Vec3,
    over: bool,
) -> Option<(f32, f32)> {
    let side = if over { Side::Above } else { Side::Below };
    let (mp, mu) = (
        seen_in(p, at, local, turn, Vec3::ZERO),
        seen_in(u, at, local, turn, shift),
    );
    if !near_around(&mp, &mu) {
        return None;
    }
    let r = relate_pair(&mp, &mu, side).filter(|r| r.shared > 0.0)?;
    Some((r.apart as f32, r.seam as f32))
}

/// How deep, as seen, the edges of `u` (moved by `shift`) and `p` facing each other
/// reach into the other monitor: the most along the stretch they share, as a share of
/// the height of the one reached into (0 when they do not overlap or do not face each
/// other). As for [`seam_gap`].
fn overlap<K: Copy + Ord>(
    p: K,
    u: K,
    at: &dyn Fn(K) -> Placement,
    local: &BTreeMap<K, (Placement, PixelSize)>,
    turn: Quat,
    shift: Vec3,
    over: bool,
) -> f32 {
    let side = if over { Side::Above } else { Side::Below };
    let (mp, mu) = (
        seen_in(p, at, local, turn, Vec3::ZERO),
        seen_in(u, at, local, turn, shift),
    );
    if !near_around(&mp, &mu) {
        return 0.0;
    }
    let into_p = reach_pair(&mp, &mu, side, OVERLAP_SAMPLES);
    let into_u = reach_pair(&mu, &mp, side.opposite(), OVERLAP_SAMPLES);
    let deepest = into_p.unwrap_or(0.0).max(into_u.unwrap_or(0.0));
    deepest.max(0.0) as f32
}

/// Whether two monitors stand within a quarter turn of each other around the eye: one
/// behind the other's back is no neighbour, and where around the eye its angles are cut
/// would decide what is seen of the two.
fn near_around<K>(a: &Monitor<K>, b: &Monitor<K>) -> bool {
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();
    flat(a.placement.position).dot(flat(b.placement.position)) > 0.0
}

/// Where `gap` (a seam's gap as the row moves outwards by so much) is 0, within `reach`
/// either way of 0 (to [`HALVED`]); `None` when it is not, or the seam goes.
fn close(gap: &dyn Fn(f32) -> Option<f32>, reach: f32) -> Option<f32> {
    let at0 = gap(0.0)?;
    // A gap closes inwards, an overlap outwards.
    let way = if at0 > 0.0 { -1.0 } else { 1.0 };
    let open = |s: f32| gap(way * s).map(|g| (g > 0.0) == (at0 > 0.0));
    let step = reach / 8.0;
    let (mut near, mut far) = (0.0f32, step);
    while open(far)? {
        if far >= reach {
            return None;
        }
        near = far;
        far += step;
    }
    while far - near > HALVED {
        let mid = (near + far) / 2.0;
        if open(mid).unwrap_or(false) {
            near = mid;
        } else {
            far = mid;
        }
    }
    Some(way * (near + far) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row tilted back as a whole is straightened on its cone: the ends of the edges
    /// nearer the axis meet, the rest stands a little apart (flat monitors on a cone),
    /// none goes into the next, and lining it up again finds it lined up already.
    #[test]
    fn a_tilted_row_is_lined_up_once() {
        let px = PixelSize::new(2560, 1440);
        for tilt in [0.0f32, 0.17, 0.35, 0.6] {
            let rough: Vec<(u32, Placement, PixelSize)> = (0..3)
                .map(|i| {
                    let yaw = (i as f32 - 1.0) * 0.62 + 0.01 * i as f32;
                    let position = Vec3::new(yaw.sin(), 0.02 * i as f32, -yaw.cos());
                    let rotation =
                        (Quat::from_rotation_y(-yaw) * Quat::from_rotation_x(-tilt)).normalize();
                    let p = Placement {
                        position,
                        rotation,
                        width: 1.25,
                    };
                    (i, p, px)
                })
                .collect();
            let row: Vec<(u32, Placement, PixelSize)> = straighten(&rough)
                .into_iter()
                .map(|(k, p)| (k, p, px))
                .collect();
            assert!(lined_up(&row, false), "tilt {tilt}: {row:?}");
            for w in row.windows(2) {
                let (a, b) = (&w[0].1, &w[1].1);
                let h = a.height(px);
                assert!(!cuts(a, h, b, h), "tilt {tilt}: through");
                // Corners: a's right ones, b's left ones (bottom, then top).
                let (ca, cb) = (a.corners(h), b.corners(h));
                let meet = ca[1].distance(cb[0]).min(ca[2].distance(cb[3]));
                assert!(meet < 2e-3, "tilt {tilt}: the near ends {meet} apart");
            }
        }
    }
}
