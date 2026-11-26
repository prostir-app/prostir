//! What the headset's mouse buttons, wheel and touches are on the wire
//! ([`crate::ClientMsg::MouseButton`], [`crate::ClientMsg::MouseScroll`],
//! [`crate::ClientMsg::Touch`]), sent only to a PC that names feature 15
//! ([`crate::state::caps::pc::INPUT`]).

use crate::wire::wire_enum;

/// One notch of a mouse wheel in [`crate::ClientMsg::MouseScroll`]: scrolling is in
/// 1/120 of a notch, so a smooth scroll sends parts of one.
pub const SCROLL_NOTCH: i32 = 120;

wire_enum! {
    /// A mouse button.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Button {
        0 => Left,
        1 => Right,
        2 => Middle,
        /// The side button that goes back.
        3 => Back,
        4 => Forward,
    }
}

wire_enum! {
    /// What a finger does in [`crate::ClientMsg::Touch`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum TouchPhase {
        /// It touched the display.
        0 => Down,
        /// It moved while touching.
        1 => Move,
        /// It left the display: what it did counts.
        2 => Up,
        /// It is gone without counting (the hand was lost, it went through the display).
        3 => Cancel,
    }
}
