//! What the headset draws itself over everything, each frame (step 8, section 6): the
//! hands, the rays, and the effects that answer what a hand or a controller does, since
//! a hand has no vibration: a light where a fingertip or a ray is about to act, a ring
//! under a press, a ring closing in while a hold runs, a wave where a tap, a click or a
//! take happened, a ring where a hand holds what it carries, the rings of "where is the
//! mouse", the edge a carried monitor would stick to. Colours, opacity and sizes come
//! from one [`Palette`]; how the hands look and how strong the effects are follows
//! "Hands (trial)" in the preview build.
//!
//! All of it goes into the hands' layer (the shell's `overlay`): no layer of its own.
//! The list lives as long as the app and is filled again each frame, so a frame takes
//! no memory; a frame with nothing to show leaves it empty and the layer is not drawn.

#![forbid(unsafe_code)]

use std::time::Instant;

use glam::Vec3;

use crate::app::Model;
use crate::ids::Thing;
use crate::interaction::{FrameInput, Hands, INDEX_TIP, Joints, Mark};
use crate::layout;
use crate::palette::{Palette, Shades};

/// How many to make room for at the start: two hands, two rays and the effects.
pub const CAPACITY: usize = 2048;

/// How a [`Fx`] is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A shaded ball (hands, rays).
    Ball,
    /// A soft light that brightens what is behind it: where a fingertip or a ray is
    /// about to touch, like the system's own windows.
    Glow,
    /// A thin bright ring.
    Ring,
}

/// One thing to draw, facing the eye: world position, radius in metres, straight
/// (not premultiplied) linear RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fx {
    pub center: Vec3,
    pub radius: f32,
    pub color: [f32; 4],
    pub kind: Kind,
}

impl Fx {
    pub fn glow(center: Vec3, radius: f32, color: [f32; 4]) -> Self {
        Self {
            center,
            radius,
            color,
            kind: Kind::Glow,
        }
    }

    pub fn ring(center: Vec3, radius: f32, color: [f32; 4]) -> Self {
        Self {
            center,
            radius,
            color,
            kind: Kind::Ring,
        }
    }

    pub fn ball(center: Vec3, radius: f32, color: [f32; 4]) -> Self {
        Self {
            center,
            radius,
            color,
            kind: Kind::Ball,
        }
    }
}

/// The frame's list, and the palette it was drawn with.
#[derive(Debug)]
pub struct Effects {
    list: Vec<Fx>,
    palette: Palette,
    shades: Shades,
}

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

impl Effects {
    pub fn new() -> Self {
        Self {
            list: Vec::with_capacity(CAPACITY),
            palette: Palette::DEFAULT,
            shades: Palette::DEFAULT.shades(),
        }
    }

    /// This frame's list (the shell sorts it in place for each eye).
    pub fn list(&mut self) -> &mut [Fx] {
        &mut self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// This frame's list, made anew from the model and the hands.
    pub fn fill(&mut self, model: &mut Model, hands: &Hands, input: &FrameInput, now: Instant) {
        let palette = model.palette();
        if palette != self.palette {
            self.palette = palette;
            self.shades = palette.shades();
        }
        self.list.clear();
        let mut d = Draw {
            out: &mut self.list,
            p: &self.palette,
            c: &self.shades,
        };
        d.menu_and_monitors(model, hands);
        d.presses(model, hands, now);
        d.mouse(model);
        d.magnet(model, hands);
        d.hands(model, hands, input);
    }
}

/// The frame's list as it is filled, with the palette.
struct Draw<'a> {
    out: &'a mut Vec<Fx>,
    p: &'a Palette,
    c: &'a Shades,
}

