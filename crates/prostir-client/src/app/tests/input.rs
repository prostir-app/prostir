//! Step 8.4b: what a hand does on a PC's monitor as messages to that PC (see
//! `app::pc_input`).

use glam::Vec2;
use prostir_proto::input::{Button, TouchPhase as Phase};

use super::*;
use crate::interaction::{MouseButton, PcInput, Scene, TouchPhase};

impl Sim {
    /// The PC moves the mouse (feature 1) and takes the headset's buttons, wheel and
    /// fingers (feature 15), this headset with every right.
    fn takes_input(&mut self) {
        self.host_can(vec![caps::pc::MOVE_MOUSE, caps::pc::INPUT]);
        self.access(state::Access::MAIN);
    }

    fn access(&mut self, part: state::Access) {
        self.net(NetEvent::Part(PC, HostMsg::Access { part }));
    }
}

/// The input's messages among `fx` (`MouseTo`, `MouseButton`, `MouseScroll`, `Touch`).
fn input_sent(fx: &[Effect]) -> Vec<ClientMsg> {
    sent(fx)
        .into_iter()
        .filter(|m| {
            matches!(
                m,
                ClientMsg::MouseTo { .. }
                    | ClientMsg::MouseButton { .. }
                    | ClientMsg::MouseScroll { .. }
                    | ClientMsg::Touch { .. }
            )
        })
        .cloned()
        .collect()
}

/// What a hand does on a monitor reaches a PC that names feature 15 and gives the
/// headset the input right, as the wire says it: a click at its pixel (the pointer
/// drawn there at once), a scroll up as the wheel turned up (`dy` below 0), a finger by
/// its slot. What stays pressed is said again every half second, and nothing once let
/// go. None of it changes what the menu shows.
#[test]
fn a_hands_input_reaches_a_pc_that_takes_it() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    sim.takes_input();
    let look = crate::cursors::Look {
        w: 2,
        h: 2,
        hot_x: 0,
        hot_y: 0,
        rgba: vec![255; 16],
    };
    sim.net(NetEvent::CursorShape {
        pc: PC,
        shape: 3,
        look,
    });
    sim.net(NetEvent::CursorAt {
        pc: PC,
        display: Some(DisplayId(2)),
        u: 0,
        v: 0,
        shape: 3,
    });
    sim.frames(100 * MS);
    sim.take();
    let rev = sim.m.rev();
    let uv = Vec2::new(0.5, 0.25);
    let now = sim.now;
    let id = k(1);
    Scene::pc(
        &mut sim.m,
        now,
        PcInput::Button {
            id,
            uv,
            button: MouseButton::Left,
            down: true,
        },
    );
    Scene::pc(&mut sim.m, now, PcInput::Scroll { id, dx: 0, dy: 120 });
    Scene::pc(
        &mut sim.m,
        now,
        PcInput::Touch {
            id: k(2),
            slot: 0,
            uv,
            phase: TouchPhase::Down,
        },
    );
    let (u, v) = (32767, 16383);
    assert_eq!(
        input_sent(&sim.take()),
        [
            ClientMsg::MouseButton {
                id: DisplayId(1),
                u,
                v,
                button: Button::Left,
                down: true,
            },
            ClientMsg::MouseScroll {
                id: DisplayId(1),
                dx: 0,
                dy: -120,
            },
            ClientMsg::Touch {
                id: DisplayId(2),
                slot: 0,
                u,
                v,
                phase: Phase::Down,
            },
        ]
    );
    let drawn = sim.m.cursors().of(PC, sim.now).expect("drawn");
    assert_eq!(drawn.id, k(1), "the pointer where it clicked");
    // Held for a second: said again, twice each.
    sim.frames(Duration::from_millis(1050));
    let to = ClientMsg::MouseTo {
        id: DisplayId(1),
        u,
        v,
    };
    let finger = ClientMsg::Touch {
        id: DisplayId(2),
        slot: 0,
        u,
        v,
        phase: Phase::Move,
    };
    assert_eq!(
        input_sent(&sim.take()),
        [to.clone(), finger.clone(), to, finger]
    );
    // Let go: said once, then nothing more.
    let now = sim.now;
    Scene::pc(
        &mut sim.m,
        now,
        PcInput::Button {
            id,
            uv,
            button: MouseButton::Left,
            down: false,
        },
    );
    Scene::pc(
        &mut sim.m,
        now,
        PcInput::Touch {
            id: k(2),
            slot: 0,
            uv,
            phase: TouchPhase::Up,
        },
    );
    assert_eq!(input_sent(&sim.take()).len(), 2);
    sim.frames(Duration::from_secs(2));
    assert_eq!(input_sent(&sim.take()), []);
    assert_eq!(sim.m.rev(), rev, "the menu shows nothing of it");
}

