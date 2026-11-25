//! The formats KWin's stream is offered, and the one it chose, in GStreamer's terms.
//!
//! What the encoder takes (its caps, asked through the `appsrc` as `pipewiresrc` asked
//! them) becomes PipeWire's `EnumFormat` params, and the `Format` KWin chose becomes
//! the caps the frames go into the encoder with. The rules are those of `pipewiresrc`
//! (PipeWire 1.6, `gstpipewireformat.c`), so KWin is offered the same formats and the
//! same DMA-BUF layouts (modifiers) as it was through it:
//!
//! - GPU buffers first: for each format of the encoder's `memory:DMABuf` caps, the
//!   modifiers it reads (from `drm-format`), as one choice KWin fixates itself
//!   (`DONT_FIXATE`) when there are several. A format the converter names without a
//!   modifier is offered as linear or as any layout the driver picks implicitly.
//! - Then plain memory, the formats without a modifier, for an encoder that takes only
//!   those (x264 or NVENC through the CPU, `--software`): KWin then sends shared memory.
//! - The size and the frame rate as the encoder allows them (ranges, lists).
//!
//! KWin picks the format and the modifier it can draw into; the frames' caps are then
//! that format intersected with what the encoder takes, fixated.

use std::io::Cursor;

use gstreamer as gst;
use gstreamer::glib::translate::{FromGlib, IntoGlib};
use gstreamer_video as gst_video;
use pipewire::spa;
use spa::pod::serialize::PodSerializer;
use spa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
use spa::sys as ss;
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle};

/// A buffer laid out in rows, no tiling.
pub const MOD_LINEAR: u64 = 0;
/// A layout the driver picks without saying which (an implicit modifier).
pub const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// The video formats SPA numbers as GStreamer does, from `I420` to `Y444_12LE`: a
/// format has the same number in both, which is how `pipewiresrc` maps them; the
/// formats after them are not offered.
fn shared_formats() -> std::ops::RangeInclusive<i32> {
    gst_video::VideoFormat::I420.into_glib()..=gst_video::VideoFormat::Y44412le.into_glib()
}

/// The SPA format of a GStreamer video format, where they share one.
pub fn spa_format(format: gst_video::VideoFormat) -> Option<u32> {
    let n = format.into_glib();
    shared_formats().contains(&n).then_some(n as u32)
}

/// The GStreamer video format of a SPA format, where they share one.
pub fn gst_format(id: u32) -> Option<gst_video::VideoFormat> {
    let n = i32::try_from(id)
        .ok()
        .filter(|n| shared_formats().contains(n))?;
    // SAFETY: a value GStreamer's enum has (an unknown one would map to `__Unknown`).
    Some(unsafe { gst_video::VideoFormat::from_glib(n) })
}

/// The values of a caps field as a list: one value alone, or each of a list.
fn each(value: &gst::glib::SendValue) -> Vec<gst::glib::SendValue> {
    match value.get::<gst::List>() {
        Ok(list) => list.as_slice().to_vec(),
        Err(_) => vec![value.clone()],
    }
}