impl Draw<'_> {
    /// `c` at opacity `a`, times the palette's strength.
    fn rgba(&self, c: [f32; 3], a: f32) -> [f32; 4] {
        [c[0], c[1], c[2], (a * self.p.strength).clamp(0.0, 1.0)]
    }

    /// A size of the palette, times its scale.
    fn size(&self, m: f32) -> f32 {
        m * self.p.scale
    }

    fn glow(&mut self, at: Vec3, m: f32, c: [f32; 3], a: f32) {
        let fx = Fx::glow(at, self.size(m), self.rgba(c, a));
        self.out.push(fx);
    }

    fn ring(&mut self, at: Vec3, m: f32, c: [f32; 3], a: f32) {
        let fx = Fx::ring(at, self.size(m), self.rgba(c, a));
        self.out.push(fx);
    }

    /// A fingertip near the menu or a monitor, and where the rays point.
    fn menu_and_monitors(&mut self, model: &Model, hands: &Hands) {
        let (p, c) = (*self.p, *self.c);
        // On the menu, where a fingertip is about to touch: wide and faint from a few
        // centimetres away, small and bright right before contact.
        let place = model.world().menu();
        let hover = model.feel().poke_hover_m;
        for (_, at, z, pressed) in hands.pokes() {
            let near = (1.0 - z.max(0.0) / hover).clamp(0.0, 1.0);
            let boost = if pressed { 0.25 } else { 0.0 };
            let a = p.hover_faint_alpha + (p.hover_alpha - p.hover_faint_alpha) * near * near;
            self.glow(
                place.position + place.rotation * Vec3::new(at.x, at.y, 0.003),
                p.hover_far_m - (p.hover_far_m - p.hover_near_m) * near,
                c.effect_accent,
                (a + boost).min(1.0),
            );
        }
        // The same on a monitor (step 8): a ring closing in as the finger comes near,
        // the touched point while pressed, and a ring closing in on a long press.
        let touch_hover = model.feel().touch_hover_m;
        for h in 0..2 {
            let Some(n) = hands.near_monitor(h) else {
                continue;
            };
            let at = n.point + n.normal * 0.003;
            if n.pressed {
                self.ring(at, p.press_ring_m, c.pressed, p.press_alpha);
                self.glow(at, p.press_glow_m, c.effect_accent, 0.6);
                if n.held > 0.25 {
                    let sizes = (p.touch_hold_from_m, p.touch_hold_to_m);
                    self.hold(at + n.normal * 0.001, n.held, sizes);
                }
            } else {
                let k = (n.z.max(0.0) / touch_hover).clamp(0.0, 1.0);
                let r = p.near_ring_m + (p.near_ring_far_m - p.near_ring_m) * k;
                self.ring(at, r, c.effect_accent, 0.35 + 0.55 * (1.0 - k));
                self.glow(at, p.near_glow_m, c.effect_accent, 0.15 + 0.35 * (1.0 - k));
            }
        }
        // Where a ray points: a small light on the menu, a wider faint one on monitors.
        for pick in hands.picks().iter().flatten() {
            let (m, a) = match pick.thing {
                Thing::Menu | Thing::MenuButton | Thing::Keyboard => {
                    (p.point_menu_m, p.point_menu_alpha)
                }
                Thing::Display(_) | Thing::Adding { .. } => {
                    (p.point_monitor_m, p.point_monitor_alpha)
                }
                Thing::Attach(_) => (p.point_attach_m, p.point_attach_alpha),
                Thing::Wrist => (p.point_palm_m, p.point_menu_alpha),
            };
            self.glow(pick.hit.point, m, c.effect_accent, a);
        }
    }

    /// A ring closing in while a hold runs (`k`: how far, 0 to 1) from one size to
    /// another, done when it closed.
    fn hold(&mut self, at: Vec3, k: f32, (from, to): (f32, f32)) {
        let (p, c) = (*self.p, *self.c);
        let k = k.clamp(0.0, 1.0);
        let color = if k >= 1.0 { c.done } else { c.waiting };
        self.ring(
            at,
            from - (from - to) * k,
            color,
            p.hold_faint_alpha + (p.hold_alpha - p.hold_faint_alpha) * k,
        );
    }

    /// Presses, holds, taps and takes.
    fn presses(&mut self, model: &Model, hands: &Hands, now: Instant) {
        let (p, c) = (*self.p, *self.c);
        for h in 0..2 {
            // A pinch held down: a ring under it, and one closing in towards taking.
            if let Some((at, k)) = hands.pressing(h) {
                self.ring(at, p.press_ring_m, c.pressed, p.press_alpha * 0.6);
                if let Some(k) = k {
                    self.hold(at, k, (p.hold_from_m, p.hold_to_m));
                }
            }
            // A press held towards taking or a right click (step 8).
            if let Some((at, k)) = hands.progress(h) {
                self.hold(at, k, (p.hold_from_m, p.hold_to_m));
            }
            // Where a hand holds what it carries.
            if let Some(at) = carried_at(model, hands, h) {
                self.ring(at, p.held_ring_m, c.effect_accent, p.held_alpha);
            }
        }
        // A tap, a click or a take spreads out as a wave for a moment.
        for (center, k, mark) in hands.marks(now) {
            let color = match mark {
                Mark::Touch => c.wave,
                Mark::Right | Mark::Take => c.done,
            };
            let r = p.wave_from_m + (p.wave_to_m - p.wave_from_m) * k;
            self.ring(center, r, color, p.wave_alpha * (1.0 - k));
        }
    }

    /// Where the PC's mouse is: rings closing in on it, three times.
    fn mouse(&mut self, model: &Model) {
        let (p, c) = (*self.p, *self.c);
        let world = model.world();
        let Some((id, uv, t)) = model.finder() else {
            return;
        };
        let Some(place) = world.shown(id) else {
            return;
        };
        let curve = world
            .curve_radius(model.settings())
            .filter(|_| !world.is_window(id));
        let (point, _) = layout::image_point(place, world.height_of(id), curve, uv);
        for k in 0..3 {
            let phase = (t * 3.0 - k as f32 * 0.33).rem_euclid(1.0);
            let r = p.mouse_dot_m + (p.mouse_ring_m - p.mouse_dot_m) * (1.0 - phase);
            let a = p.mouse_alpha * phase * (1.0 - t * 0.5);
            self.out.push(Fx::ring(point, r, rgba(c.mouse, a)));
        }
        let glow = Fx::glow(
            point,
            p.mouse_dot_m * 2.0,
            rgba(c.mouse, p.mouse_alpha * 0.9),
        );
        self.out.push(glow);
    }

    /// The magnet while dragging: the edge the monitor would stick to lights up.
    fn magnet(&mut self, model: &mut Model, hands: &Hands) {
        let (p, c) = (*self.p, *self.c);
        for h in 0..2 {
            let Some(id) = hands.carried(h).and_then(|(thing, _)| thing.display()) else {
                continue;
            };
            let Some(s) = model.magnet(id) else {
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
                let at = n.position + n.rotation * local;
                let glow = Fx::glow(at, p.guide_m, rgba(c.guide, p.guide_alpha));
                self.out.push(glow);
            }
        }
    }

    /// The hands, the rays and where a controller is. Hands are drawn over everything
    /// (the compositor has no depth for them), so near a menu they would cover its
    /// text: the hand holding the palm menu is not drawn, and a hand about to touch a
    /// menu is see-through. A hand pressing a monitor is drawn stopped on its surface.
    fn hands(&mut self, model: &Model, hands: &Hands, input: &FrameInput) {
        let (p, c) = (*self.p, *self.c);
        let world = model.world();
        let trial = model.trial();
        let menu_hand = usize::from(model.settings().left_handed);
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
            let color = [c.hand[0], c.hand[1], c.hand[2], trial.hand_alpha(near)];
            if trial.tips_only() {
                tip_balls(&j, color, self.out);
            } else {
                hand_balls(&j, color, self.out);
            }
        }
        for (h, hand) in input.hands.iter().enumerate() {
            if let Some((ray, length)) = hands.ray(h) {
                ray_balls(
                    ray.origin,
                    ray.dir,
                    length,
                    (p.ray_m, c.light, p.ray_alpha),
                    self.out,
                );
            }
            if let Some(ray) = hand.ray
                && !hand.is_hand
            {
                // Where the controller is, since it is not visible otherwise.
                let color = [c.controller[0], c.controller[1], c.controller[2], 1.0];
                self.out.push(Fx::ball(ray.origin, p.controller_m, color));
            }
        }
    }
}

