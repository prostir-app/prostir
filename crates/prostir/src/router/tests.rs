use super::*;

/// A headset that decodes H.264 and H.265 up to 8K.
fn headset() -> state::HeadsetCaps {
    let decoder = |codec| state::Decoder {
        codec,
        max_w: 7680,
        max_h: 4320,
    };
    state::HeadsetCaps {
        decoders: vec![decoder(0), decoder(1)],
        ..state::HeadsetCaps::default()
    }
}

/// One that decodes H.264 only up to 32 pixels wide.
fn small() -> state::HeadsetCaps {
    let mut small = headset();
    small.decoders[0].max_w = 32;
    small
}

fn sent(outbox: &Outbox) -> Vec<HostMsg> {
    outbox.sent()
}

fn id(n: u32) -> ScreenId {
    ScreenId::from_wire(state::DisplayId(n))
}

fn size(router: &Router, id: ScreenId) {
    router.on_event(
        id,
        EncoderEvent::Size {
            width: 64,
            height: 32,
            kbps: 30_000,
            sharpens: true,
        },
    );
}

fn frame(router: &Router, id: ScreenId, keyframe: bool) -> bool {
    router.on_event(
        id,
        EncoderEvent::Frame {
            pts_us: 0,
            keyframe,
            data: vec![0],
            timing: None,
        },
    )
}

/// A frame of a new picture (`repeat`: the picture before, sent again).
fn timed(router: &Router, id: ScreenId, keyframe: bool, repeat: bool) {
    router.on_event(
        id,
        EncoderEvent::Frame {
            pts_us: 0,
            keyframe,
            data: vec![0],
            timing: Some(FrameTiming {
                encode_us: 1000,
                repeat,
            }),
        },
    );
}

fn keyframe(router: &Router, id: ScreenId) {
    size(router, id);
    frame(router, id, true);
}

/// A session's outbox, attached to `router` as session `session` and worn.
fn attach(router: &Router, session: SessionId, caps: state::HeadsetCaps) -> Arc<Outbox> {
    let outbox = Arc::new(Outbox::default());
    router.attach(session, outbox.clone(), caps, true);
    outbox
}

/// The frames in `msgs`: their generation and number.
fn frames(msgs: &[HostMsg]) -> Vec<(Gen, u32)> {
    msgs.iter()
        .filter_map(|m| match m {
            HostMsg::Frame {
                generation, seq, ..
            } => Some((*generation, *seq)),
            _ => None,
        })
        .collect()
}

/// A stream that starts over with another encoder is announced with its codec in a
/// new generation, and the headset hears when it ends, even before the new
/// encoder sent anything.
#[test]
fn a_restarted_stream_is_announced_and_ended() {
    let router = Router::new();
    let outbox = attach(&router, 1, headset());
    let (s0, s1) = (id(0), id(1));
    router.publish(1, &BTreeSet::from([s0, s1]));
    router.add_stream(s0, Codec::H265, "test".into());
    keyframe(&router, s0);
    let [
        HostMsg::StreamStart {
            codec: Codec::H265,
            generation: first,
            ..
        },
        HostMsg::Frame {
            generation: g,
            seq: 0,
            ..
        },
    ] = sent(&outbox)[..]
    else {
        panic!("a start and its first frame")
    };
    assert_eq!(first, g);
    assert!(router.is_current(1, s0, first));
    router.add_stream(s0, Codec::H264, "test".into());
    keyframe(&router, s0);
    let [
        HostMsg::StreamStart {
            codec: Codec::H264,
            generation: second,
            ..
        },
        HostMsg::Frame { .. },
    ] = sent(&outbox)[..]
    else {
        panic!("a start and its first frame")
    };
    assert!(second > first, "a new generation");
    assert!(!router.is_current(1, s0, first), "the old one is over");
    router.add_stream(s0, Codec::H265, "test".into());
    router.remove_stream(s0);
    assert_eq!(
        sent(&outbox),
        [HostMsg::StreamEnd {
            id: s0.wire(),
            generation: second
        }]
    );
    // Never shown: nothing to end.
    router.add_stream(s1, Codec::H265, "test".into());
    router.remove_stream(s1);
    assert!(sent(&outbox).is_empty());
}

/// "Encoded → shown" is the time from a frame queued to the headset saying it
/// showed it: one sample a report, for the frame of the current generation it
/// names, and the frames before it are done too.
#[test]
fn a_shown_frame_is_timed_from_when_it_was_sent() {
    let router = Router::new();
    let outbox = attach(&router, 1, headset());
    let s = id(3);
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    for _ in 0..3 {
        frame(&router, s, false);
    }
    let generation = match sent(&outbox)[0] {
        HostMsg::StreamStart { generation, .. } => generation,
        ref other => panic!("{other:?}"),
    };
    let shown = |router: &Router| {
        let inner = router.inner.lock().unwrap();
        let d = &inner.receivers[&1].streams[&s];
        (d.shown.count(), d.shown.max(), d.sent.len())
    };
    // Another generation says nothing.
    router.frame_shown(1, s, Gen(generation.0 + 1), 1);
    assert_eq!(shown(&router).0, 0);
    router.frame_shown(1, s, generation, 2);
    let (count, max, waiting) = shown(&router);
    assert_eq!(count, 1);
    assert!(max < Duration::from_secs(1), "{max:?}");
    assert_eq!(waiting, 1, "only the frame after it is still out");
    // Nor does a frame never sent, nor another session's report.
    router.frame_shown(1, s, generation, 99);
    router.frame_shown(2, s, generation, 3);
    assert_eq!(shown(&router).0, 1);
}

