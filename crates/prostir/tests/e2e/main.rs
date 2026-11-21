//! End-to-end tests: the real PC app (built with the fake desktop) driven by a fake
//! headset over a real encrypted connection. Run with `cargo e2e` (an alias for
//! `cargo test -p prostir --no-default-features --features fake-desktop --test
//! e2e`). Needs GStreamer
//! with x264enc (plugins-ugly), h264parse (plugins-bad) and the base plugins.

mod api;
mod asleep;
mod control;
mod desk;
mod encoders;
mod failures;
mod goodbye;
mod headsets;
mod keyframes;
mod link;
mod mcp;
mod measure;
mod pc;
mod requests;
mod resize;
mod saved;
mod sound;
mod start;
mod window;

use std::collections::BTreeMap;
use std::time::Duration;

use pc::{Options, Pc, TestDir};
use prostir_proto::request::{Outcome, Primary, Request};
use prostir_proto::state::{DisplayId, Mode, Placed, Presence, Row};
use prostir_proto::{ClientMsg, Codec};
use prostir_testkit::{ConnectError, FakeHeadset, Keys, View};

/// Long enough for a slow CI machine to start GStreamer and the encoders.
pub const START: Duration = Duration::from_secs(30);
/// A change asked for by the headset shows within this.
pub const SOON: Duration = Duration::from_secs(10);

/// Two small monitors, so the CPU encoder keeps up anywhere.
pub const TWO_MONITORS: &str = "row\n640x360\n480x272\nmirror off\n";

/// Every fake monitor's source started exactly once more between `before` and `after`
/// (see [`Pc::source_starts`]).
fn assert_started_once(before: &BTreeMap<String, usize>, after: &BTreeMap<String, usize>) {
    assert!(!before.is_empty(), "no monitor source started");
    let once: BTreeMap<String, usize> = before.iter().map(|(k, n)| (k.clone(), n + 1)).collect();
    assert_eq!(
        after, &once,
        "each monitor's stream starts over exactly once"
    );
}

pub fn connect(pc: &Pc, keys: &mut Keys) -> FakeHeadset {
    FakeHeadset::connect(pc.addr, keys, None).unwrap_or_else(|e| panic!("connecting: {e}"))
}

/// A started PC app and a headset it already trusts, connected and showing every
/// monitor.
pub fn session(name: &str, options: &Options) -> (TestDir, Pc, Keys, FakeHeadset) {
    let dir = TestDir::new(name);
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    (dir, pc, keys, headset)
}

/// Takes the headset off (`false`) or puts it on.
pub fn worn(worn: bool) -> ClientMsg {
    ClientMsg::Presence {
        part: Presence { worn },
    }
}

#[test]
fn a_trusted_headset_gets_its_monitors_and_sound() {
    let (_dir, pc, _keys, mut headset) = session("trusted", &Options::default());
    assert_eq!(headset.language, "en");
    let view = headset.view();
    assert_eq!(view.order().len(), 2);
    for id in &view.order() {
        let s = &view.streams[id];
        assert_eq!(s.codec, Codec::H264, "the CPU encoder makes H.264");
        assert_eq!(
            s.first_is_key,
            Some(true),
            "{} starts with a keyframe",
            s.name
        );
        assert_eq!(s.bad_keyframes, 0);
    }
    assert_eq!(
        (
            view.streams[&view.order()[0]].width,
            view.streams[&view.order()[1]].width
        ),
        (640, 480)
    );
    let encoding = view.encoding().expect("the PC says how it encodes");
    assert_eq!(encoding.using, Some((Codec::H264, false)), "on the CPU");
    assert!(view.broken.is_empty(), "{:?}", view.broken);
    headset.wait(SOON, "sound", |v| v.audio_bytes > 0).unwrap();
    pc.quit();
}

