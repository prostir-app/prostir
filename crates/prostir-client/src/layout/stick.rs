//! The magnet: where a monitor let go almost against another sticks to it, edge to
//! edge, as seen from the eye (on a wall, in the wall's plane).

use std::collections::HashMap;

use glam::{Quat, Vec2, Vec3};

use prostir_layout::space::{Frame, Monitor, PixelSize, Side, hidden_by, seen_over};
use prostir_layout::view::relate_pair;

use super::{Key, Placement, cuts};

/// A share of a monitor's face another may be seen over or behind that is only an edge
/// they share and the last bits of a float.
const SEEN_SLACK: f64 = 0.001;

/// Two monitors seen this close (a centimetre) are glued.
const GLUED: f32 = 0.01;

/// How much of a monitor hung over the seam between two glued ones, and turned there as
/// "Line up" turns it, those two may hide where their row curves round the eye (3 %;
/// the owner's 3840x2160 over two 2560x1440 on an arc: 1.3 %).
pub const SEAM_TURNED: f64 = 0.03;

/// ... and hung from the edge as it is, where it cannot be turned so (8 %).
pub const SEAM_HUNG: f64 = 0.08;

/// Lines `value` (half size `half`) up with a neighbour at `other` (half size
/// `other_half`) only when it is all but lined up already: centres, or first or last
/// edges, within 3 cm. Anything else stays exactly where the user put it.
fn align(value: f32, half: f32, other: f32, other_half: f32) -> f32 {
    const NEAR: f32 = 0.03;
    let options = [other, other + other_half - half, other - other_half + half];
    options
        .into_iter()
        .filter(|o| (o - value).abs() <= NEAR)
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
        .unwrap_or(value)
}

/// How monitor `m` stands next to `n`, seen from `eye`: projected onto `n`'s plane.
#[derive(Debug, Clone, Copy)]
struct Beside {
    /// Gap in `n`'s plane (metres; 0 when they overlap).
    gap: f32,
    /// Beside `n` (else above or below it).
    across: bool,
    /// Which side: 1 right or above, -1 left or below.
    side: f32,
    /// How far along that edge of `n` the centre of `m` is (metres, in `n`'s plane).
    along: f32,
}

/// The corners of a flat monitor `p`, `h` metres high.
pub(super) fn corners(p: &Placement, h: f32) -> [Vec3; 4] {
    let (x, y) = (p.width / 2.0, h / 2.0);
    [(-x, -y), (x, -y), (x, y), (-x, y)]
        .map(|(a, b)| p.position + p.rotation * Vec3::new(a, b, 0.0))
}

/// `m` (`mh` high) as seen from `eye` against `n` (`nh` high). `None` when `m` is not
/// in front of `n`'s plane from there (too far around to be its neighbour).
fn beside(n: &Placement, nh: f32, m: &Placement, mh: f32, eye: Vec3) -> Option<Beside> {
    let inv = n.rotation.inverse();
    let eye_l = inv * (eye - n.position);
    if eye_l.z <= 0.05 {
        return None;
    }
    let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    let mut seen = 0;
    for c in corners(m, mh) {
        let c = inv * (c - n.position);
        // Along the line of sight from the eye through the corner, onto n's plane. A
        // corner far around (on a wide arc) does not reach the plane: the near ones
        // are what count.
        let denom = eye_l.z - c.z;
        if denom <= 0.05 {
            continue;
        }
        let at = eye_l + (c - eye_l) * (eye_l.z / denom);
        lo = lo.min(at.truncate());
        hi = hi.max(at.truncate());
        seen += 1;
    }
    if seen < 2 {
        return None;
    }
    let (nw2, nh2) = (n.width / 2.0, nh / 2.0);
    let gx = (lo.x - nw2).max(-nw2 - hi.x);
    let gy = (lo.y - nh2).max(-nh2 - hi.y);
    let across = gx > gy;
    let mid = (lo + hi) / 2.0;
    let offset = inv * (m.position - n.position);
    Some(Beside {
        gap: gx.max(gy).max(0.0),
        across,
        side: if (if across { mid.x } else { mid.y }) >= 0.0 {
            1.0
        } else {
            -1.0
        },
        along: if across { offset.y } else { offset.x },
    })
}

