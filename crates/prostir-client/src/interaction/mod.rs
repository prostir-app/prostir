//! The hands and the controllers (step 8, section 1): what they point at, click, touch,
//! drag and resize, worked out from the frame's input ([`FrameInput`]) as a state
//! machine of each hand ([`Phase`]): a hand rests, presses, touches, carries or
//! resizes, one thing at a time. Every number of how it feels is in [`Feel`], which the
//! scene gives (the defaults are the input's numbers before step 8).
//!
//! It changes nothing but its own state. What the hands do (the intents: a click, a
//! take, a drag, a touch or a click on a PC's monitor, a vibration) is said to the
//! [`Scene`] they act on (the model) as it happens, so a click and a drag of one frame
//! come in the order they came and each sees what the one before did; what a press on
//! a menu means is asked of the menu's picture ([`Menus`]), which only the shell has.
//! From outside, only cancelling is done to it ([`Hands::cancel_all`]): the system menu
//! over the app, the hands no longer the app's.
//!
//! What step 8 adds (touching a monitor like a touchscreen, a pinch or a trigger on a
//! monitor as the mouse, switching between the two by how near the fingertip is,
//! taking a monitor with a fist, a middle pinch or a held pinch) acts only while the
//! scene's [`Modes`] say so (the preview build's "For development"); with them off the
//! hands do what they did before step 8, with the same numbers (see `monitors`).

#![forbid(unsafe_code)]

pub mod feel;
mod monitors;
pub mod record;
pub mod signals;
#[cfg(test)]
mod tests;
pub mod trial;

pub use feel::Feel;
pub use monitors::{Modes, MouseButton, PcInput, Take, TouchPhase, Touched};

use std::time::Instant;

use glam::{Quat, Vec2, Vec3};

use crate::ids::{DisplayKey, Target, Thing};
use crate::lang::Moment;
use crate::layout::{Grab, Hit, Placement, Ray};
use crate::ui::{self, Action};
use crate::world::{WRIST_MENU_WIDTH, panel_height, panel_width, wrist_menu_height};

/// `XR_HAND_JOINT_INDEX_TIP_EXT`.
pub const INDEX_TIP: usize = 10;

/// Analog press with hysteresis, so a half-pulled trigger does not flicker.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub down: bool,
    /// Went down during this frame.
    pub pressed: bool,
    /// Went up during this frame.
    pub released: bool,
}

impl Button {
    /// This frame's `value` (0 to 1), by the default numbers.
    pub fn update(&mut self, value: f32) {
        self.update_by(value, &Feel::DEFAULT);
    }

    /// This frame's `value` (0 to 1): pressed above [`Feel::press_on`], let go below
    /// [`Feel::press_off`].
    pub fn update_by(&mut self, value: f32, feel: &Feel) {
        let was = self.down;
        self.down = if was {
            value > feel.press_off
        } else {
            value > feel.press_on
        };
        self.pressed = self.down && !was;
        self.released = !self.down && was;
    }
}

/// One controller or tracked hand this frame: its ray, "select" (trigger or pinch) and
/// "grab" (grip).
#[derive(Debug, Default, Clone, Copy)]
pub struct HandState {
    pub ray: Option<Ray>,
    pub select: Button,
    pub grab: Button,
    pub stick: Vec2,
    /// Driven by a tracked hand (pinch to click, pinch and hold to drag).
    pub is_hand: bool,
}

/// A tracked hand's joints (`XR_HAND_JOINT_COUNT_EXT`): position and radius of each.
pub type Joints = [(Vec3, f32); 26];

/// Where the head is and which way it faces (`right` rolls and tilts with it: where
/// the right ear is).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Head {
    pub position: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
}

/// What the shell read for this pass of the loop (step 7, 4.2).
#[derive(Debug, Clone, Copy)]
pub struct FrameInput {
    /// The head this frame; `None` when nothing is drawn (the session not running,
    /// nothing to render): the time goes on, nothing moves.
    pub head: Option<Head>,
    /// Seconds since the last frame shown, from the predicted display times (for
    /// easing only, never for timers).
    pub dt: f32,
    /// The local time for the menu's clock, read by the shell about once a second.
    pub wall: Option<Moment>,
    /// The input is the app's (the session focused): the system menu is not over it.
    pub focused: bool,
    /// The left (0) and right (1) controller or hand.
    pub hands: [HandState; 2],
    /// The tracked hands' joints, while hands show and the input is the app's.
    pub joints: [Option<Joints>; 2],
    /// The controller's menu button went down this frame.
    pub menu_button: bool,
    /// The left hand's menu gesture is made (held: it counts once).
    pub menu_gesture: bool,
    /// The controller's "Line up" button went down this frame.
    pub align: bool,
}

