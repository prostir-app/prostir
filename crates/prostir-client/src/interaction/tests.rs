//! The hands as 0.1 had them, on the desktop with the time passed in: what a click, a
//! pinch, a touch, a hold, a drag and two hands come to, against a scene that only
//! writes down what it is asked.

#![expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]

use std::collections::HashMap;
use std::time::Duration;

use glam::{Quat, Vec2, Vec3};
use prostir_proto::state::DisplayId;

use super::*;
use crate::ids::PcId;
use crate::world::panel_width;

const MS: Duration = Duration::from_millis(1);
const HOLD_TO_DRAG: Duration = Duration::from_millis(300);
const TOUCH_RING: Duration = Duration::from_millis(350);
const AFTER_TOUCH: Duration = Duration::from_millis(600);
const ATTACH_LINGER: Duration = Duration::from_millis(1200);

fn monitor(n: u32) -> DisplayKey {
    DisplayKey::new(PcId([3; 8]), DisplayId(n))
}

/// What the scene was asked, in order.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    PressMenu(Action),
    Press(Action),
    Toggle,
    Align,
    MoveMenu,
    Hold(Thing),
    Drag(Thing),
    LetGo(Thing),
    ResizeStart(DisplayKey),
    Resize(f32),
    ResizeEnd,
    Cancel,
    Haptic(usize),
}

/// A scene that writes down what it is asked: each hand's ray points at what
/// `pick_for` says for that hand (the left one's ray starts left of the head).
struct Fake {
    pick_for: [Option<Target>; 2],
    menu: Option<Placement>,
    wrist: Option<Placement>,
    places: HashMap<Thing, Placement>,
    held: Vec<Thing>,
    resizing: Option<DisplayKey>,
    windows: Vec<DisplayKey>,
    calls: Vec<Call>,
}

impl Fake {
    fn new() -> Self {
        let place = Placement {
            position: Vec3::new(0.0, 0.0, -1.0),
            rotation: Quat::IDENTITY,
            width: 0.6,
        };
        Self {
            pick_for: [None, None],
            menu: Some(Placement {
                position: Vec3::new(0.0, 0.0, -0.5),
                rotation: Quat::IDENTITY,
                width: panel_width(false),
            }),
            wrist: None,
            places: [(Thing::Menu, place), (Thing::Display(monitor(1)), place)]
                .into_iter()
                .collect(),
            held: Vec::new(),
            resizing: None,
            windows: Vec::new(),
            calls: Vec::new(),
        }
    }

    fn take(&mut self) -> Vec<Call> {
        std::mem::take(&mut self.calls)
    }
}

impl Scene for Fake {
    fn pick(&self, ray: Ray, _attach: Option<DisplayKey>) -> Option<Target> {
        self.pick_for[usize::from(ray.origin.x > 0.0)]
    }
    fn shows(&self, _id: DisplayKey) -> bool {
        true
    }
    fn is_window(&self, id: DisplayKey) -> bool {
        self.windows.contains(&id)
    }
    fn menu(&self) -> Option<Placement> {
        self.menu
    }
    fn large_menu(&self) -> bool {
        false
    }
    fn left_handed(&self) -> bool {
        false
    }
    fn wrist(&self) -> Option<Placement> {
        self.wrist
    }
    fn place(&self, thing: Thing) -> Option<Placement> {
        self.places.get(&thing).copied()
    }
    fn holds(&self, thing: Thing) -> bool {
        self.held.contains(&thing)
    }
    fn resizing(&self) -> Option<DisplayKey> {
        self.resizing
    }
    fn palm(&mut self, place: Option<Placement>) {
        self.wrist = place;
    }
    fn press_menu(&mut self, _now: Instant, action: Action) {
        self.calls.push(Call::PressMenu(action));
    }
    fn press(&mut self, _now: Instant, action: Action) {
        self.calls.push(Call::Press(action));
    }
    fn toggle_menu(&mut self) {
        self.calls.push(Call::Toggle);
    }
    fn align(&mut self, _now: Instant) {
        self.calls.push(Call::Align);
    }
    fn move_menu(&mut self, _position: Vec3, _head: Vec3) {
        self.calls.push(Call::MoveMenu);
    }
    fn hold(&mut self, thing: Thing) -> Option<Placement> {
        let at = self.place(thing)?;
        self.held.push(thing);
        self.calls.push(Call::Hold(thing));
        Some(at)
    }
    fn drag(&mut self, thing: Thing, place: Placement, _head: Vec3) {
        self.places.insert(thing, place);
        self.calls.push(Call::Drag(thing));
    }
    fn let_go(&mut self, _now: Instant, thing: Thing) -> bool {
        self.held.retain(|t| *t != thing);
        self.resizing = None;
        self.calls.push(Call::LetGo(thing));
        false
    }
    fn resize_start(&mut self, id: DisplayKey) -> Option<f32> {
        self.resizing = Some(id);
        self.calls.push(Call::ResizeStart(id));
        Some(self.place(Thing::Display(id))?.width)
    }
    fn resize(&mut self, width: f32) {
        self.calls.push(Call::Resize(width));
    }
    fn resize_end(&mut self) {
        self.resizing = None;
        self.calls.push(Call::ResizeEnd);
    }
    fn cancel_holds(&mut self) {
        self.held.clear();
        self.resizing = None;
        self.calls.push(Call::Cancel);
    }
    fn haptic(&mut self, hand: usize) {
        self.calls.push(Call::Haptic(hand));
    }
}

