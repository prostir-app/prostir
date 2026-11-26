//! A finger made into mouse actions, for a desktop without touch: a tap is a click,
//! moving the finger scrolls (and the scroll goes on a little after it lifts), holding
//! it still for 0.6 s is a right click, and holding it, then moving, drags with the
//! left button. Only the first finger acts; the others wait for gestures of their own.
//!
//! The point the finger touched is where the mouse goes; scrolling moves the page
//! with the finger, as on a phone.

use std::time::{Duration, Instant};

use super::held::Act;
use crate::platform::{MouseButton, Spot, TouchPhase};

/// The numbers of how it feels, in one place.
#[derive(Debug, Clone, Copy)]
pub struct Feel {
    /// A finger moved less than this (logical pixels) has not moved.
    pub slop: f64,
    /// Held this long without moving: a right click, or a drag once it moves.
    pub hold: Duration,
    /// How far the finger goes for one notch of the wheel, in logical pixels.
    pub per_notch: f64,
    /// How quickly the scroll after the finger lifts slows down: its speed falls to
    /// about a third in this time.
    pub coast: Duration,
    /// Slower than this (logical pixels a second), the scroll stops.
    pub stop_below: f64,
    /// A finger that rested this long before it lifted does not throw the page.
    pub rested: Duration,
}

pub const FEEL: Feel = Feel {
    slop: 10.0,
    hold: Duration::from_millis(600),
    per_notch: 50.0,
    coast: Duration::from_millis(350),
    stop_below: 40.0,
    rested: Duration::from_millis(100),
};

#[derive(Debug, Clone)]
enum State {
    Idle,
    /// Down and not moved yet.
    Pressed {
        slot: u8,
        start: Spot,
        since: Instant,
    },
    /// Moving: the page moves with it, at this speed (pixels a second, each way).
    Scrolling {
        slot: u8,
        last: (f64, f64),
        at: Instant,
        speed: (f64, f64),
    },
    Dragging {
        slot: u8,
    },
    /// Lifted while the page moved: it goes on, slowing down.
    Coasting {
        speed: (f64, f64),
        at: Instant,
    },
}

/// One session's finger as a mouse.
#[derive(Debug)]
pub struct TouchMouse {
    feel: Feel,
    state: State,
    /// Parts of a notch not sent yet, each way.
    rest: (f64, f64),
}

impl TouchMouse {
    pub fn new(feel: Feel) -> Self {
        Self {
            feel,
            state: State::Idle,
            rest: (0.0, 0.0),
        }
    }

    /// The finger in `slot` did `phase` at `at`, at `now`: what the mouse does.
    pub fn touch(&mut self, slot: u8, phase: TouchPhase, at: &Spot, now: Instant) -> Vec<Act> {
        let mine = match &self.state {
            State::Idle | State::Coasting { .. } => None,
            State::Pressed { slot, .. }
            | State::Scrolling { slot, .. }
            | State::Dragging { slot } => Some(*slot),
        };
        match (phase, mine) {
            (TouchPhase::Down, None) => {
                self.state = State::Pressed {
                    slot,
                    start: at.clone(),
                    since: now,
                };
                self.rest = (0.0, 0.0);
                vec![Act::To(at.clone())]
            }
            (_, Some(s)) if s == slot => self.of_mine(phase, at, now),
            // Another finger, or one this never saw down.
            _ => Vec::new(),
        }
    }

    fn of_mine(&mut self, phase: TouchPhase, at: &Spot, now: Instant) -> Vec<Act> {
        let feel = self.feel;
        match (phase, self.state.clone()) {
            (TouchPhase::Down | TouchPhase::Move, State::Pressed { slot, start, since }) => {
                if (at.x - start.x).hypot(at.y - start.y) < feel.slop {
                    return Vec::new();
                }
                if now.duration_since(since) >= feel.hold {
                    self.state = State::Dragging { slot };
                    vec![Act::Button(MouseButton::Left, true), Act::To(at.clone())]
                } else {
                    self.state = State::Scrolling {
                        slot,
                        last: (start.x, start.y),
                        at: since,
                        speed: (0.0, 0.0),
                    };
                    self.scroll_to(at, now)
                }
            }
            (TouchPhase::Down | TouchPhase::Move, State::Scrolling { .. }) => {
                self.scroll_to(at, now)
            }
            (TouchPhase::Down | TouchPhase::Move, State::Dragging { .. }) => {
                vec![Act::To(at.clone())]
            }
            (TouchPhase::Up, State::Pressed { since, .. }) => {
                self.state = State::Idle;
                let button = if now.duration_since(since) >= feel.hold {
                    MouseButton::Right
                } else {
                    MouseButton::Left
                };
                vec![Act::Button(button, true), Act::Button(button, false)]
            }
            (
                TouchPhase::Up,
                State::Scrolling {
                    at: last, speed, ..
                },
            ) => {
                let acts = self.scroll_to(at, now);
                let fast = speed.0.hypot(speed.1) >= feel.stop_below;
                self.state = if fast && now.duration_since(last) < feel.rested {
                    State::Coasting { speed, at: now }
                } else {
                    State::Idle
                };
                acts
            }
            (TouchPhase::Up | TouchPhase::Cancel, State::Dragging { .. }) => {
                self.state = State::Idle;
                vec![Act::Button(MouseButton::Left, false)]
            }
            _ => {
                self.state = State::Idle;
                Vec::new()
            }
        }
    }