/// The strings of a caps field (one or a list).
fn strings(s: &gst::StructureRef, field: &str) -> Vec<String> {
    s.value(field)
        .map(|v| {
            each(v)
                .iter()
                .filter_map(|v| v.get::<String>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// A choice of `values` as `pipewiresrc` builds it: one value alone, else an enum whose
/// default is the first.
fn listed<T: spa::pod::CanonicalFixedSizedPod + Copy>(values: &[T]) -> Option<Choice<T>> {
    let first = *values.first()?;
    Some(Choice(
        ChoiceFlags::empty(),
        if values.len() == 1 {
            ChoiceEnum::None(first)
        } else {
            ChoiceEnum::Enum {
                default: first,
                alternatives: values.to_vec(),
            }
        },
    ))
}

/// The size the encoder allows: both sides one number, both a range (from the
/// smallest), or both a list.
fn size_choice(s: &gst::StructureRef) -> Option<Choice<Rectangle>> {
    let (w, h) = (s.value("width").ok()?, s.value("height").ok()?);
    let rect = |w: i32, h: i32| Rectangle {
        width: w as u32,
        height: h as u32,
    };
    if let (Ok(w), Ok(h)) = (w.get::<i32>(), h.get::<i32>()) {
        return Some(Choice(ChoiceFlags::empty(), ChoiceEnum::None(rect(w, h))));
    }
    if let (Ok(w), Ok(h)) = (w.get::<gst::IntRange<i32>>(), h.get::<gst::IntRange<i32>>()) {
        let (min, max) = (rect(w.min(), h.min()), rect(w.max(), h.max()));
        let step = rect(w.step(), h.step());
        return Some(Choice(
            ChoiceFlags::empty(),
            if step.width > 1 || step.height > 1 {
                ChoiceEnum::Step {
                    default: min,
                    min,
                    max,
                    step,
                }
            } else {
                ChoiceEnum::Range {
                    default: min,
                    min,
                    max,
                }
            },
        ));
    }
    if w.get::<gst::List>().is_ok() && h.get::<gst::List>().is_ok() {
        let sizes: Vec<Rectangle> = each(w)
            .iter()
            .zip(each(h))
            .filter_map(|(w, h)| Some(rect(w.get().ok()?, h.get().ok()?)))
            .collect();
        return listed(&sizes);
    }
    None
}

/// A frame rate field as a choice: one rate, a range (from the lowest), or a list.
fn rate_choice(s: &gst::StructureRef, field: &str) -> Option<Choice<Fraction>> {
    let v = s.value(field).ok()?;
    let fraction = |f: gst::Fraction| Fraction {
        num: f.numer() as u32,
        denom: f.denom() as u32,
    };
    if let Ok(f) = v.get::<gst::Fraction>() {
        return Some(Choice(ChoiceFlags::empty(), ChoiceEnum::None(fraction(f))));
    }
    if let Ok(r) = v.get::<gst::FractionRange>() {
        let (min, max) = (fraction(r.min()), fraction(r.max()));
        return Some(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: min,
                min,
                max,
            },
        ));
    }
    let rates: Vec<Fraction> = each(v)
        .iter()
        .filter_map(|v| v.get::<gst::Fraction>().ok().map(fraction))
        .collect();
    listed(&rates)
}

fn prop(key: u32, value: Value) -> Property {
    Property::new(key, value)
}

/// Serialised, as PipeWire takes params.
pub fn bytes(object: Object) -> Vec<u8> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .expect("a pod in memory")
        .0
        .into_inner()
}

/// One `EnumFormat` of raw video: `format` (none: any) with its modifiers, and the
/// limits of the caps structure `s`.
fn enum_format(format: Option<u32>, modifiers: &[u64], s: &gst::StructureRef) -> Vec<u8> {
    let mut props = vec![
        prop(
            ss::SPA_FORMAT_mediaType,
            Value::Id(Id(ss::SPA_MEDIA_TYPE_video)),
        ),
        prop(
            ss::SPA_FORMAT_mediaSubtype,
            Value::Id(Id(ss::SPA_MEDIA_SUBTYPE_raw)),
        ),
    ];
    if let Some(format) = format {
        props.push(prop(ss::SPA_FORMAT_VIDEO_format, Value::Id(Id(format))));
    }
    if !modifiers.is_empty() {
        let mut flags = PropertyFlags::MANDATORY;
        if modifiers.len() > 1 {
            flags |= PropertyFlags::DONT_FIXATE;
        }
        let longs: Vec<i64> = modifiers.iter().map(|m| *m as i64).collect();
        let choice = listed(&longs).expect("modifiers");
        props.push(Property {
            key: ss::SPA_FORMAT_VIDEO_modifier,
            flags,
            value: Value::Choice(ChoiceValue::Long(choice)),
        });
    }
    if let Some(size) = size_choice(s) {
        props.push(prop(
            ss::SPA_FORMAT_VIDEO_size,
            Value::Choice(ChoiceValue::Rectangle(size)),
        ));
    }
    if let Some(rate) = rate_choice(s, "framerate") {
        props.push(prop(
            ss::SPA_FORMAT_VIDEO_framerate,
            Value::Choice(ChoiceValue::Fraction(rate)),
        ));
    }
    if let Some(rate) = rate_choice(s, "max-framerate") {
        props.push(prop(
            ss::SPA_FORMAT_VIDEO_maxFramerate,
            Value::Choice(ChoiceValue::Fraction(rate)),
        ));
    }
    bytes(Object {
        type_: ss::SPA_TYPE_OBJECT_Format,
        id: ss::SPA_PARAM_EnumFormat,
        properties: props,
    })
}

/// The video formats of one caps structure, each with the modifiers it is offered
/// with, in the order the caps name them.
fn formats_of(s: &gst::StructureRef, dmabuf: bool) -> Vec<(u32, Vec<u64>)> {
    let mut formats: Vec<(u32, Vec<u64>)> = Vec::new();
    let mut add = |format: u32, modifiers: &[u64]| {
        let i = match formats.iter().position(|(f, _)| *f == format) {
            Some(i) => i,
            None => {
                formats.push((format, Vec::new()));
                formats.len() - 1
            }
        };
        for m in modifiers {
            if !formats[i].1.contains(m) {
                formats[i].1.push(*m);
            }
        }
    };
    for name in strings(s, "format") {
        let format = gst_video::VideoFormat::from_string(&name);
        if dmabuf && format == gst_video::VideoFormat::DmaDrm {
            for drm in strings(s, "drm-format") {
                let Ok((fourcc, modifier)) = gst_video::dma_drm_fourcc_from_str(&drm) else {
                    continue;
                };
                if let Some(f) = gst_video::dma_drm_fourcc_to_format(fourcc)
                    .ok()
                    .and_then(spa_format)
                {
                    add(f, &[modifier]);
                }
            }
        } else if let Some(f) = spa_format(format) {
            add(
                f,
                if dmabuf {
                    &[MOD_LINEAR, MOD_INVALID]
                } else {
                    &[]
                },
            );
        }
    }
    formats
}

/// Leaves out what `pipewiresrc` left out of the caps it offered: GPU buffers whose
/// format is not said, or not known, or `DMA_DRM` without a `drm-format`.
pub fn sanitize(caps: &gst::Caps) -> gst::Caps {
    let mut kept = gst::Caps::new_empty();
    {
        let kept = kept.get_mut().expect("new caps");
        for (s, features) in caps.iter_with_features() {
            if features.contains("memory:DMABuf") {
                let Some(first) = strings(s, "format").into_iter().next() else {
                    continue;
                };
                let format = gst_video::VideoFormat::from_string(&first);
                if format == gst_video::VideoFormat::Unknown
                    || (format == gst_video::VideoFormat::DmaDrm && !s.has_field("drm-format"))
                {
                    continue;
                }
            }
            kept.append_structure_full(s.to_owned(), Some(features.to_owned()));
        }
    }
    kept
}

/// The `EnumFormat` params for the formats `caps` takes (raw video only): those in GPU
/// buffers first, then those in plain memory.
pub fn offers(caps: &gst::CapsRef) -> Vec<Vec<u8>> {
    let mut offered = Vec::new();
    for gpu in [true, false] {
        for (s, features) in caps.iter_with_features() {
            if features.contains("memory:DMABuf") != gpu || s.name() != "video/x-raw" {
                continue;
            }
            let formats = formats_of(s, gpu);
            if formats.is_empty() {
                // Any format, in plain memory.
                if !gpu {
                    offered.push(enum_format(None, &[], s));
                }
                continue;
            }
            for (format, modifiers) in formats {
                offered.push(enum_format(Some(format), &modifiers, s));
            }
        }
    }
    offered
}

/// A pod's properties by key, when it is an object.
fn properties(pod: &[u8]) -> Option<Vec<Property>> {
    match spa::pod::deserialize::PodDeserializer::deserialize_any_from(pod) {
        Ok((_, Value::Object(o))) => Some(o.properties),
        _ => None,
    }
}

fn find(props: &[Property], key: u32) -> Option<&Value> {
    props.iter().find(|p| p.key == key).map(|p| &p.value)
}

/// The values a property allows, as `pipewiresrc` reads them: one value, or the
/// choices of an enum (its default left out).
fn ids(value: &Value) -> Vec<u32> {
    match value {
        Value::Id(Id(id)) => vec![*id],
        Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::None(Id(id))))) => vec![*id],
        Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) => {
            alternatives.iter().map(|i| i.0).collect()
        }
        _ => Vec::new(),
    }
}

