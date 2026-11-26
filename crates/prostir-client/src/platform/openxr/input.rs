//! Controller and hand input, mapped onto the same actions: a ray (aim pose),
//! "select" (trigger or pinch) and "grab" (grip or fist). Which inputs of which
//! controller are bound comes from the table in `platform::controllers`.

use anyhow::{Result, bail};
use glam::{Vec2, Vec3};
use log::{info, warn};
use openxr as xr;

use crate::interaction::{Button, Feel, HandState, signals};
use crate::layout::Ray;
use crate::platform::controllers::{self, Role};
use crate::platform::openxr::{Gfx, has, quat, vec3};

pub struct Controls {
    set: xr::ActionSet,
    hands: [xr::Path; 2],
    aim_spaces: [xr::Space; 2],
    /// Where each controller is held, for its model.
    grip_spaces: [xr::Space; 2],
    grab: xr::Action<f32>,
    select: xr::Action<f32>,
    thumbstick: xr::Action<xr::Vector2f>,
    haptic: xr::Action<xr::Haptic>,
    menu: xr::Action<bool>,
    align: xr::Action<bool>,
    pinch_ready: xr::Action<bool>,
    /// The profiles of tracked hands bound (see `controllers::Profile::hands`).
    hand_profiles: Vec<xr::Path>,
    /// `khr/simple_controller`, when bound: a runtime may give tracked hands as that
    /// profile, its menu click then being the system's own menu gesture.
    simple_profile: Option<xr::Path>,
    /// Each side's hand joints were tracked this frame.
    joints_seen: [bool; 2],
    /// Which sides are currently driven by tracked hands rather than controllers.
    tracked_hands: [bool; 2],
    /// Last reported profile per side, to log changes.
    last_profile: [Option<xr::Path>; 2],
    /// Sides whose input comes from the tracked joints (see `hands_from_joints`).
    joint_hands: [bool; 2],
    joint_pinch: [bool; 2],
    /// Where each controller is held this frame (`None`: not tracked, or a hand).
    pub grips: [Option<xr::Posef>; 2],
    /// Whether tracked hands may point and click; otherwise their input is ignored.
    pub hands_enabled: bool,
    pub state: [HandState; 2],
    /// The numbers of the input (the model's, set each frame).
    pub feel: Feel,
}

