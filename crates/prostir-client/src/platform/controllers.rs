//! The controllers the app binds, as a table: for each OpenXR interaction profile, which
//! of its inputs is the ray, "select", "grab", the stick, the menu and "Line up". The
//! shell suggests every profile whose extension the runtime has, and the runtime binds
//! the one that fits the controllers in the person's hands; the core sees only the
//! roles, never which controller.
//!
//! A headset with its own controllers is one more row (its profile, the extension
//! that names it, its inputs); `khr/simple_controller` is always there, so any
//! runtime gives at least a ray, a click and a menu button.

#![forbid(unsafe_code)]

/// What an input does in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The ray (a pose).
    Aim,
    /// Click and point (a value from 0 to 1, or a click).
    Select,
    /// Take a monitor (a value, or a click).
    Grab,
    /// Distance and size while holding (two axes).
    Stick,
    /// A vibration.
    Haptic,
    /// Opens and closes the menu (a click).
    Menu,
    /// "Line up" (a click).
    Align,
    /// A hand raised, ready to pinch: only then does it point.
    PinchReady,
}

/// Which hand an input is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Each,
    Left,
    Right,
}

/// The extension that names a profile (none: in OpenXR itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Core,
    /// `XR_EXT_hand_interaction`.
    HandInteraction,
    /// `XR_BD_controller_interaction`.
    BdControllers,
    /// `XR_HTC_vive_focus3_controller_interaction`.
    HtcFocus3,
}

/// One input of a profile: on which hand, its path below `/user/hand/<side>/`, and what
/// it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    pub side: Side,
    pub path: &'static str,
    pub role: Role,
}

/// An interaction profile and its inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub path: &'static str,
    pub needs: Needs,
    /// Tracked hands rather than controllers: they point only while ready to pinch,
    /// and drag with a pinch held (see `interaction`).
    pub hands: bool,
    pub inputs: &'static [Input],
}

const fn each(path: &'static str, role: Role) -> Input {
    Input {
        side: Side::Each,
        path,
        role,
    }
}

const fn left(path: &'static str, role: Role) -> Input {
    Input {
        side: Side::Left,
        path,
        role,
    }
}

const fn right(path: &'static str, role: Role) -> Input {
    Input {
        side: Side::Right,
        path,
        role,
    }
}

/// The profiles the app suggests, each with its inputs.
pub const PROFILES: &[Profile] = &[
    // Any runtime: a ray, a click, a menu button.
    Profile {
        path: "/interaction_profiles/khr/simple_controller",
        needs: Needs::Core,
        hands: false,
        inputs: &[
            each("input/aim/pose", Role::Aim),
            each("input/select/click", Role::Select),
            each("output/haptic", Role::Haptic),
            left("input/menu/click", Role::Menu),
        ],
    },
    Profile {
        path: "/interaction_profiles/oculus/touch_controller",
        needs: Needs::Core,
        hands: false,
        inputs: &[
            each("input/aim/pose", Role::Aim),
            each("input/squeeze/value", Role::Grab),
            each("input/trigger/value", Role::Select),
            each("input/thumbstick", Role::Stick),
            each("output/haptic", Role::Haptic),
            left("input/menu/click", Role::Menu),
            left("input/x/click", Role::Menu),
            right("input/a/click", Role::Align),
        ],
    },
    Profile {
        path: "/interaction_profiles/bytedance/pico4_controller",
        needs: Needs::BdControllers,
        hands: false,
        inputs: &[
            each("input/aim/pose", Role::Aim),
            each("input/squeeze/value", Role::Grab),
            each("input/trigger/value", Role::Select),
            each("input/thumbstick", Role::Stick),
            each("output/haptic", Role::Haptic),
            left("input/menu/click", Role::Menu),
            left("input/x/click", Role::Menu),
            right("input/a/click", Role::Align),
        ],
    },
    Profile {
        path: "/interaction_profiles/htc/vive_focus3_controller",
        needs: Needs::HtcFocus3,
        hands: false,
        inputs: &[
            each("input/aim/pose", Role::Aim),
            each("input/squeeze/click", Role::Grab),
            each("input/trigger/value", Role::Select),
            each("input/thumbstick", Role::Stick),
            each("output/haptic", Role::Haptic),
            left("input/menu/click", Role::Menu),
            left("input/x/click", Role::Menu),
            right("input/a/click", Role::Align),
        ],
    },
    // Tracked hands, as the runtime aims and pinches with them.
    Profile {
        path: "/interaction_profiles/ext/hand_interaction_ext",
        needs: Needs::HandInteraction,
        hands: true,
        inputs: &[
            each("input/aim/pose", Role::Aim),
            each("input/pinch_ext/value", Role::Select),
            // Pointing only while the hand is raised in a pinch-ready pose, so hands
            // resting on the keyboard never aim or click.
            each("input/pinch_ext/ready_ext", Role::PinchReady),
        ],
    },
];

