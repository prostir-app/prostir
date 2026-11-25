//! The mouse pointer apart from the pictures (`PROTOCOL.md`, "The mouse pointer"): the
//! fake desktop tells it (`PROSTIR_FAKE_CURSOR=1`) and its commands move it. A headset
//! that draws it gets its look, then where it is, across the picture of the display it
//! is on; one that does not, alone or beside the other, gets none of it, and the
//! pointer goes back into the pictures for all.

use std::time::Duration;

use prostir_proto::state::{DisplayId, HeadsetState, caps};
use prostir_testkit::{CursorAt, FakeHeadset, Keys, View};

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START};

/// What the fake headset says of itself, drawing the pointer itself.
fn drawing() -> HeadsetState {
    let mut state = FakeHeadset::state();
    if let Some(c) = state.caps.as_mut() {
        c.features.push(caps::headset::CURSOR);
    }
    state
}

/// Connects a headset that says `state` of itself, and waits for its monitors.
fn showing(pc: &Pc, keys: &mut Keys, state: &HeadsetState) -> FakeHeadset {
    let mut headset = FakeHeadset::connect_saying(pc.addr, keys, state)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset
}

/// The PC with two small monitors (`Fake-1` 640x360, `Fake-2` 480x272) whose desktop
/// tells the pointer apart, taking two headsets at once; the headsets `first` and
/// `second` it trusts.
fn start(name: &str) -> (TestDir, Pc, Keys, Keys) {
    let dir = TestDir::new(name);
    dir.save_screens("next 200\nrow\nscreen 0 640x360\nscreen 1 480x272\n");
    dir.save_settings("mirror off\n");
    let first = dir.pair();
    let second = dir.pair_headset("second-headset");
    let options = Options {
        headsets: 2,
        env: vec![("PROSTIR_FAKE_CURSOR".into(), "1".into())],
        ..Options::default()
    };
    let pc = Pc::start(&dir, &options);
    (dir, pc, first, second)
}

/// The display streaming `w` pixels wide.
fn display_of_width(view: &View, w: u32) -> DisplayId {
    *view
        .streams
        .iter()
        .find(|(_, s)| s.width == w)
        .unwrap_or_else(|| panic!("a stream {w} wide"))
        .0
}

/// The last place the PC said of its pointer.
fn last(view: &View) -> Option<CursorAt> {
    view.cursor_at.last().copied()
}

/// Waits until the PC said its pointer is at `at`.
fn wait_at(headset: &mut FakeHeadset, at: CursorAt, what: &str) {
    headset
        .wait(SOON, what, |v| last(v) == Some(at))
        .unwrap_or_else(|e| panic!("{e:#}: {:?}", headset.view().cursor_at));
}

