//! What a session survives and what ends it: Wi-Fi freezing for a moment or for good,
//! a PC going to sleep, a connection cut half way through a frame, a broken packet.

use std::time::{Duration, Instant};

use prostir_testkit::View;

use crate::pc::Options;
use crate::pc::{Pc, TestDir};
use crate::{SOON, START, TWO_MONITORS, connect, session};

/// A busy screen, so frames keep coming and a stall loses some.
fn busy() -> Options {
    Options {
        fps: 15,
        noise: true,
        ..Options::default()
    }
}

/// Wi-Fi frozen for 2 s: the session lives on; the frames that got too old are
/// dropped on the PC, and every stream starts again from a keyframe.
#[test]
fn a_short_stall_keeps_the_session() {
    let (_dir, mut pc, _keys, mut headset) = session("stall-short", &busy());
    // The stream reports logged so far are from before the stall.
    let reports = pc.count_log("Prostir-1: sent");
    headset.reset_counts();
    headset.stall(Duration::from_secs(2));
    // Without a loss no keyframe would come for 40 s (600 frames at 15 a second).
    headset
        .wait(SOON, "a keyframe on every stream", |v| {
            v.streams.len() == 2 && v.streams.values().all(|s| s.keyframes > 0)
        })
        .unwrap();
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert!(!headset.view().closed);
    assert_eq!(pc.count_log("session ended"), 0);
    // The next two reports (10 s apart) cover the stall, however the first one falls.
    let next = pc.wait_log_after(Duration::from_secs(15), "Prostir-1: sent", reports);
    let after = pc.wait_log_after(Duration::from_secs(15), "Prostir-1: sent", reports + 1);
    assert!(
        !(next.contains("dropped 0 late") && after.contains("dropped 0 late")),
        "frames were dropped late: {next} / {after}"
    );
}

/// Wi-Fi frozen for longer than 5 s: both sides end the session, and the headset
/// connects again to the same monitors.
#[test]
fn a_long_stall_ends_the_session_on_both_sides() {
    let (_dir, mut pc, mut keys, mut headset) = session("stall-long", &busy());
    let stalled = Instant::now();
    headset.stall(Duration::from_secs(6));
    pc.wait_log(Duration::from_secs(10), "session ended");
    assert!(stalled.elapsed() < Duration::from_secs(8));
    pc.wait_log(SOON, "keeping virtual monitors for 30 s");
    headset.wait_closed(Duration::from_secs(10)).unwrap();
    let mut back = connect(&pc, &mut keys);
    back.wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(
        pc.count_log("fake desktop: created"),
        2,
        "the same monitors"
    );
}

/// A PC that stops dead (asleep): the headset notices the silence within 5 s and
/// closes; once the PC runs again its session ends too, and the headset connects.
#[test]
fn a_pc_that_stops_is_noticed() {
    let (_dir, pc, mut keys, mut headset) = session("frozen-pc", &Options::default());
    pc.freeze();
    let frozen = Instant::now();
    headset.wait_closed(Duration::from_secs(8)).unwrap();
    assert!(frozen.elapsed() >= Duration::from_secs(4));
    assert_eq!(headset.view().end, Some(std::io::ErrorKind::TimedOut));
    std::thread::sleep(Duration::from_secs(1));
    pc.thaw();
    let mut back = connect(&pc, &mut keys);
    back.wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
}

/// The headset hangs up half way through a frame: the half frame goes with the
/// session, and the next session starts clean, a keyframe first on every stream.
#[test]
fn a_cut_in_the_middle_of_a_frame_leaves_nothing_behind() {
    let (_dir, mut pc, mut keys, mut headset) = session("cut", &busy());
    headset.cut_mid_message();
    headset.wait_closed(SOON).unwrap();
    assert_eq!(
        headset.view().end,
        Some(std::io::ErrorKind::ConnectionAborted)
    );
    pc.wait_log(SOON, "keeping virtual monitors for 30 s");
    let mut back = connect(&pc, &mut keys);
    back.wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    for s in back.view().streams.values() {
        assert_eq!(
            s.first_is_key,
            Some(true),
            "{} starts with a keyframe",
            s.name
        );
        assert_eq!(s.bad_keyframes, 0);
    }
    assert_eq!(pc.count_log("panicked"), 0);
}

/// Monitors that take long to make (7 s here, longer than 5 s of silence ends a
/// session) do not cost the session: the PC opens it once it can serve it, so the
/// headset does not count that time as silence.
#[test]
fn slow_monitors_do_not_end_the_session() {
    let options = Options {
        slow_monitors: Duration::from_millis(3500),
        ..Options::default()
    };
    let (_dir, pc, _keys, mut headset) = session("slow-monitors", &options);
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert!(!headset.view().closed);
    assert_eq!(pc.count_log("headset connected"), 1, "one connection");
}

/// A broken packet ends the session with a line in the log, not a crash.
#[test]
fn a_broken_packet_ends_the_session() {
    let (_dir, mut pc, mut keys, mut headset) = session("broken", &Options::default());
    // Flags no packet has.
    headset.send_record(&[0x80, 0, 1, 2, 3]).unwrap();
    headset.wait_closed(SOON).unwrap();
    let line = pc.wait_log(SOON, "session ended");
    assert!(line.contains("unknown packet flags"), "{line}");
    assert_eq!(pc.count_log("panicked"), 0);
    let mut back = connect(&pc, &mut keys);
    back.wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
}

