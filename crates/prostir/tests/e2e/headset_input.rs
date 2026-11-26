//! The headset app's own model (`prostir-client`'s `app`, step 8.4b) turning what a
//! hand does on a monitor into messages for the PC, against the PC app with the fake
//! desktop's input: the model is told the PC's state as the fake headset got it, and
//! what it sends goes through the fake headset. A click lands on its pixel, a scroll up
//! turns the wheel up, a finger touches, and a button held for longer than the PC's
//! 2 s of silence stays held, said again by the model while it is.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use glam::Vec2;
use headset::app::{By, Effect, Event, Model, Start};
use headset::caps::{self, Caps};
use headset::ids::{DisplayKey, PcId};
use headset::interaction::{FrameInput, MouseButton, PcInput, Scene, TouchPhase};
use headset::lang::{self, LangState};
use headset::layout::Desk;
use headset::net::NetEvent;
use headset::platform::store::Store as _;
use headset::session::NetCmd;
use headset::settings::Settings;
use headset::ui::Action;
use prostir_proto::state::{PcState, permission};
use prostir_proto::{ClientMsg, HostMsg};
use prostir_testkit::FakeHeadset;

use crate::input::{display_of_width, in_order, open_input, with_input};
use crate::{SOON, session};

const PC: PcId = PcId([7; 8]);

/// The model in a session with a PC whose state is `pc` (as the fake headset got it).
fn model_of(pc: &PcState) -> Model {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), prostir_proto::TCP_PORT);
    let now = Instant::now();
    let mut m = Model::new(Start {
        now,
        settings: Settings::default(),
        desk: Desk::default(),
        keyboard: None,
        profiles: Vec::new(),
        lang: LangState::new(
            lang::Loader::new(None).load(lang::Lang::FALLBACK),
            Vec::new(),
        ),
        system_lang: None,
        battery: None,
        volume: None,
        caps: Caps::without_extensions(
            "Test headset".into(),
            caps::DECODERS_UNSAID.to_vec(),
            caps::STREAMS_UNSAID,
            None,
        ),
        refresh_hz: None,
        custom_panorama: false,
        update_from: headset::platform::store::NoStore.update_hint(),
        mic_permission: permission::UNKNOWN,
        only: Some(addr),
    });
    let mut fx = Vec::new();
    m.tick(now, &FrameInput::hidden(None));
    m.take_effects(&mut fx);
    // Asked first, no answer (no find secret): then dialed.
    let asked = fx.iter().find_map(|e| match e {
        Effect::Net(NetCmd::Ask { step, .. }) => Some(*step),
        _ => None,
    });
    if let Some(step) = asked {
        m.handle(now, Event::Net(NetEvent::Missed { step }));
        m.take_effects(&mut fx);
    }
    let step = fx
        .iter()
        .find_map(|e| match e {
            Effect::Net(NetCmd::Dial { step, .. }) => Some(*step),
            _ => None,
        })
        .expect("the only address dialed");
    m.handle(now, Event::Net(NetEvent::Dialed { step, addr }));
    m.handle(now, Event::Net(NetEvent::Reached { step, pc: PC, addr }));
    let opened_with = m.headset_state();
    m.handle(
        now,
        Event::Connected {
            pc: PC,
            addr,
            language: None,
            opened_with,
        },
    );
    let parts = [
        pc.displays.clone().map(|part| HostMsg::Displays { part }),
        pc.caps.clone().map(|part| HostMsg::HostCaps { part }),
        pc.access.map(|part| HostMsg::Access { part }),
    ];
    for part in parts.into_iter().flatten() {
        m.handle(now, Event::Net(NetEvent::Part(PC, part)));
    }
    m.handle(now, Event::Menu(Action::Close, By::Elsewhere));
    m.take_effects(&mut fx);
    m
}

