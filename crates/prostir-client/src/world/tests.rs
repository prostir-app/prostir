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

mod property;

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
    /// Every PC told its rows, in order.
    told: Vec<PcId>,
}

impl Sim {
    /// Started with `layout.txt` holding `text` (none: no file).
    fn new(text: Option<&str>) -> Self {
        Self::on(text, None)
    }

    /// [`Sim::new`] on a headset of `ppd` pixels per degree (`None`: a model not in the
    /// table, the standard size).
    fn on(text: Option<&str>, ppd: Option<f32>) -> Self {
        let desk = text.map_or_else(Desk::default, |t| Desk::parse(t).unwrap().0);
        let s = Settings::default();
        #[expect(
            clippy::disallowed_methods,
            reason = "a test's own starting time; the code under test takes the time as a parameter"
        )]
        let now = Instant::now();
        Self {
            world: World::new(desk, None, true, ppd, &s),
            s,
            now,
            head: (Vec3::ZERO, -Vec3::Z),
            pc: PC,
            arranged: Vec::new(),
            told: Vec::new(),
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
            .listed(self.pc, &displays, &self.s, self.head, self.now)
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
        while let Some((pc, rows)) = self.world.take_arrange() {
            self.told.push(pc);
            if pc == self.pc {
                self.arranged.push(rows);
            }
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
        &[],
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
    sim.world.adding_made(PC, ReqId(5), Some(k(4)));
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
    sim.world.adding_made(PC, ReqId(6), Some(k(5)));
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
    sim.world.adding_made(PC, ReqId(7), Some(k(6)));
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
    sim.world.resize(&sim.s, 1.0, sim.head.0);
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

    sim.world.disconnected(sim.pc);
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
    sim.world.switch_to(Some((
        PC,
        ProfileSwitch {
            frame: saved.frame,
            rows: saved.rows,
            ..ProfileSwitch::new(ReqId(3), saved.places)
        },
    )));
    assert!(sim.world.switching_with(PC, ReqId(3)));
    assert!(
        !sim.world.switching_with(OTHER_PC, ReqId(3)),
        "another PC's number 3"
    );
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
        .switch_to(Some((PC, ProfileSwitch::new(ReqId(4), Vec::new()))));
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
    sim.world.swap(&sim.s, one, 1);
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
    sim.world.disconnected(sim.pc);
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

/// Scenario 14 (step 7 note, 10.1), the world's part: two PCs in a session at once each
/// have their own list and are told the rows of their own displays only; one ending
/// leaves the other's list as it is.
#[test]
fn two_pcs_at_once_keep_their_own_desktops() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    sim.stream(1, QHD);
    sim.stream(2, QHD);
    sim.connect(OTHER_PC);
    sim.list(vec![monitor(1, QHD)]);
    sim.stream(1, QHD);
    sim.settled();
    assert_eq!(
        sim.world.pc(),
        Some(PC),
        "the first PC in a session is the main one"
    );
    let mut told = sim.told.clone();
    told.sort();
    told.dedup();
    assert_eq!(told, [PC, OTHER_PC], "each PC told its desktop");
    let theirs = DisplayKey::new(OTHER_PC, DisplayId(1));
    // Each PC's monitors in its own order (the desktop it was told), by PC.
    let all: Vec<DisplayKey> = sim.world.monitors().iter().map(|m| m.0).collect();
    assert_eq!(all.len(), 3);
    assert!(
        all[..2].contains(&k(1)) && all[..2].contains(&k(2)),
        "{all:?}"
    );
    assert_eq!(all[2], theirs);
    // The second ends: the first's displays stay listed and placed.
    sim.world.disconnected(OTHER_PC);
    sim.world.stream_ended(theirs);
    assert_eq!(sim.world.pc(), Some(PC));
    sim.pc = PC;
    let gone = sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    assert!(gone.is_empty(), "{gone:?}");
    assert!(sim.world.placement(theirs).is_some(), "its place kept");
    let all: Vec<DisplayKey> = sim.world.monitors().iter().map(|m| m.0).collect();
    assert!(all.len() == 2 && all.iter().all(|m| m.pc == PC), "{all:?}");
}

/// Two PCs number their requests each on their own: an answer of the other PC with
/// the same number neither takes the placeholder's monitor nor ends the profile being
/// switched to.
#[test]
fn answers_are_known_by_their_pc() {
    let mut sim = Sim::new(None);
    sim.connect(OTHER_PC);
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD)]);
    sim.stream(1, QHD);
    sim.settled();
    sim.world
        .adding_started(Some(ReqId(5)), QHD, sim.head.1, sim.now);
    let theirs = DisplayKey::new(OTHER_PC, DisplayId(4));
    // The world's main PC is the first in a session, by number.
    let main = sim.world.pc().unwrap();
    let other = if main == PC { OTHER_PC } else { PC };
    sim.world.adding_made(other, ReqId(5), Some(theirs));
    assert_eq!(
        sim.world.adding().and_then(|a| a.id),
        None,
        "not the other PC's"
    );
    let ours = DisplayKey::new(main, DisplayId(4));
    sim.world.adding_made(main, ReqId(5), Some(ours));
    assert_eq!(sim.world.adding().and_then(|a| a.id), Some(ours));
    sim.world
        .switch_to(Some((main, ProfileSwitch::new(ReqId(7), Vec::new()))));
    sim.world.switch_given_up(other, ReqId(7));
    assert!(
        sim.world.switching(),
        "another PC's request 7 timed out, not ours"
    );
    sim.world.switch_given_up(main, ReqId(7));
    assert!(!sim.world.switching());
}