/// A headset that draws the pointer: the PC names feature 16, leaves the pointer out of
/// the pictures, and sends its look once, then where it is: across the picture of the
/// display it is on, on none of them, hidden. A second headset that does not draw it
/// brings it back into the pictures (the first hears it is nowhere, then nothing more);
/// once it leaves, the first gets it again. A new session gets the look again.
#[test]
fn a_headset_that_draws_the_pointer_gets_its_look_and_place() {
    let (_dir, mut pc, mut first, mut second) = start("cursor");
    let mut headset = showing(&pc, &mut first, &drawing());
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(pc_caps.has(caps::pc::CURSOR), "{pc_caps:?}");
    pc.wait_log(SOON, "fake desktop: the pointer apart from the pictures");
    let one = display_of_width(headset.view(), 640);
    let two = display_of_width(headset.view(), 480);

    pc.desktop("cursor-shape 32 32 4 2 200");
    pc.desktop("cursor-move Fake-1 100 50");
    headset
        .wait(SOON, "the pointer's look", |v| !v.cursor_shapes.is_empty())
        .unwrap();
    // The reverse of `MouseTo`: 100 of 640 pixels, 50 of 360.
    let on_one = CursorAt {
        display: Some(one),
        u: 10_256,
        v: 9_128,
        shape: headset.view().cursor_shapes[0].0,
    };
    wait_at(&mut headset, on_one, "on the first monitor");
    assert_eq!(headset.view().cursor_shapes[0].1, (32, 32));
    // To the other monitor's last pixel, the same look: no look again.
    pc.desktop("cursor-move Fake-2 479 271");
    let on_two = CursorAt {
        display: Some(two),
        u: 65_535,
        v: 65_535,
        ..on_one
    };
    wait_at(&mut headset, on_two, "on the second monitor");
    // On a display not streamed: on none of them.
    pc.desktop("cursor-move eDP-1 10 10");
    let off = CursorAt {
        display: None,
        u: 0,
        v: 0,
        ..on_one
    };
    wait_at(&mut headset, off, "off the monitors");
    pc.desktop("cursor-hide");
    wait_at(&mut headset, CursorAt { shape: 0, ..off }, "hidden");
    pc.desktop("cursor-shape 32 32 4 2 200");
    pc.desktop("cursor-move Fake-1 100 50");
    wait_at(&mut headset, on_one, "shown again");
    assert_eq!(headset.view().cursor_shapes.len(), 1, "the look came once");
    // Another look.
    pc.desktop("cursor-shape 16 24 0 0 90");
    headset
        .wait(SOON, "a second look", |v| v.cursor_shapes.len() == 2)
        .unwrap();
    let second_look = headset.view().cursor_shapes[1].0;
    wait_at(
        &mut headset,
        CursorAt {
            shape: second_look,
            ..on_one
        },
        "in the second look",
    );

    // A headset that does not draw it: back into the pictures for both.
    let mut old = showing(&pc, &mut second, &FakeHeadset::state());
    let nowhere = CursorAt {
        display: None,
        u: 0,
        v: 0,
        shape: 0,
    };
    wait_at(&mut headset, nowhere, "nowhere, in the pictures again");
    pc.wait_log(SOON, "fake desktop: the pointer in the pictures");
    let told = headset.view().cursor_at.len();
    pc.desktop("cursor-move Fake-1 300 200");
    old.take_for(Duration::from_millis(500)).unwrap();
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(
        headset.view().cursor_at.len(),
        told,
        "nothing while in the pictures"
    );
    assert!(old.view().cursor_at.is_empty() && old.view().cursor_shapes.is_empty());

    // It leaves: apart again, the place as it is now, the look it has already.
    old.disconnect();
    let at = CursorAt {
        display: Some(one),
        u: 30_768,
        v: 36_510,
        shape: second_look,
    };
    wait_at(&mut headset, at, "apart again");
    assert_eq!(headset.view().cursor_shapes.len(), 2, "no look again");
    assert!(
        headset.view().broken.is_empty(),
        "{:?}",
        headset.view().broken
    );

    // A new session: the look comes again, before the place.
    headset.disconnect();
    let mut again = showing(&pc, &mut first, &drawing());
    wait_at(&mut again, at, "the place in a new session");
    assert_eq!(again.view().cursor_shapes, [(second_look, (16, 24))]);
    assert!(again.view().broken.is_empty(), "{:?}", again.view().broken);
    pc.quit();
}

/// A headset that does not draw the pointer, alone: the pointer stays in the pictures
/// and nothing of it goes to the headset, however it moves.
#[test]
fn a_headset_that_does_not_draw_the_pointer_gets_none() {
    let (_dir, pc, mut first, _second) = start("cursor-old");
    let mut headset = showing(&pc, &mut first, &FakeHeadset::state());
    pc.desktop("cursor-shape 32 32 4 2 200");
    pc.desktop("cursor-move Fake-1 100 50");
    headset.take_for(Duration::from_secs(1)).unwrap();
    let view = headset.view();
    assert!(view.cursor_at.is_empty() && view.cursor_shapes.is_empty());
    assert!(pc.lines("the pointer apart").is_empty());
    pc.quit();
}
