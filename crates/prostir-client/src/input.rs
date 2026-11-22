//! Controller and hand input, mapped onto the same actions: a ray (aim pose),
//! "select" (trigger or pinch) and "grab" (grip or fist).

use anyhow::Result;
use glam::{Vec2, Vec3};
use openxr as xr;

use crate::interaction::{Button, HandState};
use crate::layout::Ray;
use crate::xrutil::{Gfx, quat, vec3};

pub struct Controls {
    set: xr::ActionSet,
    hands: [xr::Path; 2],
    aim_spaces: [xr::Space; 2],
    grab: xr::Action<f32>,
    select: xr::Action<f32>,
    thumbstick: xr::Action<xr::Vector2f>,
    haptic: xr::Action<xr::Haptic>,
    menu: xr::Action<bool>,
    align: xr::Action<bool>,
    pinch_ready: xr::Action<bool>,
    /// `/interaction_profiles/ext/hand_interaction_ext`, when hands are bound.
    hand_profile: Option<xr::Path>,
    /// Which sides are currently driven by tracked hands rather than controllers.
    tracked_hands: [bool; 2],
    /// Last reported profile per side, to log changes.
    last_profile: [Option<xr::Path>; 2],
    /// Sides whose input comes from the tracked joints (see `hands_from_joints`).
    joint_hands: [bool; 2],
    joint_pinch: [bool; 2],
    /// Whether tracked hands may point and click; otherwise their input is ignored.
    pub hands_enabled: bool,
    pub state: [HandState; 2],
}

impl Controls {
    pub fn new(
        instance: &xr::Instance,
        session: &xr::Session<Gfx>,
        hand_interaction: bool,
    ) -> Result<Self> {
        let set = instance.create_action_set("monitors", "Monitors", 0)?;
        let hands = [
            instance.string_to_path("/user/hand/left")?,
            instance.string_to_path("/user/hand/right")?,
        ];
        let aim = set.create_action::<xr::Posef>("aim", "Aim", &hands)?;
        let grab = set.create_action::<f32>("grab", "Grab monitor", &hands)?;
        let select = set.create_action::<f32>("select", "Select", &hands)?;
        let thumbstick =
            set.create_action::<xr::Vector2f>("adjust", "Distance and size", &hands)?;
        let haptic = set.create_action::<xr::Haptic>("haptic", "Haptic", &hands)?;
        let menu = set.create_action::<bool>("menu", "Menu", &[])?;
        let pinch_ready =
            set.create_action::<bool>("pinch_ready", "Hand ready to pinch", &hands)?;
        let align = set.create_action::<bool>("align", "Align monitors", &[])?;
        let p = |s: &str| instance.string_to_path(s);

        let mut touch = Vec::new();
        for side in ["left", "right"] {
            let path = |input: &str| p(&format!("/user/hand/{side}/{input}"));
            touch.push(xr::Binding::new(&aim, path("input/aim/pose")?));
            touch.push(xr::Binding::new(&grab, path("input/squeeze/value")?));
            touch.push(xr::Binding::new(&select, path("input/trigger/value")?));
            touch.push(xr::Binding::new(&thumbstick, path("input/thumbstick")?));
            touch.push(xr::Binding::new(&haptic, path("output/haptic")?));
        }
        touch.push(xr::Binding::new(
            &menu,
            p("/user/hand/left/input/menu/click")?,
        ));
        touch.push(xr::Binding::new(&menu, p("/user/hand/left/input/x/click")?));
        touch.push(xr::Binding::new(
            &align,
            p("/user/hand/right/input/a/click")?,
        ));
        instance.suggest_interaction_profile_bindings(
            p("/interaction_profiles/oculus/touch_controller")?,
            &touch,
        )?;

        let hand_profile = if hand_interaction {
            Some(p("/interaction_profiles/ext/hand_interaction_ext")?)
        } else {
            None
        };
        if let Some(profile) = hand_profile {
            let mut hand = Vec::new();
            for side in ["left", "right"] {
                let path = |input: &str| p(&format!("/user/hand/{side}/input/{input}"));
                hand.push(xr::Binding::new(&aim, path("aim/pose")?));
                hand.push(xr::Binding::new(&select, path("pinch_ext/value")?));
                // Pointing only while the hand is raised in a pinch-ready pose, so
                // hands resting on the keyboard never aim or click.
                hand.push(xr::Binding::new(&pinch_ready, path("pinch_ext/ready_ext")?));
            }
            instance.suggest_interaction_profile_bindings(profile, &hand)?;
        }

        session.attach_action_sets(&[&set])?;
        let aim_spaces = [
            aim.create_space(session, hands[0], xr::Posef::IDENTITY)?,
            aim.create_space(session, hands[1], xr::Posef::IDENTITY)?,
        ];
        Ok(Self {
            set,
            hands,
            aim_spaces,
            grab,
            select,
            thumbstick,
            haptic,
            menu,
            align,
            pinch_ready,
            hand_profile,
            tracked_hands: [false; 2],
            last_profile: [None; 2],
            joint_hands: [false; 2],
            joint_pinch: [false; 2],
            hands_enabled: false,
            state: [HandState::default(); 2],
        })
    }

    /// Call when the runtime reports an interaction profile change (controllers
    /// picked up or put down).
    pub fn refresh_profiles(&mut self, session: &xr::Session<Gfx>) {
        for hand in 0..2 {
            let profile = session.current_interaction_profile(self.hands[hand]).ok();
            let is_hand = profile.is_some() && profile == self.hand_profile;
            if is_hand != self.tracked_hands[hand] || profile != self.last_profile[hand] {
                let name = profile
                    .filter(|p| *p != xr::Path::NULL)
                    .and_then(|p| session.instance().path_to_string(p).ok())
                    .unwrap_or_else(|| "none".into());
                log::info!("{} hand: {name}", if hand == 0 { "left" } else { "right" });
                self.last_profile[hand] = profile;
            }
            self.tracked_hands[hand] = is_hand;
        }
    }