impl FrameInput {
    /// A pass with nothing drawn: only the time and the clock go on.
    pub fn hidden(wall: Option<Moment>) -> Self {
        Self {
            head: None,
            dt: 0.0,
            wall,
            focused: false,
            hands: [HandState::default(); 2],
            joints: [None, None],
            menu_button: false,
            menu_gesture: false,
            align: false,
        }
    }

    /// The index fingertips of the tracked hands.
    pub fn tips(&self) -> [Option<Vec3>; 2] {
        self.joints.map(|j| j.map(|j| j[INDEX_TIP].0))
    }
}

/// Which menu's picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    /// The menu in front of the user.
    Main,
    /// The small one on the palm.
    Palm,
}

/// The menus' pictures, which know where their buttons are (the shell's, see
/// `ui::Panel`): the core never knows the menu's pixels.
pub trait Menus {
    /// The pointer is at `at` on `menu`, in its pixels (`None`: not on it).
    fn hover(&mut self, menu: Menu, at: Option<(f32, f32)>);
    /// `menu` is pressed at `at`: the action of the button there, if any.
    fn press(&mut self, menu: Menu, at: (f32, f32)) -> Option<Action>;
}

/// What the hands act on (the model), and what they ask of it.
pub trait Scene {
    // ---- What there is ----

    /// What `ray` points at, as drawn; `attach`: the "Attach" button of this detached
    /// window shows.
    fn pick(&self, ray: Ray, attach: Option<DisplayKey>) -> Option<Target>;
    /// A stream of display `id` shows.
    fn shows(&self, id: DisplayKey) -> bool;
    fn is_window(&self, id: DisplayKey) -> bool;
    /// Where the menu is, while it is open.
    fn menu(&self) -> Option<Placement>;
    fn large_menu(&self) -> bool;
    /// The menu goes on the right palm and the left hand touches it.
    fn left_handed(&self) -> bool;
    /// Where the palm menu is, while it shows.
    fn wrist(&self) -> Option<Placement>;
    /// Where `thing` goes now, for a hand to move it from.
    fn place(&self, thing: Thing) -> Option<Placement>;
    /// `thing` is in a hand, as the room sees it.
    fn holds(&self, thing: Thing) -> bool;
    /// The monitor resized with both hands, as the room sees it.
    fn resizing(&self) -> Option<DisplayKey>;
    /// The numbers of how the hands feel.
    fn feel(&self) -> &Feel {
        &Feel::DEFAULT
    }
    /// What the hands may do beyond the input before step 8 (none by default).
    fn modes(&self) -> Modes {
        Modes::default()
    }
    /// The monitor of a PC under the point `at` (a fingertip), nearest its surface
    /// (`only`: that one, or none), as a fingertip may touch it.
    fn touchable(&self, _at: Vec3, _only: Option<DisplayKey>) -> Option<Touched> {
        None
    }
    /// Where the flat-equivalent point `local` of monitor `id` is on its picture (0 to
    /// 1 from the top left).
    fn monitor_uv(&self, _id: DisplayKey, _local: Vec2) -> Option<Vec2> {
        None
    }
    /// The headset draws monitor `id`'s PC pointer over it and may move it: a ray on it
    /// is that pointer, and needs no dot of its own.
    fn mouse_shown(&self, _id: DisplayKey) -> bool {
        false
    }

    // ---- What the hands do ----

    /// The palm menu is at `place` now (`None`: not shown).
    fn palm(&mut self, place: Option<Placement>);
    /// A button of the main menu was pressed: what it comes to is said under it.
    fn press_menu(&mut self, now: Instant, action: Action);
    /// A button elsewhere (the palm menu, "Attach") was pressed.
    fn press(&mut self, now: Instant, action: Action);
    /// The menu button: the menu opens in front, or closes.
    fn toggle_menu(&mut self);
    /// The controller's "Line up" button.
    fn align(&mut self, now: Instant);
    /// A fingertip drags the menu to `position`.
    fn move_menu(&mut self, position: Vec3, head: Vec3);
    /// A hand takes `thing`: where it is taken at (`None`: it cannot be).
    fn hold(&mut self, thing: Thing) -> Option<Placement>;
    /// `thing`, in a hand, is at `place` now.
    fn drag(&mut self, thing: Thing, place: Placement, head: Vec3);
    /// A hand let go of `thing`: whether it stuck to a neighbour.
    fn let_go(&mut self, now: Instant, thing: Thing) -> bool;
    /// The other hand took hold of monitor `id` too: its width then (`None`: it
    /// cannot be resized).
    fn resize_start(&mut self, id: DisplayKey) -> Option<f32>;
    fn resize(&mut self, width: f32);
    /// The second hand let go.
    fn resize_end(&mut self);
    /// The hands are not the app's any more: what they held is let go where it is.
    fn cancel_holds(&mut self);
    /// A short vibration of `hand`'s controller.
    fn haptic(&mut self, hand: usize);
    /// A hand acts on a PC's monitor: the mouse, its buttons, a scroll, a touch.
    fn pc(&mut self, _now: Instant, _input: PcInput) {}
}

