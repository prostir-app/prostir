//! The order the monitors are drawn in, far to near.

use super::Key;

/// A monitor drawn after another (over it, where they overlap as seen) is drawn before
/// it only once it is further from the head than that one by more than this (5 cm): two
/// at about one distance, as a monitor glued over a row and that row, keep their order,
/// so the strip where they overlap never flickers from one to the other as the head
/// moves.
pub const DRAW_HOLD: f32 = 0.05;

/// The order the monitors are drawn in, far to near (the nearest last, over the others):
/// `order` holds each one's distance from the head (one held in a hand at -1, last of
/// all) and is put in order in place; `before` is the order of the frame before. It
/// starts from that order, the new ones after it, and two next to each other swap only
/// where the one drawn later is further by more than [`DRAW_HOLD`]. New ones at one
/// distance go by their numbers, so the order never depends on how they were listed.
/// Nothing is allocated.
pub fn draw_order<K: Key>(order: &mut [(f32, K)], before: &[K]) {
    let rank = |id: K| before.iter().position(|b| *b == id).unwrap_or(usize::MAX);
    // As drawn before, the new ones after, far to near (an insertion sort: in place).
    let first = |a: &(f32, K), b: &(f32, K)| {
        rank(a.1)
            .cmp(&rank(b.1))
            .then(b.0.total_cmp(&a.0))
            .then(a.1.cmp(&b.1))
            .is_lt()
    };
    for i in 1..order.len() {
        let mut j = i;
        while j > 0 && first(&order[j], &order[j - 1]) {
            order.swap(j - 1, j);
            j -= 1;
        }
    }
    for _ in 0..order.len() {
        let mut swapped = false;
        for i in 1..order.len() {
            if order[i].0 > order[i - 1].0 + DRAW_HOLD {
                order.swap(i - 1, i);
                swapped = true;
            }
        }
        if !swapped {
            break;
        }
    }
}
