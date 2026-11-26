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
