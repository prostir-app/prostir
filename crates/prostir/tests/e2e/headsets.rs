//! Several headsets at once, with the PC raised (for these tests only) above the one
//! headset a release takes: each gets every monitor, one taken off or leaving takes
//! nothing from the other, a guest does only what its rights let it, and a headset
//! that decodes less fails only for itself.

use std::time::Duration;

use prostir_proto::request::{Outcome, Request};
use prostir_proto::state::{
    Decoder, DisplayState, ErrorCode, MicDevice, Mode, Placed, Row, UpKind, UpState, permission,
    perms, role,
};
use prostir_proto::volume::HeadsetVolume;
use prostir_proto::{ClientMsg, Codec, PcGoodbye};
use prostir_testkit::{ConnectError, FIRST_VOLUME, FakeHeadset, Keys, View};

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START, connect, worn};

/// Two headsets the PC trusts, `first` and `second`, and the PC with two small
/// monitors, which takes two headsets at once; `settings` is more of its
/// `settings.txt`, with `{first}` and `{second}` for their keys. A third headset is
/// paired too.
fn two_headsets(name: &str, options: Options, settings: &str) -> (TestDir, Pc, Keys, Keys) {
    let dir = TestDir::new(name);
    dir.save_screens("next 200\nrow\nscreen 0 640x360\nscreen 1 480x272\n");
    let first = dir.pair();
    let second = dir.pair_headset("second-headset");
    dir.pair_headset("third-headset");
    let hex = |keys: &Keys| prostir_proto::secure::to_hex(&keys.identity.public);
    let settings = settings
        .replace("{first}", &hex(&first))
        .replace("{second}", &hex(&second));
    dir.save_settings(&format!("mirror off\n{settings}"));
    let options = Options {
        headsets: 2,
        ..options
    };
    let pc = Pc::start(&dir, &options);
    (dir, pc, first, second)
}