impl Input {
    /// The input's full paths: one on each hand, or the one hand's.
    pub fn paths(&self) -> impl Iterator<Item = String> + '_ {
        let sides: &[&str] = match self.side {
            Side::Each => &["left", "right"],
            Side::Left => &["left"],
            Side::Right => &["right"],
        };
        sides
            .iter()
            .map(move |side| format!("/user/hand/{side}/{}", self.path))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Every profile points and clicks with both hands; every controller opens the
    /// menu; only the hands' profile has a pinch-ready input.
    #[test]
    fn every_profile_points_and_clicks() {
        for p in PROFILES {
            for role in [Role::Aim, Role::Select] {
                assert!(
                    p.inputs
                        .iter()
                        .any(|i| i.role == role && i.side == Side::Each),
                    "{}: no {role:?} on each hand",
                    p.path
                );
            }
            let menu = p.inputs.iter().any(|i| i.role == Role::Menu);
            assert_eq!(menu, !p.hands, "{}: a menu button", p.path);
            let ready = p.inputs.iter().any(|i| i.role == Role::PinchReady);
            assert_eq!(ready, p.hands, "{}: pinch-ready", p.path);
        }
    }

    /// The simple controller is always there, needing nothing; no profile twice, no
    /// input twice within one; paths as OpenXR writes them.
    #[test]
    fn the_table_is_well_formed() {
        let simple = PROFILES
            .iter()
            .find(|p| p.path.ends_with("/khr/simple_controller"));
        assert_eq!(simple.map(|p| p.needs), Some(Needs::Core));
        let mut profiles = BTreeSet::new();
        for p in PROFILES {
            assert!(profiles.insert(p.path), "{} twice", p.path);
            assert!(p.path.starts_with("/interaction_profiles/"), "{}", p.path);
            let mut paths = BTreeSet::new();
            for i in p.inputs {
                let well_formed = i.path.starts_with("input/") || i.path.starts_with("output/");
                assert!(well_formed, "{}: {}", p.path, i.path);
                assert!(
                    i.path.bytes().all(|b| b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b"/_".contains(&b)),
                    "{}: {}",
                    p.path,
                    i.path
                );
                assert_eq!(
                    i.path.starts_with("output/"),
                    i.role == Role::Haptic,
                    "{}: {}",
                    p.path,
                    i.path
                );
                for full in i.paths() {
                    assert!(paths.insert(full.clone()), "{}: {full} twice", p.path);
                }
            }
        }
    }

    #[test]
    fn paths_on_each_hand() {
        let i = each("input/aim/pose", Role::Aim);
        assert_eq!(
            i.paths().collect::<Vec<_>>(),
            [
                "/user/hand/left/input/aim/pose",
                "/user/hand/right/input/aim/pose"
            ]
        );
        let i = right("input/a/click", Role::Align);
        assert_eq!(
            i.paths().collect::<Vec<_>>(),
            ["/user/hand/right/input/a/click"]
        );
    }
}
