//! What KWin's buffers say besides the picture: where the pointer is and how it looks
//! (`SPA_META_Cursor`, with its picture only when it changed), and which parts of the
//! picture changed (`SPA_META_VideoDamage`, rectangles).
//!
//! Read here and kept in [`Seen`] for whoever draws the pointer apart from the picture
//! (step 2 of the cursor layer) and encodes the changed parts better (an idea for
//! later); nothing reads them yet, and the pointer stays drawn into the picture.

use std::sync::{Arc, Mutex};

use pipewire::spa::sys as ss;

/// Where the pointer is, in pixels of the picture, and its hotspot in its own picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub hot_x: i32,
    pub hot_y: i32,
}

/// How the pointer looks: 8-bit RGBA, rows packed (`w * 4` bytes each), alpha as the
/// desktop gives it (KWin: premultiplied). Empty when the pointer has no picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// A part of the picture, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The parts of the picture that changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Damage {
    /// Nothing.
    #[default]
    None,
    /// These parts.
    Parts(Vec<Rect>),
    /// All of it, or not said.
    Whole,
}

/// Parts kept apart before they count as the whole picture.
const PARTS_AT_MOST: usize = 64;

impl Damage {
    /// This and `more` together.
    pub fn add(&mut self, more: Self) {
        *self = match (std::mem::take(self), more) {
            (Self::Whole, _) | (_, Self::Whole) => Self::Whole,
            (Self::None, other) | (other, Self::None) => other,
            (Self::Parts(mut a), Self::Parts(b)) => {
                a.extend(b);
                if a.len() > PARTS_AT_MOST {
                    Self::Whole
                } else {
                    Self::Parts(a)
                }
            }
        };
    }
}

/// What one buffer said of the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMeta {
    /// Where it is; none when the desktop says no pointer is on the picture.
    pub pointer: Option<Pointer>,
    /// Its new look, only when it changed.
    pub shape: Option<Shape>,
}

/// Bytes of a buffer's metadata from `offset`, as a `T` (SPA's structs: plain numbers).
fn read<T: Copy>(meta: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(size_of::<T>())?;
    let bytes = meta.get(offset..end)?;
    // SAFETY: `bytes` holds `size_of::<T>()` bytes; `T` is one of SPA's structs of
    // plain numbers, for which any bytes are a value.
    Some(unsafe { bytes.as_ptr().cast::<T>().read_unaligned() })
}

/// The bytes of one pixel of SPA's 32-bit formats, in the order R, G, B, A (an `x`
/// byte counts as opaque).
fn rgba_order(format: u32) -> Option<([usize; 3], Option<usize>)> {
    Some(match format {
        ss::SPA_VIDEO_FORMAT_RGBA => ([0, 1, 2], Some(3)),
        ss::SPA_VIDEO_FORMAT_BGRA => ([2, 1, 0], Some(3)),
        ss::SPA_VIDEO_FORMAT_ARGB => ([1, 2, 3], Some(0)),
        ss::SPA_VIDEO_FORMAT_ABGR => ([3, 2, 1], Some(0)),
        ss::SPA_VIDEO_FORMAT_RGBx => ([0, 1, 2], None),
        ss::SPA_VIDEO_FORMAT_BGRx => ([2, 1, 0], None),
        ss::SPA_VIDEO_FORMAT_xRGB => ([1, 2, 3], None),
        ss::SPA_VIDEO_FORMAT_xBGR => ([3, 2, 1], None),
        _ => return None,
    })
}