#[test]
fn an_unknown_headset_must_pair_first() {
    let dir = TestDir::new("pairing");
    dir.save_monitors(TWO_MONITORS);
    let pc = Pc::start(
        &dir,
        &Options {
            typed_code: Some("314159".into()),
            ..Options::default()
        },
    );
    let mut keys = dir.headset_keys();
    // Not paired, and no code offered: no session.
    let refused = FakeHeadset::connect(pc.addr, &mut keys, None);
    assert!(matches!(refused, Err(ConnectError::NotPaired)));
    // The person at the PC types another code than the headset shows.
    let wrong = FakeHeadset::connect(pc.addr, &mut keys, Some(&mut || "271828".into()));
    assert!(
        matches!(wrong, Err(ConnectError::PairingFailed(_))),
        "{:?}",
        wrong.err()
    );
    assert!(keys.trusted.is_empty());
    // The right code: paired, and trusted from then on without a code.
    let mut headset = FakeHeadset::connect(pc.addr, &mut keys, Some(&mut || "314159".into()))
        .unwrap_or_else(|e| panic!("pairing: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset.disconnect();
    let mut again = connect(&pc, &mut keys);
    again
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
}

/// A headset that forgot the PC (the PC still trusts it) pairs again before the session
/// starts: the versions come first, then pairing, then the session.
#[test]
fn a_headset_that_forgot_the_pc_pairs_again() {
    let dir = TestDir::new("forgot");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    keys.trusted.clear().unwrap();
    let pc = Pc::start(
        &dir,
        &Options {
            typed_code: Some("161803".into()),
            ..Options::default()
        },
    );
    let mut headset = FakeHeadset::connect(pc.addr, &mut keys, Some(&mut || "161803".into()))
        .unwrap_or_else(|e| panic!("pairing again: {e}"));
    assert_eq!(keys.trusted.len(), 1, "the headset trusts the PC again");
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
}

/// A PC that forgot the headset (the headset still trusts it) asks it to pair when it
/// opens the session: pairing then, and the session after it.
#[test]
fn a_pc_that_forgot_the_headset_pairs_again() {
    let dir = TestDir::new("pc-forgot");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    dir.forget_headsets();
    let pc = Pc::start(
        &dir,
        &Options {
            typed_code: Some("141421".into()),
            ..Options::default()
        },
    );
    let refused = FakeHeadset::connect(pc.addr, &mut keys, None);
    assert!(matches!(refused, Err(ConnectError::NotPaired)));
    let mut headset = FakeHeadset::connect(pc.addr, &mut keys, Some(&mut || "141421".into()))
        .unwrap_or_else(|e| panic!("pairing again: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset.disconnect();
    let mut again = connect(&pc, &mut keys);
    again
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
}

/// A layout that lists one monitor many times is not a layout: the PC ends the session
/// instead of taking it in (it once turned into an order longer than a message may
/// hold, which stopped the PC from sending anything), and the headset connects again
/// to the same two monitors.
#[test]
fn a_layout_with_a_repeated_monitor_is_refused() {
    let (_dir, pc, mut keys, mut headset) = session("repeated", &Options::default());
    let first = headset.view().order()[0];
    let entry = Placed {
        id: first,
        center: None,
        middle: None,
    };
    headset
        .send(&ClientMsg::Arrange {
            rows: vec![Row {
                entries: vec![entry; 16],
            }],
        })
        .unwrap();
    headset.wait_closed(SOON).unwrap();
    let mut back = connect(&pc, &mut keys);
    back.wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(back.view().order().len(), 2);
    assert_eq!(pc.count_log("panicked"), 0, "no thread of the PC app died");
}

/// A profile's monitors from the headset lay the desktop out once, not once for every
/// monitor made or removed on the way.
#[test]
fn a_new_set_of_monitors_is_laid_out_once() {
    let (_dir, mut pc, _keys, mut headset) = session("layout-once", &Options::default());
    // Laid out once the session settles.
    headset.take_for(Duration::from_secs(1)).unwrap();
    let before = pc.count_log("fake desktop: placed");
    let sizes = [(800, 450), (640, 360), (480, 272), (320, 240)];
    let outcome = headset
        .ask(Request::SetDisplays {
            modes: sizes.iter().map(|&(w, h)| Mode::new(w, h)).collect(),
            primary: Some(Primary::Index { index: 1 }),
            shown_physical: Vec::new(),
        })
        .unwrap();
    let Outcome::Ids { ids } = outcome else {
        panic!("the monitors' numbers, not {outcome:?}")
    };
    assert_eq!(ids.len(), 4);
    assert_eq!(headset.view().order(), ids, "in the order of the sizes");
    assert_eq!(headset.view().primary(), Some(ids[1]));
    headset
        .wait(SOON, "four monitors showing", |v| {
            v.order().len() == 4 && v.all_monitors_show()
        })
        .unwrap();
    headset.take_for(Duration::from_secs(1)).unwrap();
    let line = pc.wait_log_after(SOON, "fake desktop: placed", before);
    assert_eq!(pc.count_log("fake desktop: placed"), before + 1, "{line}");
    // Side by side, from the left, in the order asked for.
    assert!(line.contains("Fake-"), "{line}");
}

/// A monitor added: the list with it comes before the reply that names it (both in the
/// control lane), its start before its frames (in its own lane). Removed: the list
/// without it before the reply, and its stream ends. The last one stays.
#[test]
fn monitors_are_added_and_removed() {
    let (_dir, _pc, _keys, mut headset) = session("add-remove", &Options::default());
    let from = headset.view().arrivals.len();
    let req = headset
        .request(Request::AddDisplay {
            mode: Mode::new(800, 450),
        })
        .unwrap();
    let Outcome::Added { id: added } = headset.reply(req, SOON).unwrap() else {
        panic!("a new monitor")
    };
    headset
        .wait(SOON, "a third monitor showing", |v| {
            v.order().len() == 3 && v.all_monitors_show()
        })
        .unwrap();
    let arrivals = &headset.view().arrivals[from..];
    let at = |a: &prostir_testkit::Arrival| arrivals.iter().position(|x| x == a).unwrap();
    let listed = arrivals
        .iter()
        .position(|a| matches!(a, prostir_testkit::Arrival::Displays(ids) if ids.contains(&added)))
        .expect("a list with the new monitor");
    assert!(
        listed < at(&prostir_testkit::Arrival::Reply(req)),
        "{arrivals:?}"
    );
    let generation = headset.view().streams[&added].generation;
    assert!(
        at(&prostir_testkit::Arrival::StreamStart(added, generation))
            < at(&prostir_testkit::Arrival::FirstFrame(added, generation)),
        "{arrivals:?}"
    );
    assert_eq!(headset.view().streams[&added].width, 800);
    assert_eq!(headset.view().order().last(), Some(&added), "at the end");

    let from = headset.view().arrivals.len();
    let req = headset
        .request(Request::RemoveDisplay { id: added })
        .unwrap();
    assert_eq!(headset.reply(req, SOON).unwrap(), Outcome::Done);
    let arrivals = headset.view().arrivals[from..].to_vec();
    let gone = arrivals
        .iter()
        .position(|a| matches!(a, prostir_testkit::Arrival::Displays(ids) if !ids.contains(&added)))
        .expect("a list without it");
    let replied = arrivals
        .iter()
        .position(|a| *a == prostir_testkit::Arrival::Reply(req))
        .unwrap();
    assert!(gone < replied, "{arrivals:?}");
    headset
        .wait(SOON, "its stream ended", |v| v.ended.contains(&added))
        .unwrap();
    assert_eq!(headset.view().removed, [added]);
    assert_eq!(headset.view().order().len(), 2);
    // The last one stays: the PC says why.
    let [first, last] = headset.view().order()[..] else {
        panic!("two monitors")
    };
    assert_eq!(
        headset.ask(Request::RemoveDisplay { id: first }).unwrap(),
        Outcome::Done
    );
    let refused = headset.ask(Request::RemoveDisplay { id: last }).unwrap();
    let Outcome::Failed { detail, .. } = refused else {
        panic!("the last monitor stays, not {refused:?}")
    };
    assert!(detail.contains("last monitor"), "{detail}");
    assert_eq!(headset.view().order(), [last]);
    assert!(
        headset.view().broken.is_empty(),
        "{:?}",
        headset.view().broken
    );
}

/// The session opens with the snapshot of everything the PC has, before any stream:
/// the monitors with their numbers, sizes, kinds and places on the desktop, the
/// primary one, how it encodes, what it allows and can do.
#[test]
fn the_session_opens_with_a_snapshot() {
    let (_dir, pc, _keys, mut headset) = session("snapshot", &Options::default());
    let view = headset.view();
    let displays = view.displays();
    assert_eq!(displays.len(), 2);
    assert_eq!(displays[0].id, DisplayId(0));
    assert_eq!((displays[0].mode.w, displays[0].mode.h), (640, 360));
    assert_eq!((displays[1].mode.w, displays[1].mode.h), (480, 272));
    assert!(
        displays
            .iter()
            .all(prostir_proto::state::Display::is_virtual)
    );
    assert!(view.primary().is_some());
    let encoding = view.encoding().unwrap();
    assert!(encoding.choices.contains(&(Codec::H264, false)));
    let limits = view.pc.limits.as_ref().unwrap();
    assert_eq!(limits.virtual_max, 6);
    // H.264 on the CPU: as wide as the headset decodes it.
    assert_eq!(limits.max, (4096, 4096));
    assert!(!limits.sizes.contains(&Mode::new(5120, 1440)));
    assert_eq!(view.pc.info.as_ref().unwrap().desktop, "fake");
    assert!(view.pc.access.unwrap().is_main());
    // Where the fake desktop put them, once it laid them out.
    headset
        .wait(SOON, "both on the desktop", |v| {
            v.displays().iter().all(|d| d.desk.is_some())
        })
        .unwrap();
    let placed = saved::placed(&pc);
    for d in headset.view().displays() {
        let desk = d.desk.unwrap();
        let name = format!("Fake-{}", d.id.0 + 1);
        assert!(
            placed.contains(&(name, desk.x, desk.y)),
            "{desk:?} not in {placed:?}"
        );
    }
}

/// A request of a kind the PC does not know gets its reply all the same, with its
/// number: unsupported.
#[test]
fn an_unknown_request_is_unsupported() {
    let (_dir, _pc, _keys, mut headset) = session("unknown-request", &Options::default());
    headset
        .send(&ClientMsg::Request {
            req: prostir_proto::state::ReqId(41),
            asked: prostir_proto::request::Asked::Unknown { kind: 200 },
        })
        .unwrap();
    let outcome = headset
        .reply(prostir_proto::state::ReqId(41), SOON)
        .unwrap();
    assert!(
        matches!(outcome, Outcome::Failed { error, .. } if error == prostir_proto::state::ErrorCode::UNSUPPORTED),
        "{outcome:?}"
    );
}

/// A headset that shows neither window panels nor the PC's own displays, and decodes
/// only H.264, is told about neither, and not about a codec it does not decode.
#[test]
fn a_headset_hears_only_what_it_can_show() {
    let dir = TestDir::new("fewer-caps");
    dir.save_monitors("row\n640x360\nmirror on\ncodec h265\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &Options::default());
    let mut state = FakeHeadset::state();
    let caps = state.caps.as_mut().unwrap();
    caps.features.clear();
    caps.decoders.retain(|d| d.codec == 0);
    let mut headset = FakeHeadset::connect_saying(pc.addr, &mut keys, &state)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let view = headset.view();
    assert!(
        view.displays()
            .iter()
            .all(prostir_proto::state::Display::is_virtual)
    );
    let encoding = view.encoding().unwrap();
    assert_eq!(
        encoding.wanted.codec, None,
        "H.265 is saved, not decoded here"
    );
    assert!(encoding.choices.iter().all(|(c, _)| *c == Codec::H264));
    let limits = view.pc.limits.as_ref().unwrap();
    assert!(!limits.sizes.contains(&Mode::new(5120, 1440)));
}

/// Taken off for a moment, the monitors pause and come back; for longer than 10 s, the
/// PC gives the desktop back and makes them anew when the headset returns. Meanwhile
/// the PC logs its numbers for each stream. A moving screen, so a pause that still
/// sends frames shows (a still screen coming back is the next test).
#[test]
fn taking_the_headset_off_pauses_and_then_ends_the_session() {
    let options = Options {
        fps: 15,
        ..Options::default()
    };
    let (_dir, mut pc, mut keys, mut headset) = session("presence", &options);
    headset.send(&worn(false)).unwrap();
    // Frames already on their way still arrive; after that, nothing.
    headset.take_for(Duration::from_secs(1)).unwrap();
    headset.reset_counts();
    headset.take_for(Duration::from_secs(2)).unwrap();
    let paused: u64 = headset.view().streams.values().map(|s| s.frames).sum();
    assert_eq!(paused, 0, "no frames while the headset is off");
    // Back on: a fresh picture on every monitor (resumed streams start with a keyframe).
    headset.send(&worn(true)).unwrap();
    headset
        .wait(SOON, "pictures after putting it on", |v| {
            v.streams.len() == 2 && v.streams.values().all(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    // The numbers each stream reports every 10 s.
    let line = pc.wait_log(Duration::from_secs(15), "Prostir-1: sent");
    assert!(line.contains("waited in outbox p50"), "{line}");
    let reports: u32 = line
        .rsplit_once(" reports)")
        .and_then(|(head, _)| head.rsplit_once('('))
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(0);
    assert!(reports > 0, "the headset reported shown frames: {line}");
    pc.wait_log(Duration::from_secs(1), "sound: sent");
    // Pings both ways: the round trip in the log.
    let line = pc.wait_log(Duration::from_secs(1), "link: round trip");
    assert!(line.contains("p50"), "{line}");
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert!(headset.view().pings > 5, "the PC pings every second");
    assert!(!headset.view().round_trips.is_empty(), "and answers");
    // Off for good: the session ends, and the monitors go.
    headset.send(&worn(false)).unwrap();
    headset.wait_closed(Duration::from_secs(15)).unwrap();
    assert_eq!(
        headset.view().goodbye,
        Some(prostir_proto::PcGoodbye::TakenOff.code())
    );
    pc.wait_log(
        SOON,
        "removing virtual monitors until the headset is worn again",
    );
    pc.wait_log(SOON, "fake desktop: removed Fake-1");
    // Worn again, the headset connects and the monitors are made anew.
    let mut back = connect(&pc, &mut keys);
    back.wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(pc.count_log("fake desktop: created Fake-1 "), 2);
}

/// Put on again after a moment, a still screen shows on every monitor again though
/// nothing on it changed: the PC starts each stream over once. Before 0.1.1 the
/// picture in the headset stayed frozen.
#[test]
fn a_still_screen_shows_again_after_putting_the_headset_on() {
    let (_dir, mut pc, _keys, mut headset) = session("still", &Options::default());
    headset.send(&worn(false)).unwrap();
    pc.wait_log(SOON, "paused: headset not worn or gone");
    headset.take_for(Duration::from_secs(1)).unwrap();
    headset.reset_counts();
    let before = pc.source_starts();
    headset.send(&worn(true)).unwrap();
    headset
        .wait(SOON, "pictures after putting it on", |v| {
            v.streams.len() == 2 && v.streams.values().all(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_started_once(&before, &pc.source_starts());
}

/// The monitors kept after the headset left send their picture to the headset that
/// comes back, though the (still) screen has not changed: the PC starts their streams
/// over, once each, with the headset there to get the picture. Before 0.1.1 the
/// headset got no monitors at all; in 0.1.1 at first each stream started over twice,
/// once before the headset was attached.
#[test]
fn monitors_kept_after_a_disconnect_show_again() {
    let (_dir, mut pc, mut keys, headset) = session("linger", &Options::default());
    headset.disconnect();
    pc.wait_log(SOON, "keeping virtual monitors for 30 s");
    let before = pc.source_starts();
    let mut back = connect(&pc, &mut keys);
    back.wait(SOON, "every kept monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(back.view().order().len(), 2);
    // Time for a late second start to show in the log.
    back.take_for(Duration::from_secs(1)).unwrap();
    assert_started_once(&before, &pc.source_starts());
    // The same monitors, not new ones.
    assert_eq!(pc.count_log("fake desktop: created"), 2);
    assert_eq!(pc.count_log("fake desktop: removed"), 0);
}

/// Under H.265 the headset decodes a 5120x1440 monitor, under H.264 it does not
/// (4096 at most): asked for H.264 then, the PC refuses with `BAD_SIZE` and keeps
/// encoding as it does. Only where the PC encodes H.265 on its graphics card.
#[test]
fn a_codec_the_headset_cannot_show_a_monitor_in_is_refused() {
    let (_dir, _pc, _keys, mut headset) = session("codec-size", &Options::default());
    let can = headset.view().encoding().unwrap().choices.clone();
    if !can.contains(&(Codec::H265, true)) {
        eprintln!("no H.265 on the graphics card here ({can:?}): nothing to try");
        return;
    }
    let set = |codec| Request::SetEncoding {
        bitrate: None,
        codec: Some(codec),
        software: Some(false),
        sharp: None,
    };
    assert_eq!(headset.ask(set(Codec::H265)).unwrap(), Outcome::Done);
    let wide = Mode::new(5120, 1440);
    let added = headset.ask(Request::AddDisplay { mode: wide }).unwrap();
    let Outcome::Added { id } = added else {
        panic!("{added:?}")
    };
    headset
        .wait(SOON, "the wide monitor streams", |v| {
            v.streams
                .get(&id)
                .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    let refused = headset.ask(set(Codec::H264)).unwrap();
    assert!(
        matches!(&refused, Outcome::Failed { error, .. } if *error == prostir_proto::state::ErrorCode::BAD_SIZE),
        "{refused:?}"
    );
    let using = headset.view().encoding().unwrap().using;
    assert_eq!(using, Some((Codec::H265, true)), "encoding as before");
    assert!(headset.view().broken.is_empty());
}

/// A monitor and a display of the PC too big for the headset in H.265 (it decodes
/// H.265 up to 4096, H.264 up to 8192) stand failed; switched to H.264 they stream,
/// and `Displays` says so. Only where the PC encodes H.265 on its graphics card.
#[test]
fn displays_too_big_in_one_codec_stream_in_another() {
    use prostir_proto::state::{Decoder, DisplayState, ErrorCode};
    let options = Options {
        physical: "HDMI-A-1=Wide 0x1 0@5120x1440".into(),
        software: false,
        ..Options::default()
    };
    let dir = TestDir::new("codec-fits");
    dir.save_screens("next 200\nrow\nscreen 0 640x360\nscreen 1 5120x1440\n");
    dir.save_settings("codec h265\nsoftware off\nmirror on\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let mut state = FakeHeadset::state();
    let decoder = |codec, max_w, max_h| Decoder {
        codec,
        max_w,
        max_h,
    };
    state.caps.as_mut().unwrap().decoders = vec![decoder(0, 8192, 4320), decoder(1, 4096, 4096)];
    let mut headset = FakeHeadset::connect_saying(pc.addr, &mut keys, &state)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    let using = headset.view().encoding().unwrap().using;
    if using != Some((Codec::H265, true)) {
        eprintln!("not H.265 on the graphics card here ({using:?}): nothing to try");
        return;
    }
    let wide = |v: &View| -> Vec<(DisplayId, DisplayState)> {
        v.displays()
            .iter()
            .filter(|d| d.mode.w == 5120 || d.is_physical())
            .map(|d| (d.id, d.state.clone()))
            .collect()
    };
    headset
        .wait(SOON, "both wide ones failed, too big", |v| {
            let w = wide(v);
            w.len() == 2
                && w.iter().all(|(_, s)| {
                    matches!(s, DisplayState::Failed { error, .. } if *error == ErrorCode::BAD_SIZE)
                })
        })
        .unwrap();
    // No flap: they stay failed while nothing changes.
    headset.take_for(Duration::from_secs(2)).unwrap();
    assert!(
        wide(headset.view())
            .iter()
            .all(|(_, s)| matches!(s, DisplayState::Failed { .. })),
        "{:?}",
        wide(headset.view())
    );
    // Connected again, they are failed from the snapshot on, never running between.
    headset.disconnect();
    let mut headset = FakeHeadset::connect_saying(pc.addr, &mut keys, &state)
        .unwrap_or_else(|e| panic!("connecting again: {e}"));
    headset.take_for(Duration::from_secs(2)).unwrap();
    let wide_ids: Vec<DisplayId> = wide(headset.view()).iter().map(|(id, _)| *id).collect();
    assert_eq!(wide_ids.len(), 2);
    let flapped: Vec<_> = headset
        .view()
        .states
        .iter()
        .filter(|(id, s)| wide_ids.contains(id) && !matches!(s, DisplayState::Failed { .. }))
        .collect();
    assert!(flapped.is_empty(), "{flapped:?}");
    // By their numbers from here: the encoder may make a wide one narrower in H.264
    // (graphics cards that encode H.264 only up to 4096 scale it down).
    let ids: Vec<DisplayId> = wide(headset.view()).iter().map(|(id, _)| *id).collect();
    let state_of = |v: &View, id: DisplayId| v.display(id).map(|d| d.state.clone());
    let switched = headset
        .ask(Request::SetEncoding {
            bitrate: None,
            codec: Some(Codec::H264),
            software: Some(false),
            sharp: None,
        })
        .unwrap();
    assert_eq!(switched, Outcome::Done);
    headset
        .wait(SOON, "both wide ones running and streaming in H.264", |v| {
            ids.iter().all(|id| {
                state_of(v, *id) == Some(DisplayState::Running)
                    && v.streams
                        .get(id)
                        .is_some_and(|st| st.codec == Codec::H264 && st.has_picture())
            })
        })
        .unwrap();
    assert!(
        headset.view().broken.is_empty(),
        "{:?}",
        headset.view().broken
    );
}

/// Another codec from the headset menu restarts every stream with what the PC really
/// encodes with now, as far as it said it can (`choices` in `Encoding`): H.265 on
/// the graphics card where it can, else H.264 there. Asking for what is in use
/// already changes nothing: on a PC without a graphics card encoder (CI), asking for
/// H.265 on the card gives the CPU's H.264, which it encodes with already.
#[test]
fn a_codec_switch_restarts_every_stream() {
    let (_dir, _pc, _keys, mut headset) = session("codec", &Options::default());
    let can = headset
        .view()
        .encoding()
        .expect("the PC says how it encodes")
        .choices
        .clone();
    let starts = |headset: &FakeHeadset| -> Vec<(DisplayId, u32)> {
        let streams = &headset.view().streams;
        streams.iter().map(|(id, s)| (*id, s.starts)).collect()
    };
    // Asks for `codec` (on the CPU with `software`) and returns what the PC encodes
    // with after it (its `Encoding` comes before the reply), once every stream that
    // was to start over (`restart`) did, with the new codec and a picture.
    let ask = |headset: &mut FakeHeadset, codec, software, restart: bool| {
        let before = starts(headset);
        headset
            .ask(Request::SetEncoding {
                bitrate: Some(8),
                codec: Some(codec),
                software: Some(software),
                sharp: Some(false),
            })
            .unwrap();
        let now = headset.view().encoding().unwrap().using.unwrap();
        if restart {
            headset
                .wait(SOON, "every stream started over with the new codec", |v| {
                    before.iter().all(|(id, n)| {
                        v.streams
                            .get(id)
                            .is_some_and(|s| s.starts > *n && s.codec == now.0 && s.has_picture())
                    })
                })
                .unwrap();
        } else {
            headset.take_for(Duration::from_secs(2)).unwrap();
            assert_eq!(starts(headset), before, "no stream started over");
        }
        now
    };

    let on_gpu = [Codec::H265, Codec::H264]
        .into_iter()
        .find(|c| can.contains(&(*c, true)));
    eprintln!("the PC can encode with {can:?}: on the graphics card {on_gpu:?}");
    let got = ask(&mut headset, Codec::H265, false, on_gpu.is_some());
    match on_gpu {
        Some(codec) => {
            assert_eq!(got, (codec, true));
            let cpu = ask(&mut headset, Codec::H264, true, true);
            assert_eq!(cpu, (Codec::H264, false));
        }
        None => {
            assert_eq!(got, (Codec::H264, false));
            let same = ask(&mut headset, Codec::H264, true, false);
            assert_eq!(same, (Codec::H264, false));
        }
    }
}