/// The menus' pictures: every press of the main menu is on `main`.
struct Pictures {
    main: Option<Action>,
    palm: Option<Action>,
}

impl Menus for Pictures {
    fn hover(&mut self, _menu: Menu, _at: Option<(f32, f32)>) {}
    fn press(&mut self, menu: Menu, _at: (f32, f32)) -> Option<Action> {
        match menu {
            Menu::Main => self.main,
            Menu::Palm => self.palm,
        }
    }
}

fn hit(at: Vec3) -> Hit {
    Hit {
        t: 0.5,
        local: Vec2::ZERO,
        point: at,
        normal: Vec3::Z,
    }
}

fn on(thing: Thing) -> Option<Target> {
    Some(Target {
        thing,
        hit: hit(Vec3::new(0.0, 0.0, -0.5)),
    })
}

/// The right hand's ray, a controller's (`hand: false`) or a tracked hand's.
fn right(select: f32, grab: f32, hand: bool, previous: HandState) -> HandState {
    let mut s = previous;
    s.ray = Some(Ray {
        origin: Vec3::new(0.2, -0.2, 0.0),
        dir: -Vec3::Z,
    });
    s.select.update(select);
    s.grab.update(grab);
    s.is_hand = hand;
    s
}

/// A frame's input with the right hand as `hand` and the left one away.
fn frame(hand: HandState) -> FrameInput {
    FrameInput {
        head: Some(Head {
            position: Vec3::ZERO,
            forward: -Vec3::Z,
            right: Vec3::X,
        }),
        dt: 1.0 / 90.0,
        wall: None,
        focused: true,
        hands: [HandState::default(), hand],
        joints: [None, None],
        menu_button: false,
        menu_gesture: false,
        align: false,
        keyboard: None,
    }
}

/// The hands, a scene, the menus' pictures and the clock.
struct Sim {
    hands: Hands,
    scene: Fake,
    menus: Pictures,
    now: Instant,
    right: HandState,
}

impl Sim {
    fn new() -> Self {
        Self {
            hands: Hands::new(),
            scene: Fake::new(),
            menus: Pictures {
                main: Some(Action::Recenter),
                palm: None,
            },
            now: Instant::now(),
            right: HandState::default(),
        }
    }

    /// A frame `after` the last one, the right hand's select and grab at these values.
    fn frame(&mut self, after: Duration, select: f32, grab: f32, hand: bool) -> Vec<Call> {
        self.now += after;
        self.right = right(select, grab, hand, self.right);
        let input = frame(self.right);
        self.hands.buttons(self.now, &input, &mut self.scene);
        self.hands
            .step(self.now, &input, &mut self.scene, &mut self.menus);
        self.scene.take()
    }
}

