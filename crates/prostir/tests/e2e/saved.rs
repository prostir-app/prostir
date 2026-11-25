//! What the PC keeps between runs: the monitors with their numbers, even one that could
//! not start.

use std::time::Duration;

use prostir_proto::ClientMsg;
use prostir_proto::request::{Outcome, Request};
use prostir_proto::state::{DisplayId, Mode, Placed, Row};
use prostir_testkit::View;

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START, TWO_MONITORS, connect, session};

/// Where the fake desktop put each output the last time (`name -> (x, y)`).
pub fn placed(pc: &Pc) -> Vec<(String, i32, i32)> {
    let lines = pc.lines("fake desktop: placed ");
    let Some(line) = lines.last() else {
        return Vec::new();
    };
    let list = line
        .split("fake desktop: placed ")
        .nth(1)
        .unwrap_or_default();
    let mut out: Vec<(String, i32, i32)> = list
        .split(", ")
        .filter_map(|item| {
            let (name, at) = item.split_once(' ')?;
            let (x, y) = at.split_once(',')?;
            Some((name.to_owned(), x.parse().ok()?, y.parse().ok()?))
        })
        .collect();
    out.sort();
    out
}

/// Two rows from the headset: the second monitor over the first.
fn two_rows(first: DisplayId, second: DisplayId) -> ClientMsg {
    let at = |id, middle| Placed {
        id,
        center: Some(0),
        middle: Some(middle),
    };
    ClientMsg::Arrange {
        rows: vec![
            Row {
                entries: vec![at(second, -400)],
            },
            Row {
                entries: vec![at(first, 0)],
            },
        ],
    }
}

fn ids<const N: usize>(n: [u32; N]) -> [DisplayId; N] {
    n.map(DisplayId)
}

/// Screens saved in `screens.txt`, by number.
fn saved_numbers(dir: &TestDir) -> Vec<u32> {
    dir.saved("screens.txt")
        .lines()
        .filter_map(|l| l.strip_prefix("screen ")?.split(' ').next()?.parse().ok())
        .collect()
}

/// A monitor that cannot start keeps its place: in the order the headset gets, and in
/// the saved set. The next run without the fault shows it again.
#[test]
fn a_monitor_that_cannot_start_stays_in_the_set() {
    let dir = TestDir::new("cannot-start");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let options = Options {
        broken_monitors: "2".into(),
        ..Options::default()
    };
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "one monitor showing, the other failed", |v| {
            v.streams.values().any(prostir_testkit::Stream::has_picture)
                && v.displays().iter().any(|d| {
                    matches!(&d.state, prostir_proto::state::DisplayState::Failed { detail, .. } if detail.contains("broken"))
                })
        })
        .unwrap();
    let view = headset.view();
    assert_eq!(view.order(), ids([0, 1]), "both stay in the list");
    assert!(!view.streams.contains_key(&DisplayId(1)));
    headset.disconnect();
    pc.quit();
    assert_eq!(saved_numbers(&dir), [0, 1], "both stay in the set");

    let pc = Pc::start(&dir, &Options::default());
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(headset.view().order(), ids([0, 1]));
}

/// Monitors keep their numbers between runs, so the headset finds each where it was;
/// a new one gets a number never given before (from 101: above what earlier versions
/// gave out), also after a restart, while its output takes the lowest free name.
#[test]
fn monitors_keep_their_numbers_after_a_restart() {
    let (dir, pc, mut keys, mut headset) = session("numbers", &Options::default());
    let added = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(800, 450),
        })
        .unwrap();
    assert_eq!(added, Outcome::Added { id: DisplayId(101) });
    let named = |headset: &prostir_testkit::FakeHeadset, id| {
        headset.view().display(DisplayId(id)).unwrap().name.clone()
    };
    assert_eq!(named(&headset, 101), "Prostir-3");
    let removed = headset
        .ask(Request::RemoveDisplay { id: DisplayId(0) })
        .unwrap();
    assert_eq!(removed, Outcome::Done);
    assert_eq!(headset.view().order(), ids([1, 101]));
    // Written off the main loop within a pass.
    headset.take_for(Duration::from_millis(500)).unwrap();
    headset.disconnect();
    pc.quit();
    assert_eq!(saved_numbers(&dir), [1, 101]);

    let pc = Pc::start(&dir, &Options::default());
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(headset.view().order(), ids([1, 101]), "the same numbers");
    assert_eq!(headset.view().streams[&DisplayId(101)].width, 800);
    // Never a number given before the restart: the next block.
    let again = headset
        .ask(Request::AddDisplay {
            mode: Mode::new(640, 360),
        })
        .unwrap();
    let Outcome::Added { id } = again else {
        panic!("a monitor, not {again:?}")
    };
    assert_eq!(
        id,
        DisplayId(117),
        "after the block reserved before the restart"
    );
    // The lowest free name (monitor 1 keeps Prostir-2 as before the counter, 101 took
    // the lowest free one at the start).
    assert_eq!(named(&headset, 117), "Prostir-3");
}