/// A PC that moves the mouse but takes no buttons (no feature 15), or a headset
/// without the input right: the ray still moves the mouse, nothing else goes (as
/// before 8.4b). A press held when the right goes is not said again.
#[test]
fn without_feature_15_or_the_right_only_the_mouse_moves() {
    let uv = Vec2::new(0.1, 0.2);
    let id = k(1);
    let all = |sim: &mut Sim| {
        let now = sim.now;
        for input in [
            PcInput::Move { id, uv },
            PcInput::Button {
                id,
                uv,
                button: MouseButton::Right,
                down: true,
            },
            PcInput::Scroll {
                id,
                dx: 0,
                dy: -120,
            },
            PcInput::Touch {
                id,
                slot: 1,
                uv,
                phase: TouchPhase::Down,
            },
        ] {
            Scene::pc(&mut sim.m, now, input);
        }
        input_sent(&sim.take())
    };
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.host_can(vec![caps::pc::MOVE_MOUSE]);
    sim.access(state::Access::MAIN);
    let only_the_mouse = [ClientMsg::MouseTo {
        id: DisplayId(1),
        u: 6553,
        v: 13107,
    }];
    assert_eq!(all(&mut sim), only_the_mouse, "no feature 15");
    // Feature 15, but a guest without the input right.
    sim.host_can(vec![caps::pc::MOVE_MOUSE, caps::pc::INPUT]);
    sim.access(state::Access {
        role: state::role::GUEST,
        perms: state::perms::ALL & !state::perms::INPUT,
    });
    assert_eq!(all(&mut sim), only_the_mouse, "no input right");
    // The right given: all four; taken away again, the press held is not said again.
    sim.access(state::Access::MAIN);
    assert_eq!(all(&mut sim).len(), 4);
    sim.access(state::Access {
        role: state::role::GUEST,
        perms: 0,
    });
    sim.frames(Duration::from_secs(1));
    assert_eq!(input_sent(&sim.take()), [], "not said again");
}

/// A PC that takes input from its desktop but does not give this headset the right:
/// the first press on its monitor says where the right is turned on, once; given and
/// taken again, it is said again. A PC that cannot take input at all says nothing.
#[test]
fn a_press_without_the_right_says_where_to_turn_it_on() {
    let press = |sim: &mut Sim| {
        let now = sim.now;
        for down in [true, false] {
            let input = PcInput::Button {
                id: k(1),
                uv: Vec2::new(0.5, 0.5),
                button: MouseButton::Left,
                down,
            };
            Scene::pc(&mut sim.m, now, input);
        }
        let sent = input_sent(&sim.take());
        let toast = sim.m.toast().map(|t| (t.text.clone(), t.n));
        (sent, toast)
    };
    let no_right = state::Access {
        role: state::role::MAIN,
        perms: state::perms::ALL & !(state::perms::INPUT | state::perms::CLIPBOARD),
    };
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::Close);
    sim.host_can(Vec::new());
    sim.access(no_right);
    assert_eq!(press(&mut sim).1, None, "a PC without input says nothing");
    sim.host_can(vec![caps::pc::MOVE_MOUSE]);
    let off = sim.m.texts().t("headset-pc-input-off").to_owned();
    let (sent, first) = press(&mut sim);
    assert_eq!(sent, [], "no click sent");
    assert_eq!(first.as_ref().map(|t| &t.0), Some(&off));
    assert_eq!(press(&mut sim).1, first, "once");
    // Given: the click goes; taken again: said again.
    sim.host_can(vec![caps::pc::MOVE_MOUSE, caps::pc::INPUT]);
    sim.access(state::Access::MAIN);
    let (sent, toast) = press(&mut sim);
    let clicks = sent
        .iter()
        .filter(|m| matches!(m, ClientMsg::MouseButton { .. }));
    assert_eq!(clicks.count(), 2, "{sent:?}");
    assert_eq!(toast, first);
    sim.host_can(vec![caps::pc::MOVE_MOUSE]);
    sim.access(no_right);
    let (_, again) = press(&mut sim);
    assert!(again.is_some_and(|t| t.0 == off && t.1 != first.unwrap().1));
}

