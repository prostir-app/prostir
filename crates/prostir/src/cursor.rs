//! The mouse pointer apart from the pictures (see `PROTOCOL.md`, "The mouse pointer"):
//! what the router keeps of it and of what each headset was sent.
//!
//! The desktop tells how the pointer looks and where it is ([`CursorEvent`], through
//! a [`crate::platform::CursorSource`]); the router keeps the latest of it
//! ([`Pointer`]) and tells each headset in its own terms: the display and the point
//! across its picture, and a look it has ([`Told`]). The pictures leave the pointer
//! out only while every headset with a session draws it ([`apart_for`]).

use std::collections::VecDeque;

use log::warn;
use prostir_proto::state::{DisplayId, HeadsetCaps, caps};
use prostir_proto::{CURSOR_SHAPES, HostMsg};

use crate::platform::{CursorEvent, CursorImage, OutputKey};

/// Whether the pictures leave the pointer out for the headsets with a session
/// (`headsets`): only when there is one and every one draws it itself (the picture is
/// one for all of them).
pub fn apart_for<'a>(headsets: impl IntoIterator<Item = &'a HeadsetCaps>) -> bool {
    let mut any = false;
    for h in headsets {
        if !h.has(caps::headset::CURSOR) {
            return false;
        }
        any = true;
    }
    any
}

/// The number of a look: a hash of its size, point and pixels (FNV-1a), never 0 (which
/// means hidden), so the same picture has the same number all session long.
pub fn shape_id(image: &CursorImage) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    let head = [image.w, image.h, image.hot_x, image.hot_y].map(u16::to_le_bytes);
    for b in head.iter().flatten().chain(&image.rgba) {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash.max(1)
}

/// The point at `x`, `y` of a picture `w`x`h` pixels as `u`, `v` across it (0..=65535
/// from the top left): the reverse of how `MouseTo` finds a pixel (`u * (w - 1) /
/// 65535`, rounded down), so the headset puts the pointer where the PC has it and a
/// `MouseTo` to it lands on the same pixel. A point off the picture is at its edge.
pub fn uv_of(x: i32, y: i32, (w, h): (u32, u32)) -> (u16, u16) {
    let across = |p: i32, len: u32| {
        let last = f64::from(len.max(2) - 1);
        // Up: the pixel found back, rounded down, is this one.
        (f64::from(p).clamp(0.0, last) * 65535.0 / last)
            .ceil()
            .min(65535.0) as u16
    };
    (across(x, w), across(y, h))
}

/// The pointer as the desktop said it last.
#[derive(Debug, Default)]
pub struct Pointer {
    /// Its look: its number and the message that carries it.
    shape: Option<(u32, HostMsg)>,
    hidden: bool,
    /// The output it is on and its point there, in that output's pixels.
    at: Option<(OutputKey, i32, i32)>,
}

impl Pointer {
    /// Takes what the desktop said; whether anything changed. A look the protocol does
    /// not allow (too large) hides the pointer.
    pub fn take(&mut self, event: CursorEvent) -> bool {
        match event {
            CursorEvent::Shape(image) => {
                let id = shape_id(&image);
                let shown = std::mem::replace(&mut self.hidden, false);
                if self.shape.as_ref().is_some_and(|(s, _)| *s == id) {
                    return shown;
                }
                let CursorImage {
                    w,
                    h,
                    hot_x,
                    hot_y,
                    rgba,
                } = image;
                if let Err(e) =
                    prostir_proto::check_cursor_shape(id, (w, h), (hot_x, hot_y), rgba.len())
                {
                    warn!("a look of the pointer that cannot be sent ({w}x{h}): {e}");
                    self.shape = None;
                    return true;
                }
                let msg = HostMsg::CursorShape {
                    shape: id,
                    w,
                    h,
                    hot_x,
                    hot_y,
                    rgba,
                };
                self.shape = Some((id, msg));
                true
            }
            CursorEvent::Moved { output, x, y } => {
                let same = self
                    .at
                    .as_ref()
                    .is_some_and(|(o, ax, ay)| *o == output && (*ax, *ay) == (x, y));
                if !same {
                    self.at = Some((output, x, y));
                }
                !same
            }
            CursorEvent::Hidden => !std::mem::replace(&mut self.hidden, true),
        }
    }

    /// Forgets everything: the desktop says it anew once it is asked again.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The number of the look shown, 0 when hidden or not known.
    pub fn shape(&self) -> u32 {
        match &self.shape {
            Some((id, _)) if !self.hidden => *id,
            _ => 0,
        }
    }

    /// The message carrying the look shown.
    pub fn shape_msg(&self) -> Option<&HostMsg> {
        self.shape.as_ref().filter(|_| !self.hidden).map(|(_, m)| m)
    }

    /// The output it is on, and its point there.
    pub fn at(&self) -> Option<(&OutputKey, i32, i32)> {
        self.at.as_ref().map(|(o, x, y)| (o, *x, *y))
    }
}

/// Where the pointer is, as one headset is told (`CursorAt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct At {
    pub display: Option<DisplayId>,
    pub u: u16,
    pub v: u16,
    pub shape: u32,
}

impl At {
    /// Not on its displays, hidden: what a headset is told as the pointer goes back
    /// into the pictures.
    pub const NOWHERE: Self = Self {
        display: None,
        u: 0,
        v: 0,
        shape: 0,
    };

    pub fn msg(self) -> HostMsg {
        HostMsg::CursorAt {
            display: self.display,
            u: self.u,
            v: self.v,
            shape: self.shape,
        }
    }
}

