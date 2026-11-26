//! KWin's pointer metadata as the core hears it (`CursorSource`): what a stream made
//! with the pointer apart (`POINTER_METADATA`) says of the pointer in each buffer, as
//! [`CursorEvent`]s for its output.
//!
//! KWin puts the pointer's place in every buffer and its picture only when it changed;
//! when only the pointer moved, the buffer carries nothing else (and never reaches the
//! encoder, see `frames::has_picture`). Its place is the hotspot's, in the output's
//! pixels, and KWin says it on every output the pointer's picture touches: off the
//! output's picture, the pointer is on another (or none). Its picture is
//! premultiplied (RGBA in KWin 6.8). The core hears the hotspot's place, and the
//! picture straight, only what changed.
//!
//! A picture of a size with nothing in it is no look: KWin 6.8 paints a pointer an app
//! draws itself (kitty's, Dolphin's I-beam, a `wl_surface` of the app's) into the
//! metadata from its scene (`grabCursorOpenGL`, unlike a pointer of the theme, its
//! picture as it is), and that can come out clear (over kitty and Dolphin on a display
//! of the PC), so the pointer vanished from the headset over those apps. The look before stays then, with its
//! hotspot, until a look with something in it comes. A pointer hidden is a picture of
//! no size (KWin says so for an app that hides it).

use std::sync::{Arc, Mutex};

use super::meta::CursorMeta;
use crate::platform::linux::straighten;
use crate::platform::{CursorEvent, CursorImage, CursorSink, OutputKey};

/// Where the core's sink is while it asks for the pointer apart; none otherwise. One
/// for the desktop, shared with every stream.
pub type SinkSlot = Arc<Mutex<Option<CursorSink>>>;

/// What one stream told the core of the pointer, to tell it only what changed.
#[derive(Debug)]
pub struct CursorTap {
    output: OutputKey,
    sink: SinkSlot,
    /// The pointer is on this output, as last told.
    on: bool,
    /// Its hotspot's place and the picture's size, as last told.
    at: Option<((i32, i32), (u32, u32))>,
    /// Its picture (straight) and hotspot, as last seen and told.
    look: Option<(u32, u32, Vec<u8>)>,
    hot: Option<(i32, i32)>,
    /// Hidden, as told; KWin said so last (a look of no size), until a look comes.
    hidden: bool,
    gone: bool,
    /// Its look came and was not told yet (it was off this output).
    untold: bool,
    /// KWin's look now is clear (see the module): the one before stands for it, with
    /// the hotspot it came with.
    clear: bool,
    look_hot: Option<(i32, i32)>,
}

impl CursorTap {
    pub fn new(output: OutputKey, sink: SinkSlot) -> Self {
        Self {
            output,
            sink,
            on: false,
            at: None,
            look: None,
            hot: None,
            hidden: false,
            gone: false,
            untold: false,
            clear: false,
            look_hot: None,
        }
    }

    /// What one buffer said of the pointer (`meta`), on a picture of `size` pixels:
    /// told to the core's sink, if it asks.
    pub fn saw(&mut self, meta: &CursorMeta, size: (u32, u32)) {
        let sink = self.sink.lock().unwrap().clone();
        self.events(meta, size, |e| {
            if let Some(sink) = &sink {
                sink.send(e);
            }
        });
    }