    /// The page moved with the finger to `at`.
    fn scroll_to(&mut self, at: &Spot, now: Instant) -> Vec<Act> {
        let State::Scrolling {
            last,
            at: then,
            speed,
            ..
        } = &mut self.state
        else {
            return Vec::new();
        };
        let moved = (at.x - last.0, at.y - last.1);
        let dt = now.duration_since(*then).as_secs_f64();
        if dt > 0.0 {
            let fresh = (moved.0 / dt, moved.1 / dt);
            *speed = (0.6 * fresh.0 + 0.4 * speed.0, 0.6 * fresh.1 + 0.4 * speed.1);
        }
        *last = (at.x, at.y);
        *then = now;
        self.wheel(moved)
    }

    /// The wheel for the page moved by `moved` pixels: the other way round (a finger
    /// going up scrolls down).
    fn wheel(&mut self, moved: (f64, f64)) -> Vec<Act> {
        let notch = f64::from(prostir_proto::input::SCROLL_NOTCH);
        self.rest.0 -= moved.0 / self.feel.per_notch * notch;
        self.rest.1 -= moved.1 / self.feel.per_notch * notch;
        let whole = (self.rest.0.trunc(), self.rest.1.trunc());
        self.rest = (self.rest.0 - whole.0, self.rest.1 - whole.1);
        if whole == (0.0, 0.0) {
            return Vec::new();
        }
        vec![Act::Scroll(whole.0 as i32, whole.1 as i32)]
    }

    /// Whether it wants [`TouchMouse::tick`] soon: the page still moves.
    pub fn moving(&self) -> bool {
        matches!(self.state, State::Coasting { .. })
    }

    /// Time passed: the page goes on moving after the finger lifted, slowing down.
    pub fn tick(&mut self, now: Instant) -> Vec<Act> {
        let State::Coasting { speed, at } = self.state else {
            return Vec::new();
        };
        let dt = now.duration_since(at).as_secs_f64();
        let fall = (-dt / self.feel.coast.as_secs_f64()).exp();
        let slower = (speed.0 * fall, speed.1 * fall);
        // The distance gone meanwhile, at the speed falling all the way.
        let gone = self.feel.coast.as_secs_f64() * (1.0 - fall);
        let acts = self.wheel((speed.0 * gone, speed.1 * gone));
        self.state = if slower.0.hypot(slower.1) < self.feel.stop_below {
            State::Idle
        } else {
            State::Coasting {
                speed: slower,
                at: now,
            }
        };
        acts
    }