/// A monitor held with both hands: the distance between them sets its width.
#[derive(Debug, Clone, Copy)]
struct Scaling {
    id: DisplayKey,
    start_distance: f32,
    start_width: f32,
}

/// What one hand is doing (see the module): one thing at a time.
#[derive(Debug, Clone, Copy, Default)]
enum Phase {
    /// Pointing, or nothing; a fingertip near the menu hovers it.
    #[default]
    Idle,
    /// A fingertip pressed on the menu and not yet pulled back; `drag`: it pressed an
    /// empty place and drags the menu by that point (in the menu's plane).
    Touching { drag: Option<Vec2> },
    /// A pinch down on what the ray pointed at: since when (it takes the thing when
    /// held), and the menu's button that clicks when the pinch opens (as on the
    /// system's own windows).
    Pressed {
        since: Option<Instant>,
        click: Option<(Action, Vec3)>,
    },
    /// Something in the hand.
    Carry(monitors::Carry),
    /// The second hand on the monitor the first one holds: resizing it.
    Scale(Scaling),
    /// Step 8: a fingertip pressed on a monitor.
    Poke(monitors::Poke),
    /// Step 8: a pinch or a trigger down on a monitor (the mouse's button).
    RayPress(monitors::RayPress),
    /// Step 8: a middle pinch on a monitor: scrolling, or a right click.
    Scroll(monitors::Scroll),
}

/// One hand's state from frame to frame: its phase and what it remembers across them.
#[derive(Debug, Clone, Copy, Default)]
struct Hand {
    phase: Phase,
    /// Distance in front of the menu in the last frame, while in front of it.
    last_z: Option<f32>,
    /// The last touch: pulling the hand back often closes thumb and index into a
    /// pinch, which must not click whatever this hand's ray points at.
    touched_at: Option<Instant>,
    /// Whether this side was a tracked hand (not a controller) in its last frame.
    was_hand: Option<bool>,
    /// Step 8 (see `monitors`).
    m: monitors::HandMonitors,
}

/// Something that just happened, drawn as a ring spreading out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A touch or a click.
    Touch,
    /// A right click.
    Right,
}

/// The hands' own state from frame to frame (see the module).
#[derive(Debug)]
pub struct Hands {
    hands: [Hand; 2],
    /// What each ray points at this frame (none for a hand touching the menu).
    picks: [Option<Target>; 2],
    /// A fingertip near the menu this frame: where on it (in its plane) and how far
    /// in front.
    poke_hover: [Option<(Vec2, f32)>; 2],
    /// The poking fingertip left the palm menu since its last press.
    wrist_poke_armed: bool,
    /// Since when an open palm faces the face (its menu shows after a moment).
    palm_since: Option<Instant>,
    /// The left hand's menu gesture as last seen, to act once per gesture.
    menu_gesture_held: bool,
    /// The detached window whose "Attach" button shows, and when it was pointed at.
    attach_for: Option<(DisplayKey, Instant)>,
    /// Touches and clicks that just happened: where, when, which; drawn as a ring
    /// spreading out. Kept to be filled again.
    rings: Vec<(Vec3, Instant, Mark)>,
    /// The numbers of this frame (the scene's).
    feel: Feel,
    /// Step 8, both hands together (see `monitors`).
    m: monitors::Shared,
}

impl Default for Hands {
    fn default() -> Self {
        Self::new()
    }
}

impl Hands {
    pub fn new() -> Self {
        Self {
            hands: [Hand::default(); 2],
            picks: [None, None],
            poke_hover: [None, None],
            wrist_poke_armed: true,
            palm_since: None,
            menu_gesture_held: false,
            attach_for: None,
            rings: Vec::with_capacity(16),
            feel: Feel::DEFAULT,
            m: monitors::Shared::default(),
        }
    }

    // ---- What the shell draws ----

    /// What each ray points at this frame.
    pub fn picks(&self) -> &[Option<Target>; 2] {
        &self.picks
    }

