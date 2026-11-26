//! Where a monitor may stand (step 8.3, plans/step-8-interaction.md, section 5): its
//! nearest point no nearer the eye than 0.35 m, its middle no further than 6 m, 0.2 to
//! 8 m wide (a portrait one no taller) and no wider than 140° as seen, its middle no
//! lower than 70° below eye level (or, where the floor is known, its lower edge 5 cm
//! above it). The numbers are [`Feel`]'s. What a hand or a controller does to a
//! monitor goes through [`limit`]; the presets, "Line up" and the magnet keep their
//! own rules (rows within 40° of eye level, glued edges, nothing cut or hidden), and
//! what they make outside these bounds (a "1:1" monitor of many pixels, many monitors
//! far away) a hand takes no further out of them ([`Bounds::loosened_for`]).
//!
//! A curved monitor's chord is the flat one, and the arc lies behind it, never more
//! than half a cylinder (see `Cylinder`): its nearest point and how wide it looks are
//! the flat one's at most, so the flat rectangle stands for both.

#![forbid(unsafe_code)]

use glam::{Quat, Vec3};

use super::Placement;
use crate::interaction::Feel;

/// The bounds, from [`Feel`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub near_m: f32,
    pub far_m: f32,
    pub width_min_m: f32,
    pub width_max_m: f32,
    /// The widest a monitor may look, in radians.
    pub angle: f32,
    /// The lowest its middle may be below eye level, in radians.
    pub below: f32,
    /// How far above the floor its lower edge stays.
    pub floor_gap_m: f32,
}

impl Bounds {
    pub fn of(f: &Feel) -> Self {
        Self {
            near_m: f.bound_near_m,
            far_m: f.bound_far_m,
            width_min_m: f.width_min_m,
            width_max_m: f.width_max_m,
            angle: f.bound_angle_deg.to_radians(),
            below: f.bound_below_deg.to_radians(),
            floor_gap_m: f.bound_floor_m,
        }
    }
}

impl Bounds {
    /// These bounds, loosened as far as `was` (`aspect`: its height over its width)
    /// stands outside them, seen from `eye`: a monitor the app itself made larger or
    /// placed further (a "1:1" monitor of many pixels looks wider than 140°, many
    /// monitors stand further than 6 m) is not pushed from where it stands, but a hand
    /// takes it no further out of them.
    pub fn loosened_for(&self, was: &Placement, aspect: f32, eye: Vec3) -> Self {
        let height = was.width * aspect.max(0.0);
        let to = was.position - eye;
        if !to.is_finite() || !height.is_finite() {
            return *self;
        }
        Self {
            near_m: self.near_m.min(nearest(was, height, eye)),
            far_m: self.far_m.max(to.length()),
            width_min_m: self.width_min_m.min(was.width),
            width_max_m: self.width_max_m.max(was.width.max(height)),
            angle: self.angle.max(seen_angle(was, height, eye)),
            below: self.below.max(-elevation(to)),
            floor_gap_m: self.floor_gap_m,
        }
    }
}

impl Default for Bounds {
    fn default() -> Self {
        Self::of(&Feel::DEFAULT)
    }
}

/// What stays when a monitor is put within the bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Its size: it goes further away where it is too near or looks too wide (a
    /// monitor carried).
    Size,
    /// Its place: it is made smaller instead (a monitor resized where it stands).
    Place,
}

/// How near the eye the monitor at `p`, `height` high, comes: its nearest point.
pub fn nearest(p: &Placement, height: f32, eye: Vec3) -> f32 {
    let local = p.rotation.inverse() * (eye - p.position);
    let on = Vec3::new(
        local.x.clamp(-p.width / 2.0, p.width / 2.0),
        local.y.clamp(-height / 2.0, height / 2.0),
        0.0,
    );
    local.distance(on)
}

