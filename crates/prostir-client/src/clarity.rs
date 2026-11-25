//! Text as sharp as the headset can show it (`plans/clarity-2026-11.md`): how many of
//! its pixels a degree takes on each model of headset, how large new monitors are made
//! (one pixel of the stream on one of the headset's, "1:1", where the model is known),
//! the compositor's filters for each monitor's layer by the scale it is shown at, and
//! the choices of the menu's "For development" part (in the preview build only, see
//! [`crate::PREVIEW`]).

#![forbid(unsafe_code)]

use glam::Vec3;

use crate::layout::{Cylinder, Placement, Size, Sizes, shown_scale};

/// The pixels per degree of the headset `model` (as [`crate::device::model_name`]
/// names it); `None` for a model not in the table (`platform::pixels`), which keeps the
/// standard size.
pub fn pixels_per_degree(model: &str) -> Option<f32> {
    crate::platform::pixels::per_degree(model)
}

/// Shown smaller than this (headset pixels per stream pixel), a layer is supersampled
/// as well as sharpened; from it on only sharpened: supersampling blurs a picture shown
/// about as large as the display's pixels.
pub const SUPER_SAMPLING_BELOW: f32 = 0.9;
/// How far past [`SUPER_SAMPLING_BELOW`] the scale goes before supersampling turns on
/// or off again (0.88 on, 0.92 off), so a monitor moved about that scale does not
/// flicker between the two.
pub const SUPER_SAMPLING_SLACK: f32 = 0.02;

/// How large the monitors of a preset are made (the menu's "For development").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorSize {
    /// One pixel of the stream on one of the headset's, or the largest one scale for
    /// all at which the preset keeps its circle and every monitor in view; each monitor
    /// never smaller than the standard size and no wider than 90 degrees, a row of them
    /// at one scale (see `layout::Size::Bounded`).
    #[default]
    OneToOne,
    /// The size before 0.2.0 (2560 pixels are 1.25 m).
    Standard,
    /// The main monitor 1:1 (or as large as fits), the others standard.
    MainOnly,
    /// Every monitor at 0.85 of the headset's pixels.
    Scale85,
}

/// A compositor filter for the monitors' layers: by the scale ([`Filter::Auto`]), or
/// always on or off (the menu's "For development").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Auto,
    On,
    Off,
}

impl Filter {
    fn on(self, auto: bool) -> bool {
        match self {
            Filter::Auto => auto,
            Filter::On => true,
            Filter::Off => false,
        }
    }
}

/// The choices of "For development" (in memory only: a test of the moment, never kept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dev {
    pub size: MonitorSize,
    pub super_sampling: Filter,
    pub sharpening: Filter,
    /// The numbers next to the monitor looked at.
    pub metrics: bool,
}

impl Default for Dev {
    fn default() -> Self {
        Self {
            size: MonitorSize::default(),
            super_sampling: Filter::Auto,
            sharpening: Filter::Auto,
            metrics: true,
        }
    }
}

/// The compositor's filters for one monitor's layer
/// (`XR_FB_composition_layer_settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub super_sampling: bool,
    pub sharpening: bool,
}

impl Quality {
    /// Both, as every layer had them before 0.2.0.
    pub const BOTH: Quality = Quality {
        super_sampling: true,
        sharpening: true,
    };

    /// The four there are, each at its [`Quality::index`]: the shell makes the
    /// compositor's structure of each once a frame, on the stack.
    pub const ALL: [Quality; 4] = [
        Quality {
            super_sampling: false,
            sharpening: false,
        },
        Quality {
            super_sampling: false,
            sharpening: true,
        },
        Quality {
            super_sampling: true,
            sharpening: false,
        },
        Quality::BOTH,
    ];

    /// Where it is in [`Quality::ALL`].
    pub fn index(self) -> usize {
        usize::from(self.super_sampling) * 2 + usize::from(self.sharpening)
    }
}

