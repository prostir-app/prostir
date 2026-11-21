//! Scenarios of the room on the desktop, with the time passed in (step 7, 10.1): the
//! world is told what the PC and the streams say, the menu's actions and the hands, as
//! the app tells it, and what comes out is checked: the places, what is drawn, what a
//! ray hits, the rows for the PC and the files to write.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use glam::{Quat, Vec3};
use prostir_proto::state::{
    Display, DisplayId, DisplayKind, DisplayState, Displays, Mode, ReqId, Row,
};

use super::*;
use crate::ids::{DisplayKey, PcId, Thing};
use crate::layout::{Desk, Placement, Ray};
use crate::pending::{PROFILE_STANDS_WITHIN, ProfileSwitch};
use crate::persist::File;
use crate::settings::{Preset, Settings};

const PC: PcId = PcId([7; 8]);
const OTHER_PC: PcId = PcId([8; 8]);

/// Display `n` of the PC of these scenarios.
fn k(n: u32) -> DisplayKey {
    DisplayKey::new(PC, DisplayId(n))
}

fn monitor(n: u32, (w, h): (u32, u32)) -> Display {
    Display {
        id: DisplayId(n),
        kind: DisplayKind::Virtual,
        name: String::new(),
        mode: Mode::new(w, h),
        desk: None,
        state: DisplayState::Running,
    }
}

/// The laptop's own panel, shown.
fn laptop(n: u32) -> Display {
    Display {
        kind: DisplayKind::Physical {
            builtin: true,
            connector: "eDP-1".into(),
            shown: true,
        },
        ..monitor(n, (2880, 1800))
    }
}

fn starting(d: Display) -> Display {
    Display {
        state: DisplayState::Starting,
        ..d
    }
}

fn failed(d: Display) -> Display {
    Display {
        state: DisplayState::Failed {
            error: prostir_proto::state::ErrorCode::OTHER,
            detail: "no".into(),
        },
        ..d
    }
}

const QHD: (u32, u32) = (2560, 1440);

/// Three side by side: the scenarios' usual desk.
fn three() -> Vec<Display> {
    vec![monitor(1, QHD), monitor(2, QHD), monitor(3, QHD)]
}

/// The room, the app's settings, the time, the head, and what came out of the world:
/// the rows the PC was told.
struct Sim {
    world: World,
    s: Settings,
    now: Instant,
    head: (Vec3, Vec3),
    pc: PcId,
    arranged: Vec<Vec<Row>>,
}

impl Sim {
    /// Started with `layout.txt` holding `text` (none: no file).
    fn new(text: Option<&str>) -> Self {
        let desk = text.map_or_else(Desk::default, |t| Desk::parse(t).unwrap().0);
        let s = Settings::default();
        #[expect(
            clippy::disallowed_methods,
            reason = "a test's own starting time; the code under test takes the time as a parameter"
        )]
        let now = Instant::now();
        Self {
            world: World::new(desk, None, true, &s),
            s,
            now,
            head: (Vec3::ZERO, -Vec3::Z),
            pc: PC,
            arranged: Vec::new(),
        }
    }

    /// A session with the PC begins.
    fn connect(&mut self, pc: PcId) {
        self.pc = pc;
        self.world.connected(pc);
    }

    /// The PC lists `displays`; those gone.
    fn list(&mut self, displays: Vec<Display>) -> Vec<DisplayKey> {
        let displays = Displays {
            displays,
            primary: None,
        };
        self.world
            .listed(&displays, &self.s, self.head, self.now)
            .to_vec()
    }

    /// A stream of display `n` of the session's PC starts.
    fn stream(&mut self, n: u32, size: (u32, u32)) {
        let key = DisplayKey::new(self.pc, DisplayId(n));
        self.world
            .stream_started(key, size, &self.s, self.head, self.now);
    }

    /// `ms` pass in frames of about 11 ms, each as the app's frame goes: the button
    /// placed, the drawn places eased, the desktop settled, the drawing ordered.
    fn after(&mut self, ms: u64) {
        let frames = ms.div_ceil(11).max(1);
        let step = Duration::from_millis(ms) / frames as u32;
        for _ in 0..frames {
            self.now += step;
            self.frame(step.as_secs_f32());
        }
    }

    /// One frame of `dt` seconds, at the time it is now.
    fn frame(&mut self, dt: f32) {
        self.world.follow_head(&self.s, dt, self.head);
        self.world.place_button(&self.s);
        self.world.animate(&self.s, dt);
        self.world.settle(self.now);
        if let Some(rows) = self.world.take_arrange() {
            self.arranged.push(rows);
        }
        self.world.order_drawing(self.head.0, |_| false);
    }

    /// Long enough for everything drawn to arrive where it goes.
    fn settled(&mut self) {
        self.after(2000);
    }

    fn place(&self, key: DisplayKey) -> Placement {
        *self.world.placement(key).unwrap()
    }

    fn drawn(&self, key: DisplayKey) -> Placement {
        *self.world.shown(key).unwrap()
    }

    /// The places of every display, by key.
    fn places(&self) -> HashMap<DisplayKey, Placement> {
        self.world.placements.clone()
    }

    /// Connected, with the usual three placed and streaming, everything drawn and the
    /// PC told.
    fn at_desk() -> Self {
        let mut sim = Self::new(None);
        sim.connect(PC);
        sim.list(three());
        for n in 1..=3 {
            sim.stream(n, QHD);
        }
        sim.settled();
        sim
    }
}