/// The pointer's picture of a `spa_meta_bitmap` at `at` in `meta`.
fn bitmap(meta: &[u8], at: usize) -> Option<Shape> {
    let b: ss::spa_meta_bitmap = read(meta, at)?;
    let (w, h) = (b.size.width, b.size.height);
    if w == 0 || h == 0 {
        return Some(Shape {
            w: 0,
            h: 0,
            rgba: Vec::new(),
        });
    }
    let (rgb, alpha) = rgba_order(b.format)?;
    let row = usize::try_from(w).ok()?.checked_mul(4)?;
    let stride = usize::try_from(b.stride).ok().filter(|s| *s >= row)?;
    let start = at.checked_add(b.offset as usize)?;
    let rows = usize::try_from(h).ok()?;
    let end = start.checked_add(stride.checked_mul(rows - 1)?.checked_add(row)?)?;
    let pixels = meta.get(start..end)?;
    let mut rgba = Vec::with_capacity(row * rows);
    for y in 0..rows {
        for px in pixels[y * stride..y * stride + row].as_chunks::<4>().0 {
            rgba.extend(rgb.map(|i| px[i]));
            rgba.push(alpha.map_or(255, |a| px[a]));
        }
    }
    Some(Shape { w, h, rgba })
}

/// What a `spa_meta_cursor` (with what follows it, `meta`) says.
pub fn read_cursor(meta: &[u8]) -> Option<CursorMeta> {
    let c: ss::spa_meta_cursor = read(meta, 0)?;
    // No pointer on the picture: nothing else counts.
    if c.id == 0 {
        return Some(CursorMeta {
            pointer: None,
            shape: None,
        });
    }
    let at = c.bitmap_offset as usize;
    let shape = (at >= size_of::<ss::spa_meta_cursor>())
        .then(|| bitmap(meta, at))
        .flatten();
    Some(CursorMeta {
        pointer: Some(Pointer {
            x: c.position.x,
            y: c.position.y,
            hot_x: c.hotspot.x,
            hot_y: c.hotspot.y,
        }),
        shape,
    })
}

/// The changed parts a `SPA_META_VideoDamage` lists (`meta`): rectangles up to the
/// first empty one.
pub fn read_damage(meta: &[u8]) -> Damage {
    let step = size_of::<ss::spa_meta_region>();
    let mut parts = Vec::new();
    for at in (0..meta.len() / step).map(|i| i * step) {
        let Some(r) = read::<ss::spa_meta_region>(meta, at) else {
            break;
        };
        let (w, h) = (r.region.size.width, r.region.size.height);
        if w == 0 || h == 0 {
            break;
        }
        parts.push(Rect {
            x: r.region.position.x,
            y: r.region.position.y,
            w,
            h,
        });
    }
    if parts.is_empty() {
        Damage::None
    } else {
        Damage::Parts(parts)
    }
}

/// The metadata of one buffer, by kind.
pub struct Metas<'a> {
    pub cursor: Option<&'a [u8]>,
    pub damage: Option<&'a [u8]>,
    pub crop: Option<&'a [u8]>,
}

/// The pointer's, the changed parts' and the crop's metadata of `buffer`.
///
/// # Safety
/// `buffer` is a valid `spa_buffer` whose metadata is valid for `'a`.
pub unsafe fn metas<'a>(buffer: *const ss::spa_buffer) -> Metas<'a> {
    let mut found = Metas {
        cursor: None,
        damage: None,
        crop: None,
    };
    // SAFETY: valid, the caller says.
    let b = unsafe { &*buffer };
    if b.metas.is_null() {
        return found;
    }
    // SAFETY: `metas` points to `n_metas` of them.
    let all = unsafe { std::slice::from_raw_parts(b.metas, b.n_metas as usize) };
    for m in all {
        if m.data.is_null() {
            continue;
        }
        // SAFETY: each meta's data is `size` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(m.data.cast::<u8>(), m.size as usize) };
        match m.type_ {
            ss::SPA_META_Cursor => found.cursor = Some(bytes),
            ss::SPA_META_VideoDamage => found.damage = Some(bytes),
            ss::SPA_META_VideoCrop => found.crop = Some(bytes),
            _ => {}
        }
    }
    found
}

