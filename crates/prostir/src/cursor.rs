//! The mouse pointer apart from the pictures (see `PROTOCOL.md`, "The mouse pointer"):
//! what the router keeps of it and of what each headset was sent.
//!
//! The desktop tells how the pointer looks and where it is ([`CursorEvent`], through
//! a [`crate::platform::CursorSource`]); the router keeps the latest of it
//! ([`Pointer`]) and tells each headset in its own terms: the display and the point
//! across its picture, and a look it has ([`Told`]). The pictures leave the pointer
//! out only while every headset with a session draws it ([`apart_for`]).
//!
//! Real touches from a headset hide it, as a touch screen does ([`Pointing`]): from a
//! finger down until the mouse moves again, the headsets hear `shape` 0.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use log::warn;
use prostir_proto::state::{DisplayId, HeadsetCaps, caps};
use prostir_proto::{CURSOR_SHAPES, HostMsg};

use crate::input::Pointing;
use crate::platform::{CursorEvent, CursorImage, OutputKey};

/// A move the desktop tells this soon after the last finger lifted is the touch's own
/// (told late, or the desktop putting the pointer where the finger was), not the
/// mouse's: the pointer stays hidden.
const LIFT_SETTLES: Duration = Duration::from_millis(300);

/// Whether touches hide the pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ByTouch {
    #[default]
    No,
    /// A finger touches.
    Touching,
    /// The last finger lifted then: hidden until the mouse moves.
    Lifted(Instant),
}

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