/// `c` at opacity `a`, whatever the palette's strength.
fn rgba(c: [f32; 3], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a.clamp(0.0, 1.0)]
}

/// Where hand `h` holds what it carries, on its surface as drawn.
fn carried_at(model: &Model, hands: &Hands, h: usize) -> Option<Vec3> {
    let (thing, anchor) = hands.carried(h)?;
    let world = model.world();
    let (place, height, curve) = match thing {
        Thing::Display(id) => (
            *world.shown(id)?,
            world.height_of(id),
            world
                .curve_radius(model.settings())
                .filter(|_| !world.is_window(id)),
        ),
        Thing::Menu => {
            let m = world.menu();
            (m, 0.0, None)
        }
        Thing::Keyboard => (world.keyboard()?, 0.0, None),
        _ => return None,
    };
    let local = anchor * place.width;
    if height <= 0.0 {
        return Some(place.position + place.rotation * local.extend(0.004));
    }
    let uv = layout::image_uv(&place, height, curve, local);
    Some(layout::image_point(&place, height, curve, uv).0)
}

/// XR_EXT_hand_tracking joint chains: wrist to fingertip for each finger.
const FINGERS: [&[usize]; 5] = [
    &[1, 2, 3, 4, 5],
    &[1, 6, 7, 8, 9, 10],
    &[1, 11, 12, 13, 14, 15],
    &[1, 16, 17, 18, 19, 20],
    &[1, 21, 22, 23, 24, 25],
];
/// Across the palm, so it reads as a hand rather than five sticks.
const PALM: [&[usize]; 2] = [&[6, 11, 16, 21], &[7, 12, 17, 22]];