/// A ray from the head towards `at`.
fn towards(at: Vec3) -> Ray {
    Ray {
        origin: Vec3::ZERO,
        dir: at.normalize(),
    }
}

fn near(a: &Placement, b: &Placement) -> bool {
    a.position.distance(b.position) < 1e-5
        && a.rotation.dot(b.rotation).abs() > 1.0 - 1e-6
        && (a.width - b.width).abs() < 1e-6
}

fn same_places(a: &HashMap<DisplayKey, Placement>, b: &HashMap<DisplayKey, Placement>) -> bool {
    a.len() == b.len() && a.iter().all(|(k, p)| b.get(k).is_some_and(|q| near(p, q)))
}

/// Scenario 1. Three monitors of a PC come to the places `layout.txt` has for them, and
/// the PC hears of their rows once they have stood for 0.8 s; never while a monitor is
/// in a hand.
#[test]
fn connecting_puts_monitors_where_the_file_says() {
    // A desk kept by hand: the right monitor raised a little.
    let mut was = Desk::default();
    let mut laid = layout::arrange(
        Preset::Arc,
        &[(k(1), 2560, 1440), (k(2), 2560, 1440), (k(3), 2560, 1440)],
        None,
        None,
        None,
    );
    laid.get_mut(&k(3)).unwrap().position.y += 0.05;
    let sizes = laid.keys().map(|k| (*k, QHD)).collect();
    was.keep(
        laid.clone(),
        &sizes,
        ((Vec3::ZERO, 0.0), layout::RADIUS, 0.0),
        false,
    );
    let mut text = String::new();
    was.write_text(&mut text);

    let mut sim = Sim::new(Some(&text));
    // Let go anywhere, a monitor stays there (the magnet has scenarios of its own).
    sim.s.magnet = false;
    sim.connect(PC);
    sim.list(three());
    for n in 1..=3 {
        sim.stream(n, QHD);
        // Nothing told while they arrive one by one.
        sim.after(100);
    }
    assert!(same_places(&sim.places(), &laid), "as the file says");
    assert!(
        sim.arranged.is_empty(),
        "not before they stood for a moment"
    );
    sim.after(500);
    assert!(sim.arranged.is_empty());
    sim.after(400);
    assert_eq!(sim.arranged.len(), 1, "told once they stood");
    let ids: Vec<u32> = sim.arranged[0]
        .iter()
        .flat_map(Row::ids)
        .map(|id| id.0)
        .collect();
    assert_eq!(ids, [1, 2, 3]);

    // In a hand, a monitor goes up and down for two seconds: nothing is told.
    sim.arranged.clear();
    let start = sim.world.hold(Thing::Display(k(2)), &sim.s).unwrap();
    for i in 0..200 {
        let mut p = sim.world.place(Thing::Display(k(2))).unwrap();
        p.position.y = start.position.y + 0.6 * ((i as f32) * 0.05).sin();
        sim.world.drag(&sim.s, Thing::Display(k(2)), p, sim.head.0);
        sim.after(11);
    }
    assert!(sim.arranged.is_empty(), "never while it is held");
    // Let go well above the others: told once it stood.
    let mut p = sim.world.place(Thing::Display(k(2))).unwrap();
    p.position.y = start.position.y + 0.9;
    sim.world.drag(&sim.s, Thing::Display(k(2)), p, sim.head.0);
    sim.world.let_go(Thing::Display(k(2)), &sim.s, sim.now);
    sim.after(700);
    assert!(sim.arranged.is_empty());
    sim.after(200);
    assert_eq!(sim.arranged.len(), 1);
    assert_eq!(sim.arranged[0].len(), 2, "it is a row of its own now");
}

/// Scenario 2. A monitor asked for shows at once as a placeholder where it will be,
/// and takes that place when its picture comes. One without a picture for 15 s goes,
/// and so does one the PC says failed.
#[test]
fn a_new_monitor_takes_its_placeholders_place() {
    let mut sim = Sim::at_desk();
    sim.world
        .adding_started(Some(ReqId(5)), QHD, sim.head.1, sim.now);
    let shown = *sim.world.adding().unwrap();
    assert_eq!(
        shown.thing(),
        Thing::Adding {
            pc: PC,
            req: ReqId(5)
        }
    );
    // The PC made display 4; its list says so before its stream starts.
    sim.world.adding_made(ReqId(5), Some(k(4)));
    let mut list = three();
    list.push(starting(monitor(4, QHD)));
    sim.list(list.clone());
    assert!(
        sim.world.placement(k(4)).is_none(),
        "placed only once it streams"
    );
    sim.stream(4, QHD);
    assert!(
        near(&sim.place(k(4)), &shown.place),
        "where its placeholder was"
    );
    sim.world.adding_due(sim.now, false);
    assert!(sim.world.adding().is_none(), "its picture is there");

    // Display 5 never streams: its placeholder waits 15 s, then goes.
    sim.world
        .adding_started(Some(ReqId(6)), QHD, sim.head.1, sim.now);
    sim.world.adding_made(ReqId(6), Some(k(5)));
    list.push(starting(monitor(5, QHD)));
    sim.list(list.clone());
    let asked = sim.now;
    sim.world
        .adding_due(asked + PLACEHOLDER_FOR - Duration::from_millis(1), false);
    assert!(sim.world.adding().is_some());
    sim.world.adding_due(asked + PLACEHOLDER_FOR, false);
    assert!(sim.world.adding().is_none(), "waited too long");

    // Display 6 could not start: the menu says why, and its placeholder goes.
    sim.world
        .adding_started(Some(ReqId(7)), QHD, sim.head.1, sim.now);
    list.push(failed(monitor(6, QHD)));
    sim.list(list);
    sim.world.adding_due(sim.now, true);
    assert!(sim.world.adding().is_some(), "waits for the PC's answer");
    sim.world.adding_made(ReqId(7), Some(k(6)));
    sim.world.adding_due(sim.now, false);
    assert!(sim.world.adding().is_none(), "failed");

    // Asked, and the request failed before the PC made it (nothing waits for it).
    sim.world
        .adding_started(Some(ReqId(8)), QHD, sim.head.1, sim.now);
    sim.world.adding_due(sim.now, true);
    assert!(sim.world.adding().is_some(), "waits for its answer");
    sim.world.adding_due(sim.now, false);
    assert!(sim.world.adding().is_none(), "the answer was a failure");
}