/// Scenario 16 (step 7 note, 10.1), the world's part: the desktop's arrangement is told
/// once it stood, also with no frame drawn (the headset off the head): the passes of
/// the main loop without frames settle it.
#[test]
fn the_desktop_is_told_without_frames() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    sim.list(three());
    for n in 1..=3 {
        sim.stream(n, QHD);
    }
    let mut told = None;
    for _ in 0..20 {
        sim.now += Duration::from_millis(100);
        sim.world.settle(sim.now);
        if let Some(t) = sim.world.take_arrange() {
            told = Some(t);
            break;
        }
    }
    let (pc, rows) = told.expect("told without a frame");
    assert_eq!(pc, PC);
    assert_eq!(rows.iter().map(|r| r.entries.len()).sum::<usize>(), 3);
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
    sim.world.swap(&sim.s, k(1), 1);
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
    sim.world.swap(&sim.s, one, 1);
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

/// Turned and moved: the arrangement is carried in front of the head, its shape as it
/// stood.
#[test]
fn recenter_keeps_the_arrangement_shape() {
    let mut sim = Sim::at_desk();
    let before = sim.places();

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
    sim.world.switch_to(Some((
        PC,
        ProfileSwitch {
            frame: saved.frame,
            rows: saved.rows,
            ..ProfileSwitch::new(ReqId(3), saved.places)
        },
    )));
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

/// A display of the PC itself, shown, where `desk` (x, y, width, height) says it is on
/// the PC's desktop, `size` pixels.
fn pc_display(n: u32, size: (u32, u32), desk: [i32; 4]) -> Display {
    Display {
        kind: DisplayKind::Physical {
            builtin: false,
            connector: format!("DP-{n}"),
            shown: true,
        },
        desk: Some(prostir_proto::state::DeskRect {
            x: desk[0],
            y: desk[1],
            w: desk[2].unsigned_abs(),
            h: desk[3].unsigned_abs(),
        }),
        ..monitor(n, size)
    }
}

/// Whether `a` and `b` (`a_px`, `b_px` pixels: their shape) cover each other as seen
/// from in front: near enough the same plane, and their rectangles overlapping by more
/// than a hair.
fn cover(a: &Placement, a_px: (u32, u32), b: &Placement, b_px: (u32, u32)) -> bool {
    let (ah, bh) = (
        a.width * a_px.1 as f32 / a_px.0 as f32,
        b.width * b_px.1 as f32 / b_px.0 as f32,
    );
    // `b`'s corners on `a`'s plane.
    let corners = [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)].map(|(x, y)| {
        let at = b.position + b.rotation * Vec3::new(x * b.width / 2.0, y * bh / 2.0, 0.0);
        a.rotation.inverse() * (at - a.position)
    });
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for c in &corners {
        (x0, x1, y0, y1) = (x0.min(c.x), x1.max(c.x), y0.min(c.y), y1.max(c.y));
    }
    let eps = 1e-4;
    corners.iter().all(|c| c.z.abs() < 0.3)
        && x1 > -a.width / 2.0 + eps
        && x0 < a.width / 2.0 - eps
        && y1 > -ah / 2.0 + eps
        && y0 < ah / 2.0 - eps
}

/// Displays by number, with their sizes in pixels.
type Sized = Vec<(u32, (u32, u32))>;

/// The monitors and three displays of the PC side by side on its desktop, all
/// streaming and drawn.
fn with_three_pc_displays() -> (Sim, Sized) {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    let pcs = [
        (101, (1920, 1080), [0, 0, 1920, 1080]),
        (102, (2560, 1440), [1920, 0, 2560, 1440]),
        (103, (1920, 1080), [4480, 360, 1920, 1080]),
    ];
    let mut displays = three();
    displays.extend(pcs.iter().map(|&(n, size, desk)| pc_display(n, size, desk)));
    sim.list(displays);
    let mut sizes: Vec<(u32, (u32, u32))> = (1..=3).map(|n| (n, QHD)).collect();
    sizes.extend(pcs.iter().map(|&(n, size, _)| (n, size)));
    for &(n, size) in &sizes {
        sim.stream(n, size);
    }
    sim.settled();
    (sim, sizes)
}

/// No two of `sizes` (number, pixels) cover each other.
fn none_covers(sim: &Sim, sizes: &[(u32, (u32, u32))]) {
    for (i, &(a, ap)) in sizes.iter().enumerate() {
        for &(b, bp) in &sizes[i + 1..] {
            assert!(
                !cover(&sim.place(k(a)), ap, &sim.place(k(b)), bp),
                "{a} and {b} cover each other: {:?} {:?}",
                sim.place(k(a)),
                sim.place(k(b))
            );
        }
    }
}

/// No two of `sizes` (number, pixels) seen over each other from the eye, by more than
/// a hundredth of the smaller one across and up.
fn none_seen_over(sim: &Sim, sizes: &[(u32, (u32, u32))]) {
    use prostir_layout::space::{Monitor, PixelSize, overlaps, see};
    let frame = layout::frame(sim.world.center, sim.world.tilt);
    let all: Vec<Monitor<u32>> = sizes
        .iter()
        .map(|&(n, (w, h))| Monitor {
            id: n,
            px: PixelSize::new(w, h),
            placement: sim.place(k(n)),
        })
        .collect();
    let seen = see(&frame, &all);
    for (i, a) in seen.iter().enumerate() {
        for b in &seen[i + 1..] {
            assert!(
                !overlaps(a, b, 0.01),
                "{} and {} seen over each other: {a:?} {b:?}",
                a.id,
                b.id
            );
        }
    }
}

/// No two of `sizes` (number, pixels) cut into each other in the room.
fn none_cut(sim: &Sim, sizes: &[(u32, (u32, u32))]) {
    let height = |p: &Placement, (w, h): (u32, u32)| p.width * h as f32 / w as f32;
    for (i, &(a, ap)) in sizes.iter().enumerate() {
        for &(b, bp) in &sizes[i + 1..] {
            let (pa, pb) = (sim.place(k(a)), sim.place(k(b)));
            assert!(
                !layout::cuts(&pa, height(&pa, ap), &pb, height(&pb, bp)),
                "{a} and {b} cut into each other: {pa:?} {pb:?}"
            );
        }
    }
}