fn longs(value: &Value) -> Vec<u64> {
    match value {
        Value::Long(l) => vec![*l as u64],
        Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::None(l)))) => vec![*l as u64],
        Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) => {
            alternatives.iter().map(|l| *l as u64).collect()
        }
        _ => Vec::new(),
    }
}

/// A list field, or a single value when there is one.
fn set_one_or_list<T: Into<gst::glib::Value> + Send + Clone>(
    s: &mut gst::StructureRef,
    field: &str,
    values: &[T],
) {
    match values {
        [] => {}
        [one] => s.set(field, one.clone()),
        many => s.set(field, gst::List::new(many.iter().cloned())),
    }
}

/// The formats and layouts of a format with a modifier, as caps fields: a
/// `DMA_DRM` with its `drm-format`s for each known layout, and the plain format where
/// the layout is linear or implicit and no `drm-format` names it.
fn set_dmabuf_formats(s: &mut gst::StructureRef, formats: &[u32], modifiers: &[u64]) {
    let mut plain = Vec::new();
    let mut drm = Vec::new();
    for &f in formats {
        let Some(format) = gst_format(f) else {
            continue;
        };
        for &m in modifiers {
            let named = m != MOD_INVALID
                && gst_video::dma_drm_fourcc_from_format(format).is_ok_and(|fourcc| {
                    drm.push(gst_video::dma_drm_fourcc_to_string(fourcc, m).to_string());
                    true
                });
            if !named && (m == MOD_LINEAR || m == MOD_INVALID) {
                plain.push(format.to_str().to_string());
            }
        }
    }
    if !drm.is_empty() {
        plain.push("DMA_DRM".to_owned());
        set_one_or_list(s, "drm-format", &drm);
    }
    set_one_or_list(s, "format", &plain);
}

