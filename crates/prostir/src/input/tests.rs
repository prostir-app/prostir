//! The input thread's way with devices: opened on the first event, everything a
//! session holds let go when it goes or falls silent, a refusal not asked again,
//! devices the desktop closed opened again after a pause.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::platform::OutputKey;
use prostir_proto::state::DeskRect;

/// What the fake devices were told, in order.
type Told = Arc<Mutex<Vec<String>>>;

struct Recorder {
    told: Told,
    touches: bool,
    /// Fails every call from now on.
    broken: Arc<std::sync::atomic::AtomicBool>,
}

impl Recorder {
    fn say(&self, what: String) -> anyhow::Result<()> {
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
    fn pointer_to(&mut self, at: &Spot) -> anyhow::Result<()> {
        self.say(format!("to {},{}", at.x, at.y))
    }
    fn pointer_by(&mut self, dx: f64, dy: f64) -> anyhow::Result<()> {
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
    refuse: bool,
    asked: AtomicU32,
    broken: Arc<std::sync::atomic::AtomicBool>,
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
            broken: self.broken.clone(),
        }))
    }
}

fn opener(touches: bool, refuse: bool) -> Arc<Opener> {
    Arc::new(Opener {
        told: Told::default(),
        touches,
        refuse,
        asked: AtomicU32::new(0),
        broken: Arc::default(),
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
        x,
        y: 10.0,
    }
}

/// A run with `opener`, its devices opened by a first move.
fn opened(opener: &Arc<Opener>) -> Run {
    let mut run = Run::new(Arc::new(Router::new()));
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
    let touch = |phase, x| Ev::Touch {
        from: 1,
        slot: 0,
        phase,
        at: Some(at(x)),
    };
    run.event(touch(TouchPhase::Down, 100.0), now);
    run.event(
        touch(TouchPhase::Up, 100.0),
        now + Duration::from_millis(80),
    );
    assert_eq!(told(&o), ["to 100,10", "Left down", "Left up"]);
    run.event(touch(TouchPhase::Down, 100.0), now);
    run.event(touch(TouchPhase::Move, 200.0), now + Duration::from_secs(1));
    assert_eq!(told(&o), ["to 100,10", "Left down", "to 200,10"]);
    run.event(Ev::Release { from: 1 }, now);
    assert_eq!(told(&o), ["Left up"]);
}

/// A person who said no is not asked again, and nothing reaches the desktop.
#[test]
fn a_refusal_is_not_asked_again() {
    let o = opener(true, true);
    let mut run = opened(&o);
    assert!(run.refused && run.injector.is_none());
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
