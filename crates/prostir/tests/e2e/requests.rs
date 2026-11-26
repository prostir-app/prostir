//! Requests and their replies: each gets exactly one, with its number; what failed
//! says why with a code; what made displays answers with all of them, also those that
//! did not start; only the main headset's choices touch what the PC allows.

use prostir_proto::ClientMsg;
use prostir_proto::request::{Asked, Outcome, Primary, Request};
use prostir_proto::state::{DisplayId, DisplayState, ErrorCode, Mode, Placed, Prefs, ReqId, Row};
use prostir_testkit::{FakeHeadset, View};

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START, TWO_MONITORS, connect, session};

/// The code a failed reply carries.
fn failed_with(outcome: &Outcome) -> Option<ErrorCode> {
    match outcome {
        Outcome::Failed { error, .. } => Some(error.known()),
        _ => None,
    }
}

/// A headset whose H.264 decoder takes 4096 a side but keeps up at 60 frames a second
/// only with 2560x1600 worth of pixels (its `DecoderPixels` in `Open`): the PC offers
/// it no 4K, refuses a 4K monitor asked for (`BAD_SIZE`), and makes a 1440p one.
#[test]
fn the_pixel_budget_bounds_the_sizes() {
    let dir = TestDir::new("pixel-budget");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &Options::default());
    let mut state = FakeHeadset::state();
    state.caps.as_mut().unwrap().pixels = vec![prostir_proto::state::PixelBudget {
        codec: 0,
        max_pixels: 2560 * 1600,
    }];
    let mut headset = FakeHeadset::connect_saying(pc.addr, &mut keys, &state)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let limits = headset.view().pc.limits.clone().unwrap();
    assert!(!limits.sizes.contains(&Mode::new(3840, 2160)), "{limits:?}");
    assert!(limits.sizes.contains(&Mode::new(2560, 1440)), "{limits:?}");
    let big = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(3840, 2160),
        })
        .unwrap();
    assert_eq!(failed_with(&big), Some(ErrorCode::BAD_SIZE), "{big:?}");
    let fits = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(2560, 1440),
        })
        .unwrap();
    assert!(matches!(fits, Outcome::Added { .. }), "{fits:?}");
}

/// Over the limit, a size the PC does not make, the last monitor, a display that is
/// not there, a request nobody knows: each answered under its own number with its
/// code, and nothing changed.
#[test]
fn failures_say_why() {
    let (_dir, _pc, _keys, mut headset) = session("failures", &Options::default());
    let limit = usize::from(headset.view().pc.limits.as_ref().unwrap().virtual_max);
    let small = Mode::new(320, 240);
    for _ in 2..limit {
        let added = headset.ask(Request::AddDisplay { mode: small }).unwrap();
        assert!(matches!(added, Outcome::Added { .. }), "{added:?}");
    }
    let req = headset
        .request(Request::AddDisplay { mode: small })
        .unwrap();
    let over = headset.reply(req, SOON).unwrap();
    assert_eq!(failed_with(&over), Some(ErrorCode::LIMIT), "{over:?}");
    assert_eq!(headset.view().order().len(), limit);
    let odd = headset
        .ask(Request::AddDisplay {
            mode: Mode {
                scale: 133,
                ..Mode::new(1920, 1080)
            },
        })
        .unwrap();
    assert_eq!(failed_with(&odd), Some(ErrorCode::BAD_SIZE));
    let tiny = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(10, 10),
        })
        .unwrap();
    assert_eq!(failed_with(&tiny), Some(ErrorCode::BAD_SIZE));
    let nobody = headset
        .ask(Request::RemoveDisplay {
            id: DisplayId(4_000_000),
        })
        .unwrap();
    assert_eq!(failed_with(&nobody), Some(ErrorCode::UNKNOWN_DISPLAY));
    let order = headset.view().order();
    for id in &order[1..] {
        assert_eq!(
            headset.ask(Request::RemoveDisplay { id: *id }).unwrap(),
            Outcome::Done
        );
    }
    let last = headset
        .ask(Request::RemoveDisplay { id: order[0] })
        .unwrap();
    assert_eq!(failed_with(&last), Some(ErrorCode::LAST_DISPLAY));
    // Unknown kind: its number and UNSUPPORTED.
    let unknown = ReqId(9_999);
    headset
        .send(&ClientMsg::Request {
            req: unknown,
            asked: Asked::Unknown { kind: 77 },
        })
        .unwrap();
    let outcome = headset.reply(unknown, SOON).unwrap();
    assert_eq!(failed_with(&outcome), Some(ErrorCode::UNSUPPORTED));
    // Nothing but the replies asked for.
    assert!(
        headset
            .view()
            .replies
            .keys()
            .all(|r| r.0 <= 64 || *r == unknown)
    );
    assert!(
        headset.view().broken.is_empty(),
        "{:?}",
        headset.view().broken
    );
}