fn set_size(s: &mut gst::StructureRef, value: &Value) {
    let int = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    match value {
        Value::Rectangle(r)
        | Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::None(r)))) => {
            s.set("width", int(r.width));
            s.set("height", int(r.height));
        }
        Value::Choice(ChoiceValue::Rectangle(Choice(
            _,
            ChoiceEnum::Range { min, max, .. } | ChoiceEnum::Step { min, max, .. },
        ))) => {
            s.set("width", gst::IntRange::new(int(min.width), int(max.width)));
            s.set(
                "height",
                gst::IntRange::new(int(min.height), int(max.height)),
            );
        }
        Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) => {
            let w: Vec<i32> = alternatives.iter().map(|r| int(r.width)).collect();
            let h: Vec<i32> = alternatives.iter().map(|r| int(r.height)).collect();
            set_one_or_list(s, "width", &w);
            set_one_or_list(s, "height", &h);
        }
        _ => {}
    }
}

fn set_rate(s: &mut gst::StructureRef, field: &str, value: &Value) {
    let fraction = |f: &Fraction| {
        gst::Fraction::new(
            i32::try_from(f.num).unwrap_or(i32::MAX),
            i32::try_from(f.denom).unwrap_or(i32::MAX).max(1),
        )
    };
    match value {
        Value::Fraction(f)
        | Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::None(f)))) => {
            s.set(field, fraction(f));
        }
        Value::Choice(ChoiceValue::Fraction(Choice(
            _,
            ChoiceEnum::Range { min, max, .. } | ChoiceEnum::Step { min, max, .. },
        ))) => s.set(field, gst::FractionRange::new(fraction(min), fraction(max))),
        Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) => {
            let rates: Vec<gst::Fraction> = alternatives.iter().map(fraction).collect();
            set_one_or_list(s, field, &rates);
        }
        _ => {}
    }
}

/// The colorimetry a format names, where it names any part of it: SPA numbers each
/// part as GStreamer does.
fn colorimetry(props: &[Property]) -> Option<String> {
    let part = |key| find(props, key).and_then(|v| ids(v).first().copied());
    let keys = [
        ss::SPA_FORMAT_VIDEO_colorRange,
        ss::SPA_FORMAT_VIDEO_colorMatrix,
        ss::SPA_FORMAT_VIDEO_transferFunction,
        ss::SPA_FORMAT_VIDEO_colorPrimaries,
    ];
    if keys.iter().all(|k| part(*k).is_none()) {
        return None;
    }
    let [range, matrix, transfer, primaries] =
        keys.map(|k| i32::try_from(part(k).unwrap_or(0)).unwrap_or(0));
    // SAFETY: GStreamer's enums map a number they do not have to `__Unknown`.
    let colorimetry = unsafe {
        gst_video::VideoColorimetry::new(
            gst_video::VideoColorRange::from_glib(if (0..=2).contains(&range) { range } else { 0 }),
            gst_video::VideoColorMatrix::from_glib(if (0..=6).contains(&matrix) {
                matrix
            } else {
                0
            }),
            gst_video::VideoTransferFunction::from_glib(if (0..=16).contains(&transfer) {
                transfer
            } else {
                0
            }),
            gst_video::VideoColorPrimaries::from_glib(if (0..=12).contains(&primaries) {
                primaries
            } else {
                0
            }),
        )
    };
    Some(colorimetry.to_string())
}

