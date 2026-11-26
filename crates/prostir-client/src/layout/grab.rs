//! A thing held by a controller or a hand, moved along its ray (its numbers in
//! [`Feel`]); or a monitor moved on a sphere round the eye (step 8.3).

use glam::{Quat, Vec2, Vec3};

use super::{Placement, Ray, facing};
use crate::ids::Thing;
use crate::interaction::Feel;

/// A thing held by one controller or hand: a display, the menu or the keyboard
/// window.
#[derive(Debug, Clone, Copy)]
pub struct Grab {
    pub id: Thing,
    /// Distance from the controller to the grabbed point.
    pub distance: f32,
    /// Grabbed point relative to the monitor centre, in the monitor's plane, as a
    /// fraction of its width so resizing keeps the point under the controller.
    pub anchor: Vec2,
    /// For hands: distance at the start and how far the hand was from the head, so
    /// reaching out pushes the monitor away and pulling in brings it closer.
    reach: Option<(f32, f32)>,
    /// For hands taking hold of something within reach: it moves with the hand one to
    /// one, like a thing held (hand and its position when it was taken).
    near: bool,
    held_from: Option<(Vec3, Vec3)>,
    /// Moved on a sphere round the eye (see [`Grab::on_sphere`]).
    sphere: Option<OnSphere>,
}

/// A monitor moved on a sphere round the eye as it was when taken (the pivot), facing
/// it, upright. In a hand it turns about the pivot as the direction from the shoulder
/// to the hand turns (the wrist's turn does not move it), and comes nearer or goes
/// further only as the hand reaches in or out from the shoulder (a move that goes
/// across as much does not count, see [`Feel::reach_dominance`]), to the power
/// `power`, past a dead zone. By a controller it turns as the controller's ray does,
/// and its stick sets the distance and the size. Near the nearest and the furthest a
/// monitor may stand it slows down and stops ([`Feel::bound_soft_m`]).
#[derive(Debug, Clone, Copy)]
struct OnSphere {
    pivot: Vec3,
    shoulder: Vec3,
    /// The grabbed point's direction and distance from the pivot when taken.
    dir0: Vec3,
    d0: f32,
    /// The hand's direction and distance from the shoulder when taken (by a
    /// controller: its ray's direction).
    hand_dir0: Vec3,
    r0: f32,
    power: f32,
    /// By a controller's ray; its distance now (the stick's).
    by_ray: Option<f32>,
    /// How far the hand reached in or out since it took it, counting only the moves
    /// that went mostly that way, and where it was from the shoulder when a move was
    /// last counted (a move is judged once it went [`REACH_STEP`]: the tracking's
    /// jitter, which goes nowhere, never adds up).
    reached: f32,
    last_arm: Vec3,
    /// The nearest and the furthest it may go: the bounds, or where it was taken if
    /// that is outside them (it does not jump when taken, and goes no further out).
    lo: f32,
    hi: f32,
}

/// How far a hand moves before the move is judged as reaching in or out, or across.
const REACH_STEP: f32 = 0.01;

impl OnSphere {
    /// The distance `d` within its bounds, slowing down over the last `knee` before
    /// each (no more than it stood from the bound when taken, so it does not jump).
    fn bounded(&self, d: f32, knee: f32) -> f32 {
        let (lo, hi) = (self.lo, self.hi);
        let knee_lo = knee.min(self.d0 - lo).max(0.0);
        let knee_hi = knee.min(hi - self.d0).max(0.0);
        soft_clamp(d, (lo, knee_lo), (hi, knee_hi))
    }
}

/// `d` within `lo` to `hi`, slowing down over the last knee before each.
fn soft_clamp(d: f32, (lo, knee_lo): (f32, f32), (hi, knee_hi): (f32, f32)) -> f32 {
    if knee_lo > 0.0 && d < lo + knee_lo {
        lo + knee_lo * ((d - lo - knee_lo) / knee_lo).exp()
    } else if knee_hi > 0.0 && d > hi - knee_hi {
        hi - knee_hi * (-(d - hi + knee_hi) / knee_hi).exp()
    } else {
        d.clamp(lo, hi)
    }
}

impl Grab {
    pub fn start(id: Thing, distance: f32, local_hit: Vec2, placement: &Placement) -> Self {
        Self {
            id,
            distance,
            anchor: local_hit / placement.width,
            reach: None,
            near: false,
            held_from: None,
            sphere: None,
        }
    }