/// A headset that reads slowly (a slow link on plain loopback): the PC's queues fill,
/// and sound still goes between the packets of the frames instead of after them.
#[test]
fn sound_goes_between_the_packets_of_frames() {
    let dir = TestDir::new("slow-reader");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &busy());
    // 16 Mbit/s: less than the two busy monitors send.
    let mut headset = prostir_testkit::FakeHeadset::connect_reading_at(pc.addr, &mut keys, 2 << 20)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset.reset_counts();
    headset.take_for(Duration::from_secs(5)).unwrap();
    let view = headset.view();
    assert!(
        view.sound_between_fragments > 10,
        "sound between the packets of frames: {} of {} slices",
        view.sound_between_fragments,
        view.sound_delays.len()
    );
    let over = view.sound_gaps.over();
    assert_eq!(over[3], 0, "no pause in the sound over 250 ms: {over:?}");
}

/// A display of the PC wider than the headset decodes in H.264 (5120 against 4096)
/// stands failed with `BAD_SIZE`, saying why, and never streams; the monitors stream,
/// and the session goes on.
#[test]
fn a_display_too_big_for_the_headset_fails_alone() {
    use prostir_proto::state::{DisplayState, ErrorCode};
    let options = Options {
        physical: "HDMI-A-1=Wide 0x1 0@5120x1440".into(),
        ..Options::default()
    };
    let dir = TestDir::new("too-big");
    dir.save_monitors("row\n640x360\nmirror on\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    let failed = |v: &View| {
        v.displays()
            .iter()
            .find(|d| {
                d.is_physical()
                    && matches!(&d.state, DisplayState::Failed { error, detail }
                    if *error == ErrorCode::BAD_SIZE && detail.contains("5120x1440"))
            })
            .map(|d| d.id)
    };
    headset
        .wait(SOON, "the wide display failed, saying why", |v| {
            failed(v).is_some() && v.all_monitors_show()
        })
        .unwrap();
    let wide = failed(headset.view()).unwrap();
    headset.take_for(Duration::from_secs(3)).unwrap();
    let view = headset.view();
    assert!(!view.streams.contains_key(&wide), "it never streamed");
    assert!(view.all_monitors_show());
    assert!(
        !view.closed && view.goodbye.is_none(),
        "the session goes on"
    );
    assert!(view.broken.is_empty(), "{:?}", view.broken);
    assert!(!pc.lines("is larger than the headset decodes").is_empty());
}

/// A request for a keyframe of an earlier generation of a stream is for frames long
/// gone: the PC makes no keyframe for it. One for the generation it sends now gets one.
///
/// The keyframes that come do not tell which request made them: on a busy machine the
/// PC drops frames that waited too long to go out and makes a keyframe of its own to
/// start the picture again. What the PC decided for each request does tell, so the
/// test reads that from its log. The PC handles a headset's messages in order, so an
/// old request sent after the current one shows when that one was handled too.
#[test]
fn a_keyframe_asked_for_an_old_generation_is_not_made() {
    use prostir_proto::ClientMsg;
    use prostir_proto::state::Gen;
    const NONE_MADE: &str = "not the generation it streams now: none made";
    let moving = Options {
        fps: 15,
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("old-generation", &moving);
    let id = headset.view().order()[0];
    let now = headset.view().streams[&id].generation;
    let ask = |headset: &mut prostir_testkit::FakeHeadset, generation| {
        headset
            .send(&ClientMsg::RequestKeyframe { id, generation })
            .unwrap();
    };
    for old in [Gen(now.0 - 1), Gen(0)] {
        ask(&mut headset, old);
    }
    pc.wait_log_after(SOON, NONE_MADE, 1);
    assert_eq!(pc.count_log(NONE_MADE), 2, "none made for either old one");
    let before = headset.view().streams[&id].keyframes;
    ask(&mut headset, now);
    ask(&mut headset, Gen(0));
    pc.wait_log_after(SOON, NONE_MADE, 2);
    assert_eq!(
        pc.count_log(NONE_MADE),
        3,
        "the current one is not taken for an old one"
    );
    headset
        .wait(SOON, "a keyframe for the current one", |v| {
            v.streams[&id].keyframes > before
        })
        .unwrap();
    assert_eq!(headset.view().streams[&id].generation, now);
}

/// Two requests for a keyframe within 100 ms, the second after the first one's
/// keyframe went: it comes within the 300 ms the PC leaves between a headset's
/// keyframes, and is not dropped for it (no keyframe comes on its own any more) but
/// asked for once they are over.
#[test]
fn a_keyframe_asked_twice_soon_comes_after_the_window() {
    use prostir_proto::ClientMsg;
    const LATER: &str = "within 300 ms of the one before, asked for now";
    let moving = Options {
        fps: 30,
        env: vec![("RUST_LOG".into(), "info,prostir::router=debug".into())],
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("keyframe-twice", &moving);
    let id = headset.view().order()[0];
    let generation = headset.view().streams[&id].generation;
    let before = headset.view().streams[&id].keyframes;
    let later = pc.count_log(LATER);
    let ask = |headset: &mut prostir_testkit::FakeHeadset| {
        headset
            .send(&ClientMsg::RequestKeyframe { id, generation })
            .unwrap();
    };
    let asked = Instant::now();
    ask(&mut headset);
    headset
        .wait(SOON, "the first keyframe", |v| {
            v.streams[&id].keyframes > before
        })
        .unwrap();
    let again = asked.elapsed();
    ask(&mut headset);
    headset
        .wait(SOON, "a keyframe for each request", |v| {
            v.streams[&id].keyframes >= before + 2
        })
        .unwrap();
    // On a slow machine the first keyframe may have come after the window: then the
    // second request was answered at once, with no wait to look at.
    if again < Duration::from_millis(250) {
        pc.wait_log_after(SOON, LATER, later);
        assert!(
            asked.elapsed() >= Duration::from_millis(300),
            "the second keyframe came within the window"
        );
    }
    assert_eq!(headset.view().streams[&id].generation, generation);
}