/// `m` (`mw` × `mh`) joined to `n` (`nh` high) along one of its edges, like a hinge:
/// the shared edge is one line in the room, and `m` turns about it to face `eye`, so a
/// monitor beside `n` curves around the user and one above or below tilts towards
/// them like a laptop lid. `along`: where on that edge (see [`Beside::along`]).
fn hinge(n: &Placement, nh: f32, mw: f32, mh: f32, b: &Beside, eye: Vec3) -> Placement {
    let eye_l = n.rotation.inverse() * (eye - n.position);
    let (pivot, arm) = if b.across {
        (
            Vec3::new(b.side * n.width / 2.0, b.along, 0.0),
            Vec3::new(b.side * mw / 2.0, 0.0, 0.0),
        )
    } else {
        (
            Vec3::new(b.along, b.side * nh / 2.0, 0.0),
            Vec3::new(0.0, b.side * mh / 2.0, 0.0),
        )
    };
    let turn = |a: f32| {
        if b.across {
            Quat::from_rotation_y(a)
        } else {
            Quat::from_rotation_x(a)
        }
    };
    // The angle depends on where the centre ends up and the other way round.
    let mut a = 0.0f32;
    for _ in 0..8 {
        let d = eye_l - (pivot + turn(a) * arm);
        a = if b.across {
            d.x.atan2(d.z)
        } else {
            (-d.y).atan2(d.z)
        };
    }
    let r = turn(a);
    Placement {
        position: n.position + n.rotation * (pivot + r * arm),
        rotation: (n.rotation * r).normalize(),
        width: mw,
    }
}

/// A monitor of the arrangement: placement, height, width in pixels.
type Part<K> = (K, Placement, f32, f32);

/// The memory the magnet works in, kept from one call to the next: it is asked every
/// frame while a monitor is dragged, and allocates nothing once this is large enough.
#[derive(Debug)]
pub struct StickScratch<K> {
    parts: Vec<Part<K>>,
    /// The neighbours a monitor could join, nearest first.
    near: Vec<(Part<K>, Beside)>,
    /// On a wall: (id, centre in the wall's frame, width, height).
    wall: Vec<(K, Vec3, f32, f32)>,
}

impl<K> Default for StickScratch<K> {
    fn default() -> Self {
        Self {
            parts: Vec::new(),
            near: Vec::new(),
            wall: Vec::new(),
        }
    }
}

/// The placed ones of `monitors`, into `out`.
fn parts<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    out: &mut Vec<Part<K>>,
) {
    out.clear();
    out.extend(monitors.iter().filter_map(|&(id, w, h)| {
        let p = *placements.get(&id)?;
        Some((id, p, p.width * h as f32 / w.max(1) as f32, w.max(1) as f32))
    }));
}

/// The neighbours `m` could join among `others`, into `out`, nearest first as seen from
/// `eye` (between two it touches equally, the one its centre is closer to).
fn nearest<K: Key>(m: &Part<K>, others: &[Part<K>], eye: Vec3, out: &mut Vec<(Part<K>, Beside)>) {
    out.clear();
    out.extend(
        others
            .iter()
            .filter(|o| o.0 != m.0)
            .filter_map(|o| Some((*o, beside(&o.1, o.2, &m.1, m.2, eye)?))),
    );
    out.sort_unstable_by(|(a, ba), (b, bb)| {
        ba.gap.total_cmp(&bb.gap).then(
            a.1.position
                .distance(m.1.position)
                .total_cmp(&b.1.position.distance(m.1.position)),
        )
    });
}

/// Where along `n`'s edge `m` goes: where the user put it (lined up only when it all
/// but is), keeping some of the edges shared so the mouse can cross.
fn settle_along<K: Key>(b: &Beside, n: &Part<K>, mw: f32, mh: f32) -> f32 {
    let (half_m, half_n) = if b.across {
        (mh / 2.0, n.2 / 2.0)
    } else {
        (mw / 2.0, n.1.width / 2.0)
    };
    let shared = 0.15 * (2.0 * half_m).min(2.0 * half_n);
    align(b.along, half_m, 0.0, half_n).clamp(-(half_n + half_m - shared), half_n + half_m - shared)
}