    /// Quest sometimes keeps reporting a controller for a side whose controller was put
    /// down while the cameras already track the hand. Then that side's input is worked
    /// out from the hand joints: the thumb and index tips together are a pinch, and the
    /// ray runs from about the shoulder through the pinch, like the system's own.
    /// Call after `sync`, with this frame's joints.
    pub fn hands_from_joints(
        &mut self,
        joints: &[Option<[(Vec3, f32); 26]>; 2],
        head: Vec3,
        forward: Vec3,
    ) {
        const THUMB_TIP: usize = 5;
        const INDEX_TIP: usize = 10;
        #[allow(clippy::needless_range_loop)] // `hand` indexes several per-side arrays
        for hand in 0..2 {
            let use_joints = !self.tracked_hands[hand] && joints[hand].is_some();
            if use_joints != self.joint_hands[hand] {
                log::info!(
                    "{} hand: {}",
                    if hand == 0 { "left" } else { "right" },
                    if use_joints {
                        "from the joints"
                    } else {
                        "from the runtime"
                    }
                );
                self.joint_hands[hand] = use_joints;
                self.joint_pinch[hand] = false;
                self.state[hand] = HandState::default();
            }
            let Some(j) = joints[hand].filter(|_| use_joints) else {
                continue;
            };
            let s = &mut self.state[hand];
            s.is_hand = true;
            s.grab = Button::default();
            s.stick = Vec2::ZERO;
            if !self.hands_enabled {
                s.ray = None;
                s.select = Button::default();
                continue;
            }
            let (thumb, index) = (j[THUMB_TIP].0, j[INDEX_TIP].0);
            let gap = thumb.distance(index);
            let pinched = if self.joint_pinch[hand] {
                gap < 0.035
            } else {
                gap < 0.015
            };
            self.joint_pinch[hand] = pinched;
            s.select.update(if pinched { 1.0 } else { 0.0 });
            // Only a hand getting ready to pinch points, as with the runtime's input.
            if gap > 0.05 && !s.select.down {
                s.ray = None;
                continue;
            }
            let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z);
            let side = if hand == 0 { -1.0 } else { 1.0 };
            let shoulder = head - Vec3::Y * 0.18 + flat.cross(Vec3::Y) * side * 0.17;
            let origin = (thumb + index) * 0.5;
            s.ray = Some(Ray {
                origin,
                dir: (origin - shoulder).normalize_or(flat),
            });
        }
    }

    /// Reads all inputs for this frame.
    pub fn sync(
        &mut self,
        session: &xr::Session<Gfx>,
        base: &xr::Space,
        time: xr::Time,
    ) -> Result<()> {
        session.sync_actions(&[(&self.set).into()])?;
        // Cheap, and the runtime does not always announce the profile at start-up.
        self.refresh_profiles(session);
        for hand in 0..2 {
            let path = self.hands[hand];
            let location = self.aim_spaces[hand].locate(base, time)?;
            let tracked = location.location_flags.contains(
                xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID,
            );
            let is_hand = self.tracked_hands[hand];
            if self.joint_hands[hand] && !is_hand {
                // Filled in by `hands_from_joints`.
                continue;
            }
            let ready = !is_hand
                || self
                    .pinch_ready
                    .state(session, path)
                    .is_ok_and(|s| s.current_state);
            let s = &mut self.state[hand];
            s.is_hand = is_hand;
            if is_hand && (!self.hands_enabled || !ready) {
                s.ray = None;
                s.grab = Button::default();
                s.stick = Vec2::ZERO;
                if self.hands_enabled {
                    // Still notice the pinch opening, so a drag always ends.
                    let pinch = self
                        .select
                        .state(session, path)
                        .map_or(0.0, |v| v.current_state);
                    s.select.update(pinch);
                } else {
                    s.select = Button::default();
                }
                continue;
            }
            s.ray = tracked.then(|| Ray {
                origin: vec3(location.pose.position),
                dir: quat(location.pose.orientation) * -Vec3::Z,
            });
            let value =
                |a: &xr::Action<f32>| a.state(session, path).map_or(0.0, |s| s.current_state);
            s.select.update(value(&self.select));
            // Hands have no grip: `interaction` turns pinch-and-hold into a drag.
            s.grab.update(if is_hand { 0.0 } else { value(&self.grab) });
            s.stick = self
                .thumbstick
                .state(session, path)
                .map_or(Vec2::ZERO, |s| {
                    Vec2::new(s.current_state.x, s.current_state.y)
                });
        }
        Ok(())
    }

    pub fn menu_pressed(&self, session: &xr::Session<Gfx>) -> bool {
        pressed_now(&self.menu, session)
    }

    pub fn align_pressed(&self, session: &xr::Session<Gfx>) -> bool {
        pressed_now(&self.align, session)
    }

    pub fn pulse(&self, session: &xr::Session<Gfx>, hand: usize) {
        let vibration = xr::HapticVibration::new()
            .amplitude(0.4)
            .duration(xr::Duration::from_nanos(25_000_000))
            .frequency(xr::FREQUENCY_UNSPECIFIED);
        let _ = self
            .haptic
            .apply_feedback(session, self.hands[hand], &vibration);
    }
}

fn pressed_now(action: &xr::Action<bool>, session: &xr::Session<Gfx>) -> bool {
    action
        .state(session, xr::Path::NULL)
        .is_ok_and(|s| s.is_active && s.changed_since_last_sync && s.current_state)
}