/// A stream larger than the headset decodes in its codec never starts: no
/// `StreamStart`, no frames, and `Displays` hears of it to mark it failed.
#[test]
fn a_stream_too_big_for_the_headset_never_starts() {
    let router = Router::new();
    let outbox = attach(&router, 1, small());
    let s = id(5);
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    assert!(sent(&outbox).is_empty(), "nothing of it goes");
    assert!(router.still_too_big(s));
    // What was found for a headset that left is let go.
    router.detach(1);
    assert!(router.take_too_big().is_empty());
    assert!(!router.still_too_big(s), "nobody to be too big for");
    let outbox = attach(&router, 2, small());
    assert_eq!(router.take_too_big(), [(s, (64, 32), Codec::H264)]);
    // A stream started over is not too big until its encoder says its size.
    router.add_stream(s, Codec::H264, "test".into());
    assert!(!router.still_too_big(s));
    // The same stream fits another headset.
    router.detach(2);
    let outbox_3 = attach(&router, 3, headset());
    router.publish(3, &BTreeSet::from([s]));
    keyframe(&router, s);
    assert!(matches!(sent(&outbox_3)[0], HostMsg::StreamStart { .. }));
    assert!(router.take_too_big().is_empty());
    assert!(sent(&outbox).is_empty());
}

/// A stream starts only once the headset has its display in the list: before,
/// its frames go nowhere, and the start waits for a keyframe after.
#[test]
fn a_stream_waits_for_its_display_in_the_list() {
    let router = Router::new();
    let outbox = attach(&router, 1, headset());
    let s = id(7);
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    assert!(sent(&outbox).is_empty(), "not listed yet");
    assert_eq!(
        router.publish(1, &BTreeSet::from([s])),
        [s],
        "a picture now"
    );
    assert!(router.publish(1, &BTreeSet::from([s])).is_empty());
    keyframe(&router, s);
    assert!(matches!(
        sent(&outbox)[..],
        [HostMsg::StreamStart { .. }, HostMsg::Frame { seq: 0, .. }]
    ));
    // A new session lists everything anew.
    router.detach(1);
    let outbox = attach(&router, 2, headset());
    keyframe(&router, s);
    assert!(sent(&outbox).is_empty());
}

/// Two headsets get the same stream, each announced in its own generation and
/// numbered from 0; the one that joins later gets it from the next keyframe, and
/// one that leaves takes nothing from the other.
#[test]
fn a_stream_fans_out_to_every_headset() {
    let router = Router::new();
    let s = id(1);
    let a = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    frame(&router, s, false);
    assert_eq!(frames(&sent(&a)).len(), 2);
    let b = attach(&router, 2, headset());
    router.publish(2, &BTreeSet::from([s]));
    // Not a keyframe: the new one waits for one, and the encoder is asked.
    assert!(frame(&router, s, false));
    assert!(matches!(sent(&b)[..], [HostMsg::StreamStart { .. }]));
    assert_eq!(frames(&sent(&a)).len(), 1, "the first one goes on");
    frame(&router, s, true);
    let (ga, gb) = (frames(&sent(&a)), frames(&sent(&b)));
    assert_eq!(gb.len(), 1);
    assert_eq!(gb[0].1, 0, "numbered from 0 for the new one");
    assert_ne!(ga[0].0, gb[0].0, "a generation each");
    assert_eq!(ga[0].1, 3, "the first one counts on");
    router.detach(1);
    frame(&router, s, false);
    assert_eq!(frames(&sent(&b)).len(), 1, "the other one goes on");
    assert!(sent(&a).is_empty());
}

/// A stream too big for one headset goes to the other, and fails for all only
/// once the one that decodes it leaves.
#[test]
fn a_stream_too_big_for_one_headset_goes_to_the_other() {
    let router = Router::new();
    let s = id(2);
    let big = attach(&router, 1, headset());
    let little = attach(&router, 2, small());
    for session in [1, 2] {
        router.publish(session, &BTreeSet::from([s]));
    }
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    assert_eq!(frames(&sent(&big)).len(), 1);
    assert!(sent(&little).is_empty(), "nothing of it to the small one");
    assert!(router.take_too_big().is_empty(), "not failed for all");
    assert!(!router.still_too_big(s));
    assert!(router.fits(Codec::H264, (64, 32)));
    router.detach(1);
    assert_eq!(router.take_too_big(), [(s, (64, 32), Codec::H264)]);
    assert!(router.still_too_big(s));
    assert!(!router.fits(Codec::H264, (64, 32)));
}

