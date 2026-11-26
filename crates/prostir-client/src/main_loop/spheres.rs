//! What the overlay draws over everything, as spheres: lights where a fingertip is
//! about to touch the menu or a monitor and where a ray points, the rings of a touch,
//! a click and of "where is the mouse", a ring closing in while a press is held, the
//! magnet's edge while dragging, the hands and the rays. How the hands and the effects
//! look follows "Hands (trial)" in the preview build (as before step 8 otherwise).

use std::time::Instant;

use glam::Vec3;

use crate::app::Model;
use crate::ids::Thing;
use crate::interaction::{FrameInput, Hands, INDEX_TIP, Mark};
use crate::layout;
use crate::platform::openxr::overlay::{self, Sphere};

/// The light blue of hovers, touches and clicks.
const BLUE: [f32; 3] = [0.72, 0.84, 1.0];
/// A ring held towards what holding does.
const AMBER: [f32; 3] = [1.0, 0.82, 0.4];
/// Done: a right click, a take.
const GREEN: [f32; 3] = [0.6, 1.0, 0.75];

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
    let trial = model.trial();
    let (size, strength) = (trial.fx_size(), trial.fx_alpha());
    let rgba = |c: [f32; 3], a: f32| [c[0], c[1], c[2], (a * strength).min(1.0)];
    spheres.clear();
    // Light on the menu where a fingertip is about to touch: wide and faint from a
    // few centimetres away, small and bright right before contact.
    let place = world.menu();
    let hover = model.feel().poke_hover_m;
    for (_, at, z, pressed) in hands.pokes() {
        let near = (1.0 - z.max(0.0) / hover).clamp(0.0, 1.0);
        let pressed = if pressed { 0.25 } else { 0.0 };
        spheres.push(Sphere::glow(
            place.position + place.rotation * Vec3::new(at.x, at.y, 0.003),
            (0.034 - 0.020 * near) * size,
            rgba(BLUE, (0.12 + 0.6 * near * near + pressed).min(1.0)),
        ));
    }
    // The same on a monitor (step 8): a ring closing in as the finger comes near, the
    // touched point while pressed, and a ring closing in on a long press.
    let touch_hover = model.feel().touch_hover_m;
    for h in 0..2 {
        let Some(n) = hands.near_monitor(h) else {
            continue;
        };
        let at = n.point + n.normal * 0.003;
        if n.pressed {
            spheres.push(Sphere::ring(at, 0.012 * size, rgba([1.0; 3], 0.95)));
            spheres.push(Sphere::glow(at, 0.018 * size, rgba(BLUE, 0.6)));
            if n.held > 0.25 {
                let c = if n.held >= 1.0 { GREEN } else { AMBER };
                spheres.push(Sphere::ring(
                    at + n.normal * 0.001,
                    (0.04 - 0.026 * n.held) * size,
                    rgba(c, 0.4 + 0.5 * n.held),
                ));
            }
        } else {
            let k = (n.z.max(0.0) / touch_hover).clamp(0.0, 1.0);
            spheres.push(Sphere::ring(
                at,
                (0.007 + 0.03 * k) * size,
                rgba(BLUE, 0.35 + 0.55 * (1.0 - k)),
            ));
            spheres.push(Sphere::glow(
                at,
                0.02 * size,
                rgba(BLUE, 0.15 + 0.35 * (1.0 - k)),
            ));
        }
    }
    // A press held towards taking or a right click: a ring closing in.
    for h in 0..2 {
        if let Some((at, k)) = hands.progress(h) {
            let c = if k >= 1.0 { GREEN } else { AMBER };
            spheres.push(Sphere::ring(
                at,
                (0.05 - 0.035 * k) * size,
                rgba(c, 0.4 + 0.5 * k),
            ));
        }
    }
    // A touch or a click spreads out as a ring for a moment.
    for (center, k, mark) in hands.marks(now) {
        let c = match mark {
            Mark::Touch => [0.8, 0.9, 1.0],
            Mark::Right => GREEN,
        };
        spheres.push(Sphere::ring(
            center,
            (0.012 + 0.03 * k) * size,
            rgba(c, 0.9 * (1.0 - k)),
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
            radius * size,
            rgba(BLUE, alpha),
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
    // drawn, and a hand about to touch a menu is see-through. A hand pressing a
    // monitor is drawn stopped on its surface.
    let menu_hand = usize::from(settings.left_handed);
    for (h, j) in input.joints.iter().enumerate() {
        let Some(j) = j else { continue };
        if h == menu_hand && world.wrist().is_some() {
            continue;
        }
        let mut j = *j;
        if let Some(push) = hands.push(h) {
            for joint in &mut j {
                joint.0 += push;
            }
        }
        let tip = j[INDEX_TIP].0;
        let near_palm = world
            .wrist()
            .is_some_and(|w| w.position.distance(tip) < 0.15);
        let near = near_palm || hands.poking(h) || hands.near_monitor(h).is_some();
        let alpha = trial.hand_alpha(near);
        if trial.tips_only() {
            overlay::tip_spheres(&j, alpha, spheres);
        } else {
            overlay::hand_spheres(&j, alpha, spheres);
        }
    }
    for (h, hand) in input.hands.iter().enumerate() {
        if let Some((ray, length)) = hands.ray(h) {
            overlay::ray_spheres(ray.origin, ray.dir, length, spheres);
        }
        if let Some(ray) = hand.ray
            && !hand.is_hand
        {
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