/// The PC's displays glued to each other on its desktop stand a little apart in the
/// headset, never over each other; "Line up" takes them along with the first of them
/// and leaves no monitor covering any.
#[test]
fn the_pcs_displays_stand_apart_and_line_up_along() {
    let (mut sim, sizes) = with_three_pc_displays();
    none_covers(&sim, &sizes);
    let (a, b) = (sim.place(k(101)), sim.place(k(102)));
    let side = a.rotation * Vec3::X;
    let gap =
        (b.position - b.rotation * Vec3::X * b.width / 2.0 - (a.position + side * a.width / 2.0))
            .dot(side);
    assert!(gap > 0.0 && gap < 0.01, "glued, a hair apart: {gap}");
    // A monitor pushed down over them, then "Line up".
    let mut low = sim.place(k(2));
    low.position.y -= 0.35;
    sim.world.placements.insert(k(2), low);
    sim.world.tidy_up(&sim.s, sim.now);
    sim.settled();
    none_covers(&sim, &sizes);
}

/// Displays that show one desktop cloned (the same place on it) stand side by side,
/// never on top of each other.
#[test]
fn cloned_displays_stand_side_by_side() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    let mut displays = three();
    displays.push(pc_display(101, (1920, 1080), [0, 0, 1920, 1080]));
    displays.push(pc_display(102, (1920, 1080), [0, 0, 1920, 1080]));
    sim.list(displays);
    let mut sizes: Vec<(u32, (u32, u32))> = (1..=3).map(|n| (n, QHD)).collect();
    sizes.extend([(101, (1920, 1080)), (102, (1920, 1080))]);
    for &(n, size) in &sizes {
        sim.stream(n, size);
    }
    sim.settled();
    none_covers(&sim, &sizes);
    let (a, b) = (sim.place(k(101)), sim.place(k(102)));
    assert!((a.position.y - b.position.y).abs() < 1e-4, "side by side");
}

/// A display of the PC moved by hand stays where it was put when another one starts
/// showing; the new one stands next to it as the group has it.
#[test]
fn a_display_moved_by_hand_stays_when_another_comes() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    let first = pc_display(101, (1920, 1080), [0, 0, 1920, 1080]);
    let second = pc_display(102, (1920, 1080), [1920, 0, 1920, 1080]);
    let mut displays = three();
    displays.push(first.clone());
    sim.list(displays);
    for n in 1..=3 {
        sim.stream(n, QHD);
    }
    sim.stream(101, (1920, 1080));
    sim.settled();
    // Moved by hand, up and to the left.
    let mut moved = sim.place(k(101));
    moved.position += Vec3::new(-0.4, 0.2, 0.0);
    sim.world.placements.insert(k(101), moved);
    let mut displays = three();
    displays.extend([first, second]);
    sim.list(displays);
    sim.stream(102, (1920, 1080));
    sim.settled();
    assert!(near(&sim.place(k(101)), &moved), "where it was put");
    let (a, b) = (sim.place(k(101)), sim.place(k(102)));
    let local = a.rotation.inverse() * (b.position - a.position);
    assert!(
        local.x > a.width * 0.9 && local.y.abs() < 1e-3,
        "beside it: {local:?}"
    );
}

/// The ids of each row, top to bottom.
fn row_ids(rows: &[Row]) -> Vec<Vec<u32>> {
    rows.iter()
        .map(|r| r.ids().map(|id| id.0).collect())
        .collect()
}

/// A monitor that could not start and never had a place (it stays in the list,
/// failed) does not hold the desktop up: a monitor moved by hand is still told.
#[test]
fn a_failed_monitor_never_placed_does_not_hold_the_desktop_up() {
    let mut sim = Sim::new(None);
    sim.s.magnet = false;
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    sim.stream(1, QHD);
    sim.stream(2, QHD);
    sim.settled();
    sim.list(vec![
        monitor(1, QHD),
        monitor(2, QHD),
        failed(monitor(3, QHD)),
    ]);
    sim.settled();
    sim.arranged.clear();
    // Monitor 2 put well above monitor 1, by hand.
    let mut p = sim.place(k(1));
    p.position.y += 1.0;
    sim.world.hold(Thing::Display(k(2)), &sim.s);
    sim.world.drag(&sim.s, Thing::Display(k(2)), p, sim.head.0);
    sim.world.let_go(Thing::Display(k(2)), &sim.s, sim.now);
    sim.settled();
    assert!(sim.world.placement(k(3)).is_none(), "never placed");
    let told = sim.arranged.last().map(|r| row_ids(r));
    assert_eq!(told, Some(vec![vec![2], vec![1]]), "the PC is told");
}

/// "Add" pressed twice before the first new monitor's picture comes: each new monitor
/// gets a place of its own next to the others, and the placeholder still waiting moves
/// out of the way of the one placed first.
#[test]
fn two_monitors_added_at_once_each_get_a_place() {
    let mut sim = Sim::new(None);
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    sim.stream(1, QHD);
    sim.stream(2, QHD);
    sim.settled();
    let gaze = sim.head.1;
    sim.world.adding_started(Some(ReqId(1)), QHD, gaze, sim.now);
    sim.world.adding_started(Some(ReqId(2)), QHD, gaze, sim.now);
    sim.world.adding_made(PC, ReqId(1), Some(k(3)));
    sim.world.adding_made(PC, ReqId(2), Some(k(4)));
    let mut list = vec![monitor(1, QHD), monitor(2, QHD), monitor(3, QHD)];
    sim.list(list.clone());
    sim.stream(3, QHD);
    let waiting = sim.world.adding().expect("the second one waits").place;
    assert!(
        !cover(&sim.place(k(3)), QHD, &waiting, QHD),
        "its placeholder is where the first one went"
    );
    list.push(monitor(4, QHD));
    sim.list(list);
    sim.stream(4, QHD);
    sim.settled();
    none_covers(&sim, &[(1, QHD), (2, QHD), (3, QHD), (4, QHD)]);
}