/// Two rows from the headset reach the desktop (one monitor over the other), and a
/// restart of the PC app lays the desktop out the same way before the headset says
/// anything.
#[test]
fn two_rows_survive_a_restart() {
    let (dir, pc, mut keys, mut headset) = session("two-rows", &Options::default());
    let order = headset.view().order();
    headset.send(&two_rows(order[0], order[1])).unwrap();
    // Fake-1 640x360 under Fake-2 480x272, touching.
    let stacked = |p: &[(String, i32, i32)]| {
        let at = |n: &str| p.iter().find(|o| o.0 == n).map(|o| (o.1, o.2));
        matches!((at("Fake-1"), at("Fake-2")), (Some((_, y1)), Some((_, y2))) if y2 + 272 == y1)
    };
    let until = std::time::Instant::now() + SOON;
    while !stacked(&placed(&pc)) {
        assert!(std::time::Instant::now() < until, "{:?}", placed(&pc));
        headset.take_for(Duration::from_millis(100)).unwrap();
    }
    let before = placed(&pc);
    headset.take_for(Duration::from_millis(500)).unwrap();
    headset.disconnect();
    pc.quit();
    assert!(
        dir.saved("screens.txt").matches("row").count() == 2,
        "{}",
        dir.saved("screens.txt")
    );

    let mut pc = Pc::start(&dir, &Options::default());
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log(SOON, "fake desktop: placed");
    assert_eq!(placed(&pc), before, "laid out as before");
}

/// A choice made in the tray while no headset is connected is kept: the PC app quits
/// and starts with it.
#[test]
fn a_tray_change_without_monitors_survives_quitting() {
    let dir = TestDir::new("tray-offline");
    dir.save_monitors(TWO_MONITORS);
    let mut pc = Pc::start(&dir, &Options::default());
    // On by default; switched back.
    pc.tray("sharp off");
    pc.wait_log(SOON, "clearer text: false");
    pc.quit();
    assert!(
        dir.saved("settings.txt").contains("sharp off\n"),
        "{}",
        dir.saved("settings.txt")
    );
    // The options of that run were not saved.
    assert!(dir.saved("settings.txt").contains("software off\n"));
    assert!(!dir.saved("settings.txt").contains("bitrate"), "not chosen");
}