/// The point `f` across (0..=65535) an output at `start`, `len` pixels long, in the
/// desktop's pixels: the middle of its first pixel to the middle of its last, so 65535
/// is the very edge pixel (where the desktop's screen edge is) and the pixel found back
/// (rounded down) is the one [`uv_of`] says.
pub fn point_of(start: i32, len: u32, f: u16) -> f64 {
    f64::from(start) + f64::from(len.saturating_sub(1)) * f64::from(f) / 65535.0
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

/// `image` scaled down by `fx` across and `fy` down (each at most 1), as the picture it
/// is drawn over was scaled down to stream: each pixel the average of those it covers,
/// weighed by their alpha (so a clear pixel's colour does not bleed in), its point
/// where it was.
pub fn scaled(image: &CursorImage, fx: f64, fy: f64) -> CursorImage {
    let size = |n: u16, f: f64| ((f64::from(n) * f).round() as u16).clamp(1, n);
    let (w, h) = (size(image.w, fx), size(image.h, fy));
    let (sw, sh) = (usize::from(image.w), usize::from(image.h));
    let (dw, dh) = (usize::from(w), usize::from(h));
    let mut rgba = Vec::with_capacity(dw * dh * 4);
    for y in 0..dh {
        let (y0, y1) = (y * sh / dh, ((y + 1) * sh).div_ceil(dh));
        for x in 0..dw {
            let (x0, x1) = (x * sw / dw, ((x + 1) * sw).div_ceil(dw));
            // Premultiplied sums, then straight again.
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &image.rgba[(sy * sw + sx) * 4..][..4];
                    let a = u32::from(p[3]);
                    for c in 0..3 {
                        sum[c] += u32::from(p[c]) * a;
                    }
                    sum[3] += a;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let a = sum[3];
            for c in &sum[..3] {
                rgba.push((c + a / 2).checked_div(a).unwrap_or(0) as u8);
            }
            rgba.push(((a + n / 2) / n) as u8);
        }
    }
    // The pixel the middle of the one it pointed with falls in.
    let point = |p: u16, f: f64, n: u16| (((f64::from(p) + 0.5) * f) as u16).min(n - 1);
    CursorImage {
        w,
        h,
        hot_x: point(image.hot_x, fx, w),
        hot_y: point(image.hot_y, fy, h),
        rgba,
    }
}

/// A look as it goes to the headsets: its number and the message that carries it.
#[derive(Debug)]
struct Sent {
    id: u32,
    msg: HostMsg,
}

impl Sent {
    fn of(image: CursorImage) -> Self {
        let id = shape_id(&image);
        let CursorImage {
            w,
            h,
            hot_x,
            hot_y,
            rgba,
        } = image;
        Self {
            id,
            msg: HostMsg::CursorShape {
                shape: id,
                w,
                h,
                hot_x,
                hot_y,
                rgba,
            },
        }
    }
}

/// The pointer as the desktop said it last.
#[derive(Debug, Default)]
pub struct Pointer {
    /// Its look as the desktop made it, and as it goes out at that size.
    image: Option<(CursorImage, Sent)>,
    /// The look scaled down for the picture it is over (the scale it was made for, in
    /// thousandths across and down), made once for each such picture it goes onto.
    scaled: Option<((u32, u32), Sent)>,
    hidden: bool,
    /// The output it is on, its point there, and that output's picture, in pixels as
    /// the desktop makes it.
    at: Option<(OutputKey, i32, i32, (u32, u32))>,
    /// Hidden by touches (the input's, not the desktop's: kept by [`Pointer::clear`]).
    by_touch: ByTouch,
}

impl Pointer {
    /// Takes what the desktop said; whether anything changed. A look the protocol does
    /// not allow (too large) hides the pointer.
    pub fn take(&mut self, event: CursorEvent) -> bool {
        match event {
            CursorEvent::Shape(image) => {
                let shown = std::mem::replace(&mut self.hidden, false);
                if self.image.as_ref().is_some_and(|(i, _)| *i == image) {
                    return shown;
                }
                self.scaled = None;
                let (w, h, hot) = (image.w, image.h, (image.hot_x, image.hot_y));
                if let Err(e) = prostir_proto::check_cursor_shape(1, (w, h), hot, image.rgba.len())
                {
                    warn!("a look of the pointer that cannot be sent ({w}x{h}): {e}");
                    self.image = None;
                    return true;
                }
                let sent = Sent::of(image.clone());
                self.image = Some((image, sent));
                true
            }
            CursorEvent::Moved { output, x, y, size } => {
                let same = self
                    .at
                    .as_ref()
                    .is_some_and(|(o, ax, ay, s)| *o == output && (*ax, *ay, *s) == (x, y, size));
                if !same {
                    self.at = Some((output, x, y, size));
                }
                !same
            }
            CursorEvent::Hidden => !std::mem::replace(&mut self.hidden, true),
            CursorEvent::Left { output } => self.at.take_if(|(o, _, _, _)| *o == output).is_some(),
        }
    }

    /// Forgets everything the desktop said: it says it anew once it is asked again.
    pub fn clear(&mut self) {
        *self = Self {
            by_touch: self.by_touch,
            ..Self::default()
        };
    }

    /// What the input did at `now` (see [`Pointing`]); whether that hides or shows
    /// the pointer.
    pub fn touch(&mut self, p: Pointing, now: Instant) -> bool {
        let was = self.by_touch != ByTouch::No;
        self.by_touch = match (p, self.by_touch) {
            (Pointing::Touch, _) => ByTouch::Touching,
            (Pointing::Lifted, ByTouch::No) => ByTouch::No,
            (Pointing::Lifted, _) => ByTouch::Lifted(now),
            (Pointing::Mouse, _) => ByTouch::No,
        };
        was != (self.by_touch != ByTouch::No)
    }

    /// The desktop told a move of the pointer at `now` (see [`Pointer::take`]): once
    /// the fingers lifted a moment ago, the mouse moved it, and it shows again. A move
    /// told after a [`Pointer::clear`] or a change of the output's size counts too
    /// (the place differs from the one kept), so the pointer may come back then
    /// without the mouse: the safe way, never hidden for good.
    pub fn desktop_moved(&mut self, now: Instant) {
        if let ByTouch::Lifted(at) = self.by_touch
            && now.saturating_duration_since(at) >= LIFT_SETTLES
        {
            self.by_touch = ByTouch::No;
        }
    }

    /// Hidden by touches.
    pub fn touch_hides(&self) -> bool {
        self.by_touch != ByTouch::No
    }

    /// The look shown over a picture streamed at `stream` pixels (none: over none), its
    /// number and message: scaled down as that picture is. None when hidden (by the
    /// desktop, by touches) or not known.
    pub fn look(&mut self, stream: Option<(u32, u32)>) -> Option<(u32, &HostMsg)> {
        if self.hidden || self.touch_hides() {
            return None;
        }
        let native = self.at.as_ref().map(|(_, _, _, size)| *size);
        let thousandths = |s: u32, n: u32| (u64::from(s) * 1000 / u64::from(n.max(1))) as u32;
        let scale = stream
            .zip(native)
            .map(|((sw, sh), (nw, nh))| (thousandths(sw, nw), thousandths(sh, nh)))
            .filter(|&(fx, fy)| fx < 1000 || fy < 1000);
        let (image, full) = self.image.as_ref()?;
        let Some((fx, fy)) = scale else {
            return Some((full.id, &full.msg));
        };
        if self.scaled.as_ref().is_none_or(|(s, _)| *s != (fx, fy)) {
            let f = |t: u32| f64::from(t.min(1000)) / 1000.0;
            self.scaled = Some(((fx, fy), Sent::of(scaled(image, f(fx), f(fy)))));
        }
        self.scaled.as_ref().map(|(_, s)| (s.id, &s.msg))
    }

    /// The output it is on, its point there, and that output's picture in pixels.
    pub fn at(&self) -> Option<(&OutputKey, i32, i32, (u32, u32))> {
        self.at.as_ref().map(|(o, x, y, s)| (o, *x, *y, *s))
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

    /// Whether the headset was told the pointer is somewhere (not nowhere) since it was
    /// last told it is nowhere.
    pub fn told(&self) -> bool {
        self.last.is_some_and(|a| a != At::NOWHERE)
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

    /// The headset's 0 and 65535 are the very first and last pixel of the output on the
    /// desktop (where its screen edges are, KWin's on the pixel rounded to), each pixel
    /// `uv_of` says comes back as itself, and a point a hair off the edge still
    /// rounds onto it.
    #[test]
    fn the_ends_across_an_output_are_its_edge_pixels() {
        // HDMI-A-1 of a desktop, below a monitor of Prostir.
        let (x, w, y, h) = (2320, 1920, 2560, 1080);
        assert_eq!(point_of(x, w, 0), 2320.0);
        assert_eq!(point_of(x, w, 65535), 4239.0);
        assert_eq!(point_of(y, h, 65535), 3639.0);
        assert_eq!(point_of(y, h, 65534).round(), 3639.0);
        for py in [0, 1, 539, 1078, 1079] {
            let (_, v) = uv_of(0, py, (w, h));
            assert_eq!(point_of(y, h, v).floor() as i32, y + py, "row {py}");
        }
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
        let shape = |p: &mut Pointer| p.look(None).map_or(0, |(id, _)| id);
        assert_eq!(shape(&mut p), 0);
        let a = image(32, 32, 1);
        assert!(p.take(CursorEvent::Shape(a.clone())));
        assert_eq!(shape(&mut p), shape_id(&a));
        assert!(!p.take(CursorEvent::Shape(a.clone())), "the same look");
        assert!(p.take(CursorEvent::Hidden));
        assert_eq!(shape(&mut p), 0);
        assert!(p.take(CursorEvent::Shape(a.clone())), "shown again");
        assert_eq!(shape(&mut p), shape_id(&a));
        let moved = |x| CursorEvent::Moved {
            output: OutputKey::new("Fake-1"),
            x,
            y: 2,
            size: (640, 360),
        };
        assert!(p.take(moved(1)));
        assert!(!p.take(moved(1)));
        assert_eq!(p.at(), Some((&OutputKey::new("Fake-1"), 1, 2, (640, 360))));
        assert!(p.take(CursorEvent::Shape(image(300, 2, 1))));
        assert_eq!(shape(&mut p), 0, "too large to send");
        p.clear();
        assert_eq!(p.at(), None);
    }

    /// Touches hide the pointer from a finger down until the mouse moves: the input's
    /// own move, or one the desktop tells once the lift settled; a move told right
    /// after the lift is the touch's own. Forgetting what the desktop said keeps it.
    #[test]
    fn touches_hide_the_pointer_until_the_mouse_moves() {
        let t0 = Instant::now();
        let mut p = Pointer::default();
        let shape = |p: &mut Pointer| p.look(None).map_or(0, |(id, _)| id);
        let a = image(32, 32, 1);
        p.take(CursorEvent::Shape(a.clone()));
        assert!(!p.touch(Pointing::Lifted, t0), "nothing touched");
        assert!(!p.touch(Pointing::Mouse, t0));
        assert!(p.touch(Pointing::Touch, t0), "hidden");
        assert_eq!(shape(&mut p), 0);
        p.desktop_moved(t0 + LIFT_SETTLES * 2);
        assert_eq!(shape(&mut p), 0, "moved while the finger touches");
        let lifted = t0 + Duration::from_secs(1);
        assert!(!p.touch(Pointing::Lifted, lifted), "still hidden");
        p.desktop_moved(lifted + LIFT_SETTLES / 2);
        assert_eq!(shape(&mut p), 0, "the touch's own move");
        p.clear();
        p.take(CursorEvent::Shape(a.clone()));
        assert_eq!(shape(&mut p), 0, "kept over a clear");
        p.desktop_moved(lifted + LIFT_SETTLES);
        assert_eq!(shape(&mut p), shape_id(&a), "the mouse moved it");
        assert!(p.touch(Pointing::Touch, lifted));
        assert!(p.touch(Pointing::Mouse, lifted), "the input's mouse");
        assert_eq!(shape(&mut p), shape_id(&a));
    }

    /// Over a picture scaled down to stream (a 4K monitor sent as 1920x1080), the look
    /// is scaled with it, once, its point where it was: as large in the headset as on
    /// the PC. At full size it goes as it is.
    #[test]
    fn a_look_is_scaled_with_its_picture() {
        let mut p = Pointer::default();
        let a = CursorImage {
            w: 48,
            h: 48,
            hot_x: 10,
            hot_y: 47,
            ..image(48, 48, 200)
        };
        p.take(CursorEvent::Shape(a.clone()));
        p.take(CursorEvent::Moved {
            output: OutputKey::new("Fake-1"),
            x: 0,
            y: 0,
            size: (3840, 2160),
        });
        let full = p.look(Some((3840, 2160))).map(|(id, _)| id);
        assert_eq!(full, Some(shape_id(&a)));
        let (id, msg) = p.look(Some((1920, 1080))).expect("shown");
        let HostMsg::CursorShape {
            shape,
            w,
            h,
            hot_x,
            hot_y,
            rgba,
        } = msg.clone()
        else {
            panic!("a look");
        };
        assert_eq!((shape, w, h, hot_x, hot_y), (id, 24, 24, 5, 23));
        assert_eq!(rgba[..4], [200, 200, 200, 200]);
        assert_ne!(id, shape_id(&a));
        // Made once for that scale.
        assert_eq!(p.look(Some((1920, 1080))).map(|(id, _)| id), Some(id));
    }

    /// Scaling down averages what each pixel covers by alpha: a clear pixel's colour
    /// does not bleed into its opaque neighbour.
    #[test]
    fn scaling_down_weighs_by_alpha() {
        let image = CursorImage {
            w: 2,
            h: 1,
            hot_x: 1,
            hot_y: 0,
            rgba: vec![255, 0, 0, 0, 0, 0, 255, 255],
        };
        let small = scaled(&image, 0.5, 1.0);
        assert_eq!((small.w, small.h, small.hot_x), (1, 1, 0));
        assert_eq!(small.rgba, [0, 0, 255, 128]);
        // The point: the pixel the middle of the old one falls in (1.5 of 4 is 1.125
        // of 3), not its left edge (0.75).
        let wide = CursorImage {
            w: 4,
            h: 1,
            hot_x: 1,
            hot_y: 0,
            rgba: vec![255; 16],
        };
        assert_eq!(scaled(&wide, 0.75, 1.0).hot_x, 1);
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