/// Two monitors side by side, the right one raised by hand, a step at a time, until it
/// shares only a third of its height with the other: still one row, as the PC has it
/// (a monitor leaves its row only once clearly moved out of it). The rows the PC was
/// told, top to bottom.
fn raised_beside() -> (Sim, Vec<Vec<u32>>) {
    let mut sim = Sim::new(None);
    sim.s.magnet = false;
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    sim.stream(1, QHD);
    sim.stream(2, QHD);
    sim.settled();
    let h = sim.world.height_of(k(2));
    for _ in 0..13 {
        let mut p = sim.place(k(2));
        p.position.y += h * 0.05;
        sim.world.hold(Thing::Display(k(2)), &sim.s);
        sim.world.drag(&sim.s, Thing::Display(k(2)), p, sim.head.0);
        sim.world.let_go(Thing::Display(k(2)), &sim.s, sim.now);
        sim.settled();
    }
    let rows = row_ids(sim.arranged.last().expect("told"));
    assert_eq!(rows.len(), 1, "one row: {rows:?}");
    (sim, rows)
}

/// The link drops and comes back with nothing moved: the PC is told its desktop again,
/// the same rows (they are kept, as those it had, through the reconnect).
#[test]
fn a_reconnect_keeps_the_rows() {
    let (mut sim, rows) = raised_beside();
    let places = sim.places();
    sim.world.disconnected(PC);
    sim.world.stream_ended(k(1));
    sim.world.stream_ended(k(2));
    sim.arranged.clear();
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    sim.stream(1, QHD);
    sim.stream(2, QHD);
    sim.settled();
    assert!(same_places(&sim.places(), &places), "nothing moved");
    let told = sim.arranged.last().map(|r| row_ids(r));
    assert_eq!(told, Some(rows), "told again, the same rows");
}

/// The app starts again with what `layout.txt` keeps: the PC is told the rows it had.
#[test]
fn a_restart_keeps_the_rows() {
    let (sim, rows) = raised_beside();
    let mut text = String::new();
    sim.world.desk().write_text(&mut text);
    let mut again = Sim::new(Some(&text));
    again.connect(PC);
    again.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    again.stream(1, QHD);
    again.stream(2, QHD);
    again.settled();
    assert!(
        same_places(&again.places(), &sim.places()),
        "where they were"
    );
    let told = again.arranged.last().map(|r| row_ids(r));
    assert_eq!(told, Some(rows), "the same rows");
}

/// Monitors of different widths moved left or right ("Move" in the menu): the two
/// swapped stand edge to edge where the pair stood, neither over the other or over a
/// neighbour.
#[test]
fn swapping_monitors_of_different_widths_keeps_them_apart() {
    let sizes = [(1, (3440, 1440)), (2, (1920, 1080)), (3, QHD)];
    for (first, delta) in [(0, 1), (1, 1), (2, -1), (1, -1)] {
        let mut sim = Sim::new(None);
        sim.connect(PC);
        sim.list(sizes.iter().map(|&(n, s)| monitor(n, s)).collect());
        for &(n, s) in &sizes {
            sim.stream(n, s);
        }
        sim.settled();
        let order: Vec<DisplayKey> = sim.world.monitors().iter().map(|m| m.0).collect();
        sim.world.swap(&sim.s, order[first], delta);
        none_seen_over(&sim, &sizes);
        none_cut(&sim, &sizes);
    }
}

/// A monitor comes back at a larger resolution under the same number (the PC changed
/// it): the same size of a pixel, so it is wider, and its neighbours in the row move
/// out of its way; none is over another.
#[test]
fn a_monitor_grown_pushes_its_neighbours_clear() {
    for n in 1..=3 {
        let mut sim = Sim::at_desk();
        sim.world.stream_ended(k(n));
        let mut list = three();
        list[n as usize - 1] = monitor(n, (3840, 2160));
        sim.list(list);
        sim.stream(n, (3840, 2160));
        sim.settled();
        let mut sizes = vec![(1, QHD), (2, QHD), (3, QHD)];
        sizes[n as usize - 1].1 = (3840, 2160);
        assert!(
            (sim.place(k(n)).width - 1.875).abs() < 1e-4,
            "as large a pixel"
        );
        none_seen_over(&sim, &sizes);
        none_cut(&sim, &sizes);
    }
}

/// Every monitor of `ns` has its middle in view, and none goes through another.
fn all_in_view(sim: &Sim, ns: &[u32], what: &str) {
    let at = (sim.world.center, sim.world.tilt);
    for (i, &a) in ns.iter().enumerate() {
        assert!(layout::in_view(&sim.place(k(a)), at), "{what}: {a} in view");
        for &b in &ns[i + 1..] {
            let (pa, pb) = (sim.place(k(a)), sim.place(k(b)));
            let (ha, hb) = (sim.world.height_of(k(a)), sim.world.height_of(k(b)));
            assert!(
                !layout::cuts(&pa, ha, &pb, hb),
                "{what}: {a} goes through {b}"
            );
        }
    }
}