/// Connects a headset with `keys` and waits for its monitors.
fn showing(pc: &Pc, keys: &mut Keys) -> FakeHeadset {
    let mut headset = connect(pc, keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset
}

/// Frames that came for every stream since the counts started.
fn frames(headset: &FakeHeadset) -> u64 {
    headset.view().streams.values().map(|s| s.frames).sum()
}

/// Frames keep coming to `headset`: one since the counts started, waited for (a busy
/// machine can hold a moving picture up for a moment).
fn goes_on(headset: &mut FakeHeadset, what: &str) {
    headset
        .wait(SOON, what, |v| {
            v.streams.values().map(|s| s.frames).sum::<u64>() > 0
        })
        .unwrap();
}

/// Both headsets get the same monitors, the one connected first is the main one and
/// takes the commands, the other is told it is a guest though its line would make it
/// main too; a third is turned away.
#[test]
fn two_headsets_get_the_same_monitors() {
    let (dir, mut pc, mut a, mut b) = two_headsets("two", Options::default(), "");
    let mut first = showing(&pc, &mut a);
    let mut second = showing(&pc, &mut b);
    assert_eq!(first.view().order(), second.view().order());
    assert_eq!(first.view().order().len(), 2);
    assert_eq!(pc.count_log("fake desktop: created"), 2, "none made anew");
    let access = |h: &FakeHeadset| h.view().pc.access.unwrap();
    assert!(access(&first).is_main());
    assert_eq!(access(&second).role, role::GUEST);
    assert!(
        access(&second).may(perms::CHANGES),
        "with the rights of its line"
    );
    if let Some(done) = pc.ctl(&["recenter"]) {
        assert_eq!(done.code, 0, "{done:?}");
    }
    first.take_for(Duration::from_millis(500)).unwrap();
    second.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(first.view().commands.len(), 1, "the main one takes it");
    assert!(second.view().commands.is_empty());
    let mut third = dir.keys_of("third-headset");
    match FakeHeadset::connect(pc.addr, &mut third, None) {
        Err(ConnectError::Refused(text)) => assert!(text.contains("Another headset"), "{text}"),
        Err(e) => panic!("expected Refused, got {e}"),
        Ok(_) => panic!("a third headset got a session"),
    }
    // A monitor one adds streams to both.
    let added = first
        .ask(Request::AddDisplay {
            mode: Mode::new(320, 240),
        })
        .unwrap();
    let Outcome::Added { id } = added else {
        panic!("{added:?}")
    };
    for h in [&mut first, &mut second] {
        h.wait(SOON, "the new monitor showing", |v| {
            v.order().contains(&id) && v.all_monitors_show()
        })
        .unwrap();
        assert!(h.view().broken.is_empty(), "{:?}", h.view().broken);
    }
    pc.wait_log(SOON, "fake desktop: created Fake-3");
    assert_eq!(pc.count_log("panicked"), 0);
}

/// One headset taken off gets nothing while the other goes on streaming; the
/// encoders pause only once both are off. Off for good, its session ends and the
/// other's goes on, with the same monitors.
#[test]
fn one_headset_taken_off_does_not_pause_the_other() {
    let options = Options {
        fps: 15,
        ..Options::default()
    };
    let (_dir, mut pc, mut a, mut b) = two_headsets("one-off", options, "");
    let mut first = showing(&pc, &mut a);
    let mut second = showing(&pc, &mut b);
    first.send(&worn(false)).unwrap();
    // Frames already on their way still arrive; after that, nothing for it.
    first.take_for(Duration::from_secs(1)).unwrap();
    second.take_for(Duration::from_millis(100)).unwrap();
    first.reset_counts();
    second.reset_counts();
    first.take_for(Duration::from_secs(2)).unwrap();
    assert_eq!(frames(&first), 0, "nothing for the headset taken off");
    goes_on(&mut second, "the other one goes on");
    assert_eq!(pc.count_log("paused: headset not worn or gone"), 0);
    // Put on again: a fresh picture of every monitor.
    first.send(&worn(true)).unwrap();
    first
        .wait(SOON, "pictures after putting it on", |v| {
            v.streams.len() == 2 && v.streams.values().all(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    // Both off: now the PC pauses.
    first.send(&worn(false)).unwrap();
    second.send(&worn(false)).unwrap();
    pc.wait_log(SOON, "paused: headset not worn or gone");
    // The second one back on: it streams again, the first still off.
    second.send(&worn(true)).unwrap();
    second.reset_counts();
    second
        .wait(SOON, "pictures after putting it on", |v| {
            v.streams.len() == 2 && v.streams.values().all(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    // The first one off for good: its session ends, the other's goes on.
    first.wait_closed(Duration::from_secs(15)).unwrap();
    assert_eq!(first.view().goodbye, Some(PcGoodbye::TakenOff.code()));
    second.reset_counts();
    goes_on(&mut second, "the other one goes on");
    assert!(!second.view().closed);
    assert_eq!(
        pc.count_log("fake desktop: removed"),
        0,
        "the monitors stay"
    );
}

/// One headset leaving cuts nothing of the other: it goes on streaming, becomes the
/// main one and takes the commands; the monitors stay without waiting to linger.
#[test]
fn one_headset_leaving_does_not_cut_the_other() {
    let options = Options {
        fps: 15,
        ..Options::default()
    };
    let (_dir, mut pc, mut a, mut b) = two_headsets("one-leaves", options, "");
    let first = showing(&pc, &mut a);
    let mut second = showing(&pc, &mut b);
    assert_eq!(second.view().pc.access.unwrap().role, role::GUEST);
    first.disconnect();
    pc.wait_log(SOON, "headset disconnected");
    second
        .wait(SOON, "the main one now", |v| {
            v.pc.access.is_some_and(|a| a.is_main())
        })
        .unwrap();
    second.reset_counts();
    goes_on(&mut second, "it goes on streaming");
    assert!(!second.view().closed);
    assert!(second.view().ended.is_empty(), "no stream of it ended");
    assert_eq!(pc.count_log("keeping virtual monitors"), 0);
    assert_eq!(pc.count_log("paused: headset not worn or gone"), 0);
    if let Some(done) = pc.ctl(&["recenter"]) {
        assert_eq!(done.code, 0, "{done:?}");
    }
    second.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(second.view().commands.len(), 1, "commands go to it now");
    // The one that left comes back: a guest now, the same monitors.
    let back = showing(&pc, &mut a);
    assert_eq!(back.view().pc.access.unwrap().role, role::GUEST);
    assert_eq!(back.view().order(), second.view().order());
    assert_eq!(pc.count_log("fake desktop: created"), 2, "none made anew");
}

/// A guest without rights gets every monitor but changes nothing: its requests are
/// not allowed, its arrangement leaves the desktop alone, it may not move the mouse,
/// and its microphone does not reach the PC (it is not even asked for it), while the
/// main headset does all of it.
#[test]
fn a_guest_cannot_do_what_it_has_no_right_to() {
    let (_dir, mut pc, mut a, mut b) =
        two_headsets("guest", Options::default(), "access {second} guest\n");
    let mut first = showing(&pc, &mut a);
    let mut guest = showing(&pc, &mut b);
    let access = guest.view().pc.access.unwrap();
    assert_eq!((access.role, access.perms), (role::GUEST, 0));
    let refused = guest
        .ask(Request::AddDisplay {
            mode: Mode::new(320, 240),
        })
        .unwrap();
    assert!(
        matches!(&refused, Outcome::Failed { error, .. } if *error == ErrorCode::NOT_ALLOWED),
        "{refused:?}"
    );
    let ids = guest.view().order();
    let rows = |ids: &[_]| ClientMsg::Arrange {
        rows: vec![Row {
            entries: ids
                .iter()
                .rev()
                .map(|id| Placed {
                    id: *id,
                    center: None,
                    middle: None,
                })
                .collect(),
        }],
    };
    guest.send(&rows(&ids)).unwrap();
    pc.wait_log(SOON, "a guest's arrangement leaves the desktop as it is");
    guest
        .send(&ClientMsg::MouseTo {
            id: ids[0],
            u: 10,
            v: 10,
        })
        .unwrap();
    pc.wait_log(SOON, "the headset has no right to move the mouse");
    // A program records: the main headset's microphone is wanted, not the guest's.
    pc.desktop("record on");
    first
        .wait(SOON, "the microphone wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
        })
        .unwrap();
    guest.take_for(Duration::from_secs(1)).unwrap();
    let pc_state = &guest.view().pc;
    assert!(pc_state.up_wanted.get(&UpKind::Mic).is_none_or(|u| !u.on));
    assert_eq!(
        pc_state.sound.as_ref().map(|s| s.mic.clone()),
        Some(MicDevice::Off)
    );
    // The main one may.
    let added = first
        .ask(Request::AddDisplay {
            mode: Mode::new(320, 240),
        })
        .unwrap();
    assert!(matches!(added, Outcome::Added { .. }), "{added:?}");
    guest
        .wait(SOON, "the new monitor showing", |v| {
            v.order().len() == 3 && v.all_monitors_show()
        })
        .unwrap();
    assert_eq!(
        pc.count_log("the headset has no right to"),
        2,
        "the guest's only"
    );
}

/// A headset that decodes H.264 only up to 600 pixels wide gets the 640x360 monitor
/// failed (`BAD_SIZE`), never started, and the other monitor running; the other
/// headset gets both. Once that one leaves, the monitor is failed for all.
#[test]
fn a_headset_that_decodes_less_fails_only_for_itself() {
    let (_dir, mut pc, mut a, mut b) = two_headsets("decodes-less", Options::default(), "");
    let mut first = showing(&pc, &mut a);
    let wide = first.view().order()[0];
    assert_eq!(first.view().display(wide).unwrap().mode.w, 640);
    let mut small = FakeHeadset::state();
    small.caps.as_mut().unwrap().decoders = vec![Decoder {
        codec: 0,
        max_w: 600,
        max_h: 4096,
    }];
    let mut second = FakeHeadset::connect_saying(pc.addr, &mut b, &small)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    let too_big = |v: &View| {
        matches!(
            v.display(wide).map(|d| &d.state),
            Some(DisplayState::Failed { error, .. }) if *error == ErrorCode::BAD_SIZE
        )
    };
    let narrow = second.view().order()[1];
    second
        .wait(SOON, "the narrow monitor showing", |v| {
            v.streams
                .get(&narrow)
                .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    assert!(too_big(second.view()), "{:?}", second.view().displays());
    assert!(
        second
            .view()
            .states
            .iter()
            .all(|(id, s)| *id != wide || matches!(s, DisplayState::Failed { .. })),
        "failed from the snapshot on: {:?}",
        second.view().states
    );
    // The first one still has both, each with a picture since its last start. The
    // second one coming starts a still picture over, but one that sent its first
    // frames a moment before looks moving and only gets a keyframe asked of it, which
    // never comes; the first one needs none of it.
    first
        .wait(SOON, "a picture of both", |v| {
            v.all_monitors_show()
                && v.order().iter().all(|id| {
                    v.display(*id)
                        .is_some_and(|d| d.state == DisplayState::Running)
                })
        })
        .unwrap();
    assert!(!too_big(first.view()));
    second.take_for(Duration::from_secs(1)).unwrap();
    assert!(!second.view().streams.contains_key(&wide), "never started");
    assert!(
        second.view().broken.is_empty(),
        "{:?}",
        second.view().broken
    );
    pc.wait_log(SOON, "is larger than one headset decodes");
    // The one that decodes it leaves: failed for all now, the rest goes on.
    first.disconnect();
    pc.wait_log(SOON, "is larger than the headset decodes");
    second.take_for(Duration::from_secs(1)).unwrap();
    assert!(too_big(second.view()));
    assert!(!second.view().closed);
    assert!(second.view().streams.contains_key(&narrow));
}

/// A headset that takes one stream at once gets the first monitor and the second
/// failed (`LIMIT`) for itself only, never started there; the other headset gets
/// both. Once the first monitor is gone, the second streams to it too.
#[test]
fn a_headset_that_takes_fewer_streams_fails_only_for_itself() {
    let (_dir, pc, mut a, mut b) = two_headsets("fewer-streams", Options::default(), "");
    let mut first = showing(&pc, &mut a);
    let [one, two] = first.view().order()[..] else {
        panic!("two monitors")
    };
    let mut few = FakeHeadset::state();
    few.caps.as_mut().unwrap().streams_max = 1;
    let mut second = FakeHeadset::connect_saying(pc.addr, &mut b, &few)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    let limited = |v: &View| {
        matches!(
            v.display(two).map(|d| &d.state),
            Some(DisplayState::Failed { error, .. }) if *error == ErrorCode::LIMIT
        )
    };
    second
        .wait(SOON, "the first monitor showing", |v| {
            v.streams
                .get(&one)
                .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    assert!(limited(second.view()), "{:?}", second.view().displays());
    second.take_for(Duration::from_secs(1)).unwrap();
    assert!(!second.view().streams.contains_key(&two), "never started");
    first.take_for(Duration::from_millis(200)).unwrap();
    assert!(first.view().all_monitors_show() && !limited(first.view()));
    // One monitor fewer: the other one streams to it now.
    assert_eq!(
        first.ask(Request::RemoveDisplay { id: one }).unwrap(),
        Outcome::Done
    );
    second
        .wait(SOON, "the second monitor showing", |v| {
            !limited(v)
                && v.streams
                    .get(&two)
                    .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    assert!(
        second.view().broken.is_empty(),
        "{:?}",
        second.view().broken
    );
}

/// Without a main headset the first connected takes the commands, and keeps them
/// when it connects again (its old session replaced) after the second one.
#[test]
fn a_headset_back_keeps_its_place() {
    let (_dir, pc, mut a, mut b) = two_headsets(
        "keeps-place",
        Options::default(),
        "access {first} guest input\naccess {second} guest input\n",
    );
    let first = showing(&pc, &mut a);
    let mut second = showing(&pc, &mut b);
    // Its Wi-Fi went away, and it came back before the PC noticed.
    first.stall(Duration::from_secs(60));
    let mut back = showing(&pc, &mut a);
    // From the tray: it goes to the first of the guests (commands from `ctl` need a
    // main headset).
    pc.tray("recenter");
    back.take_for(Duration::from_millis(500)).unwrap();
    second.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(back.view().commands.len(), 1, "the first one still");
    assert!(second.view().commands.is_empty());
}

/// The microphone of a guest that a program records from stays the PC's while the
/// main headset connects; once nothing records, it is the main headset's.
#[test]
fn the_microphone_in_use_does_not_move() {
    let (_dir, mut pc, mut a, mut b) = two_headsets(
        "mic-stays",
        Options::default(),
        "access {first} guest mic\n",
    );
    let mut guest = showing(&pc, &mut a);
    let wanted = |h: &FakeHeadset| {
        h.view()
            .pc
            .up_wanted
            .get(&UpKind::Mic)
            .is_some_and(|u| u.on)
    };
    pc.desktop("record on");
    guest
        .wait(SOON, "the microphone wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
        })
        .unwrap();
    let mut main = showing(&pc, &mut b);
    assert!(main.view().pc.access.unwrap().is_main());
    main.take_for(Duration::from_secs(1)).unwrap();
    guest.take_for(Duration::from_millis(100)).unwrap();
    assert!(wanted(&guest), "still the guest's");
    assert!(!wanted(&main));
    assert_eq!(pc.count_log("follows another headset"), 0);
    pc.desktop("record off");
    pc.wait_log(SOON, "follows another headset");
    pc.desktop("record on");
    main.wait(SOON, "the microphone wanted", |v| {
        v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
    })
    .unwrap();
    guest.take_for(Duration::from_secs(1)).unwrap();
    assert!(!wanted(&guest), "not the guest's any more");
}

/// H.265 asked for while the other headset decodes only H.264: done, kept as chosen,
/// and the streams stay in H.264 (one switch at most, none here); once that headset
/// leaves, they go in H.265. Only where the PC encodes H.265 on its graphics card.
#[test]
fn a_codec_one_headset_cannot_decode_waits_for_it_to_leave() {
    let options = Options {
        software: false,
        ..Options::default()
    };
    let (_dir, mut pc, mut a, mut b) =
        two_headsets("codec-waits", options, "codec h264\nsoftware off\n");
    let mut first = showing(&pc, &mut a);
    let can = first.view().encoding().unwrap().choices.clone();
    if !can.contains(&(Codec::H265, true)) {
        eprintln!("no H.265 on the graphics card here ({can:?}): nothing to try");
        return;
    }
    let mut h264 = FakeHeadset::state();
    h264.caps
        .as_mut()
        .unwrap()
        .decoders
        .retain(|d| d.codec == 0);
    let mut second = FakeHeadset::connect_saying(pc.addr, &mut b, &h264)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    second
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let switches = pc.count_log("(from the headset menu)");
    let asked = first
        .ask(Request::SetEncoding {
            bitrate: None,
            codec: Some(Codec::H265),
            software: Some(false),
            sharp: None,
        })
        .unwrap();
    assert_eq!(asked, Outcome::Done);
    let encoding = first.view().encoding().unwrap();
    assert_eq!(encoding.wanted.codec, Some(Codec::H265), "kept as chosen");
    assert_eq!(encoding.using.map(|u| u.0), Some(Codec::H264));
    first.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(
        pc.count_log("(from the headset menu)"),
        switches,
        "no switch"
    );
    assert!(
        second
            .view()
            .streams
            .values()
            .all(|s| s.codec == Codec::H264)
    );
    second.disconnect();
    pc.wait_log(SOON, "every headset decodes H265");
    first
        .wait(SOON, "every monitor in H.265", |v| {
            v.order().iter().all(|id| {
                v.streams
                    .get(id)
                    .is_some_and(|s| s.codec == Codec::H265 && s.has_picture())
            })
        })
        .unwrap();
}

/// Every session ending at once (the tray forgets the headsets) starts no stream over
/// on the way for the headsets still to end: the codec waits in H.264 for the one
/// that does not decode H.265 and is not switched back for the others meanwhile.
/// Only where the PC encodes H.265 on its graphics card.
#[test]
fn sessions_ending_together_switch_no_codec() {
    let options = Options {
        software: false,
        ..Options::default()
    };
    let (_dir, mut pc, mut a, mut b) =
        two_headsets("end-together", options, "codec h265\nsoftware off\n");
    let mut first = showing(&pc, &mut a);
    let can = first.view().encoding().unwrap().choices.clone();
    if !can.contains(&(Codec::H265, true)) {
        eprintln!("no H.265 on the graphics card here ({can:?}): nothing to try");
        return;
    }
    let mut h264 = FakeHeadset::state();
    h264.caps
        .as_mut()
        .unwrap()
        .decoders
        .retain(|d| d.codec == 0);
    let mut second = FakeHeadset::connect_saying(pc.addr, &mut b, &h264)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    second
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(SOON, "a headset does not decode H265");
    pc.tray("forget");
    first.wait_closed(SOON).unwrap();
    second.wait_closed(SOON).unwrap();
    pc.wait_log(SOON, "keeping virtual monitors");
    assert_eq!(pc.count_log("every headset decodes"), 0);
}

/// While the picture moves, a second headset connecting, or put on again, gets its
/// fresh picture as a keyframe: no stream starts over, so the first headset sees no
/// hiccup.
#[test]
fn a_headset_coming_starts_no_stream_over_for_the_other() {
    let options = Options {
        fps: 15,
        ..Options::default()
    };
    let (_dir, pc, mut a, mut b) = two_headsets("no-hiccup", options, "");
    let mut first = showing(&pc, &mut a);
    first.take_for(Duration::from_secs(1)).unwrap();
    let before = pc.source_starts();
    let mut second = showing(&pc, &mut b);
    second.send(&worn(false)).unwrap();
    second.take_for(Duration::from_secs(1)).unwrap();
    second.reset_counts();
    second.send(&worn(true)).unwrap();
    second
        .wait(SOON, "pictures after putting it on", |v| {
            v.streams.len() == 2 && v.streams.values().all(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    first.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(pc.source_starts(), before, "no stream started over");
    assert!(first.view().broken.is_empty(), "{:?}", first.view().broken);
}

/// The headset that becomes the main one, or connects while another listens, does
/// not move the PC's volume to its own level: it is asked to take the PC's.
#[test]
fn a_new_main_headset_takes_the_pc_volume() {
    let (_dir, mut pc, mut a, mut b) = two_headsets("volume-stays", Options::default(), "");
    let mut first = showing(&pc, &mut a);
    // Until the PC took its new sound device in, its first reading may come later and
    // put the main headset's level on it, by then the second one's (90).
    crate::sound::wait_first_volume(&mut pc, &mut first);
    let mut loud = FakeHeadset::state();
    loud.volume = Some(HeadsetVolume::new(90, false).now());
    let mut second = FakeHeadset::connect_saying(pc.addr, &mut b, &loud)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    second
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    first.disconnect();
    second
        .wait(SOON, "the main one now", |v| {
            v.pc.access.is_some_and(|a| a.is_main())
        })
        .unwrap();
    second
        .wait(SOON, "asked for the PC's level", |v| {
            v.volume_asks.iter().any(|a| a.level == FIRST_VOLUME)
        })
        .unwrap();
    assert_eq!(pc.count_log("fake desktop: volume set to 90"), 0, "no jump");
}

/// A monitor added while a headset that decodes less is there is never running for
/// it before its size is known: starting, then failed (`BAD_SIZE`).
#[test]
fn a_monitor_too_big_for_a_headset_is_never_running_for_it() {
    let (_dir, pc, mut a, mut b) = two_headsets("pending-size", Options::default(), "");
    let mut first = showing(&pc, &mut a);
    let mut small = FakeHeadset::state();
    small.caps.as_mut().unwrap().decoders = vec![Decoder {
        codec: 0,
        max_w: 700,
        max_h: 4096,
    }];
    let mut second = FakeHeadset::connect_saying(pc.addr, &mut b, &small)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    second
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let added = first
        .ask(Request::AddDisplay {
            mode: Mode::new(800, 450),
        })
        .unwrap();
    let Outcome::Added { id } = added else {
        panic!("{added:?}")
    };
    second
        .wait(SOON, "the new monitor failed for it", |v| {
            matches!(
                v.display(id).map(|d| &d.state),
                Some(DisplayState::Failed { error, .. }) if *error == ErrorCode::BAD_SIZE
            )
        })
        .unwrap();
    let states: Vec<_> = second
        .view()
        .states
        .iter()
        .filter(|(d, _)| *d == id)
        .collect();
    assert!(
        states.iter().all(|(_, s)| *s != DisplayState::Running),
        "{states:?}"
    );
    first
        .wait(SOON, "the new monitor showing", |v| {
            v.streams
                .get(&id)
                .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
}

/// The tray says the microphone has no permission for the headset whose microphone
/// the PC takes (a guest here, the main headset having no right to it), and counts
/// the other headset.
#[test]
fn the_tray_shows_the_microphone_the_pc_takes() {
    let (_dir, mut pc, mut a, mut b) = two_headsets(
        "tray-mic",
        Options::default(),
        "access {first} guest mic\naccess {second} main input changes\n",
    );
    let mut guest = showing(&pc, &mut a);
    let main = showing(&pc, &mut b);
    assert!(main.view().pc.access.unwrap().is_main());
    pc.wait_log(
        SOON,
        "fake tray: 1 other headset(s), microphone denied: false",
    );
    let denied = UpState {
        kind: UpKind::Mic,
        switch: true,
        permission: permission::DENIED,
    };
    guest.send(&ClientMsg::UpState { part: denied }).unwrap();
    pc.wait_log(
        SOON,
        "fake tray: 1 other headset(s), microphone denied: true",
    );
}