/// Cinema on, then off, through `World::set_cinema` as the app does it (the setting
/// changes after).
fn cinema(sim: &mut Sim, on: bool) {
    sim.world.set_cinema(&sim.s, on, sim.head.1, sim.now);
    sim.s.cinema = on;
}

/// Scenario 3. Cinema moved about by hand, through the system menu and resized with
/// both hands, and left: the desk arrangement is the one from before cinema, all in
/// one pass too, with nothing read from a file. A profile saved in cinema keeps that
/// desk.
#[test]
fn cinema_leaves_the_desk_as_it_was() {
    let mut sim = Sim::at_desk();
    // Arranged by hand before cinema.
    let mut p = sim.place(k(3));
    p.position += Vec3::new(0.1, 0.2, 0.0);
    sim.world.hold(Thing::Display(k(3)), &sim.s);
    sim.world.drag(&sim.s, Thing::Display(k(3)), p, sim.head.0);
    sim.world.let_go(Thing::Display(k(3)), &sim.s, sim.now);
    sim.settled();
    let desk = sim.places();
    let profile_at_desk = sim.world.profile(Vec::new(), |_| 100);

    cinema(&mut sim, true);
    sim.settled();
    assert_eq!(sim.world.placements.len(), 1, "one screen");
    let (&screen, _) = sim.world.placements.iter().next().unwrap();
    // Moved by hand, through the system menu, and resized with both hands.
    sim.world.hold(Thing::Display(screen), &sim.s);
    let mut p = sim.place(screen);
    p.position += Vec3::new(0.4, 0.3, 0.5);
    sim.world
        .drag(&sim.s, Thing::Display(screen), p, sim.head.0);
    sim.world.cancel_holds();
    sim.world.hold(Thing::Display(screen), &sim.s);
    sim.world.resize_start(screen);
    sim.world.resize(&sim.s, 1.0);
    sim.world.resize_end();
    sim.world.let_go(Thing::Display(screen), &sim.s, sim.now);
    sim.settled();
    // Saved as a profile in cinema: the desk.
    let profile_in_cinema = sim.world.profile(Vec::new(), |_| 100);
    assert_eq!(profile_in_cinema.places.len(), 3);
    for (a, b) in profile_in_cinema.places.iter().zip(&profile_at_desk.places) {
        assert!(near(a, b), "the desk, not the cinema view");
    }
    cinema(&mut sim, false);
    assert!(same_places(&sim.places(), &desk), "the desk as it was");

    // All in one pass: on, moved, off.
    cinema(&mut sim, true);
    let (&screen, _) = sim.world.placements.iter().next().unwrap();
    sim.world.hold(Thing::Display(screen), &sim.s);
    let mut p = sim.place(screen);
    p.position.x -= 0.7;
    sim.world
        .drag(&sim.s, Thing::Display(screen), p, sim.head.0);
    sim.world.let_go(Thing::Display(screen), &sim.s, sim.now);
    cinema(&mut sim, false);
    assert!(same_places(&sim.places(), &desk), "in one pass too");
    assert!(same_places(
        &sim.world.desk().arrangement().placements,
        &desk
    ));
}

/// Scenario 4. The system menu over the app in the middle of a drag: the monitor is let
/// go where it is, and the desk is written once.
#[test]
fn the_system_menu_lets_a_drag_go() {
    let mut sim = Sim::at_desk();
    sim.world.take_saves();
    sim.world.hold(Thing::Display(k(1)), &sim.s);
    for _ in 0..10 {
        let mut p = sim.place(k(1));
        p.position.y += 0.01;
        sim.world.drag(&sim.s, Thing::Display(k(1)), p, sim.head.0);
        sim.after(11);
    }
    assert!(
        sim.world.take_saves().is_empty(),
        "nothing kept while it moves"
    );
    let there = sim.place(k(1));
    sim.world.cancel_holds();
    assert!(!sim.world.holding() && !sim.world.holds(Thing::Display(k(1))));
    let mut saves = sim.world.take_saves();
    assert_eq!(
        saves.take().collect::<Vec<_>>(),
        [File::Desk],
        "the desk, once"
    );
    assert!(near(&sim.place(k(1)), &there), "let go where it was");
    sim.settled();
    assert!(near(&sim.place(k(1)), &there), "and it stays");
}