/// The stream's size of `k` among `monitors`.
fn px_of<K: Key>(monitors: &[(K, u32, u32)], k: K) -> PixelSize {
    monitors
        .iter()
        .find(|x| x.0 == k)
        .map_or(PixelSize::new(1, 1), |&(_, w, h)| PixelSize::new(w, h))
}

/// Whether `m`, hung from `n` as `b` says, reaches over the seam between `n` and `o`:
/// `o` glued beside `n` across the edge `m` hangs from, a corner of `o` on the line of
/// that edge (their corners meet there), and `m` past the end of that edge, on `o`'s
/// side. A monitor over two glued ones.
fn over_the_seam<K: Key>(m: &Part<K>, n: &Part<K>, b: &Beside, o: &Part<K>, eye: Vec3) -> bool {
    if o.0 == n.0 || o.0 == m.0 {
        return false;
    }
    let (half_m, half_n) = if b.across {
        (m.2 / 2.0, n.2 / 2.0)
    } else {
        (m.1.width / 2.0, n.1.width / 2.0)
    };
    let (hinge, way) = if b.across {
        (Vec3::new(b.side * n.1.width / 2.0, 0.0, 0.0), Vec3::Y)
    } else {
        (Vec3::new(0.0, b.side * n.2 / 2.0, 0.0), Vec3::X)
    };
    let on_hinge = |c: Vec3| {
        let l = n.1.rotation.inverse() * (c - n.1.position) - hinge;
        l.reject_from_normalized(way).length() < GLUED
    };
    beside(&n.1, n.2, &o.1, o.2, eye).is_some_and(|g| {
        g.across != b.across && g.gap < GLUED && g.side * b.along + half_m > half_n
    }) && corners(&o.1, o.2).into_iter().any(on_hinge)
}

/// `m` hung over or under `n` at `at`, turned as "Line up" turns a row glued to the row
/// before it (see `prostir_layout::tidy`): about its own middle to face the axis of the
/// frame without leaning, its tilt kept, then moved up or down until the middle of
/// each seam it has with `n` and with what it reaches over beside `n` (`over`) closes as
/// the eye sees it, as near as the seam that would open most needs. `None` where no
/// seam can be closed so. Worked out in the frame's own coordinates; allocates nothing.
fn seam_turn<K: Key>(
    at: &Placement,
    (m, n): (&Part<K>, &Part<K>),
    b: &Beside,
    over: &dyn Fn(&Part<K>) -> bool,
    all: &[Part<K>],
    frame: &Frame,
    monitors: &[(K, u32, u32)],
) -> Option<Placement> {
    let local = frame.unplace(*at);
    let c = local.position;
    let pitch = local.normal().y.clamp(-1.0, 1.0).asin();
    let rotation = Quat::from_rotation_y((-c.x).atan2(-c.z)) * Quat::from_rotation_x(-pitch);
    let up = Vec3::Y * b.side;
    let moved = |s: f32| Placement {
        position: c + up * s,
        rotation,
        width: local.width,
    };
    let side = if b.side > 0.0 {
        Side::Above
    } else {
        Side::Below
    };
    let monitor = |k: K, placement: Placement| Monitor {
        id: k,
        px: px_of(monitors, k),
        placement,
    };
    // How far apart the two are across the seam in its middle, as seen, `m` moved by
    // `s`: closed at 0.
    let gap = |x: &Part<K>, s: f32| {
        relate_pair(
            &monitor(x.0, frame.unplace(x.1)),
            &monitor(m.0, moved(s)),
            side,
        )
        .filter(|r| r.shared > 0.0)
        .map(|r| r.apart)
    };
    let close = |x: &Part<K>| -> Option<f32> {
        let (mut lo, mut hi) = (-m.2 / 4.0, m.2 / 4.0);
        if !(gap(x, lo)? < 0.0 && gap(x, hi)? > 0.0) {
            return None;
        }
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if gap(x, mid)? > 0.0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Some(hi)
    };
    let target = std::iter::once(n)
        .chain(all.iter().filter(|o| over(o)))
        .filter_map(close)
        .reduce(f32::min)?;
    Some(frame.place(moved(target)))
}

