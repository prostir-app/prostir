//! A fake headset: connects to the PC app like the Quest app does (encryption,
//! pairing, hello), keeps track of what the PC says, and sends what a test asks.
//!
//! Used by the PC app's end-to-end tests (`crates/prostir/tests/e2e`, against a PC app built
//! with the fake desktop) and by `prostir-probe`, which checks a real PC by hand.
//!
//! And a fake PC ([`pc`]), for the headset: its selftest, and tests of its code.

pub mod find;
mod headset;
pub mod nal;
pub mod pc;

pub use headset::{
    Arrival, ConnectError, CursorAt, FIRST_VOLUME, FakeHeadset, Keys, Stream, VOLUME_STEPS, View,
    stepped,
};