/// A headset taken off gets neither frames nor sound while the other does; put on
/// again, it gets a fresh picture of what went meanwhile.
#[test]
fn a_headset_taken_off_gets_nothing_and_a_fresh_picture_after() {
    let router = Router::new();
    let s = id(4);
    let on = attach(&router, 1, headset());
    let off = attach(&router, 2, headset());
    for session in [1, 2] {
        router.publish(session, &BTreeSet::from([s]));
    }
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    assert!(router.set_worn(2, false).is_empty());
    let _ = (sent(&on), sent(&off));
    frame(&router, s, true);
    router.send_audio(vec![1, 2]);
    assert_eq!(sent(&on).len(), 2, "a frame and sound");
    assert!(sent(&off).is_empty());
    assert_eq!(router.set_worn(2, true), [s], "a fresh picture");
    assert!(router.set_worn(2, true).is_empty(), "once");
    // Waits for that keyframe.
    frame(&router, s, false);
    assert!(sent(&off).is_empty());
    frame(&router, s, true);
    assert_eq!(frames(&sent(&off)).len(), 1);
}

/// A message for every headset goes to each.
#[test]
fn a_problem_goes_to_every_headset() {
    let router = Router::new();
    let (a, b) = (attach(&router, 1, headset()), attach(&router, 2, headset()));
    let problem = HostMsg::Problem {
        error: state::ErrorCode::DESKTOP,
        detail: "test".into(),
        display: None,
    };
    router.send_all(&problem);
    assert_eq!(sent(&a), std::slice::from_ref(&problem));
    assert_eq!(sent(&b), [problem]);
    assert_eq!(router.headsets().len(), 2);
}

/// Alone, a headset asks for a keyframe whenever the stream may; next to others,
/// one that keeps asking asks less and less often (300 ms, 600 ms, 1.2 s, then
/// every 2 s), never while its last keyframe still waits, and as often as at first
/// once it went 4 s without asking.
#[test]
fn a_headset_that_keeps_losing_frames_asks_for_keyframes_less_often() {
    let t0 = Instant::now();
    let ms = |n| t0 + Duration::from_millis(n);
    let mut alone = KeyAsks::default();
    assert!((0..5).all(|i| alone.due(ms(i), false, true)));
    let mut asks = KeyAsks::default();
    assert!(asks.due(ms(0), true, false));
    assert!(!asks.due(ms(299), true, false));
    assert!(asks.due(ms(300), true, false));
    assert!(!asks.due(ms(899), true, false));
    assert!(!asks.due(ms(900), true, true), "its keyframe still waits");
    assert!(asks.due(ms(900), true, false));
    assert!(!asks.due(ms(2099), true, false));
    assert!(asks.due(ms(2100), true, false));
    assert!(!asks.due(ms(4099), true, false), "2 s at most");
    assert!(asks.due(ms(4100), true, false));
    assert!(asks.due(ms(8100), true, false), "kept up for 4 s");
    assert!(asks.due(ms(8400), true, false), "300 ms again");
}

/// A headset that waits for a keyframe while its last one still sits in its queue
/// does not ask the encoder for another, which the other headset would get too;
/// once its queue drained, it asks.
#[test]
fn a_headset_behind_asks_no_keyframe_for_the_others() {
    let router = Router::new();
    let s = id(6);
    let a = attach(&router, 1, headset());
    let b = attach(&router, 2, headset());
    for session in [1, 2] {
        router.publish(session, &BTreeSet::from([s]));
    }
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    let _ = sent(&a);
    // The second one lost a frame; its keyframe is still queued, never sent.
    let need = |router: &Router| {
        let mut inner = router.inner.lock().unwrap();
        let d = inner
            .receivers
            .get_mut(&2)
            .unwrap()
            .streams
            .get_mut(&s)
            .unwrap();
        d.need_keyframe = true;
    };
    need(&router);
    assert!(!frame(&router, s, false), "no keyframe for it yet");
    assert_eq!(frames(&sent(&a)).len(), 1, "the first one goes on");
    // Sent now: it asks.
    let _ = sent(&b);
    assert!(frame(&router, s, false));
    assert!(!frame(&router, s, false), "not again at once");
}

/// A stream sending frames to a headset is live; a headset that waits for a fresh
/// picture of a stream that sent nothing since is starved after a while, once,
/// and no longer once a keyframe went to it.
#[test]
fn a_still_stream_starves_a_headset_that_waits_for_its_picture() {
    let router = Router::new();
    let s = id(8);
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    frame(&router, s, false);
    assert!(!router.live(s), "it goes to nobody");
    let a = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    assert!(
        router.starved(Duration::from_secs(60)).is_empty(),
        "not yet"
    );
    assert_eq!(router.starved(Duration::ZERO), [s]);
    assert!(
        router.starved(Duration::from_secs(60)).is_empty(),
        "waits again"
    );
    // A keyframe went to it: nothing to wait for, and it is live for it.
    keyframe(&router, s);
    assert_eq!(frames(&sent(&a)).len(), 1);
    assert!(router.starved(Duration::ZERO).is_empty());
    frame(&router, s, false);
    assert!(router.live(s));
    router.set_worn(1, false);
    assert!(!router.live(s), "not to a headset taken off");
}

