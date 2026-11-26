//! The input thread's way with devices: opened on the first event, everything a
//! session holds let go when it goes or falls silent, a refusal not asked again,
//! devices the desktop closed opened again after a pause.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;
use crate::platform::OutputKey;
use prostir_proto::state::DeskRect;

/// What the fake devices were told, in order.
type Told = Arc<Mutex<Vec<String>>>;

struct Recorder {
    told: Told,
    touches: bool,
    /// Takes relative moves as a mouse's (libei); else they fail, as uinput's do.
    relative: bool,
    /// Fails every call from now on.
    broken: Arc<std::sync::atomic::AtomicBool>,
    /// The person stopped it.
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Recorder {
    fn say(&self, what: String) -> anyhow::Result<()> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(InputStopped.into());
        }
        anyhow::ensure!(
            !self.broken.load(Ordering::Relaxed),
            "closed by the desktop"
        );
        self.told.lock().unwrap().push(what);
        Ok(())
    }
}

impl Injector for Recorder {
    fn touches(&self) -> bool {
        self.touches
    }
    fn relative(&self) -> bool {
        self.relative
    }
    fn pointer_to(&mut self, at: &Spot) -> anyhow::Result<()> {
        self.say(format!("to {},{}", at.x, at.y))
    }
    fn pointer_by(&mut self, dx: f64, dy: f64) -> anyhow::Result<()> {
        anyhow::ensure!(self.relative, "moves to points only");
        self.say(format!("by {dx},{dy}"))
    }
    fn button(&mut self, button: MouseButton, down: bool) -> anyhow::Result<()> {
        self.say(format!("{button:?} {}", if down { "down" } else { "up" }))
    }
    fn scroll(&mut self, dx: i32, dy: i32) -> anyhow::Result<()> {
        self.say(format!("scroll {dx},{dy}"))
    }
    fn touch(&mut self, id: u32, phase: TouchPhase, at: &Spot) -> anyhow::Result<()> {
        self.say(format!("touch {id} {phase:?} {},{}", at.x, at.y))
    }
    fn key(&mut self, code: u16, down: bool) -> anyhow::Result<()> {
        self.say(format!("key {code} {down}"))
    }
}

/// Opens a recorder, or refuses, and counts how often it was asked.
struct Opener {
    told: Told,
    touches: bool,
    relative: bool,
    refuse: bool,
    asked: AtomicU32,
    broken: Arc<std::sync::atomic::AtomicBool>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl OpenInput for Opener {
    fn open(&self) -> anyhow::Result<Box<dyn Injector>> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        if self.refuse {
            return Err(InputRefused.into());
        }
        Ok(Box::new(Recorder {
            told: self.told.clone(),
            touches: self.touches,
            relative: self.relative,
            broken: self.broken.clone(),
            stop: self.stop.clone(),
        }))
    }
}

fn opener(touches: bool, refuse: bool) -> Arc<Opener> {
    opener_with(touches, refuse, true)
}

fn opener_with(touches: bool, refuse: bool, relative: bool) -> Arc<Opener> {
    Arc::new(Opener {
        told: Told::default(),
        touches,
        relative,
        refuse,
        asked: AtomicU32::new(0),
        broken: Arc::default(),
        stop: Arc::default(),
    })
}

fn at(x: f64) -> Spot {
    let rect = DeskRect {
        x: 0,
        y: 0,
        w: 640,
        h: 360,
    };
    Spot {
        output: OutputKey::new("Fake-1"),
        rect,
        desk: rect,
        open: crate::platform::Sides::default(),
        x,
        y: 10.0,
    }
}

/// What a run shares, with a no of its own (not the process's).
fn own_shared() -> Arc<Shared> {
    Arc::new(Shared::new(Box::leak(Box::new(AtomicBool::new(false)))))
}