/// A controller's trigger on a menu button clicks at once, with a vibration and a
/// ring where it pointed; letting go clicks nothing more.
#[test]
fn a_trigger_clicks_the_menu_at_once() {
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    assert_eq!(sim.frame(MS, 0.0, 0.0, false), []);
    assert_eq!(
        sim.frame(11 * MS, 1.0, 0.0, false),
        [Call::Haptic(1), Call::PressMenu(Action::Recenter)]
    );
    assert_eq!(sim.hands.rings(sim.now).count(), 1);
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, false), []);
    // The ring spreads for a moment, then goes.
    sim.frame(TOUCH_RING, 0.0, 0.0, false);
    assert_eq!(sim.hands.rings(sim.now).count(), 0);
}

/// A hand's pinch on a menu button clicks when it opens, not when it closes: held
/// long enough, it takes the menu instead and clicks nothing.
#[test]
fn a_pinch_clicks_when_it_opens_or_drags_when_held() {
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    assert_eq!(sim.frame(MS, 1.0, 0.0, true), []);
    assert_eq!(
        sim.frame(100 * MS, 0.0, 0.0, true),
        [Call::Haptic(1), Call::PressMenu(Action::Recenter)]
    );
    // Held: just short of the hold, nothing; then the menu is taken.
    assert_eq!(sim.frame(MS, 1.0, 0.0, true), []);
    assert_eq!(sim.frame(HOLD_TO_DRAG - 2 * MS, 1.0, 0.0, true), []);
    let took = sim.frame(2 * MS, 1.0, 0.0, true);
    assert_eq!(took, [Call::Hold(Thing::Menu), Call::Haptic(1)]);
    assert_eq!(
        sim.frame(11 * MS, 1.0, 0.0, true),
        [Call::Drag(Thing::Menu)]
    );
    // Let go: no click, the menu is let go of.
    assert_eq!(
        sim.frame(11 * MS, 0.0, 0.0, true),
        [Call::LetGo(Thing::Menu)]
    );
}

/// A controller's grip takes a monitor, moves it every frame and lets it go.
#[test]
fn a_grip_takes_moves_and_lets_go() {
    let mut sim = Sim::new();
    let k = Thing::Display(monitor(1));
    sim.scene.pick_for[1] = on(k);
    assert_eq!(
        sim.frame(MS, 0.0, 1.0, false),
        [Call::Hold(k), Call::Haptic(1)]
    );
    assert_eq!(sim.hands.held().collect::<Vec<_>>(), [k]);
    assert_eq!(sim.frame(11 * MS, 0.0, 1.0, false), [Call::Drag(k)]);
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, false), [Call::LetGo(k)]);
    assert_eq!(sim.hands.held().count(), 0);
}

/// What the room no longer holds (laid out anew, gone) the hand lets go of too: it
/// is not moved any more.
#[test]
fn a_hand_lets_go_of_what_the_room_let_go_of() {
    let mut sim = Sim::new();
    let k = Thing::Display(monitor(1));
    sim.scene.pick_for[1] = on(k);
    sim.frame(MS, 0.0, 1.0, false);
    sim.scene.held.clear();
    assert_eq!(sim.frame(11 * MS, 0.0, 1.0, false), []);
    assert_eq!(sim.hands.held().count(), 0);
}

/// The system menu over the app: what the hands held is let go where it is, and a
/// pinch waiting on a menu button does not click later.
#[test]
fn cancelling_lets_go_and_forgets_the_pinch() {
    let mut sim = Sim::new();
    let k = Thing::Display(monitor(1));
    sim.scene.pick_for[1] = on(k);
    assert!(!sim.hands.busy());
    sim.frame(MS, 0.0, 1.0, false);
    assert!(sim.hands.busy());
    sim.hands.cancel_all(&mut sim.scene);
    assert!(!sim.hands.busy());
    assert_eq!(sim.scene.take(), [Call::Cancel]);
    assert_eq!(sim.frame(11 * MS, 0.0, 1.0, false), []);
    // A pinch on a button, then the system menu, then the pinch opens elsewhere.
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    sim.frame(MS, 1.0, 0.0, true);
    sim.hands.cancel_all(&mut sim.scene);
    sim.scene.take();
    sim.scene.pick_for[1] = None;
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, true), []);
}