/// The caps of a `Format` param (raw video), as `pipewiresrc` reads it.
pub fn caps_of(format: &[u8]) -> Option<gst::Caps> {
    let props = properties(format)?;
    let id = |key| find(&props, key).and_then(|v| ids(v).first().copied());
    if id(ss::SPA_FORMAT_mediaType) != Some(ss::SPA_MEDIA_TYPE_video)
        || id(ss::SPA_FORMAT_mediaSubtype) != Some(ss::SPA_MEDIA_SUBTYPE_raw)
    {
        return None;
    }
    let mut s = gst::Structure::new_empty("video/x-raw");
    let mut dmabuf = false;
    let formats = find(&props, ss::SPA_FORMAT_VIDEO_format)
        .map(ids)
        .unwrap_or_default();
    match find(&props, ss::SPA_FORMAT_VIDEO_modifier) {
        Some(modifier) if !formats.is_empty() => {
            let modifiers = longs(modifier);
            if !modifiers.is_empty() {
                dmabuf = true;
                set_dmabuf_formats(&mut s, &formats, &modifiers);
            }
        }
        _ => {
            let names: Vec<String> = formats
                .iter()
                .filter_map(|f| gst_format(*f))
                .map(|f| f.to_str().to_string())
                .collect();
            set_one_or_list(&mut s, "format", &names);
        }
    }
    let interlace = find(&props, ss::SPA_FORMAT_VIDEO_interlaceMode)
        .and_then(|v| ids(v).first().copied())
        .map_or("progressive", |mode| match mode {
            ss::SPA_VIDEO_INTERLACE_MODE_INTERLEAVED => "interleaved",
            ss::SPA_VIDEO_INTERLACE_MODE_MIXED => "mixed",
            ss::SPA_VIDEO_INTERLACE_MODE_FIELDS => "fields",
            _ => "progressive",
        });
    s.set("interlace-mode", interlace);
    if let Some(size) = find(&props, ss::SPA_FORMAT_VIDEO_size) {
        set_size(&mut s, size);
    }
    if let Some(rate) = find(&props, ss::SPA_FORMAT_VIDEO_framerate) {
        set_rate(&mut s, "framerate", rate);
    }
    if let Some(rate) = find(&props, ss::SPA_FORMAT_VIDEO_maxFramerate) {
        set_rate(&mut s, "max-framerate", rate);
    }
    if let Some(c) = colorimetry(&props) {
        s.set("colorimetry", c);
    }
    let mut caps = gst::Caps::new_empty();
    caps.get_mut()
        .expect("new caps")
        .append_structure_full(s, dmabuf.then(|| gst::CapsFeatures::new(["memory:DMABuf"])));
    Some(caps)
}

/// The caps the frames of `format` go into the encoder with: KWin's format as the
/// encoder takes it (`possible`), fixated; none when the encoder takes none of it.
pub fn chosen(format: &[u8], possible: &gst::Caps) -> Option<gst::Caps> {
    let peer = caps_of(format)?;
    let mut caps = peer.intersect_with_mode(possible, gst::CapsIntersectMode::First);
    if caps.is_empty() {
        return None;
    }
    caps.fixate();
    Some(caps)
}

/// Whether a `Format` param has a modifier: the frames come in GPU buffers only.
pub fn has_modifier(format: &[u8]) -> bool {
    properties(format).is_some_and(|p| find(&p, ss::SPA_FORMAT_VIDEO_modifier).is_some())
}

/// The size of a pointer's metadata with a picture of `w`x`h`.
const fn cursor_meta_size(w: usize, h: usize) -> i32 {
    (size_of::<ss::spa_meta_cursor>() + size_of::<ss::spa_meta_bitmap>() + w * h * 4) as i32
}

/// The rectangles of changed parts each frame can carry.
pub const DAMAGE_RECTS: usize = 16;