    /// A fingertip near the menu: where on it (in its plane), how far in front, and
    /// whether it is pressed.
    pub fn pokes(&self) -> impl Iterator<Item = (usize, Vec2, f32, bool)> + '_ {
        self.poke_hover.iter().enumerate().filter_map(|(h, p)| {
            p.map(|(at, z)| {
                let pressed = matches!(self.hands[h].phase, Phase::Touching { drag: None });
                (h, at, z, pressed)
            })
        })
    }

    /// Hand `h`'s fingertip is near the menu.
    pub fn poking(&self, h: usize) -> bool {
        self.poke_hover.get(h).is_some_and(Option::is_some)
    }

    /// The touches spreading out as rings: where, and how far they spread (0 to 1).
    pub fn rings(&self, now: Instant) -> impl Iterator<Item = (Vec3, f32)> + '_ {
        self.marks(now).map(|(at, k, _)| (at, k))
    }

    /// The same, with what each was.
    pub fn marks(&self, now: Instant) -> impl Iterator<Item = (Vec3, f32, Mark)> + '_ {
        let ring = self.feel.touch_ring();
        self.rings.iter().filter_map(move |(at, when, mark)| {
            let age = now.saturating_duration_since(*when);
            (age < ring).then(|| (*at, age.as_secs_f32() / ring.as_secs_f32(), *mark))
        })
    }

    /// What the hands hold.
    pub fn held(&self) -> impl Iterator<Item = Thing> + '_ {
        self.hands.iter().filter_map(|h| match h.phase {
            Phase::Carry(c) => Some(c.grab.id),
            _ => None,
        })
    }

    /// The detached window whose "Attach" button shows.
    pub fn attach_for(&self) -> Option<DisplayKey> {
        self.attach_for.map(|(id, _)| id)
    }

    /// A ray points at the "Attach" button.
    pub fn attach_lit(&self) -> bool {
        self.picks
            .iter()
            .flatten()
            .any(|p| matches!(p.thing, Thing::Attach(_)))
    }

    /// A ray points at the menu button.
    pub fn over_button(&self) -> bool {
        self.picks
            .iter()
            .flatten()
            .any(|p| p.thing == Thing::MenuButton)
    }

    // ---- From outside ----

    /// A hand holds something, resizes, or waits on a pinch: what
    /// [`Hands::cancel_all`] would end.
    pub fn busy(&self) -> bool {
        self.hands.iter().any(|h| {
            matches!(
                h.phase,
                Phase::Carry(_)
                    | Phase::Scale(_)
                    | Phase::Pressed { .. }
                    | Phase::Poke(_)
                    | Phase::RayPress(_)
                    | Phase::Scroll(_)
            )
        })
    }

    /// The hands are not the app's any more (the system menu over it, another way of
    /// input): what they held is let go where it is, a drag and a resize end, a pinch
    /// on a menu button no longer clicks when it opens, and what they pressed on a
    /// PC's monitor is let go there.
    pub fn cancel_all(&mut self, scene: &mut impl Scene) {
        // Without the time of a frame: the last one's.
        let now = self.m.now;
        for h in 0..2 {
            self.end_monitor(h, now, scene, true);
            if !matches!(self.hands[h].phase, Phase::Touching { .. }) {
                self.hands[h].phase = Phase::Idle;
            }
        }
        scene.cancel_holds();
    }

    // ---- A frame ----

    /// The first part of this frame's input, acted on `scene`: the palm menu (on the
    /// palm turned to the face), the menu button and its hand gesture, "Line up". The
    /// shell then gives the palm menu's picture the model's state before
    /// [`Hands::step`] points at it. Nothing without a frame.
    pub fn buttons(&mut self, now: Instant, input: &FrameInput, scene: &mut impl Scene) {
        let Some(Head { position: head, .. }) = input.head else {
            return;
        };
        self.feel = *scene.feel();
        self.m.begin(now);
        let ring = self.feel.touch_ring();
        self.rings
            .retain(|(_, at, _)| now.saturating_duration_since(*at) < ring);
        self.sync(scene);
        let menu_hand = usize::from(scene.left_handed());
        let wrist = palm_menu_place(
            input.joints.get(menu_hand).and_then(Option::as_ref),
            menu_hand == 1,
            head,
            scene.wrist(),
            &mut self.palm_since,
            now,
            &self.feel,
        );
        scene.palm(wrist);
        // The left hand's menu gesture counts as the menu button.
        let gesture_pressed = input.menu_gesture && !self.menu_gesture_held;
        self.menu_gesture_held = input.menu_gesture;
        if input.menu_button || gesture_pressed {
            scene.toggle_menu();
            self.pressed(scene);
        }
        if input.align {
            scene.align(now);
            self.sync(scene);
        }
    }

    /// The rest of this frame's input, acted on `scene`: the pointers, the palm menu's
    /// and the main menu's touches (and, in step 8's modes, the monitors'), resizing
    /// with both hands, then each hand's click, drag and letting go. Nothing without a
    /// frame.
    pub fn step(
        &mut self,
        now: Instant,
        input: &FrameInput,
        scene: &mut impl Scene,
        menus: &mut impl Menus,
    ) {
        let Some(head) = input.head else {
            return;
        };
        self.sync(scene);
        let modes = scene.modes();
        if modes != self.m.modes {
            self.m.modes = modes;
            self.modes_changed(now, scene);
        }
        let tips = input.tips();

        // Step 8: each hand's gestures, and whether its fingertip touches or its ray
        // points.
        self.switch(input, scene);

        // Pointing, clicking and dragging.
        let attach = self.attach_for();
        for h in 0..2 {
            let ray = input.hands[h].ray.filter(|_| !self.hands[h].m.touch_mode);
            self.picks[h] = ray.and_then(|r| scene.pick(r, attach));
        }
        // A detached window pointed at shows "Attach" under it; it lingers so the
        // pointer can move down onto it.
        let pointed = self.picks.iter().flatten().find_map(|p| match p.thing {
            Thing::Display(id) | Thing::Attach(id) if scene.is_window(id) => Some(id),
            _ => None,
        });
        if let Some(id) = pointed {
            self.attach_for = Some((id, now));
        } else if self.attach_for.is_some_and(|(id, t)| {
            now.saturating_duration_since(t) > self.feel.attach_linger() || !scene.shows(id)
        }) {
            self.attach_for = None;
        }

        self.palm_menu(now, &tips, scene, menus);
        self.touch_menu(now, head.position, &tips, scene, menus);
        self.touch_monitors(now, input, scene);

        let large = scene.large_menu();
        let poked = self
            .poke_hover
            .iter()
            .flatten()
            .next()
            .map(|(at, _)| plane_point(*at, large));
        let hover = poked.or_else(|| {
            [1, 0].into_iter().find_map(|h| match self.picks[h] {
                Some(Target {
                    thing: Thing::Menu,
                    hit,
                }) => Some(panel_point(&hit, large)),
                _ => None,
            })
        });
        menus.hover(Menu::Main, hover);

        // Both hands on one monitor: the distance between them sets its width.
        let scaling = self.hands.iter().find_map(|h| match h.phase {
            Phase::Scale(s) => Some(s),
            _ => None,
        });
        if let Some(s) = scaling
            && let (Some(a), Some(b)) = (input.hands[0].ray, input.hands[1].ray)
        {
            let d = a.origin.distance(b.origin);
            let width = (s.start_width * d / s.start_distance.max(self.feel.two_hands_min_m))
                .clamp(self.feel.width_min_m, self.feel.width_max_m);
            scene.resize(width);
        }

        for hand in 0..2 {
            if !self.monitor_hand(now, hand, input, head, scene) {
                self.hand(now, hand, input, head.position, scene, menus);
            }
        }
        self.mouse(now, scene);
        self.shown_rays(input, head);
    }

    /// The palm menu, pointed at or touched with the other hand's index finger.
    fn palm_menu(
        &mut self,
        now: Instant,
        tips: &[Option<Vec3>; 2],
        scene: &mut impl Scene,
        menus: &mut impl Menus,
    ) {
        let f = self.feel;
        let mut wrist_hover = self
            .picks
            .iter()
            .flatten()
            .find_map(|p| matches!(p.thing, Thing::Wrist).then(|| wrist_point(p.hit.local)));
        let mut wrist_pressed = None;
        let poking_hand = 1 - usize::from(scene.left_handed());
        let tip = tips.get(poking_hand).copied().flatten();
        if let (Some(w), Some(tip)) = (scene.wrist(), tip) {
            let local = w.rotation.inverse() * (tip - w.position);
            let inside =
                local.x.abs() < WRIST_MENU_WIDTH / 2.0 && local.y.abs() < wrist_menu_height() / 2.0;
            if inside && local.z < f.wrist_hover_m {
                wrist_hover = Some(wrist_point(Vec2::new(local.x, local.y)));
            }
            if inside
                && local.z < f.wrist_press_m
                && local.z > -f.wrist_behind_m
                && self.wrist_poke_armed
            {
                self.wrist_poke_armed = false;
                wrist_pressed = wrist_hover;
                self.rings.push((tip, now, Mark::Touch));
            }
            if !inside || local.z > f.wrist_rearm_m {
                self.wrist_poke_armed = true;
            }
        } else {
            self.wrist_poke_armed = true;
        }
        if scene.wrist().is_some() {
            menus.hover(Menu::Palm, wrist_hover);
            if let Some(at) = wrist_pressed
                && let Some(action) = menus.press(Menu::Palm, at)
            {
                scene.haptic(poking_hand);
                scene.press(now, action);
                self.pressed(scene);
            }
        }
    }

    /// Touching the menu with a fingertip: hover when close, press on contact, drag
    /// the menu by touching an empty spot.
    fn touch_menu(
        &mut self,
        now: Instant,
        head: Vec3,
        tips: &[Option<Vec3>; 2],
        scene: &mut impl Scene,
        menus: &mut impl Menus,
    ) {
        let f = self.feel;
        self.poke_hover = [None, None];
        for h in 0..2 {
            let hand = &mut self.hands[h];
            // One thing at a time: a hand that holds, presses or touches a monitor
            // does not touch the menu.
            // A hand that carries something or presses a monitor does not touch the
            // menu (one thing at a time). A pinch held on something with the
            // fingertip coming near the menu: as before step 8, the finger is then
            // the pointer, not the ray, so the hold takes nothing; it presses nothing
            // either.
            let (touching, pinching) = match hand.phase {
                Phase::Idle => (None, false),
                Phase::Touching { drag } => (Some(drag), false),
                Phase::Pressed { .. } => (None, true),
                _ => {
                    hand.last_z = None;
                    continue;
                }
            };
            let (Some(tip), Some(place)) = (tips[h], scene.menu()) else {
                hand.last_z = None;
                if touching.is_some() {
                    hand.phase = Phase::Idle;
                }
                continue;
            };
            let large = scene.large_menu();
            let l = place.rotation.inverse() * (tip - place.position);
            let (half_w, half_h) = (panel_width(large) / 2.0, panel_height(large) / 2.0);
            if let Some(Some(anchor)) = touching {
                if l.z > f.poke_release_m {
                    hand.last_z = None;
                    hand.phase = Phase::Idle;
                    continue;
                }
                // The touched point follows the finger; pushing deeper moves it away.
                let shift = Vec3::new(
                    l.x - anchor.x,
                    l.y - anchor.y,
                    (l.z + f.menu_push_m).min(0.0),
                );
                let position = place.position + place.rotation * shift;
                scene.move_menu(position, head);
                self.picks[h] = None;
                continue;
            }
            let inside = l.x.abs() <= half_w && l.y.abs() <= half_h;
            if !inside || l.z > f.poke_hover_m || l.z < -f.poke_behind_m {
                hand.last_z = None;
                if !pinching {
                    hand.phase = Phase::Idle;
                }
                continue;
            }
            // Near the menu, the finger is the pointer, not this hand's ray.
            self.picks[h] = None;
            let at = Vec2::new(l.x, l.y);
            self.poke_hover[h] = Some((at, l.z));
            if pinching {
                hand.last_z = Some(l.z);
                continue;
            }
            let mut pressed = touching.is_some();
            if pressed && l.z > f.poke_release_m {
                pressed = false;
                hand.phase = Phase::Idle;
            }
            let arrived = hand.last_z.is_some_and(|z| z >= f.poke_press_m);
            // Hand tracking jitters: a finger "bouncing" off the menu is not a second tap.
            let settled = hand
                .touched_at
                .is_none_or(|t| now.saturating_duration_since(t) > f.touch_settles());
            hand.last_z = Some(l.z);
            if !pressed && arrived && settled && l.z < f.poke_press_m {
                hand.touched_at = Some(now);
                hand.phase = Phase::Touching { drag: None };
                self.rings.push((
                    place.position + place.rotation * Vec3::new(l.x, l.y, 0.003),
                    now,
                    Mark::Touch,
                ));
                match menus.press(Menu::Main, plane_point(at, large)) {
                    Some(action) => {
                        scene.press_menu(now, action);
                        self.pressed(scene);
                    }
                    None => self.hands[h].phase = Phase::Touching { drag: Some(at) },
                }
            }
        }
    }

    /// Hand `hand`'s click, drag and letting go.
    fn hand(
        &mut self,
        now: Instant,
        hand: usize,
        frame: &FrameInput,
        head: Vec3,
        scene: &mut impl Scene,
        menus: &mut impl Menus,
    ) {
        let f = self.feel;
        let mut input = frame.hands[hand];
        // The controller put down and the hand taking over (or the other way): the
        // button or pinch that held something will never be seen let go, so what it
        // held is let go now, as letting go would, and a pinch waiting to click is
        // forgotten.
        let switched = self.hands[hand]
            .was_hand
            .is_some_and(|was| was != input.is_hand);
        self.hands[hand].was_hand = Some(input.is_hand);
        // The button or pinch that holds is seen neither down nor let go: its input
        // stopped (hands that no longer point, as the settings say, give nothing), and
        // it will never be seen let go either.
        let holding = if input.is_hand {
            input.select
        } else {
            input.grab
        };
        let stopped = !holding.down && !holding.released;
        if (switched || (input.is_hand && stopped))
            && matches!(self.hands[hand].phase, Phase::Pressed { .. })
        {
            self.hands[hand].phase = Phase::Idle;
        }
        // A hand that just touched the menu: its next pinch is the hand pulling back,
        // not a click.
        if input.is_hand
            && self.hands[hand]
                .touched_at
                .is_some_and(|t| now.saturating_duration_since(t) < f.after_touch())
        {
            input.select.pressed = false;
        }
        if input.select.pressed {
            self.select_pressed(now, hand, input, scene, menus);
        }
        if input.select.released
            && let Phase::Pressed { click, .. } = self.hands[hand].phase
        {
            self.hands[hand].phase = Phase::Idle;
            // On the menu as it still is (not closed, nothing else pressed meanwhile).
            if let Some((action, point)) = click
                && scene.menu().is_some()
            {
                self.rings.push((point, now, Mark::Touch));
                scene.haptic(hand);
                scene.press_menu(now, action);
                self.pressed(scene);
            }
        }
        let carrying = matches!(self.hands[hand].phase, Phase::Carry(_));
        let (start_grab, end_grab) = if input.is_hand {
            let held = input.select.down
                && matches!(self.hands[hand].phase, Phase::Pressed { since: Some(t), .. }
                    if now.saturating_duration_since(t) >= f.hold_to_take());
            (held && !carrying, input.select.released)
        } else {
            (input.grab.pressed, input.grab.released)
        };
        let end_grab = end_grab || switched || stopped;

        if end_grab && matches!(self.hands[hand].phase, Phase::Scale(_)) {
            self.hands[hand].phase = Phase::Idle;
            scene.resize_end();
        }
        if end_grab && let Phase::Carry(released) = self.hands[hand].phase {
            self.hands[hand].phase = Phase::Idle;
            // Letting go with the first hand ends resizing too.
            let other = &mut self.hands[1 - hand];
            if matches!(other.phase, Phase::Scale(_)) {
                other.phase = Phase::Idle;
            }
            // Let go next to another monitor: it sticks to it, edge to edge.
            if scene.let_go(now, released.grab.id) {
                scene.haptic(hand);
            }
            self.sync(scene);
        }
        let Some(ray) = input.ray else {
            return;
        };
        if let Phase::Carry(mut carry) = self.hands[hand].phase {
            // Moved from where it goes now; the room keeps the keyboard flat, a
            // monitor on a wall parallel to it, and moves the rest along with "move all
            // together".
            if let Some(mut p) = scene.place(carry.grab.id) {
                carry
                    .grab
                    .update(&mut p, ray, head, input.stick, frame.dt, &f);
                scene.drag(carry.grab.id, p, head);
                self.hands[hand].phase = Phase::Carry(carry);
            }
        } else if start_grab {
            // Held on a button: the menu moves, the button is not pressed.
            self.hands[hand].phase = Phase::Idle;
            let other = match self.hands[1 - hand].phase {
                Phase::Carry(c) => Some(c.grab.id),
                _ => None,
            };
            let grabbed = match self.picks[hand] {
                Some(Target {
                    thing: thing @ (Thing::Menu | Thing::Keyboard),
                    hit,
                }) if other != Some(thing) => take(scene, thing, hit),
                // The other hand already holds it: resize with both.
                Some(Target {
                    thing: Thing::Display(id),
                    ..
                }) if other == Some(Thing::Display(id)) => {
                    if let (Some(a), Some(b)) = (frame.hands[0].ray, frame.hands[1].ray)
                        && let Some(start_width) = scene.resize_start(id)
                    {
                        self.hands[hand].phase = Phase::Scale(Scaling {
                            id,
                            start_distance: a.origin.distance(b.origin),
                            start_width,
                        });
                        scene.haptic(hand);
                    }
                    None
                }
                Some(Target {
                    thing: thing @ Thing::Display(_),
                    hit,
                }) => take(scene, thing, hit),
                _ => None,
            }
            .map(|g| match frame.head {
                Some(full) if input.is_hand => self.reach(g, ray, full, hand),
                _ => g,
            });
            if let Some(grab) = grabbed {
                scene.haptic(hand);
                self.hands[hand].phase = Phase::Carry(monitors::Carry::by_ray(grab));
            }
        }
    }

    /// Hand `hand`'s select (trigger or pinch) went down: a click on what its ray
    /// points at (a controller's at once, a hand's on a menu button when the pinch
    /// opens), and for a hand the start of a hold that takes it.
    fn select_pressed(
        &mut self,
        now: Instant,
        hand: usize,
        input: HandState,
        scene: &mut impl Scene,
        menus: &mut impl Menus,
    ) {
        let mut clicked = false;
        let mut click = None;
        match self.picks[hand] {
            Some(Target {
                thing: Thing::Attach(id),
                hit,
            }) => {
                clicked = true;
                self.rings.push((hit.point, now, Mark::Touch));
                scene.haptic(hand);
                self.attach_for = None;
                scene.press(now, Action::CloseWindow(id));
                self.pressed(scene);
            }
            Some(Target {
                thing: Thing::Menu,
                hit,
            }) => {
                let at = panel_point(&hit, scene.large_menu());
                if let Some(action) = menus.press(Menu::Main, at) {
                    if input.is_hand {
                        // Decided when the pinch opens: a click, or a drag.
                        click = Some((action, hit.point));
                    } else {
                        self.rings.push((hit.point, now, Mark::Touch));
                        clicked = true;
                        scene.haptic(hand);
                        scene.press_menu(now, action);
                        self.pressed(scene);
                    }
                }
            }
            Some(Target {
                thing: Thing::Wrist,
                hit,
            }) => {
                clicked = true;
                if let Some(action) = menus.press(Menu::Palm, wrist_point(hit.local)) {
                    self.rings.push((hit.point, now, Mark::Touch));
                    scene.haptic(hand);
                    scene.press(now, action);
                    self.pressed(scene);
                }
            }
            Some(Target {
                thing: Thing::MenuButton,
                hit,
            }) => {
                self.rings.push((hit.point, now, Mark::Touch));
                clicked = true;
                scene.haptic(hand);
                scene.toggle_menu();
                self.pressed(scene);
            }
            _ => {}
        }
        // Hands drag with pinch-and-hold, like the system's own windows.
        let since = (input.is_hand && !clicked && self.picks[hand].is_some()).then_some(now);
        if (since.is_some() || click.is_some())
            && matches!(self.hands[hand].phase, Phase::Idle | Phase::Pressed { .. })
        {
            self.hands[hand].phase = Phase::Pressed { since, click };
        }
    }

    /// A press was made (a button, the menu toggled): what the menu showed changed, so a
    /// pinch waiting to click on it clicks nothing.
    fn pressed(&mut self, scene: &impl Scene) {
        for hand in &mut self.hands {
            if let Phase::Pressed { click, .. } = &mut hand.phase {
                *click = None;
            }
        }
        self.sync(scene);
    }

    /// A hand lets go of what the room no longer has it hold (laid out anew, or gone),
    /// and resizing ends when the room ended it.
    fn sync(&mut self, scene: &impl Scene) {
        for hand in &mut self.hands {
            match hand.phase {
                Phase::Carry(c) if !scene.holds(c.grab.id) => hand.phase = Phase::Idle,
                Phase::Scale(s) if scene.resizing() != Some(s.id) => hand.phase = Phase::Idle,
                _ => {}
            }
        }
    }
}