/// A headset's request for a keyframe within 300 ms of the one before is not lost:
/// requests in the window count as one, asked for once the window is over, with the
/// next frame or the next tick, and only once.
#[test]
fn a_keyframe_asked_again_soon_comes_after_the_window() {
    let router = Router::new();
    let s = id(4);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    assert!(
        !frame(&router, s, false),
        "a stream that flows asks nothing"
    );
    router.keyframe_asked(s);
    router.keyframe_asked(s);
    router.keyframe_asked(s);
    assert!(!frame(&router, s, false), "not within the 300 ms");
    assert_eq!(router.pending_keyframes(), 0);
    std::thread::sleep(HEADSET_KEYFRAMES + Duration::from_millis(20));
    assert!(frame(&router, s, false), "asked once the window is over");
    assert!(!frame(&router, s, false), "once for all three");
    assert_eq!(router.pending_keyframes(), 0);

    // On a still screen no frame comes: the tick asks.
    router.keyframe_asked(s);
    std::thread::sleep(HEADSET_KEYFRAMES + Duration::from_millis(20));
    router.keyframe_asked(s);
    router.keyframe_asked(s);
    assert_eq!(router.pending_keyframes(), 0, "the window began again");
    std::thread::sleep(HEADSET_KEYFRAMES + Duration::from_millis(20));
    assert_eq!(router.pending_keyframes(), 1);
    assert_eq!(router.pending_keyframes(), 0, "only once");
    assert!(!frame(&router, s, false), "asked already");
}

/// A screen that went still after new pictures gets one keyframe, once for each still
/// moment; repeats of the picture and a keyframe that came anyway change nothing.
#[test]
fn a_still_screen_gets_one_keyframe() {
    let router = Router::new();
    let s = id(5);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    let still = Duration::from_millis(50);
    assert_eq!(
        router.still_keyframes(still, still / 5),
        0,
        "nothing new since the keyframe"
    );
    timed(&router, s, false, false);
    assert_eq!(router.still_keyframes(still, still / 5), 0, "not still yet");
    std::thread::sleep(still + Duration::from_millis(10));
    // The source's keepalive: the same picture, it is still still.
    timed(&router, s, false, true);
    assert_eq!(router.still_keyframes(still, still / 5), 1);
    assert_eq!(router.still_keyframes(still, still / 5), 0, "once");
    timed(&router, s, true, true);
    std::thread::sleep(still + Duration::from_millis(10));
    assert_eq!(
        router.still_keyframes(still, still / 5),
        0,
        "still since that keyframe"
    );
    // It moved again, and a keyframe came anyway before it went still: none asked.
    timed(&router, s, false, false);
    timed(&router, s, true, false);
    std::thread::sleep(still + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(still, still / 5), 0);
    // It moved again: one more once still.
    timed(&router, s, false, false);
    std::thread::sleep(still + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(still, still / 5), 1);
}

/// A frame of `bytes` (`repeat`: the picture before, sent again).
fn sized(router: &Router, id: ScreenId, keyframe: bool, repeat: bool, bytes: usize) {
    router.on_event(
        id,
        EncoderEvent::Frame {
            pts_us: 0,
            keyframe,
            data: vec![0; bytes],
            timing: Some(FrameTiming {
                encode_us: 1000,
                repeat,
            }),
        },
    );
}

/// What stream `id` counted since the last report.
fn counted(router: &Router, id: ScreenId) -> StreamStats {
    let mut inner = router.inner.lock().unwrap();
    inner
        .streams
        .get_mut(&id)
        .unwrap()
        .tally
        .take(Instant::now())
}

/// At 30 Mbit/s a frame's budget is 62.5 KB: 300 KB is a big change, 8 KB typing.
const BIG: usize = 300_000;
const TYPED: usize = 8_000;

/// The times of the tick, a tenth of the real ones (3 s, 0.7 s).
const STILL: Duration = Duration::from_millis(300);
const EARLY: Duration = Duration::from_millis(70);

/// After a big change the still picture is sharpened early: one keyframe once it is
/// still for the shorter time, counted, and nothing more in that pause; the next
/// small change waits the usual time again.
#[test]
fn a_big_change_is_sharpened_early_once() {
    assert!(big_frame(BIG, 30_000) && !big_frame(TYPED, 30_000));
    assert!(!big_frame(BIG, 0), "not before the bitrate is known");
    let router = Router::new();
    let s = id(5);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    sized(&router, s, false, false, BIG);
    assert_eq!(router.still_keyframes(STILL, EARLY), 0, "not still yet");
    std::thread::sleep(EARLY + Duration::from_millis(10));
    // A keepalive of the source: the same picture.
    sized(&router, s, false, true, 100);
    assert_eq!(router.still_keyframes(STILL, EARLY), 1, "sharpened early");
    std::thread::sleep(STILL);
    assert_eq!(router.still_keyframes(STILL, EARLY), 0, "once a pause");
    let stats = counted(&router, s);
    assert_eq!(stats.sharpened, 1);
    assert!(
        stats
            .summary()
            .contains("keyframes 1 (1 to sharpen a still picture)"),
        "{}",
        stats.summary()
    );
    // The keyframe asked for came; then a small change: the usual time.
    sized(&router, s, true, true, BIG);
    sized(&router, s, false, false, TYPED);
    std::thread::sleep(EARLY + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(STILL, EARLY), 0, "small: not early");
    std::thread::sleep(STILL);
    assert_eq!(router.still_keyframes(STILL, EARLY), 1);
    assert_eq!(counted(&router, s).sharpened, 0, "the usual one");
    // A big keyframe (a headset asked for one) is sharp: nothing to sharpen.
    sized(&router, s, true, false, BIG);
    sized(&router, s, false, false, TYPED);
    std::thread::sleep(EARLY + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(STILL, EARLY), 0);
}