/// How wide the monitor at `p`, `height` high, looks from `eye` (radians): across, or
/// up and down if that is more.
pub fn seen_angle(p: &Placement, height: f32, eye: Vec3) -> f32 {
    let at = |x: f32, y: f32| p.position + p.rotation * Vec3::new(x, y, 0.0) - eye;
    let (w, h) = (p.width / 2.0, height / 2.0);
    let across = at(-w, 0.0).angle_between(at(w, 0.0));
    let up = at(0.0, -h).angle_between(at(0.0, h));
    across.max(up)
}

/// Whether `p` (`aspect`: its height over its width) stands within `b` seen from
/// `eye`, the floor at `floor` (where known); to a tenth of a millimetre or degree.
pub fn within(p: &Placement, aspect: f32, eye: Vec3, floor: Option<f32>, b: &Bounds) -> bool {
    let height = p.width * aspect;
    let to = p.position - eye;
    let low = match floor {
        Some(y) => lowest(p, height) >= y + b.floor_gap_m - 1e-4,
        None => elevation(to) >= -b.below - 1e-3,
    };
    p.width >= b.width_min_m - 1e-4
        && p.width <= widest(b, aspect) + 1e-4
        && to.length() <= b.far_m + 1e-4
        && nearest(p, height, eye) >= b.near_m - 1e-4
        && seen_angle(p, height, eye) <= b.angle + 1e-3
        && low
}