/// The controller put down mid-drag and the hand taking over (its grip never seen let
/// go): what it held is let go at once, as letting go would, not carried on by the
/// hand. The other way round too: a pinch held on a monitor, then a controller.
#[test]
fn controllers_to_hands_mid_drag_let_go() {
    let mut sim = Sim::new();
    let k = Thing::Display(monitor(1));
    sim.scene.pick_for[1] = on(k);
    sim.frame(MS, 0.0, 1.0, false);
    assert_eq!(sim.frame(11 * MS, 0.0, 1.0, false), [Call::Drag(k)]);
    // The runtime's input for the hand starts afresh: no grip, no pinch.
    sim.right = HandState::default();
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, true), [Call::LetGo(k)]);
    assert_eq!(sim.hands.held().count(), 0);
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, true), []);

    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(k);
    sim.frame(MS, 1.0, 0.0, true);
    sim.frame(HOLD_TO_DRAG + MS, 1.0, 0.0, true);
    assert_eq!(sim.hands.held().collect::<Vec<_>>(), [k]);
    sim.right = HandState::default();
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, false), [Call::LetGo(k)]);
    assert_eq!(sim.hands.held().count(), 0);
}

/// A pinch on a menu button when the controller is picked up does not click when the
/// controller's trigger is seen up.
#[test]
fn a_pinch_waiting_to_click_goes_with_the_hand() {
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    sim.frame(MS, 1.0, 0.0, true);
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, false), []);
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, false), []);
}

/// Both hands on one monitor resize it by how far apart they are; the second hand
/// letting go ends it.
#[test]
fn two_hands_resize() {
    let mut sim = Sim::new();
    let id = monitor(1);
    let k = Thing::Display(id);
    sim.scene.pick_for = [on(k), on(k)];
    let ray = |x: f32| {
        Some(Ray {
            origin: Vec3::new(x, -0.2, 0.0),
            dir: -Vec3::Z,
        })
    };
    let mut left = HandState {
        ray: ray(-0.2),
        ..HandState::default()
    };
    let mut right = HandState {
        ray: ray(0.2),
        ..HandState::default()
    };
    let step = |left: HandState, right: HandState, sim: &mut Sim| {
        sim.now += 11 * MS;
        let input = FrameInput {
            hands: [left, right],
            ..frame(right)
        };
        sim.hands.buttons(sim.now, &input, &mut sim.scene);
        sim.hands
            .step(sim.now, &input, &mut sim.scene, &mut sim.menus);
        sim.scene.take()
    };
    left.grab.update(1.0);
    assert_eq!(
        step(left, right, &mut sim),
        [Call::Hold(k), Call::Haptic(0)]
    );
    left.grab.update(1.0);
    right.grab.update(1.0);
    assert_eq!(
        step(left, right, &mut sim),
        [Call::Drag(k), Call::ResizeStart(id), Call::Haptic(1)]
    );
    // Twice as far apart: twice as wide.
    left.grab.update(1.0);
    right.grab.update(1.0);
    left.ray = ray(-0.4);
    right.ray = ray(0.4);
    let calls = step(left, right, &mut sim);
    assert!(
        matches!(calls.first(), Some(Call::Resize(w)) if (w - 1.2).abs() < 1e-5),
        "{calls:?}"
    );
    left.grab.update(1.0);
    right.grab.update(0.0);
    assert!(step(left, right, &mut sim).contains(&Call::ResizeEnd));
}