/// A monitor that cannot be made is still added: its number comes back at once, with
/// the monitor starting in the list, and once the desktop's worker tried it stands
/// there as failed, with why.
#[test]
fn a_monitor_that_cannot_start_is_added_failed() {
    let options = Options {
        // The third output the fake desktop makes.
        broken_monitors: "3".into(),
        ..Options::default()
    };
    let (_dir, _pc, _keys, mut headset) = session("added-failed", &options);
    let outcome = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(640, 360),
        })
        .unwrap();
    let Outcome::Added { id } = outcome else {
        panic!("added, not {outcome:?}")
    };
    let d = headset.view().display(id).cloned().unwrap();
    assert_eq!(d.state, DisplayState::Starting, "listed before it is made");
    headset
        .wait(SOON, "the monitor failed", |v| {
            v.display(id)
                .is_some_and(|d| matches!(d.state, DisplayState::Failed { .. }))
        })
        .unwrap();
    let d = headset.view().display(id).cloned().unwrap();
    let DisplayState::Failed { error, detail } = d.state else {
        panic!("failed, not {:?}", d.state)
    };
    assert_eq!(error.known(), ErrorCode::DESKTOP);
    assert!(detail.contains("broken"), "{detail}");
    assert_eq!(headset.view().order().len(), 3, "it keeps its place");
}