/// `p` (`aspect`: its height over its width) put within `b` as seen from `eye`, the
/// floor at `floor` (where known), keeping what `keep` says. A monitor within them
/// comes back as it was.
pub fn limit(
    mut p: Placement,
    aspect: f32,
    eye: Vec3,
    floor: Option<f32>,
    b: &Bounds,
    keep: Keep,
) -> Placement {
    if !p.position.is_finite() || !p.width.is_finite() {
        return p;
    }
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        9.0 / 16.0
    };
    p.width = p.width.clamp(b.width_min_m, widest(b, aspect));
    let fits = |p: &Placement| {
        let h = p.width * aspect;
        nearest(p, h, eye) >= b.near_m && seen_angle(p, h, eye) <= b.angle
    };
    // Further away, as little as it takes: along the line from the eye (over a
    // known floor, level, so it does not go down into it).
    let push = |p: Placement| {
        let to = p.position - eye;
        let dir = match floor {
            Some(_) => Vec3::new(to.x, 0.0, to.z).normalize_or(Vec3::NEG_Z),
            None => to.normalize_or(Vec3::NEG_Z),
        };
        let moved = |t: f32| Placement {
            position: p.position + dir * t,
            ..p
        };
        let mut hi = 0.05;
        while !fits(&moved(hi)) && hi < 4.0 * b.far_m {
            hi *= 2.0;
        }
        let mut lo = 0.0;
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if fits(&moved(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        moved(hi)
    };
    // Each step may undo another a little (brought nearer, lifted over the floor):
    // again until all hold, a few times at most.
    for _ in 0..4 {
        // Not too low: its middle turned up round the eye, or its lower edge lifted over
        // the floor.
        let to = p.position - eye;
        match floor {
            Some(y) => {
                let lift = y + b.floor_gap_m - lowest(&p, p.width * aspect);
                if lift > 0.0 {
                    p.position.y += lift;
                }
            }
            None if elevation(to) < -b.below => {
                let flat = Vec3::new(to.x, 0.0, to.z).normalize_or(Vec3::NEG_Z);
                let up = flat * b.below.cos() - Vec3::Y * b.below.sin();
                let turn = Quat::from_rotation_arc(to.normalize(), up);
                p.position = eye + up * to.length();
                p.rotation = (turn * p.rotation).normalize();
            }
            None => {}
        }
        if !fits(&p) {
            match keep {
                Keep::Size => p = push(p),
                Keep::Place => {
                    // Smaller, as little as it takes (no smaller than the least width);
                    // where even that is too near, further away too.
                    let (mut lo, mut hi) = (b.width_min_m, p.width);
                    for _ in 0..24 {
                        let mid = (lo + hi) / 2.0;
                        if fits(&Placement { width: mid, ..p }) {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    p.width = lo;
                    if !fits(&p) {
                        p = push(p);
                    }
                }
            }
        }
        // Not too far: its middle brought nearer (over a known floor, level, as long
        // as that is enough, so it does not go down into it).
        let to = p.position - eye;
        if to.length() > b.far_m {
            let flat = Vec3::new(to.x, 0.0, to.z);
            p.position = match floor {
                Some(_) if to.y.abs() < b.far_m => {
                    let h = (b.far_m * b.far_m - to.y * to.y).sqrt();
                    eye + Vec3::Y * to.y + flat.normalize_or(Vec3::NEG_Z) * h
                }
                _ => eye + to.normalize() * b.far_m,
            };
        }
        if within(&p, aspect, eye, floor, b) {
            break;
        }
    }
    p
}

/// The widest a monitor of `aspect` (its height over its width) may be: no side
/// longer than the most width (a portrait monitor no taller).
fn widest(b: &Bounds, aspect: f32) -> f32 {
    (b.width_max_m / aspect.max(1.0)).max(b.width_min_m)
}

/// Elevation of a direction from the eye: 0 at eye level, up positive.
fn elevation(to: Vec3) -> f32 {
    to.y.atan2(Vec3::new(to.x, 0.0, to.z).length())
}

/// The lowest point of the monitor at `p`, `height` high.
fn lowest(p: &Placement, height: f32) -> f32 {
    let (w, h) = (p.width / 2.0, height / 2.0);
    [(-w, -h), (w, -h), (-w, h), (w, h)]
        .map(|(x, y)| (p.position + p.rotation * Vec3::new(x, y, 0.0)).y)
        .into_iter()
        .fold(f32::INFINITY, f32::min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::facing;

    const EYE: Vec3 = Vec3::new(0.0, 1.6, 0.0);

    fn at(position: Vec3, width: f32) -> Placement {
        Placement {
            position,
            rotation: facing(position, EYE),
            width,
        }
    }

    /// A monitor within the bounds stays as it was.
    #[test]
    fn within_stays() {
        let b = Bounds::default();
        let p = at(Vec3::new(0.3, 1.5, -1.4), 1.2);
        assert!(within(&p, 0.5625, EYE, None, &b));
        assert_eq!(limit(p, 0.5625, EYE, None, &b, Keep::Size), p);
        assert_eq!(limit(p, 0.5625, EYE, None, &b, Keep::Place), p);
    }

    /// Too near: further along the line from the eye until its nearest point is
    /// 0.35 m away; too far: brought to 6 m.
    #[test]
    fn near_and_far() {
        let b = Bounds::default();
        let p = limit(
            at(Vec3::new(0.0, 1.6, -0.2), 0.6),
            0.5625,
            EYE,
            None,
            &b,
            Keep::Size,
        );
        assert!((p.position.distance(EYE) - 0.35).abs() < 1e-3, "{p:?}");
        assert_eq!(p.width, 0.6);
        let p = limit(
            at(Vec3::new(0.0, 1.6, -9.0), 1.0),
            0.5625,
            EYE,
            None,
            &b,
            Keep::Size,
        );
        assert!((p.position.distance(EYE) - 6.0).abs() < 1e-4, "{p:?}");
    }

    /// Too wide as seen: a carried one goes further, one resized gets smaller; never
    /// narrower than 0.2 m nor wider than 8 m.
    #[test]
    fn too_wide() {
        let b = Bounds::default();
        let wide = at(Vec3::new(0.0, 1.6, -0.6), 4.0);
        let carried = limit(wide, 0.3, EYE, None, &b, Keep::Size);
        assert_eq!(carried.width, 4.0);
        let seen = seen_angle(&carried, 1.2, EYE).to_degrees();
        assert!((seen - 140.0).abs() < 0.1, "{seen}");
        let resized = limit(wide, 0.3, EYE, None, &b, Keep::Place);
        assert_eq!(resized.position, wide.position);
        assert!(resized.width < 4.0 && within(&resized, 0.3, EYE, None, &b));
        let p = limit(
            at(Vec3::new(0.0, 1.6, -5.0), 20.0),
            0.3,
            EYE,
            None,
            &b,
            Keep::Size,
        );
        assert_eq!(p.width, 8.0);
        let p = limit(
            at(Vec3::new(0.0, 1.6, -1.0), 0.05),
            0.5,
            EYE,
            None,
            &b,
            Keep::Size,
        );
        assert_eq!(p.width, 0.2);
    }

    /// Too low: without a floor, turned up to 70° below eye level, still facing the
    /// eye; with one, lifted 5 cm over it.
    #[test]
    fn too_low() {
        let b = Bounds::default();
        let under = at(Vec3::new(0.0, 0.2, -0.3), 0.6);
        let p = limit(under, 0.5625, EYE, None, &b, Keep::Size);
        assert!(within(&p, 0.5625, EYE, None, &b), "{p:?}");
        let n = p.rotation * Vec3::Z;
        assert!(
            n.dot((EYE - p.position).normalize()) > 0.99,
            "facing the eye"
        );
        let floored = limit(
            at(Vec3::new(0.0, 0.2, -1.5), 1.0),
            0.5625,
            EYE,
            Some(0.0),
            &b,
            Keep::Size,
        );
        assert!(
            (lowest(&floored, 0.5625) - 0.05).abs() < 1e-3,
            "{floored:?}"
        );
    }

    /// A monitor already outside the bounds (larger than 140° as seen, further than
    /// 6 m) stays where it is, and goes no further out of them.
    #[test]
    fn what_stands_outside_is_not_pushed() {
        let b = Bounds::default();
        let big = at(Vec3::new(0.0, 1.6, -1.2), 7.0);
        let loose = b.loosened_for(&big, 0.3, EYE);
        assert!(!within(&big, 0.3, EYE, None, &b));
        assert_eq!(limit(big, 0.3, EYE, None, &loose, Keep::Size), big);
        let nearer = at(Vec3::new(0.0, 1.6, -0.9), 7.0);
        let p = limit(nearer, 0.3, EYE, None, &loose, Keep::Size);
        assert!(seen_angle(&p, 2.1, EYE) <= seen_angle(&big, 2.1, EYE) + 1e-3);
        let far = at(Vec3::new(0.0, 1.6, -8.0), 1.0);
        let loose = b.loosened_for(&far, 0.5625, EYE);
        let further = at(Vec3::new(0.0, 1.6, -9.0), 1.0);
        let p = limit(further, 0.5625, EYE, None, &loose, Keep::Size);
        assert!((p.position.distance(EYE) - 8.0).abs() < 1e-3, "{p:?}");
    }

    /// Whatever comes in (any place, size, turn), what comes out is within the
    /// bounds, on random monitors (the same ones every run).
    #[test]
    fn whatever_comes_in_goes_out_within() {
        let b = Bounds::default();
        let mut seed: u64 = 7;
        let mut f = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 40) as f32) / (1u64 << 24) as f32
        };
        for _ in 0..2000 {
            let position = Vec3::new(f() * 20.0 - 10.0, f() * 12.0 - 5.0, f() * 20.0 - 10.0);
            let rotation = Quat::from_euler(glam::EulerRot::YXZ, f() * 6.3, f() * 1.0 - 0.5, 0.0);
            let p = Placement {
                position,
                rotation,
                width: f() * 12.0,
            };
            let aspect = 0.2 + f() * 1.6;
            let floor = (f() < 0.3).then_some(0.0);
            for keep in [Keep::Size, Keep::Place] {
                let out = limit(p, aspect, EYE, floor, &b, keep);
                assert!(
                    within(&out, aspect, EYE, floor, &b),
                    "{p:?} {aspect} {floor:?} {keep:?}: {out:?}"
                );
            }
        }
    }
}
