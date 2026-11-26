//! Input from the headset (`PROTOCOL.md`, "Input from the headset"): clicks, the wheel
//! and fingers reach the fake desktop's virtual input (`PROSTIR_FAKE_INPUT`), which
//! writes what it is told to the log, in the pixels of its output. A desktop without
//! touch gets mouse actions; whatever a headset holds is let go when it leaves or
//! falls silent; a headset without the right moves nothing, and a person's no is said
//! once and not asked again.

use std::time::Duration;

use prostir_proto::ClientMsg;
use prostir_proto::input::{Button, TouchPhase};
use prostir_proto::state::{DisplayId, caps};
use prostir_testkit::{FakeHeadset, View};

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START, connect, session};

/// The PC with the fake desktop's input, with touch (`touch`) or without (`mouse`).
fn with_input(kind: &str) -> Options {
    Options {
        env: vec![("PROSTIR_FAKE_INPUT".into(), kind.into())],
        ..Options::default()
    }
}

/// The display streaming `w` pixels wide (`Fake-1` is 640, `Fake-2` 480).
fn display_of_width(headset: &FakeHeadset, w: u32) -> DisplayId {
    *headset
        .view()
        .streams
        .iter()
        .find(|(_, s)| s.width == w)
        .unwrap_or_else(|| panic!("a stream {w} wide"))
        .0
}

fn button(id: DisplayId, u: u16, v: u16, button: Button, down: bool) -> ClientMsg {
    ClientMsg::MouseButton {
        id,
        u,
        v,
        button,
        down,
    }
}

fn touch(id: DisplayId, slot: u8, u: u16, v: u16, phase: TouchPhase) -> ClientMsg {
    ClientMsg::Touch {
        id,
        slot,
        u,
        v,
        phase,
    }
}

/// The first event opens the desktop's devices (and goes nowhere): sent, and waited
/// for.
fn open_input(pc: &mut Pc, headset: &mut FakeHeadset, id: DisplayId) {
    headset
        .send(&ClientMsg::MouseTo { id, u: 0, v: 0 })
        .unwrap();
    pc.wait_log(SOON, "input: the desktop's virtual devices are open");
}

/// Waits for each of `lines`, in this order, after the `skip` lines of the first.
fn in_order(pc: &mut Pc, lines: &[&str]) {
    let mut last = 0;
    for line in lines {
        let found = pc.wait_log(SOON, &format!("fake desktop: {line}"));
        let all = pc.lines("fake desktop: ");
        let at = all
            .iter()
            .position(|l| *l == found)
            .unwrap_or_else(|| panic!("{line} in the log"));
        assert!(at >= last, "{line} after the one before: {all:#?}");
        last = at;
    }
}

/// A headset with the right: the PC names features 1 and 15; a click lands on the
/// pixel `MouseTo` would put the mouse on, the wheel turns in 1/120 of a notch, and a
/// finger is a touch on the desktop. A button held when the headset leaves is let go.
#[test]
fn clicks_the_wheel_and_touches_reach_the_desktop() {
    let (_dir, mut pc, _keys, mut headset) = session("input", &with_input("touch"));
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(pc_caps.has(caps::pc::MOVE_MOUSE), "{pc_caps:?}");
    assert!(pc_caps.has(caps::pc::INPUT), "{pc_caps:?}");
    let one = display_of_width(&headset, 640);
    let two = display_of_width(&headset, 480);
    open_input(&mut pc, &mut headset, one);

    // 100 of 640 pixels, 50 of 360 (as in the pointer's tests, the other way round).
    for msg in [
        button(one, 10_256, 9_128, Button::Left, true),
        button(one, 10_256, 9_128, Button::Left, false),
        ClientMsg::MouseScroll {
            id: one,
            dx: 0,
            dy: 60,
        },
        touch(two, 0, 65_535, 65_535, TouchPhase::Down),
        touch(two, 0, 0, 65_535, TouchPhase::Move),
        touch(two, 0, 0, 65_535, TouchPhase::Up),
    ] {
        headset.send(&msg).unwrap();
    }
    in_order(
        &mut pc,
        &[
            "pointer to Fake-1 at 100,50",
            "button Left down",
            "button Left up",
            "scroll 0,60",
            "touch 0 Down on Fake-2 at 479,271",
            "touch 0 Move on Fake-2 at 0,271",
            "touch 0 Up on Fake-2 at 0,271",
        ],
    );

    // Gone with the right button down: the PC lets go of it.
    headset
        .send(&button(one, 0, 0, Button::Right, true))
        .unwrap();
    pc.wait_log(SOON, "fake desktop: button Right down");
    headset.disconnect();
    pc.wait_log(SOON, "fake desktop: button Right up");
    pc.quit();
}

