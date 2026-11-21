//! The numbers of things in the headset, in one place: which PC ([`PcId`]), which of
//! its displays ([`DisplayKey`]), what stands in the room for a hand to point at or
//! take ([`Thing`]), and where a ray meets one ([`Target`]).
//!
//! A display's number alone is the PC's: two PCs may both have a display 3. Everything
//! the headset keeps of a display (its place, its stream, the files) goes by the PC and
//! the number together, so a second PC is another key, not a collision.

#![forbid(unsafe_code)]

use std::fmt;

use prostir_proto::state::{DisplayId, ReqId};

use crate::layout::Hit;

/// A PC, by its key: the first 8 bytes of the SHA-256 of its public key (see
/// `prostir_proto::secure::key_id`). The same between runs; never on the wire, the
/// network thread knows the key from the handshake.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PcId(pub [u8; 8]);

impl PcId {
    /// The PC whose public key is `key`.
    pub fn of(key: &[u8; 32]) -> Self {
        Self(prostir_proto::secure::key_id(key))
    }

    /// As the files write it: 16 hex digits (see the `Display` impl).
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 16 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 8];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Display for PcId {
    /// 16 lowercase hex digits.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for PcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PcId({self})")
    }
}

/// A display of a PC: a virtual monitor, a window panel or a display of the PC itself.
/// Ordered by the PC, then by its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayKey {
    pub pc: PcId,
    pub id: DisplayId,
}

impl DisplayKey {
    pub fn new(pc: PcId, id: DisplayId) -> Self {
        Self { pc, id }
    }

    /// As the files write it: `<PcId>:<number>` (see the `Display` impl).
    pub fn parse(text: &str) -> Option<Self> {
        let (pc, id) = text.split_once(':')?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            pc: PcId::parse(pc)?,
            id: DisplayId(id.parse().ok()?),
        })
    }
}

impl fmt::Display for DisplayKey {
    /// `<PcId>:<number>`, `0123456789abcdef:3`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.pc, self.id.0)
    }
}

/// What stands in the room and a hand can point at or take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Thing {
    /// A display of a PC.
    Display(DisplayKey),
    /// The placeholder of a monitor being added: where it will appear, until its
    /// picture comes (the request to its PC).
    Adding { pc: PcId, req: ReqId },
    /// The menu.
    Menu,
    /// The round button that opens the menu.
    MenuButton,
    /// The window onto the real keyboard.
    Keyboard,
    /// The menu by the wrist.
    Wrist,
    /// "Attach" under a detached window: back onto the monitors.
    Attach(DisplayKey),
}

impl Thing {
    /// The display it is, if it is one.
    pub fn display(self) -> Option<DisplayKey> {
        match self {
            Self::Display(k) => Some(k),
            _ => None,
        }
    }
}

/// Where a ray meets a thing: the thing, and the hit on it.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub thing: Thing,
    pub hit: Hit,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written and read back, a PC and a display are the same; what is not one of them
    /// is not read as one.
    #[test]
    fn text_roundtrip() {
        let pc = PcId([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        assert_eq!(pc.to_string(), "0123456789abcdef");
        assert_eq!(PcId::parse("0123456789abcdef"), Some(pc));
        assert_eq!(PcId::parse("0123456789ABCDEF"), Some(pc));
        let key = DisplayKey::new(pc, DisplayId(4_000_000_000));
        assert_eq!(key.to_string(), "0123456789abcdef:4000000000");
        assert_eq!(DisplayKey::parse(&key.to_string()), Some(key));
        for bad in [
            "",
            "3",
            ":3",
            "0123456789abcdef",
            "0123456789abcdef:",
            "0123456789abcdef:-1",
            "0123456789abcdef:x",
            "0123456789abcde:3",
            "0123456789abcdef0:3",
            "0123456789abcdeg:3",
            "+123456789abcdef:3",
            "0123456789abcdef:3:4",
            "0123456789abcdef:+3",
        ] {
            assert_eq!(DisplayKey::parse(bad), None, "{bad:?}");
        }
    }

    /// A PC's number comes from its key, the same every time; two PCs with a display of
    /// the same number are two displays.
    #[test]
    fn displays_of_two_pcs_differ() {
        let (a, b) = (PcId::of(&[1; 32]), PcId::of(&[2; 32]));
        assert_eq!(a, PcId::of(&[1; 32]));
        assert_ne!(a, b);
        let three = DisplayId(3);
        assert_ne!(DisplayKey::new(a, three), DisplayKey::new(b, three));
        assert_eq!(
            Thing::Display(DisplayKey::new(a, three))
                .display()
                .map(|k| k.pc),
            Some(a)
        );
        assert_eq!(Thing::Menu.display(), None);
    }

    /// One PC's displays sort by their numbers, as the PC gives them.
    #[test]
    fn a_pcs_displays_in_their_order() {
        let pc = PcId([9; 8]);
        let mut keys: Vec<DisplayKey> = [12, 3, 100, 7]
            .into_iter()
            .map(|n| DisplayKey::new(pc, DisplayId(n)))
            .collect();
        keys.sort_unstable();
        let numbers: Vec<u32> = keys.iter().map(|k| k.id.0).collect();
        assert_eq!(numbers, [3, 7, 12, 100]);
    }
}
