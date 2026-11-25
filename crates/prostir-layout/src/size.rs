//! How large a new monitor is made for its stream's pixels: the same metres per pixel
//! for every monitor (the size before 0.2.0, [`Size::STANDARD`]), or as large as the
//! headset shows it at a scale, its pixels seen at so many of the headset's pixels at
//! its middle ([`Size::Seen`]: at scale 1 one pixel of the stream is one of the
//! headset's, "1:1"), or as large within limits ([`Size::Bounded`]: never smaller than
//! the standard size, nor wider than [`WIDEST`]).
//!
//! A flat monitor `n` pixels wide, `m` metres per pixel, with both vertical edges on the
//! circle of radius `R` around the eye has its middle `d = sqrt(R² - (n m / 2)²)` from
//! the eye, and a pixel there is seen `m / d` radians wide. On a headset of `p` pixels
//! per degree a pixel of the headset is `t = π / (180 p)` radians; seen at `s` of them
//! per pixel, `m / d = s t`, so `m = s t R / sqrt(1 + (s t n / 2)²)`. A wider monitor
//! comes nearer the eye, so it gets fewer metres per pixel for the same scale.

use crate::space::{METRES_PER_PIXEL, PixelSize};

/// How large a monitor is made for its pixels (see the module).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    /// The same metres per pixel for every monitor: neighbours line up like one big
    /// screen.
    Fixed(f32),
    /// Seen at `scale` of the headset's pixels per pixel of the stream at its middle,
    /// on a headset of `ppd` pixels per degree, its edges on the arrangement's circle.
    Seen { ppd: f32, scale: f32 },
    /// As [`Size::Seen`], but each monitor within its own limits (see [`bounds`]): never
    /// shown smaller than the standard size makes it (up to 1:1: past that it is no
    /// sharper, only wider), and no wider than [`WIDEST`] unless that is. What the
    /// presets make at "1:1".
    Bounded { ppd: f32, scale: f32 },
}

/// The widest a monitor of [`Size::Bounded`] is made, as the eye sees it with its edges
/// on the circle: 90 degrees, seen whole without turning the head.
pub const WIDEST: f32 = std::f32::consts::FRAC_PI_2;

/// The least and the most scale a monitor `width` pixels wide whose edges stand
/// `radius` from the eye is made at by [`Size::Bounded`] on a headset of `ppd` pixels
/// per degree: the scale the standard size shows it at (at most 1), and the one at
/// which it is [`WIDEST`] wide. The least may be the larger: then it is the one.
pub fn bounds(ppd: f32, width: u32, radius: f32) -> (f32, f32) {
    let n = width.max(1);
    let standard = METRES_PER_PIXEL * n as f32;
    let least = if standard / 2.0 < radius {
        let d = (radius * radius - standard * standard / 4.0).sqrt();
        shown_scale(standard, n, d, ppd).min(1.0)
    } else {
        1.0
    };
    let widest = 2.0 * radius * (WIDEST / 2.0).sin();
    let most = shown_scale(widest, n, radius * (WIDEST / 2.0).cos(), ppd);
    (least, most)
}

impl Size {
    /// The size before 0.2.0, and on a headset whose pixels are not known: 2560 pixels
    /// are 1.25 m.
    pub const STANDARD: Size = Size::Fixed(METRES_PER_PIXEL);

    /// Metres per pixel of a stream `width` pixels wide whose edges stand `radius` from
    /// the eye.
    pub fn metres_per_pixel(self, width: u32, radius: f32) -> f32 {
        match self {
            Size::Fixed(m) => m,
            Size::Seen { ppd, scale } => {
                let t = scale * std::f32::consts::PI / (180.0 * ppd.max(1.0));
                let half = t * width.max(1) as f32 / 2.0;
                t * radius / (1.0 + half * half).sqrt()
            }
            Size::Bounded { ppd, scale } => {
                let (least, most) = bounds(ppd, width, radius);
                let scale = scale.min(most).max(least);
                Size::Seen { ppd, scale }.metres_per_pixel(width, radius)
            }
        }
    }

    /// Width and height in metres of a stream of `px` whose edges stand `radius` from
    /// the eye.
    pub fn metres(self, px: PixelSize, radius: f32) -> (f32, f32) {
        let m = self.metres_per_pixel(px.w, radius);
        (px.w as f32 * m, px.h as f32 * m)
    }

    /// The width of a new monitor of `px` next to one `width` metres wide whose stream
    /// is `beside` pixels wide: as much larger or smaller than its own size as that one
    /// is than its own (a monitor resized by hand gives its neighbour the same). With
    /// one size for all, the neighbour's metres per pixel, as before.
    pub fn beside(self, px: PixelSize, beside: u32, width: f32, radius: f32) -> f32 {
        match self {
            Size::Fixed(_) => px.w as f32 * width / beside.max(1) as f32,
            Size::Seen { .. } | Size::Bounded { .. } => {
                let own = |w: u32| w.max(1) as f32 * self.metres_per_pixel(w, radius);
                width * own(px.w) / own(beside)
            }
        }
    }
}

impl Default for Size {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// The sizes an arrangement makes its monitors: the main (primary) one's and the
/// others'.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sizes {
    pub main: Size,
    pub others: Size,
}

impl Sizes {
    /// Every monitor of `size`.
    pub const fn all(size: Size) -> Self {
        Self {
            main: size,
            others: size,
        }
    }

    /// The size of a monitor, the main one or not.
    pub fn of(self, main: bool) -> Size {
        if main { self.main } else { self.others }
    }
}

