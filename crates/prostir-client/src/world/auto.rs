//! What "Auto" of the PC's monitor scales reads of the room (see `crate::autoscale`),
//! besides how large each monitor is shown (`World::shown_scale`): which ones are
//! glued edge to edge, and whether something is being moved. Nothing here allocates.

use glam::Vec3;

use super::World;
use crate::clarity;
use crate::ids::DisplayKey;
use crate::layout::Placement;
use crate::settings::Settings;

/// Two monitors whose edges are this close where they go (metres) are glued: the
/// magnet leaves them edge to edge (`layout::stick`), a hand's tremor apart at most.
const GLUED_WITHIN: f32 = 0.03;

impl World {
    /// How many headset pixels a pixel of a monitor of `size` pixels would take, added
    /// now (the user looking along `gaze`): where its placeholder stands, which it
    /// takes once its picture comes. None where the headset's pixels per degree are
    /// not known, or in cinema (new monitors are not placed there).
    pub fn shown_if_added(&self, size: (u32, u32), gaze: Vec3, s: &Settings) -> Option<f32> {
        let ppd = self.ppd.filter(|_| !self.cinema)?;
        let (place, _) = self.waiting_place(size, gaze);
        let curve = self.curve_radius(s);
        Some(clarity::shown(&place, size.0, self.center.0, curve, ppd))
    }

    /// A monitor is in a hand or being resized, or a profile is being switched to:
    /// nothing is settled.
    pub fn moving(&self) -> bool {
        self.carried() || self.switching()
    }

    /// Whether monitors `a` and `b` stand edge to edge where they go: a corner of one
    /// on an edge of the other (side by side, or one over or under a row).
    pub fn glued(&self, a: DisplayKey, b: DisplayKey) -> bool {
        let (Some(pa), Some(pb)) = (self.placements.get(&a), self.placements.get(&b)) else {
            return false;
        };
        let (ha, hb) = (self.height_of(a), self.height_of(b));
        if ha <= 0.0 || hb <= 0.0 {
            return false;
        }
        let (ca, cb) = (corners(pa, ha), corners(pb, hb));
        near_edges(&ca, &cb) || near_edges(&cb, &ca)
    }
}

/// The corners of flat monitor `p`, `h` metres high, in order round it.
fn corners(p: &Placement, h: f32) -> [Vec3; 4] {
    let (x, y) = (p.width / 2.0, h / 2.0);
    [(-x, -y), (x, -y), (x, y), (-x, y)]
        .map(|(a, b)| p.position + p.rotation * Vec3::new(a, b, 0.0))
}

/// Whether a corner of `of` lies on an edge of `to`, within [`GLUED_WITHIN`].
fn near_edges(of: &[Vec3; 4], to: &[Vec3; 4]) -> bool {
    of.iter()
        .any(|&c| (0..4).any(|i| to_segment(c, to[i], to[(i + 1) % 4]) <= GLUED_WITHIN))
}

/// The distance from `p` to the segment from `a` to `b`.
fn to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len = ab.length_squared();
    let t = if len > 0.0 {
        ((p - a).dot(ab) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    fn flat(x: f32, y: f32, width: f32) -> Placement {
        Placement {
            position: Vec3::new(x, y, -1.0),
            rotation: Quat::IDENTITY,
            width,
        }
    }

    /// Side by side, one over two, a gap: glued only edge to edge.
    #[test]
    fn edges_that_touch_are_glued() {
        let h = 0.36;
        let left = corners(&flat(-0.32, 0.0, 0.64), h);
        let right = corners(&flat(0.32, 0.0, 0.64), h);
        assert!(near_edges(&left, &right) || near_edges(&right, &left));
        let over = corners(&flat(0.0, 0.36, 0.64), h);
        assert!(near_edges(&left, &over) || near_edges(&over, &left));
        let apart = corners(&flat(1.2, 0.0, 0.64), h);
        assert!(!near_edges(&left, &apart) && !near_edges(&apart, &left));
    }
}