/// The owner's report of 2026-11-25: a 5120x1440 monitor added to three went right over
/// the head, and stayed there through "Line up" (its row of its own kept as the rows
/// the PC has, in `layout.txt` too) and the preset. Now it goes into the row, which
/// makes room; put back over the head by hand, "Line up" brings it into the row again,
/// after a restart as well; and so does the preset.
#[test]
fn a_wide_monitor_is_always_brought_into_view() {
    const WIDE: (u32, u32) = (5120, 1440);
    let mut sim = Sim::at_desk();
    let before = sim.places();
    let mut list = three();
    list.push(monitor(4, WIDE));
    sim.list(list.clone());
    sim.stream(4, WIDE);
    sim.settled();
    all_in_view(&sim, &[1, 2, 3, 4], "added");
    let rows = row_ids(sim.arranged.last().expect("told"));
    assert_eq!(rows.len(), 1, "into the row: {rows:?}");
    // Where it went before, right over the head, the others where they were.
    let over = prostir_layout::circle::on_cylinder(0.0, 0.67, 2.5, 1.25 * 1.001);
    sim.world.placements.extend(before);
    sim.world.placements.insert(k(4), over);
    sim.world.save_layout();
    sim.settled();
    let rows = row_ids(sim.arranged.last().expect("told"));
    assert_eq!(rows, [vec![4], vec![2, 1, 3]], "a row of its own");
    let mut text = String::new();
    sim.world.desk().write_text(&mut text);
    sim.world.tidy_up(&sim.s, sim.now);
    sim.settled();
    all_in_view(&sim, &[1, 2, 3, 4], "lined up");
    let rows = row_ids(sim.arranged.last().expect("told"));
    assert_eq!(rows.len(), 1, "lined up into the row: {rows:?}");
    // The app started again with that arrangement and its rows.
    let mut again = Sim::new(Some(&text));
    again.connect(PC);
    again.list(list.clone());
    for (n, size) in [(1, QHD), (2, QHD), (3, QHD), (4, WIDE)] {
        again.stream(n, size);
    }
    again.settled();
    assert!(!layout::in_view(
        &again.place(k(4)),
        (again.world.center, again.world.tilt)
    ));
    again.world.tidy_up(&again.s, again.now);
    again.settled();
    all_in_view(&again, &[1, 2, 3, 4], "lined up after a restart");
    // And the preset.
    again.world.preset_chosen(&again.s, again.now);
    again.settled();
    all_in_view(&again, &[1, 2, 3, 4], "the preset");
}

/// A 5120x1440 monitor added to three goes into the row, whose monitors make room only
/// once its picture comes: till then its placeholder waits where it covers none of
/// them and goes through none (not on the seam it will take, over two of them).
#[test]
fn a_wide_monitor_waits_where_it_covers_nothing() {
    const WIDE: (u32, u32) = (5120, 1440);
    let mut sim = Sim::at_desk();
    sim.world
        .adding_started(Some(ReqId(1)), WIDE, sim.head.1, sim.now);
    let waiting = *sim.world.adding().expect("a placeholder");
    assert!(waiting.into_row, "into the row once it comes");
    let at = (sim.world.center, sim.world.tilt);
    let sizes: Vec<(DisplayKey, u32, u32)> = (1..=3).map(|n| (k(n), QHD.0, QHD.1)).collect();
    assert!(
        !layout::covers_any(
            &sim.world.placements,
            &sizes,
            k(4),
            WIDE,
            &waiting.place,
            at
        ),
        "the placeholder covers none of the three"
    );
    sim.world.adding_made(PC, ReqId(1), Some(k(4)));
    let mut list = three();
    list.push(monitor(4, WIDE));
    sim.list(list);
    sim.stream(4, WIDE);
    sim.settled();
    all_in_view(&sim, &[1, 2, 3, 4], "added");
    let rows = row_ids(sim.arranged.last().expect("told"));
    assert_eq!(rows.len(), 1, "into the row: {rows:?}");
}

/// Monitors 1 and 2 of `lower` (sizes) and 3 and 4 of `upper` laid out by `preset` as
/// it stacked rows before 2026-11-25: on the circle of 1.1 m, each row centred ahead,
/// the upper one standing on the lower one as seen; in the preset's frame.
fn stacked_before(sim: &mut Sim, preset: Preset, lower: [(u32, u32); 2], upper: [(u32, u32); 2]) {
    use prostir_layout::circle::{angular_width, clear_of, on_cylinder};
    let sizes: Vec<(u32, (u32, u32))> = (1..).zip(lower.into_iter().chain(upper)).collect();
    sim.connect(PC);
    sim.list(sizes.iter().map(|&(n, size)| monitor(n, size)).collect());
    for &(n, size) in &sizes {
        sim.stream(n, size);
    }
    sim.settled();
    sim.s.preset = preset;
    sim.world.preset_chosen(&sim.s, sim.now);
    sim.settled();
    let metres = |(w, h): (u32, u32)| {
        (
            w as f32 * layout::METRES_PER_PIXEL,
            h as f32 * layout::METRES_PER_PIXEL,
        )
    };
    let radius = layout::RADIUS;
    let y = if preset == Preset::Overhead {
        0.0
    } else {
        -0.08
    };
    let row = |sizes: [(u32, u32); 2], bottom: f32| {
        let a = sizes.map(|s| angular_width(metres(s).0, radius));
        let start = -(a[0] + a[1]) / 2.0;
        [start + a[0] / 2.0, start + a[0] + a[1] / 2.0]
            .into_iter()
            .zip(sizes)
            .map(|(yaw, s)| {
                let (w, h) = metres(s);
                (on_cylinder(yaw, bottom + h / 2.0, w, radius), h)
            })
            .collect::<Vec<_>>()
    };
    // The lower row's tops on one line, the primary's (none: the tallest) at `y`.
    let tallest = lower.map(|s| metres(s).1).into_iter().fold(0.0, f32::max);
    let low: Vec<(Placement, f32)> = row(lower, y - tallest / 2.0)
        .into_iter()
        .map(|(p, h)| {
            let top = y + tallest / 2.0;
            let mut p = p;
            p.position.y = top - h / 2.0;
            (p, h)
        })
        .collect();
    let shelf = y + tallest / 2.0;
    let high = row(upper, shelf);
    let bottom = clear_of(&high, &low).map_or(shelf, |c| c.max(shelf));
    let high = row(upper, bottom);
    let frame = layout::frame(sim.world.center, sim.world.tilt);
    for (n, (p, _)) in (1..).zip(low.into_iter().chain(high)) {
        sim.world.placements.insert(k(n), frame.place(p));
    }
    sim.world.save_layout();
    sim.settled();
}

