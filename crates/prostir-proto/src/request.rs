//! Requests from the headset, commands from the PC, and the one reply each gets.
//!
//! A request or command is an envelope: the asking side's [`ReqId`], a `u8` kind, then
//! the kind's fields. A kind the reader does not know reads as [`Asked::Unknown`] with
//! its fields skipped, and is answered with [`ErrorCode::UNSUPPORTED`], so "exactly
//! one reply" holds between any two versions. A new field in a kind makes a new kind,
//! never a longer old one; a new [`Outcome`] comes only with a new kind.

use crate::Codec;
use crate::state::{DisplayId, ErrorCode, MAX_DETAIL, MAX_DISPLAY_NAME, MAX_DISPLAYS, Mode};
use crate::wire::{Reader, Wire, WireError, wire_enum};

/// Longest path in [`Outcome::Saved`], in bytes.
pub const MAX_PATH: usize = 1024;

wire_enum! {
    /// Which display becomes the primary one after [`Request::SetDisplays`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Primary {
        /// The monitor made for `modes[index]`.
        0 => Index { index: u8 },
        /// A display of the PC itself (the laptop's panel, say).
        1 => Display { id: DisplayId },
    }
}

wire_enum! {
    message
    /// What the headset asks the PC to change, from its menu.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Request {
        /// A new monitor: [`Outcome::Added`].
        1 => AddDisplay { mode: Mode },
        2 => RemoveDisplay { id: DisplayId },
        /// The monitors of a profile, left to right: [`Outcome::Ids`] in the order of
        /// `modes`. Monitors of a size that is there already are kept, with their
        /// windows; `shown_physical` are the displays of the PC to show.
        3 => SetDisplays {
            #[wire(max = MAX_DISPLAYS)]
            modes: Vec<Mode>,
            primary: Option<Primary>,
            #[wire(max = MAX_DISPLAYS)]
            shown_physical: Vec<DisplayId>,
        },
        4 => SetPrimary { id: DisplayId },
        /// Shows or hides a display of the PC itself.
        5 => ShowPhysical { id: DisplayId, shown: bool },
        /// How the PC encodes; none leaves that setting as it is. What came of it shows
        /// in `Encoding`.
        6 => SetEncoding {
            bitrate: Option<u16>,
            codec: Option<Codec>,
            software: Option<bool>,
            sharp: Option<bool>,
        },
        /// The PC's sound in the headset, or on the PC.
        7 => SetSound { to_headset: bool },
        /// The PC's active window on a panel of its own: [`Outcome::Added`].
        9 => DetachWindow,
        /// The PC's active window onto display `id`.
        10 => WindowHere { id: DisplayId },
        /// A picture of display `id` saved on the PC: [`Outcome::Saved`].
        11 => Screenshot { id: DisplayId },
        /// Where the mouse is: [`Outcome::Pointer`].
        12 => FindMouse,
        /// Another scale for monitor `id`, percent (one of `Limits.scales`): made anew
        /// at it under the same number, `Starting` until it runs again (from 0.2.0 step
        /// 7.10; an older PC answers `UNSUPPORTED`).
        13 => SetScale { id: DisplayId, scale: u16 },
    }
}

wire_enum! {
    message
    /// What the PC (its tray, and later `prostir ctl` and hotkeys) asks the headset to
    /// do: actions of the headset's menu that make sense from outside.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Command {
        /// Bring everything in front.
        1 => Recenter,
        /// Switch to a profile: its name, or its number from 1 as the headset's menu
        /// lists them; answered once it is in place.
        2 => Profile {
            #[wire(max = MAX_DISPLAY_NAME)]
            name: String,
        },
        3 => Cinema { on: bool },
        /// "Focus" (changes `Prefs`).
        4 => FocusMode { on: bool },
        /// Closer (below 0) or further.
        5 => Distance { steps: i8 },
        /// A preset arrangement (a number the headset knows, else `BAD_ARGUMENT`).
        6 => Preset { preset: u8 },
        /// "Line up".
        7 => LineUp,
        /// Show where the mouse is.
        8 => ShowMouse,
        // Kind 9 is retired (see `RETIRED_COMMAND_KINDS`).
        /// Follow the head.
        10 => Follow { on: bool },
        /// "Reset the arrangement".
        11 => ResetArrangement,
        /// "No limits" (changes `Prefs`).
        12 => NoLimits { on: bool },
        /// Gives profile `profile` (its name, or its number from 1) the name `name`; an
        /// empty name gives it back the menu's own ("Profile 2"). The headset's
        /// `Profiles` part shows it before the reply (from 0.2.0 step 7.10; an older
        /// headset answers `UNSUPPORTED`).
        13 => RenameProfile {
            #[wire(max = MAX_DISPLAY_NAME)]
            profile: String,
            #[wire(max = MAX_DISPLAY_NAME)]
            name: String,
        },
    }
}

wire_enum! {
    /// How a request or command went: exactly one for each.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Outcome {
        0 => Done,
        /// The display made for it.
        1 => Added { id: DisplayId },
        /// The displays made or kept for it, in the order asked.
        2 => Ids {
            #[wire(max = MAX_DISPLAYS)]
            ids: Vec<DisplayId>,
        },
        /// Where it was saved on the PC.
        3 => Saved {
            #[wire(max = MAX_PATH)]
            path: String,
        },
        /// Display `id`, the point `u`, `v` across it (0..=65535 from the top left).
        4 => Pointer { id: DisplayId, u: u16, v: u16 },
        /// It did not happen: why, and the detail for a code the reader does not know.
        5 => Failed {
            error: ErrorCode,
            #[wire(max = MAX_DETAIL)]
            detail: String,
        },
    }
}

