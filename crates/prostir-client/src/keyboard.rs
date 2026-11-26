//! The person's own keyboard, as the headset tracks it (step 8.7): where it lies, and
//! the window onto the room that settles over it, so the person types without taking
//! the headset off. The shell reads the keyboard's pose from the runtime
//! (`XR_FB_keyboard_tracking`, read by the platform's part) and gives it as [`Seen`];
//! [`Follow`] eases the window towards it, and keeps it where it was when the keyboard
//! goes out of view. Without a tracked keyboard the window is the one placed by hand.
//!
//! Nothing here allocates, and the file of the hand-placed window is never written for
//! it: the tracked place is only for this run.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use glam::{Quat, Vec3};

use crate::layout::Placement;

/// The window reaches this far past the keyboard on every side, so the hands show
/// around it.
pub const MARGIN: f32 = 0.03;
/// Out of view longer than this, the window stays where the keyboard was.
pub const LOST_AFTER: Duration = Duration::from_secs(2);
/// A keyboard farther than this from where the window was is not eased to: the window
/// goes there at once (a new keyboard, or the room moved).
const JUMP: f32 = 0.4;
/// How fast the window eases towards the keyboard, per second.
const EASE: f32 = 14.0;
/// Sides of a keyboard the window takes: a runtime's bad size is not drawn (metres).
const SIDE_MIN: f32 = 0.1;
const SIDE_MAX: f32 = 1.2;

/// A tracked keyboard, as the runtime places it: the middle of its top face, the
/// rotation with +Y up from the keys, and the sides along X (width) and Z (depth).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub position: Vec3,
    pub rotation: Quat,
    pub width: f32,
    pub depth: f32,
}

/// The window onto the room over a keyboard: where it lies, and how deep it is (its
/// width is the placement's).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub place: Placement,
    pub depth: f32,
}

impl Seen {
    /// Whether the numbers can be drawn: finite, a unit rotation, sides a keyboard may
    /// have.
    pub fn sane(&self) -> bool {
        let side = |s: f32| (SIDE_MIN..=SIDE_MAX).contains(&s);
        self.position.is_finite()
            && self.rotation.is_finite()
            && (self.rotation.length() - 1.0).abs() < 0.01
            && side(self.width)
            && side(self.depth)
    }

    /// The window over this keyboard: lying in the keyboard's face (the window's plane
    /// is its X and Y, the keyboard's face is X and Z), with the margin on each side.
    pub fn window(&self) -> Window {
        let flat = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        Window {
            place: Placement {
                position: self.position,
                rotation: self.rotation * flat,
                width: self.width + 2.0 * MARGIN,
            },
            depth: self.depth + 2.0 * MARGIN,
        }
    }
}

/// The window following the keyboard.
#[derive(Debug, Default)]
pub struct Follow {
    /// Where the window is now (`None`: the keyboard was not seen yet).
    window: Option<Window>,
    /// The last time the keyboard was in view.
    seen_at: Option<Instant>,
}

impl Follow {
    /// A frame `dt` seconds after the last, at `now`, with the keyboard `seen` (`None`:
    /// not in view). Where the window goes, once the keyboard has been seen: it stays
    /// there after the keyboard is gone, which is what [`Follow::lost`] says.
    pub fn step(&mut self, now: Instant, dt: f32, seen: Option<Seen>) -> Option<Window> {
        let Some(goal) = seen.filter(Seen::sane).map(|s| s.window()) else {
            return self.window;
        };
        self.seen_at = Some(now);
        self.window = Some(match self.window {
            Some(w) if w.place.position.distance(goal.place.position) <= JUMP => {
                let t = 1.0 - (-EASE * dt.max(0.0)).exp();
                Window {
                    place: Placement {
                        position: w.place.position.lerp(goal.place.position, t),
                        rotation: w.place.rotation.slerp(goal.place.rotation, t),
                        width: w.place.width + (goal.place.width - w.place.width) * t,
                    },
                    depth: w.depth + (goal.depth - w.depth) * t,
                }
            }
            _ => goal,
        });
        self.window
    }

    /// The keyboard has not been in view for [`LOST_AFTER`], though it was seen before:
    /// the window is where it was.
    pub fn lost(&self, now: Instant) -> bool {
        self.seen_at
            .is_some_and(|at| now.saturating_duration_since(at) > LOST_AFTER)
    }