/// Killed while the headset changes the layout over and over, the PC app leaves files
/// it reads on the next start, with every monitor.
#[test]
fn killed_mid_change_leaves_readable_files() {
    let (dir, pc, mut keys, mut headset) = session("killed", &Options::default());
    let order = headset.view().order();
    for i in 0..60 {
        let msg = if i % 2 == 0 {
            two_rows(order[0], order[1])
        } else {
            two_rows(order[1], order[0])
        };
        headset.send(&msg).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    pc.kill();
    let screens = dir.saved("screens.txt");
    // In the order of the rows the PC took last, whichever that was.
    let mut numbers = saved_numbers(&dir);
    numbers.sort_unstable();
    assert_eq!(numbers, [0, 1], "{screens}");

    let mut pc = Pc::start(&dir, &Options::default());
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    assert_eq!(headset.view().order().len(), 2);
    assert_eq!(pc.count_log("screens.txt.bad"), 0);
    pc.wait_log(SOON, "fake desktop: placed");
}

/// Every display of the PC shows at first; one unplugged (the lid closed) stays in
/// the list as absent, with its number, and comes back with it when plugged in again;
/// it has its place on the desktop, shown or not.
#[test]
fn a_display_of_the_pc_comes_back_with_its_number() {
    let options = Options {
        physical: "eDP-1=BOE 0x0a1c 0@1920x1080;HDMI-A-1=@1280x720".into(),
        ..Options::default()
    };
    let dir = TestDir::new("physical");
    // The laptop screen wanted, as 0.1 said it.
    dir.save_monitors("row\n640x360\nmirror on\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    use prostir_proto::state::{DisplayKind, DisplayState};
    let physical = |v: &View| -> Vec<prostir_proto::state::Display> {
        v.displays()
            .iter()
            .filter(|d| d.is_physical())
            .cloned()
            .collect()
    };
    headset
        .wait(SOON, "the PC's displays, both shown", |v| {
            let p = physical(v);
            p.len() == 2
                && p.iter().all(|d| d.desk.is_some())
                && p.iter().all(|d| {
                    d.is_shown_physical()
                        && v.streams
                            .get(&d.id)
                            .is_some_and(prostir_testkit::Stream::has_picture)
                })
        })
        .unwrap();
    let all = physical(headset.view());
    let hdmi = all
        .iter()
        .find(|d| matches!(&d.kind, DisplayKind::Physical { connector, .. } if connector == "HDMI-A-1"))
        .unwrap()
        .id;
    let laptop = all.iter().find(|d| d.id != hdmi).unwrap().id;
    assert!(
        matches!(
            &headset.view().display(laptop).unwrap().kind,
            DisplayKind::Physical {
                builtin: true,
                shown: true,
                ..
            }
        ),
        "the laptop's panel shown at first too"
    );
    assert!(hdmi.0 >= 101 && laptop.0 >= 101);

    pc.desktop("unplug HDMI-A-1");
    headset
        .wait(SOON, "the display absent, its stream ended", |v| {
            v.display(hdmi)
                .is_some_and(|d| d.state == DisplayState::Absent && d.desk.is_none())
                && v.ended.contains(&hdmi)
        })
        .unwrap();
    assert!(headset.view().removed.is_empty(), "still in the list");
    pc.desktop("plug HDMI-A-1");
    headset
        .wait(SOON, "the same display back, streaming", |v| {
            v.display(hdmi)
                .is_some_and(|d| d.state == DisplayState::Running)
                && v.streams
                    .get(&hdmi)
                    .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
    // Kept with its numbers, and when each was seen, for the next run.
    headset.take_for(Duration::from_millis(500)).unwrap();
    headset.disconnect();
    pc.quit();
    let screens = dir.saved("screens.txt");
    assert!(
        screens.contains(&format!("physical {} connector HDMI-A-1 shown", hdmi.0)),
        "{screens}"
    );
    assert!(
        screens.contains(&format!(
            "physical {} edid \"BOE 0x0a1c 0\" connector eDP-1 builtin shown seen ",
            laptop.0
        )),
        "{screens}"
    );
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let again: Vec<DisplayId> = physical(headset.view()).iter().map(|d| d.id).collect();
    assert!(
        again.contains(&hdmi) && again.contains(&laptop),
        "{again:?}"
    );
}

/// Starting with the session is on by default: the first start turns it on and keeps
/// that it did; later starts do nothing to it, and once turned off it stays off.
#[test]
fn autostart_is_on_by_default_and_off_stays_off() {
    let dir = TestDir::new("autostart");
    let pc = Pc::start(&dir, &Options::default());
    assert_eq!(pc.count_log("fake autostart: the first start"), 1);
    pc.quit();
    assert!(dir.saved("settings.txt").contains("autostart on\n"));

    let mut pc = Pc::start(&dir, &Options::default());
    assert_eq!(pc.count_log("fake autostart"), 0, "kept, not set again");
    pc.tray("autostart off");
    pc.wait_log(SOON, "fake autostart: off");
    pc.quit();
    assert!(dir.saved("settings.txt").contains("autostart off\n"));

    let pc = Pc::start(&dir, &Options::default());
    assert_eq!(pc.count_log("fake autostart"), 0, "off stays off");
    pc.quit();
}

/// An update from 0.1 that did not start with the session (no entry of its own, no
/// socket): 0.1 kept no choice, so the first start of 0.2 keeps it off, for good.
#[test]
fn an_update_from_0_1_without_autostart_keeps_it_off() {
    let dir = TestDir::new("autostart-0-1");
    // What 0.1 left: its monitors.
    dir.save_monitors(TWO_MONITORS);
    let mut pc = Pc::start(&dir, &Options::default());
    pc.wait_log(SOON, "autostart stays off");
    assert_eq!(pc.count_log("fake autostart"), 0, "not turned on");
    pc.quit();
    assert!(dir.saved("settings.txt").contains("autostart off\n"));

    let pc = Pc::start(&dir, &Options::default());
    assert_eq!(pc.count_log("fake autostart"), 0, "kept");
    assert_eq!(pc.count_log("autostart stays off"), 0, "decided once");
    pc.quit();
}