/// An encoder whose keyframe is no sharper than a big frame (VA-API's faster preset)
/// gets the usual keyframe of a still screen after a big change, not an early one;
/// switched to one that sharpens, the next big change is sharpened early.
#[test]
fn only_an_encoder_that_sharpens_is_asked_early() {
    let router = Router::new();
    let s = id(5);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H265, "test".into());
    keyframe(&router, s);
    router.set_tuning(s, 30_000, false);
    sized(&router, s, false, false, BIG);
    std::thread::sleep(EARLY + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(STILL, EARLY), 0, "not early");
    std::thread::sleep(STILL);
    assert_eq!(router.still_keyframes(STILL, EARLY), 1, "the usual one");
    assert_eq!(counted(&router, s).sharpened, 0);
    sized(&router, s, true, true, BIG);
    router.set_tuning(s, 30_000, true);
    sized(&router, s, false, false, BIG);
    std::thread::sleep(EARLY + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(STILL, EARLY), 1, "early");
    // A big change, then the encoder no longer sharpens: nothing early.
    sized(&router, s, true, true, BIG);
    sized(&router, s, false, false, BIG);
    router.set_tuning(s, 30_000, false);
    std::thread::sleep(EARLY + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(STILL, EARLY), 0);
}

/// The control loop wakes when a keyframe of a still screen is due (`still_due`), so
/// the one that sharpens a coarse picture is asked for at its time, not at the next
/// housekeeping, which comes every 500 ms.
#[test]
fn a_coarse_picture_is_sharpened_on_time() {
    let router = Router::new();
    let s = id(5);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H265, "test".into());
    keyframe(&router, s);
    assert_eq!(router.still_due(STILL, EARLY), None, "settled");
    let before = Instant::now();
    sized(&router, s, false, false, BIG);
    let after = Instant::now();
    let due = router.still_due(STILL, EARLY).expect("due");
    assert!(
        due >= before + EARLY && due <= after + EARLY,
        "{:?}",
        due - before
    );
    // A keepalive changes nothing; the loop sleeps until then.
    sized(&router, s, false, true, 100);
    assert_eq!(router.still_due(STILL, EARLY), Some(due));
    std::thread::sleep(due.saturating_duration_since(Instant::now()));
    assert_eq!(router.still_keyframes(STILL, EARLY), 1);
    let took = before.elapsed();
    assert!(
        took < EARLY + Duration::from_millis(40),
        "asked {took:?} after"
    );
    assert_eq!(router.still_due(STILL, EARLY), None, "once");
    // A small change: due after the usual time.
    sized(&router, s, true, true, BIG);
    let before = Instant::now();
    sized(&router, s, false, false, TYPED);
    let due = router.still_due(STILL, EARLY).expect("due");
    assert!(due >= before + STILL && due <= Instant::now() + STILL);
}

/// Typing with pauses makes no keyframe in the pauses, only the usual one once it
/// stops; after a big change, one in the first pause and none in the others.
#[test]
fn typing_with_pauses_makes_no_extra_keyframes() {
    let router = Router::new();
    let s = id(6);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    counted(&router, s);
    let pause = EARLY + Duration::from_millis(30);
    let mut asked = 0;
    for _ in 0..4 {
        for _ in 0..3 {
            sized(&router, s, false, false, TYPED);
        }
        std::thread::sleep(pause);
        asked += router.still_keyframes(STILL, EARLY);
    }
    assert_eq!(asked, 0, "none in the pauses");
    std::thread::sleep(STILL);
    assert_eq!(router.still_keyframes(STILL, EARLY), 1, "the usual one");
    sized(&router, s, true, true, BIG);
    // A new file, then typing in it.
    sized(&router, s, false, false, BIG);
    let mut asked = Vec::new();
    for _ in 0..4 {
        std::thread::sleep(pause);
        let n = router.still_keyframes(STILL, EARLY);
        if n > 0 {
            // The keyframe asked for comes with the next frame.
            sized(&router, s, true, false, BIG);
        }
        asked.push(n);
        sized(&router, s, false, false, TYPED);
    }
    assert_eq!(asked, [1, 0, 0, 0], "sharpened in the first pause only");
    let stats = counted(&router, s);
    assert_eq!(stats.sharpened, 1);
}

