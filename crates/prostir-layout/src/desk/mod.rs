//! The PC desktop from rows: where each output goes, top-left corner in logical pixels
//! (the compositor's units: pixel size divided by the output's scale).
//!
//! The desktop reproduces the arrangement for movement in every direction: where two
//! monitors meet as seen, the mouse (and a window dragged across) leaves one and enters
//! the other at the same point of the room. How two monitors stand along the edge they
//! share comes from a [`View`] as a [`Relation`]: the offset that puts the middle of the
//! shared edge at the same desktop point on both (with different pixel densities a
//! perfect match along the whole edge is impossible; matching the middle halves the
//! largest jump), how much they share, and how far apart they are.
//!
//! - Within a row, monitors go side by side, left to right as seen. Each is offset up
//!   or down from its left neighbour exactly as seen: a monitor hung a third of its
//!   height higher is a third higher on the desktop. Only a step within [`STEP`] of
//!   lining up (tops, middles or bottoms) is taken as what a hand leaves and lined up,
//!   so a row placed by hand is straight, while an offset made on purpose stays.
//!   Neighbours always share at least [`need`] of their edge: an offset so large that
//!   less would be left is cut back to that.
//! - The row with the main monitor starts at `0,0`. Every other row, outwards from it,
//!   is cut where another row's monitor stands between two of its monitors as seen (a
//!   portrait one reaching up from the row below), and each piece is put across where
//!   it is seen against what is placed (a weighted median of what every pair that
//!   faces each other as seen says, snapped to lining up when within [`NOISE`]). Its
//!   monitor seen nearest the rows below rests on them; the others follow their
//!   neighbours as seen, and rest on what is under them too where they are seen
//!   touching it.
//! - The laptop screen goes under the main monitor unless the headset placed it, other
//!   real displays next to it, window panels in a column right of everything.
//!
//! Nothing ever overlaps and every output can be reached by the mouse, whatever the
//! input (see `plans/layout-math.md` for the proofs).

use std::collections::{BTreeMap, BTreeSet};

use crate::space::Side;

mod boxes;
mod check;
mod pieces;

pub use boxes::{Boxes, SeenBox, as_boxes};
pub use check::{Origin, crossings, fit, rebase, settled};
pub use pieces::{GAP_CLOSE, pieces_of};
use pieces::{already_planned, bridges, chain, members_of, pieces, place_piece, put};

/// Shared edge the mouse needs to cross from one output to the next (or all of a
/// shorter edge).
pub const REACH: i32 = 200;
/// A difference this small, as a fraction of the shorter (narrower) of two monitors, is
/// a hand's noise: it is lined up. 3 % is 2 cm on a 1440-pixel-high monitor at the
/// default size (0.70 m high), above a hand's tremor and the magnet's own precision,
/// and well under any offset made on purpose (see `plans/layout-math.md`).
pub const NOISE: f64 = 0.03;
/// A step between two neighbours in a row this small, as a fraction of the shorter of
/// the two, is what a hand leaves: the row is lined up there. A tenth is 7 cm on a
/// 1440-pixel-high monitor at the default size (0.70 m high, 144 px). Placed by hand in
/// the headset a monitor ends up a few centimetres off, and further as seen when it is
/// pulled nearer or pushed further than its neighbour above or below the eyes (10 cm of
/// depth at 0.8 m over the eyes is a step of about 150 px): on 2026-11-20 the owner,
/// lifting a monitor level with its neighbour, got within 100 px (7 %) and no closer.
/// An offset made on purpose is a sixth of the height or more (240 px), well beyond.
/// Between rows (where a monitor stands across the row next to it) [`NOISE`] still
/// holds: there the plan follows what is seen as closely as it can.
pub const STEP: f64 = 0.10;
/// Two monitors are seen touching when the gap between their facing edges is at most
/// this fraction of the first one's size across it (5 %: 6 cm between two monitors 1.25
/// m wide), or they overlap.
pub const TOUCH_GAP: f64 = 0.05;
/// The headset keeps its previous plan while no output of the new one is further than
/// this fraction of its size (across, and up and down) from where it was (see
/// [`settled`]): noise and a centimetre or so more.
pub const DEADBAND: f64 = 0.05;

