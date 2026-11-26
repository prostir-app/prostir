//! The mouse held against a screen edge: pushed on, as a real mouse is.
//!
//! A desktop's screen edges (KWin's: an auto-hiding panel, a corner's action) take
//! effect only while the mouse keeps pushing against them: KWin pushes the pointer
//! back a pixel the first time it comes to the edge's last pixel, and acts when it
//! comes there again after a moment (`ElectricBorderDelay`, 75 ms) and before the
//! attempt lapses (`ElectricBorderCooldown`, 350 ms). The headset puts the mouse there
//! once and says nothing more while its ray stays: so the mouse put on the last pixel
//! of an open side of its output (no other output against it, as KWin finds its edges)
//! is pushed on towards it a few times, by a small move relative to where it is.

use std::time::{Duration, Instant};

use crate::platform::Spot;

/// How often the mouse held at an edge is pushed on, and how many times.
pub const PUSH_EVERY: Duration = Duration::from_millis(100);
const PUSHES: u8 = 4;
/// How far each push goes, in logical pixels (the desktop keeps the mouse on its
/// output; KWin pushes it back 1 by default).
const PUSH: f64 = 4.0;

/// The pushes due for the mouse put at an edge (see the module).
#[derive(Debug, Default)]
pub struct EdgePush {
    /// Towards where, the next push and how many are left.
    pending: Option<((f64, f64), Instant, u8)>,
}

impl EdgePush {
    /// The mouse was put at `at` at `now`: pushed on from there if that is an edge.
    pub fn moved(&mut self, at: &Spot, now: Instant) {
        self.pending = towards(at).map(|d| (d, now + PUSH_EVERY, PUSHES));
    }

    /// Something else moved it (a finger): no pushes.
    pub fn stop(&mut self) {
        self.pending = None;
    }

    /// When the next push is due.
    pub fn next(&self) -> Option<Instant> {
        self.pending.map(|(_, at, _)| at)
    }

    /// The push due at `now`, as a move by `dx`, `dy`.
    pub fn due(&mut self, now: Instant) -> Option<(f64, f64)> {
        let (d, at, left) = self.pending?;
        if now < at {
            return None;
        }
        self.pending = (left > 1).then(|| (d, now + PUSH_EVERY, left - 1));
        Some((d.0 * PUSH, d.1 * PUSH))
    }
}

/// Which way `at` is against an open side of its output, each way -1, 0 or 1: on the
/// side's last pixel (where the desktop rounds the point to), none where it is not.
fn towards(at: &Spot) -> Option<(f64, f64)> {
    let r = at.rect;
    let way = |p: f64, start: i32, len: u32, open: (bool, bool)| {
        let px = p.round() as i64;
        let (start, end) = (i64::from(start), i64::from(start) + i64::from(len) - 1);
        if open.0 && px <= start {
            -1.0
        } else if open.1 && px >= end {
            1.0
        } else {
            0.0
        }
    };
    let o = at.open;
    let d = (
        way(at.x, r.x, r.w, (o.left, o.right)),
        way(at.y, r.y, r.h, (o.top, o.bottom)),
    );
    (d != (0.0, 0.0)).then_some(d)
}

#[cfg(test)]
mod tests {
    use prostir_proto::state::DeskRect;

    use super::*;
    use crate::platform::{OutputKey, Sides};

    fn spot(x: f64, y: f64, open: Sides) -> Spot {
        let rect = DeskRect {
            x: 2320,
            y: 2560,
            w: 1920,
            h: 1080,
        };
        Spot {
            output: OutputKey::new("HDMI-A-1"),
            rect,
            desk: rect,
            open,
            x,
            y,
        }
    }

    const BOTTOM: Sides = Sides {
        left: false,
        right: false,
        top: false,
        bottom: true,
    };

    /// On the last row of an open bottom side: pushed down 4 times, 100 ms apart, then
    /// no more; a move off the edge stops them.
    #[test]
    fn the_mouse_held_at_an_open_edge_is_pushed_on() {
        let t0 = Instant::now();
        let mut push = EdgePush::default();
        push.moved(&spot(3000.0, 3639.0, BOTTOM), t0);
        assert_eq!(push.due(t0), None, "not at once");
        let mut pushes = Vec::new();
        for i in 1..10 {
            pushes.extend(push.due(t0 + PUSH_EVERY * i));
        }
        assert_eq!(pushes, [(0.0, 4.0); 4]);
        assert_eq!(push.next(), None);
        // Rounded onto the last row (as KWin takes it): an edge; a row above: not.
        push.moved(&spot(3000.0, 3638.6, BOTTOM), t0);
        assert!(push.next().is_some());
        push.moved(&spot(3000.0, 3638.0, BOTTOM), t0);
        assert_eq!(push.next(), None);
    }

    /// The sides KWin has its edges on: not where another output is against it along
    /// any length. A desktop of three Prostir monitors with HDMI-A-1 below the middle.
    #[test]
    fn open_sides_are_those_no_output_is_against() {
        let r = |x, y, w, h| DeskRect { x, y, w, h };
        let (left, middle, right) = (
            r(0, 560, 2560, 1440),
            r(2560, 0, 1440, 2560),
            r(4000, 560, 2560, 1440),
        );
        let hdmi = r(2320, 2560, 1920, 1080);
        let all = [left, middle, right, hdmi];
        assert_eq!(
            Sides::open(hdmi, all),
            Sides {
                left: true,
                right: true,
                top: false,
                bottom: true,
            }
        );
        assert_eq!(
            Sides::open(middle, all),
            Sides {
                left: false,
                right: false,
                top: true,
                bottom: false,
            }
        );
        assert_eq!(Sides::open(left, [left]), Sides::open(left, []));
        assert!(
            Sides::open(left, all).bottom,
            "HDMI-A-1 is below, not against it"
        );
    }

    /// Only an open side: not one another output is against; a corner both ways.
    #[test]
    fn only_open_sides_are_pushed_against() {
        let t0 = Instant::now();
        let mut push = EdgePush::default();
        push.moved(&spot(2320.0, 2560.0, BOTTOM), t0);
        assert_eq!(push.next(), None, "the top left corner is not open");
        let all = Sides {
            left: true,
            right: true,
            top: true,
            bottom: true,
        };
        push.moved(&spot(2320.0, 2560.0, all), t0);
        assert_eq!(push.due(t0 + PUSH_EVERY), Some((-4.0, -4.0)));
        push.moved(&spot(4239.0, 3639.0, all), t0);
        assert_eq!(push.due(t0 + PUSH_EVERY), Some((4.0, 4.0)));
        push.stop();
        assert_eq!(push.due(t0 + PUSH_EVERY), None);
    }
}