/// What the frames of a chosen format are to come in, as `pipewiresrc` asked
/// (`Buffers`: 16 buffers, GPU buffers only with a modifier, else also shared or
/// plain memory) with the metadata it asked for (`Meta`: header, crop, transform, the
/// pointer up to 384x384), and the changed parts of the picture.
pub fn buffer_params(modifier: bool) -> Vec<Vec<u8>> {
    let int = |v: i32| Value::Int(v);
    let range = |default: i32, min: i32, max: i32| {
        Value::Choice(ChoiceValue::Int(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range { default, min, max },
        )))
    };
    let mut types = 1 << ss::SPA_DATA_DmaBuf;
    if !modifier {
        types |= (1 << ss::SPA_DATA_MemFd) | (1 << ss::SPA_DATA_MemPtr);
    }
    let buffers = Object {
        type_: ss::SPA_TYPE_OBJECT_ParamBuffers,
        id: ss::SPA_PARAM_Buffers,
        properties: vec![
            prop(ss::SPA_PARAM_BUFFERS_buffers, range(16, 1, i32::MAX)),
            prop(ss::SPA_PARAM_BUFFERS_blocks, range(0, 1, i32::MAX)),
            prop(ss::SPA_PARAM_BUFFERS_size, range(0, 1, i32::MAX)),
            prop(ss::SPA_PARAM_BUFFERS_stride, range(0, 0, i32::MAX)),
            prop(
                ss::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: types,
                        flags: Vec::new(),
                    },
                ))),
            ),
        ],
    };
    let meta = |kind: u32, size: Value| Object {
        type_: ss::SPA_TYPE_OBJECT_ParamMeta,
        id: ss::SPA_PARAM_Meta,
        properties: vec![
            prop(ss::SPA_PARAM_META_type, Value::Id(Id(kind))),
            prop(ss::SPA_PARAM_META_size, size),
        ],
    };
    let region = size_of::<ss::spa_meta_region>() as i32;
    let cursor = cursor_meta_size(384, 384);
    vec![
        bytes(buffers),
        bytes(meta(
            ss::SPA_META_Header,
            int(size_of::<ss::spa_meta_header>() as i32),
        )),
        bytes(meta(ss::SPA_META_VideoCrop, int(region))),
        bytes(meta(
            ss::SPA_META_VideoTransform,
            int(size_of::<ss::spa_meta_videotransform>() as i32),
        )),
        bytes(meta(
            ss::SPA_META_Cursor,
            range(cursor, size_of::<ss::spa_meta_cursor>() as i32, cursor),
        )),
        bytes(meta(
            ss::SPA_META_VideoDamage,
            range(
                region * DAMAGE_RECTS as i32,
                region,
                region * DAMAGE_RECTS as i32,
            ),
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        gst::init().unwrap();
    }

    fn caps(s: &str) -> gst::Caps {
        s.parse().unwrap()
    }

    /// An `EnumFormat` read back: its format, its modifiers (default first) and the
    /// flags of the modifier property.
    fn read(pod: &[u8]) -> (Option<u32>, Vec<u64>, Option<PropertyFlags>, Vec<Property>) {
        let props = properties(pod).unwrap();
        let format =
            find(&props, ss::SPA_FORMAT_VIDEO_format).and_then(|v| ids(v).first().copied());
        let modifier = props
            .iter()
            .find(|p| p.key == ss::SPA_FORMAT_VIDEO_modifier);
        let modifiers = modifier.map_or_else(Vec::new, |p| match &p.value {
            Value::Long(l) | Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::None(l)))) => {
                vec![*l as u64]
            }
            Value::Choice(ChoiceValue::Long(Choice(
                _,
                ChoiceEnum::Enum {
                    default,
                    alternatives,
                },
            ))) => std::iter::once(default)
                .chain(alternatives)
                .map(|l| *l as u64)
                .collect(),
            other => panic!("modifier as {other:?}"),
        });
        (format, modifiers, modifier.map(|p| p.flags), props)
    }

    #[test]
    fn formats_have_the_same_numbers_in_spa_and_gstreamer() {
        use gst_video::VideoFormat as F;
        use spa::param::video::VideoFormat as S;
        init();
        for (g, s) in [
            (F::I420, S::I420),
            (F::Bgrx, S::BGRx),
            (F::Bgra, S::BGRA),
            (F::Rgbx, S::RGBx),
            (F::Rgba, S::RGBA),
            (F::Xrgb, S::xRGB),
            (F::Nv12, S::NV12),
            (F::Y44412le, S::Y444_12LE),
        ] {
            assert_eq!(spa_format(g), Some(s.as_raw()), "{g:?}");
            assert_eq!(gst_format(s.as_raw()), Some(g));
        }
        // After `Y444_12LE` the numbers part: not offered.
        assert_eq!(gst_format(S::Y444_12LE.as_raw() + 1), None);
        assert_eq!(spa_format(F::Encoded), None);
        assert_eq!(gst_format(1), None);
    }

    /// VA-API's converter: GPU buffers in two layouts of one format and one of
    /// another, and plain memory. KWin is offered the GPU buffers first, a format's
    /// layouts as one choice it fixates itself, then the plain formats.
    #[test]
    fn gpu_buffers_are_offered_first_with_their_layouts() {
        init();
        let encoder = caps(
            "video/x-raw(memory:DMABuf), format=(string)DMA_DRM, \
             drm-format=(string){ AR24:0x0100000000000002, AR24:0x0100000000000001, XB24 }, \
             width=(int)[ 1, 16384 ], height=(int)[ 1, 16384 ], framerate=(fraction)[ 0/1, 2147483647/1 ]; \
             video/x-raw, format=(string){ NV12, BGRx }, width=(int)[ 1, 16384 ], \
             height=(int)[ 1, 16384 ], framerate=(fraction)[ 0/1, 2147483647/1 ]",
        );
        let offered = offers(&encoder);
        assert_eq!(offered.len(), 4, "AR24, XB24, then NV12 and BGRx");
        let (format, modifiers, flags, props) = read(&offered[0]);
        assert_eq!(format, spa_format(gst_video::VideoFormat::Bgra));
        assert_eq!(
            modifiers,
            [
                0x0100_0000_0000_0002,
                0x0100_0000_0000_0002,
                0x0100_0000_0000_0001
            ],
            "the default, then each layout"
        );
        assert_eq!(
            flags,
            Some(PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE)
        );
        let size = find(&props, ss::SPA_FORMAT_VIDEO_size).unwrap();
        let Value::Choice(ChoiceValue::Rectangle(Choice(
            _,
            ChoiceEnum::Range { default, min, max },
        ))) = size
        else {
            panic!("{size:?}")
        };
        assert_eq!((default.width, min.height, max.width), (1, 1, 16384));
        let rate = find(&props, ss::SPA_FORMAT_VIDEO_framerate).unwrap();
        let Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Range { min, max, .. }))) =
            rate
        else {
            panic!("{rate:?}")
        };
        assert_eq!((min.num, max.num, max.denom), (0, 2_147_483_647, 1));

        // One layout: no choice to fixate.
        let (format, modifiers, flags, _) = read(&offered[1]);
        assert_eq!(format, spa_format(gst_video::VideoFormat::Rgbx));
        assert_eq!(modifiers, [MOD_LINEAR]);
        assert_eq!(flags, Some(PropertyFlags::MANDATORY));

        for (pod, want) in offered[2..]
            .iter()
            .zip([gst_video::VideoFormat::Nv12, gst_video::VideoFormat::Bgrx])
        {
            let (format, modifiers, flags, _) = read(pod);
            assert_eq!(format, spa_format(want));
            assert!(modifiers.is_empty() && flags.is_none(), "plain memory");
        }
    }

    /// A format the converter names in GPU buffers without `DMA_DRM` is offered as
    /// linear or as an implicit layout; one with no format at all in plain memory is
    /// any format.
    #[test]
    fn plain_formats_in_gpu_buffers_and_any_format() {
        init();
        let offered = offers(&caps(
            "video/x-raw(memory:DMABuf), format=(string)BGRx, width=(int)640, height=(int)360; \
             video/x-raw, width=(int)640, height=(int)360",
        ));
        assert_eq!(offered.len(), 2);
        let (format, modifiers, flags, props) = read(&offered[0]);
        assert_eq!(format, spa_format(gst_video::VideoFormat::Bgrx));
        assert_eq!(modifiers, [MOD_LINEAR, MOD_LINEAR, MOD_INVALID]);
        assert!(flags.unwrap().contains(PropertyFlags::DONT_FIXATE));
        assert!(matches!(
            find(&props, ss::SPA_FORMAT_VIDEO_size),
            Some(Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::None(r))))) if r.width == 640
        ));
        let (format, modifiers, _, _) = read(&offered[1]);
        assert_eq!((format, modifiers.len()), (None, 0));
    }

    /// x264 (`--software`): only plain memory, so KWin is offered no GPU buffers.
    #[test]
    fn the_cpu_encoder_is_offered_plain_memory_only() {
        init();
        let offered = offers(&caps(
            "video/x-raw, format=(string)I420, width=(int)[ 16, 4096 ], height=(int)[ 16, 4096 ]",
        ));
        assert_eq!(offered.len(), 1);
        let (format, modifiers, _, _) = read(&offered[0]);
        assert_eq!(format, spa_format(gst_video::VideoFormat::I420));
        assert!(modifiers.is_empty());
        assert!(!has_modifier(&offered[0]));
    }

    #[test]
    fn gpu_buffers_of_no_format_are_left_out() {
        init();
        let kept = sanitize(&caps(
            "video/x-raw(memory:DMABuf), width=(int)1; \
             video/x-raw(memory:DMABuf), format=(string)DMA_DRM; \
             video/x-raw(memory:DMABuf), format=(string)DMA_DRM, drm-format=(string)XR24; \
             video/x-raw, format=(string)NV12",
        ));
        assert_eq!(kept.size(), 2, "{kept}");
    }

    /// A `Format` as KWin fixates it.
    fn kwin_format(format: gst_video::VideoFormat, modifier: Option<u64>) -> Vec<u8> {
        let mut props = vec![
            prop(
                ss::SPA_FORMAT_mediaType,
                Value::Id(Id(ss::SPA_MEDIA_TYPE_video)),
            ),
            prop(
                ss::SPA_FORMAT_mediaSubtype,
                Value::Id(Id(ss::SPA_MEDIA_SUBTYPE_raw)),
            ),
            prop(
                ss::SPA_FORMAT_VIDEO_format,
                Value::Id(Id(spa_format(format).unwrap())),
            ),
            prop(
                ss::SPA_FORMAT_VIDEO_size,
                Value::Rectangle(Rectangle {
                    width: 2560,
                    height: 1440,
                }),
            ),
            prop(
                ss::SPA_FORMAT_VIDEO_framerate,
                Value::Fraction(Fraction { num: 0, denom: 1 }),
            ),
            prop(
                ss::SPA_FORMAT_VIDEO_maxFramerate,
                Value::Fraction(Fraction { num: 60, denom: 1 }),
            ),
        ];
        if let Some(m) = modifier {
            props.push(Property {
                key: ss::SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::MANDATORY,
                value: Value::Long(m as i64),
            });
        }
        bytes(Object {
            type_: ss::SPA_TYPE_OBJECT_Format,
            id: ss::SPA_PARAM_Format,
            properties: props,
        })
    }

    const VA: &str = "video/x-raw(memory:DMABuf), format=(string)DMA_DRM, \
        drm-format=(string){ AR24:0x0100000000000002, XR24:0x0100000000000002, XR24 }, \
        width=(int)[ 1, 16384 ], height=(int)[ 1, 16384 ], framerate=(fraction)[ 0/1, 2147483647/1 ]; \
        video/x-raw, format=(string){ NV12, BGRx }, width=(int)[ 1, 16384 ], \
        height=(int)[ 1, 16384 ], framerate=(fraction)[ 0/1, 2147483647/1 ]";

    /// KWin chose a tiled layout: the frames come as `DMA_DRM` in it, at its size, with
    /// the rate it says (none: 0/1).
    #[test]
    fn a_chosen_layout_becomes_dma_drm_caps() {
        init();
        let format = kwin_format(gst_video::VideoFormat::Bgrx, Some(0x0100_0000_0000_0002));
        assert!(has_modifier(&format));
        let caps = chosen(&format, &caps(VA)).unwrap();
        assert!(caps.is_fixed(), "{caps}");
        let s = caps.structure(0).unwrap();
        assert!(caps.features(0).unwrap().contains("memory:DMABuf"));
        assert_eq!(s.get::<&str>("format").unwrap(), "DMA_DRM");
        assert_eq!(
            s.get::<&str>("drm-format").unwrap(),
            "XR24:0x0100000000000002"
        );
        assert_eq!(
            (
                s.get::<i32>("width").unwrap(),
                s.get::<i32>("height").unwrap()
            ),
            (2560, 1440)
        );
        assert_eq!(
            s.get::<gst::Fraction>("framerate").unwrap(),
            gst::Fraction::new(0, 1)
        );
        let info = gst_video::VideoInfoDmaDrm::from_caps(&caps).unwrap();
        assert_eq!(
            info.to_video_info().unwrap().format(),
            gst_video::VideoFormat::Bgrx
        );
    }

    /// Linear: `DMA_DRM` with the format's name alone, as the converter lists it.
    #[test]
    fn a_linear_layout() {
        init();
        let caps = chosen(
            &kwin_format(gst_video::VideoFormat::Bgrx, Some(MOD_LINEAR)),
            &caps(VA),
        )
        .unwrap();
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<&str>("format").unwrap(), "DMA_DRM");
        assert_eq!(s.get::<&str>("drm-format").unwrap(), "XR24");
    }

    /// Shared memory (no modifier): plain caps in the format KWin chose.
    #[test]
    fn shared_memory_becomes_plain_caps() {
        init();
        let format = kwin_format(gst_video::VideoFormat::Bgrx, None);
        assert!(!has_modifier(&format));
        let caps = chosen(&format, &caps(VA)).unwrap();
        assert!(
            caps.features(0)
                .is_none_or(|f| !f.contains("memory:DMABuf")),
            "{caps}"
        );
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<&str>("format").unwrap(), "BGRx");
        assert_eq!(s.get::<&str>("interlace-mode").unwrap(), "progressive");
    }

    /// A layout the encoder does not read: no caps (the stream fails on it).
    #[test]
    fn a_layout_the_encoder_does_not_read() {
        init();
        let format = kwin_format(gst_video::VideoFormat::Bgrx, Some(0x0100_0000_0000_0009));
        assert_eq!(chosen(&format, &caps(VA)), None);
    }

    /// What the frames are to come in: GPU buffers only with a modifier, and the
    /// metadata, the pointer and the changed parts among it.
    #[test]
    fn buffers_and_metadata_asked_for() {
        let types = |modifier| {
            let params = buffer_params(modifier);
            let props = properties(&params[0]).unwrap();
            match find(&props, ss::SPA_PARAM_BUFFERS_dataType) {
                Some(Value::Choice(ChoiceValue::Int(Choice(
                    _,
                    ChoiceEnum::Flags { default, .. },
                )))) => *default as u32,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(types(true), 1 << ss::SPA_DATA_DmaBuf);
        assert_eq!(
            types(false),
            (1 << ss::SPA_DATA_DmaBuf) | (1 << ss::SPA_DATA_MemFd) | (1 << ss::SPA_DATA_MemPtr)
        );
        let metas: Vec<u32> = buffer_params(true)[1..]
            .iter()
            .map(|p| {
                let props = properties(p).unwrap();
                ids(find(&props, ss::SPA_PARAM_META_type).unwrap())[0]
            })
            .collect();
        assert_eq!(
            metas,
            [
                ss::SPA_META_Header,
                ss::SPA_META_VideoCrop,
                ss::SPA_META_VideoTransform,
                ss::SPA_META_Cursor,
                ss::SPA_META_VideoDamage
            ]
        );
    }
}
