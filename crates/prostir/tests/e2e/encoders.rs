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
#[test]
fn probes_are_kept_and_one_that_hangs_holds_nothing_up() {
    let dir = TestDir::new("probe-hangs");
    dir.save_monitors(THREE_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        software: false,
        encoder: Some("hangs"),
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
        took < Duration::from_secs(3),
        "monitors after {took:?}, the probe held them up"
    );
    assert_eq!(pc.count_log("fake hardware H264: did not answer in 3 s"), 1);
    assert_eq!(pc.count_log("encoding with fake hardware"), 0);
    let kept = cache(&dir);
    assert!(kept.contains("retry fake - h264 did not answer"), "{kept}");
    assert!(!kept.contains("missing fake"), "{kept}");
    headset.disconnect();
    pc.quit();

    // Started again: from the cache, the hung one probed again in the background,
    // not waited for.
    let mut pc = Pc::start(&dir, &options);
    let started = Instant::now();
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "monitors after {took:?}");
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
    pc.desktop("fail-source 1");
    pc.wait_log(SOON, "the next 1 source starts fail");
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

/// A source that fails again: the monitor gives up at once with the reason, without
/// trying any other encoder on it, and the session goes on.
#[test]
fn a_source_that_keeps_failing_gives_up_with_its_reason() {
    let options = Options {
        software: false,
        encoder: Some("works"),
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = crate::session("source-fails", &options);
    pc.desktop("fail-source 2");
    pc.wait_log(SOON, "the next 2 source starts fail");
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