    /// For a hand at `hand` taking a monitor along `ray`: moved on a sphere round
    /// `eye` (see [`OnSphere`]), the hand's reach from `shoulder` to the power `power`.
    /// Within reach ([`Feel::near_take_m`]), it follows the hand one to one instead.
    pub fn on_sphere(
        mut self,
        ray: Ray,
        eye: Vec3,
        shoulder: Vec3,
        power: f32,
        feel: &Feel,
    ) -> Self {
        if self.distance < feel.near_take_m {
            self.near = true;
            return self;
        }
        let point = ray.origin + ray.dir * self.distance;
        let arm = ray.origin - shoulder;
        let d0 = point.distance(eye).max(0.05);
        self.sphere = Some(OnSphere {
            pivot: eye,
            shoulder,
            dir0: (point - eye).normalize_or(Vec3::NEG_Z),
            d0,
            hand_dir0: arm.normalize_or(Vec3::NEG_Z),
            r0: arm.length().max(0.05),
            power,
            by_ray: None,
            reached: 0.0,
            last_arm: arm,
            lo: feel.bound_near_m.min(d0),
            hi: feel.bound_far_m.max(d0),
        });
        self
    }

    /// For a controller taking a monitor along `ray`: moved on a sphere round `eye`
    /// as its ray turns, nearer, further and larger by its stick (see [`OnSphere`]).
    pub fn on_sphere_by_ray(mut self, ray: Ray, eye: Vec3, feel: &Feel) -> Self {
        let point = ray.origin + ray.dir * self.distance;
        let d0 = point.distance(eye).max(0.05);
        self.sphere = Some(OnSphere {
            pivot: eye,
            shoulder: ray.origin,
            dir0: (point - eye).normalize_or(Vec3::NEG_Z),
            d0,
            hand_dir0: ray.dir,
            r0: 1.0,
            power: 1.0,
            by_ray: Some(d0),
            reached: 0.0,
            last_arm: Vec3::ZERO,
            lo: feel.bound_near_m.min(d0),
            hi: feel.bound_far_m.max(d0),
        });
        self
    }

    /// Distance follows how far the hand reaches from the head (no thumbstick on hands).
    /// Within reach ([`Feel::near_take_m`]), it follows the hand one to one instead.
    pub fn with_reach(mut self, hand: Vec3, head: Vec3, feel: &Feel) -> Self {
        if self.distance < feel.near_take_m {
            self.near = true;
        } else {
            self.reach = Some((self.distance, hand.distance(head).max(0.05)));
        }
        self
    }