/// Step 8's hands (the preview build's trial): a controller's trigger on a monitor
/// presses the PC's left button at once and holds it, said again while held; the
/// system menu over the app, the controller lost, or a hand taking its place let go
/// of it on the PC.
#[cfg(feature = "preview")]
#[test]
fn a_trigger_held_on_a_monitor_is_let_go_on_the_pc() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    sim.takes_input();
    sim.menu(Action::DevMonitors(true));
    sim.take();
    let at = sim.m.world().shown(k(1)).expect("drawn").position;
    let origin = Vec3::new(0.0, -0.3, 0.0);
    let ray = Some(Ray {
        origin,
        dir: (at - origin).normalize(),
    });
    let mut hands = Hands::new();
    let mut right = HandState {
        ray,
        ..HandState::default()
    };
    let step = |sim: &mut Sim, hands: &mut Hands, right: &mut HandState, pull: f32| {
        sim.now += 11 * MS;
        right.select.update(pull);
        let input = frame(*right);
        sim.m.tick(sim.now, &input);
        hands.buttons(sim.now, &input, &mut sim.m);
        hands.step(sim.now, &input, &mut sim.m, &mut Pictures(None));
        sim.m.animate(sim.now, &input);
        sim.check_view();
    };
    let buttons = |fx: &[Effect]| -> Vec<bool> {
        input_sent(fx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::MouseButton {
                    id: DisplayId(1),
                    button: Button::Left,
                    down,
                    ..
                } => Some(down),
                _ => None,
            })
            .collect()
    };
    for _ in 0..3 {
        step(&mut sim, &mut hands, &mut right, 0.0);
    }
    sim.take();
    // Pulled and held for a second: down at once, its place said again.
    for _ in 0..100 {
        step(&mut sim, &mut hands, &mut right, 1.0);
    }
    let fx = sim.take();
    assert_eq!(buttons(&fx), [true]);
    let tos = input_sent(&fx)
        .iter()
        .filter(|m| matches!(m, ClientMsg::MouseTo { .. }))
        .count();
    assert!(tos >= 2, "said again while held: {tos}");
    // The system menu.
    hands.cancel_all(&mut sim.m);
    assert_eq!(buttons(&sim.take()), [false], "the system menu");
    for _ in 0..3 {
        step(&mut sim, &mut hands, &mut right, 0.0);
    }
    // The controller lost (its ray gone longer than a moment).
    step(&mut sim, &mut hands, &mut right, 1.0);
    assert_eq!(buttons(&sim.take()), [true]);
    right.ray = None;
    for _ in 0..30 {
        step(&mut sim, &mut hands, &mut right, 1.0);
    }
    assert_eq!(buttons(&sim.take()), [false], "the controller lost");
    right.ray = ray;
    for _ in 0..3 {
        step(&mut sim, &mut hands, &mut right, 0.0);
    }
    // A hand in its place.
    step(&mut sim, &mut hands, &mut right, 1.0);
    assert_eq!(buttons(&sim.take()), [true]);
    right.is_hand = true;
    step(&mut sim, &mut hands, &mut right, 1.0);
    assert_eq!(buttons(&sim.take()), [false], "a hand in its place");
    sim.frames(Duration::from_secs(1));
    assert_eq!(input_sent(&sim.take()), [], "nothing held");
}

/// The monitors `MouseTo` sent the mouse to among `fx`.
fn mouse_tos(fx: &[Effect]) -> Vec<u32> {
    input_sent(fx)
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::MouseTo { id, .. } => Some(id.0),
            _ => None,
        })
        .collect()
}

/// With the mouse following the gaze: a button or a finger held still on monitor 1
/// (nothing new sent for seconds) keeps the mouse there however long monitor 2 is
/// looked at; once let go, the gaze takes the mouse to monitor 2 again.
#[test]
fn the_gaze_leaves_the_mouse_alone_while_something_is_held() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    sim.takes_input();
    sim.menu(Action::GazeMouse(true));
    let (one, two) = (sim.towards(1), sim.towards(2));
    sim.look(one, Duration::from_secs(2));
    sim.take();
    let uv = Vec2::new(0.5, 0.5);
    let presses = [
        (
            PcInput::Button {
                id: k(1),
                uv,
                button: MouseButton::Left,
                down: true,
            },
            PcInput::Button {
                id: k(1),
                uv,
                button: MouseButton::Left,
                down: false,
            },
        ),
        (
            PcInput::Touch {
                id: k(1),
                slot: 0,
                uv,
                phase: TouchPhase::Down,
            },
            PcInput::Touch {
                id: k(1),
                slot: 0,
                uv,
                phase: TouchPhase::Up,
            },
        ),
    ];
    for (down, up) in presses {
        let now = sim.now;
        Scene::pc(&mut sim.m, now, down);
        sim.look(two, Duration::from_secs(4));
        let tos = mouse_tos(&sim.take());
        assert!(tos.iter().all(|&d| d == 1), "{down:?}: {tos:?}");
        let now = sim.now;
        Scene::pc(&mut sim.m, now, up);
        sim.look(two, Duration::from_secs(3));
        assert_eq!(mouse_tos(&sim.take()), [2], "{up:?}: the gaze again");
        sim.look(one, Duration::from_secs(3));
        sim.take();
    }
}

