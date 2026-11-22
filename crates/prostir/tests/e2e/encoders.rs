//! The encoders: probed once and kept in the cache, a graphics device each while they
//! stream, a probe that hangs holding nothing up, and a session limit learned when an
//! encoder does not open, the output going on with the next encoder.

use std::time::{Duration, Instant};

use prostir_proto::request::{Outcome, Request};
use prostir_testkit::View;

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START, connect};

/// Three small monitors.
const THREE_MONITORS: &str = "row\n640x360\n480x272\n320x240\nmirror off\n";

/// Five monitors, as many as a desk of them.
const FIVE_MONITORS: &str = "row\n640x360\n640x360\n640x360\n640x360\n640x360\nmirror off\n";

/// The cache of what the probes found, in the test's cache directory.
fn cache(dir: &TestDir) -> String {
    std::fs::read_to_string(dir.path().join("cache/prostir/encoders.txt")).unwrap_or_default()
}

/// Five monitors on VA-API: each encoder keeps a VA display of its own (a file of the
/// render node), as measured no slower than one shared (see `encoder/gpu.rs`), and lets
/// it go with its stream: once all moved to the CPU, no graphics device is open while
/// the session goes on, and they come back with the streams. Skipped where the PC has
/// no VA-API encoder that takes the fake desktop's frames.
#[test]
fn each_va_encoder_has_a_display_while_it_streams() {
    let dir = TestDir::new("va-displays");
    dir.save_monitors(FIVE_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        software: false,
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(START, "video encoders");
    let on_card = pc.count_log("encoding with VA-API");
    if on_card == 0 {
        eprintln!("no VA-API encoder here: no displays to count");
        return;
    }
    let files = pc.gpu_files();
    eprintln!(
        "{on_card} monitors on VA-API with {} graphics device files open: {files:?}",
        files.len()
    );
    assert_eq!(on_card, 5, "every monitor on the card");
    assert_eq!(files.len(), 5, "a display each: {files:?}");
    assert_eq!(pc.count_log("sharing gst.va.display.handle"), 0);

    let encode = |headset: &mut prostir_testkit::FakeHeadset, software| {
        let asked = Request::SetEncoding {
            bitrate: None,
            codec: None,
            software: Some(software),
            sharp: None,
        };
        assert_eq!(headset.ask(asked).unwrap(), Outcome::Done);
    };
    let open_files = |pc: &Pc, n: usize| {
        let until = Instant::now() + SOON;
        while pc.gpu_files().len() != n {
            assert!(Instant::now() < until, "open: {:?}", pc.gpu_files());
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    encode(&mut headset, true);
    open_files(&pc, 0);
    headset
        .wait(SOON, "every monitor showing", View::all_monitors_show)
        .unwrap();
    encode(&mut headset, false);
    headset
        .wait(SOON, "every monitor showing", View::all_monitors_show)
        .unwrap();
    open_files(&pc, 5);
    assert!(!headset.view().closed, "the session went on");
}

/// Asked to quit right after it started, while its encoders are still probed, the PC
/// app waits for the probes and quits cleanly: before, the exit tore a driver's library
/// down under a probe, and the process crashed on the way out. A probe that hangs
/// holds quitting up no longer than a probe's time limit.
#[test]
fn quitting_while_the_encoders_are_probed() {
    for mode in ["slow", "hangs"] {
        let dir = TestDir::new(&format!("probe-{mode}-quit"));
        dir.save_monitors(THREE_MONITORS);
        let options = Options {
            software: false,
            encoder: Some(mode),
            ..Options::default()
        };
        let pc = Pc::start(&dir, &options);
        // Exits with success, within 10 s.
        pc.quit();
        if mode == "slow" {
            let kept = cache(&dir);
            assert!(
                kept.contains("encoder fake - h264"),
                "probes not waited for: {kept:?}"
            );
        }
    }
}

/// The encoders are probed when the PC app starts, before any headset, and kept: the
/// next start reads what was found instead. A probe that never ends holds nothing up
/// past its time limit, the encoder it was for is not used, and it is not kept as
/// missing: the next start uses what works at once and probes it again meanwhile.
/// The time limit is longer here than a busy machine takes to show the monitors, so
/// waiting for the probe shows in how long they took.
#[test]
fn probes_are_kept_and_one_that_hangs_holds_nothing_up() {
    const LIMIT: Duration = Duration::from_secs(15);
    let dir = TestDir::new("probe-hangs");
    dir.save_monitors(THREE_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        software: false,
        encoder: Some("hangs"),
        env: vec![(
            "PROSTIR_TEST_PROBE_SECONDS".into(),
            LIMIT.as_secs().to_string(),
        )],
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    // Before any headset.
    pc.wait_log(START, "video encoders probed");
    let started = Instant::now();
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let took = started.elapsed();
    assert!(
        took < LIMIT * 2 / 3,
        "monitors after {took:?}, the probe held them up"
    );
    assert_eq!(
        pc.count_log("fake hardware H264: did not answer in 15 s"),
        1
    );
    assert_eq!(pc.count_log("encoding with fake hardware"), 0);
    let kept = cache(&dir);
    assert!(kept.contains("retry fake - h264 did not answer"), "{kept}");
    assert!(!kept.contains("missing fake"), "{kept}");
    headset.disconnect();
    // Not quit: the PC would wait the limit for the hung probe (quitting so is
    // `quitting_while_the_encoders_are_probed`'s).
    drop(pc);

    // Started again: from the cache, the hung one probed again in the background,
    // not waited for.
    let mut pc = Pc::start(&dir, &options);
    let started = Instant::now();
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let took = started.elapsed();
    assert!(took < LIMIT * 2 / 3, "monitors after {took:?}");
    pc.wait_log(START, "video encoders as found before");
    pc.wait_log(START, "probing again in the background: fake hardware H264");
    assert_eq!(pc.count_log("video encoders probed in"), 0, "not all again");
    assert_eq!(pc.count_log("encoding with fake hardware"), 0);
}

/// A hardware encoder that opens two sessions: the third monitor's encoder does not
/// open, once and again when tried once more, so the limit is learned and kept, and
/// that monitor alone goes on with the next encoder (x264, or the PC's own card),
/// saying why; the session goes on. The next start tries a third once (a limit kept is
/// a hint, it may have risen) and holds the limit after that one failure.
#[test]
fn a_session_limit_is_learned_and_one_output_goes_on_with_the_next() {
    let dir = TestDir::new("session-limit");
    dir.save_monitors(THREE_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        software: false,
        encoder: Some("sessions=2"),
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(
        START,
        "fake hardware did not open with 2 open: tried once more",
    );
    pc.wait_log(START, "fake hardware opens at most 2 sessions at once");
    let why = pc.wait_log(START, "fake hardware: ");
    assert!(why.contains('2'), "{why}");
    assert_eq!(pc.count_log("did not open: no more encoder sessions"), 2);
    // Tried for all three, twice for the third.
    assert_eq!(pc.count_log("encoding with fake hardware"), 4);
    assert_eq!(pc.count_log("session ended"), 0, "the session goes on");
    assert!(cache(&dir).contains("sessions fake - 2"), "{}", cache(&dir));
    // Each monitor still streams.
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert!(headset.view().all_monitors_show());
    headset.disconnect();
    pc.quit();

    let mut pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(START, "fake hardware: ");
    assert_eq!(pc.count_log("did not open: no more encoder sessions"), 1);
    assert_eq!(
        pc.count_log("tried once more"),
        0,
        "one failure is enough now"
    );
    assert_eq!(pc.count_log("encoding with fake hardware"), 3);
    assert!(cache(&dir).contains("sessions fake - 2"), "{}", cache(&dir));
}

/// A hardware encoder that does not start at all past two sessions: the same as one
/// that starts and then does not open. The third monitor tries it twice, the limit is
/// learned, and that monitor goes on with the next encoder.
#[test]
fn an_encoder_that_does_not_start_teaches_its_limit_too() {
    let dir = TestDir::new("refuses");
    dir.save_monitors(THREE_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        software: false,
        encoder: Some("refuses=2"),
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(START, "fake hardware opens at most 2 sessions at once");
    assert_eq!(
        pc.count_log("H264 did not start: no more encoder sessions"),
        2
    );
    assert_eq!(pc.count_log("encoding with fake hardware H264"), 2);
    assert!(cache(&dir).contains("sessions fake - 2"), "{}", cache(&dir));
}

/// A hardware encoder whose probe fails is not used; the monitors stream all the same.
#[test]
fn an_encoder_that_fails_its_probe_is_not_used() {
    let dir = TestDir::new("probe-fails");
    dir.save_monitors(THREE_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        software: false,
        encoder: Some("broken"),
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(START, "fake hardware H264: broken on purpose");
    assert_eq!(pc.count_log("encoding with fake hardware"), 0);
    assert!(cache(&dir).contains("retry fake - h264 broken on purpose"));
}

/// A monitor added while the stream of its picture fails once (KWin's PipeWire node
/// not there): its source is tried once more with the same encoder, and it streams;
/// no other encoder is tried, nothing waits for a time limit, and the encoder is
/// not counted as one that did not open.
#[test]
fn a_source_that_fails_once_is_tried_again() {
    let options = Options {
        software: false,
        encoder: Some("works"),
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = crate::session("source-fails-once", &options);
    let before = pc.count_log("encoding with fake hardware");
    // The monitor added next.
    pc.desktop("fail-source Fake-3 1");
    pc.wait_log(SOON, "the next 1 source starts of Fake-3 fail");
    let asked = Instant::now();
    let added = headset
        .ask(Request::AddDisplay {
            mode: prostir_proto::state::Mode::new(320, 240),
        })
        .unwrap();
    let Outcome::Added { id } = added else {
        panic!("added, not {added:?}")
    };
    headset
        .wait(SOON, "the new monitor shows", |v| {
            v.streams
                .get(&id)
                .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(pc.count_log("its stream is gone (fail-source)"), 1);
    assert_eq!(pc.count_log("trying it once more"), 1);
    assert_eq!(pc.count_log(" did not start: "), 0);
    assert_eq!(pc.count_log(" did not open: "), 0);
    assert_eq!(pc.count_log("encoding with fake hardware"), before + 1);
    assert_eq!(pc.count_log("Prostir-3: encoding with fake hardware"), 1);
    assert!(!headset.view().closed);
}

/// A source that fails again: the monitor fails at once with the reason (it is tried
/// again by itself after its pause), without trying any other encoder on it, and the
/// session goes on.
#[test]
fn a_source_that_keeps_failing_gives_up_with_its_reason() {
    let options = Options {
        software: false,
        encoder: Some("works"),
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = crate::session("source-fails", &options);
    pc.desktop("fail-source Fake-3 2");
    pc.wait_log(SOON, "the next 2 source starts of Fake-3 fail");
    let asked = Instant::now();
    let added = headset
        .ask(Request::AddDisplay {
            mode: prostir_proto::state::Mode::new(320, 240),
        })
        .unwrap();
    assert!(matches!(added, Outcome::Added { .. }), "{added:?}");
    pc.wait_log(SOON, "the source of the frames fails");
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(pc.count_log("its stream is gone (fail-source)"), 2);
    assert_eq!(pc.count_log(" did not start: "), 0);
    assert_eq!(pc.count_log(" did not open: "), 0);
    assert_eq!(
        pc.count_log("Prostir-3: encoding with"),
        0,
        "no other encoder"
    );
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert!(!headset.view().closed);
}

/// The time a log line was written (`[2026-11-22T12:04:01.263Z …`), in ms of the day.
fn logged_ms(line: &str) -> u64 {
    let time = line.split('T').nth(1).unwrap_or_default();
    let mut parts = time.split([':', '.', 'Z']);
    let mut next = || -> u64 { parts.next().and_then(|p| p.parse().ok()).unwrap_or(0) };
    let (h, m, s, ms) = (next(), next(), next(), next());
    ((h * 60 + m) * 60 + s) * 1000 + ms
}

/// The encoder of one monitor breaks in the middle of its stream, again and again (a
/// driver that fails): only that monitor starts over, after a pause; the other one
/// goes on streaming all along and the session stays. Broken twice soon after it
/// started, that encoder is not used for this monitor for now: the next one is, and
/// it streams.
#[test]
fn one_monitor_whose_encoder_breaks_starts_again_alone() {
    let options = Options {
        software: false,
        encoder: Some("works"),
        fps: 10,
        // The monitors go with the headset, and are made anew for the next session.
        linger: 0,
        ..Options::default()
    };
    let (_dir, mut pc, mut keys, mut headset) = crate::session("encoder-breaks", &options);
    let first = pc.source_starts()["Fake-1"];
    let made = pc.count_log("Prostir-2: encoding with");
    pc.desktop("break-encoder Fake-2 2");
    pc.wait_log(SOON, "the next 2 fake hardware encoders of Fake-2 break");
    pc.wait_log(SOON, "Prostir-2: its encoder broke (1 in a row)");
    pc.wait_log(SOON, "Prostir-2: its encoder broke (2 in a row)");
    let next = pc.wait_log_after(SOON, "Prostir-2: encoding with", made + 1);
    assert!(!next.contains("encoding with fake hardware"), "{next}");
    headset
        .wait(SOON, "every monitor showing again", View::all_monitors_show)
        .unwrap();
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(pc.count_log("Prostir-2: its encoder broke ("), 2);
    assert_eq!(pc.source_starts()["Fake-1"], first, "the other one went on");
    let view = headset.view();
    assert!(
        !view.closed && view.goodbye.is_none(),
        "the session goes on"
    );
    // A new session tries every encoder again for it: its monitor, made anew, gets
    // the fake hardware encoder again.
    headset.disconnect();
    pc.wait_log(SOON, "headset disconnected");
    let hardware = pc.count_log("Prostir-2: encoding with fake hardware");
    let mut again = connect(&pc, &mut keys);
    again
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log_after(SOON, "Prostir-2: encoding with fake hardware", hardware);
}

/// An encoder that keeps breaking on a monitor, where it is the only one there is
/// (x264): it is never passed over, which would leave the monitor with none; the
/// monitor streams again once it breaks no more.
#[test]
fn the_only_encoder_is_never_passed_over() {
    let options = Options {
        fps: 10,
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = crate::session("encoder-only-one", &options);
    pc.desktop("break-encoder Fake-2 3 any");
    pc.wait_log(SOON, "the next 3 encoders of Fake-2 break");
    pc.wait_log(SOON, "Prostir-2: its encoder broke (3 in a row)");
    headset
        .wait(SOON, "every monitor showing again", View::all_monitors_show)
        .unwrap();
    assert_eq!(pc.count_log("not used for this output"), 0);
    let view = headset.view();
    assert!(
        !view.closed && view.goodbye.is_none(),
        "the session goes on"
    );
}

/// The desktop closes one monitor's output (KWin may), and its stream ends with it:
/// only that one is made again, after one pause (one failure, not two), with the
/// encoder it had (the stream's end is no fault of the encoder's), and streams; the
/// other goes on and the session stays.
#[test]
fn a_monitor_the_desktop_closed_is_made_again_alone() {
    let options = Options {
        software: false,
        encoder: Some("works"),
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = crate::session("output-closed", &options);
    let first = pc.source_starts()["Fake-1"];
    pc.desktop("close Fake-2");
    pc.wait_log(SOON, "fake desktop: closed Fake-2");
    pc.wait_log(SOON, "fake desktop: removed Fake-2");
    pc.wait_log_after(SOON, "fake desktop: created Fake-2 ", 1);
    headset
        .wait(SOON, "every monitor showing again", View::all_monitors_show)
        .unwrap();
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(
        pc.count_log("Prostir-2: trying it again (failure 1 in a row)"),
        1
    );
    assert_eq!(pc.count_log("(failure 2 in a row)"), 0, "counted twice");
    assert_eq!(pc.count_log("its encoder broke"), 0);
    assert_eq!(
        pc.count_log("Prostir-2: encoding with fake hardware"),
        2,
        "the same encoder again"
    );
    assert_eq!(pc.count_log("fake desktop: created Fake-1 "), 1);
    assert_eq!(pc.source_starts()["Fake-1"], first, "the other one went on");
    let view = headset.view();
    assert!(
        !view.closed && view.goodbye.is_none(),
        "the session goes on"
    );
}

/// A monitor that cannot be made is tried again by itself, each time after a longer
/// pause (1 s, 2 s…), while the other streams and the session stays.
#[test]
fn a_monitor_that_cannot_be_made_is_tried_again_after_longer_pauses() {
    let options = Options {
        broken_monitors: "2".into(),
        ..Options::default()
    };
    let dir = TestDir::new("monitor-retried");
    dir.save_monitors(crate::TWO_MONITORS);
    let mut keys = dir.pair();
    let mut pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    pc.wait_log_after(START, "fake monitor 2 is broken", 2);
    let tries: Vec<u64> = pc
        .lines("fake monitor 2 is broken")
        .iter()
        .map(|l| logged_ms(l))
        .collect();
    let (first, second) = (tries[1] - tries[0], tries[2] - tries[1]);
    assert!(
        (800..2500).contains(&first) && second > first + 200,
        "tried again after {first} ms, then {second} ms"
    );
    headset
        .wait(SOON, "the other monitor showing", |v| {
            v.streams.values().any(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    let view = headset.view();
    assert!(
        !view.closed && view.goodbye.is_none(),
        "the session goes on"
    );
}

/// No monitor can be made at all: the session goes on while each is tried again (the
/// desktop may come back from a restart), and ends, saying why, only once nothing
/// streamed for 30 s.
#[test]
fn a_session_where_nothing_streams_ends_after_a_while() {
    let options = Options {
        broken_monitors: "1,2".into(),
        ..Options::default()
    };
    let dir = TestDir::new("nothing-streams");
    dir.save_monitors(crate::TWO_MONITORS);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let started = Instant::now();
    let mut headset = connect(&pc, &mut keys);
    headset.wait_closed(Duration::from_secs(60)).unwrap();
    let ended = started.elapsed();
    assert!(
        ended >= Duration::from_secs(28) && ended < Duration::from_secs(45),
        "ended after {ended:?}"
    );
    assert_eq!(
        headset.view().goodbye,
        Some(prostir_proto::PcGoodbye::Failed.code())
    );
    assert!(pc.count_log("fake monitor 1 is broken") >= 4, "tried again");
}