/// The filters of a layer shown at `scale` headset pixels per stream pixel (`None`:
/// not known, the headset not in the table): below [`SUPER_SAMPLING_BELOW`]
/// supersampled and sharpened, from it on sharpened only, not known both; unless `dev`
/// says otherwise. `was`: whether it was supersampled by the scale before (`None`: it
/// was not shown): then it changes only [`SUPER_SAMPLING_SLACK`] past the line.
pub fn quality(scale: Option<f32>, dev: &Dev, was: Option<bool>) -> Quality {
    let line = match was {
        None => SUPER_SAMPLING_BELOW,
        Some(true) => SUPER_SAMPLING_BELOW + SUPER_SAMPLING_SLACK,
        Some(false) => SUPER_SAMPLING_BELOW - SUPER_SAMPLING_SLACK,
    };
    let small = scale.is_none_or(|s| s < line);
    Quality {
        super_sampling: dev.super_sampling.on(small),
        sharpening: dev.sharpening.on(true),
    }
}

/// The sizes `size` makes the monitors of a preset on a headset of `ppd` pixels per
/// degree (`None`: not known, the standard size). For 1:1 (all, or the main one) the
/// largest scale up to 1 that `largest` finds for sizes made at a scale (see
/// `layout::largest_scale`), for all within each one's limits (see
/// `layout::Size::Bounded`); none, the standard size.
pub fn preset_sizes(
    ppd: Option<f32>,
    size: MonitorSize,
    largest: impl FnOnce(&dyn Fn(f32) -> Sizes) -> Option<f32>,
) -> Sizes {
    let Some(ppd) = ppd else {
        return Sizes::default();
    };
    let seen = move |scale: f32| Size::Seen { ppd, scale };
    match size {
        MonitorSize::Standard => Sizes::default(),
        MonitorSize::Scale85 => Sizes::all(seen(0.85)),
        MonitorSize::OneToOne => {
            let at = move |k: f32| Sizes::all(Size::Bounded { ppd, scale: k });
            largest(&at).map_or_else(Sizes::default, at)
        }
        MonitorSize::MainOnly => {
            let at = move |k: f32| Sizes {
                main: seen(k),
                others: Size::STANDARD,
            };
            largest(&at).map_or_else(Sizes::default, at)
        }
    }
}

/// The sizes new monitors get before any preset laid them out this run (the
/// arrangement came from the files): as [`preset_sizes`] at 1:1.
pub fn start_sizes(ppd: Option<f32>, size: MonitorSize) -> Sizes {
    preset_sizes(ppd, size, |_| Some(1.0))
}

/// How many of the headset's pixels (of `ppd` a degree) a pixel of a stream `width`
/// pixels wide takes at the middle of a monitor placed at `p`, seen from `eye`; on a
/// cylinder of `curve` metres when curved.
pub fn shown(p: &Placement, width: u32, eye: Vec3, curve: Option<f32>, ppd: f32) -> f32 {
    match curve {
        Some(radius) => {
            let c = Cylinder::new(p, radius);
            let middle = c.center - p.rotation * Vec3::Z * c.radius;
            shown_scale(c.arc_length(), width, middle.distance(eye), ppd)
        }
        None => shown_scale(p.width, width, p.position.distance(eye), ppd),
    }
}