/// Whether `m` at `p` stands clear of every other of `all`: facing the eye, through
/// none, and seen over or behind none, but for `n` and what it reaches over beside it
/// (`under`), which may hide `cap` of it (never it any of them).
fn fits<K: Key>(
    p: &Placement,
    m: &Part<K>,
    under: &dyn Fn(&Part<K>) -> bool,
    all: &[Part<K>],
    eye: Vec3,
    cap: f64,
) -> bool {
    p.normal().dot(eye - p.position) > 0.0
        && all.iter().filter(|o| o.0 != m.0).all(|o| {
            !cuts(p, m.2, &o.1, o.2)
                && if under(o) {
                    hidden_by(&o.1, o.2, p, m.2, eye) <= SEEN_SLACK
                        && hidden_by(p, m.2, &o.1, o.2, eye) <= cap
                } else {
                    seen_over(p, m.2, &o.1, o.2, eye) <= SEEN_SLACK
                        && seen_over(&o.1, o.2, p, m.2, eye) <= SEEN_SLACK
                }
        })
}

/// Where `m`, hung from `n` at `at` as `b` says, goes: there when it stands clear of
/// every other (see [`fits`]). Hung over the seam between `n` and one glued beside it,
/// it reaches past their corner, and where their row curves round the eye the two hide
/// a little of it: then turned as "Line up" would turn it ([`seam_turn`]), they may
/// hide [`SEAM_TURNED`] of it, else as it hangs [`SEAM_HUNG`]. `None`: not there.
fn settle<K: Key>(
    at: Placement,
    (m, n): (&Part<K>, &Part<K>),
    b: &Beside,
    all: &[Part<K>],
    frame: &Frame,
    monitors: &[(K, u32, u32)],
) -> Option<Placement> {
    let eye = frame.eye;
    let over = |o: &Part<K>| over_the_seam(m, n, b, o, eye);
    if !all.iter().any(over) {
        return fits(&at, m, &|o| o.0 == n.0, all, eye, SEEN_SLACK).then_some(at);
    }
    let under = |o: &Part<K>| o.0 == n.0 || over(o);
    if !b.across
        && let Some(turned) = seam_turn(&at, (m, n), b, &over, all, frame, monitors)
        && fits(&turned, m, &under, all, eye, SEAM_TURNED)
    {
        return Some(turned);
    }
    fits(&at, m, &under, all, eye, SEAM_HUNG).then_some(at)
}

/// Where a monitor being let go of would stick: the monitor it joins and the edge.
#[derive(Debug, Clone, Copy)]
pub struct Stick<K> {
    pub placement: Placement,
    pub neighbour: K,
    /// Side by side (else above or below).
    pub across: bool,
    /// Which side of the neighbour: 1 right or above, -1 left or below.
    pub side: f32,
}

/// The magnet for one monitor: let go almost against another (within 15 % of a
/// monitor, as seen from `eye`) and at about the same distance, where it would stick
/// to it, edge to edge, where along the edge it is (see [`hinge`]); `None` otherwise.
/// Joined to its nearest neighbour, a monitor that would go through another one (a
/// hinge turns it towards the eye, into a third monitor beside or under it), lie over
/// or behind one as seen, or turn its back to the eye is joined to the next nearest
/// instead, and with none it stays where it was let go (see [`settle`]). `center`: the
/// eye and the way the arrangement faces (the headset's `center`; the magnet works
/// only upright). Worked out in `scratch`.
pub fn stick<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    center: (Vec3, f32),
    id: K,
    scratch: &mut StickScratch<K>,
) -> Option<Stick<K>> {
    let frame = Frame::upright(center.0, center.1);
    let eye = center.0;
    parts(placements, monitors, &mut scratch.parts);
    let all = &scratch.parts;
    let m = *all.iter().find(|m| m.0 == id)?;
    nearest(&m, all, eye, &mut scratch.near);
    let mut first = true;
    for &(n, b) in &scratch.near {
        if b.gap >= 0.15 * m.1.width.min(n.1.width) {
            continue;
        }
        let b = Beside {
            along: settle_along(&b, &n, m.1.width, m.2),
            ..b
        };
        let placement = hinge(&n.1, n.2, m.1.width, m.2, &b, eye);
        // Pulled much nearer or pushed away on purpose: it stays there.
        let depth = m.1.position.distance(eye) / placement.position.distance(eye).max(0.1);
        if !(0.7..=1.45).contains(&depth) {
            if first {
                return None;
            }
            continue;
        }
        first = false;
        // Turned away from the eye, through another one, or seen over or behind
        // another one (hung from an edge in front of a monitor further away, or folded
        // back behind the one it hangs from): not there.
        let Some(placement) = settle(placement, (&m, &n), &b, all, &frame, monitors) else {
            continue;
        };
        return Some(Stick {
            placement,
            neighbour: n.0,
            across: b.across,
            side: b.side,
        });
    }
    None
}