impl Outcome {
    /// A failure with `detail` cut to fit.
    pub fn failed(error: ErrorCode, detail: &str) -> Self {
        Self::Failed {
            error,
            detail: crate::state::clip(detail, MAX_DETAIL),
        }
    }
}

/// Kinds of requests no longer used, never to be used again: `8` was planned as "No
/// limits", which the headset owns.
pub const RETIRED_REQUEST_KINDS: &[u8] = &[8];
/// Kinds of commands no longer used, never to be used again: `9` set the headset's
/// microphone switch, which only the user changes, in the headset. A headset reads it
/// as a kind it does not know and answers `UNSUPPORTED`.
pub const RETIRED_COMMAND_KINDS: &[u8] = &[9];

/// A request or command as read: a kind this build knows, or another one, whose
/// fields were skipped. It runs to the end of its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked<T> {
    Known(T),
    Unknown { kind: u8 },
}

/// The kinds of an envelope ([`Request`], [`Command`]).
pub trait Kinds: Sized {
    fn kind(&self) -> u8;
    fn put_kind_fields(&self, out: &mut Vec<u8>) -> Result<(), WireError>;
    fn get_kind(kind: u8, r: &mut Reader<'_>) -> Result<Option<Self>, WireError>;
}

macro_rules! kinds {
    ($($t:ty),*) => {$(
        impl Kinds for $t {
            fn kind(&self) -> u8 {
                self.wire_tag()
            }
            fn put_kind_fields(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
                self.put_fields(out)
            }
            fn get_kind(kind: u8, r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
                Self::get_fields(kind, r)
            }
        }
    )*};
}

kinds!(Request, Command);

impl<T: Kinds> Wire for Asked<T> {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            Self::Known(k) => {
                out.push(k.kind());
                k.put_kind_fields(out)
            }
            // Only tests send what nobody knows.
            Self::Unknown { kind } => {
                out.push(*kind);
                Ok(())
            }
        }
    }

    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        let kind = u8::get(r)?;
        match T::get_kind(kind, r)? {
            Some(k) => Ok(Self::Known(k)),
            None => {
                r.rest();
                Ok(Self::Unknown { kind })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{decode, encode};

    #[test]
    fn a_known_kind_reads_back_and_an_unknown_one_is_skipped() {
        let asked = Asked::Known(Request::SetDisplays {
            modes: vec![Mode::new(2560, 1440), Mode::new(5120, 1440)],
            primary: Some(Primary::Index { index: 1 }),
            shown_physical: vec![DisplayId(100)],
        });
        let bytes = encode(&asked).unwrap();
        assert_eq!(bytes[0], 3);
        assert_eq!(decode::<Asked<Request>>(&bytes).unwrap(), asked);
        // A kind from a newer headset, with fields this build cannot read.
        let unknown = [200, 1, 2, 3, 4, 5];
        assert_eq!(
            decode::<Asked<Request>>(&unknown).unwrap(),
            Asked::Unknown { kind: 200 }
        );
        // A known kind stays strict.
        let mut longer = bytes;
        longer.push(0);
        assert!(decode::<Asked<Request>>(&longer).is_err());
    }

    /// Retired kinds are never given out again, and read as unknown, so an older
    /// sender of one gets `UNSUPPORTED`.
    #[test]
    fn retired_kinds_stay_unused() {
        for kind in RETIRED_REQUEST_KINDS {
            assert!(
                !Request::TAGS.contains(kind),
                "request kind {kind} is retired"
            );
            assert_eq!(
                decode::<Asked<Request>>(&[*kind]).unwrap(),
                Asked::Unknown { kind: *kind }
            );
        }
        for kind in RETIRED_COMMAND_KINDS {
            assert!(
                !Command::TAGS.contains(kind),
                "command kind {kind} is retired"
            );
            // The microphone switch, as an older PC sends it.
            assert_eq!(
                decode::<Asked<Command>>(&[*kind, 1]).unwrap(),
                Asked::Unknown { kind: *kind }
            );
        }
    }

    #[test]
    fn commands_and_outcomes_round_trip() {
        for c in [
            Command::Recenter,
            Command::Profile {
                name: "Робота".into(),
            },
            Command::Distance { steps: -2 },
            Command::NoLimits { on: true },
            Command::RenameProfile {
                profile: "2".into(),
                name: "Робота".into(),
            },
        ] {
            let asked = Asked::Known(c);
            assert_eq!(
                decode::<Asked<Command>>(&encode(&asked).unwrap()).unwrap(),
                asked
            );
        }
        for o in [
            Outcome::Done,
            Outcome::Added { id: DisplayId(7) },
            Outcome::Ids {
                ids: vec![DisplayId(1), DisplayId(9)],
            },
            Outcome::Saved {
                path: "~/Pictures/a.png".into(),
            },
            Outcome::Pointer {
                id: DisplayId(2),
                u: 0,
                v: 65535,
            },
            Outcome::failed(ErrorCode::LIMIT, "6"),
        ] {
            assert_eq!(decode::<Outcome>(&encode(&o).unwrap()).unwrap(), o);
        }
        // An outcome is strict: a new one comes only with a new kind of request.
        assert!(decode::<Outcome>(&[6]).is_err());
        let long = Outcome::failed(ErrorCode::OTHER, &"ї".repeat(MAX_DETAIL));
        assert!(encode(&long).is_ok(), "a long detail is cut to fit");
    }
}