impl Controls {
    /// The actions, and the bindings of every profile of the table whose extension is
    /// on in `exts`.
    pub fn new(
        instance: &xr::Instance,
        session: &xr::Session<Gfx>,
        exts: &xr::ExtensionSet,
    ) -> Result<Self> {
        let set = instance.create_action_set("monitors", "Monitors", 0)?;
        let hands = [
            instance.string_to_path("/user/hand/left")?,
            instance.string_to_path("/user/hand/right")?,
        ];
        let aim = set.create_action::<xr::Posef>("aim", "Aim", &hands)?;
        let grip = set.create_action::<xr::Posef>("grip", "Controller", &hands)?;
        let grab = set.create_action::<f32>("grab", "Grab monitor", &hands)?;
        let select = set.create_action::<f32>("select", "Select", &hands)?;
        let thumbstick =
            set.create_action::<xr::Vector2f>("adjust", "Distance and size", &hands)?;
        let haptic = set.create_action::<xr::Haptic>("haptic", "Haptic", &hands)?;
        // For each hand, so a hand whose "button" is the system's own gesture can be
        // left out (see `menu_pressed`).
        let menu = set.create_action::<bool>("menu", "Menu", &hands)?;
        let pinch_ready =
            set.create_action::<bool>("pinch_ready", "Hand ready to pinch", &hands)?;
        let align = set.create_action::<bool>("align", "Align monitors", &[])?;
        // Each profile of the table the runtime can have; one it refuses (an input it
        // does not know) is left out, the others still bound.
        let mut hand_profiles = Vec::new();
        let mut simple_profile = None;
        let mut suggested = 0;
        for profile in controllers::PROFILES {
            if !has(exts, profile.needs) {
                continue;
            }
            let mut bindings = Vec::new();
            for input in profile.inputs {
                for path in input.paths() {
                    let path = instance.string_to_path(&path)?;
                    bindings.push(match input.role {
                        Role::Aim => xr::Binding::new(&aim, path),
                        Role::Grip => xr::Binding::new(&grip, path),
                        Role::Select => xr::Binding::new(&select, path),
                        Role::Grab => xr::Binding::new(&grab, path),
                        Role::Stick => xr::Binding::new(&thumbstick, path),
                        Role::Haptic => xr::Binding::new(&haptic, path),
                        Role::Menu => xr::Binding::new(&menu, path),
                        Role::Align => xr::Binding::new(&align, path),
                        Role::PinchReady => xr::Binding::new(&pinch_ready, path),
                    });
                }
            }
            let path = instance.string_to_path(profile.path)?;
            match instance.suggest_interaction_profile_bindings(path, &bindings) {
                Ok(()) => {
                    suggested += 1;
                    if profile.hands {
                        hand_profiles.push(path);
                    }
                    if profile.path == controllers::SIMPLE {
                        simple_profile = Some(path);
                    }
                }
                Err(e) => warn!("controllers {}: {e}", profile.path),
            }
        }
        if suggested == 0 {
            bail!("the runtime took no controller's bindings");
        }
        info!("bindings for {suggested} interaction profiles");

        session.attach_action_sets(&[&set])?;
        let aim_spaces = [
            aim.create_space(session, hands[0], xr::Posef::IDENTITY)?,
            aim.create_space(session, hands[1], xr::Posef::IDENTITY)?,
        ];
        let grip_spaces = [
            grip.create_space(session, hands[0], xr::Posef::IDENTITY)?,
            grip.create_space(session, hands[1], xr::Posef::IDENTITY)?,
        ];
        Ok(Self {
            set,
            hands,
            aim_spaces,
            grip_spaces,
            grab,
            select,
            thumbstick,
            haptic,
            menu,
            align,
            pinch_ready,
            hand_profiles,
            simple_profile,
            joints_seen: [false; 2],
            tracked_hands: [false; 2],
            last_profile: [None; 2],
            joint_hands: [false; 2],
            joint_pinch: [false; 2],
            grips: [None; 2],
            hands_enabled: false,
            state: [HandState::default(); 2],
            feel: Feel::DEFAULT,
        })
    }

    /// The path of hand `side` (`/user/hand/left` or `right`).
    pub fn hand_path(&self, side: usize) -> xr::Path {
        self.hands[side]
    }

    /// The interaction profile of side `side` as last read (`None`: none yet), and
    /// whether it is tracked hands rather than a controller.
    pub fn profile(&self, side: usize) -> (Option<xr::Path>, bool) {
        (
            self.last_profile[side].filter(|p| *p != xr::Path::NULL),
            self.tracked_hands[side] || self.joint_hands[side],
        )
    }

    /// Call when the runtime reports an interaction profile change (controllers
    /// picked up or put down). Whether a side's profile changed.
    pub fn refresh_profiles(&mut self, session: &xr::Session<Gfx>) -> bool {
        let mut changed = false;
        for hand in 0..2 {
            let profile = session.current_interaction_profile(self.hands[hand]).ok();
            let is_hand = profile.is_some_and(|p| self.hand_profiles.contains(&p));
            if is_hand != self.tracked_hands[hand] || profile != self.last_profile[hand] {
                // Nothing to something at the start is not a change of controllers.
                changed |= self.last_profile[hand].is_some_and(|p| p != xr::Path::NULL);
                let name = profile
                    .filter(|p| *p != xr::Path::NULL)
                    .and_then(|p| session.instance().path_to_string(p).ok())
                    .unwrap_or_else(|| "none".into());
                info!("{} hand: {name}", if hand == 0 { "left" } else { "right" });
                self.last_profile[hand] = profile;
            }
            self.tracked_hands[hand] = is_hand;
        }
        changed
    }

