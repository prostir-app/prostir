//! What a tracked hand does, worked out from its joints when the runtime does not say
//! (step 8, section 1): the index pinch and its ray from the shoulder (for a side whose
//! runtime still reports a controller while the cameras track the hand), the middle
//! pinch and the fist (the gestures that may take a monitor), the ray from the
//! shoulder through the palm. Each with hysteresis, by the numbers of [`Feel`].

#![forbid(unsafe_code)]

use glam::Vec3;

use super::{Feel, INDEX_TIP, Joints};
use crate::layout::Ray;

/// `XR_EXT_hand_tracking` joints used here.
pub const PALM: usize = 0;
pub const WRIST: usize = 1;
pub const THUMB_TIP: usize = 5;
pub const MIDDLE_PROXIMAL: usize = 12;
pub const MIDDLE_TIP: usize = 15;
const FINGER_TIPS: [usize; 4] = [10, 15, 20, 25];

/// A gesture held, with the frame it began and the frame it ended in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
}

impl Held {
    pub fn set(&mut self, down: bool) {
        self.pressed = down && !self.down;
        self.released = !down && self.down;
        self.down = down;
    }
}

/// About where the shoulder of hand `left` (or the right one) is, for a head at `head`
/// looking `forward`.
pub fn shoulder(head: Vec3, forward: Vec3, left: bool, feel: &Feel) -> Vec3 {
    let flat = flat(forward);
    let side = if left { -1.0 } else { 1.0 };
    head - Vec3::Y * feel.shoulder_down_m + flat.cross(Vec3::Y) * side * feel.shoulder_side_m
}

fn flat(forward: Vec3) -> Vec3 {
    Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z)
}

/// The index pinch from the joints (`was`: closed in the last frame): closed, and the
/// ray from the shoulder through the point between thumb and index while the hand is
/// ready to pinch (`None` otherwise), as the system's own.
pub fn index_pinch(
    j: &Joints,
    was: bool,
    head: Vec3,
    forward: Vec3,
    left: bool,
    feel: &Feel,
) -> (bool, Option<Ray>) {
    let (thumb, index) = (j[THUMB_TIP].0, j[INDEX_TIP].0);
    let gap = thumb.distance(index);
    let closed = if was {
        gap < feel.pinch_off_m
    } else {
        gap < feel.pinch_on_m
    };
    // Only a hand getting ready to pinch points, as with the runtime's input.
    if gap > feel.pinch_ready_m && !closed {
        return (closed, None);
    }
    let origin = (thumb + index) * 0.5;
    let ray = Ray {
        origin,
        dir: (origin - shoulder(head, forward, left, feel)).normalize_or(flat(forward)),
    };
    (closed, Some(ray))
}

/// From about the shoulder through the palm: where a fist or a middle pinch points (the
/// runtime gives no ray then).
pub fn palm_ray(j: &Joints, head: Vec3, forward: Vec3, left: bool, feel: &Feel) -> Ray {
    let palm = j[PALM].0;
    Ray {
        origin: palm,
        dir: (palm - shoulder(head, forward, left, feel)).normalize_or(flat(forward)),
    }
}

/// The middle pinch and the fist of one hand, from its joints, each with hysteresis.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Gestures {
    pub middle: Held,
    pub fist: Held,
}