/// A right hand pointing with its palm at `palm`, fingers forward (as the scripts of
/// `interaction::record` pose it): the index fingertip 10 cm in front of the palm and
/// a little to the right, the other fingers curled.
#[cfg(feature = "preview")]
fn pointing(palm: Vec3) -> crate::interaction::Joints {
    use crate::interaction::{INDEX_TIP, signals};
    let mut j = [(palm, 0.01); 26];
    j[signals::WRIST].0 = palm + Vec3::new(0.0, 0.0, 0.06);
    j[signals::MIDDLE_PROXIMAL].0 = palm + Vec3::new(0.0, 0.0, -0.03);
    for (tip, dx) in [(15, -0.01), (20, -0.025), (25, -0.04)] {
        j[tip].0 = palm + Vec3::new(dx, -0.02, -0.025);
    }
    j[INDEX_TIP].0 = palm + TIP;
    j[signals::THUMB_TIP].0 = palm + Vec3::new(0.06, 0.0, -0.04);
    j
}

/// Where the index fingertip of [`pointing`] is from its palm.
#[cfg(feature = "preview")]
const TIP: Vec3 = Vec3::new(0.025, 0.0, -0.1);

/// Step 8's hands (the preview build's trial, "Hands: control"): a fingertip on a
/// monitor touches the PC by its hand's slot: down as it crosses the surface, moves as
/// it slides, up as it comes back; touching again and then lost for longer than a
/// moment, the touch is cancelled, never a tap. Each as the wire says it, to a PC that
/// takes input.
#[cfg(feature = "preview")]
#[test]
fn a_fingertip_touches_a_pc_and_a_lost_hand_cancels() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    sim.takes_input();
    sim.menu(Action::DevMonitors(true));
    sim.menu(Action::Hands(crate::settings::HandMode::Control));
    sim.take();
    let p = *sim.m.world().shown(k(1)).expect("drawn");
    let (normal, side) = (p.rotation * Vec3::Z, p.rotation * Vec3::X);
    let at = |x: f32, z: f32| p.position + side * x + normal * z;
    let mut hands = Hands::new();
    let step = |sim: &mut Sim, hands: &mut Hands, tip: Option<Vec3>| {
        sim.now += 11 * MS;
        let mut input = frame(HandState {
            is_hand: true,
            ..HandState::default()
        });
        input.joints[1] = tip.map(|t| pointing(t - TIP));
        sim.m.tick(sim.now, &input);
        hands.buttons(sim.now, &input, &mut sim.m);
        hands.step(sim.now, &input, &mut sim.m, &mut Pictures(None));
        sim.m.animate(sim.now, &input);
        sim.check_view();
    };
    let go = |sim: &mut Sim, hands: &mut Hands, n: u32, from: Vec3, to: Vec3| {
        for i in 1..=n {
            step(sim, hands, Some(from.lerp(to, i as f32 / n as f32)));
        }
    };
    let phases = |fx: &[Effect]| -> Vec<Phase> {
        input_sent(fx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Touch {
                    id: DisplayId(1),
                    slot: 1,
                    phase,
                    ..
                } => Some(phase),
                _ => None,
            })
            .collect()
    };
    go(&mut sim, &mut hands, 10, at(0.0, 0.12), at(0.0, 0.01));
    assert_eq!(phases(&sim.take()), [], "a hover");
    go(&mut sim, &mut hands, 3, at(0.0, 0.01), at(0.0, -0.004));
    assert_eq!(phases(&sim.take()), [Phase::Down]);
    go(&mut sim, &mut hands, 12, at(0.0, -0.004), at(0.06, -0.004));
    let slid = phases(&sim.take());
    assert!(
        slid.len() >= 3 && slid.iter().all(|&p| p == Phase::Move),
        "{slid:?}"
    );
    go(&mut sim, &mut hands, 6, at(0.06, -0.004), at(0.06, 0.08));
    assert_eq!(phases(&sim.take()), [Phase::Up]);

    sim.frames(Duration::from_secs(1));
    go(&mut sim, &mut hands, 10, at(0.0, 0.12), at(0.0, -0.004));
    assert_eq!(phases(&sim.take()), [Phase::Down]);
    for _ in 0..30 {
        step(&mut sim, &mut hands, None);
    }
    assert_eq!(phases(&sim.take()), [Phase::Cancel], "the hand lost");
    sim.frames(Duration::from_secs(1));
    assert_eq!(phases(&sim.take()), [], "nothing held");
}