/// What the stream said so far, for whoever draws the pointer apart from the
/// picture: where the pointer is, its look, and the parts of the picture that changed
/// since they were last taken.
#[derive(Debug, Default)]
pub struct Seen {
    pub pointer: Option<Pointer>,
    pub shape: Option<Arc<Shape>>,
    /// Counts the looks seen: a new number, a new look.
    pub shapes: u64,
    pub damage: Damage,
}

impl Seen {
    /// What one buffer said: `cursor`, and `damage` when it carries a picture (none
    /// said: all of it changed).
    pub fn saw(&mut self, cursor: Option<CursorMeta>, damage: Option<Damage>) {
        if let Some(c) = cursor {
            self.pointer = c.pointer;
            if let Some(shape) = c.shape {
                self.shape = Some(Arc::new(shape));
                self.shapes += 1;
            }
        }
        if let Some(d) = damage {
            self.damage.add(d);
        }
    }
}

/// [`Seen`], shared between the stream's thread and whoever reads it.
#[derive(Debug, Clone, Default)]
pub struct Sightings(Arc<Mutex<Seen>>);

impl Sightings {
    pub fn saw(&self, cursor: Option<CursorMeta>, damage: Option<Damage>) {
        self.0.lock().unwrap().saw(cursor, damage);
    }

    /// Where the pointer is now, and its look with the number it was counted under.
    #[cfg_attr(not(test), expect(dead_code, reason = "step 2 of the cursor layer"))]
    pub fn pointer(&self) -> (Option<Pointer>, Option<(u64, Arc<Shape>)>) {
        let seen = self.0.lock().unwrap();
        (seen.pointer, seen.shape.clone().map(|s| (seen.shapes, s)))
    }

