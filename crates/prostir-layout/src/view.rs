//! The desk plan's [`View`] of the room: how two monitors stand along the edge they
//! share, from where they float, as the eye sees them (what the headset knows).
//!
//! Along an edge a monitor's pixels are not spread evenly over the angle it takes: its
//! middle is nearer the eye than its ends. So the point on an edge seen at a given
//! angle is found on the edge itself (by bisection), and so is the pixel there.

use glam::{DVec3, Vec3};

use crate::desk::{NOISE, Relation, View};
use crate::space::{
    Frame, Monitor, Seen, Side, Touch, azimuth, elevation, see, seen_corners, unwrap,
};

/// The room as the desk plan sees it.
#[derive(Debug, Clone)]
pub struct Room<K> {
    monitors: Vec<Shown<K>>,
}

#[derive(Debug, Clone)]
struct Shown<K> {
    id: K,
    /// Corners in the frame: top left, top right, bottom right, bottom left.
    corners: [DVec3; 4],
    /// Pixels across and down.
    px: (f64, f64),
    seen: Seen<K>,
}

/// An edge as seen: a segment in the frame and the pixels along it.
#[derive(Debug, Clone, Copy)]
struct Edge {
    from: DVec3,
    to: DVec3,
    pixels: f64,
    /// Runs down (the pixel 0 is at `to`, the top) rather than across.
    down: bool,
}

impl<K: Copy + PartialEq> Room<K> {
    /// The monitors as seen from the frame's eye; their stream sizes are their sizes on
    /// the desktop (the headset's plan).
    pub fn new(frame: &Frame, monitors: &[Monitor<K>]) -> Self {
        let seen = see(frame, monitors);
        let monitors = monitors
            .iter()
            .zip(seen)
            .map(|(m, s)| Shown {
                id: m.id,
                corners: m
                    .placement
                    .corners(m.height())
                    .map(|c: Vec3| frame.to_local(c).as_dvec3()),
                px: (f64::from(m.px.w), f64::from(m.px.h)),
                seen: s,
            })
            .collect();
        Self { monitors }
    }

    /// Every monitor as seen.
    pub fn seen(&self) -> Vec<Seen<K>> {
        self.monitors.iter().map(|m| m.seen).collect()
    }

    fn get(&self, id: K) -> Option<&Shown<K>> {
        self.monitors.iter().find(|m| m.id == id)
    }

    /// Points along the edge `a` and `b` share as seen (`b` on `side` of `a`), `n` of
    /// them spread over the middle 90 % of it: for each, the pixel along `a`'s edge and
    /// the pixel along `b`'s (from the top, or from the left) seen at the same angle
    /// along the edge. Where the mouse leaves `a` at the first, it should enter `b` at
    /// the second.
    pub fn seam(&self, a: K, b: K, side: Side, n: usize) -> Vec<(f64, f64)> {
        let (Some(ma), Some(mb)) = (self.get(a), self.get(b)) else {
            return Vec::new();
        };
        let ea = edge(ma, side);
        let eb = edge(mb, side.opposite());
        let (ca, cb) = (ma.seen.center_x(), mb.seen.center_x());
        let range = |e: &Edge, around: f64| {
            let (x, y) = (e.angle(0.0, around), e.angle(1.0, around));
            (x.min(y), x.max(y))
        };
        let ((a0, a1), (b0, b1)) = (range(&ea, ca), range(&eb, cb));
        let (lo, hi) = (a0.max(b0), a1.min(b1));
        if hi <= lo {
            return Vec::new();
        }
        (0..n)
            .map(|j| {
                let f = 0.05 + 0.9 * (j as f64 + 0.5) / n as f64;
                let angle = lo + f * (hi - lo);
                (ea.pixel(ea.find(angle, ca)), eb.pixel(eb.find(angle, cb)))
            })
            .collect()
    }

    /// The angle seen along `k`'s edge on `side` (elevation for a side edge, azimuth for
    /// a top or bottom one, radians, cut where [`see`] cuts) at `pixel` along it (from
    /// the top, or the left).
    pub fn angle_along(&self, k: K, side: Side, pixel: f64) -> Option<f64> {
        let m = self.get(k)?;
        let e = edge(m, side);
        let t = if e.down {
            1.0 - pixel / e.pixels
        } else {
            pixel / e.pixels
        };
        Some(e.angle(t, m.seen.center_x()))
    }

    /// Metres per pixel along `k`'s edge on `side`.
    pub fn metres_per_pixel(&self, k: K, side: Side) -> Option<f64> {
        let m = self.get(k)?;
        let e = edge(m, side);
        Some((e.to - e.from).length() / e.pixels)
    }

