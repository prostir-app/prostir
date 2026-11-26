//! The colours, the opacity and the sizes of what the headset draws itself over the
//! monitors (step 8, section 6): the lights of a fingertip or a ray, the rings of a
//! press, a hold and a take, the wave of a tap, the hands and the rays. One
//! [`Palette`], plain data: colours as sRGB, as a theme file writes them, made linear
//! once when the palette changes ([`Shades`]); sizes in metres. The menu takes its
//! colours from here too once themes come (step 9), so a theme changes all at once.
//!
//! The defaults are neutral: a pale blue light, white for a press, amber while a hold
//! runs, green when it is done, grey hands; the look as it was before step 8.

#![forbid(unsafe_code)]

/// An sRGB colour, 0 to 255 each, as a theme writes it (`#ddecff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Srgb(pub [u8; 3]);

impl Srgb {
    /// The colour in linear light, as the shader blends it.
    pub fn linear(self) -> [f32; 3] {
        self.0.map(|c| {
            let v = f32::from(c) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    }
}

/// What is drawn over the monitors: its colours, how opaque, how large (see the
/// module). Each `_alpha` is the most an effect is opaque (0 to 1), each `_m` a size
/// in metres before [`Palette::scale`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    // ---- Colours ----
    /// A fingertip or a ray about to act: the hover's light, a touch, where a ray
    /// points.
    pub effect_accent: Srgb,
    /// The ray itself.
    pub light: Srgb,
    /// The wave of a tap or a click.
    pub wave: Srgb,
    /// The ring of a press held down.
    pub pressed: Srgb,
    /// A ring closing in while a hold runs.
    pub waiting: Srgb,
    /// The hold done, a right click, a take.
    pub done: Srgb,
    /// Where the PC's mouse is ("Show the mouse").
    pub mouse: Srgb,
    /// The edge a monitor carried would stick to.
    pub guide: Srgb,
    /// The hands.
    pub hand: Srgb,
    /// Where a controller is.
    pub controller: Srgb,

    // ---- Everything at once ----
    /// Every effect this much larger (a theme's or a trial's setting).
    pub scale: f32,
    /// And this much more opaque.
    pub strength: f32,

    // ---- A fingertip near the menu, or a monitor ----
    /// The hover's light from the furthest a fingertip hovers...
    pub hover_far_m: f32,
    /// ...to the touch.
    pub hover_near_m: f32,
    /// How faint it starts, and how bright it gets.
    pub hover_faint_alpha: f32,
    pub hover_alpha: f32,
    /// Before a monitor: a ring closing in from this...
    pub near_ring_far_m: f32,
    /// ...to this at the touch.
    pub near_ring_m: f32,
    /// And the light around it.
    pub near_glow_m: f32,

    // ---- A press, a hold, a tap, a take ----
    /// The ring of a press held down.
    pub press_ring_m: f32,
    pub press_alpha: f32,
    /// The light around a fingertip pressed on a monitor.
    pub press_glow_m: f32,
    /// A fingertip held on a monitor towards a long press: a ring closing in from
    /// this...
    pub touch_hold_from_m: f32,
    /// ...to this.
    pub touch_hold_to_m: f32,
    /// A ring closing in while a hold runs: from this...
    pub hold_from_m: f32,
    /// ...to this, when it is done.
    pub hold_to_m: f32,
    /// How faint it starts, and how bright it gets.
    pub hold_faint_alpha: f32,
    pub hold_alpha: f32,
    /// A tap's wave spreads out from this...
    pub wave_from_m: f32,
    /// ...to this, fading.
    pub wave_to_m: f32,
    pub wave_alpha: f32,
    /// Where a hand holds what it carries: a small ring there.
    pub held_ring_m: f32,
    pub held_alpha: f32,

    // ---- Where a ray points ----
    /// On the menu, its button or the keyboard window.
    pub point_menu_m: f32,
    pub point_menu_alpha: f32,
    /// On a monitor, wider and fainter.
    pub point_monitor_m: f32,
    pub point_monitor_alpha: f32,
    /// On "Attach" under a window.
    pub point_attach_m: f32,
    pub point_attach_alpha: f32,
    /// On the palm menu.
    pub point_palm_m: f32,

    // ---- The rest (as large and opaque whatever the scale and strength) ----
    /// The rings closing in on the PC's mouse: from this...
    pub mouse_ring_m: f32,
    /// ...to this, and the light on it.
    pub mouse_dot_m: f32,
    pub mouse_alpha: f32,
    /// The lights along the edge a carried monitor would stick to.
    pub guide_m: f32,
    pub guide_alpha: f32,
    /// The dots of a ray.
    pub ray_m: f32,
    pub ray_alpha: f32,
    /// Where a controller is.
    pub controller_m: f32,
    /// A hand drawn as the runtime models it: see-through glass, this opaque.
    pub hand_mesh_alpha: f32,
}

impl Palette {
    /// The neutral palette (see the module).
    pub const DEFAULT: Palette = Palette {
        effect_accent: Srgb([221, 236, 255]),
        light: Srgb([237, 246, 255]),
        wave: Srgb([231, 243, 255]),
        pressed: Srgb([255, 255, 255]),
        waiting: Srgb([255, 234, 170]),
        done: Srgb([203, 255, 225]),
        mouse: Srgb([255, 237, 160]),
        guide: Srgb([196, 225, 255]),
        hand: Srgb([206, 212, 223]),
        controller: Srgb([149, 153, 166]),
        scale: 1.0,
        strength: 1.0,
        hover_far_m: 0.034,
        hover_near_m: 0.014,
        hover_faint_alpha: 0.12,
        hover_alpha: 0.72,
        near_ring_far_m: 0.037,
        near_ring_m: 0.007,
        near_glow_m: 0.02,
        press_ring_m: 0.012,
        press_alpha: 0.95,
        press_glow_m: 0.018,
        touch_hold_from_m: 0.04,
        touch_hold_to_m: 0.014,
        hold_from_m: 0.05,
        hold_to_m: 0.015,
        hold_faint_alpha: 0.4,
        hold_alpha: 0.9,
        wave_from_m: 0.012,
        wave_to_m: 0.042,
        wave_alpha: 0.9,
        held_ring_m: 0.016,
        held_alpha: 0.7,
        point_menu_m: 0.012,
        point_menu_alpha: 0.55,
        point_monitor_m: 0.03,
        point_monitor_alpha: 0.3,
        point_attach_m: 0.01,
        point_attach_alpha: 0.5,
        point_palm_m: 0.008,
        mouse_ring_m: 0.1,
        mouse_dot_m: 0.01,
        mouse_alpha: 0.9,
        guide_m: 0.03,
        guide_alpha: 0.55,
        ray_m: 0.0018,
        ray_alpha: 0.85,
        controller_m: 0.012,
        hand_mesh_alpha: 0.3,
    };