    /// The parts of the picture changed since this was last asked.
    #[cfg_attr(not(test), expect(dead_code, reason = "step 2 of the cursor layer"))]
    pub fn take_damage(&self) -> Damage {
        std::mem::take(&mut self.0.lock().unwrap().damage)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A struct of SPA's as its bytes.
    fn bytes_of<T>(value: &T) -> Vec<u8> {
        // SAFETY: a plain struct of numbers, read as the bytes it is made of.
        unsafe {
            std::slice::from_raw_parts(std::ptr::from_ref(value).cast::<u8>(), size_of::<T>())
        }
        .to_vec()
    }

    /// A pointer's metadata as KWin writes it: where, the hotspot, and a `w`x`h`
    /// picture in `format` (none: the look did not change).
    pub fn cursor_meta(
        id: u32,
        at: (i32, i32),
        hot: (i32, i32),
        picture: Option<(u32, u32, u32, &[u8])>,
    ) -> Vec<u8> {
        let head = size_of::<ss::spa_meta_cursor>();
        // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
        let mut c: ss::spa_meta_cursor = unsafe { std::mem::zeroed() };
        c.id = id;
        c.position.x = at.0;
        c.position.y = at.1;
        c.hotspot.x = hot.0;
        c.hotspot.y = hot.1;
        c.bitmap_offset = if picture.is_some() { head as u32 } else { 0 };
        let mut meta = bytes_of(&c);
        if let Some((format, w, h, pixels)) = picture {
            // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
            let mut b: ss::spa_meta_bitmap = unsafe { std::mem::zeroed() };
            b.format = format;
            b.size.width = w;
            b.size.height = h;
            b.stride = (w * 4) as i32;
            b.offset = size_of::<ss::spa_meta_bitmap>() as u32;
            meta.extend(bytes_of(&b));
            meta.extend_from_slice(pixels);
        }
        meta
    }

    pub fn damage_meta(rects: &[(i32, i32, u32, u32)], room: usize) -> Vec<u8> {
        let mut meta = Vec::new();
        for i in 0..room {
            // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
            let mut r: ss::spa_meta_region = unsafe { std::mem::zeroed() };
            if let Some(&(x, y, w, h)) = rects.get(i) {
                r.region.position.x = x;
                r.region.position.y = y;
                r.region.size.width = w;
                r.region.size.height = h;
            }
            meta.extend(bytes_of(&r));
        }
        meta
    }

    #[test]
    fn the_pointer_where_and_its_look_once() {
        // Two pixels, BGRA: blue opaque, red half.
        let pixels = [255, 0, 0, 255, 0, 0, 128, 128];
        let meta = cursor_meta(
            1,
            (100, 50),
            (3, 4),
            Some((ss::SPA_VIDEO_FORMAT_BGRA, 2, 1, &pixels)),
        );
        let read = read_cursor(&meta).unwrap();
        assert_eq!(
            read.pointer,
            Some(Pointer {
                x: 100,
                y: 50,
                hot_x: 3,
                hot_y: 4
            })
        );
        let shape = read.shape.unwrap();
        assert_eq!((shape.w, shape.h), (2, 1));
        assert_eq!(shape.rgba, [0, 0, 255, 255, 128, 0, 0, 128]);

        // Moved, same look: no picture.
        let moved = read_cursor(&cursor_meta(1, (101, 52), (3, 4), None)).unwrap();
        assert_eq!(moved.pointer.map(|p| (p.x, p.y)), Some((101, 52)));
        assert_eq!(moved.shape, None);

        // Off the picture.
        let gone = read_cursor(&cursor_meta(0, (5, 5), (0, 0), None)).unwrap();
        assert_eq!(
            gone,
            CursorMeta {
                pointer: None,
                shape: None
            }
        );
    }

    #[test]
    fn pointer_pictures_in_other_orders_and_bad_ones() {
        let px = [1, 2, 3, 4];
        let shape = |format| {
            read_cursor(&cursor_meta(1, (0, 0), (0, 0), Some((format, 1, 1, &px))))
                .unwrap()
                .shape
                .map(|s| s.rgba)
        };
        assert_eq!(shape(ss::SPA_VIDEO_FORMAT_RGBA), Some(vec![1, 2, 3, 4]));
        assert_eq!(shape(ss::SPA_VIDEO_FORMAT_ARGB), Some(vec![2, 3, 4, 1]));
        assert_eq!(shape(ss::SPA_VIDEO_FORMAT_ABGR), Some(vec![4, 3, 2, 1]));
        assert_eq!(shape(ss::SPA_VIDEO_FORMAT_BGRx), Some(vec![3, 2, 1, 255]));
        assert_eq!(
            shape(ss::SPA_VIDEO_FORMAT_NV12),
            None,
            "not a pointer's format"
        );

        // A picture said larger than the metadata: not read past its end.
        let mut meta = cursor_meta(
            1,
            (0, 0),
            (0, 0),
            Some((ss::SPA_VIDEO_FORMAT_RGBA, 4, 4, &px)),
        );
        assert_eq!(read_cursor(&meta).unwrap().shape, None);
        // A size of nothing: a pointer with no picture.
        meta = cursor_meta(
            1,
            (0, 0),
            (0, 0),
            Some((ss::SPA_VIDEO_FORMAT_RGBA, 0, 0, &[])),
        );
        assert_eq!(read_cursor(&meta).unwrap().shape.map(|s| s.w), Some(0));
        // Too short to be one.
        assert_eq!(read_cursor(&[0; 4]), None);
    }

    #[test]
    fn changed_parts_up_to_the_first_empty_one() {
        let meta = damage_meta(&[(0, 0, 10, 20), (100, 5, 1, 1)], 4);
        assert_eq!(
            read_damage(&meta),
            Damage::Parts(vec![
                Rect {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 20
                },
                Rect {
                    x: 100,
                    y: 5,
                    w: 1,
                    h: 1
                }
            ])
        );
        assert_eq!(read_damage(&damage_meta(&[], 4)), Damage::None);
        // All of the room used.
        assert!(
            matches!(read_damage(&damage_meta(&[(1, 1, 2, 2); 3], 3)), Damage::Parts(p) if p.len() == 3)
        );
    }

    #[test]
    fn changed_parts_add_up_until_taken() {
        let seen = Sightings::default();
        let part = |x| {
            Damage::Parts(vec![Rect {
                x,
                y: 0,
                w: 1,
                h: 1,
            }])
        };
        seen.saw(None, Some(part(1)));
        seen.saw(None, Some(Damage::None));
        seen.saw(None, Some(part(2)));
        assert!(matches!(seen.take_damage(), Damage::Parts(p) if p.len() == 2));
        assert_eq!(seen.take_damage(), Damage::None, "taken");
        seen.saw(None, Some(part(1)));
        seen.saw(None, Some(Damage::Whole));
        seen.saw(None, Some(part(3)));
        assert_eq!(seen.take_damage(), Damage::Whole);
        for x in 0..=PARTS_AT_MOST as i32 {
            seen.saw(None, Some(part(x)));
        }
        assert_eq!(seen.take_damage(), Damage::Whole, "too many parts");
    }

    #[test]
    fn the_look_is_counted_once_per_change() {
        let seen = Sightings::default();
        let look = Shape {
            w: 1,
            h: 1,
            rgba: vec![0; 4],
        };
        let at = |x| Pointer {
            x,
            y: 0,
            hot_x: 0,
            hot_y: 0,
        };
        seen.saw(
            Some(CursorMeta {
                pointer: Some(at(1)),
                shape: Some(look.clone()),
            }),
            None,
        );
        seen.saw(
            Some(CursorMeta {
                pointer: Some(at(2)),
                shape: None,
            }),
            None,
        );
        let (pointer, shape) = seen.pointer();
        assert_eq!(pointer, Some(at(2)));
        let (n, s) = shape.unwrap();
        assert_eq!((n, &*s), (1, &look));
        seen.saw(
            Some(CursorMeta {
                pointer: None,
                shape: None,
            }),
            None,
        );
        assert_eq!(seen.pointer().0, None, "hidden; its look is kept");
        assert!(seen.pointer().1.is_some());
    }

    /// A buffer as PipeWire hands it over, with metadata: found by kind.
    #[test]
    fn metadata_found_in_a_buffer() {
        let mut cursor = cursor_meta(1, (7, 8), (0, 0), None);
        let mut damage = damage_meta(&[(0, 0, 4, 4)], 2);
        let mut header = vec![0u8; size_of::<ss::spa_meta_header>()];
        let mut metas = [
            ss::spa_meta {
                type_: ss::SPA_META_Header,
                size: header.len() as u32,
                data: header.as_mut_ptr().cast(),
            },
            ss::spa_meta {
                type_: ss::SPA_META_Cursor,
                size: cursor.len() as u32,
                data: cursor.as_mut_ptr().cast(),
            },
            ss::spa_meta {
                type_: ss::SPA_META_VideoDamage,
                size: damage.len() as u32,
                data: damage.as_mut_ptr().cast(),
            },
        ];
        // SAFETY: one of SPA's structs of numbers and pointers, for which all zero is a value.
        let mut buffer: ss::spa_buffer = unsafe { std::mem::zeroed() };
        buffer.n_metas = metas.len() as u32;
        buffer.metas = metas.as_mut_ptr();
        // SAFETY: the buffer and its metadata live through the call.
        let found = unsafe { super::metas(&raw const buffer) };
        assert_eq!(
            read_cursor(found.cursor.unwrap())
                .unwrap()
                .pointer
                .map(|p| p.x),
            Some(7)
        );
        assert!(matches!(read_damage(found.damage.unwrap()), Damage::Parts(p) if p.len() == 1));
        buffer.n_metas = 1;
        // SAFETY: as above.
        let found = unsafe { super::metas(&raw const buffer) };
        assert!(found.cursor.is_none() && found.damage.is_none());
    }
}