    /// The finger is gone with everything else it did (its session's end): it stops,
    /// and lets go of a drag.
    pub fn cancel(&mut self) -> Vec<Act> {
        let dragging = matches!(self.state, State::Dragging { .. });
        self.state = State::Idle;
        if dragging {
            vec![Act::Button(MouseButton::Left, false)]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::OutputKey;
    use prostir_proto::state::DeskRect;

    fn at(x: f64, y: f64) -> Spot {
        let rect = DeskRect {
            x: 0,
            y: 0,
            w: 1000,
            h: 1000,
        };
        Spot {
            output: OutputKey::new("Fake-1"),
            rect,
            desk: rect,
            x,
            y,
        }
    }

    fn ms(start: Instant, n: u64) -> Instant {
        start + Duration::from_millis(n)
    }

    const LEFT: MouseButton = MouseButton::Left;

    /// A tap is a left click where the finger touched; a shake within the slop is
    /// still a tap.
    #[test]
    fn a_tap_is_a_click() {
        let t = Instant::now();
        let mut m = TouchMouse::new(FEEL);
        assert_eq!(
            m.touch(0, TouchPhase::Down, &at(100.0, 100.0), t),
            vec![Act::To(at(100.0, 100.0))]
        );
        assert!(
            m.touch(0, TouchPhase::Move, &at(104.0, 103.0), ms(t, 50))
                .is_empty()
        );
        assert_eq!(
            m.touch(0, TouchPhase::Up, &at(104.0, 103.0), ms(t, 120)),
            vec![Act::Button(LEFT, true), Act::Button(LEFT, false)]
        );
    }

    /// Held still for 0.6 s and lifted: a right click.
    #[test]
    fn holding_still_is_a_right_click() {
        let t = Instant::now();
        let mut m = TouchMouse::new(FEEL);
        m.touch(0, TouchPhase::Down, &at(10.0, 10.0), t);
        let right = MouseButton::Right;
        assert_eq!(
            m.touch(0, TouchPhase::Up, &at(10.0, 10.0), ms(t, 700)),
            vec![Act::Button(right, true), Act::Button(right, false)]
        );
    }

    /// Held, then moved: a drag from where it touched with the left button, let go
    /// when it lifts (or is cancelled).
    #[test]
    fn holding_then_moving_drags() {
        let t = Instant::now();
        let mut m = TouchMouse::new(FEEL);
        m.touch(0, TouchPhase::Down, &at(10.0, 10.0), t);
        assert_eq!(
            m.touch(0, TouchPhase::Move, &at(60.0, 10.0), ms(t, 700)),
            vec![Act::Button(LEFT, true), Act::To(at(60.0, 10.0))]
        );
        assert_eq!(
            m.touch(0, TouchPhase::Move, &at(90.0, 10.0), ms(t, 750)),
            vec![Act::To(at(90.0, 10.0))]
        );
        assert_eq!(
            m.touch(0, TouchPhase::Up, &at(90.0, 10.0), ms(t, 800)),
            vec![Act::Button(LEFT, false)]
        );
        m.touch(0, TouchPhase::Down, &at(10.0, 10.0), ms(t, 1000));
        m.touch(0, TouchPhase::Move, &at(60.0, 10.0), ms(t, 1700));
        assert_eq!(m.cancel(), vec![Act::Button(LEFT, false)]);
        assert_eq!(m.cancel(), Vec::new());
    }

    /// Moved at once: the page follows the finger, a notch for each 50 pixels (parts of
    /// one kept), up the finger, down the page; no click.
    #[test]
    fn moving_scrolls_with_the_finger() {
        let t = Instant::now();
        let mut m = TouchMouse::new(FEEL);
        m.touch(0, TouchPhase::Down, &at(500.0, 500.0), t);
        // 25 pixels up: half a notch down.
        assert_eq!(
            m.touch(0, TouchPhase::Move, &at(500.0, 475.0), ms(t, 100)),
            vec![Act::Scroll(0, 60)]
        );
        // 30 more, and 10 to the left: the page goes right.
        assert_eq!(
            m.touch(0, TouchPhase::Move, &at(490.0, 445.0), ms(t, 200)),
            vec![Act::Scroll(24, 72)]
        );
        // Rested before it lifted: the page stops with it.
        assert!(
            m.touch(0, TouchPhase::Up, &at(490.0, 445.0), ms(t, 500))
                .is_empty()
        );
        assert!(!m.moving());
    }

    /// Thrown: the page goes on the same way, slowing down, and stops by itself; a new
    /// touch stops it at once.
    #[test]
    fn a_thrown_page_coasts_and_stops() {
        let t = Instant::now();
        let mut m = TouchMouse::new(FEEL);
        m.touch(0, TouchPhase::Down, &at(500.0, 900.0), t);
        for i in 1u32..=5 {
            m.touch(
                0,
                TouchPhase::Move,
                &at(500.0, 900.0 - 40.0 * f64::from(i)),
                ms(t, 20 * u64::from(i)),
            );
        }
        m.touch(0, TouchPhase::Up, &at(500.0, 700.0), ms(t, 110));
        assert!(m.moving());
        let mut total = 0;
        let mut now = ms(t, 110);
        for _ in 0..200 {
            now += Duration::from_millis(16);
            for act in m.tick(now) {
                let Act::Scroll(dx, dy) = act else {
                    panic!("{act:?}")
                };
                assert_eq!(dx, 0);
                assert!(dy > 0, "down, as the finger threw it");
                total += dy;
            }
            if !m.moving() {
                break;
            }
        }
        assert!(!m.moving(), "it stops by itself");
        assert!(total > 120, "it went on for more than a notch: {total}");
        m.touch(0, TouchPhase::Down, &at(500.0, 900.0), t);
        m.touch(0, TouchPhase::Move, &at(500.0, 700.0), ms(t, 20));
        m.touch(0, TouchPhase::Up, &at(500.0, 650.0), ms(t, 30));
        assert!(m.moving());
        m.touch(0, TouchPhase::Down, &at(5.0, 5.0), ms(t, 40));
        assert!(!m.moving());
    }

    /// A second finger does nothing while the first acts, nor one never seen down.
    #[test]
    fn only_the_first_finger_acts() {
        let t = Instant::now();
        let mut m = TouchMouse::new(FEEL);
        assert!(m.touch(1, TouchPhase::Up, &at(1.0, 1.0), t).is_empty());
        m.touch(0, TouchPhase::Down, &at(10.0, 10.0), t);
        assert!(
            m.touch(1, TouchPhase::Down, &at(300.0, 300.0), t)
                .is_empty()
        );
        assert!(m.touch(1, TouchPhase::Up, &at(300.0, 300.0), t).is_empty());
        assert_eq!(
            m.touch(0, TouchPhase::Up, &at(10.0, 10.0), ms(t, 50)),
            vec![Act::Button(LEFT, true), Act::Button(LEFT, false)]
        );
    }
}