/// Balls along the bones of one tracked hand, of `color`.
pub fn hand_balls(joints: &Joints, color: [f32; 4], out: &mut Vec<Fx>) {
    let mut segment = |a: usize, b: usize, scale: f32| {
        let (pa, ra) = joints[a];
        let (pb, rb) = joints[b];
        let steps = ((pa.distance(pb) / 0.004).ceil() as usize).max(1);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            out.push(Fx::ball(
                pa.lerp(pb, t),
                (ra + (rb - ra) * t) * scale,
                color,
            ));
        }
    };
    for chain in FINGERS {
        for w in chain.windows(2) {
            segment(w[0], w[1], 0.95);
        }
    }
    for chain in PALM {
        for w in chain.windows(2) {
            segment(w[0], w[1], 1.3);
        }
    }
}

/// Only the fingertips of one tracked hand (the five tips, a little larger than their
/// joints).
pub fn tip_balls(joints: &Joints, color: [f32; 4], out: &mut Vec<Fx>) {
    for tip in [5, 10, 15, 20, 25] {
        let (center, radius) = joints[tip];
        out.push(Fx::ball(center, radius * 1.4, color));
    }
}

/// A dotted pointer ray from `origin` for `length` metres: dots of `radius`, of
/// `color`, fading from `alpha` to the end.
pub fn ray_balls(
    origin: Vec3,
    dir: Vec3,
    length: f32,
    (radius, color, alpha): (f32, [f32; 3], f32),
    out: &mut Vec<Fx>,
) {
    let steps = (length / 0.025) as usize;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        out.push(Fx::ball(
            origin + dir * (length * t),
            radius,
            [color[0], color[1], color[2], alpha * (1.0 - t * 0.7)],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand's balls, its tips and a ray fit the list made at the start, so filling
    /// it never takes memory.
    #[test]
    fn two_hands_and_rays_fit() {
        let joints: Joints =
            std::array::from_fn(|i| (Vec3::new(0.0, i as f32 * 0.008, -0.4), 0.008));
        let mut out = Vec::with_capacity(CAPACITY);
        for _ in 0..2 {
            hand_balls(&joints, [1.0; 4], &mut out);
            ray_balls(
                Vec3::ZERO,
                Vec3::NEG_Z,
                4.0,
                (0.002, [1.0; 3], 1.0),
                &mut out,
            );
        }
        assert!(out.len() < CAPACITY / 2, "{}", out.len());
        out.clear();
        tip_balls(&joints, [1.0; 4], &mut out);
        assert_eq!(out.len(), 5);
    }
}