/// A desktop without touch: a tap is a left click where the finger touched, a finger
/// moved at once scrolls with it; a button held without a word for 2 s is let go.
#[test]
fn a_desktop_without_touch_gets_the_mouse() {
    let (_dir, mut pc, _keys, mut headset) = session("input-mouse", &with_input("mouse"));
    let one = display_of_width(&headset, 640);
    open_input(&mut pc, &mut headset, one);
    headset
        .send(&touch(one, 0, 10_256, 9_128, TouchPhase::Down))
        .unwrap();
    headset
        .send(&touch(one, 0, 10_256, 9_128, TouchPhase::Up))
        .unwrap();
    in_order(
        &mut pc,
        &[
            "pointer to Fake-1 at 100,50",
            "button Left down",
            "button Left up",
        ],
    );
    assert_eq!(pc.count_log("fake desktop: touch"), 0, "no touches");

    // 100 pixels up the monitor (of 360): the page goes down, two notches.
    headset
        .send(&touch(one, 0, 32_768, 32_768, TouchPhase::Down))
        .unwrap();
    headset
        .send(&touch(one, 0, 32_768, 14_564, TouchPhase::Move))
        .unwrap();
    pc.wait_log(SOON, "fake desktop: scroll 0,239");
    headset
        .send(&touch(one, 0, 32_768, 14_564, TouchPhase::Cancel))
        .unwrap();

    headset
        .send(&button(one, 0, 0, Button::Middle, true))
        .unwrap();
    pc.wait_log(SOON, "fake desktop: button Middle down");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(pc.count_log("fake desktop: button Middle up"), 0, "not yet");
    pc.wait_log(SOON, "fake desktop: button Middle up");
    pc.wait_log(SOON, "input: nothing from session");
    pc.quit();
}

/// A headset without the input right: the PC names no feature 15, and what it sends
/// reaches nothing (the devices are not even opened).
#[test]
fn a_headset_without_the_right_moves_nothing() {
    let dir = TestDir::new("input-no-right");
    dir.save_screens("next 200\nrow\nscreen 0 640x360\nscreen 1 480x272\n");
    let mut keys = dir.pair();
    let key = prostir_proto::secure::to_hex(&keys.identity.public);
    dir.save_settings(&format!("mirror off\naccess {key} main\n"));
    let mut pc = Pc::start(&dir, &with_input("touch"));
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(!pc_caps.has(caps::pc::INPUT), "{pc_caps:?}");
    let one = display_of_width(&headset, 640);
    headset
        .send(&button(one, 0, 0, Button::Left, true))
        .unwrap();
    headset
        .send(&ClientMsg::MouseTo {
            id: one,
            u: 0,
            v: 0,
        })
        .unwrap();
    pc.wait_log(SOON, "the headset has no right to move the mouse");
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(pc.count_log("virtual input open"), 0);
    pc.quit();
}

