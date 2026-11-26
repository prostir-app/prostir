//! A thing held by a controller or a hand, moved along its ray (its numbers in
//! [`Feel`]); or, to try (step 8, 8.0a), a monitor in a hand moved on a sphere round
//! the eye.

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

/// A monitor in a hand moved on a sphere round the eye as it was when taken (the
/// pivot): it turns about the pivot as the direction from the shoulder to the hand
/// turns (the wrist's turn does not move it), and comes nearer or goes further only
/// as the hand does from the shoulder, to the power `power`, past a dead zone. It
/// faces the pivot, upright.
#[derive(Debug, Clone, Copy)]
struct OnSphere {
    pivot: Vec3,
    shoulder: Vec3,
    /// The grabbed point's direction and distance from the pivot when taken.
    dir0: Vec3,
    d0: f32,
    /// The hand's direction and distance from the shoulder when taken.
    hand_dir0: Vec3,
    r0: f32,
    power: f32,
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
        self.sphere = Some(OnSphere {
            pivot: eye,
            shoulder,
            dir0: (point - eye).normalize_or(Vec3::NEG_Z),
            d0: point.distance(eye).max(0.05),
            hand_dir0: arm.normalize_or(Vec3::NEG_Z),
            r0: arm.length().max(0.05),
            power,
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
        if let Some(s) = self.sphere {
            let f = feel;
            let arm = ray.origin - s.shoulder;
            let turn = Quat::from_rotation_arc(s.hand_dir0, arm.normalize_or(s.hand_dir0));
            let dir = turn * s.dir0;
            // Only reaching in or out moves it nearer or further, past a dead zone.
            let dr = arm.length() - s.r0;
            let dr = dr.signum() * (dr.abs() - f.reach_dead_m).max(0.0);
            let d = (s.d0 * ((s.r0 + dr) / s.r0).max(0.05).powf(s.power))
                .clamp(f.distance_min_m, f.distance_max_m);
            let point = s.pivot + dir * d;
            self.distance = point.distance(ray.origin);
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
        // Out by 10 cm: 8 past the dead zone, the distance by as much as the reach.
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
}
