//! A thing held by a controller or a hand, moved along its ray (its numbers in
//! [`Feel`]).

use glam::{Vec2, Vec3};

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
        }
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
}