/// Scenario 5. The PC goes away and comes back: the places stay, and the same
/// numbers come back to them. In a session, a monitor gone from the list loses its
/// place; a display of the PC itself keeps it.
#[test]
fn a_disconnect_keeps_the_places() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    let mut list = three();
    list.push(laptop(9));
    sim.list(list.clone());
    for n in 1..=3 {
        sim.stream(n, QHD);
    }
    sim.stream(9, (2880, 1800));
    sim.settled();
    assert!(
        sim.world.placement(k(9)).is_some(),
        "the laptop screen is placed"
    );
    let before = sim.places();

    sim.world.disconnected();
    for n in [1, 2, 3, 9] {
        sim.world.stream_ended(k(n));
    }
    sim.settled();
    assert!(same_places(&sim.places(), &before), "kept while away");

    sim.connect(PC);
    sim.list(list);
    for n in 1..=3 {
        sim.stream(n, QHD);
    }
    sim.stream(9, (2880, 1800));
    sim.settled();
    assert!(same_places(&sim.places(), &before), "the same places again");

    // Monitor 2 removed, the laptop screen gone from the list too.
    let gone = sim.list(vec![monitor(1, QHD), monitor(3, QHD)]);
    assert_eq!(gone, [k(2)], "a display of the PC itself is not gone");
    assert!(sim.world.placement(k(2)).is_none());
    assert!(
        !sim.world
            .desk()
            .arrangement()
            .placements
            .contains_key(&k(2))
    );
    assert!(near(&sim.place(k(9)), &before[&k(9)]), "it keeps its place");
    assert!(near(&sim.place(k(1)), &before[&k(1)]));
}

/// Scenario 6. A profile switched to: the PC answers with the numbers of its monitors,
/// and each takes the profile's place by its order. A profile without places waits
/// for its monitors to stream, at most 8 s, and is laid out with what streams then.
#[test]
fn a_profile_places_its_monitors_by_their_numbers() {
    let mut sim = Sim::at_desk();
    // Two of them moved about, and the setup saved.
    let mut p = sim.place(k(1));
    p.position += Vec3::new(-0.2, 0.3, 0.1);
    sim.world.hold(Thing::Display(k(1)), &sim.s);
    sim.world.drag(&sim.s, Thing::Display(k(1)), p, sim.head.0);
    sim.world.let_go(Thing::Display(k(1)), &sim.s, sim.now);
    sim.settled();
    let saved = sim.world.profile(Vec::new(), |_| 100);
    let at_save = sim.places();
    // The profile's monitors go left to right, as the PC has them.
    let order: Vec<DisplayKey> = sim.world.monitors().iter().map(|m| m.0).collect();

    // Switched to: the PC makes three new monitors, 11, 12 and 13.
    sim.world.switch_to(Some(ProfileSwitch {
        frame: saved.frame,
        rows: saved.rows,
        ..ProfileSwitch::new(ReqId(3), saved.places)
    }));
    assert!(sim.world.switching_with(ReqId(3)));
    sim.world
        .profile_made(vec![k(11), k(12), k(13)], &sim.s, sim.head.1, sim.now);
    assert!(!sim.world.switching());
    for (new, old) in [11, 12, 13].into_iter().zip(&order) {
        assert!(
            near(&sim.place(k(new)), &at_save[old]),
            "{new} where {old} was"
        );
    }
    sim.list(vec![monitor(11, QHD), monitor(12, QHD), monitor(13, QHD)]);
    for n in 11..=13 {
        sim.stream(n, QHD);
    }
    sim.settled();
    for (new, old) in [11, 12, 13].into_iter().zip(&order) {
        assert!(near(&sim.place(k(new)), &at_save[old]), "and stays there");
    }

    // A profile saved before its monitors had places: laid out once they stream, or
    // after 8 s with what streams then.
    sim.world
        .switch_to(Some(ProfileSwitch::new(ReqId(4), Vec::new())));
    sim.list(vec![starting(monitor(21, QHD)), starting(monitor(22, QHD))]);
    sim.world
        .profile_made(vec![k(21), k(22)], &sim.s, sim.head.1, sim.now);
    assert!(sim.world.switching(), "it waits for them");
    sim.stream(21, QHD);
    let made = sim.now;
    sim.world.switch_due(
        &sim.s,
        made + PROFILE_STANDS_WITHIN - Duration::from_millis(1),
    );
    assert!(sim.world.switching());
    assert!(sim.world.placement(k(21)).is_none(), "not one by one");
    sim.world.switch_due(&sim.s, made + PROFILE_STANDS_WITHIN);
    assert!(!sim.world.switching());
    assert!(
        sim.world.placement(k(21)).is_some(),
        "laid out with what streams"
    );
    assert!(sim.world.placement(k(22)).is_none());
}