/// What the model sent the PC since the last look, sent on by the fake headset; how
/// many.
fn send_on(m: &mut Model, headset: &mut FakeHeadset) -> usize {
    let mut fx = Vec::new();
    m.take_effects(&mut fx);
    let mut n = 0;
    for e in fx {
        if let Effect::Send(pc, msg) = e
            && pc == PC
            && matches!(
                msg,
                ClientMsg::MouseTo { .. }
                    | ClientMsg::MouseButton { .. }
                    | ClientMsg::MouseScroll { .. }
                    | ClientMsg::Touch { .. }
            )
        {
            headset.send(&msg).unwrap();
            n += 1;
        }
    }
    n
}

/// A point across a display's picture that the wire says as `u`, `v`.
fn at(u: u16, v: u16) -> Vec2 {
    let f = |n: u16| (f32::from(n) + 0.5) / 65535.0;
    Vec2::new(f(u), f(v))
}

#[test]
fn the_headsets_clicks_the_wheel_and_touches_reach_the_desktop() {
    let (_dir, mut pc, _keys, mut headset) = session("headset-input", &with_input("touch"));
    let one = display_of_width(&headset, 640);
    let two = display_of_width(&headset, 480);
    let mut m = model_of(&headset.view().pc);
    open_input(&mut pc, &mut headset, one);
    let (one, two) = (DisplayKey::new(PC, one), DisplayKey::new(PC, two));

    // 100 of 640 pixels, 50 of 360; a notch up; a finger across Fake-2.
    let now = Instant::now();
    let click = at(10_256, 9_128);
    for input in [
        PcInput::Button {
            id: one,
            uv: click,
            button: MouseButton::Left,
            down: true,
        },
        PcInput::Button {
            id: one,
            uv: click,
            button: MouseButton::Left,
            down: false,
        },
        PcInput::Scroll {
            id: one,
            dx: 0,
            dy: 120,
        },
        PcInput::Touch {
            id: two,
            slot: 0,
            uv: Vec2::ONE,
            phase: TouchPhase::Down,
        },
        PcInput::Touch {
            id: two,
            slot: 0,
            uv: Vec2::new(0.0, 1.0),
            phase: TouchPhase::Move,
        },
        PcInput::Touch {
            id: two,
            slot: 0,
            uv: Vec2::new(0.0, 1.0),
            phase: TouchPhase::Up,
        },
    ] {
        Scene::pc(&mut m, now, input);
    }
    assert_eq!(send_on(&mut m, &mut headset), 6);
    in_order(
        &mut pc,
        &[
            "pointer to Fake-1 at 100,50",
            "button Left down",
            "button Left up",
            "scroll 0,-120",
            "touch 0 Down on Fake-2 at 479,271",
            "touch 0 Move on Fake-2 at 0,271",
            "touch 0 Up on Fake-2 at 0,271",
        ],
    );

    // Held for 3 s, past the PC's 2 s of silence: the model says where it is again,
    // so the PC keeps it held until it is let go.
    Scene::pc(
        &mut m,
        Instant::now(),
        PcInput::Button {
            id: one,
            uv: click,
            button: MouseButton::Right,
            down: true,
        },
    );
    send_on(&mut m, &mut headset);
    pc.wait_log(SOON, "fake desktop: button Right down");
    let until = Instant::now() + Duration::from_secs(3);
    let mut again = 0;
    while Instant::now() < until {
        m.tick(Instant::now(), &FrameInput::hidden(None));
        again += send_on(&mut m, &mut headset);
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
    assert!(again >= 4, "said again {again} times");
    assert_eq!(pc.count_log("fake desktop: button Right up"), 0, "held");
    Scene::pc(
        &mut m,
        Instant::now(),
        PcInput::Button {
            id: one,
            uv: click,
            button: MouseButton::Right,
            down: false,
        },
    );
    send_on(&mut m, &mut headset);
    pc.wait_log(SOON, "fake desktop: button Right up");
    pc.quit();
}