/// The owner's report of 2026-11-25: two rows of two as the preset stacked them before,
/// the upper one standing on the lower one as seen, its middle out of view: four
/// 3440x1440 on an arc; three 3440x1440 and a 2560x1440 on an arc; three 3440x1440 and a
/// 1920x1080 overhead. "Line up" takes the whole arrangement a little further away:
/// every monitor in view, the same two rows, every seam closed as seen still closed,
/// nothing going through anything.
#[test]
fn line_up_brings_a_row_over_the_head_into_view() {
    const ULTRA: (u32, u32) = (3440, 1440);
    for (preset, top) in [
        (Preset::Arc, ULTRA),
        (Preset::Arc, QHD),
        (Preset::Overhead, (1920, 1080)),
    ] {
        let case = format!("{preset:?} with {top:?}");
        let mut sim = Sim::new(None);
        stacked_before(&mut sim, preset, [ULTRA; 2], [ULTRA, top]);
        let at = (sim.world.center, sim.world.tilt);
        assert!(
            !layout::in_view(&sim.place(k(3)), at),
            "{case}: the upper row out of view"
        );
        let rows = row_ids(sim.arranged.last().expect("told"));
        assert_eq!(rows, [vec![3, 4], vec![1, 2]], "{case}");
        let glued = sim.world.glued_now();
        assert!(!glued.is_empty(), "{case}: glued {glued:?}");
        let laid = sim.world.sizes.clone();
        let meeting = layout::meeting_pairs(&sim.world.placements, &laid, at, 1.5e-3);
        let before = layout::seam_gaps(&sim.world.placements, &laid, at, &meeting);
        sim.world.tidy_up(&sim.s, sim.now);
        sim.settled();
        all_in_view(&sim, &[1, 2, 3, 4], &case);
        let rows = row_ids(sim.arranged.last().expect("told"));
        assert_eq!(rows, [vec![3, 4], vec![1, 2]], "{case}: the same rows");
        assert!(sim.world.radius > layout::RADIUS, "{case}: further away");
        let after = layout::seam_gaps(&sim.world.placements, &laid, at, &meeting);
        for ((pair, was), now) in meeting.iter().zip(&before).zip(&after) {
            let (p, u) = (pair.0.id.0, pair.1.id.0);
            let (Some(was), Some(now)) = (was, now) else {
                panic!("{case}: {u} and {p} no longer one over the other");
            };
            assert!(
                now - was < 5e-4,
                "{case}: the seam of {u} and {p} opens: {was} to {now}"
            );
        }
    }
}

/// A 3840x2160 added to a 3440x1440, beside which it has no room: over it on its circle
/// it would stand out of view (the middle of a flat ultrawide comes close to the eye,
/// and a tall monitor resting on it stands high). It goes over it further away, in
/// view, resting on it as seen; its placeholder waits there too. Nothing cuts.
#[test]
fn a_monitor_added_over_a_row_is_in_view() {
    const ULTRA: (u32, u32) = (3440, 1440);
    let mut sim = Sim::new(None);
    sim.connect(PC);
    const UHD: (u32, u32) = (3840, 2160);
    let mut list = vec![monitor(1, ULTRA)];
    sim.list(list.clone());
    sim.stream(1, ULTRA);
    sim.settled();
    sim.world
        .adding_started(Some(ReqId(1)), UHD, sim.head.1, sim.now);
    let waiting = *sim.world.adding().expect("a placeholder");
    let at = (sim.world.center, sim.world.tilt);
    assert!(
        layout::in_view(&waiting.place, at),
        "the placeholder in view"
    );
    sim.world.adding_made(PC, ReqId(1), Some(k(2)));
    list.push(monitor(2, UHD));
    sim.list(list);
    sim.stream(2, UHD);
    sim.settled();
    all_in_view(&sim, &[1, 2], "added");
    let rows = row_ids(sim.arranged.last().expect("told"));
    assert_eq!(rows.len(), 2, "over the row: {rows:?}");
    assert!(
        layout::seam_gaps(
            &sim.world.placements,
            &sim.world.sizes,
            at,
            &sim.world.glued_now()
        )
        .iter()
        .all(|g| g.is_some_and(|g| g.abs() < 1e-3)),
        "resting on the row as seen"
    );
}

#[cfg(feature = "preview")]
use crate::clarity::{Filter, MonitorSize};

/// How many of the headset's pixels (25 a degree) a pixel of `n`'s stream takes at its
/// middle, where
/// it is placed, seen from the eye.
fn scale_of(sim: &Sim, n: u32) -> f32 {
    sim.world.shown_scale(k(n)).expect("worked out")
}

/// On a headset of 25 pixels a degree the first monitors are made 1:1: two QHD side by
/// side on the circle of
/// 1.1 m, each pixel one of the headset's at its middle, so larger than the standard
/// size; on a headset not in the table, the standard size as before.
#[test]
fn the_first_monitors_are_made_one_to_one() {
    let mut sim = Sim::on(None, Some(25.0));
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    for n in 1..=2 {
        sim.stream(n, QHD);
    }
    sim.settled();
    let mut standard = Sim::new(None);
    standard.connect(PC);
    standard.list(vec![monitor(1, QHD), monitor(2, QHD)]);
    for n in 1..=2 {
        standard.stream(n, QHD);
    }
    standard.settled();
    for n in 1..=2 {
        let (p, was) = (sim.place(k(n)), standard.place(k(n)));
        assert!(
            (scale_of(&sim, n) - 1.0).abs() < 0.01,
            "{}",
            scale_of(&sim, n)
        );
        assert!(p.width > was.width * 1.1, "{} {}", p.width, was.width);
        // The edges stay 1.1 m from the eye (around it): larger, not nearer.
        let edge = p.position + p.rotation * Vec3::X * p.width / 2.0;
        let around = Vec3::new(edge.x, 0.0, edge.z).length();
        assert!((around - layout::RADIUS).abs() < 1e-3, "{around}");
        assert_eq!(standard.world.shown_scale(k(n)), None, "not known");
        assert_eq!(standard.world.layer_quality(k(n)), Quality::BOTH);
        // Shown at 1:1: sharpened, not supersampled.
        assert_eq!(
            sim.world.layer_quality(k(n)),
            Quality {
                super_sampling: false,
                sharpening: true
            }
        );
    }
}