/// Scenario 12. A monitor easing to another place is hit where it is drawn, not where
/// it goes; once it arrived, where it is.
#[test]
fn a_ray_hits_a_monitor_where_it_is_drawn() {
    let mut sim = Sim::at_desk();
    // The first two from the left: swapped, each goes to the other's place, and is
    // drawn on its way.
    let (one, other) = (sim.world.monitors()[0].0, sim.world.monitors()[1].0);
    let (a, b) = (sim.place(one), sim.place(other));
    sim.world.swap(one, 1);
    assert!(near(&sim.place(one), &b) && near(&sim.place(other), &a));
    sim.after(11);
    let drawn = sim.drawn(one);
    assert!(
        drawn.position.distance(a.position) < drawn.position.distance(b.position),
        "on its way"
    );
    let pick = |sim: &Sim, at: Vec3| {
        sim.world
            .pick(towards(at), &sim.s, Pickable::default(), |_| false)
            .map(|t| t.thing)
    };
    assert_eq!(
        pick(&sim, drawn.position),
        Some(Thing::Display(one)),
        "where it is seen"
    );
    assert_eq!(
        pick(&sim, sim.drawn(other).position),
        Some(Thing::Display(other)),
        "where the other is seen"
    );
    // Where it goes, the other one is still seen: that is what is hit.
    assert_eq!(pick(&sim, b.position), Some(Thing::Display(other)));
    // A monitor asked to go is not hit.
    assert_ne!(
        sim.world
            .pick(
                towards(drawn.position),
                &sim.s,
                Pickable::default(),
                |id| id == one
            )
            .map(|t| t.thing),
        Some(Thing::Display(one))
    );
    sim.settled();
    assert_eq!(pick(&sim, b.position), Some(Thing::Display(one)), "arrived");
    assert_eq!(pick(&sim, a.position), Some(Thing::Display(other)));
}

/// What a ray hits first among things as near: the one higher in the compositor's
/// stack (the palm menu, "Attach", the menu, its button, the placeholder, the
/// monitors, the keyboard window). A placeholder in front of a monitor is hit, not
/// the monitor behind.
#[test]
fn of_two_as_near_the_higher_is_hit() {
    let mut sim = Sim::at_desk();
    let button = sim.world.button().expect("under the first monitor");
    let at = |sim: &Sim, can: Pickable| {
        sim.world
            .pick(towards(button.position), &sim.s, can, |_| false)
            .unwrap()
            .thing
    };
    let open = Pickable {
        menu: true,
        attach: None,
    };
    sim.world.menu = Placement {
        width: panel_width(false),
        ..button
    };
    assert_eq!(at(&sim, Pickable::default()), Thing::MenuButton);
    assert_eq!(at(&sim, open), Thing::Menu, "the menu over its button");
    sim.world.set_wrist(Some(button));
    assert_eq!(at(&sim, open), Thing::Wrist, "the palm menu over all");

    // A placeholder right in front of a monitor.
    let mut sim = Sim::at_desk();
    sim.world
        .adding_started(Some(ReqId(9)), QHD, sim.head.1, sim.now);
    let mut adding = *sim.world.adding().unwrap();
    let m = sim.place(k(2));
    adding.place = Placement {
        position: m.position * 0.95,
        ..m
    };
    sim.world.adding = Some(adding);
    let hit = sim
        .world
        .pick(towards(m.position), &sim.s, Pickable::default(), |_| false)
        .unwrap();
    assert_eq!(
        hit.thing,
        Thing::Adding {
            pc: PC,
            req: ReqId(9)
        }
    );
}

/// Two monitors overlapping, the one drawn over the other 4 cm further away (a
/// monitor glued over a row stays drawn over it, see `layout::DRAW_HOLD`): where they
/// overlap the person sees the one on top, and that is what the ray and the gaze hit,
/// not the nearer one under it. Window panels, flat, are stacked over curved monitors.
#[test]
fn of_monitors_the_one_on_top_is_hit() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    let panel = Display {
        kind: DisplayKind::Window {
            title: String::new(),
        },
        ..monitor(3, (1280, 720))
    };
    sim.list(vec![monitor(1, QHD), monitor(2, QHD), panel]);
    for (n, size) in [(1, QHD), (2, QHD), (3, (1280, 720))] {
        sim.stream(n, size);
    }
    sim.settled();
    let flat = |x: f32, z: f32| Placement {
        position: Vec3::new(x, 0.0, z),
        rotation: Quat::IDENTITY,
        width: 1.25,
    };
    let stand = |sim: &mut Sim, n: u32, p: Placement| {
        sim.world.placements.insert(k(n), p);
        sim.world.shown.insert(k(n), p);
    };
    stand(&mut sim, 1, flat(0.0, -1.0));
    stand(&mut sim, 2, flat(0.5, -1.04));
    stand(&mut sim, 3, flat(1.0, -2.0));
    // Through where 1 and 2 overlap.
    let ray = towards(Vec3::new(0.3, 0.0, -1.0));
    let hit = |sim: &Sim| {
        let picked = sim
            .world
            .pick(ray, &sim.s, Pickable::default(), |_| false)
            .map(|t| t.thing);
        let looked = sim
            .world
            .looked_at(ray, &sim.s, false, |_| false)
            .map(|l| Thing::Display(l.0));
        assert_eq!(picked, looked, "the ray and the gaze agree");
        picked
    };
    sim.world.drawn = vec![k(3), k(1), k(2)];
    assert_eq!(
        hit(&sim),
        Some(Thing::Display(k(2))),
        "on top, though further"
    );
    sim.world.drawn = vec![k(3), k(2), k(1)];
    assert_eq!(hit(&sim), Some(Thing::Display(k(1))));
    // A monitor asked to go is not drawn: the one under it is hit.
    assert_eq!(
        sim.world
            .pick(ray, &sim.s, Pickable::default(), |id| id == k(1))
            .map(|t| t.thing),
        Some(Thing::Display(k(2)))
    );

    // Curved monitors: the window panel, flat, is over them, wherever it is drawn.
    sim.s.curve = crate::settings::Curve::Gentle;
    stand(&mut sim, 3, flat(0.3, -1.1));
    sim.world.drawn = vec![k(3), k(2), k(1)];
    assert_eq!(
        hit(&sim),
        Some(Thing::Display(k(3))),
        "the window, over both"
    );
}