    /// The pairs that meet as seen, from their edges: `b` on `side` of `a`, the facing
    /// edges at most `gap` apart (a fraction of `a`'s size across them; overlapping
    /// counts), sharing at least `seam` of the shorter edge. Of a pair seen both beside
    /// and over the other, the way they are apart (the larger gap) counts.
    pub fn touches(&self, gap: f64, seam: f64) -> Vec<Touch<K>> {
        let mut out = Vec::new();
        for (i, a) in self.monitors.iter().enumerate() {
            for b in &self.monitors[i + 1..] {
                let apart = |side: Side| {
                    let rel = self.relate(a.id, b.id, side).filter(|r| r.shared > 0.0)?;
                    Some((side, rel.apart, rel.seam))
                };
                let across = apart(Side::Right).or_else(|| apart(Side::Left));
                let up = apart(Side::Above).or_else(|| apart(Side::Below));
                let way = match (across, up) {
                    (Some(x), Some(y)) => Some(if x.1 >= y.1 { x } else { y }),
                    (x, y) => x.or(y),
                };
                if let Some((side, g, s)) = way
                    && g <= gap
                    && s >= seam
                {
                    out.push(Touch {
                        a: a.id,
                        b: b.id,
                        side,
                        seam: s,
                    });
                }
            }
        }
        out
    }

    /// The azimuth of `k`'s middle as seen (cut where [`see`] cuts).
    pub fn center_x(&self, k: K) -> Option<f64> {
        Some(self.get(k)?.seen.center_x())
    }
}

/// The edge of `m` on `side` (as a segment, pixel 0 at its start).
fn edge<K>(m: &Shown<K>, side: Side) -> Edge {
    let [tl, tr, br, bl] = m.corners;
    match side {
        Side::Right => Edge {
            from: br,
            to: tr,
            pixels: m.px.1,
            down: true,
        },
        Side::Left => Edge {
            from: bl,
            to: tl,
            pixels: m.px.1,
            down: true,
        },
        Side::Above => Edge {
            from: tl,
            to: tr,
            pixels: m.px.0,
            down: false,
        },
        Side::Below => Edge {
            from: bl,
            to: br,
            pixels: m.px.0,
            down: false,
        },
    }
}

impl Edge {
    fn at(&self, t: f64) -> DVec3 {
        self.from + (self.to - self.from) * t
    }

    /// The angle along the edge at `t`: elevation for an edge that runs down, else
    /// azimuth (unwrapped around `around`).
    fn angle(&self, t: f64, around: f64) -> f64 {
        let p = self.at(t);
        if self.down {
            elevation(p)
        } else {
            unwrap(azimuth(p), around)
        }
    }

    /// The angle across the edge at `t` (azimuth for an edge that runs down, else
    /// elevation).
    fn across(&self, t: f64, around: f64) -> f64 {
        let p = self.at(t);
        if self.down {
            unwrap(azimuth(p), around)
        } else {
            elevation(p)
        }
    }

    /// Where along the edge (0..=1) the angle along it is `angle` (clamped to the ends).
    fn find(&self, angle: f64, around: f64) -> f64 {
        let (a0, a1) = (self.angle(0.0, around), self.angle(1.0, around));
        let rising = a1 >= a0;
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            let below = self.angle(mid, around) < angle;
            if below == rising {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }

    /// The pixel along the edge at `t`, counted from the top (an edge that runs down) or
    /// from the left.
    fn pixel(&self, t: f64) -> f64 {
        if self.down {
            (1.0 - t) * self.pixels
        } else {
            t * self.pixels
        }
    }
}

impl<K: Copy + PartialEq> View<K> for Room<K> {
    fn relate(&self, a: K, b: K, side: Side) -> Option<Relation> {
        relate_shown(self.get(a)?, self.get(b)?, side)
    }

    fn between(&self, a: K, b: K) -> bool {
        let (Some(ma), Some(mb)) = (self.get(a), self.get(b)) else {
            return false;
        };
        let (sa, sb) = (ma.seen, mb.seen);
        let (l, r) = (sa.right, sb.left);
        let (lo, hi) = (sa.bottom.max(sb.bottom), sa.top.min(sb.top));
        if r - l <= NOISE * sa.width().min(sb.width()) || hi <= lo {
            return false;
        }
        self.monitors.iter().any(|c| {
            let s = c.seen;
            c.id != a
                && c.id != b
                && s.right.min(r) - s.left.max(l) > NOISE * (r - l)
                && s.top.min(hi) - s.bottom.max(lo) > NOISE * (hi - lo)
        })
    }