/// The person says no: the headset hears it once, and the PC does not ask again.
#[test]
fn a_refusal_is_said_once_and_not_asked_again() {
    let (_dir, mut pc, _keys, mut headset) = session("input-refused", &with_input("touch"));
    let one = display_of_width(&headset, 640);
    pc.desktop("input-refuse");
    pc.wait_log(SOON, "the person will not allow the virtual input");
    headset
        .send(&ClientMsg::MouseTo {
            id: one,
            u: 0,
            v: 0,
        })
        .unwrap();
    pc.wait_log(
        SOON,
        "fake desktop: the person did not allow the virtual input",
    );
    headset
        .wait(SOON, "the problem said", |v| {
            v.errors.iter().any(|e| e.contains("not allowed"))
        })
        .unwrap();
    for _ in 0..3 {
        headset
            .send(&button(one, 0, 0, Button::Left, true))
            .unwrap();
        headset.take_for(Duration::from_millis(200)).unwrap();
    }
    assert_eq!(
        pc.count_log("fake desktop: the person did not allow the virtual input"),
        1
    );
    assert_eq!(pc.count_log("virtual input open"), 0);
    pc.quit();
}

/// The desktop closes the devices: what they held goes with them, and the next event
/// (after a pause) opens them again.
#[test]
fn devices_the_desktop_closed_open_again() {
    let (_dir, mut pc, _keys, mut headset) = session("input-closed", &with_input("touch"));
    let one = display_of_width(&headset, 640);
    open_input(&mut pc, &mut headset, one);
    pc.desktop("input-close");
    pc.wait_log(SOON, "closing the virtual input");
    headset
        .send(&ClientMsg::MouseTo {
            id: one,
            u: 0,
            v: 0,
        })
        .unwrap();
    pc.wait_log(SOON, "input: the desktop's virtual devices are gone");
    // Opened again on an event once the pause is over.
    let reopened = std::time::Instant::now() + Duration::from_secs(10);
    while pc.count_log("fake desktop: virtual input open") < 2 {
        assert!(std::time::Instant::now() < reopened, "not opened again");
        headset
            .send(&ClientMsg::MouseTo {
                id: one,
                u: 0,
                v: 0,
            })
            .unwrap();
        headset.take_for(Duration::from_millis(300)).unwrap();
    }
    headset
        .send(&button(one, 0, 0, Button::Left, true))
        .unwrap();
    pc.wait_log(SOON, "fake desktop: button Left down");
    pc.quit();
}

/// A headset taken off while it holds a button: the PC lets go at once, and takes
/// nothing more from it until it is worn again.
#[test]
fn taking_the_headset_off_lets_go() {
    let (_dir, mut pc, _keys, mut headset) = session("input-off", &with_input("touch"));
    let one = display_of_width(&headset, 640);
    open_input(&mut pc, &mut headset, one);
    headset
        .send(&button(one, 0, 0, Button::Left, true))
        .unwrap();
    pc.wait_log(SOON, "fake desktop: button Left down");
    headset.send(&crate::worn(false)).unwrap();
    pc.wait_log(SOON, "fake desktop: button Left up");
    headset
        .send(&button(one, 0, 0, Button::Right, true))
        .unwrap();
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(pc.count_log("fake desktop: button Right"), 0);
    pc.quit();
}

/// A flick whose moves come in one burst (as a busy network gives them): the page
/// goes the way the finger went and stops soon, a few notches, never thousands.
#[test]
fn a_burst_of_moves_scrolls_a_short_way() {
    let (_dir, mut pc, _keys, mut headset) = session("input-burst", &with_input("mouse"));
    let one = display_of_width(&headset, 640);
    open_input(&mut pc, &mut headset, one);
    headset
        .send(&touch(one, 0, 32_768, 32_768, TouchPhase::Down))
        .unwrap();
    // Eight moves of about 4 pixels each, sent together.
    for i in 1..=8u16 {
        headset
            .send(&touch(one, 0, 32_768, 32_768 - 730 * i, TouchPhase::Move))
            .unwrap();
    }
    headset
        .send(&touch(one, 0, 32_768, 32_768 - 730 * 8, TouchPhase::Up))
        .unwrap();
    pc.wait_log(SOON, "fake desktop: scroll 0,");
    headset.take_for(Duration::from_secs(3)).unwrap();
    let total: i32 = pc
        .lines("fake desktop: scroll 0,")
        .iter()
        .filter_map(|l| l.rsplit(',').next()?.trim().parse::<i32>().ok())
        .sum();
    assert!(total > 0, "down, as the finger went up");
    assert!(total < 120 * 20, "{total}: a short way");
    pc.quit();
}