/// Positions further out than this on the wire are nonsense (and would overflow).
pub const FAR: i32 = 100_000;

/// A monitor in a row of `SetLayout`, and where the centre of its box is in the
/// headset's plan, in the pixels of the streams (see [`Spot::wire`] and `PROTOCOL.md`,
/// "Layout"): what the PC keeps of what the headset sent. `None`: not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<K> {
    pub id: K,
    /// Across, left to right.
    pub center: Option<i32>,
    /// Down.
    pub middle: Option<i32>,
}

impl<K> Entry<K> {
    /// A monitor whose place is not known.
    pub fn new(id: K) -> Self {
        Self {
            id,
            center: None,
            middle: None,
        }
    }
}

/// An output's size on the desktop, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

/// How monitor `b` stands next to monitor `a` along the edge they share as seen, in
/// desktop pixels. For `b` right or left of `a` the edge runs down and `offset` is
/// `b.y - a.y`; for `b` above or below it runs across and `offset` is `b.x - a.x`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Relation {
    /// The offset that puts the middle of the shared edge at the same desktop point on
    /// both, each monitor's pixels where they are seen on it.
    pub offset: f64,
    /// Where that middle is along `a`'s edge, in `a`'s pixels from its top (left).
    pub at: f64,
    /// How much of an edge they share as seen, in `a`'s pixels (0 when they do not
    /// face each other).
    pub shared: f64,
    /// The same as a fraction of the shorter of the two edges, as seen (0..=1).
    pub seam: f64,
    /// How far apart they are across the edge as seen, in `a`'s pixels (negative when
    /// they overlap).
    pub gap: f64,
    /// The same as a fraction of `a`'s size across the edge (its width for a side
    /// neighbour, its height for one above or below).
    pub apart: f64,
}

/// What the plan knows of how the monitors are seen.
pub trait View<K> {
    /// How `b` stands on `side` of `a` as seen, if they face each other there.
    fn relate(&self, a: K, b: K, side: Side) -> Option<Relation>;
    /// Whether another monitor is seen in the gap between `a` and `b` (`a` left of
    /// `b`), so they cannot be put side by side.
    fn between(&self, a: K, b: K) -> bool;
    /// Where the middle of `k` is seen: across (right positive) and up, in any unit.
    fn middle(&self, k: K) -> Option<(f64, f64)>;
    /// How high `k` is seen, in the unit of [`View::middle`].
    fn height(&self, _k: K) -> Option<f64> {
        None
    }
    /// Where `k` is on a desktop already, if this view is a plan (the headset's, read
    /// back by the PC): its top-left corner, when its box is whole pixels at `size`.
    fn exact(&self, _k: K, _size: Size) -> Option<(i32, i32)> {
        None
    }
}

/// Nothing is known of how the monitors are seen (a fresh start, `SetOrder`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Unseen;

impl<K> View<K> for Unseen {
    fn relate(&self, _: K, _: K, _: Side) -> Option<Relation> {
        None
    }

    fn between(&self, _: K, _: K) -> bool {
        false
    }

    fn middle(&self, _: K) -> Option<(f64, f64)> {
        None
    }
}

/// What goes on the desktop.
#[derive(Debug, Clone)]
pub struct Desk<K> {
    /// Rows from top to bottom, each left to right. A monitor listed twice counts where
    /// it is listed first.
    pub rows: Vec<Vec<K>>,
    /// The primary monitor; its row is laid out first, from `0,0`.
    pub main: Option<K>,
    /// Goes under the main monitor unless it is in a row (the laptop screen).
    pub under_main: Option<K>,
    /// Other real displays: next to `under_main`, under everything over them.
    pub bottom: Vec<K>,
    /// Window panels, in a column right of everything.
    pub column: Vec<K>,
}

impl<K> Default for Desk<K> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            main: None,
            under_main: None,
            bottom: Vec::new(),
            column: Vec::new(),
        }
    }
}

