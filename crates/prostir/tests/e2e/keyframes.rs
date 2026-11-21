//! When keyframes come: no encoder makes them on its own any more, so the PC asks for
//! one when a headset does and once when a screen goes still.

use std::time::{Duration, Instant};

use prostir_proto::state::DisplayId;
use prostir_testkit::FakeHeadset;

use crate::pc::Options;
use crate::{SOON, session};

/// Reads what the PC sends for `time`, as a headset does all along.
fn soak(headset: &mut FakeHeadset, time: Duration) {
    let until = Instant::now() + time;
    while Instant::now() < until {
        let _ = headset.recv(Duration::from_millis(20));
    }
}

/// Keyframes of every stream so far.
fn keyframes(headset: &FakeHeadset) -> Vec<(DisplayId, u64)> {
    let view = headset.view();
    view.order()
        .into_iter()
        .map(|id| (id, view.streams[&id].keyframes))
        .collect()
}

/// A screen that went still after new pictures gets one keyframe about 3 s after its
/// last change, with the next frame its source sends again (here every 500 ms, as
/// PipeWire's every 2 s), and no more while it stays still.
#[test]
fn a_still_screen_gets_one_keyframe_then_none() {
    let options = Options {
        env: vec![("PROSTIR_FAKE_KEEPALIVE_MS".into(), "500".into())],
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("still-keyframe", &options);
    soak(&mut headset, Duration::from_secs(1));
    let before = keyframes(&headset);
    // One at a time: a command is a file the fake desktop reads.
    for name in ["Fake-1", "Fake-2"] {
        pc.desktop(&format!("change {name} 5"));
        pc.wait_log(SOON, &format!("fake desktop: {name} changed 5 times"));
    }
    let changed = Instant::now();
    soak(&mut headset, Duration::from_millis(2500));
    assert_eq!(
        keyframes(&headset),
        before,
        "none while it was moving or just still"
    );
    let mut after = keyframes(&headset);
    while Instant::now() < changed + Duration::from_secs(6) && after == before {
        soak(&mut headset, Duration::from_millis(100));
        after = keyframes(&headset);
    }
    soak(&mut headset, Duration::from_secs(1));
    let after = keyframes(&headset);
    for ((id, was), (_, now)) in before.iter().zip(&after) {
        assert_eq!(*now, was + 1, "{id:?}: one keyframe once still");
    }
    soak(&mut headset, Duration::from_secs(4));
    assert_eq!(keyframes(&headset), after, "none more while it stays still");
    pc.quit();
}