/// The owner's desk (two QHD and a QHD standing on end, the first the primary display;
/// with the laptop's own display under it and without) laid out by the preset on a
/// headset of 25 pixels a degree: no monitor smaller than the standard size shows it,
/// and without the lid the row as large as fits around the user (0.93 of the
/// headset's pixels, the standard size 0.77 and 0.67).
#[test]
fn the_preset_never_makes_a_monitor_smaller_than_the_standard_size() {
    let start = |ppd, lid: bool| {
        let mut sim = Sim::on(None, ppd);
        sim.connect(PC);
        let mut displays = vec![monitor(1, QHD), monitor(2, QHD), monitor(3, (1440, 2560))];
        if lid {
            displays.push(laptop(9));
        }
        let displays = Displays {
            displays,
            primary: Some(DisplayId(1)),
        };
        sim.world
            .listed(sim.pc, &displays, &sim.s, sim.head, sim.now);
        for n in 1..=2 {
            sim.stream(n, QHD);
        }
        sim.stream(3, (1440, 2560));
        if lid {
            sim.stream(9, (2880, 1800));
        }
        sim.settled();
        sim.world.preset_chosen(&sim.s, sim.now);
        sim.settled();
        sim
    };
    for lid in [false, true] {
        let (sim, standard) = (start(Some(25.0), lid), start(None, lid));
        for (n, w) in [(1, 2560), (2, 2560), (3, 1440)] {
            let was = clarity::shown(&standard.place(k(n)), w, Vec3::ZERO, None, 25.0);
            let now = scale_of(&sim, n);
            assert!(now >= was - 1e-3, "{lid} {n}: {now}, standard {was}");
            assert!(lid || (0.9..1.0).contains(&now), "{n}: {now}");
        }
    }
}

/// A monitor the person placed (here as `layout.txt` keeps it, at the standard size)
/// stays exactly where and as large as it was when the headset makes new ones 1:1: the
/// new one goes beside it, as large to the eye as it (the same scale), and nothing else
/// moves. Its layer keeps both filters: it is shown smaller than 0.9.
#[test]
fn a_monitor_placed_by_hand_is_left_as_it_is() {
    let mut was = Desk::default();
    let mut laid = layout::arrange(Preset::Arc, &[(k(1), 2560, 1440)], None, &[], None);
    let p = laid.get_mut(&k(1)).unwrap();
    p.position.x += 0.2;
    p.width *= 0.9;
    let sizes = laid.keys().map(|k| (*k, QHD)).collect();
    was.keep(
        laid.clone(),
        &sizes,
        ((Vec3::ZERO, 0.0), layout::RADIUS, 0.0),
        false,
    );
    let mut text = String::new();
    was.write_text(&mut text);
    let mut sim = Sim::on(Some(&text), Some(25.0));
    sim.connect(PC);
    sim.list(vec![monitor(1, QHD)]);
    sim.stream(1, QHD);
    sim.settled();
    assert!(near(&sim.place(k(1)), &laid[&k(1)]), "as it was kept");
    let kept = sim.place(k(1));
    let scale = scale_of(&sim, 1);
    assert!(scale < 0.9, "{scale}");
    assert_eq!(sim.world.layer_quality(k(1)), Quality::BOTH);
    // A new monitor of another size comes.
    sim.list(vec![monitor(1, QHD), monitor(2, (1920, 1080))]);
    sim.stream(2, (1920, 1080));
    sim.settled();
    assert_eq!(sim.place(k(1)), kept, "untouched");
    // New ones are made at the scale it is shown at (not the standard size: it was
    // made smaller by hand), as much smaller than their own size as it is than its own.
    let made = sim.world.sized.others;
    let layout::Size::Seen { scale: at, .. } = made else {
        panic!("{made:?}");
    };
    assert!((at - scale).abs() < 1e-4, "{at} {scale}");
    let own = |w: u32, h: u32| {
        made.metres(prostir_layout::space::PixelSize::new(w, h), 1.1)
            .0
    };
    let new = sim.place(k(2)).width / own(1920, 1080);
    let beside = kept.width / own(2560, 1440);
    assert!((new - beside).abs() < 1e-4, "{new} beside {beside}");
}

/// "For development": each size lays the monitors out again at once (1:1, the main one
/// only, 0.85, standard), and "Reset" puts them back where they were; the filters
/// follow the choices with the next frame.
#[cfg(feature = "preview")]
#[test]
fn the_sizes_for_development_and_reset() {
    let mut sim = Sim::on(None, Some(25.0));
    sim.connect(PC);
    sim.list(three());
    for n in 1..=3 {
        sim.stream(n, QHD);
    }
    sim.settled();
    let before = sim.places();
    assert!(!sim.world.can_reset_size());
    let dev = *sim.world.dev();
    let set = |sim: &mut Sim, size| {
        sim.world.set_dev(&sim.s, Dev { size, ..dev }, sim.now);
        sim.settled();
    };
    let made_at = |scale| {
        layout::Size::Seen { ppd: 25.0, scale }
            .metres(prostir_layout::space::PixelSize::new(QHD.0, QHD.1), 1.1)
            .0
    };
    set(&mut sim, MonitorSize::Scale85);
    for n in 1..=3 {
        assert!((sim.place(k(n)).width - made_at(0.85)).abs() < 1e-5);
    }
    set(&mut sim, MonitorSize::Standard);
    let standard: Vec<f32> = (1..=3).map(|n| sim.place(k(n)).width).collect();
    assert!(
        standard
            .iter()
            .all(|w| (w - 2560.0 * layout::METRES_PER_PIXEL).abs() < 1e-5)
    );
    set(&mut sim, MonitorSize::MainOnly);
    let main = sim.world.primary().map_or(2, |k| k.id.0);
    for n in 1..=3 {
        let w = sim.place(k(n)).width;
        if n == main {
            assert!(w > standard[0] && w <= made_at(1.0) + 1e-5, "{n}: {w}");
        } else {
            assert!((w - standard[0]).abs() < 1e-5, "{n}: {w}");
        }
    }
    assert!(sim.world.can_reset_size());
    sim.world.reset_size(sim.now);
    sim.settled();
    assert!(same_places(&sim.places(), &before), "back as it was");
    assert_eq!(sim.world.dev().size, MonitorSize::default());
    assert!(!sim.world.can_reset_size());
    // The filters: forced on and off over the rule.
    let forced = Dev {
        super_sampling: Filter::On,
        sharpening: Filter::Off,
        ..*sim.world.dev()
    };
    sim.world.set_dev(&sim.s, forced, sim.now);
    sim.after(20);
    assert_eq!(
        sim.world.layer_quality(k(1)),
        Quality {
            super_sampling: true,
            sharpening: false
        }
    );
}