/// A keyframe the router itself needs for a headset (one put on again, one that just
/// came) within 300 ms of the one asked before is not lost when the screen then
/// stands still: it waits, and the tick asks for it once the 300 ms are over.
#[test]
fn a_keyframe_the_router_needs_soon_after_another_is_not_lost() {
    let router = Router::new();
    let s = id(7);
    let _first = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    // Taken off and put on again: the next frame asks for a keyframe for it.
    router.set_worn(1, false);
    frame(&router, s, false);
    router.set_worn(1, true);
    assert!(frame(&router, s, false), "asked at once");
    // Another headset comes within the 300 ms, and the screen goes still after one
    // more frame: that frame may not ask, the keyframe waits for the window.
    let _second = attach(&router, 2, headset());
    router.publish(2, &BTreeSet::from([s]));
    assert!(!frame(&router, s, false), "not within the 300 ms");
    assert_eq!(router.pending_keyframes(), 0, "not yet");
    std::thread::sleep(KEYFRAME_EVERY + Duration::from_millis(20));
    assert_eq!(
        router.pending_keyframes(),
        1,
        "asked once the window is over"
    );
    assert_eq!(router.pending_keyframes(), 0, "only once");
    // The keyframe answers it: nothing waits any more.
    frame(&router, s, true);
    std::thread::sleep(KEYFRAME_EVERY + Duration::from_millis(20));
    assert_eq!(router.pending_keyframes(), 0);
}

/// A keyframe that goes out answers a request that waited for the window.
#[test]
fn a_keyframe_going_out_answers_a_waiting_request() {
    let router = Router::new();
    let s = id(6);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into());
    keyframe(&router, s);
    router.keyframe_asked(s);
    router.keyframe_asked(s);
    frame(&router, s, true);
    std::thread::sleep(HEADSET_KEYFRAMES + Duration::from_millis(20));
    assert_eq!(router.pending_keyframes(), 0, "answered by the keyframe");
    assert!(!frame(&router, s, false));
}

/// Sound stops going out after 200 ms of silence, and comes again with the first
/// slice that is not silent.
#[test]
fn silence_stops_after_200_ms_and_sound_comes_at_once() {
    let router = Router::new();
    let outbox = attach(&router, 1, headset());
    let audio = |outbox: &Outbox| {
        sent(outbox)
            .iter()
            .filter(|m| matches!(m, HostMsg::Audio { .. }))
            .count()
    };
    let silence = vec![0u8; 1920];
    let mut sound = vec![0u8; 1920];
    sound[1919] = 1;
    for _ in 0..5 {
        router.send_audio(sound.clone());
    }
    assert_eq!(audio(&outbox), 5);
    // The outbox keeps 8 slices at most: read as they come.
    let mut got = 0;
    for _ in 0..50 {
        router.send_audio(silence.clone());
        got += audio(&outbox);
    }
    assert_eq!(got, 20, "200 ms of silence, then nothing");
    router.send_audio(sound.clone());
    assert_eq!(
        audio(&outbox),
        3,
        "sound again at once, after 20 ms of silence"
    );
    router.send_audio(silence);
    assert_eq!(audio(&outbox), 1, "silence after sound goes on for a while");
}

/// A headset that draws the pointer itself.
fn drawing() -> state::HeadsetCaps {
    state::HeadsetCaps {
        features: vec![state::caps::headset::CURSOR],
        ..headset()
    }
}

fn look(grey: u8) -> CursorEvent {
    CursorEvent::Shape(crate::platform::CursorImage {
        w: 2,
        h: 2,
        hot_x: 0,
        hot_y: 0,
        rgba: vec![grey; 16],
    })
}

/// The pointer moved to `x`, `y` of `output`, whose picture is 64x32 on the desktop.
fn moved(output: &str, x: i32, y: i32) -> CursorEvent {
    CursorEvent::Moved {
        output: OutputKey::new(output),
        x,
        y,
        size: (64, 32),
    }
}

/// The pointer's messages among what `outbox` sent: the looks' numbers, and the places.
fn pointer(outbox: &Outbox) -> (Vec<u32>, Vec<At>) {
    let (mut looks, mut places) = (Vec::new(), Vec::new());
    for m in sent(outbox) {
        match m {
            HostMsg::CursorShape { shape, .. } => looks.push(shape),
            HostMsg::CursorAt {
                display,
                u,
                v,
                shape,
            } => places.push(At {
                display,
                u,
                v,
                shape,
            }),
            _ => {}
        }
    }
    (looks, places)
}

