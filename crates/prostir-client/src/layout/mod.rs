//! Where monitors float in the room, how controllers move them, and the files of the
//! arrangement.
//!
//! How the monitors are arranged (the presets, a new monitor, "Line up"), the rows they
//! make and the PC desktop made of those is `prostir_layout`, the code the PC runs too;
//! here the headset's state goes in and out of it. Pure functions, by topic: where a
//! ray meets a monitor ([`hit`]), the magnet ([`stick`]), the presets, a new monitor
//! and "Line up" ([`arrange`]), cinema ([`cinema`]), a thing held ([`grab`]), the PC's
//! desktop ([`desktop`]), the order of drawing ([`draw`]) and the files ([`saved`]).

#![forbid(unsafe_code)]

mod arrange;
mod cinema;
mod desktop;
mod draw;
mod grab;
mod hit;
mod saved;
mod stick;
#[cfg(test)]
mod tests;

pub use arrange::{
    ahead, arrange, carried, facing_yaw, frame, line_up, place_new, preset_radius, refit, tilt,
};
pub use cinema::{cinema, cinema_choice};
pub use desktop::{Desktop, DesktopMemo};
pub use draw::{DRAW_HOLD, draw_order};
pub use grab::Grab;
pub use hit::{Cylinder, Hit, Ray, hit, hit_surface, image_point, image_uv};
pub use saved::{Arrangement, Desk, PixelSize, parse_keyboard, write_keyboard};
pub(crate) use saved::{parse_place, parse_radius, parse_tilt, placed_well, upright, write_place};
pub use stick::{Stick, StickScratch, stick, stick_on_wall};

pub use prostir_layout::circle::{OVERHEAD_PITCH, RADIUS};

/// What the arrangement's rules take a monitor's number to be: a display of a PC in
/// the headset ([`crate::ids::DisplayKey`]), any number in the tests. The rules never
/// look into it.
pub trait Key: Copy + Ord + std::hash::Hash {}

impl<K: Copy + Ord + std::hash::Hash> Key for K {}
pub use prostir_layout::space::{METRES_PER_PIXEL, Placement, facing};
