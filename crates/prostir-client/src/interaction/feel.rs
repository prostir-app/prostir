//! Every number of how the hands and controllers feel, in one place (step 8, section
//! 1): thresholds, delays, distances, speeds. Each has its unit in its name (`_m`
//! metres, `_ms` milliseconds, `_deg` degrees, `_per_s` a second; none: a fraction or
//! a ratio) and the least and the most it may be. The defaults are the numbers the
//! input had before step 8 (and, for what step 8 adds, the prototype's).
//!
//! In the preview build `feel.txt` in the app's external folder overrides any of them
//! (`key = value`, one a line, `#` starts a comment), read at the start and again from
//! "For development"; no other build reads it. A key it does not know, a value that is
//! not a number or is out of bounds is said once in the log and the default stays.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::time::Duration;

/// A number of milliseconds as a duration (none below zero).
pub fn ms(v: f32) -> Duration {
    Duration::from_micros((v.max(0.0) * 1000.0).round() as u64)
}

macro_rules! feel {
    ($( $(#[doc = $doc:expr])* $name:ident: $default:expr, $min:expr, $max:expr; )*) => {
        /// The numbers (see the module).
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Feel {
            $( $(#[doc = $doc])* pub $name: f32, )*
        }

        impl Feel {
            /// As the input had them before step 8.
            pub const DEFAULT: Feel = Feel { $( $name: $default, )* };

            /// Each number's key in `feel.txt`, the least and the most it may be.
            pub const FIELDS: &'static [(&'static str, f32, f32)] =
                &[ $( (stringify!($name), $min, $max), )* ];

            fn slot(&mut self, key: &str) -> Option<&mut f32> {
                match key {
                    $( stringify!($name) => Some(&mut self.$name), )*
                    _ => None,
                }
            }

            /// The value of the `i`th of [`Feel::FIELDS`].
            fn value(&self, i: usize) -> f32 {
                [ $( self.$name, )* ][i]
            }
        }
    };
}

feel! {
    // ---- Buttons and pinches ----

    /// A trigger, grip or pinch strength counts as pressed above this (0 to 1).
    press_on: 0.75, 0.05, 1.0;
    /// And as let go below this, so a half pulled trigger does not flicker.
    press_off: 0.4, 0.0, 0.95;
    /// A pinch worked out from the joints closes when thumb and index tips come this
    /// near.
    pinch_on_m: 0.015, 0.002, 0.06;
    /// And opens when they are this far apart.
    pinch_off_m: 0.035, 0.005, 0.1;
    /// A hand worked out from the joints points (is "ready to pinch") only with thumb
    /// and index tips nearer than this.
    pinch_ready_m: 0.05, 0.01, 0.2;
    /// The shoulder a hand's ray starts from, below the head.
    shoulder_down_m: 0.18, 0.0, 0.5;
    /// And to its side.
    shoulder_side_m: 0.17, 0.0, 0.5;

    // ---- Holding, taking, carrying ----

    /// A pinch held this long on something that can be taken takes it.
    hold_to_take_ms: 300.0, 50.0, 3000.0;
    /// Taken nearer than this from the hand, a thing moves with the hand one to one.
    near_take_m: 0.35, 0.0, 2.0;
    /// A hand's reach moves what it holds by the reach to this power.
    reach_power: 3.0, 0.5, 6.0;
    /// Moving on a sphere: reaching in or out less than this does not change the
    /// distance.
    reach_dead_m: 0.02, 0.0, 0.2;
    /// And a hand's move counts as reaching in or out only while it goes this many
    /// times faster towards or away from the shoulder than across.
    reach_dominance: 1.5, 1.0, 5.0;
    /// What a hand holds is never nearer than this.
    distance_min_m: 0.3, 0.1, 2.0;
    /// Nor further than this.
    distance_max_m: 6.0, 1.0, 50.0;
    /// A monitor is made no narrower than this (the stick, two hands).
    width_min_m: 0.2, 0.05, 2.0;
    /// Nor wider than this.
    width_max_m: 8.0, 1.0, 50.0;
    /// A controller's stick does nothing within this of its middle.
    stick_dead: 0.15, 0.0, 0.9;
    /// The stick fully over moves what is held nearer or further by this share of its
    /// distance a second.
    stick_distance_per_s: 1.5, 0.1, 10.0;
    /// And resizes it by this share a second.
    stick_width_per_s: 1.0, 0.1, 10.0;
    /// Two hands resizing one monitor count as no nearer than this.
    two_hands_min_m: 0.05, 0.01, 0.5;

    // ---- Where a monitor may stand (step 8.3) ----

    /// A monitor's nearest point is never nearer the eye than this...
    bound_near_m: 0.35, 0.1, 2.0;
    /// ...and its middle never further than this.
    bound_far_m: 6.0, 1.0, 50.0;
    /// Nor wider than this angle as seen from the eye.
    bound_angle_deg: 140.0, 30.0, 175.0;
    /// Its middle never lower than this below eye level (where the floor is not
    /// known).
    bound_below_deg: 70.0, 10.0, 89.0;
    /// Its lower edge at least this above the floor, where the floor is known.
    bound_floor_m: 0.05, 0.0, 1.0;
    /// A monitor carried slows down this near a bound, and stops at it.
    bound_soft_m: 0.05, 0.0, 0.5;

    // ---- Touching the menu ----

    /// A fingertip nearer than this in front of the menu hovers it.
    poke_hover_m: 0.06, 0.01, 0.3;
    /// Nearer than this, it presses.
    poke_press_m: 0.008, -0.02, 0.05;
    /// A press ends once the finger is this far in front again.
    poke_release_m: 0.025, 0.0, 0.1;
    /// Further behind the menu than this, a finger is not touching it (reaching past).
    poke_behind_m: 0.06, 0.0, 0.3;
    /// A finger bouncing off the menu this soon after a touch is not a second touch.
    touch_settles_ms: 400.0, 0.0, 2000.0;
    /// Pushing deeper than this behind the menu while dragging it moves it away.
    menu_push_m: 0.03, 0.0, 0.2;
    /// Pinches of a hand that just touched something are ignored this long (pulling
    /// the hand back often closes thumb and index).
    after_touch_ms: 600.0, 0.0, 3000.0;
    /// A touch spreads out as a ring this long.
    touch_ring_ms: 350.0, 50.0, 3000.0;

    // ---- The palm menu ----

    /// An open palm faces the face this long before its menu shows.
    palm_dwell_ms: 250.0, 0.0, 3000.0;
    /// How straight the palm must face the face for its menu to show (the cosine).
    palm_face_on: 0.8, 0.0, 1.0;
    /// And how straight to keep showing.
    palm_face_off: 0.55, 0.0, 1.0;
    /// The middle fingertip at least this far from the palm: the hand is open.
    palm_open_m: 0.07, 0.0, 0.2;
    /// The other fingertip nearer than this in front of the palm menu hovers it.
    wrist_hover_m: 0.04, 0.0, 0.2;
    /// Nearer than this, it presses.
    wrist_press_m: 0.008, -0.02, 0.05;
    /// Further behind it than this, it is not touching.
    wrist_behind_m: 0.03, 0.0, 0.2;
    /// Further in front than this, the next touch can press again.
    wrist_rearm_m: 0.02, 0.0, 0.2;

    // ---- What a ray points at ----

    /// "Attach" under a detached window stays this long after the ray left it.
    attach_linger_ms: 1200.0, 0.0, 10000.0;
    /// The mouse goes where the user looks after the gaze rests this long (the PC is
    /// told it with the settings it acts on).
    gaze_dwell_ms: 600.0, 100.0, 5000.0;

    // ---- Step 8: touching a monitor like a touchscreen ----

    /// A fingertip nearer than this to a surface it can touch: its ray goes and touch
    /// acts.
    switch_touch_m: 0.15, 0.02, 0.5;
    /// Further than this, the ray comes back.
    switch_ray_m: 0.20, 0.02, 0.6;
    /// A fingertip nearer than this in front of a monitor hovers it.
    touch_hover_m: 0.06, 0.01, 0.3;
    /// The fingertip presses once this deep past the monitor's surface.
    touch_depth_m: 0.0, 0.0, 0.03;
    /// A press starts only from in front: crossing the surface no deeper than this.
    touch_enter_m: 0.025, 0.005, 0.1;
    /// Pushed this far through a monitor, a touch is cancelled.
    touch_through_m: 0.06, 0.01, 0.3;
    /// The touched point stays where it touched until the finger slides this far
    /// along the surface.
    touch_pin_m: 0.015, 0.0, 0.05;
    /// A touch ends once the finger came back this far from its deepest point.
    touch_recoil_m: 0.012, 0.002, 0.05;
    /// Or once it is this far in front of the surface.
    touch_release_m: 0.02, 0.0, 0.1;
    /// A finger bouncing off a monitor this soon after a touch is not a second touch.
    touch_settle_ms: 120.0, 0.0, 2000.0;
    /// A touch held still this long is a long press (the PC's right click).
    long_press_ms: 600.0, 100.0, 3000.0;
    /// A hand's tracking lost this long ends what it was doing, as letting go would.
    lost_grace_ms: 150.0, 0.0, 2000.0;

    // ---- Step 8: a far ray as the mouse ----

    /// From a press the pointer stays where it pressed this long...
    freeze_ms: 300.0, 0.0, 2000.0;
    /// ...or until the ray turned this far.
    freeze_deg: 1.5, 0.0, 10.0;
    /// A ray turned less than this since the last move sends no move (the real mouse
    /// wins).
    still_deg: 0.1, 0.0, 2.0;
    /// Moves of the mouse no more often than this.
    mouse_every_ms: 10.0, 0.0, 200.0;
    /// The mouse following the gaze waits while a ray or a finger acts on a monitor,
    /// and this long after.
    gaze_quiet_ms: 1000.0, 0.0, 10000.0;
    /// A scroll of one notch for this much of the hand (or a swipe) moving.
    scroll_notch_m: 0.01, 0.002, 0.1;
    /// A controller's stick fully over scrolls this many notches a second.
    stick_notches_per_s: 12.0, 1.0, 60.0;

    // ---- Step 8: taking a monitor with a hand ----

    /// A fist or a middle pinch takes what the hand pointed at this recently.
    take_latch_ms: 300.0, 0.0, 2000.0;
    /// "Hold" takes after a pinch held this long...
    hold_take_ms: 500.0, 100.0, 3000.0;
    /// ...with the ray turned no more than this.
    hold_slop_deg: 1.5, 0.1, 10.0;
    /// A middle pinch closes when thumb and middle tips come this near...
    middle_on_m: 0.015, 0.002, 0.06;
    /// ...and opens when they are this far apart.
    middle_off_m: 0.035, 0.005, 0.1;
    /// A middle pinch only with the index free: its tip this far from the thumb's.
    middle_index_apart_m: 0.03, 0.0, 0.1;
    /// A fingertip nearer the palm than this share of the palm's length is curled...
    fist_on: 0.6, 0.1, 1.5;
    /// ...and further than this, open.
    fist_off: 0.8, 0.1, 2.0;
    /// A middle pinch moved this far (up or down) scrolls; less is a right click.
    scroll_start_m: 0.02, 0.0, 0.1;

    // ---- What is shown ----

    /// A press held still shows a ring closing in only once it lasted this long.
    ring_after_ms: 150.0, 0.0, 2000.0;
    /// A controller's vibration: how strong (0 to 1).
    haptic_amplitude: 0.4, 0.0, 1.0;
    /// And how long.
    haptic_ms: 25.0, 1.0, 500.0;
}

impl Default for Feel {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Feel {
    /// `feel.txt` read over the defaults: the numbers, and what was wrong with its
    /// lines (each said once in the log by the caller).
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut feel = Self::DEFAULT;
        let mut problems = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                problems.push(format!("line {}: no \"=\"", n + 1));
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let Some(i) = Self::FIELDS.iter().position(|(k, _, _)| *k == key) else {
                problems.push(format!("line {}: no such number \"{key}\"", n + 1));
                continue;
            };
            let (_, min, max) = Self::FIELDS[i];
            match value.parse::<f32>() {
                Ok(v) if (min..=max).contains(&v) => {
                    if let Some(slot) = feel.slot(key) {
                        *slot = v;
                    }
                }
                Ok(v) => problems.push(format!(
                    "line {}: {key} = {v} is not within {min} to {max}",
                    n + 1
                )),
                Err(_) => problems.push(format!("line {}: {key}: not a number", n + 1)),
            }
        }
        // Pairs where one must stay above the other: back to both defaults.
        for (on, off) in feel.pairs() {
            let (a, b) = (feel.get(on), feel.get(off));
            if a >= b {
                problems.push(format!("{on} ({a}) must be less than {off} ({b})"));
                for key in [on, off] {
                    let default = Self::DEFAULT.get(key);
                    if let Some(slot) = feel.slot(key) {
                        *slot = default;
                    }
                }
            }
        }
        (feel, problems)
    }

    /// The pairs whose first must stay below the second (the hysteresis of each
    /// threshold, the bounds of each range).
    fn pairs(&self) -> [(&'static str, &'static str); 10] {
        [
            ("press_off", "press_on"),
            ("pinch_on_m", "pinch_off_m"),
            ("distance_min_m", "distance_max_m"),
            ("width_min_m", "width_max_m"),
            ("palm_face_off", "palm_face_on"),
            ("bound_near_m", "bound_far_m"),
            ("switch_touch_m", "switch_ray_m"),
            ("middle_on_m", "middle_off_m"),
            ("fist_on", "fist_off"),
            ("poke_press_m", "poke_release_m"),
        ]
    }

    /// The number of `key` (0 for a key there is not).
    pub fn get(&self, key: &str) -> f32 {
        Self::FIELDS
            .iter()
            .position(|(k, _, _)| *k == key)
            .map_or(0.0, |i| self.value(i))
    }

    /// The numbers that differ from the defaults, as `feel.txt` writes them (empty:
    /// all are the defaults).
    pub fn write_changed(&self, out: &mut String) {
        for (i, (key, _, _)) in Self::FIELDS.iter().enumerate() {
            let v = self.value(i);
            if v != Self::DEFAULT.value(i) {
                let _ = writeln!(out, "{key} = {v}");
            }
        }
    }

    // ---- As the code uses them ----

    pub fn hold_to_take(&self) -> Duration {
        ms(self.hold_to_take_ms)
    }

    pub fn touch_settles(&self) -> Duration {
        ms(self.touch_settles_ms)
    }

    pub fn after_touch(&self) -> Duration {
        ms(self.after_touch_ms)
    }

    pub fn touch_ring(&self) -> Duration {
        ms(self.touch_ring_ms)
    }

    pub fn palm_dwell(&self) -> Duration {
        ms(self.palm_dwell_ms)
    }

    pub fn attach_linger(&self) -> Duration {
        ms(self.attach_linger_ms)
    }

    pub fn gaze_dwell(&self) -> Duration {
        ms(self.gaze_dwell_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every default is within its own bounds, and every pair keeps its order.
    #[test]
    fn the_defaults_are_within_bounds() {
        let feel = Feel::DEFAULT;
        for (i, &(key, min, max)) in Feel::FIELDS.iter().enumerate() {
            let v = feel.value(i);
            assert!(min <= max, "{key}");
            assert!((min..=max).contains(&v), "{key} = {v} not in {min}..={max}");
            assert!(
                key.ends_with("_m")
                    || key.ends_with("_ms")
                    || key.ends_with("_deg")
                    || key.ends_with("_per_s")
                    || (0.0..=6.0).contains(&v),
                "{key}: a unit in the name, or a plain ratio"
            );
        }
        for (a, b) in feel.pairs() {
            assert!(feel.get(a) < feel.get(b), "{a} < {b}");
        }
        let keys: std::collections::HashSet<_> = Feel::FIELDS.iter().map(|f| f.0).collect();
        assert_eq!(keys.len(), Feel::FIELDS.len(), "each key once");
    }

    /// `feel.txt`: comments, spaces, good lines taken, bad ones said and passed over.
    #[test]
    fn feel_txt_is_read_over_the_defaults() {
        let text = "# tried on 2026-11-26\n\
                    hold_to_take_ms = 450\n\
                    \n\
                    switch_touch_m=0.1   # nearer\n\
                    no_such = 3\n\
                    reach_power = many\n\
                    press_on = 7\n\
                    just words\n";
        let (feel, problems) = Feel::parse(text);
        assert_eq!(feel.hold_to_take_ms, 450.0);
        assert_eq!(feel.hold_to_take(), Duration::from_millis(450));
        assert_eq!(feel.switch_touch_m, 0.1);
        assert_eq!(feel.reach_power, Feel::DEFAULT.reach_power);
        assert_eq!(feel.press_on, Feel::DEFAULT.press_on);
        assert_eq!(problems.len(), 4, "{problems:?}");
        let mut out = String::new();
        feel.write_changed(&mut out);
        assert_eq!(out, "hold_to_take_ms = 450\nswitch_touch_m = 0.1\n");
        let (back, problems) = Feel::parse(&out);
        assert!(problems.is_empty());
        assert_eq!(back, feel);
    }

    /// A threshold set past its pair is not taken: both stay as they were.
    #[test]
    fn a_pair_out_of_order_keeps_the_defaults() {
        let (feel, problems) = Feel::parse("press_off = 0.9\npress_on = 0.8\n");
        assert_eq!(feel.press_off, Feel::DEFAULT.press_off);
        assert_eq!(feel.press_on, Feel::DEFAULT.press_on);
        assert_eq!(problems.len(), 1);
        let (feel, problems) = Feel::parse("switch_touch_m = 0.25\n");
        assert_eq!(feel.switch_touch_m, Feel::DEFAULT.switch_touch_m);
        assert_eq!(problems.len(), 1);
    }
}
