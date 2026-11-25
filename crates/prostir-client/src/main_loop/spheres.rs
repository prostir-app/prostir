//! What the overlay draws over everything, as spheres: lights where a fingertip is
//! about to touch the menu and where a ray points, the rings of a touch and of
//! "where is the mouse", the magnet's edge while dragging, the hands and the rays.

use std::time::Instant;

use glam::Vec3;

use crate::app::Model;
use crate::ids::Thing;
use crate::interaction::{FrameInput, Hands, INDEX_TIP, POKE_HOVER};
use crate::layout;
use crate::platform::openxr::overlay::{self, Sphere};

/// The frame's spheres into `spheres` (emptied first).
pub fn fill(
    spheres: &mut Vec<Sphere>,
    model: &mut Model,
    hands: &Hands,
    input: &FrameInput,
    now: Instant,
) {
    let world = model.world();
    let settings = *model.settings();
    spheres.clear();
    // Light on the menu where a fingertip is about to touch: wide and faint from a
    // few centimetres away, small and bright right before contact.
    let place = world.menu();
    for (_, at, z, pressed) in hands.pokes() {
        let near = (1.0 - z.max(0.0) / POKE_HOVER).clamp(0.0, 1.0);
        let pressed = if pressed { 0.25 } else { 0.0 };
        spheres.push(Sphere::glow(
            place.position + place.rotation * Vec3::new(at.x, at.y, 0.003),
            0.034 - 0.020 * near,
            [
                0.72,
                0.84,
                1.0,
                (0.12 + 0.6 * near * near + pressed).min(1.0),
            ],
        ));
    }
    // A touch spreads out as a ring for a moment.
    for (center, k) in hands.rings(now) {
        spheres.push(Sphere::ring(
            center,
            0.012 + 0.03 * k,
            [0.8, 0.9, 1.0, 0.9 * (1.0 - k)],
        ));
    }
    // Where a ray points: a small light on the menu, a wider faint one on monitors.
    for pick in hands.picks().iter().flatten() {
        let (radius, alpha) = match pick.thing {
            Thing::Menu | Thing::MenuButton | Thing::Keyboard => (0.012, 0.55),
            Thing::Display(_) | Thing::Adding { .. } => (0.03, 0.3),
            Thing::Attach(_) => (0.01, 0.5),
            Thing::Wrist => (0.008, 0.55),
        };
        spheres.push(Sphere::glow(
            pick.hit.point,
            radius,
            [0.72, 0.84, 1.0, alpha],
        ));
    }
    // Where the mouse is: rings closing in on it, three times.
    if let Some((id, uv, t)) = model.finder()
        && let Some(p) = world.shown(id)
    {
        let c = world
            .curve_radius(&settings)
            .filter(|_| !world.is_window(id));
        let (point, _) = layout::image_point(p, world.height_of(id), c, uv);
        for k in 0..3 {
            let phase = (t * 3.0 - k as f32 * 0.33).rem_euclid(1.0);
            spheres.push(Sphere::ring(
                point,
                0.09 * (1.0 - phase) + 0.01,
                [1.0, 0.85, 0.35, 0.9 * phase * (1.0 - t * 0.5)],
            ));
        }
        spheres.push(Sphere::glow(point, 0.02, [1.0, 0.85, 0.35, 0.8]));
    }
    // The magnet while dragging: the edge the monitor would stick to lights up.
    for thing in hands.held() {
        let Some(s) = thing.display().and_then(|id| model.magnet(id)) else {
            continue;
        };
        let world = model.world();
        let Some(n) = world.shown(s.neighbour).copied() else {
            continue;
        };
        let (half_w, half_h) = (n.width / 2.0, world.height_of(s.neighbour) / 2.0);
        let steps = 28;
        for i in 0..=steps {
            let k = i as f32 / steps as f32 * 2.0 - 1.0;
            let local = if s.across {
                Vec3::new(s.side * half_w, k * half_h, 0.01)
            } else {
                Vec3::new(k * half_w, s.side * half_h, 0.01)
            };
            spheres.push(Sphere::glow(
                n.position + n.rotation * local,
                0.03,
                [0.55, 0.75, 1.0, 0.55],
            ));
        }
    }
    let world = model.world();
    // Hands are drawn over everything (the compositor has no depth for them), so
    // near a menu they would cover its text: the hand holding the palm menu is not
    // drawn, and a hand about to touch a menu is see-through.
    let menu_hand = usize::from(settings.left_handed);
    for (h, j) in input.joints.iter().enumerate() {
        let Some(j) = j else { continue };
        if h == menu_hand && world.wrist().is_some() {
            continue;
        }
        let tip = j[INDEX_TIP].0;
        let near_palm = world
            .wrist()
            .is_some_and(|w| w.position.distance(tip) < 0.15);
        let near_panel = hands.poking(h);
        let alpha = if near_palm || near_panel { 0.3 } else { 1.0 };
        overlay::hand_spheres(j, alpha, spheres);
    }
    for (input, pick) in input.hands.iter().zip(hands.picks()) {
        if let Some(ray) = input.ray {
            let length = pick.map_or(1.2, |p| p.hit.t).min(4.0);
            overlay::ray_spheres(ray.origin, ray.dir, length, spheres);
            if !input.is_hand {
                // Where the controller is, since it is not visible otherwise.
                spheres.push(Sphere {
                    center: ray.origin,
                    radius: 0.012,
                    color: [0.30, 0.32, 0.38, 1.0],
                    kind: overlay::Kind::Ball,
                });
            }
        }
    }
}
