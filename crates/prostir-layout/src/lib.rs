//! Where Prostir's monitors go, as pure functions both sides share: only vector math
//! (`glam`), no input or output and nothing tied to an operating system, so the PC and
//! the headset use the same code and everything here is tested on the PC. The
//! invariants each part keeps, and why, are in `plans/layout-math.md`; the tests in
//! `tests/` check them over thousands of seeded arrangements.
//!
//! The pipeline, from the room to the PC desktop:
//!
//! 1. [`space::see`]: every monitor as the eye sees it, an angular rectangle in the
//!    arrangement's frame (upright, or tilted for lying down).
//! 2. [`rows::rows_of`]: rows from what is seen, with hysteresis (a monitor between two
//!    rows joins one; a hand's tremor moves nothing between rows).
//! 3. [`desk::plan`]: the desktop from the rows and a [`desk::View`] of how monitors
//!    stand along the edges they share: the headset passes the room
//!    ([`view::Room`]), the PC the headset's plan as boxes ([`desk::Boxes`]). Planning
//!    a plan again gives the same plan, so both sides agree to the pixel.
//!
//! And the arrangement in the room, on request only (the headset):
//!
//! - [`circle::arrange`]: the widening circle (one row up to 210°, the radius grows to
//!   1.5 m, then two and three rows above the main one).
//! - [`place::place_new`]: a new monitor next to the arrangement, moving nothing; or
//!   [`place::into_row`], into the main row, for one far wider than its circle that
//!   would otherwise be out of view ([`space::in_view`]).
//! - [`tidy::line_up`]: every row straightened on its own surface, rows never merged.
//!
//! Every function is generic over the key that names a monitor (`K: Copy + Ord`), so
//! the numbers each side uses can change without changing the rules.

pub mod circle;
pub mod desk;
pub mod place;
pub mod rows;
pub mod space;
pub mod tidy;
pub mod view;