/// What one headset was sent of the pointer: the looks it keeps, as it counts them
/// (the [`CURSOR_SHAPES`] that came last), and the place it was told last.
#[derive(Debug, Default)]
pub struct Told {
    shapes: VecDeque<u32>,
    last: Option<At>,
}

impl Told {
    /// Whether look `shape` has to go to the headset first (it never had it, or may
    /// have let it go); then it counts as sent.
    pub fn needs(&mut self, shape: u32) -> bool {
        if shape == 0 || self.shapes.contains(&shape) {
            return false;
        }
        if self.shapes.len() >= CURSOR_SHAPES {
            self.shapes.pop_front();
        }
        self.shapes.push_back(shape);
        true
    }

    /// Whether `at` is news to the headset; then it counts as told.
    pub fn news(&mut self, at: At) -> bool {
        if self.last == Some(at) {
            return false;
        }
        self.last = Some(at);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u16, h: u16, fill: u8) -> CursorImage {
        CursorImage {
            w,
            h,
            hot_x: 1,
            hot_y: 2,
            rgba: vec![fill; usize::from(w) * usize::from(h) * 4],
        }
    }

    /// The same picture is the same number; another picture, point or size is another.
    #[test]
    fn a_look_is_numbered_by_its_picture() {
        let a = image(32, 32, 7);
        assert_eq!(shape_id(&a), shape_id(&a.clone()));
        assert_ne!(shape_id(&a), 0);
        assert_ne!(shape_id(&a), shape_id(&image(32, 32, 8)));
        assert_ne!(
            shape_id(&a),
            shape_id(&CursorImage {
                hot_x: 2,
                ..a.clone()
            })
        );
        assert_ne!(shape_id(&a), shape_id(&image(16, 64, 7)));
    }

    /// The point across a picture is the reverse of where `MouseTo` puts the mouse,
    /// for pictures of every shape (a monitor turned, scaled, small): each pixel comes
    /// back as itself.
    #[test]
    fn a_point_across_the_picture_is_the_reverse_of_mouse_to() {
        // As the worker's `mouse_to` finds the pixel (`at`).
        let pixel = |f: u16, len: u32| (f64::from(len - 1) * f64::from(f) / 65535.0) as i32;
        for (w, h) in [
            (2560, 1440),
            (1440, 2560),
            (3840, 2160),
            (2160, 3840),
            (1707, 960),
            (640, 360),
            (272, 480),
            (2, 2),
        ] {
            for (x, y) in [
                (0, 0),
                (w as i32 - 1, h as i32 - 1),
                (w as i32 / 2, h as i32 / 3),
                (1, h as i32 - 2),
            ] {
                let (u, v) = uv_of(x, y, (w, h));
                assert_eq!((pixel(u, w), pixel(v, h)), (x, y), "{w}x{h} at {x},{y}");
            }
        }
        // Off the picture: at its edge.
        assert_eq!(uv_of(-5, 4000, (640, 360)), (0, 65535));
    }

    /// A look goes to a headset once, and again only once the headset may have let it
    /// go: after 64 others came.
    #[test]
    fn a_look_goes_again_once_the_headset_may_have_let_it_go() {
        let mut told = Told::default();
        assert!(told.needs(1));
        assert!(!told.needs(1));
        assert!(!told.needs(0), "hidden needs no look");
        for n in 2..=CURSOR_SHAPES as u32 {
            assert!(told.needs(n));
        }
        // 64 kept: the first still there, used or not.
        assert!(!told.needs(1));
        assert!(told.needs(1000), "the 65th: the first goes");
        assert!(told.needs(1), "sent again");
        assert!(!told.needs(1000));
    }

    /// A place is news once.
    #[test]
    fn a_place_is_told_once() {
        let mut told = Told::default();
        let at = At {
            display: Some(DisplayId(1)),
            u: 3,
            v: 4,
            shape: 9,
        };
        assert!(told.news(at));
        assert!(!told.news(at));
        assert!(told.news(At::NOWHERE));
        assert!(!told.news(At::NOWHERE));
    }

    /// The pointer as the desktop says it: a look shows it, hidden until the next
    /// look; a look too large hides it.
    #[test]
    fn the_pointer_follows_the_desktop() {
        let mut p = Pointer::default();
        assert_eq!(p.shape(), 0);
        let a = image(32, 32, 1);
        assert!(p.take(CursorEvent::Shape(a.clone())));
        assert_eq!(p.shape(), shape_id(&a));
        assert!(!p.take(CursorEvent::Shape(a.clone())), "the same look");
        assert!(p.take(CursorEvent::Hidden));
        assert_eq!(p.shape(), 0);
        assert!(p.shape_msg().is_none());
        assert!(p.take(CursorEvent::Shape(a.clone())), "shown again");
        assert_eq!(p.shape(), shape_id(&a));
        let moved = |x| CursorEvent::Moved {
            output: OutputKey::new("Fake-1"),
            x,
            y: 2,
        };
        assert!(p.take(moved(1)));
        assert!(!p.take(moved(1)));
        assert_eq!(p.at(), Some((&OutputKey::new("Fake-1"), 1, 2)));
        assert!(p.take(CursorEvent::Shape(image(300, 2, 1))));
        assert_eq!(p.shape(), 0, "too large to send");
        p.clear();
        assert_eq!(p.at(), None);
    }

    #[test]
    fn the_pointer_is_apart_only_where_every_headset_draws_it() {
        let draws = HeadsetCaps {
            features: vec![caps::headset::CURSOR],
            ..HeadsetCaps::default()
        };
        let plain = HeadsetCaps::default();
        assert!(apart_for([&draws]));
        assert!(apart_for([&draws, &draws]));
        assert!(!apart_for([&draws, &plain]));
        assert!(!apart_for([&plain]));
        assert!(!apart_for([]));
    }
}
