//! The headset's `Gaze` (step 7.12): the PC names feature 14 (it takes the gaze), what
//! the headset says of the monitor it looks at reaches the PC's log, and the monitor
//! looked at gets every frame while the others get fewer.

use std::time::{Duration, Instant};

use prostir_proto::ClientMsg;
use prostir_proto::state::{DisplayId, caps};
use prostir_testkit::FakeHeadset;

use crate::pc::Options;
use crate::{SOON, session};

/// The headset looks at `display` (none: at none of the monitors).
fn look_at(headset: &mut FakeHeadset, display: Option<DisplayId>) {
    headset
        .send(&ClientMsg::Gaze {
            display,
            u: 32768,
            v: 32768,
        })
        .unwrap();
}

/// Frames and keyframes the headset got of `id` so far.
fn got(headset: &FakeHeadset, id: DisplayId) -> (u64, u64) {
    let s = &headset.view().streams[&id];
    (s.frames, s.keyframes)
}

/// Two monitors whose pictures change 30 times a second (the fake monitors' rate):
/// the one looked at gets every frame, the other half of them; looking at the other
/// one gives it every frame at once, and the first one gets fewer only 2 s after the
/// gaze left it. No keyframe comes of any of it.
#[test]
fn the_monitor_looked_at_gets_every_frame() {
    let options = Options {
        fps: 30,
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("gaze-rate", &options);
    let order = headset.view().order();
    let (a, b) = (order[0], order[1]);
    let name = |id: DisplayId| headset.view().streams[&id].name.clone();
    let (name_a, name_b) = (name(a), name(b));
    look_at(&mut headset, Some(a));
    pc.wait_log(SOON, &format!("{name_b}: half its frames, 15 a second"));
    assert_eq!(
        pc.count_log(&format!("{name_a}: half its frames")),
        0,
        "the one looked at keeps every frame"
    );

    // What came of each over a few seconds, after what was on its way.
    headset.take_for(Duration::from_millis(500)).unwrap();
    let before = (got(&headset, a), got(&headset, b));
    headset.take_for(Duration::from_secs(4)).unwrap();
    let after = (got(&headset, a), got(&headset, b));
    let frames_a = after.0.0 - before.0.0;
    let frames_b = after.1.0 - before.1.0;
    assert!(frames_a >= 40, "{frames_a} frames of the monitor looked at");
    assert!(
        frames_b * 10 >= frames_a * 3 && frames_b * 10 <= frames_a * 7,
        "about half: {frames_b} frames of the other, {frames_a} of the one looked at"
    );
    assert_eq!(
        (after.0.1, after.1.1),
        (before.0.1, before.1.1),
        "no keyframe of a level"
    );

    // Looked at, the other gets every frame at once; the first slows down 2 s later.
    let slowed_a = pc.count_log(&format!("{name_a}: half its frames"));
    let looked = Instant::now();
    look_at(&mut headset, Some(b));
    pc.wait_log(
        SOON,
        &format!("{name_b}: every frame, 30 a second (looked at)"),
    );
    assert!(
        looked.elapsed() < Duration::from_millis(1500),
        "at once: {:?}",
        looked.elapsed()
    );
    pc.wait_log_after(SOON, &format!("{name_a}: half its frames"), slowed_a);
    assert!(
        looked.elapsed() >= Duration::from_millis(1900),
        "only 2 s after the gaze left: {:?}",
        looked.elapsed()
    );

    // Looking at none of the monitors: both slow down, 2 s after.
    let slowed_b = pc.count_log(&format!("{name_b}: half its frames"));
    look_at(&mut headset, None);
    pc.wait_log_after(SOON, &format!("{name_b}: half its frames"), slowed_b);
    // Taken off: every monitor as it was (nothing known of where it looks).
    headset.send(&crate::worn(false)).unwrap();
    pc.wait_log(SOON, &format!("{name_a}: every frame, 30 a second"));
    pc.quit();
}

/// The PC says it takes the gaze, and a `Gaze` of the headset comes to its log, for a
/// monitor and for none.
#[test]
fn the_pc_takes_the_gaze_and_logs_it() {
    // The PC logs it at the debug level: it only shows for what a later step does.
    let options = Options {
        env: vec![("RUST_LOG".into(), "info,prostir::session=debug".into())],
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("gaze", &options);
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(pc_caps.has(caps::pc::GAZE), "{pc_caps:?}");
    // The fake headset says it can tell where the user looks.
    let said = FakeHeadset::state().caps.unwrap();
    assert!(said.has(caps::headset::GAZE));

    let id = headset.view().order()[0];
    headset
        .send(&ClientMsg::Gaze {
            display: Some(id),
            u: 1234,
            v: 54321,
        })
        .unwrap();
    pc.wait_log(
        SOON,
        &format!("gaze: the headset looks at display {} at 1234,54321", id.0),
    );
    headset
        .send(&ClientMsg::Gaze {
            display: None,
            u: 0,
            v: 0,
        })
        .unwrap();
    pc.wait_log(SOON, "gaze: the headset looks at none of the displays");
    pc.quit();
}