impl Gestures {
    /// This frame's joints (`None`: not tracked, both end).
    pub fn update(&mut self, joints: Option<&Joints>, feel: &Feel) {
        let Some(j) = joints else {
            self.middle.set(false);
            self.fist.set(false);
            return;
        };
        let thumb = j[THUMB_TIP].0;
        let index_gap = thumb.distance(j[INDEX_TIP].0);
        let middle_gap = thumb.distance(j[MIDDLE_TIP].0);
        let middle = if self.middle.down {
            middle_gap < feel.middle_off_m
        } else {
            middle_gap < feel.middle_on_m && index_gap > feel.middle_index_apart_m
        };
        self.middle.set(middle);
        // Relative to the palm's length, so a small and a large hand count the same.
        let palm = j[PALM].0;
        let length = j[WRIST].0.distance(j[MIDDLE_PROXIMAL].0).max(0.03);
        let ratio = |t: usize| j[t].0.distance(palm) / length;
        let curled = FINGER_TIPS
            .iter()
            .filter(|&&t| ratio(t) < feel.fist_on)
            .count();
        let open = FINGER_TIPS
            .iter()
            .filter(|&&t| ratio(t) > feel.fist_off)
            .count();
        // The index among the curled ones: a hand pointing to touch (three fingers
        // curled, the index out) is not a fist.
        let index_curled = ratio(INDEX_TIP) < feel.fist_on;
        let fist = if self.fist.down {
            open < 2
        } else {
            curled >= 3 && index_curled
        };
        self.fist.set(fist);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hand(palm: Vec3, tips_from_palm: f32, thumb_index: f32, thumb_middle: f32) -> Joints {
        let mut j = [(palm, 0.01); 26];
        j[WRIST].0 = palm + Vec3::new(0.0, -0.05, 0.03);
        j[MIDDLE_PROXIMAL].0 = palm + Vec3::new(0.0, 0.04, -0.01);
        for t in FINGER_TIPS {
            j[t].0 = palm + Vec3::new(0.0, tips_from_palm, 0.0);
        }
        j[THUMB_TIP].0 = j[INDEX_TIP].0 + Vec3::new(thumb_index, 0.0, 0.0);
        j[MIDDLE_TIP].0 = j[THUMB_TIP].0 + Vec3::new(0.0, 0.0, thumb_middle);
        j
    }

    #[test]
    fn a_fist_and_an_open_hand() {
        let feel = Feel::DEFAULT;
        let mut g = Gestures::default();
        let palm = Vec3::new(0.2, 1.2, -0.4);
        g.update(Some(&hand(palm, 0.12, 0.05, 0.05)), &feel);
        assert!(!g.fist.down);
        g.update(Some(&hand(palm, 0.03, 0.05, 0.05)), &feel);
        assert!(g.fist.pressed && g.fist.down);
        // Half open: still a fist (hysteresis).
        g.update(Some(&hand(palm, 0.07, 0.05, 0.05)), &feel);
        assert!(g.fist.down);
        g.update(Some(&hand(palm, 0.12, 0.05, 0.05)), &feel);
        assert!(g.fist.released && !g.fist.down);
        g.update(Some(&hand(palm, 0.03, 0.05, 0.05)), &feel);
        g.update(None, &feel);
        assert!(g.fist.released);
    }

    /// Pointing (the index out, the rest curled) is not a fist.
    #[test]
    fn pointing_is_not_a_fist() {
        let feel = Feel::DEFAULT;
        let mut g = Gestures::default();
        let palm = Vec3::new(0.2, 1.2, -0.4);
        let mut j = hand(palm, 0.03, 0.05, 0.05);
        j[INDEX_TIP].0 = palm + Vec3::new(0.0, 0.12, 0.0);
        g.update(Some(&j), &feel);
        assert!(!g.fist.down);
    }

    #[test]
    fn a_middle_pinch_is_not_an_index_pinch() {
        let feel = Feel::DEFAULT;
        let mut g = Gestures::default();
        let palm = Vec3::new(0.2, 1.2, -0.4);
        // Thumb on the index (and so near the middle too): no middle pinch.
        g.update(Some(&hand(palm, 0.12, 0.005, 0.01)), &feel);
        assert!(!g.middle.down);
        g.update(Some(&hand(palm, 0.12, 0.05, 0.01)), &feel);
        assert!(g.middle.pressed);
        g.update(Some(&hand(palm, 0.12, 0.05, 0.05)), &feel);
        assert!(g.middle.released);
    }

    /// The index pinch closes near, opens far, and points only when ready.
    #[test]
    fn an_index_pinch_from_the_joints() {
        let feel = Feel::DEFAULT;
        let head = Vec3::new(0.0, 1.6, 0.0);
        let palm = Vec3::new(0.2, 1.3, -0.4);
        let gap = |g: f32| hand(palm, 0.12, g, 0.05);
        let (closed, ray) = index_pinch(&gap(0.01), false, head, -Vec3::Z, false, &feel);
        assert!(closed && ray.is_some());
        let (closed, _) = index_pinch(&gap(0.03), true, head, -Vec3::Z, false, &feel);
        assert!(closed, "held open less than its release");
        let (closed, ray) = index_pinch(&gap(0.03), false, head, -Vec3::Z, false, &feel);
        assert!(!closed && ray.is_some(), "ready, not closed");
        let (closed, ray) = index_pinch(&gap(0.08), true, head, -Vec3::Z, false, &feel);
        assert!(!closed && ray.is_none(), "not ready: no ray");
        // From the right shoulder: the ray goes forward, a little up and left of it.
        let (_, ray) = index_pinch(&gap(0.01), false, head, -Vec3::Z, false, &feel);
        let ray = ray.expect("ray");
        assert!(ray.dir.z < -0.5, "{:?}", ray.dir);
    }
}