    /// Whether the keyboard is in view now (seen in the last frame or within
    /// [`LOST_AFTER`]).
    pub fn tracking(&self, now: Instant) -> bool {
        self.window.is_some() && !self.lost(now)
    }

    /// Forgets where the keyboard was (the follow turned off, or the window hidden): the
    /// next sight starts afresh.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;

    fn seen(x: f32) -> Seen {
        Seen {
            position: Vec3::new(x, 0.7, -0.4),
            rotation: Quat::from_rotation_y(0.3),
            width: 0.44,
            depth: 0.14,
        }
    }

    /// The window lies in the keyboard's face, with the margin, and faces up from it.
    #[test]
    fn window_lies_on_the_keyboard() {
        let w = seen(0.0).window();
        assert!((w.place.width - (0.44 + 2.0 * MARGIN)).abs() < 1e-6);
        assert!((w.depth - (0.14 + 2.0 * MARGIN)).abs() < 1e-6);
        let up = Quat::from_rotation_y(0.3) * Vec3::Y;
        // The window's normal (+Z) is the keyboard's up; its X is the keyboard's X.
        assert!((w.place.rotation * Vec3::Z).distance(up) < 1e-5);
        let x = Quat::from_rotation_y(0.3) * Vec3::X;
        assert!((w.place.rotation * Vec3::X).distance(x) < 1e-5);
        // Its Y runs along the keyboard's depth.
        let back = Quat::from_rotation_y(0.3) * -Vec3::Z;
        assert!((w.place.rotation * Vec3::Y).distance(back) < 1e-5);
    }

    /// A pose a runtime may give wrongly is not drawn.
    #[test]
    fn broken_poses_are_not_drawn() {
        assert!(seen(0.0).sane());
        let mut s = seen(0.0);
        s.position.x = f32::NAN;
        assert!(!s.sane());
        let mut s = seen(0.0);
        s.width = 0.0;
        assert!(!s.sane());
        let mut s = seen(0.0);
        s.depth = 9.0;
        assert!(!s.sane());
        let mut s = seen(0.0);
        s.rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 2.0);
        assert!(!s.sane());
        let mut f = Follow::default();
        assert_eq!(
            f.step(Instant::now(), 0.011, Some(s)),
            None,
            "nothing to show"
        );
    }

    /// The first sight puts the window there; later ones ease, a far one jumps.
    #[test]
    fn eases_and_jumps() {
        let t0 = Instant::now();
        let mut f = Follow::default();
        assert_eq!(f.step(t0, 0.011, None), None);
        let first = f.step(t0, 0.011, Some(seen(0.0))).unwrap();
        assert_eq!(first, seen(0.0).window());
        let near = f.step(t0, 0.011, Some(seen(0.1))).unwrap();
        assert!(near.place.position.x > 0.0 && near.place.position.x < 0.1);
        // Given time, it arrives.
        let mut w = near;
        for _ in 0..200 {
            w = f.step(t0, 0.011, Some(seen(0.1))).unwrap();
        }
        assert!((w.place.position.x - 0.1).abs() < 1e-3);
        // A keyboard 2 m away is not crossed to: the window is there at once.
        let far = f.step(t0, 0.011, Some(seen(2.0))).unwrap();
        assert_eq!(far, seen(2.0).window());
    }

    /// Out of view the window stays; after two seconds it is said lost, and a new sight
    /// is followed again.
    #[test]
    fn stays_where_the_keyboard_was() {
        let t0 = Instant::now();
        let mut f = Follow::default();
        assert!(!f.lost(t0) && !f.tracking(t0));
        let at = f.step(t0, 0.011, Some(seen(0.0))).unwrap();
        assert!(f.tracking(t0));
        let later = t0 + Duration::from_secs(1);
        assert_eq!(f.step(later, 0.011, None), Some(at));
        assert!(f.tracking(later));
        let long = t0 + Duration::from_secs(3);
        assert_eq!(f.step(long, 0.011, None), Some(at), "stays");
        assert!(f.lost(long) && !f.tracking(long));
        assert!(f.step(long, 0.011, Some(seen(0.0))).is_some());
        assert!(f.tracking(long));
        f.reset();
        assert_eq!(f.step(long, 0.011, None), None);
    }
}