/// Touching the menu with a fingertip: one press as the finger arrives, none from a
/// finger bouncing off it, and the same hand's pinch right after does not click.
#[test]
fn a_touch_presses_once_and_its_pinch_does_not_click() {
    let mut sim = Sim::new();
    let tip_at = |z: f32| {
        let mut joints = [(Vec3::ZERO, 0.01); 26];
        joints[INDEX_TIP].0 = Vec3::new(0.0, 0.0, -0.5 + z);
        joints
    };
    let touch = |z: f32, after: Duration, sim: &mut Sim| {
        sim.now += after;
        let input = FrameInput {
            joints: [None, Some(tip_at(z))],
            ..frame(HandState::default())
        };
        sim.hands.buttons(sim.now, &input, &mut sim.scene);
        sim.hands
            .step(sim.now, &input, &mut sim.scene, &mut sim.menus);
        sim.scene.take()
    };
    assert_eq!(touch(0.05, MS, &mut sim), []);
    assert_eq!(touch(0.02, 11 * MS, &mut sim), []);
    assert_eq!(
        touch(0.0, 11 * MS, &mut sim),
        [Call::PressMenu(Action::Recenter)]
    );
    // Pulled back a little and in again at once: hand tracking jitters, no second
    // press.
    assert_eq!(touch(0.03, 11 * MS, &mut sim), []);
    assert_eq!(touch(0.0, 11 * MS, &mut sim), []);
    // The same hand's pinch now is the hand pulling back.
    sim.scene.pick_for[1] = on(Thing::Menu);
    sim.right = HandState::default();
    assert_eq!(sim.frame(11 * MS, 1.0, 0.0, true), []);
    assert_eq!(sim.frame(11 * MS, 0.0, 0.0, true), []);
    // Later it clicks again.
    sim.frame(AFTER_TOUCH, 0.0, 0.0, true);
    sim.frame(11 * MS, 1.0, 0.0, true);
    assert_eq!(
        sim.frame(11 * MS, 0.0, 0.0, true),
        [Call::Haptic(1), Call::PressMenu(Action::Recenter)]
    );
}

/// The menu button and its hand gesture: one press each, the gesture once while held.
#[test]
fn the_menu_button_and_the_gesture_toggle_once() {
    let mut sim = Sim::new();
    let mut input = frame(HandState::default());
    input.menu_button = true;
    sim.hands.buttons(sim.now, &input, &mut sim.scene);
    assert_eq!(sim.scene.take(), [Call::Toggle]);
    input.menu_button = false;
    input.menu_gesture = true;
    for _ in 0..3 {
        sim.hands.buttons(sim.now, &input, &mut sim.scene);
    }
    assert_eq!(sim.scene.take(), [Call::Toggle]);
    input.align = true;
    sim.hands.buttons(sim.now, &input, &mut sim.scene);
    assert_eq!(sim.scene.take(), [Call::Align]);
}

/// "Attach" under a detached window stays a moment after the pointer left it, so the
/// pointer can move down onto it; a click on it attaches the window.
#[test]
fn attach_lingers_and_attaches() {
    let mut sim = Sim::new();
    let id = monitor(5);
    sim.scene.windows.push(id);
    sim.scene.pick_for[1] = on(Thing::Display(id));
    sim.frame(MS, 0.0, 0.0, false);
    assert_eq!(sim.hands.attach_for(), Some(id));
    sim.scene.pick_for[1] = None;
    sim.frame(ATTACH_LINGER, 0.0, 0.0, false);
    assert_eq!(sim.hands.attach_for(), Some(id));
    sim.scene.pick_for[1] = on(Thing::Attach(id));
    assert!(sim.frame(11 * MS, 0.0, 0.0, false).is_empty());
    assert!(sim.hands.attach_lit());
    assert_eq!(
        sim.frame(11 * MS, 1.0, 0.0, false),
        [Call::Haptic(1), Call::Press(Action::CloseWindow(id))]
    );
    sim.scene.pick_for[1] = None;
    sim.frame(ATTACH_LINGER + MS, 0.0, 0.0, false);
    assert_eq!(sim.hands.attach_for(), None);
}

/// Without a frame the hands do nothing.
#[test]
fn nothing_without_a_frame() {
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    let mut input = FrameInput::hidden(None);
    input.menu_button = true;
    sim.hands.buttons(sim.now, &input, &mut sim.scene);
    sim.hands
        .step(sim.now, &input, &mut sim.scene, &mut sim.menus);
    assert!(sim.scene.take().is_empty());
}

