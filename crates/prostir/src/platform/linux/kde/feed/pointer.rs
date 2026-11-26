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
    hidden: bool,
    /// Its look came and was not told yet (it was off this output).
    untold: bool,
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
            untold: false,
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
        // Its look, kept also while it is off this output: told once it is on it.
        let mut gone = false;
        if let Some(shape) = &meta.shape {
            if shape.w == 0 || shape.h == 0 {
                self.look = None;
                gone = true;
            } else {
                let mut rgba = shape.rgba.clone();
                straighten(&mut rgba);
                self.look = Some((shape.w, shape.h, rgba));
                self.untold = true;
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
        if gone && !std::mem::replace(&mut self.hidden, true) {
            tell(CursorEvent::Hidden);
        }
        let hot = (p.hot_x, p.hot_y);
        if let Some((w, h, rgba)) = &self.look
            && (self.untold || self.hot != Some(hot) || self.hidden)
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