/// The magnet on a wall: like [`stick`], but in the wall's plane (monitors there are
/// flat in one plane, facing the same way). Worked out in `scratch`.
pub fn stick_on_wall<K: Key>(
    placements: &HashMap<K, Placement>,
    monitors: &[(K, u32, u32)],
    center: (Vec3, f32),
    id: K,
    scratch: &mut StickScratch<K>,
) -> Option<Stick<K>> {
    let unturn = Quat::from_rotation_y(-center.1);
    let turn = Quat::from_rotation_y(center.1);
    scratch.wall.clear();
    scratch
        .wall
        .extend(monitors.iter().filter_map(|&(oid, w, h)| {
            let p = placements.get(&oid)?;
            Some((
                oid,
                unturn * (p.position - center.0),
                p.width,
                p.width * h as f32 / w.max(1) as f32,
            ))
        }));
    let all = &scratch.wall;
    let (_, mut at, mw, mh) = *all.iter().find(|m| m.0 == id)?;
    let gaps = |a: Vec3, aw: f32, ah: f32, b: Vec3, bw: f32, bh: f32| {
        (
            (a.x - b.x).abs() - (aw + bw) / 2.0,
            (a.y - b.y).abs() - (ah + bh) / 2.0,
        )
    };
    let sep = |o: &(K, Vec3, f32, f32)| {
        let (gx, gy) = gaps(at, mw, mh, o.1, o.2, o.3);
        (gx.max(gy).max(0.0), at.distance(o.1))
    };
    let others = || all.iter().filter(|o| o.0 != id);
    let n = *others().min_by(|a, b| {
        let (sa, da) = sep(a);
        let (sb, db) = sep(b);
        sa.total_cmp(&sb).then(da.total_cmp(&db))
    })?;
    let depth = at.z / n.1.z.min(-0.1);
    if sep(&n).0 >= mw.min(n.2) / 3.0 || !(0.7..=1.45).contains(&depth) {
        return None;
    }
    let (gx, gy) = gaps(at, mw, mh, n.1, n.2, n.3);
    let across = gx > gy;
    let side = |a: f32, b: f32| if a >= b { 1.0 } else { -1.0 };
    let dir = if across {
        side(at.x, n.1.x)
    } else {
        side(at.y, n.1.y)
    };
    at.z = n.1.z;
    if across {
        at.x = n.1.x + dir * (n.2 + mw) / 2.0;
        at.y = align(at.y, mh / 2.0, n.1.y, n.3 / 2.0);
    } else {
        at.y = n.1.y + dir * (n.3 + mh) / 2.0;
        at.x = align(at.x, mw / 2.0, n.1.x, n.2 / 2.0);
    }
    // Still on top of another one: past it, the same way.
    for _ in 0..others().count() {
        let Some(o) = others().find(|o| {
            let (gx, gy) = gaps(at, mw, mh, o.1, o.2, o.3);
            gx < -1e-3 && gy < -1e-3
        }) else {
            break;
        };
        if across {
            at.x = o.1.x + dir * (o.2 + mw) / 2.0;
        } else {
            at.y = o.1.y + dir * (o.3 + mh) / 2.0;
        }
    }
    Some(Stick {
        placement: Placement {
            position: center.0 + turn * at,
            rotation: turn,
            width: mw,
        },
        neighbour: n.0,
        across,
        side: dir,
    })
}