    /// Moves the monitor so the grabbed point follows the ray, facing the head.
    /// `stick` pushes/pulls (y) and resizes (x).
    pub fn update(
        &mut self,
        placement: &mut Placement,
        ray: Ray,
        head: Vec3,
        stick: Vec2,
        dt: f32,
        feel: &Feel,
    ) {
        if let Some(mut s) = self.sphere {
            let f = feel;
            let dead = |v: f32| if v.abs() < f.stick_dead { 0.0 } else { v };
            let (dir, d) = match s.by_ray {
                Some(d) => {
                    let turn = Quat::from_rotation_arc(s.hand_dir0, ray.dir);
                    let d =
                        (d * (1.0 + dead(stick.y) * f.stick_distance_per_s * dt)).clamp(s.lo, s.hi);
                    s.by_ray = Some(d);
                    placement.width = (placement.width
                        * (1.0 + dead(stick.x) * f.stick_width_per_s * dt))
                        .clamp(f.width_min_m, f.width_max_m);
                    (turn * s.dir0, d)
                }
                None => {
                    let arm = ray.origin - s.shoulder;
                    let turn = Quat::from_rotation_arc(s.hand_dir0, arm.normalize_or(s.hand_dir0));
                    // Only reaching in or out moves it nearer or further: a move that
                    // goes mostly that way, judged once it went a centimetre, past a
                    // dead zone.
                    let step = (arm - s.last_arm).length();
                    if step >= REACH_STEP {
                        let radial = arm.length() - s.last_arm.length();
                        let across = (step * step - radial * radial).max(0.0).sqrt();
                        if radial.abs() > f.reach_dominance * across {
                            s.reached += radial;
                        }
                        s.last_arm = arm;
                    }
                    let dr = s.reached;
                    let dr = dr.signum() * (dr.abs() - f.reach_dead_m).max(0.0);
                    let d = s.d0 * ((s.r0 + dr) / s.r0).max(0.05).powf(s.power);
                    (turn * s.dir0, d)
                }
            };
            let d = s.bounded(d, f.bound_soft_m);
            let point = s.pivot + dir * d;
            self.distance = point.distance(ray.origin);
            self.sphere = Some(s);
            let offset = (self.anchor * placement.width).extend(0.0);
            for _ in 0..2 {
                placement.position = point - placement.rotation * offset;
                placement.rotation = facing(placement.position, s.pivot);
            }
            return;
        }
        if self.near {
            let (hand, start) = *self
                .held_from
                .get_or_insert((ray.origin, placement.position));
            placement.position = start + (ray.origin - hand);
            placement.rotation = facing(placement.position, head);
            return;
        }
        let f = feel;
        let dead = |v: f32| if v.abs() < f.stick_dead { 0.0 } else { v };
        self.distance = match self.reach {
            Some((d0, r0)) => d0 * (ray.origin.distance(head) / r0).powf(f.reach_power),
            None => self.distance * (1.0 + dead(stick.y) * f.stick_distance_per_s * dt),
        }
        .clamp(f.distance_min_m, f.distance_max_m);
        placement.width = (placement.width * (1.0 + dead(stick.x) * f.stick_width_per_s * dt))
            .clamp(f.width_min_m, f.width_max_m);
        let point = ray.origin + ray.dir * self.distance;
        let offset = (self.anchor * placement.width).extend(0.0);
        // Two passes: the orientation depends on the position and vice versa.
        for _ in 0..2 {
            placement.position = point - placement.rotation * offset;
            placement.rotation = facing(placement.position, head);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thing_held_near_follows_the_hand_one_to_one() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let mut p = Placement {
            position: Vec3::new(0.0, 1.4, -0.6),
            rotation: facing(Vec3::new(0.0, 1.4, -0.6), head),
            width: 0.6,
        };
        let hand = Vec3::new(0.05, 1.35, -0.4);
        let f = Feel::DEFAULT;
        let mut g = Grab::start(Thing::Menu, 0.2, Vec2::ZERO, &p).with_reach(hand, head, &f);
        let ray = |o: Vec3| Ray {
            origin: o,
            dir: -Vec3::Z,
        };
        g.update(&mut p, ray(hand), head, Vec2::ZERO, 0.01, &f);
        // Pulled 25 cm towards the user: the menu comes 25 cm closer.
        g.update(
            &mut p,
            ray(hand + Vec3::new(0.0, 0.0, 0.25)),
            head,
            Vec2::ZERO,
            0.01,
            &f,
        );
        assert!((p.position.z - (-0.35)).abs() < 1e-4, "{:?}", p.position);
    }

    /// On the sphere: turning the wrist does not move the monitor; moving the hand
    /// round the shoulder turns it round the eye at the same distance; reaching out
    /// takes it further, as far as the power says, past the dead zone.
    #[test]
    fn a_monitor_on_the_sphere() {
        let f = Feel::DEFAULT;
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let shoulder = Vec3::new(0.17, 1.42, 0.0);
        let hand = Vec3::new(0.2, 1.3, -0.4);
        let at = Vec3::new(0.0, 1.6, -1.5);
        let mut p = Placement {
            position: at,
            rotation: facing(at, eye),
            width: 1.0,
        };
        let ray = Ray {
            origin: hand,
            dir: (at - hand).normalize(),
        };
        let mut g = Grab::start(Thing::Menu, at.distance(hand), Vec2::ZERO, &p)
            .on_sphere(ray, eye, shoulder, 1.0, &f);
        let turned = Ray {
            origin: hand,
            dir: Quat::from_rotation_y(0.4) * ray.dir,
        };
        g.update(&mut p, turned, eye, Vec2::ZERO, 0.01, &f);
        assert!(
            p.position.distance(at) < 1e-4,
            "the wrist: {:?}",
            p.position
        );
        // Round the shoulder, as far from it: as far from the eye, turned.
        let round = shoulder + Quat::from_rotation_y(0.3) * (hand - shoulder);
        g.update(
            &mut p,
            Ray {
                origin: round,
                ..ray
            },
            eye,
            Vec2::ZERO,
            0.01,
            &f,
        );
        assert!((p.position.distance(eye) - at.distance(eye)).abs() < 1e-3);
        assert!(p.position.distance(at) > 0.3, "{:?}", p.position);
        // Back round the shoulder, then out by 10 cm: 8 past the dead zone, the
        // distance by as much as the reach.
        g.update(&mut p, ray, eye, Vec2::ZERO, 0.01, &f);
        assert!(p.position.distance(at) < 1e-3, "{:?}", p.position);
        let arm = hand - shoulder;
        let out = shoulder + arm * ((arm.length() + 0.1) / arm.length());
        g.update(
            &mut p,
            Ray { origin: out, ..ray },
            eye,
            Vec2::ZERO,
            0.01,
            &f,
        );
        let want = at.distance(eye) * (arm.length() + 0.08) / arm.length();
        assert!((p.position.distance(eye) - want).abs() < 1e-3);
        // Facing the eye, upright.
        let normal = p.rotation * Vec3::Z;
        assert!(normal.dot((eye - p.position).normalize()) > 0.999);
        assert!((p.rotation * Vec3::X).y.abs() < 1e-4);
    }

    /// A monitor 1.5 m ahead taken by a hand on the sphere, with `p` there, the hand
    /// and the shoulder.
    fn taken_by_hand() -> (Grab, Placement, Vec3, Vec3, Vec3, Ray) {
        let f = Feel::DEFAULT;
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let shoulder = Vec3::new(0.17, 1.42, 0.0);
        let hand = Vec3::new(0.2, 1.3, -0.4);
        let at = Vec3::new(0.0, 1.6, -1.5);
        let p = Placement {
            position: at,
            rotation: facing(at, eye),
            width: 1.0,
        };
        let ray = Ray {
            origin: hand,
            dir: (at - hand).normalize(),
        };
        let g = Grab::start(Thing::Menu, at.distance(hand), Vec2::ZERO, &p)
            .on_sphere(ray, eye, shoulder, 1.0, &f);
        (g, p, eye, shoulder, hand, ray)
    }

    /// A hand moving as much across as in or out (diagonally, in small steps) does not
    /// change the distance: only a move mostly in or out does.
    #[test]
    fn only_reaching_in_or_out_changes_the_distance() {
        let f = Feel::DEFAULT;
        let (mut g, mut p, eye, shoulder, hand, ray) = taken_by_hand();
        let d0 = p.position.distance(eye);
        // Each step 5 mm out from the shoulder and 5 mm across.
        let mut at = hand;
        for _ in 0..30 {
            let out = (at - shoulder).normalize();
            let side = out.cross(Vec3::Y).normalize();
            at += (out + side) * 0.005;
            g.update(&mut p, Ray { origin: at, ..ray }, eye, Vec2::ZERO, 0.01, &f);
        }
        assert!(
            (p.position.distance(eye) - d0).abs() < 1e-3,
            "diagonally: {} from {d0}",
            p.position.distance(eye)
        );
        // Straight out from there by 10 cm, in small steps: further.
        let from = at;
        for i in 1..=20 {
            let at = from + (from - shoulder).normalize() * (i as f32 * 0.005);
            g.update(&mut p, Ray { origin: at, ..ray }, eye, Vec2::ZERO, 0.01, &f);
        }
        assert!(p.position.distance(eye) > d0 + 0.2, "out");
    }

    /// However far a hand reaches, the monitor stops at the bounds of the distance,
    /// slowing down before them.
    #[test]
    fn a_monitor_on_the_sphere_stops_at_the_bounds() {
        let f = Feel::DEFAULT;
        let (mut g, mut p, eye, shoulder, hand, ray) = taken_by_hand();
        let arm = (hand - shoulder).normalize();
        let mut last = p.position.distance(eye);
        // Further than an arm reaches, to be sure.
        for i in 1..=300 {
            let at = hand + arm * (i as f32 * 0.01);
            g.update(&mut p, Ray { origin: at, ..ray }, eye, Vec2::ZERO, 0.01, &f);
            let d = p.position.distance(eye);
            assert!(
                d <= f.bound_far_m + 1e-4 && d >= last - 1e-4,
                "{d} after {last}"
            );
            last = d;
        }
        assert!(
            last > f.bound_far_m - f.bound_soft_m,
            "near the bound: {last}"
        );
        for i in 1..=80 {
            let at = hand - arm * (i as f32 * 0.004);
            g.update(&mut p, Ray { origin: at, ..ray }, eye, Vec2::ZERO, 0.01, &f);
            let d = p.position.distance(eye);
            assert!(d >= f.bound_near_m - 1e-4, "{d}");
        }
    }

    /// By a controller: its ray turned turns the monitor about the eye at the same
    /// distance; its stick takes it further and makes it larger.
    #[test]
    fn a_controller_moves_a_monitor_on_the_sphere() {
        let f = Feel::DEFAULT;
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let at = Vec3::new(0.0, 1.6, -2.0);
        let controller = Vec3::new(0.2, 1.2, -0.3);
        let mut p = Placement {
            position: at,
            rotation: facing(at, eye),
            width: 1.0,
        };
        let ray = Ray {
            origin: controller,
            dir: (at - controller).normalize(),
        };
        let mut g = Grab::start(Thing::Menu, at.distance(controller), Vec2::ZERO, &p)
            .on_sphere_by_ray(ray, eye, &f);
        let turned = Ray {
            dir: Quat::from_rotation_y(0.5) * ray.dir,
            ..ray
        };
        g.update(&mut p, turned, eye, Vec2::ZERO, 0.01, &f);
        assert!(
            (p.position.distance(eye) - 2.0).abs() < 1e-3,
            "{:?}",
            p.position
        );
        let seen = (p.position - eye).normalize().angle_between(Vec3::NEG_Z);
        assert!((seen - 0.5).abs() < 0.05, "turned by {seen}");
        for _ in 0..10 {
            g.update(&mut p, turned, eye, Vec2::new(1.0, 1.0), 0.05, &f);
        }
        assert!(p.position.distance(eye) > 2.5, "{:?}", p.position);
        assert!(p.width > 1.3, "{}", p.width);
    }

    #[test]
    fn soft_clamp_slows_down_before_each_bound() {
        let (lo, hi) = ((0.35, 0.05), (6.0, 0.05));
        assert_eq!(soft_clamp(1.0, lo, hi), 1.0);
        assert!(soft_clamp(9.0, lo, hi) <= 6.0);
        assert!(soft_clamp(9.0, lo, hi) > 5.99);
        assert!(soft_clamp(-3.0, lo, hi) >= 0.35);
        let (a, b) = (soft_clamp(5.97, lo, hi), soft_clamp(5.98, lo, hi));
        assert!(a < b && b - a < 0.01);
        assert_eq!(soft_clamp(7.0, lo, (6.0, 0.0)), 6.0);
    }

    /// A monitor taken where it stands beyond 6 m (the app put it there) does not
    /// jump when taken, by a hand or a controller, and goes no further out.
    #[test]
    fn taken_beyond_the_bounds_it_does_not_jump() {
        let f = Feel::DEFAULT;
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let at = Vec3::new(0.0, 1.6, -7.0);
        let p0 = Placement {
            position: at,
            rotation: facing(at, eye),
            width: 2.0,
        };
        let controller = Vec3::new(0.2, 1.2, -0.3);
        let ray = Ray {
            origin: controller,
            dir: (at - controller).normalize(),
        };
        let mut p = p0;
        let mut g = Grab::start(Thing::Menu, at.distance(controller), Vec2::ZERO, &p)
            .on_sphere_by_ray(ray, eye, &f);
        g.update(&mut p, ray, eye, Vec2::ZERO, 0.01, &f);
        assert!(
            p.position.distance(at) < 1e-3,
            "by a controller: {:?}",
            p.position
        );
        g.update(&mut p, ray, eye, Vec2::new(0.0, 1.0), 0.5, &f);
        assert!(p.position.distance(eye) <= 7.0 + 1e-3, "no further");
        g.update(&mut p, ray, eye, Vec2::new(0.0, -1.0), 0.2, &f);
        assert!(p.position.distance(eye) < 6.9, "nearer");

        let shoulder = Vec3::new(0.17, 1.42, 0.0);
        let hand = Vec3::new(0.2, 1.3, -0.4);
        let ray = Ray {
            origin: hand,
            dir: (at - hand).normalize(),
        };
        let mut p = p0;
        let mut g = Grab::start(Thing::Menu, at.distance(hand), Vec2::ZERO, &p)
            .on_sphere(ray, eye, shoulder, 1.0, &f);
        g.update(&mut p, ray, eye, Vec2::ZERO, 0.01, &f);
        assert!(
            p.position.distance(at) < 1e-3,
            "by a hand: {:?}",
            p.position
        );
    }

    /// The tracking's jitter, a few millimetres to and fro for seconds, never adds
    /// up past the dead zone: the distance stays.
    #[test]
    fn jitter_does_not_change_the_distance() {
        let f = Feel::DEFAULT;
        let (mut g, mut p, eye, _, hand, ray) = taken_by_hand();
        let d0 = p.position.distance(eye);
        let mut seed: u32 = 3;
        let mut k = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        for _ in 0..2000 {
            // Up to 3 mm off where the hand is, every way.
            let at = hand + Vec3::new(k(), k(), k()) * 0.006;
            g.update(&mut p, Ray { origin: at, ..ray }, eye, Vec2::ZERO, 0.01, &f);
        }
        assert!(
            (p.position.distance(eye) - d0).abs() < 1e-3,
            "{} from {d0}",
            p.position.distance(eye)
        );
    }
}