/// A placed output: top-left corner and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Spot<K> {
    pub id: K,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl<K> Spot<K> {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// How many columns the two share (0 when none).
    pub fn across<L>(&self, o: &Spot<L>) -> i32 {
        (self.right().min(o.right()) - self.x.max(o.x)).max(0)
    }

    /// How many lines the two share (0 when none).
    pub fn along<L>(&self, o: &Spot<L>) -> i32 {
        (self.bottom().min(o.bottom()) - self.y.max(o.y)).max(0)
    }

    /// Its centre in whole pixels as `SetLayout` carries it: the top-left corner plus
    /// half the size, rounded down. [`SeenBox::from_wire`] adds the half pixel of an odd
    /// size back, so the PC gets this very spot.
    pub fn wire(&self) -> (i32, i32) {
        (self.x + self.w.div_euclid(2), self.y + self.h.div_euclid(2))
    }

    /// Whether the two cover a pixel in common.
    pub fn overlaps<L>(&self, o: &Spot<L>) -> bool {
        self.across(o) > 0 && self.along(o) > 0
    }

    /// Where `o` touches this one and how long an edge they share (`None` when they do
    /// not touch along an edge).
    pub fn shared_edge<L>(&self, o: &Spot<L>) -> Option<(Side, i32)> {
        let (side, len) = if o.x == self.right() {
            (Side::Right, self.along(o))
        } else if o.right() == self.x {
            (Side::Left, self.along(o))
        } else if o.bottom() == self.y {
            (Side::Above, self.across(o))
        } else if o.y == self.bottom() {
            (Side::Below, self.across(o))
        } else {
            return None;
        };
        (len > 0).then_some((side, len))
    }

    /// The side `o` is on if the mouse crosses from this one to it: they share at least
    /// [`need`] of an edge.
    pub fn crossing<L>(&self, o: &Spot<L>) -> Option<Side> {
        let (side, len) = self.shared_edge(o)?;
        let enough = match side {
            Side::Left | Side::Right => need(self.h, o.h),
            Side::Above | Side::Below => need(self.w, o.w),
        };
        (len >= enough).then_some(side)
    }
}

/// Shared edge the mouse needs between edges this long: [`REACH`] or all of the
/// shorter.
pub fn need(a: i32, b: i32) -> i32 {
    REACH.min(a).min(b).max(1)
}

/// The largest difference taken as noise next to a monitor `len` pixels long, in whole
/// pixels (whole, so planning a plan again decides the same: see [`snap`]).
pub fn noise(len: i32) -> i32 {
    (NOISE * f64::from(len.max(0))).floor() as i32
}

/// [`STEP`] of `len` pixels, rounded down.
pub fn step(len: i32) -> i32 {
    (STEP * f64::from(len.max(0))).floor() as i32
}

/// `value` lined up with the nearest of `options` within `tolerance` pixels, else
/// rounded. Half a pixel more is allowed, so a value already rounded snaps exactly as
/// the unrounded one did.
fn snap(value: f64, options: &[i32], tolerance: i32) -> i32 {
    options
        .iter()
        .copied()
        .map(|o| (o, (value - f64::from(o)).abs()))
        .filter(|(_, d)| *d <= f64::from(tolerance) + 0.5)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or_else(|| value.clamp(-1.0e9, 1.0e9).round() as i32, |(o, _)| o)
}

/// How far down from `a`'s top `b`'s top goes, `b` next to `a` in a row: as seen,
/// lined up with `a`'s middle, top or bottom when within [`STEP`], sharing at least
/// [`need`]. Unknown: `fallback` (middles lined up in a row, tops for the laptop screen
/// and other displays).
fn side_offset(rel: Option<Relation>, ha: i32, hb: i32, fallback: Lining) -> i32 {
    let middle = (ha - hb).div_euclid(2);
    let options = [middle, 0, ha - hb];
    let enough = need(ha, hb);
    let (lo, hi) = (enough - hb, ha - enough);
    let offset = match rel {
        Some(r) if r.offset.is_finite() => snap(r.offset, &options, step(ha.min(hb))),
        _ => match fallback {
            Lining::Middle => middle,
            Lining::Top => 0,
        },
    };
    offset.clamp(lo, hi)
}