/// A hand takes `thing`, pointed at with `hit`, where it is drawn: a monitor easing to
/// its place goes on from there, under the hand.
fn take(scene: &mut impl Scene, thing: Thing, hit: Hit) -> Option<Grab> {
    scene
        .hold(thing)
        .map(|at| Grab::start(thing, hit.t, hit.local, &at))
}

/// A point of the main menu's plane (from its centre, in metres) in its pixels.
fn plane_point(at: Vec2, large: bool) -> (f32, f32) {
    (
        (at.x / panel_width(large) + 0.5) * ui::WIDTH as f32,
        (0.5 - at.y / panel_height(large)) * ui::HEIGHT as f32,
    )
}

/// Where a ray hit the main menu, in its pixels.
fn panel_point(hit: &Hit, large: bool) -> (f32, f32) {
    plane_point(hit.local, large)
}

/// A point of the palm menu's plane in its pixels.
fn wrist_point(local: Vec2) -> (f32, f32) {
    (
        (local.x / WRIST_MENU_WIDTH + 0.5) * ui::WRIST_WIDTH as f32,
        (0.5 - local.y / wrist_menu_height()) * ui::WRIST_HEIGHT as f32,
    )
}

/// Where the palm menu goes: lying on the palm (a little above it, its top towards the
/// fingers), while an open palm is turned to the face. It takes a moment of that to
/// appear, so hands passing by (typing, reaching) do not bring it up. `since`: when
/// the palm started facing the face.
fn palm_menu_place(
    joints: Option<&Joints>,
    right_hand: bool,
    head: Vec3,
    previous: Option<Placement>,
    since: &mut Option<Instant>,
    now: Instant,
    feel: &Feel,
) -> Option<Placement> {
    let Some(j) = joints else {
        *since = None;
        return None;
    };
    let (palm, wrist) = (j[0].0, j[1].0);
    // Knuckles, a hand's width apart: a steady palm normal. For the left hand it is
    // little × index from the wrist (checked in the headset: the other order points
    // out of the back of the hand); the right hand is its mirror image.
    let (index, middle, little, middle_tip) = (j[7].0, j[12].0, j[22].0, j[15].0);
    let mut normal = (little - wrist).cross(index - wrist).normalize_or_zero();
    if right_hand {
        normal = -normal;
    }
    let facing = normal.dot((head - palm).normalize_or_zero());
    let open = middle_tip.distance(palm) > feel.palm_open_m;
    let needed = if previous.is_some() {
        feel.palm_face_off
    } else {
        feel.palm_face_on
    };
    if facing < needed || !open {
        *since = None;
        return None;
    }
    if previous.is_none()
        && now.saturating_duration_since(*since.get_or_insert(now)) < feel.palm_dwell()
    {
        return None;
    }
    let up = (middle - wrist)
        .reject_from_normalized(normal)
        .normalize_or(Vec3::Y);
    let rotation = Quat::from_mat3(&glam::Mat3::from_cols(up.cross(normal), up, normal));
    let mut at = palm + normal * 0.03;
    let mut rotation = rotation.normalize();
    // Hands shake a little: the menu follows smoothly.
    if let Some(p) = previous {
        at = p.position.lerp(at, 0.35);
        rotation = p.rotation.slerp(rotation, 0.35);
    }
    Some(Placement {
        position: at,
        rotation,
        width: WRIST_MENU_WIDTH,
    })
}