/// Of two monitors as straight ahead, the one in front is the one of the smaller
/// number, every time.
#[test]
fn the_monitor_in_front_of_two_as_ahead() {
    let mut sim = Sim::at_desk();
    let ahead = Placement {
        position: Vec3::new(0.0, 0.0, -1.0),
        rotation: Quat::IDENTITY,
        width: 1.25,
    };
    for n in [3, 1, 2] {
        sim.world.placements.insert(k(n), ahead);
    }
    assert_eq!(sim.world.monitor_in_front(sim.head, |_| false), Some(k(1)));
    assert_eq!(
        sim.world.monitor_in_front(sim.head, |id| id == k(1)),
        Some(k(2))
    );
}

/// Scenario 13. Two PCs with a display of the same number: two displays, each with a
/// place of its own, and moving one leaves the other.
#[test]
fn two_pcs_with_one_number_have_two_places() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD)]);
    sim.stream(1, QHD);
    sim.settled();
    let first = sim.place(k(1));
    sim.world.disconnected();
    sim.world.stream_ended(k(1));

    let theirs = DisplayKey::new(OTHER_PC, DisplayId(1));
    sim.connect(OTHER_PC);
    sim.list(vec![monitor(1, QHD)]);
    sim.stream(1, QHD);
    sim.settled();
    assert!(sim.world.placement(theirs).is_some());
    assert!(near(&sim.place(k(1)), &first), "the first PC's kept");
    let mut p = sim.place(theirs);
    p.position.x += 0.8;
    sim.world.hold(Thing::Display(theirs), &sim.s);
    sim.world
        .drag(&sim.s, Thing::Display(theirs), p, sim.head.0);
    sim.world.let_go(Thing::Display(theirs), &sim.s, sim.now);
    assert!(near(&sim.place(theirs), &p));
    assert!(near(&sim.place(k(1)), &first), "not moved with it");
    let kept = &sim.world.desk().arrangement().placements;
    assert!(
        kept.contains_key(&k(1)) && kept.contains_key(&theirs),
        "both kept"
    );
}

/// Scenario 17. A monitor moved by a hand is drawn at its new place in the same pass
/// (no frame behind the hand), over the others; and one taken on its way goes on from
/// where it is drawn, not from where it was going.
#[test]
fn a_drag_is_drawn_in_the_same_pass() {
    let mut sim = Sim::at_desk();
    sim.world.hold(Thing::Display(k(2)), &sim.s);
    let mut p = sim.place(k(2));
    p.position += Vec3::new(0.0, 0.25, 0.1);
    p.rotation = layout::facing(p.position, sim.head.0);
    sim.world.drag(&sim.s, Thing::Display(k(2)), p, sim.head.0);
    sim.world.animate(&sim.s, 0.011);
    assert!(
        near(&sim.drawn(k(2)), &p),
        "drawn where the hand put it, this pass"
    );
    let order = sim.world.order_drawing(sim.head.0, |_| false);
    assert_eq!(order.last().map(|o| o.1), Some(k(2)), "over the others");
    sim.world.let_go(Thing::Display(k(2)), &sim.s, sim.now);

    // Swapped, and taken on the way.
    sim.settled();
    sim.world.swap(k(1), 1);
    sim.after(30);
    let drawn = sim.drawn(k(1));
    assert!(!near(&drawn, &sim.place(k(1))), "still on its way");
    let at = sim.world.hold(Thing::Display(k(1)), &sim.s).unwrap();
    assert!(near(&at, &drawn), "taken where it is drawn");
    assert!(near(&sim.place(k(1)), &drawn), "and goes on from there");
    sim.after(11);
    assert!(near(&sim.drawn(k(1)), &drawn), "it does not jump");
}

/// With "move all together", a monitor taken on its way takes the others along from
/// where they are drawn too: none of them jumps to where it was going.
#[test]
fn moving_all_from_the_way_nothing_jumps() {
    let mut sim = Sim::at_desk();
    sim.s.move_all = true;
    let (one, other) = (sim.world.monitors()[0].0, sim.world.monitors()[1].0);
    sim.world.swap(one, 1);
    sim.after(30);
    let drawn: HashMap<DisplayKey, Placement> = sim
        .world
        .monitors()
        .iter()
        .map(|m| (m.0, sim.drawn(m.0)))
        .collect();
    assert!(!near(&drawn[&other], &sim.place(other)), "on its way too");
    sim.world.hold(Thing::Display(one), &sim.s);
    for (k, p) in &drawn {
        assert!(
            near(&sim.place(*k), p),
            "{k}: goes on from where it is drawn"
        );
    }
    // Moved a little: all go along, from where they were seen.
    let mut p = sim.place(one);
    p.position.y += 0.1;
    sim.world.drag(&sim.s, Thing::Display(one), p, sim.head.0);
    sim.world.animate(&sim.s, 0.011);
    for (k, was) in &drawn {
        let now = sim.drawn(*k);
        assert!(
            (now.position - was.position - Vec3::new(0.0, 0.1, 0.0)).length() < 1e-4,
            "{k} moved with it, and only that"
        );
    }
}