/// The controller holding a monitor with its grip is put down and the hand takes over:
/// the monitor is let go, it does not follow the hand.
#[test]
fn a_controller_put_down_mid_drag_lets_go() {
    let mut sim = Sim::new();
    let id = Thing::Display(monitor(1));
    sim.scene.pick_for[1] = on(id);
    sim.frame(MS, 0.0, 0.0, false);
    let took = sim.frame(11 * MS, 0.0, 1.0, false);
    assert!(took.contains(&Call::Hold(id)), "{took:?}");
    let mut calls = Vec::new();
    for _ in 0..10 {
        calls.extend(sim.frame(11 * MS, 0.0, 0.0, true));
    }
    assert!(calls.contains(&Call::LetGo(id)), "{calls:?}");
}

/// A monitor held with a pinch, then the hands stop being the app's pointers (the
/// setting changed, so their input stops, its pinch never seen opening): the monitor
/// is let go, and a pinch waiting to click does not click later.
#[test]
fn hands_no_longer_pointing_let_go() {
    let mut sim = Sim::new();
    let k = Thing::Display(monitor(1));
    sim.scene.pick_for[1] = on(k);
    sim.frame(MS, 1.0, 0.0, true);
    sim.frame(HOLD_TO_DRAG + MS, 1.0, 0.0, true);
    assert_eq!(sim.hands.held().collect::<Vec<_>>(), [k]);
    // The hand's input as the shell gives it then: no ray, no pinch, nothing let go.
    sim.right = HandState {
        is_hand: true,
        ..HandState::default()
    };
    let input = frame(sim.right);
    sim.now += 11 * MS;
    sim.hands.buttons(sim.now, &input, &mut sim.scene);
    sim.hands
        .step(sim.now, &input, &mut sim.scene, &mut sim.menus);
    assert!(sim.scene.take().contains(&Call::LetGo(k)));
    assert_eq!(sim.hands.held().count(), 0);

    // A pinch on a button, then the hands stop: it never clicks.
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    sim.frame(MS, 1.0, 0.0, true);
    let stopped = HandState {
        is_hand: true,
        ..HandState::default()
    };
    for _ in 0..3 {
        sim.now += 11 * MS;
        let input = frame(stopped);
        sim.hands
            .step(sim.now, &input, &mut sim.scene, &mut sim.menus);
    }
    assert!(!sim.hands.busy(), "nothing waits to click");
}

/// A pinch on a menu button, then the menu closes before the pinch opens (the menu
/// button of the other hand): nothing is clicked.
#[test]
fn a_pinch_on_a_closed_menu_clicks_nothing() {
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    assert_eq!(sim.frame(MS, 1.0, 0.0, true), []);
    sim.scene.menu = None;
    sim.scene.pick_for[1] = None;
    let calls = sim.frame(100 * MS, 0.0, 0.0, true);
    assert!(
        !calls.iter().any(|c| matches!(c, Call::PressMenu(_))),
        "clicked on a closed menu: {calls:?}"
    );
}

/// A pinch on a menu button, then another press on the menu (a touch, the other
/// hand) changes it before the pinch opens: the pinch clicks nothing on what the menu
/// shows now.
#[test]
fn a_pinch_after_another_press_clicks_nothing() {
    let mut sim = Sim::new();
    sim.scene.pick_for[1] = on(Thing::Menu);
    assert_eq!(sim.frame(MS, 1.0, 0.0, true), []);
    // The other hand's menu gesture toggles the menu twice: closed, open again.
    for _ in 0..2 {
        let mut input = frame(sim.right);
        input.menu_gesture = true;
        sim.now += 11 * MS;
        sim.hands.buttons(sim.now, &input, &mut sim.scene);
        let mut input = frame(sim.right);
        input.menu_gesture = false;
        sim.hands.buttons(sim.now, &input, &mut sim.scene);
    }
    assert!(sim.scene.take().contains(&Call::Toggle));
    let calls = sim.frame(100 * MS, 0.0, 0.0, true);
    assert!(
        !calls.iter().any(|c| matches!(c, Call::PressMenu(_))),
        "{calls:?}"
    );
}