    fn middle(&self, k: K) -> Option<(f64, f64)> {
        self.get(k).map(|m| (m.seen.center_x(), m.seen.center_y()))
    }

    fn height(&self, k: K) -> Option<f64> {
        self.get(k).map(|m| m.seen.height())
    }
}

/// Two edges seen side by side over less than this angle (radians) meet at a point.
const FACING: f64 = 1e-9;

/// How `mb` stands on `side` of `ma` as seen (see [`View::relate`]). Whether they share
/// a stretch of edge (`shared` above 0) does not depend on which is asked about first.
fn relate_shown<K>(ma: &Shown<K>, mb: &Shown<K>, side: Side) -> Option<Relation> {
    // `b` must be on that side of `a` at all.
    let on_side = match side {
        Side::Right => mb.seen.center_x() > ma.seen.center_x(),
        Side::Left => mb.seen.center_x() < ma.seen.center_x(),
        Side::Above => mb.seen.center_y() > ma.seen.center_y(),
        Side::Below => mb.seen.center_y() < ma.seen.center_y(),
    };
    if !on_side {
        return None;
    }
    let ea = edge(ma, side);
    let eb = edge(mb, side.opposite());
    // Each edge's azimuths around its own monitor's middle: every middle is already
    // cut where `see` cuts, so the two edges are comparable.
    let (ca, cb) = (ma.seen.center_x(), mb.seen.center_x());
    let range = |e: &Edge, around: f64| {
        let (x, y) = (e.angle(0.0, around), e.angle(1.0, around));
        (x.min(y), x.max(y))
    };
    let ((a0, a1), (b0, b1)) = (range(&ea, ca), range(&eb, cb));
    let (lo, hi) = (a0.max(b0), a1.min(b1));
    let m = if hi > lo {
        (lo + hi) / 2.0
    } else {
        (a0 + a1 + b0 + b1) / 4.0
    };
    let (ta, tb) = (ea.find(m, ca), eb.find(m, cb));
    // They face each other along a stretch (more than a point where two corners meet,
    // to the last bits of a float), the same stretch whichever of the two is asked
    // about first; how long it is along `a`'s edge, in its pixels. Along an edge seen
    // passing under or over the eye the angle does not run one way, and the pixels
    // found at the two ends of the stretch can be one: then its share of the angle
    // `a`'s edge spans is taken.
    let shared = if hi - lo > FACING {
        let found = (ea.pixel(ea.find(hi, ca)) - ea.pixel(ea.find(lo, ca))).abs();
        if found > 0.0 {
            found
        } else {
            (hi - lo) / (a1 - a0) * ea.pixels
        }
    } else {
        0.0
    };
    // Across the edge, from `a` outwards, in `a`'s pixels.
    let (across_a, across_b) = (ea.across(ta, ca), eb.across(tb, cb));
    let outwards = match side {
        Side::Right | Side::Above => across_b - across_a,
        Side::Left | Side::Below => across_a - across_b,
    };
    let scale = match side {
        Side::Right | Side::Left => ma.px.0 / ma.seen.width(),
        Side::Above | Side::Below => ma.px.1 / ma.seen.height(),
    };
    let size_across = match side {
        Side::Right | Side::Left => ma.seen.width(),
        Side::Above | Side::Below => ma.seen.height(),
    };
    Some(Relation {
        offset: ea.pixel(ta) - eb.pixel(tb),
        at: ea.pixel(ta),
        shared,
        seam: ((hi - lo) / (a1 - a0).min(b1 - b0).max(1e-12)).clamp(0.0, 1.0),
        gap: outwards * scale,
        apart: outwards / size_across,
    })
}

/// `a` and `b` in a frame's own coordinates (the eye at the origin) as a room of their
/// own sees them, with nothing allocated: two monitors at a time.
fn shown_pair<K: Copy>(a: &Monitor<K>, b: &Monitor<K>) -> (Shown<K>, Shown<K>) {
    let shown = |m: &Monitor<K>, around: f64| {
        let corners = m.placement.corners(m.height()).map(|c: Vec3| c.as_dvec3());
        Shown {
            id: m.id,
            corners,
            px: (f64::from(m.px.w), f64::from(m.px.h)),
            seen: seen_corners(m.id, corners, around),
        }
    };
    let around = azimuth(a.placement.position.as_dvec3());
    let b_around = unwrap(azimuth(b.placement.position.as_dvec3()), around);
    (shown(a, around), shown(b, b_around))
}

/// How `b` stands on `side` of `a` as [`Room::relate`] has it for a room of the two,
/// both in a frame's own coordinates (the eye at the origin), with nothing allocated.
pub fn relate_pair<K: Copy>(a: &Monitor<K>, b: &Monitor<K>, side: Side) -> Option<Relation> {
    let (ma, mb) = shown_pair(a, b);
    relate_shown(&ma, &mb, side)
}

/// How far, as seen, the edge of `b` facing `a` reaches into `a` across the edge of `a`
/// on `side` where they face each other: the most of `samples` points spread over the
/// stretch they share, as a share of `a`'s size across (its height over or under it, its
/// width beside it); 0 or less where they do not overlap. Both in a frame's own
/// coordinates. `None` when `b` is not on that side or they share no stretch.
pub fn reach_pair<K: Copy>(
    a: &Monitor<K>,
    b: &Monitor<K>,
    side: Side,
    samples: usize,
) -> Option<f64> {
    let (ma, mb) = shown_pair(a, b);
    relate_shown(&ma, &mb, side).filter(|r| r.shared > 0.0)?;
    let ea = edge(&ma, side);
    let eb = edge(&mb, side.opposite());
    let (ca, cb) = (ma.seen.center_x(), mb.seen.center_x());
    let range = |e: &Edge, around: f64| {
        let (x, y) = (e.angle(0.0, around), e.angle(1.0, around));
        (x.min(y), x.max(y))
    };
    let ((a0, a1), (b0, b1)) = (range(&ea, ca), range(&eb, cb));
    let (lo, hi) = (a0.max(b0), a1.min(b1));
    let size_across = match side {
        Side::Right | Side::Left => ma.seen.width(),
        Side::Above | Side::Below => ma.seen.height(),
    };
    let n = samples.max(2);
    (0..n)
        .map(|j| {
            let angle = lo + (hi - lo) * j as f64 / (n - 1) as f64;
            let (across_a, across_b) = (
                ea.across(ea.find(angle, ca), ca),
                eb.across(eb.find(angle, cb), cb),
            );
            let into = match side {
                Side::Right | Side::Above => across_a - across_b,
                Side::Left | Side::Below => across_b - across_a,
            };
            into / size_across
        })
        .reduce(f64::max)
}

#[cfg(test)]
mod tests {
    use glam::Quat;