/// The numbers shown next to the monitor looked at, while developing.
#[cfg(feature = "preview")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Headset pixels per stream pixel at its middle (`None`: the headset's pixels are
    /// not known).
    pub scale: Option<f32>,
    /// Megabits a second of its stream over the last second (`None`: not yet).
    pub mbit: Option<f32>,
    /// The last keyframe's size, in bytes (`None`: none came yet).
    pub keyframe: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{RADIUS, facing};

    /// Below 0.9 both filters, from 0.9 sharpening only, not known both as before; the
    /// choices of "For development" over the rule.
    #[test]
    fn the_filters_by_the_scale() {
        let auto = Dev::default();
        let both = Quality::BOTH;
        let sharp = Quality {
            super_sampling: false,
            sharpening: true,
        };
        assert_eq!(quality(Some(0.7), &auto, None), both);
        assert_eq!(quality(Some(0.899), &auto, None), both);
        assert_eq!(quality(Some(0.9), &auto, None), sharp);
        assert_eq!(quality(Some(1.2), &auto, None), sharp);
        assert_eq!(quality(None, &auto, None), both);
        // Moved about the line: it changes only past 0.88 or 0.92.
        assert_eq!(quality(Some(0.91), &auto, Some(true)), both);
        assert_eq!(quality(Some(0.921), &auto, Some(true)), sharp);
        assert_eq!(quality(Some(0.89), &auto, Some(false)), sharp);
        assert_eq!(quality(Some(0.879), &auto, Some(false)), both);
        let forced = Dev {
            super_sampling: Filter::On,
            sharpening: Filter::Off,
            ..auto
        };
        assert_eq!(
            quality(Some(1.0), &forced, None),
            Quality {
                super_sampling: true,
                sharpening: false
            }
        );
        let none = Dev {
            super_sampling: Filter::Off,
            ..auto
        };
        assert_eq!(quality(Some(0.5), &none, None), sharp);
    }

    /// Each of the four is at its place in the list of them.
    #[test]
    fn each_quality_at_its_index() {
        for (i, q) in Quality::ALL.into_iter().enumerate() {
            assert_eq!(q.index(), i);
        }
    }

    /// The sizes of each choice: the standard one where the headset is not known; the
    /// largest scale found for 1:1, all (each within its limits) or the main one; 0.85
    /// as it is; the standard one where no scale does.
    #[test]
    fn the_sizes_of_each_choice() {
        let seen = |scale| Size::Seen { ppd: 25.0, scale };
        let found = |_: &dyn Fn(f32) -> Sizes| Some(0.8);
        let none = |_: &dyn Fn(f32) -> Sizes| None;
        assert_eq!(
            preset_sizes(None, MonitorSize::OneToOne, found),
            Sizes::default()
        );
        assert_eq!(
            preset_sizes(Some(25.0), MonitorSize::OneToOne, found),
            Sizes::all(Size::Bounded {
                ppd: 25.0,
                scale: 0.8
            })
        );
        assert_eq!(
            preset_sizes(Some(25.0), MonitorSize::MainOnly, found),
            Sizes {
                main: seen(0.8),
                others: Size::STANDARD
            }
        );
        assert_eq!(
            preset_sizes(Some(25.0), MonitorSize::Scale85, none),
            Sizes::all(seen(0.85))
        );
        assert_eq!(
            preset_sizes(Some(25.0), MonitorSize::OneToOne, none),
            Sizes::default()
        );
        assert_eq!(
            preset_sizes(Some(25.0), MonitorSize::Standard, found),
            Sizes::default()
        );
        assert_eq!(
            start_sizes(Some(20.0), MonitorSize::OneToOne),
            Sizes::all(Size::Bounded {
                ppd: 20.0,
                scale: 1.0
            })
        );
    }

    /// A monitor made at a scale is shown at it from the eye, flat or curved around the
    /// eye; nearer, larger.
    #[test]
    fn shown_at_the_scale_it_was_made() {
        let size = Size::Seen {
            ppd: 25.0,
            scale: 1.0,
        };
        let (w, _) = size.metres(prostir_layout::space::PixelSize::new(1920, 1080), RADIUS);
        let d = (RADIUS * RADIUS - w * w / 4.0).sqrt();
        let position = Vec3::new(0.0, 0.0, -d);
        let p = Placement {
            position,
            rotation: facing(position, Vec3::ZERO),
            width: w,
        };
        let flat = shown(&p, 1920, Vec3::ZERO, None, 25.0);
        assert!((flat - 1.0).abs() < 1e-3, "{flat}");
        // Curved around the eye: its pixels are as many degrees as there are of them.
        let curved = shown(&p, 1920, Vec3::ZERO, Some(RADIUS), 25.0);
        let degrees = 2.0 * (w / 2.0 / RADIUS).asin().to_degrees();
        assert!((curved - degrees * 25.0 / 1920.0).abs() < 1e-3, "{curved}");
        let nearer = shown(&p, 1920, Vec3::new(0.0, 0.0, -0.3), None, 25.0);
        assert!(nearer > flat);
    }
}