    /// A runtime may keep reporting a controller for a side whose controller was put
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
        #[allow(clippy::needless_range_loop)] // `hand` indexes several per-side arrays
        for hand in 0..2 {
            self.joints_seen[hand] = joints[hand].is_some();
            let use_joints = !self.tracked_hands[hand] && joints[hand].is_some();
            if use_joints != self.joint_hands[hand] {
                info!(
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
            let (pinched, ray) = signals::index_pinch(
                &j,
                self.joint_pinch[hand],
                head,
                forward,
                hand == 0,
                &self.feel,
            );
            self.joint_pinch[hand] = pinched;
            s.select
                .update_by(if pinched { 1.0 } else { 0.0 }, &self.feel);
            s.ray = ray;
        }
    }

    /// Reads all inputs for this frame; whether a side's controllers changed (which a
    /// runtime does not always announce).
    pub fn sync(
        &mut self,
        session: &xr::Session<Gfx>,
        base: &xr::Space,
        time: xr::Time,
    ) -> Result<bool> {
        session.sync_actions(&[(&self.set).into()])?;
        // Cheap, and the runtime does not always announce the profile at start-up.
        let changed = self.refresh_profiles(session);
        for hand in 0..2 {
            let path = self.hands[hand];
            let location = self.aim_spaces[hand].locate(base, time)?;
            let tracked = location.location_flags.contains(
                xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID,
            );
            let is_hand = self.tracked_hands[hand];
            self.grips[hand] = None;
            if self.joint_hands[hand] && !is_hand {
                // Filled in by `hands_from_joints`.
                continue;
            }
            if !is_hand {
                let held = self.grip_spaces[hand].locate(base, time)?;
                let flags = xr::SpaceLocationFlags::POSITION_VALID
                    | xr::SpaceLocationFlags::ORIENTATION_VALID;
                self.grips[hand] = held.location_flags.contains(flags).then_some(held.pose);
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
                    s.select.update_by(pinch, &self.feel);
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
            s.select.update_by(value(&self.select), &self.feel);
            // Hands have no grip: `interaction` turns pinch-and-hold into a drag.
            s.grab
                .update_by(if is_hand { 0.0 } else { value(&self.grab) }, &self.feel);
            s.stick = self
                .thumbstick
                .state(session, path)
                .map_or(Vec2::ZERO, |s| {
                    Vec2::new(s.current_state.x, s.current_state.y)
                });
        }
        Ok(changed)
    }

    /// The menu button pressed this frame, on a side with a controller in it. A side
    /// worked out from the joints, or one the runtime gives as the simple controller
    /// while its hand's joints are tracked, is left out: its menu click is the system's
    /// own menu gesture, which would open the menu and the system would close it again.
    /// Call after `hands_from_joints`.
    pub fn menu_pressed(&self, session: &xr::Session<Gfx>) -> bool {
        (0..2).any(|hand| {
            let gesture = self.joint_hands[hand]
                || (self.joints_seen[hand]
                    && self.simple_profile.is_some()
                    && self.last_profile[hand] == self.simple_profile);
            !gesture && pressed_now(&self.menu, session, self.hands[hand])
        })
    }

    pub fn align_pressed(&self, session: &xr::Session<Gfx>) -> bool {
        pressed_now(&self.align, session, xr::Path::NULL)
    }

    pub fn pulse(&self, session: &xr::Session<Gfx>, hand: usize) {
        let vibration = xr::HapticVibration::new()
            .amplitude(self.feel.haptic_amplitude)
            .duration(xr::Duration::from_nanos(
                (self.feel.haptic_ms * 1e6).round() as i64,
            ))
            .frequency(xr::FREQUENCY_UNSPECIFIED);
        let _ = self
            .haptic
            .apply_feedback(session, self.hands[hand], &vibration);
    }
}

/// `action` pressed since the last sync, on the hand `side` (`NULL`: either).
fn pressed_now(action: &xr::Action<bool>, session: &xr::Session<Gfx>, side: xr::Path) -> bool {
    action
        .state(session, side)
        .is_ok_and(|s| s.is_active && s.changed_since_last_sync && s.current_state)
}
