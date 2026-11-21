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
    router.add_stream(s0, Codec::H265, "test".into(), false);
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
    router.add_stream(s0, Codec::H264, "test".into(), false);
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
    router.add_stream(s0, Codec::H265, "test".into(), false);
    router.remove_stream(s0);
    assert_eq!(
        sent(&outbox),
        [HostMsg::StreamEnd {
            id: s0.wire(),
            generation: second
        }]
    );
    // Never shown: nothing to end.
    router.add_stream(s1, Codec::H265, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
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
    router.add_stream(s, Codec::H264, "test".into(), false);
    keyframe(&router, s);
    let still = Duration::from_millis(50);
    assert_eq!(
        router.still_keyframes(still),
        0,
        "nothing new since the keyframe"
    );
    timed(&router, s, false, false);
    assert_eq!(router.still_keyframes(still), 0, "not still yet");
    std::thread::sleep(still + Duration::from_millis(10));
    // The source's keepalive: the same picture, it is still still.
    timed(&router, s, false, true);
    assert_eq!(router.still_keyframes(still), 1);
    assert_eq!(router.still_keyframes(still), 0, "once");
    timed(&router, s, true, true);
    std::thread::sleep(still + Duration::from_millis(10));
    assert_eq!(
        router.still_keyframes(still),
        0,
        "still since that keyframe"
    );
    // It moved again, and a keyframe came anyway before it went still: none asked.
    timed(&router, s, false, false);
    timed(&router, s, true, false);
    std::thread::sleep(still + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(still), 0);
    // It moved again: one more once still.
    timed(&router, s, false, false);
    std::thread::sleep(still + Duration::from_millis(10));
    assert_eq!(router.still_keyframes(still), 1);
}

/// A keyframe that goes out answers a request that waited for the window.
#[test]
fn a_keyframe_going_out_answers_a_waiting_request() {
    let router = Router::new();
    let s = id(6);
    let _outbox = attach(&router, 1, headset());
    router.publish(1, &BTreeSet::from([s]));
    router.add_stream(s, Codec::H264, "test".into(), false);
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
