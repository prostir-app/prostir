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

/// When each of `ids` next gets a keyframe (more than `before`), taking what comes
/// for up to `within`.
fn keyframes_at(
    headset: &mut FakeHeadset,
    ids: &[DisplayId],
    before: &[u64],
    within: Duration,
) -> Vec<Instant> {
    let until = Instant::now() + within;
    let mut at: Vec<Option<Instant>> = vec![None; ids.len()];
    while at.iter().any(Option::is_none) {
        let left = until.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "no keyframe of every monitor within {within:?}"
        );
        headset.recv(left.min(Duration::from_millis(5))).unwrap();
        for (i, id) in ids.iter().enumerate() {
            if at[i].is_none() && got(headset, *id).1 > before[i] {
                at[i] = Some(Instant::now());
            }
        }
    }
    at.into_iter().flatten().collect()
}

/// With "Focus" on, the monitor nobody looks at gets 5 frames a second, one every
/// 200 ms; yet a keyframe asked of it comes at once, not at its next turn (resuming
/// opens the gate the same way: see `encoder.rs`, `a_resume_and_a_keyframe_open_the_gate`).
#[test]
fn a_keyframe_of_a_monitor_in_focus_mode_is_not_held() {
    let options = Options {
        fps: 30,
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("gaze-focus", &options);
    let order = headset.view().order();
    let (a, b) = (order[0], order[1]);
    let name_b = headset.view().streams[&b].name.clone();
    let focus = prostir_proto::state::Prefs {
        focus_mode: true,
        ..prostir_proto::state::Prefs::default()
    };
    headset.send(&ClientMsg::Prefs { part: focus }).unwrap();
    look_at(&mut headset, Some(a));
    pc.wait_log(SOON, &format!("{name_b}: 5 frames a second, Focus"));
    headset.take_for(Duration::from_millis(500)).unwrap();

    // Asked a few times, more than 300 ms apart (the PC asks at most that often).
    let mut took = Vec::new();
    for _ in 0..5 {
        let generation = headset.view().streams[&b].generation;
        let before = got(&headset, b).1;
        let asked = Instant::now();
        headset
            .send(&ClientMsg::RequestKeyframe { id: b, generation })
            .unwrap();
        let at = keyframes_at(&mut headset, &[b], &[before], SOON);
        took.push(at[0] - asked);
        headset.take_for(Duration::from_millis(400)).unwrap();
    }
    took.sort();
    eprintln!("keyframes of the monitor at 5 frames a second came after {took:?}");
    assert!(
        took[2] < Duration::from_millis(120),
        "a keyframe waited for its turn: {took:?}"
    );
    pc.quit();
}