/// How monitors whose place is not known line up in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lining {
    Middle,
    Top,
}

/// A monitor of a row and its size.
#[derive(Debug, Clone, Copy)]
struct Member<K> {
    id: K,
    size: Size,
}

/// Which way a piece goes from what is placed: up onto it or down under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
}

impl Dir {
    /// The side the piece is on, seen from what it rests on.
    fn side(self) -> Side {
        match self {
            Dir::Up => Side::Above,
            Dir::Down => Side::Below,
        }
    }
}

/// Where each output goes (see the module's description). Outputs without a size are
/// left out (an output the compositor does not list yet).
///
/// The headset plans from the room ([`crate::view::Room`]) and sends the plan as boxes
/// ([`as_boxes`]); the PC plans from those boxes and gets the same plan back (planning
/// a plan again gives the same plan).
pub fn plan<K: Copy + Ord, V: View<K>>(
    desk: &Desk<K>,
    size: impl Fn(K) -> Option<Size>,
    view: &V,
) -> Vec<Spot<K>> {
    let usable = |id: K| size(id).filter(|s| s.w > 0 && s.h > 0);
    let (rows, mut listed) = members_of(desk, &usable);

    // The main monitor: the primary, or the first of the widest row (the lower of two).
    let width = |r: &[Member<K>]| r.iter().map(|m| m.size.w).sum::<i32>();
    let main_key = desk
        .main
        .filter(|m| rows.iter().flatten().any(|x| x.id == *m))
        .or_else(|| {
            let r = (0..rows.len()).max_by_key(|&r| (width(&rows[r]), r))?;
            Some(rows[r][0].id)
        });
    let main_index = rows
        .iter()
        .position(|r| r.iter().any(|m| Some(m.id) == main_key));
    let mut placed: Vec<Spot<K>> = Vec::new();
    if let Some(kept) = already_planned(&rows, view, main_index) {
        // A plan made already (the headset's, as the PC reads it back): kept as it is.
        placed = kept;
    } else if !rows.is_empty() {
        let main_row = rows
            .iter()
            .position(|r| r.iter().any(|m| Some(m.id) == main_key))
            .expect("the main monitor is in a row");
        let cut: Vec<Vec<&[Member<K>]>> = (0..rows.len())
            .map(|r| pieces(&rows[r], &bridges(&rows, r, main_row), view, r == main_row))
            .collect();
        // What each row's pieces became, by piece.
        let mut done: Vec<BTreeMap<usize, Vec<Spot<K>>>> = vec![BTreeMap::new(); rows.len()];
        let main_cut = &cut[main_row];
        let first_piece = main_cut
            .iter()
            .position(|p| p.iter().any(|m| Some(m.id) == main_key))
            .unwrap_or(0);
        let first = main_cut[first_piece]
            .iter()
            .position(|m| Some(m.id) == main_key)
            .unwrap_or(0);
        let spots = chain(main_cut[first_piece], first, (0, 0), view, Lining::Middle);
        placed.extend(spots.iter().copied());
        done[main_row].insert(first_piece, spots);
        // The main row's other pieces wait, outwards from the main one, for what holds
        // them where they are seen: the row above that spans them, else the row below,
        // else the piece next to them.
        let mut waiting: Vec<usize> = (first_piece + 1..main_cut.len())
            .chain((0..first_piece).rev())
            .collect();
        let held = |piece: &[Member<K>], placed: &[Spot<K>], side: Side| -> Vec<Spot<K>> {
            placed
                .iter()
                .filter(|p| {
                    piece.iter().any(|m| {
                        view.relate(p.id, m.id, side)
                            .is_some_and(|r| r.shared > 0.0)
                    })
                })
                .copied()
                .collect()
        };
        let row_spots = |done: &BTreeMap<usize, Vec<Spot<K>>>| -> Vec<Spot<K>> {
            done.values().flatten().copied().collect()
        };
        for r in (0..main_row).rev() {
            let adjacent = row_spots(&done[r + 1]);
            for k in 0..cut[r].len() {
                put(
                    &cut[r],
                    k,
                    Dir::Up,
                    &adjacent,
                    &mut placed,
                    &mut done[r],
                    view,
                );
            }
        }
        waiting.retain(|&k| {
            let over = held(cut[main_row][k], &placed, Side::Below);
            if over.is_empty() {
                return true;
            }
            put(
                &cut[main_row],
                k,
                Dir::Down,
                &over,
                &mut placed,
                &mut done[main_row],
                view,
            );
            false
        });
        for r in main_row + 1..rows.len() {
            let adjacent = row_spots(&done[r - 1]);
            for k in 0..cut[r].len() {
                put(
                    &cut[r],
                    k,
                    Dir::Down,
                    &adjacent,
                    &mut placed,
                    &mut done[r],
                    view,
                );
            }
        }
        // A piece cut to be held from above is, but one the row above no longer spans
        // as seen (a monitor let go beside it, a row turned) is not: it goes on in the
        // main row on its own, held by nothing.
        for k in waiting {
            put(
                &cut[main_row],
                k,
                Dir::Down,
                &[],
                &mut placed,
                &mut done[main_row],
                view,
            );
        }
    }

    // The laptop screen under the main monitor, other real displays right of it, side
    // by side, tops lined up, hanging under whatever is over them.
    let mut extra = |id: K| {
        listed
            .insert(id)
            .then(|| usable(id))
            .flatten()
            .map(|size| Member { id, size })
    };
    let lid = desk.under_main.and_then(&mut extra);
    let members: Vec<Member<K>> = lid
        .into_iter()
        .chain(desk.bottom.iter().filter_map(|&id| extra(id)))
        .collect();
    if !members.is_empty() {
        let main_spot = main_key
            .and_then(|m| placed.iter().find(|p| p.id == m))
            .copied();
        match main_spot {
            Some(main) => {
                let target = match lid {
                    Some(l) => main.x + (main.w - l.size.w).div_euclid(2),
                    None => main.x,
                };
                let spots = place_piece(
                    &members,
                    Dir::Down,
                    &placed,
                    &[main],
                    (None, None),
                    &Unseen,
                    Some(target),
                );
                placed.extend(spots);
            }
            None => placed.extend(chain(&members, 0, (0, 0), &Unseen, Lining::Top)),
        }
    }

    // Window panels in a column right of everything, from the top of the output that
    // reaches furthest right.
    let (x, mut y) = placed
        .iter()
        .max_by_key(|p| (p.right(), std::cmp::Reverse(p.y)))
        .map_or((0, 0), |p| (p.right(), p.y));
    for &id in &desk.column {
        if let Some(m) = extra(id) {
            placed.push(Spot {
                id,
                x,
                y,
                w: m.size.w,
                h: m.size.h,
            });
            y += m.size.h;
        }
    }

    // The desktop starts at 0,0 (KWin, Mutter and Hyprland take that; see `rebase`).
    rebase(&placed, Origin::TopLeft)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spot sent as `SetLayout` (centre and middle in whole pixels) comes back as its
    /// own box, odd sizes too, so the PC plans the headset's plan to the pixel.
    #[test]
    fn a_spot_survives_the_wire() {
        for (w, h) in [(2560, 1440), (1366, 768), (1365, 767), (1, 1)] {
            for (x, y) in [(0, 0), (-1281, 7), (4096, -2049)] {
                let s = Spot {
                    id: 0u8,
                    x,
                    y,
                    w,
                    h,
                };
                let (c, m) = s.wire();
                assert_eq!(SeenBox::from_wire(c, m, Size { w, h }), SeenBox::of(&s));
                let boxes = Boxes::new(vec![(
                    0u8,
                    SeenBox::from_wire(c, m, Size { w, h }),
                    Size { w, h },
                )]);
                assert_eq!(boxes.exact(0, Size { w, h }), Some((x, y)));
            }
        }
    }
}
