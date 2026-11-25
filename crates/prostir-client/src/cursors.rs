//! The PCs' mouse pointers, drawn by the headset itself (see `PROTOCOL.md`, "The mouse
//! pointer"): the looks each PC sent and where its pointer is now, for the frame's
//! layers (`layers`), which put it over its monitor.
//!
//! Each PC's looks are kept as the PC counts them: the [`CURSOR_SHAPES`] that came
//! last, the first of them going when one more comes. Where the pointer is comes from
//! the PC (`CursorAt`), except right after the headset moved the mouse itself
//! (`MouseTo`): then the pointer is drawn where the headset sent it, until the PC's
//! word catches up with that or [`OWN_FOR`] passed since.
//!
//! Nothing is allocated for a place or a look already kept, so a frame takes no memory
//! however the pointer moves.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use glam::Vec2;
use prostir_proto::CURSOR_SHAPES;
use prostir_proto::state::DisplayId;

use crate::ids::{DisplayKey, PcId};

/// After the headset moved the mouse itself, how long the pointer is drawn where it
/// sent it, if the PC's word does not catch up before.
pub const OWN_FOR: Duration = Duration::from_millis(200);
/// How near the PC's word has to come to where the headset sent the mouse to count as
/// caught up, across the picture (1 %).
const CAUGHT_UP: u16 = 655;

/// A look of a pointer: its picture, `w`x`h` pixels of RGBA in sRGB (not
/// premultiplied), top row first, pointing at `hot_x`, `hot_y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    pub w: u16,
    pub h: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

/// Where a pointer is, across the picture of one of its PC's displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    display: DisplayId,
    u: u16,
    v: u16,
}

/// One PC's pointer.
#[derive(Debug)]
struct PcPointer {
    pc: PcId,
    /// Its looks by number, the first to come first.
    looks: VecDeque<(u32, Look)>,
    /// Where the PC said it is, and how it looks there (0: hidden).
    at: Option<Place>,
    shape: u32,
    /// The last look it showed, for where the headset sends the mouse itself.
    last_shape: u32,
    /// Where the headset sent the mouse itself, and when.
    own: Option<(Place, Instant)>,
}

impl PcPointer {
    fn look(&self, shape: u32) -> Option<&Look> {
        self.looks
            .iter()
            .find_map(|(n, look)| (*n == shape).then_some(look))
    }
}

/// A pointer to draw: over monitor `id`, its point at `uv` across the monitor's
/// picture (0..=1 from the top left), looking as look `shape` of its PC.
#[derive(Debug, Clone, Copy)]
pub struct Shown<'a> {
    pub id: DisplayKey,
    pub uv: Vec2,
    pub shape: u32,
    pub look: &'a Look,
}

/// Every PC's pointer (see the module).
#[derive(Debug, Default)]
pub struct Cursors {
    pcs: Vec<PcPointer>,
}

impl Cursors {
    fn pc_mut(&mut self, pc: PcId) -> &mut PcPointer {
        let i = match self.pcs.iter().position(|p| p.pc == pc) {
            Some(i) => i,
            None => {
                self.pcs.push(PcPointer {
                    pc,
                    looks: VecDeque::with_capacity(CURSOR_SHAPES),
                    at: None,
                    shape: 0,
                    last_shape: 0,
                    own: None,
                });
                self.pcs.len() - 1
            }
        };
        &mut self.pcs[i]
    }

    /// PC `pc` sent look `shape` (checked by the protocol: numbered, its picture whole).
    pub fn take_look(&mut self, pc: PcId, shape: u32, look: Look) {
        let p = self.pc_mut(pc);
        // Sent again: it counts as new, as the PC counts it.
        p.looks.retain(|(n, _)| *n != shape);
        if p.looks.len() >= CURSOR_SHAPES {
            p.looks.pop_front();
        }
        p.looks.push_back((shape, look));
    }

    /// PC `pc` says its pointer is on `display` at `u`, `v` (none: on none of the
    /// displays it streams here), looking as `shape` (0: hidden). Where the headset
    /// sent the mouse itself, the PC's word caught up once it is there.
    pub fn take_at(&mut self, pc: PcId, display: Option<DisplayId>, u: u16, v: u16, shape: u32) {
        let p = self.pc_mut(pc);
        p.at = display.map(|display| Place { display, u, v });
        p.shape = shape;
        if shape != 0 {
            p.last_shape = shape;
        }
        if let (Some((own, _)), Some(at)) = (p.own, p.at)
            && own.display == at.display
            && own.u.abs_diff(at.u) <= CAUGHT_UP
            && own.v.abs_diff(at.v) <= CAUGHT_UP
        {
            p.own = None;
        }
    }

    /// The headset moved PC `pc`'s mouse itself (`MouseTo`), to `u`, `v` across
    /// `display`, at `now`: its pointer is drawn there for now.
    pub fn moved_here(&mut self, pc: PcId, display: DisplayId, u: u16, v: u16, now: Instant) {
        let p = self.pc_mut(pc);
        // A PC that never said its pointer draws none: its pictures have it.
        if p.last_shape == 0 {
            return;
        }
        p.own = Some((Place { display, u, v }, now));
    }