/// The pointer goes apart from the pictures only while every headset draws it: then
/// each hears its look once and its place across the picture of a display it was told
/// of; a headset that does not draw it brings it back into the pictures, and the other
/// hears it is nowhere.
#[test]
fn the_pointer_goes_to_headsets_that_all_draw_it() {
    let router = Router::new();
    router.set_cursor_source(true);
    let s = id(1);
    router.add_stream(s, Codec::H264, "Prostir-1".into());
    router.set_stream_output(s, OutputKey::new("Fake-1"), false);
    size(&router, s);
    let first = attach(&router, 1, drawing());
    assert!(router.cursor_apart());
    router.publish(1, &BTreeSet::from([s]));
    sent(&first);
    router.cursor(look(1));
    router.cursor(moved("Fake-1", 63, 0));
    let (looks, places) = pointer(&first);
    let shape = looks[0];
    assert_eq!(looks.len(), 1);
    // The newest place only: the look first came with a place of none.
    assert_eq!(
        places,
        [At {
            display: Some(s.wire()),
            u: 65535,
            v: 0,
            shape
        }]
    );
    // The same again: nothing; on an output not streamed: nowhere, the same look.
    router.cursor(moved("Fake-1", 63, 0));
    router.cursor(look(1));
    assert_eq!(pointer(&first), (vec![], vec![]));
    router.cursor(moved("eDP-1", 5, 5));
    assert_eq!(
        pointer(&first).1,
        [At {
            shape,
            ..At::NOWHERE
        }]
    );
    router.cursor(CursorEvent::Hidden);
    assert_eq!(pointer(&first).1, [At::NOWHERE]);

    // A second headset that draws it gets the look and the place it has now.
    router.cursor(look(2));
    router.cursor(moved("Fake-1", 0, 31));
    sent(&first);
    let second = attach(&router, 2, drawing());
    router.publish(2, &BTreeSet::from([s]));
    let (looks, places) = pointer(&second);
    assert_eq!(looks.len(), 1);
    assert_eq!(
        places.last(),
        Some(&At {
            display: Some(s.wire()),
            u: 0,
            v: 65535,
            shape: looks[0]
        })
    );
    // One that does not: into the pictures, both told it is nowhere, then nothing.
    let old = attach(&router, 3, headset());
    assert!(!router.cursor_apart());
    assert_eq!(pointer(&first), (vec![], vec![At::NOWHERE]));
    assert_eq!(pointer(&second), (vec![], vec![At::NOWHERE]));
    router.cursor(moved("Fake-1", 9, 9));
    assert_eq!(pointer(&first), (vec![], vec![]));
    assert_eq!(pointer(&old), (vec![], vec![]));
    // It leaves: apart again; what the desktop says next goes out, no look again.
    router.detach(3);
    assert!(router.cursor_apart());
    router.cursor(moved("Fake-1", 0, 0));
    let (looks, places) = pointer(&first);
    assert!(looks.is_empty());
    assert_eq!(places.last().map(|a| (a.u, a.v)), Some((0, 0)));
    // The stream stops: the pointer on it is nowhere.
    router.remove_stream(s);
    assert_eq!(pointer(&first).1.last().map(|a| a.display), Some(None));
}

/// Nothing of the pointer goes where the desktop does not tell it; the last headset
/// leaving changes nothing (one that comes back starts nothing over); a headset off
/// the head is sent nothing, and hears where the pointer is once put on again.
#[test]
fn the_pointer_goes_only_where_the_desktop_tells_it_and_the_headset_is_worn() {
    let router = Router::new();
    let s = id(1);
    router.add_stream(s, Codec::H264, "Prostir-1".into());
    router.set_stream_output(s, OutputKey::new("Fake-1"), false);
    size(&router, s);
    let first = attach(&router, 1, drawing());
    router.publish(1, &BTreeSet::from([s]));
    assert!(!router.cursor_apart(), "the desktop does not tell it");
    router.cursor(look(1));
    router.cursor(moved("Fake-1", 1, 1));
    assert_eq!(pointer(&first), (vec![], vec![]));
    router.set_cursor_source(true);
    assert!(router.cursor_apart());
    router.detach(1);
    assert!(router.cursor_apart(), "kept without a headset");
    let first = attach(&router, 1, drawing());
    router.publish(1, &BTreeSet::from([s]));
    sent(&first);
    // Off the head: nothing; on again: where it is now.
    router.set_worn(1, false);
    router.cursor(moved("Fake-1", 2, 2));
    assert_eq!(pointer(&first), (vec![], vec![]));
    router.set_worn(1, true);
    let (_, places) = pointer(&first);
    assert_eq!(places.last().map(|a| a.display), Some(Some(s.wire())));
}