/// A profile's monitors come back numbered in the order of the sizes, with the PC's own
/// displays shown as asked and one of them the primary display; the headset's
/// arrangement moves them on the desktop.
#[test]
fn a_profile_gets_its_numbers_in_order() {
    let options = Options {
        physical: "eDP-1=BOE 0x0a1c 0@1920x1080".into(),
        ..Options::default()
    };
    let (_dir, _pc, _keys, mut headset) = session("profile", &options);
    headset
        .wait(SOON, "the laptop's panel listed", |v| {
            v.displays()
                .iter()
                .any(prostir_proto::state::Display::is_physical)
        })
        .unwrap();
    let laptop = headset
        .view()
        .displays()
        .iter()
        .find(|d| d.is_physical())
        .unwrap()
        .id;
    let sizes = [
        Mode::new(800, 450),
        Mode::new(640, 360),
        Mode::new(480, 272),
    ];
    let outcome = headset
        .ask(Request::SetDisplays {
            modes: sizes.to_vec(),
            primary: Some(Primary::Display { id: laptop }),
            shown_physical: vec![laptop],
        })
        .unwrap();
    let Outcome::Ids { ids } = outcome else {
        panic!("numbers, not {outcome:?}")
    };
    assert_eq!(ids.len(), 3);
    let view = headset.view();
    for (id, size) in ids.iter().zip(sizes) {
        let d = view.display(*id).unwrap();
        assert_eq!((d.mode.w, d.mode.h), (size.w, size.h));
    }
    assert_eq!(view.order(), ids, "in the order of the sizes");
    assert_eq!(view.primary(), Some(laptop));
    assert!(view.display(laptop).unwrap().is_shown_physical());
    headset
        .wait(SOON, "every monitor and the laptop's panel showing", |v| {
            v.all_monitors_show()
                && v.streams
                    .get(&laptop)
                    .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();

    // The arrangement: the laptop's panel over the first monitor.
    let at = |id, middle| Placed {
        id,
        center: Some(0),
        middle: Some(middle),
    };
    headset
        .send(&ClientMsg::Arrange {
            rows: vec![
                Row {
                    entries: vec![at(laptop, -600)],
                },
                Row {
                    entries: ids.iter().map(|id| at(*id, 0)).collect(),
                },
            ],
        })
        .unwrap();
    headset
        .wait(
            SOON,
            "the laptop's panel over the monitors on the desktop",
            |v| {
                let (Some(top), Some(first)) = (
                    v.display(laptop).and_then(|d| d.desk),
                    v.display(ids[0]).and_then(|d| d.desk),
                ) else {
                    return false;
                };
                top.y + top.h as i32 <= first.y
            },
        )
        .unwrap();
}

/// "No limits" of the main headset raises what the PC allows, and the PC makes that
/// many; a guest's (a headset with a line of its own in `settings.txt`) does not, and
/// a guest may not change the monitors.
#[test]
fn only_the_main_headset_changes_the_limits() {
    let (_dir, _pc, _keys, mut headset) = session("no-limits", &Options::default());
    let before = headset.view().pc.limits.clone().unwrap();
    let free = Prefs {
        no_limits: true,
        ..Prefs::default()
    };
    headset.send(&ClientMsg::Prefs { part: free }).unwrap();
    headset
        .wait(SOON, "more allowed", |v| {
            v.pc.limits.as_ref().unwrap().virtual_max > before.virtual_max
        })
        .unwrap();
    let max = headset.view().pc.limits.as_ref().unwrap().virtual_max;
    let made = headset
        .ask(Request::SetDisplays {
            modes: vec![Mode::new(640, 360); usize::from(max)],
            primary: None,
            shown_physical: Vec::new(),
        })
        .unwrap();
    let Outcome::Ids { ids } = made else {
        panic!("{made:?}")
    };
    assert_eq!(ids.len(), usize::from(max));

    let dir = TestDir::new("limits-guest");
    dir.save_screens("next 200\nrow\nscreen 0 640x360\n");
    let mut keys = dir.pair();
    let key = prostir_proto::secure::to_hex(&keys.identity.public);
    dir.save_settings(&format!("access {key} guest input\n"));
    let pc = Pc::start(&dir, &Options::default());
    let mut guest = connect(&pc, &mut keys);
    guest
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert!(!guest.view().pc.access.unwrap().is_main());
    guest.send(&ClientMsg::Prefs { part: free }).unwrap();
    guest.take_for(std::time::Duration::from_secs(1)).unwrap();
    assert_eq!(
        guest.view().pc.limits.as_ref().unwrap().virtual_max,
        before.virtual_max
    );
    let refused = guest
        .ask(Request::AddDisplay {
            mode: Mode::new(640, 360),
        })
        .unwrap();
    assert_eq!(failed_with(&refused), Some(ErrorCode::NOT_ALLOWED));
}

/// A headset that decodes three streams at once gets no fourth display of any kind:
/// a monitor or a window panel beyond that is refused with `LIMIT`, however many
/// monitors the PC would make.
#[test]
fn no_more_streams_than_the_headset_decodes() {
    let dir = TestDir::new("streams-max");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &Options::default());
    let mut state = FakeHeadset::state();
    state.caps.as_mut().unwrap().streams_max = 3;
    let mut headset = FakeHeadset::connect_saying(pc.addr, &mut keys, &state)
        .unwrap_or_else(|e| panic!("connecting: {e}"));
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let limits = headset.view().pc.limits.clone().unwrap();
    assert_eq!(limits.streams_max, 3);
    assert!(limits.virtual_max > 3, "the monitors' own limit is higher");
    let small = Mode::new(320, 240);
    let third = headset.ask(Request::AddDisplay { mode: small }).unwrap();
    assert!(matches!(third, Outcome::Added { .. }), "{third:?}");
    let fourth = headset.ask(Request::AddDisplay { mode: small }).unwrap();
    assert_eq!(failed_with(&fourth), Some(ErrorCode::LIMIT), "{fourth:?}");
    let panel = headset.ask(Request::DetachWindow).unwrap();
    assert_eq!(failed_with(&panel), Some(ErrorCode::LIMIT), "{panel:?}");
    let four = headset
        .ask(Request::SetDisplays {
            modes: vec![small; 4],
            primary: None,
            shown_physical: Vec::new(),
        })
        .unwrap();
    assert_eq!(failed_with(&four), Some(ErrorCode::LIMIT), "{four:?}");
    assert_eq!(headset.view().order().len(), 3);
}

/// A guest's volume is its own: the PC neither shares its sound device's volume with
/// a guest nor asks one to take the slider's level.
#[test]
fn a_guest_is_not_asked_for_the_volume() {
    use prostir_proto::state::caps;
    let dir = TestDir::new("guest-volume");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let key = prostir_proto::secure::to_hex(&keys.identity.public);
    dir.save_settings(&format!("access {key} guest input\n"));
    let pc = Pc::start(&dir, &Options::default());
    let mut guest = connect(&pc, &mut keys);
    guest
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let pc_caps = guest.view().pc.caps.clone().unwrap();
    assert!(!pc_caps.has(caps::pc::VOLUME), "{pc_caps:?}");
    pc.desktop("volume 40");
    guest.take_for(std::time::Duration::from_secs(1)).unwrap();
    assert!(guest.view().volume_asks.is_empty());
}

/// Another scale for a monitor: the same number, given in place (the fake desktop
/// does, as KWin): still running with the reply, its stream not started over, laid
/// out at its new size; a scale the PC does not offer is `BAD_SIZE` and changes
/// nothing; a display that is not there is `UNKNOWN_DISPLAY`.
#[test]
fn a_monitor_gets_another_scale() {
    let (_dir, _pc, _keys, mut headset) = session("set-scale", &Options::default());
    let id = headset.view().order()[0];
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(pc_caps.has(prostir_proto::state::caps::pc::SCALE_IN_PLACE));
    let scales = headset.view().pc.limits.as_ref().unwrap().scales.clone();
    let larger = *scales.iter().find(|s| **s != 100).expect("a larger text");
    let starts = headset.view().streams[&id].starts;
    let width = headset.view().display(id).unwrap().mode.w;
    let outcome = headset
        .ask(Request::SetScale { id, scale: larger })
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let d = headset.view().display(id).cloned().unwrap();
    assert_eq!(
        (d.mode.scale, d.state),
        (larger, DisplayState::Running),
        "running with the reply"
    );
    let logical = (f64::from(width) * 100.0 / f64::from(larger)).round() as u32;
    headset
        .wait(SOON, "laid out at the new size", |v| {
            v.display(id)
                .and_then(|d| d.desk)
                .is_some_and(|r| r.w.abs_diff(logical) <= 1)
        })
        .unwrap();
    assert_eq!(
        headset.view().streams[&id].starts,
        starts,
        "its stream did not start over"
    );
    assert_eq!(headset.view().order()[0], id, "the same number");
    let odd = headset.ask(Request::SetScale { id, scale: 133 }).unwrap();
    assert_eq!(failed_with(&odd), Some(ErrorCode::BAD_SIZE), "{odd:?}");
    assert_eq!(headset.view().display(id).unwrap().mode.scale, larger);
    let nobody = headset
        .ask(Request::SetScale {
            id: DisplayId(4_000_000),
            scale: 100,
        })
        .unwrap();
    assert_eq!(failed_with(&nobody), Some(ErrorCode::UNKNOWN_DISPLAY));
}

/// Every display of the PC seen for the first time shows in the headset (the PC's
/// displays wanted there), not only the one the desktop starts with, each streaming,
/// as many as the headset may show without "No limits": of five, four.
#[test]
fn every_display_of_the_pc_shows_at_first() {
    let options = Options {
        physical: "HDMI-A-1=DEL 0x1 0@640x360;DP-1=LG 0x2 0@640x360;DP-2=LG 0x3 0@640x360;\
                   DP-3=LG 0x4 0@640x360;DP-4=LG 0x5 0@640x360"
            .into(),
        ..Options::default()
    };
    let dir = TestDir::new("physical-all");
    dir.save_settings("mirror on\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(SOON, "four displays of the PC shown and streaming", |v| {
            let shown: Vec<_> = v
                .displays()
                .iter()
                .filter(|d| d.is_shown_physical())
                .map(|d| d.id)
                .collect();
            let all = v.displays().iter().filter(|d| d.is_physical()).count();
            all == 5
                && shown.len() == 4
                && shown.iter().all(|id| {
                    v.streams
                        .get(id)
                        .is_some_and(prostir_testkit::Stream::has_picture)
                })
        })
        .unwrap();
}