    /// PC `pc`'s session ended: its looks and pointer go with it.
    pub fn forget(&mut self, pc: PcId) {
        self.pcs.retain(|p| p.pc != pc);
    }

    /// Whether look `shape` of PC `pc` is kept (the shell keeps a picture of it as long).
    pub fn has(&self, pc: PcId, shape: u32) -> bool {
        self.pcs
            .iter()
            .any(|p| p.pc == pc && p.looks.iter().any(|(n, _)| *n == shape))
    }

    /// PC `pc`'s pointer at `now`, where it is to be drawn: over which monitor, where
    /// on it, how it looks. None when hidden, off its displays, or of a look not come.
    pub fn of(&self, pc: PcId, now: Instant) -> Option<Shown<'_>> {
        let p = self.pcs.iter().find(|p| p.pc == pc)?;
        let (place, shape) = match p.own {
            Some((own, since)) if now.saturating_duration_since(since) < OWN_FOR => {
                (own, p.last_shape)
            }
            _ => (p.at?, p.shape),
        };
        let look = p.look(shape)?;
        let f = |n: u16| f32::from(n) / 65535.0;
        Some(Shown {
            id: DisplayKey::new(pc, place.display),
            uv: Vec2::new(f(place.u), f(place.v)),
            shape,
            look,
        })
    }

    /// The pointer over monitor `id` at `now`, if its PC's is there.
    pub fn on(&self, id: DisplayKey, now: Instant) -> Option<Shown<'_>> {
        self.of(id.pc, now).filter(|s| s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: PcId = PcId([1; 8]);

    fn look(fill: u8) -> Look {
        Look {
            w: 2,
            h: 2,
            hot_x: 0,
            hot_y: 1,
            rgba: vec![fill; 16],
        }
    }

    /// The PC's word says where the pointer is and how it looks; hidden, off its
    /// displays, or of a look that never came, it is not drawn.
    #[test]
    fn the_pointer_is_where_the_pc_says() {
        let now = Instant::now();
        let mut c = Cursors::default();
        let d = DisplayId(3);
        c.take_at(PC, Some(d), 0, 65535, 5);
        assert!(c.of(PC, now).is_none(), "its look has not come");
        c.take_look(PC, 5, look(1));
        let s = c.of(PC, now).expect("drawn");
        assert_eq!(s.id, DisplayKey::new(PC, d));
        assert_eq!(s.uv, Vec2::new(0.0, 1.0));
        assert_eq!(s.look, &look(1));
        assert!(c.on(DisplayKey::new(PC, d), now).is_some());
        assert!(c.on(DisplayKey::new(PC, DisplayId(4)), now).is_none());
        c.take_at(PC, Some(d), 1, 1, 0);
        assert!(c.of(PC, now).is_none(), "hidden");
        c.take_at(PC, None, 0, 0, 5);
        assert!(c.of(PC, now).is_none(), "off its displays");
        c.forget(PC);
        assert!(!c.has(PC, 5));
    }

    /// Looks are kept as the PC counts them: 64, the first to come going first.
    #[test]
    fn the_first_look_goes_when_one_more_comes() {
        let mut c = Cursors::default();
        for n in 1..=CURSOR_SHAPES as u32 {
            c.take_look(PC, n, look(n as u8));
        }
        assert!(c.has(PC, 1));
        c.take_look(PC, 1000, look(0));
        assert!(!c.has(PC, 1), "the first went");
        assert!(c.has(PC, 2) && c.has(PC, 1000));
        // Sent again, it is the newest.
        c.take_look(PC, 2, look(9));
        c.take_look(PC, 1001, look(0));
        assert!(c.has(PC, 2) && !c.has(PC, 3));
    }

    /// Moved by the headset, the pointer is drawn where it sent the mouse, in the last
    /// look, until the PC's word comes there or 200 ms passed.
    #[test]
    fn the_headset_moving_the_mouse_draws_it_there_until_the_pc_catches_up() {
        let t0 = Instant::now();
        let mut c = Cursors::default();
        let (one, two) = (DisplayId(1), DisplayId(2));
        c.take_look(PC, 5, look(1));
        c.take_at(PC, Some(one), 100, 100, 5);
        c.moved_here(PC, two, 30_000, 20_000, t0);
        let s = c.of(PC, t0).unwrap();
        assert_eq!((s.id.id, s.shape), (two, 5));
        // The PC still says where it was: the headset's place holds.
        c.take_at(PC, Some(one), 110, 100, 5);
        assert_eq!(c.of(PC, t0 + OWN_FOR / 2).unwrap().id.id, two);
        // 200 ms on, the PC's word again.
        assert_eq!(c.of(PC, t0 + OWN_FOR).unwrap().id.id, one);
        // Caught up before that: the PC's word at once.
        c.moved_here(PC, two, 30_000, 20_000, t0);
        c.take_at(PC, Some(two), 30_100, 19_900, 5);
        let s = c.of(PC, t0).unwrap();
        assert_eq!((s.uv.x * 65535.0).round() as u16, 30_100);
        // A PC that never said its pointer: nothing drawn for a move of the headset.
        let other = PcId([2; 8]);
        c.moved_here(other, one, 0, 0, t0);
        assert!(c.of(other, t0).is_none());
    }
}