/// On a window panel (its frames keep the pointer) the pointer is on none of the
/// displays; over a picture scaled down to stream, its look goes scaled with it.
#[test]
fn a_panel_keeps_the_pointer_and_a_scaled_picture_scales_it() {
    let router = Router::new();
    router.set_cursor_source(true);
    let (panel, big) = (id(1), id(2));
    router.add_stream(panel, Codec::H264, "Prostir-1".into());
    router.set_stream_output(panel, OutputKey::new("Panel-1"), true);
    size(&router, panel);
    router.add_stream(big, Codec::H264, "Prostir-2".into());
    router.set_stream_output(big, OutputKey::new("Big-2"), false);
    size(&router, big);
    let first = attach(&router, 1, drawing());
    router.publish(1, &BTreeSet::from([panel, big]));
    sent(&first);
    router.cursor(look(1));
    router.cursor(moved("Panel-1", 3, 3));
    assert_eq!(pointer(&first).1.last().map(|a| a.display), Some(None));
    // The 64x32 stream of a picture of 128x64: the 2x2 look goes as 1x1.
    router.cursor(CursorEvent::Moved {
        output: OutputKey::new("Big-2"),
        x: 127,
        y: 0,
        size: (128, 64),
    });
    let sizes: Vec<(u16, u16)> = sent(&first)
        .into_iter()
        .filter_map(|m| match m {
            HostMsg::CursorShape { w, h, .. } => Some((w, h)),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, [(1, 1)]);
}

/// A headset that says where it looks (feature 6).
fn gazing() -> state::HeadsetCaps {
    state::HeadsetCaps {
        features: vec![state::caps::headset::GAZE],
        ..headset()
    }
}

/// Two streams with a frame gate each.
fn gated(router: &Router) -> [(ScreenId, Arc<Gate>); 2] {
    [id(0), id(1)].map(|s| {
        router.add_stream(s, Codec::H264, format!("Prostir-{}", s.wire().0));
        let gate = Arc::new(Gate::default());
        router.set_gate(s, gate.clone());
        (s, gate)
    })
}

/// The stream looked at gets every frame, the other half; looking at the other one
/// gives it every frame at once and keeps them for the first for 2 s. None of it opens
/// a gate (a keyframe's frame would) or asks for a keyframe.
#[test]
fn the_gaze_sets_the_frame_gates() {
    let router = Router::new();
    let [(a, gate_a), (b, gate_b)] = gated(&router);
    let outbox = attach(&router, 1, gazing());
    assert_eq!(
        (gate_a.level(), gate_b.level()),
        (Level::Full, Level::Full),
        "nothing said of the gaze yet"
    );
    router.gaze(1, Some(a));
    assert_eq!((gate_a.level(), gate_b.level()), (Level::Full, Level::Side));
    assert_eq!(router.priority_due(), None);
    router.gaze(1, Some(b));
    assert_eq!(
        (gate_a.level(), gate_b.level()),
        (Level::Full, Level::Full),
        "up at once, down later"
    );
    let due = router.priority_due().expect("the first comes down");
    assert!(due > Instant::now() + Duration::from_millis(1900));
    router.priorities();
    assert_eq!(gate_a.level(), Level::Full, "not yet");
    assert!(!gate_a.is_open() && !gate_b.is_open());
    assert!(sent(&outbox).is_empty(), "nothing to the headset");
    // A new encoder's gate starts at the stream's level.
    let again = Arc::new(Gate::default());
    again.set_level(Level::Side);
    router.gaze(1, None);
    router.set_gate(a, again.clone());
    assert_eq!(again.level(), Level::Full, "the gaze left it a moment ago");
}

/// "Focus" in every headset: 5 frames a second for the streams nobody looks at; a
/// headset without the gaze, or one taken off and put on again, sees everything; the
/// active monitor gets every frame.
#[test]
fn focus_and_headsets_that_do_not_say_where_they_look() {
    let router = Router::new();
    let [(a, gate_a), (b, gate_b)] = gated(&router);
    attach(&router, 1, gazing());
    router.set_focus_mode(1, true);
    router.gaze(1, Some(a));
    assert_eq!(
        (gate_a.level(), gate_b.level()),
        (Level::Full, Level::Glance)
    );
    assert_eq!(gate_b.fps_mhz(), 5_000);
    attach(&router, 2, headset());
    assert_eq!(gate_b.level(), Level::Full, "a headset without the gaze");
    router.set_worn(2, false);
    assert_eq!(
        gate_b.level(),
        Level::Glance,
        "taken off, it counts for nothing"
    );
    router.detach(2);
    router.set_worn(1, false);
    router.set_worn(1, true);
    assert_eq!(
        gate_b.level(),
        Level::Full,
        "put on again: its gaze is not known"
    );
    router.gaze(1, Some(a));
    assert_eq!(gate_b.level(), Level::Glance);
    router.set_active(Some(b));
    assert_eq!(gate_b.level(), Level::Full);
}

/// The report says what each gate let through, and when an encoder getting every
/// frame of a busy picture takes longer than a frame of its monitor lasts.
#[test]
fn the_report_says_what_the_gate_held() {
    let gate = GateStats {
        level: Level::Side,
        rate_mhz: 60_000,
        fps_mhz: 30_000,
        held: 300,
        unchanged: 12,
        dropped: 0,
    };
    assert_eq!(
        gate.summary(),
        "half rate 30/s, 300 frames held by rate, 12 skipped for no change, 0 dropped by a \
         busy encoder"
    );
    let full = GateStats {
        level: Level::Full,
        fps_mhz: 60_000,
        dropped: 40,
        ..gate
    };
    let stats = StreamStats {
        window_ms: 10_000,
        frames: 560,
        encode_us_avg: 20_000,
        gate: Some(full),
        ..StreamStats::default()
    };
    assert!(
        stats.summary().ends_with(
            "; gate: full rate 60/s, 300 frames held by rate, 12 skipped for no change, 40 \
             dropped by a busy encoder"
        ),
        "{}",
        stats.summary()
    );
    assert!(stats.behind(), "40 of 600 dropped");
    let keeps_up = StreamStats {
        gate: Some(GateStats { dropped: 2, ..full }),
        ..stats
    };
    assert!(!keeps_up.behind(), "a few");
    let slow_only = StreamStats {
        gate: Some(GateStats { dropped: 0, ..full }),
        ..stats
    };
    assert!(
        !slow_only.behind(),
        "slow to come out, but every frame encoded"
    );
}