    /// What `meta` says, as the events that tell what changed, to `tell`.
    pub fn events(
        &mut self,
        meta: &CursorMeta,
        size: (u32, u32),
        mut tell: impl FnMut(CursorEvent),
    ) {
        // Its look, kept also while it is off this output (and while hidden): told
        // once it is on it.
        if let Some(shape) = &meta.shape {
            if shape.w == 0 || shape.h == 0 {
                self.gone = true;
            } else {
                self.gone = false;
                let clear = shape.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
                // A clear one: the look before stands for it (see the module), told
                // again only to show it again.
                self.clear = clear && self.look.is_some();
                self.untold |= !self.clear || self.hidden;
                if !self.clear {
                    self.look_hot = meta.pointer.map(|p| (p.hot_x, p.hot_y));
                    let mut rgba = shape.rgba.clone();
                    straighten(&mut rgba);
                    self.look = Some((shape.w, shape.h, rgba));
                }
            }
        }
        // On this output's picture: KWin says a pointer on every output its picture
        // touches, the hotspot off the others.
        let inside = meta
            .pointer
            .filter(|p| (0..size.0 as i32).contains(&p.x) && (0..size.1 as i32).contains(&p.y));
        let Some(p) = inside else {
            // Not on this output any more (on another, or none).
            if std::mem::take(&mut self.on) {
                self.at = None;
                tell(CursorEvent::Left {
                    output: self.output.clone(),
                });
            }
            return;
        };
        // The hotspot KWin says is its look's: the kept look keeps its own.
        let hot = match self.look_hot {
            Some(hot) if self.clear => hot,
            _ => (p.hot_x, p.hot_y),
        };
        if self.gone {
            if !std::mem::replace(&mut self.hidden, true) {
                tell(CursorEvent::Hidden);
            }
        } else if let Some((w, h, rgba)) = &self.look
            && (self.untold || self.hot != Some(hot))
        {
            let side = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
            let within = |v: i32, n: u32| u16::try_from(v.clamp(0, n as i32 - 1)).unwrap_or(0);
            self.hot = Some(hot);
            self.hidden = false;
            self.untold = false;
            tell(CursorEvent::Shape(CursorImage {
                w: side(*w),
                h: side(*h),
                hot_x: within(hot.0, *w),
                hot_y: within(hot.1, *h),
                rgba: rgba.clone(),
            }));
        }
        // KWin's place is the hotspot's, in the output's pixels.
        let at = ((p.x, p.y), size);
        if !self.on || self.at != Some(at) {
            self.on = true;
            self.at = Some(at);
            tell(CursorEvent::Moved {
                output: self.output.clone(),
                x: p.x,
                y: p.y,
                size,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::meta::{Pointer, Shape};
    use super::*;

    fn heard(tap: &mut CursorTap, meta: &CursorMeta) -> Vec<CursorEvent> {
        let mut out = Vec::new();
        tap.events(meta, (640, 360), |e| out.push(e));
        out
    }

    fn at(x: i32, y: i32, shape: Option<Shape>) -> CursorMeta {
        CursorMeta {
            pointer: Some(Pointer {
                x,
                y,
                hot_x: 2,
                hot_y: 3,
            }),
            shape,
        }
    }

    /// The look once (straight, its hotspot), the hotspot's place on each move, a
    /// move off this output once, hidden by a look of no size, shown by the next.
    #[test]
    fn kwins_pointer_as_the_core_hears_it() {
        let mut tap = CursorTap::new(OutputKey::new("Virtual-Prostir-1"), SinkSlot::default());
        let look = Shape {
            w: 4,
            h: 4,
            rgba: [100, 50, 0, 128].repeat(16),
        };
        let events = heard(&mut tap, &at(10, 20, Some(look.clone())));
        let [
            CursorEvent::Shape(image),
            CursorEvent::Moved { x, y, size, .. },
        ] = &events[..]
        else {
            panic!("{events:?}");
        };
        assert_eq!((image.w, image.h, image.hot_x, image.hot_y), (4, 4, 2, 3));
        assert_eq!(image.rgba[..4], [199, 100, 0, 128], "straight");
        assert_eq!((*x, *y, *size), (10, 20, (640, 360)));
        assert!(heard(&mut tap, &at(10, 20, None)).is_empty(), "nothing new");
        assert!(matches!(
            heard(&mut tap, &at(11, 20, None))[..],
            [CursorEvent::Moved { x: 11, .. }]
        ));
        let off = CursorMeta {
            pointer: None,
            shape: None,
        };
        assert!(matches!(
            heard(&mut tap, &off)[..],
            [CursorEvent::Left { .. }]
        ));
        assert!(heard(&mut tap, &off).is_empty(), "once");
        let none = Shape {
            w: 0,
            h: 0,
            rgba: Vec::new(),
        };
        let events = heard(&mut tap, &at(11, 20, Some(none)));
        assert!(
            matches!(events[..], [CursorEvent::Hidden, CursorEvent::Moved { .. }]),
            "{events:?}"
        );
        let events = heard(&mut tap, &at(11, 20, Some(look)));
        assert!(matches!(events[..], [CursorEvent::Shape(_)]), "{events:?}");
    }

    /// On a display of the PC (HDMI-A-1, 1920x1080), KWin's look of kitty's I-beam
    /// comes clear: the arrow before stays, with its own hotspot, and only the place is
    /// told; hidden while typing, the clear I-beam shows the arrow again; a look with
    /// something in it is told as ever.
    #[test]
    fn a_clear_look_keeps_the_one_before() {
        let mut tap = CursorTap::new(OutputKey::new("HDMI-A-1"), SinkSlot::default());
        let mut heard = |x, hot, shape| {
            let meta = CursorMeta {
                pointer: Some(Pointer {
                    x,
                    y: 500,
                    hot_x: hot,
                    hot_y: hot,
                }),
                shape,
            };
            let mut out = Vec::new();
            tap.events(&meta, (1920, 1080), |e| out.push(e));
            out
        };
        let look = |side: u32, alpha: u8| Shape {
            w: side,
            h: side,
            rgba: [0, 0, 0, alpha].repeat((side * side) as usize),
        };
        let events = heard(100, 1, Some(look(4, 255)));
        let [CursorEvent::Shape(arrow), CursorEvent::Moved { .. }] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!((arrow.w, arrow.hot_x), (4, 1));
        let arrow = arrow.clone();
        // Over kitty: the I-beam, 24x24 pointing at its middle, clear.
        let events = heard(900, 11, Some(look(24, 0)));
        assert!(
            matches!(events[..], [CursorEvent::Moved { x: 900, .. }]),
            "{events:?}"
        );
        assert!(heard(901, 11, None).len() == 1, "only the place");
        // Hidden while typing, then shown again by the mouse.
        let gone = Shape {
            w: 0,
            h: 0,
            rgba: Vec::new(),
        };
        assert!(matches!(
            heard(901, 0, Some(gone))[..],
            [CursorEvent::Hidden]
        ));
        assert!(heard(901, 0, None).is_empty(), "still hidden");
        let events = heard(902, 11, Some(look(24, 0)));
        let [CursorEvent::Shape(again), CursorEvent::Moved { .. }] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(*again, arrow, "the arrow, its own hotspot");
        // An I-beam KWin drew: told.
        let mut beam = look(24, 0);
        beam.rgba[11 * 24 * 4 + 11 * 4 + 3] = 255;
        let events = heard(902, 11, Some(beam));
        let [CursorEvent::Shape(beam)] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!((beam.w, beam.hot_x), (24, 11));
    }

    /// At the edge between two outputs side by side (640 wide each), KWin says the
    /// pointer on both while its picture touches both: only the one whose picture has
    /// the hotspot has it. Crossing, the one left hears it left, the other gets the
    /// look (seen while it was off it) and the place.
    #[test]
    fn at_a_shared_edge_only_the_output_with_the_hotspot_has_the_pointer() {
        let mut left = CursorTap::new(OutputKey::new("Virtual-Prostir-1"), SinkSlot::default());
        let mut right = CursorTap::new(OutputKey::new("Virtual-Prostir-2"), SinkSlot::default());
        let look = Shape {
            w: 4,
            h: 4,
            rgba: [0, 0, 0, 255].repeat(16),
        };
        // At 638 on the left one: -2 on the right one, its picture reaching into it.
        let events = heard(&mut left, &at(638, 100, Some(look.clone())));
        assert!(matches!(
            events[..],
            [CursorEvent::Shape(_), CursorEvent::Moved { x: 638, .. }]
        ));
        assert!(
            heard(&mut right, &at(-2, 100, Some(look))).is_empty(),
            "not there"
        );
        // Over the edge.
        assert!(matches!(
            heard(&mut left, &at(641, 100, None))[..],
            [CursorEvent::Left { .. }]
        ));
        let events = heard(&mut right, &at(1, 100, None));
        assert!(
            matches!(
                events[..],
                [CursorEvent::Shape(_), CursorEvent::Moved { x: 1, .. }]
            ),
            "{events:?}"
        );
        // Below the picture: off it too.
        assert!(matches!(
            heard(&mut right, &at(1, 360, None))[..],
            [CursorEvent::Left { .. }]
        ));
    }
}