/// A desk kept at the standard size (here by a build before 1:1) stays at it on a
/// headset that makes new monitors 1:1: the PC's own display shown is laid under it at
/// the standard size, and stays so when the curve changes (which lays it out again);
/// new monitors beside it get the standard size too.
#[test]
fn a_standard_desk_from_the_file_keeps_the_standard_size() {
    let mut was = Desk::default();
    let laid = layout::arrange(
        Preset::Arc,
        &[(k(1), 2560, 1440), (k(2), 2560, 1440)],
        None,
        &[],
        None,
    );
    let sizes = laid.keys().map(|k| (*k, QHD)).collect();
    was.keep(
        laid,
        &sizes,
        ((Vec3::ZERO, 0.0), layout::RADIUS, 0.0),
        false,
    );
    let mut text = String::new();
    was.write_text(&mut text);
    let run = |ppd| {
        let mut sim = Sim::on(Some(&text), ppd);
        sim.connect(PC);
        sim.list(vec![monitor(1, QHD), monitor(2, QHD), laptop(9)]);
        for n in 1..=2 {
            sim.stream(n, QHD);
        }
        sim.stream(9, (2880, 1800));
        sim.settled();
        assert_eq!(sim.world.sized, layout::Sizes::default(), "{ppd:?}");
        let lid = sim.place(k(9));
        sim.s.curve = crate::settings::Curve::Gentle;
        sim.world.curve_chosen(&sim.s);
        sim.settled();
        sim.s.curve = crate::settings::Curve::Flat;
        sim.world.curve_chosen(&sim.s);
        sim.settled();
        assert!(near(&sim.place(k(9)), &lid), "{ppd:?}: the lid as it was");
        assert_eq!(sim.world.sized, layout::Sizes::default());
        lid
    };
    let (standard, one) = (run(None), run(Some(25.0)));
    assert!(near(&standard, &one), "{standard:?} {one:?}");
}

/// Step 8.3: whatever a hand does to a monitor (anywhere, any size, a few hundred
/// random drags, the same every run), it stands within the bounds after (or, laid out
/// outside them by the app, no further out than it was), or where it was if the
/// bounds would have put it into another; a resize with both hands keeps it within
/// them where it stands.
#[test]
fn a_hand_keeps_monitors_within_the_bounds() {
    let mut sim = Sim::at_desk();
    let head = sim.head.0;
    let bounds = layout::Bounds::default();
    let aspect = 1440.0 / 2560.0;
    let mut seed: u64 = 11;
    let mut f = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 40) as f32) / (1u64 << 24) as f32
    };
    let mut kept = 0;
    for step in 0..300 {
        let id = k(1 + (f() * 3.0) as u32 % 3);
        let thing = Thing::Display(id);
        if sim.world.hold(thing, &sim.s).is_none() {
            continue;
        }
        let was = sim.place(id);
        let to = Vec3::new(f() * 16.0 - 8.0, f() * 8.0 - 4.0, f() * 16.0 - 8.0);
        let p = Placement {
            position: head + to,
            rotation: layout::facing(head + to, head),
            width: f() * 10.0,
        };
        sim.world.drag(&sim.s, thing, p, head);
        let now = sim.place(id);
        let loose = bounds.loosened_for(&was, aspect, head);
        kept += usize::from(now == was);
        assert!(
            now == was || layout::within(&now, aspect, head, None, &loose),
            "step {step}: {p:?} came to {now:?} from {was:?}"
        );
        if f() < 0.3 && sim.world.resize_start(id).is_some() {
            // The other hand on it too: wider than the bounds let it be.
            sim.world.resize(&sim.s, 20.0, head);
            let resized = sim.place(id);
            let loose = bounds.loosened_for(&now, aspect, head);
            assert!(layout::within(&resized, aspect, head, None, &loose));
            assert_eq!(resized.position, now.position, "where it stood");
            sim.world.resize_end();
        }
        sim.world.let_go(thing, &sim.s, sim.now);
        sim.after(20);
    }
    assert!(kept < 150, "mostly moved: {kept} kept");
}

/// A monitor carried in front of another (hiding it, as the hand put it) and pulled
/// nearer than the bounds let it be stops at the bound, rather than staying where it
/// was a frame ago: only trouble the bounds themselves would make keeps it there.
#[test]
fn carried_in_front_of_another_it_stops_at_the_bound() {
    let mut sim = Sim::at_desk();
    let head = sim.head.0;
    let thing = Thing::Display(k(2));
    sim.world.hold(thing, &sim.s).expect("taken");
    let one = sim.place(k(1));
    let toward = (one.position - head).normalize();
    let at = |d: f32| Placement {
        position: head + toward * d,
        rotation: layout::facing(head + toward * d, head),
        width: 0.6,
    };
    sim.world.drag(&sim.s, thing, at(0.7), head);
    let before = sim.place(k(2));
    assert!(
        before.position.distance(at(0.7).position) < 1e-4,
        "in front of 1"
    );
    sim.world.drag(&sim.s, thing, at(0.2), head);
    let d = sim.place(k(2)).position.distance(head);
    assert!((d - 0.35).abs() < 2e-3, "stopped at the bound: {d}");
}
