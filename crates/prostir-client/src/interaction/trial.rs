//! What the preview build lets the owner try in the headset before step 8 settles it
//! (plans/step-8-interaction.md, section 9, 8.0a and 8.0b), chosen in "For development",
//! kept in memory only: touching and clicking monitors and how a hand takes one, how
//! the hands are drawn, how strong the effects are. Every choice's default is the input
//! and the look as they were before step 8, but how a monitor moves: on the sphere
//! (step 8.3), the way before kept here to compare.

#![forbid(unsafe_code)]

use super::monitors::Take;

#[cfg(any(test, feature = "preview"))]
/// How the hands are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandLook {
    /// As before step 8: solid, see-through only near a menu.
    #[default]
    Solid,
    /// See-through all the time, this many percent opaque.
    Percent(u8),
    /// Only the fingertips.
    Tips,
}

#[cfg(any(test, feature = "preview"))]
impl HandLook {
    /// The choices, in the menu's order.
    pub const ALL: [HandLook; 5] = [
        HandLook::Solid,
        HandLook::Percent(50),
        HandLook::Percent(30),
        HandLook::Percent(20),
        HandLook::Tips,
    ];

    /// How opaque a hand is drawn (`near`: near a menu, where a solid hand would cover
    /// its text).
    pub fn alpha(self, near: bool) -> f32 {
        match self {
            HandLook::Solid if near => 0.3,
            HandLook::Solid | HandLook::Tips => 1.0,
            HandLook::Percent(p) => f32::from(p.min(100)) / 100.0,
        }
    }
}

#[cfg(any(test, feature = "preview"))]
/// How strong the lights, rings and rays are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FxSet {
    Thin,
    /// As before step 8.
    #[default]
    Standard,
    Bold,
}

#[cfg(any(test, feature = "preview"))]
impl FxSet {
    pub const ALL: [FxSet; 3] = [FxSet::Thin, FxSet::Standard, FxSet::Bold];

    /// How much larger than standard each light and ring is drawn.
    pub fn size(self) -> f32 {
        match self {
            FxSet::Thin => 0.7,
            FxSet::Standard => 1.0,
            FxSet::Bold => 1.4,
        }
    }

    /// How much more opaque.
    pub fn alpha(self) -> f32 {
        match self {
            FxSet::Thin => 0.7,
            FxSet::Standard => 1.0,
            FxSet::Bold => 1.25,
        }
    }
}

/// How a monitor in a hand or by a controller moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    /// As before step 8: along the ray, a hand's distance by its reach cubed.
    AsNow,
    /// On a sphere round the eye (step 8.3, see `layout::Grab::on_sphere`), a hand's
    /// distance by its reach to this power.
    Sphere(u8),
}

impl Default for Movement {
    /// On the sphere, the distance as much as the reach (plans/step-8-interaction.md,
    /// section 5; the power the owner tries in the headset).
    fn default() -> Self {
        Movement::Sphere(1)
    }
}

impl Movement {
    pub const ALL: [Movement; 4] = [
        Movement::AsNow,
        Movement::Sphere(1),
        Movement::Sphere(2),
        Movement::Sphere(3),
    ];
}

/// The choices of "Hands (trial)".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Trial {
    /// Monitors are touched and clicked (see `monitors`).
    pub monitors: bool,
    pub take: Take,
    pub movement: Movement,
    #[cfg(any(test, feature = "preview"))]
    pub look: HandLook,
    #[cfg(any(test, feature = "preview"))]
    pub fx: FxSet,
}

impl Trial {
    /// How much larger than standard each light and ring is drawn.
    pub fn fx_size(&self) -> f32 {
        #[cfg(any(test, feature = "preview"))]
        return self.fx.size();
        #[cfg(not(any(test, feature = "preview")))]
        1.0
    }

    /// How much more opaque.
    pub fn fx_alpha(&self) -> f32 {
        #[cfg(any(test, feature = "preview"))]
        return self.fx.alpha();
        #[cfg(not(any(test, feature = "preview")))]
        1.0
    }

    /// How opaque a hand is drawn (`near`: near a menu or a monitor it may touch).
    pub fn hand_alpha(&self, near: bool) -> f32 {
        #[cfg(any(test, feature = "preview"))]
        return self.look.alpha(near);
        #[cfg(not(any(test, feature = "preview")))]
        if near { 0.3 } else { 1.0 }
    }

    /// Only the fingertips of the hands are drawn.
    pub fn tips_only(&self) -> bool {
        #[cfg(any(test, feature = "preview"))]
        return self.look == HandLook::Tips;
        #[cfg(not(any(test, feature = "preview")))]
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the look before step 8.
    #[test]
    fn the_defaults_are_as_before() {
        let t = Trial::default();
        assert!(!t.monitors);
        assert_eq!(t.movement, Movement::Sphere(1));
        assert_eq!(t.look.alpha(false), 1.0);
        assert_eq!(t.look.alpha(true), 0.3);
        assert_eq!((t.fx.size(), t.fx.alpha()), (1.0, 1.0));
        assert_eq!(HandLook::Percent(20).alpha(true), 0.2);
    }
}