/// Where the head looks, `yaw` radians from straight ahead (positive to the left, as
/// the arrangement's center counts it).
fn looking(yaw: f32) -> (Vec3, Vec3) {
    (Vec3::ZERO, Quat::from_rotation_y(yaw) * -Vec3::Z)
}

/// The way the arrangement faces, seen from the head.
fn facing(sim: &Sim) -> f32 {
    layout::facing_yaw(&sim.places(), sim.head.0).unwrap()
}

/// Following the head: turned well away, the arrangement glides after it and is kept
/// once in front again; a small turn leaves it. A monitor let go with the head turned
/// stays there until the head comes back near where the arrangement faces.
#[test]
fn following_the_head() {
    let mut sim = Sim::at_desk();
    sim.s.follow = true;
    let was = facing(&sim);
    sim.head = looking(0.2);
    sim.settled();
    assert!((facing(&sim) - was).abs() < 1e-4, "a small turn leaves it");
    sim.world.take_saves();
    sim.head = looking(1.0);
    sim.settled();
    assert!((facing(&sim) - 1.0).abs() < 0.03, "in front again");
    assert!(sim.world.take_saves().has(File::Desk), "kept where it came");

    // Let go with the head turned away: it waits for the head to come back first.
    sim.head = looking(1.0);
    let id = sim.world.monitors()[0].0;
    sim.world.hold(Thing::Display(id), &sim.s);
    sim.head = looking(2.0);
    sim.world.let_go(Thing::Display(id), &sim.s, sim.now);
    let stays = facing(&sim);
    sim.settled();
    assert!((facing(&sim) - stays).abs() < 1e-4, "waits for the head");
    sim.head = looking(1.0);
    sim.settled();
    sim.head = looking(2.0);
    sim.settled();
    assert!(
        (facing(&sim) - 2.0).abs() < 0.03,
        "back, and following again"
    );
}

/// Closer and further a step at a time, each keeping its size and the arrangement its
/// shape; in front: the arrangement turned and carried to the head as it stood.
#[test]
fn closer_further_and_in_front() {
    let mut sim = Sim::at_desk();
    let before = sim.places();
    sim.world.closer(-1, sim.now);
    for (k, p) in &before {
        let q = sim.place(*k);
        assert!((q.position.length() - p.position.length() / 1.15).abs() < 1e-4);
        assert!((q.width - p.width).abs() < 1e-6, "its size kept");
    }
    sim.world.closer(1, sim.now);
    assert!(same_places(&sim.places(), &before), "and back");

    // Turned and moved: in front again, the shape as it was.
    sim.head = (
        Vec3::new(0.3, 0.1, -0.2),
        Quat::from_rotation_y(1.2) * -Vec3::Z,
    );
    sim.world.recenter(&sim.s, sim.head, sim.now);
    assert!((facing(&sim) - 1.2).abs() < 1e-3, "in front of the head");
    let ids: Vec<DisplayKey> = before.keys().copied().collect();
    for a in &ids {
        for b in &ids {
            let was = before[a].position.distance(before[b].position);
            let now = sim.place(*a).position.distance(sim.place(*b).position);
            assert!((was - now).abs() < 1e-4, "the shape kept");
        }
    }
}

/// A profile switched to in cinema: its places become the desk arrangement under the
/// cinema view, and they show once cinema ends.
#[test]
fn a_profile_switched_to_in_cinema() {
    let mut sim = Sim::at_desk();
    let mut p = sim.place(k(1));
    p.position += Vec3::new(0.0, 0.3, 0.0);
    sim.world.hold(Thing::Display(k(1)), &sim.s);
    sim.world.drag(&sim.s, Thing::Display(k(1)), p, sim.head.0);
    sim.world.let_go(Thing::Display(k(1)), &sim.s, sim.now);
    sim.settled();
    let saved = sim.world.profile(Vec::new(), |_| 100);
    let at_save = sim.places();
    let order: Vec<DisplayKey> = sim.world.monitors().iter().map(|m| m.0).collect();
    sim.world.arrange_all(&sim.s, sim.now);
    sim.settled();

    cinema(&mut sim, true);
    sim.world.switch_to(Some(ProfileSwitch {
        frame: saved.frame,
        rows: saved.rows,
        ..ProfileSwitch::new(ReqId(3), saved.places)
    }));
    sim.list(vec![monitor(11, QHD), monitor(12, QHD), monitor(13, QHD)]);
    sim.world
        .profile_made(vec![k(11), k(12), k(13)], &sim.s, sim.head.1, sim.now);
    for n in 11..=13 {
        sim.stream(n, QHD);
    }
    sim.settled();
    assert_eq!(sim.world.placements.len(), 1, "still the cinema view");
    let desk = &sim.world.desk().arrangement().placements;
    for (new, old) in [11, 12, 13].into_iter().zip(&order) {
        assert!(near(&desk[&k(new)], &at_save[old]), "kept for the desk");
    }
    cinema(&mut sim, false);
    for (new, old) in [11, 12, 13].into_iter().zip(&order) {
        assert!(
            near(&sim.place(k(new)), &at_save[old]),
            "{new} shown where {old} was"
        );
    }
}