/// A run with `opener`, its devices opened by a first move.
fn opened(opener: &Arc<Opener>) -> Run {
    let mut run = Run::new(Box::new(|_| {}), own_shared());
    run.set_opener(Some(opener.clone()));
    let now = Instant::now();
    run.event(
        Ev::To {
            from: 1,
            at: at(1.0),
        },
        now,
    );
    wait_open(&mut run);
    run
}

fn wait_open(run: &mut Run) {
    for _ in 0..200 {
        run.tick(Instant::now());
        if run.opening.is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the devices did not open");
}

fn told(opener: &Opener) -> Vec<String> {
    std::mem::take(&mut *opener.told.lock().unwrap())
}

fn click(from: SessionId, down: bool) -> Ev {
    Ev::Button {
        from,
        at: Some(at(5.0)),
        button: MouseButton::Left,
        down,
    }
}

/// The devices open on the first event, which goes nowhere; then moves, clicks and
/// the wheel reach them in order.
#[test]
fn the_first_event_opens_the_devices() {
    let o = opener(true, false);
    let mut run = opened(&o);
    assert!(run.injector.is_some());
    assert_eq!(o.asked.load(Ordering::Relaxed), 1);
    assert!(
        told(&o).is_empty(),
        "the event that opened them went nowhere"
    );
    let now = Instant::now();
    run.event(click(1, true), now);
    run.event(click(1, false), now);
    run.event(
        Ev::Scroll {
            from: 1,
            dx: 0,
            dy: 120,
        },
        now,
    );
    assert_eq!(
        told(&o),
        ["to 5,10", "Left down", "to 5,10", "Left up", "scroll 0,120"]
    );
}

/// The mouse put on the last pixel of an open side is pushed on towards it, by moves
/// relative to where it is, as the time comes; one put elsewhere is not.
#[test]
fn the_mouse_at_an_open_edge_is_pushed_on() {
    let o = opener(true, false);
    let mut run = opened(&o);
    let now = Instant::now();
    run.event(
        Ev::To {
            from: 1,
            at: edge(639.0),
        },
        now,
    );
    run.tick(now);
    assert_eq!(told(&o), ["to 639,100"], "not at once");
    for i in 1..=6 {
        run.tick(now + edge::PUSH_EVERY * i);
    }
    assert_eq!(told(&o), ["by 4,0"; 4]);
    run.event(
        Ev::To {
            from: 1,
            at: edge(600.0),
        },
        now,
    );
    run.tick(now + edge::PUSH_EVERY * 2);
    assert_eq!(told(&o), ["to 600,100"]);
}

/// A point on the open right side of the output, away from its corners.
fn edge(x: f64) -> Spot {
    Spot {
        open: crate::platform::Sides {
            right: true,
            ..Default::default()
        },
        y: 100.0,
        ..at(x)
    }
}

/// Devices whose relative moves the desktop does not take as a mouse's (uinput's
/// fail, wlroots' virtual pointer): never pushed, and nothing is lost.
#[test]
fn devices_without_relative_moves_never_push() {
    let o = opener_with(true, false, false);
    let mut run = opened(&o);
    let now = Instant::now();
    run.event(
        Ev::To {
            from: 1,
            at: edge(639.0),
        },
        now,
    );
    assert_eq!(run.edge.next(), None);
    for i in 1..=6 {
        run.tick(now + edge::PUSH_EVERY * i);
    }
    assert_eq!(told(&o), ["to 639,100"]);
    assert!(run.injector.is_some(), "the devices stay");
    assert_eq!(o.asked.load(Ordering::Relaxed), 1);
}

/// The session let go (it ended, the headset taken off): its mouse at an edge is
/// pushed on no more; so too once everything is let go.
#[test]
fn letting_go_stops_the_pushes() {
    let o = opener(true, false);
    let mut run = opened(&o);
    let now = Instant::now();
    let to = Ev::To {
        from: 1,
        at: edge(639.0),
    };
    run.event(to.clone(), now);
    run.event(Ev::Release { from: 1 }, now);
    run.tick(now + edge::PUSH_EVERY * 2);
    assert_eq!(told(&o), ["to 639,100"]);
    run.event(to, now);
    run.release_everything();
    run.tick(now + edge::PUSH_EVERY * 2);
    assert_eq!(told(&o), ["to 639,100"]);
}

/// A session that ends, or falls silent for 2 s, while it holds a button and a
/// finger: both are let go, and nothing more after.
#[test]
fn what_a_session_holds_goes_with_it() {
    let o = opener(true, false);
    let mut run = opened(&o);
    let now = Instant::now();
    run.event(click(1, true), now);
    run.event(
        Ev::Touch {
            from: 1,
            slot: 0,
            phase: TouchPhase::Down,
            at: Some(at(7.0)),
            came: now,
        },
        now,
    );
    told(&o);
    run.event(Ev::Release { from: 1 }, now);
    assert_eq!(told(&o), ["touch 0 Cancel 7,10", "Left up"]);
    run.event(click(1, false), now);
    assert_eq!(told(&o), ["to 5,10"], "no second release");

    run.event(click(2, true), now);
    told(&o);
    run.tick(now + Duration::from_secs(1));
    assert!(told(&o).is_empty());
    run.tick(now + held::QUIET + Duration::from_millis(1));
    assert_eq!(told(&o), ["Left up"]);
}

/// Without touch on the desktop, a tap is a click, and a drag let go with its
/// session.
#[test]
fn a_desktop_without_touch_gets_mouse_actions() {
    let o = opener(false, false);
    let mut run = opened(&o);
    let now = Instant::now();
    let touch = |phase, x, came| Ev::Touch {
        from: 1,
        slot: 0,
        phase,
        at: Some(at(x)),
        came,
    };
    run.event(touch(TouchPhase::Down, 100.0, now), now);
    let soon = now + Duration::from_millis(80);
    run.event(touch(TouchPhase::Up, 100.0, soon), soon);
    assert_eq!(told(&o), ["to 100,10", "Left down", "Left up"]);
    run.event(touch(TouchPhase::Down, 100.0, now), now);
    let later = now + Duration::from_secs(1);
    run.event(touch(TouchPhase::Move, 200.0, later), later);
    assert_eq!(told(&o), ["to 100,10", "Left down", "to 200,10"]);
    run.event(Ev::Release { from: 1 }, now);
    assert_eq!(told(&o), ["Left up"]);
}

/// A person who said no is not asked again, and nothing reaches the desktop.
#[test]
fn a_refusal_is_not_asked_again() {
    let o = opener(true, true);
    let mut run = opened(&o);
    assert!(run.shared.refused() && run.injector.is_none());
    for _ in 0..3 {
        run.event(click(1, true), Instant::now());
        run.tick(Instant::now());
    }
    assert_eq!(o.asked.load(Ordering::Relaxed), 1);
    assert!(told(&o).is_empty());
}

/// Devices the desktop closed: what they held is gone with them, and they open again
/// on an event after the pause, not before; a button let go after that is not sent.
#[test]
fn devices_closed_open_again_after_a_pause() {
    let o = opener(true, false);
    let mut run = opened(&o);
    run.event(click(1, true), Instant::now());
    o.broken.store(true, Ordering::Relaxed);
    run.event(click(1, false), Instant::now());
    assert!(run.injector.is_none() && !run.held.any());
    o.broken.store(false, Ordering::Relaxed);
    run.event(click(1, true), Instant::now());
    run.tick(Instant::now());
    assert_eq!(o.asked.load(Ordering::Relaxed), 1, "not at once");
    run.event(click(1, true), Instant::now() + REOPEN_FIRST);
    wait_open(&mut run);
    assert_eq!(o.asked.load(Ordering::Relaxed), 2);
    told(&o);
    run.event(click(1, false), Instant::now());
    assert_eq!(
        told(&o),
        ["to 5,10"],
        "the press was lost with the old devices"
    );
}

/// Moves one after another go as the last; a click between them keeps its place.
#[test]
fn moves_in_a_row_go_as_the_last() {
    let to = |from, x| Msg::Ev(Ev::To { from, at: at(x) });
    let batch = vec![
        to(1, 1.0),
        to(1, 2.0),
        to(2, 9.0),
        to(1, 3.0),
        to(1, 4.0),
        Msg::Ev(click(1, true)),
        to(1, 5.0),
        to(1, 6.0),
    ];
    let left: Vec<String> = coalesce(batch)
        .into_iter()
        .map(|m| match m {
            Msg::Ev(Ev::To { from, at }) => format!("{from} {}", at.x),
            Msg::Ev(_) => "click".into(),
            Msg::Opener(_) => "opener".into(),
        })
        .collect();
    assert_eq!(left, ["1 2", "2 9", "1 4", "click", "1 6"]);
}

/// A person who stops devices that were open (the desktop's remote control ended):
/// as a no, not opened again, and said once.
#[test]
fn devices_the_person_stopped_are_not_opened_again() {
    let o = opener(true, false);
    let told = Arc::new(Mutex::new(Vec::new()));
    let mut run = {
        let told = told.clone();
        Run::new(
            Box::new(move |n| told.lock().unwrap().push(n)),
            own_shared(),
        )
    };
    run.set_opener(Some(o.clone()));
    run.event(
        Ev::To {
            from: 1,
            at: at(1.0),
        },
        Instant::now(),
    );
    wait_open(&mut run);
    o.stop.store(true, Ordering::Relaxed);
    run.event(click(1, true), Instant::now());
    assert!(run.injector.is_none() && run.shared.refused());
    assert_eq!(
        State::of(run.shared.state.load(Ordering::Relaxed)),
        State::Refused
    );
    o.stop.store(false, Ordering::Relaxed);
    for _ in 0..3 {
        run.event(click(1, true), Instant::now() + REOPEN_MAX);
        run.tick(Instant::now());
    }
    assert_eq!(o.asked.load(Ordering::Relaxed), 1);
    assert_eq!(*told.lock().unwrap(), [News::Refused]);
}

/// A pass that panics lets go of what is held (as far as it can), drops the devices
/// and keeps the thread going.
#[test]
fn a_pass_that_panics_lets_go_and_goes_on() {
    let o = opener(true, false);
    let mut run = opened(&o);
    run.event(click(1, true), Instant::now());
    told(&o);
    let pass = catch_unwind(AssertUnwindSafe(|| panic!("a bug")));
    assert!(pass.is_err());
    run.recover();
    assert!(run.injector.is_none() && !run.held.any());
    assert_eq!(told(&o), ["Left up"]);
}

/// A press on a display gone is dropped; a release there still lets go.
#[test]
fn a_press_nowhere_is_dropped_and_a_release_lets_go() {
    let o = opener(true, false);
    let mut run = opened(&o);
    let now = Instant::now();
    let nowhere = |down| Ev::Button {
        from: 1,
        at: None,
        button: MouseButton::Left,
        down,
    };
    run.event(nowhere(true), now);
    assert!(told(&o).is_empty());
    run.event(click(1, true), now);
    told(&o);
    run.event(nowhere(false), now);
    assert_eq!(told(&o), ["Left up"]);
}

/// A no outlives the displays it was said with: the next set (a new thread, a new
/// opener, as for the next session) does not ask again.
#[test]
fn a_no_holds_for_the_next_displays_too() {
    let o = opener(true, true);
    let mut first = opened(&o);
    assert!(first.shared.refused());
    let again = opener(true, false);
    let mut next = Run::new(Box::new(|_| {}), first.shared.clone());
    next.set_opener(Some(again.clone()));
    assert_eq!(
        State::of(next.shared.state.load(Ordering::Relaxed)),
        State::Refused
    );
    next.event(click(1, true), Instant::now());
    next.tick(Instant::now());
    assert_eq!(again.asked.load(Ordering::Relaxed), 0);
    first.tick(Instant::now());
}

/// Real touches hide the pointer from a finger down until the mouse moves, each
/// change said once: a second finger, a finger moved or one of two lifted change
/// nothing; the last one lifted keeps it hidden; a move, a press or the wheel shows
/// it. Devices gone mid-touch lift the fingers. A desktop without touch moves the
/// pointer as the mouse and never hides it.
#[test]
fn real_touches_hide_the_pointer_until_the_mouse_moves() {
    let said = Arc::new(Mutex::new(Vec::new()));
    let run_with = |o: &Arc<Opener>| {
        let mut run = opened(o);
        let said = said.clone();
        run.pointing = Box::new(move |p| said.lock().unwrap().push(p));
        run
    };
    let said_now = || std::mem::take(&mut *said.lock().unwrap());
    let now = Instant::now();
    let touch = |slot, phase| Ev::Touch {
        from: 1,
        slot,
        phase,
        at: Some(at(50.0)),
        came: now,
    };
    let o = opener(true, false);
    let mut run = run_with(&o);
    run.event(touch(0, TouchPhase::Down), now);
    run.event(touch(1, TouchPhase::Down), now);
    run.event(touch(0, TouchPhase::Move), now);
    run.event(touch(0, TouchPhase::Up), now);
    assert_eq!(said_now(), [Pointing::Touch]);
    run.event(touch(1, TouchPhase::Cancel), now);
    assert_eq!(said_now(), [Pointing::Lifted]);
    run.event(click(1, false), now);
    assert_eq!(said_now(), [Pointing::Mouse], "moved there first");
    run.event(
        Ev::Scroll {
            from: 1,
            dx: 0,
            dy: 120,
        },
        now,
    );
    assert!(said_now().is_empty(), "shown already");
    run.event(touch(0, TouchPhase::Down), now);
    o.broken.store(true, Ordering::Relaxed);
    run.event(touch(0, TouchPhase::Move), now);
    assert_eq!(said_now(), [Pointing::Touch, Pointing::Lifted], "gone");

    let o = opener(false, false);
    let mut run = run_with(&o);
    run.event(touch(0, TouchPhase::Down), now);
    run.event(touch(0, TouchPhase::Up), now + Duration::from_millis(80));
    assert_eq!(told(&o), ["to 50,10", "Left down", "Left up"]);
    assert_eq!(said_now(), [Pointing::Mouse]);
}

/// A finger lifted on a display gone meanwhile (no point for it): on a desktop with
/// touch it is cancelled where it last was, so nothing stays down and the pointer no
/// longer hides for it; one never down tells nothing.
#[test]
fn a_finger_lifted_on_a_display_gone_is_cancelled_where_it_was() {
    let said = Arc::new(Mutex::new(Vec::new()));
    let o = opener(true, false);
    let mut run = opened(&o);
    {
        let said = said.clone();
        run.pointing = Box::new(move |p| said.lock().unwrap().push(p));
    }
    let now = Instant::now();
    let touch = |phase, at| Ev::Touch {
        from: 1,
        slot: 0,
        phase,
        at,
        came: now,
    };
    run.event(touch(TouchPhase::Down, Some(at(50.0))), now);
    run.event(touch(TouchPhase::Up, None), now);
    assert_eq!(told(&o), ["touch 0 Down 50,10", "touch 0 Cancel 50,10"]);
    assert_eq!(*said.lock().unwrap(), [Pointing::Touch, Pointing::Lifted]);
    assert!(!run.held.any());
    run.event(touch(TouchPhase::Up, None), now);
    assert!(told(&o).is_empty());
}