/// How many of the headset's pixels one pixel of a stream `width` pixels wide takes at
/// the middle of a monitor `metres` wide (its picture laid out that wide), seen from
/// `distance` metres, on a headset of `ppd` pixels per degree.
pub fn shown_scale(metres: f32, width: u32, distance: f32, ppd: f32) -> f32 {
    let pixel = metres / width.max(1) as f32 / distance.max(1e-3);
    pixel * 180.0 / std::f32::consts::PI * ppd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circle::RADIUS;

    fn seen(ppd: f32, scale: f32) -> Size {
        Size::Seen { ppd, scale }
    }

    /// The table of `plans/clarity-2026-11.md`, part 1: at 1:1 on 25 pixels per degree
    /// on the circle of 1.1 m, full HD is 0.638 mm a pixel with its middle 0.91 m away,
    /// QHD 0.573 mm, an ultrawide 0.491 mm, 4K 0.459 mm, a portrait QHD 0.686 mm; full
    /// HD is 68 by 41 degrees.
    #[test]
    fn one_to_one_is_as_the_table_says() {
        for (w, mm) in [
            (1920, 0.638),
            (2560, 0.573),
            (3440, 0.491),
            (3840, 0.459),
            (1440, 0.686),
        ] {
            let m = seen(25.0, 1.0).metres_per_pixel(w, RADIUS) * 1000.0;
            assert!((m - mm).abs() < 0.002, "{w}: {m} mm");
        }
        let (w, h) = seen(25.0, 1.0).metres(PixelSize::new(1920, 1080), RADIUS);
        let d = (RADIUS * RADIUS - w * w / 4.0).sqrt();
        assert!((d - 0.91).abs() < 0.01, "{d}");
        let degrees = |x: f32| 2.0 * (x / 2.0 / d).atan().to_degrees();
        assert!((degrees(w) - 68.0).abs() < 1.0, "{}", degrees(w));
        assert!((degrees(h) - 41.0).abs() < 1.0, "{}", degrees(h));
    }

    /// The scale it is made at is the scale it is seen at, for every size and scale,
    /// and a smaller scale makes it smaller.
    #[test]
    fn seen_at_the_scale_it_was_made_for() {
        for w in [1280, 1920, 2560, 3440, 3840, 5120] {
            let mut last = 0.0;
            for scale in [0.5, 0.7, 0.85, 1.0] {
                for ppd in [20.0, 22.0, 25.0] {
                    let m = seen(ppd, scale).metres_per_pixel(w, RADIUS);
                    let width = m * w as f32;
                    let d = (RADIUS * RADIUS - width * width / 4.0).sqrt();
                    let s = shown_scale(width, w, d, ppd);
                    assert!((s - scale).abs() < 1e-4, "{w} at {scale} on {ppd}: {s}");
                }
                let m = seen(25.0, scale).metres_per_pixel(w, RADIUS);
                assert!(m > last);
                last = m;
            }
        }
    }

    /// The standard size is the old one, bit for bit; next to a monitor the new one
    /// takes its metres per pixel, as before.
    #[test]
    fn the_standard_size_is_as_before() {
        let px = PixelSize::new(2560, 1440);
        assert_eq!(
            Size::STANDARD.metres(px, 1.7),
            (2560.0 * METRES_PER_PIXEL, 1440.0 * METRES_PER_PIXEL)
        );
        assert_eq!(
            Size::STANDARD.beside(px, 1920, 0.9, RADIUS),
            2560.0 * 0.9 / 1920.0
        );
        assert_eq!(Sizes::default(), Sizes::all(Size::STANDARD));
    }

    /// Next to a monitor at its own size, a new one gets its own size; next to one made
    /// half as large again by hand, half as large again.
    #[test]
    fn beside_one_at_its_size_the_new_one_gets_its_own() {
        let size = seen(25.0, 1.0);
        let full_hd = PixelSize::new(1920, 1080);
        let (w, _) = size.metres(full_hd, RADIUS);
        for px in [PixelSize::new(2560, 1440), PixelSize::new(3840, 2160)] {
            let own = size.metres(px, RADIUS).0;
            let next = size.beside(px, 1920, w, RADIUS);
            assert!((next - own).abs() < 1e-5, "{px:?}: {next} {own}");
            let larger = size.beside(px, 1920, w * 1.5, RADIUS);
            assert!((larger - own * 1.5).abs() < 1e-5);
        }
    }

    /// Within limits: full HD on a Quest 3 1:1 as it is; QHD on a Quest 3S 90 degrees
    /// wide rather than 96 at 1:1; asked smaller than the standard size, the standard
    /// size; a 4K whose standard size is past 1:1 (and wider than 90 degrees) 1:1.
    #[test]
    fn within_limits() {
        let within = |ppd, scale| Size::Bounded { ppd, scale };
        let wide = |size: Size, w: u32| {
            let m = size.metres_per_pixel(w, RADIUS) * w as f32;
            2.0 * (m / 2.0 / RADIUS).asin().to_degrees()
        };
        assert_eq!(
            within(25.0, 1.0).metres_per_pixel(1920, RADIUS),
            seen(25.0, 1.0).metres_per_pixel(1920, RADIUS)
        );
        assert!((wide(seen(20.0, 1.0), 2560) - 96.4).abs() < 0.5);
        assert!((wide(within(20.0, 1.0), 2560) - 90.0).abs() < 1e-3);
        for w in [1440, 1920, 2560] {
            let m = within(25.0, 0.5).metres_per_pixel(w, RADIUS);
            assert!((m / METRES_PER_PIXEL - 1.0).abs() < 1e-4, "{w}: {m}");
        }
        let (least, most) = bounds(25.0, 3840, RADIUS);
        assert_eq!(least, 1.0);
        assert!(most < 0.8, "{most}");
        assert_eq!(
            within(25.0, 0.7).metres_per_pixel(3840, RADIUS),
            seen(25.0, 1.0).metres_per_pixel(3840, RADIUS)
        );
    }
}