/// A PC forgotten: the places of its displays go, from the desk arrangement too, and
/// so does cinema's pick of one of them; another PC's stay.
#[test]
fn forgetting_a_pc_forgets_its_places() {
    let mut sim = Sim::at_desk();
    let theirs = DisplayKey::new(OTHER_PC, DisplayId(1));
    let p = sim.place(k(1));
    sim.world.placements.insert(theirs, p);
    sim.world.save_layout();
    sim.world.pick_cinema(k(2), &sim.s, sim.now);
    sim.world.take_saves();
    sim.world.forget_pc(PC);
    assert!(sim.world.take_saves().has(File::Desk));
    assert_eq!(
        sim.world.placements.keys().copied().collect::<Vec<_>>(),
        [theirs]
    );
    let desk = sim.world.desk();
    assert_eq!(
        desk.arrangement()
            .placements
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [theirs]
    );
    assert_eq!(desk.cinema(), None);
}

/// Let go almost against a neighbour, a monitor sticks to it, edge to edge where it
/// was (true: the hand feels it); with the magnet off it stays where it was let go.
#[test]
fn the_magnet_on_letting_go() {
    for magnet in [true, false] {
        let mut sim = Sim::at_desk();
        sim.s.magnet = magnet;
        let id = sim.world.monitors().last().unwrap().0;
        let was = sim.place(id);
        let mut p = was;
        p.position += p.rotation * Vec3::new(0.04, 0.0, 0.0);
        sim.world.hold(Thing::Display(id), &sim.s);
        sim.world.drag(&sim.s, Thing::Display(id), p, sim.head.0);
        let stuck = sim.world.let_go(Thing::Display(id), &sim.s, sim.now);
        assert_eq!(stuck, magnet);
        let now = sim.place(id);
        if magnet {
            assert!(
                now.position.distance(was.position) < 1e-3,
                "back against it"
            );
        } else {
            assert!(near(&now, &p), "where it was let go");
        }
    }
}

/// The chosen layout chosen again lays the monitors out by it anew, as choosing it
/// does: a monitor moved by hand goes back to its place in the layout.
#[test]
fn the_chosen_layout_again_lays_out_anew() {
    let mut sim = Sim::at_desk();
    sim.world.preset_chosen(&sim.s, sim.now);
    let laid = sim.places();
    let mut p = sim.place(k(1));
    p.position += Vec3::new(0.2, 0.4, 0.1);
    sim.world.hold(Thing::Display(k(1)), &sim.s);
    sim.world.drag(&sim.s, Thing::Display(k(1)), p, sim.head.0);
    sim.world.let_go(Thing::Display(k(1)), &sim.s, sim.now);
    assert!(!same_places(&sim.places(), &laid));
    sim.world.preset_chosen(&sim.s, sim.now);
    assert!(same_places(&sim.places(), &laid), "laid out anew");
}

/// Laid out anew, what the hands held is let go (the app's grabs follow it).
#[test]
fn laying_out_lets_go() {
    let mut sim = Sim::at_desk();
    sim.world.hold(Thing::Display(k(1)), &sim.s);
    sim.world.hold(Thing::Menu, &sim.s);
    sim.world.tidy_up(&sim.s, sim.now);
    assert!(!sim.world.holding());
    sim.world.hold(Thing::Display(k(3)), &sim.s);
    sim.world.arrange_all(&sim.s, sim.now);
    assert!(!sim.world.holding());
    // A monitor gone from the list is let go; the other hand keeps the menu.
    sim.world.hold(Thing::Display(k(3)), &sim.s);
    sim.world.hold(Thing::Menu, &sim.s);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    assert!(!sim.world.holds(Thing::Display(k(3))));
    assert!(sim.world.holds(Thing::Menu));
}

/// The keyboard window lies flat wherever a hand takes it; on a wall a monitor stays
/// parallel to it; with "move all together" the rest goes along.
#[test]
fn dragging_keeps_the_rules() {
    let mut sim = Sim::at_desk();
    let head = sim.head.0;
    let keyboard = sim.world.keyboard_here(sim.head);
    sim.world.hold(Thing::Keyboard, &sim.s);
    let mut p = keyboard;
    p.position += Vec3::new(0.2, 0.1, 0.0);
    p.rotation = Quat::IDENTITY;
    sim.world.drag(&sim.s, Thing::Keyboard, p, head);
    let now = sim.world.keyboard().unwrap();
    assert!(now.rotation.dot(lying_flat(now.position, head)).abs() > 1.0 - 1e-6);
    sim.world.take_saves();
    sim.world.let_go(Thing::Keyboard, &sim.s, sim.now);
    assert!(sim.world.take_saves().has(File::Keyboard));

    sim.s.move_all = true;
    let others = (sim.place(k(1)), sim.place(k(3)));
    let mut p = sim.place(k(2));
    p.position.y += 0.3;
    sim.world.hold(Thing::Display(k(2)), &sim.s);
    sim.world.drag(&sim.s, Thing::Display(k(2)), p, head);
    assert!((sim.place(k(1)).position.y - others.0.position.y - 0.3).abs() < 1e-5);
    assert!((sim.place(k(3)).position.y - others.1.position.y - 0.3).abs() < 1e-5);
    sim.world.let_go(Thing::Display(k(2)), &sim.s, sim.now);
}