    use super::*;
    use crate::desk::View;
    use crate::space::{PixelSize, Placement, facing};

    /// Whichever of two is asked about first, they share a stretch of edge or they do
    /// not: on random rooms (the same every run), monitors all round the eye, under and
    /// over it too, some turned well away from facing it.
    #[test]
    fn a_shared_edge_is_shared_both_ways() {
        let mut seed = 7u64;
        let mut f = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 11) as f64 / (1u64 << 53) as f64) as f32
        };
        let sizes = [(2560, 1440), (1920, 1080), (1440, 2560), (5120, 1440)];
        let mut checked = 0;
        for _ in 0..3000 {
            let monitors: Vec<Monitor<u8>> = (0..3)
                .map(|id| {
                    let (az, el) = ((f() - 0.5) * 7.0, (f() - 0.5) * 3.0);
                    let r = 0.4 + 1.6 * f();
                    let at = Vec3::new(az.sin() * el.cos(), el.sin(), -az.cos() * el.cos()) * r;
                    let turn = Quat::from_rotation_y((f() - 0.5) * 1.5)
                        * Quat::from_rotation_x((f() - 0.5) * 1.5);
                    let (w, h) = sizes[(f() * 4.0) as usize % 4];
                    Monitor {
                        id,
                        px: PixelSize::new(w, h),
                        placement: Placement {
                            position: at,
                            rotation: facing(at, Vec3::ZERO) * turn,
                            width: 0.3 + 1.5 * f(),
                        },
                    }
                })
                .collect();
            let room = Room::new(&Frame::upright(Vec3::ZERO, 0.0), &monitors);
            for a in 0..3u8 {
                for b in 0..3u8 {
                    for side in [Side::Right, Side::Left, Side::Above, Side::Below] {
                        if a == b {
                            continue;
                        }
                        let one = room.relate(a, b, side).map(|r| r.shared > 0.0);
                        let other = room.relate(b, a, side.opposite()).map(|r| r.shared > 0.0);
                        assert_eq!(one, other, "{a} {side:?} {b}: {monitors:?}");
                        checked += usize::from(one == Some(true));
                    }
                }
            }
        }
        assert!(checked > 100, "only {checked} shared edges");
    }
}