/// A display of the PC shown from the headset is starting in the list that comes
/// with the reply, and streams after; hidden, it stops at once.
#[test]
fn a_display_of_the_pc_shown_starts_at_once() {
    let options = Options {
        physical: "HDMI-A-1=DEL 0x1 0@640x360".into(),
        ..Options::default()
    };
    let (_dir, _pc, _keys, mut headset) = session("show-physical", &options);
    headset
        .wait(SOON, "the display of the PC listed", |v| {
            v.displays()
                .iter()
                .any(prostir_proto::state::Display::is_physical)
        })
        .unwrap();
    let id = headset
        .view()
        .displays()
        .iter()
        .find(|d| d.is_physical())
        .unwrap()
        .id;
    let shown = headset
        .ask(Request::ShowPhysical { id, shown: true })
        .unwrap();
    assert_eq!(shown, Outcome::Done);
    assert_eq!(
        headset.view().display(id).map(|d| d.state.clone()),
        Some(DisplayState::Starting),
        "starting with the reply"
    );
    headset
        .wait(SOON, "the display of the PC streams", |v| {
            v.streams
                .get(&id)
                .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    let hidden = headset
        .ask(Request::ShowPhysical { id, shown: false })
        .unwrap();
    assert_eq!(hidden, Outcome::Done);
    assert_eq!(
        headset.view().display(id).map(|d| d.state.clone()),
        Some(DisplayState::Running),
        "not shown: as connected"
    );
    headset
        .wait(SOON, "its stream ended", |v| v.ended.contains(&id))
        .unwrap();
}

/// A picture of one monitor where it comes from its own stream (as on KDE), of a
/// still screen: a keyframe asked of it would come with the source's next frame,
/// which on a still screen is its keepalive (here never), so the stream starts over
/// and sends its picture at once; it is that monitor's picture, not the whole
/// desktop's.
#[test]
fn a_picture_of_a_still_monitor_comes_from_its_stream() {
    let options = Options {
        env: vec![("PROSTIR_FAKE_SCREENSHOT".into(), "stream".into())],
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("screenshot-still", &options);
    let id = headset.view().order()[0];
    // Still since it started.
    headset.take_for(std::time::Duration::from_secs(1)).unwrap();
    let outcome = headset.ask(Request::Screenshot { id }).unwrap();
    let from_stream = pc.count_log("screenshot from the stream:");
    assert_eq!(
        pc.count_log("waiting on channel"),
        0,
        "no keyframe came: {:?}",
        pc.lines("screenshot")
    );
    match outcome {
        Outcome::Saved { path } => {
            assert!(std::path::Path::new(&path).is_file(), "{path}");
            assert_eq!(from_stream, 0, "{:?}", pc.lines("screenshot"));
        }
        // Without a decoder for H.264 here the keyframe came, but made no picture.
        other => {
            eprintln!("no picture made of the keyframe: {other:?}");
            pc.wait_log(SOON, "screenshot from the stream:");
        }
    }
}

/// Auto (feature 17): monitors kept by an earlier version are not in it; turned on,
/// the main headset's choice is taken in place (a scale the desktop does not give that
/// size becomes the nearest it gives); one chosen by hand takes the monitor out of it,
/// after which the headset's choice changes nothing; a display of the PC cannot be in
/// it.
#[test]
fn auto_takes_the_headsets_scale_until_one_is_chosen() {
    let (_dir, _pc, _keys, mut headset) = session("auto-scale", &Options::default());
    let id = headset.view().order()[0];
    let in_auto = |v: &View| v.pc.scale_auto.as_ref().is_some_and(|a| a.has(id));
    assert!(headset.view().pc.scale_auto.is_some(), "the part is there");
    assert!(!in_auto(headset.view()), "kept from before: not in Auto");
    let not_auto = headset.ask(Request::AutoScale { id, scale: 125 }).unwrap();
    assert_eq!(not_auto, Outcome::Done);
    assert_eq!(headset.view().display(id).unwrap().mode.scale, 100);
    let on = headset.ask(Request::SetScaleAuto { id, on: true }).unwrap();
    assert_eq!(on, Outcome::Done);
    assert!(in_auto(headset.view()));
    let chosen = headset.ask(Request::AutoScale { id, scale: 150 }).unwrap();
    assert_eq!(chosen, Outcome::Done);
    let d = headset.view().display(id).cloned().unwrap();
    assert_eq!((d.mode.scale, d.state), (150, DisplayState::Running));
    assert!(in_auto(headset.view()), "still in Auto");
    let by_hand = headset.ask(Request::SetScale { id, scale: 125 }).unwrap();
    assert_eq!(by_hand, Outcome::Done);
    assert!(!in_auto(headset.view()), "chosen by hand");
    headset.ask(Request::AutoScale { id, scale: 200 }).unwrap();
    assert_eq!(headset.view().display(id).unwrap().mode.scale, 125);
    let nobody = headset
        .ask(Request::SetScaleAuto {
            id: DisplayId(4_000_000),
            on: true,
        })
        .unwrap();
    assert_eq!(failed_with(&nobody), Some(ErrorCode::UNKNOWN_DISPLAY));
    // A monitor added is in Auto.
    let added = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(1920, 1080),
        })
        .unwrap();
    let Outcome::Added { id: new } = added else {
        panic!("{added:?}");
    };
    assert!(
        headset
            .view()
            .pc
            .scale_auto
            .as_ref()
            .is_some_and(|a| a.has(new))
    );
}