    /// This palette with every effect `scale` times as large and `strength` times as
    /// opaque.
    pub fn scaled(mut self, scale: f32, strength: f32) -> Self {
        self.scale *= scale;
        self.strength *= strength;
        self
    }

    /// The colours in linear light (made once, when the palette changes).
    pub fn shades(&self) -> Shades {
        Shades {
            effect_accent: self.effect_accent.linear(),
            light: self.light.linear(),
            wave: self.wave.linear(),
            pressed: self.pressed.linear(),
            waiting: self.waiting.linear(),
            done: self.done.linear(),
            mouse: self.mouse.linear(),
            guide: self.guide.linear(),
            hand: self.hand.linear(),
            controller: self.controller.linear(),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A palette's colours in linear light (see [`Palette::shades`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Shades {
    pub effect_accent: [f32; 3],
    pub light: [f32; 3],
    pub wave: [f32; 3],
    pub pressed: [f32; 3],
    pub waiting: [f32; 3],
    pub done: [f32; 3],
    pub mouse: [f32; 3],
    pub guide: [f32; 3],
    pub hand: [f32; 3],
    pub controller: [f32; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the colours drawn before step 8, which were written in linear
    /// light: within the step of an 8-bit sRGB value.
    #[test]
    fn the_defaults_are_the_look_before() {
        let s = Palette::DEFAULT.shades();
        let before: [([f32; 3], [f32; 3]); 9] = [
            (s.effect_accent, [0.72, 0.84, 1.0]),
            (s.light, [0.85, 0.92, 1.0]),
            (s.wave, [0.8, 0.9, 1.0]),
            (s.pressed, [1.0, 1.0, 1.0]),
            (s.waiting, [1.0, 0.82, 0.4]),
            (s.done, [0.6, 1.0, 0.75]),
            (s.mouse, [1.0, 0.85, 0.35]),
            (s.guide, [0.55, 0.75, 1.0]),
            (s.hand, [0.62, 0.66, 0.74]),
        ];
        for (i, (now, was)) in before.iter().enumerate() {
            for c in 0..3 {
                assert!((now[c] - was[c]).abs() < 0.006, "{i}: {now:?} {was:?}");
            }
        }
        assert!((s.controller[0] - 0.30).abs() < 0.006);
    }

    /// Black and white stay so, and the scale and strength multiply.
    #[test]
    fn linear_light_and_scaling() {
        assert_eq!(Srgb([0, 0, 0]).linear(), [0.0; 3]);
        assert_eq!(Srgb([255, 255, 255]).linear(), [1.0; 3]);
        let p = Palette::DEFAULT.scaled(1.4, 0.7).scaled(0.5, 2.0);
        assert!((p.scale - 0.7).abs() < 1e-6);
        assert!((p.strength - 1.4).abs() < 1e-6);
    }
}
