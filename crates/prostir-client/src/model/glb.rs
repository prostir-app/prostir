//! A glTF 2.0 binary (`.glb`) as the runtimes give their controllers: the scene's
//! nodes with their meshes, each corner placed by its node, coloured by its
//! material's base colour, as one [`Mesh`]. What a controller needs and no more:
//! triangles, positions, normals (made when missing), texture coordinates, corner
//! colours, the base colour and its picture; no animation, skins, morphs, cameras,
//! lights or sparse accessors (a model with any of these still draws, without them).
//!
//! A picture of the material is not put on the GPU: the shell decodes the ones it
//! can (PNG, JPEG) and [`Model::paint`] gives each corner the colour under it; one
//! it cannot (KTX2 with Basis Universal, as `KHR_texture_basisu` has it) gets one
//! colour instead ([`Model::tint`]). The reader trusts nothing it reads: every
//! offset and count is checked, and a broken model is an error, never a panic.

use std::ops::Range;

use glam::{Mat3, Mat4, Quat, Vec3};
use serde_json::Value;

use super::{Bad, MAX_TRIANGLES, MAX_VERTICES, Mesh, bad, linear, smooth_normals};

const MAGIC: u32 = 0x4654_6C67; // "glTF"
const JSON: u32 = 0x4E4F_534A; // "JSON"
const BIN: u32 = 0x004E_4942; // "BIN\0"
/// How deep nodes may nest (a loop of children would otherwise never end).
const DEPTH_MAX: usize = 32;

/// How a material's picture is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    /// KTX2 (with Basis Universal): not decoded here.
    Ktx2,
    Other,
}

/// A material's picture, as the model has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub kind: ImageKind,
    pub bytes: Vec<u8>,
}

/// The corners of a primitive whose colour comes from a picture, and which picture.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Part {
    corners: Range<usize>,
    image: usize,
}

/// A model read, with the pictures its colours still need (see [`Model::paint`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    pub mesh: Mesh,
    /// The pictures of the materials, by the model's own numbers.
    pub images: Vec<Image>,
    /// Each corner's place on its picture (0 where it has none).
    uvs: Vec<[f32; 2]>,
    parts: Vec<Part>,
}

impl Model {
    /// The model in `glb`.
    pub fn read(glb: &[u8]) -> Result<Self, Bad> {
        let (json, bin) = chunks(glb)?;
        let doc: Value = serde_json::from_slice(json).map_err(|e| Bad(format!("JSON: {e}")))?;
        let reader = Reader {
            doc: &doc,
            bin: bin.unwrap_or(&[]),
        };
        reader.model()
    }

    /// Gives each corner of the parts coloured by picture `image` the colour of the
    /// picture under it (`rgba`: `width` by `height`, top row first, sRGB), times its
    /// material's. A picture of the wrong size changes nothing.
    pub fn paint(&mut self, image: usize, width: u32, height: u32, rgba: &[u8]) {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || rgba.len() != w * h * 4 {
            return;
        }
        for part in self.parts.iter().filter(|p| p.image == image) {
            for i in part.corners.clone() {
                let [u, v] = self.uvs[i];
                let x = texel(u, w);
                let y = texel(v, h);
                let at = (y * w + x) * 4;
                let px = &rgba[at..at + 4];
                let c = &mut self.mesh.colors[i];
                for k in 0..3 {
                    c[k] *= linear(px[k]);
                }
                c[3] *= f32::from(px[3]) / 255.0;
            }
        }
    }

    /// Gives the parts coloured by picture `image` one colour instead (linear),
    /// times their material's: for a picture that cannot be decoded.
    pub fn tint(&mut self, image: usize, color: [f32; 3]) {
        for part in self.parts.iter().filter(|p| p.image == image) {
            for i in part.corners.clone() {
                let c = &mut self.mesh.colors[i];
                for k in 0..3 {
                    c[k] *= color[k];
                }
            }
        }
    }

    /// The model's mesh, its pictures and their places no longer needed.
    pub fn into_mesh(self) -> Mesh {
        self.mesh
    }
}

/// The texel of a coordinate along a side of `n` texels (repeating, as glTF's default).
fn texel(t: f32, n: usize) -> usize {
    let t = if t.is_finite() { t } else { 0.0 };
    let t = if (0.0..=1.0).contains(&t) {
        t
    } else {
        t.rem_euclid(1.0)
    };
    ((t * n as f32) as usize).min(n - 1)
}

/// The JSON and the binary chunk of a `.glb`.
fn chunks(glb: &[u8]) -> Result<(&[u8], Option<&[u8]>), Bad> {
    let word = |at: usize| -> Option<u32> {
        let b = glb.get(at..at + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if word(0) != Some(MAGIC) {
        return bad("not a glTF binary");
    }
    if word(4) != Some(2) {
        return bad(format!("glTF version {:?}, not 2", word(4)));
    }
    let length = word(8).map_or(0, |l| l as usize).min(glb.len());
    let (mut json, mut bin) = (None, None);
    let mut at = 12;
    while at + 8 <= length {
        let (Some(size), Some(kind)) = (word(at), word(at + 4)) else {
            break;
        };
        let start = at + 8;
        let Some(end) = start.checked_add(size as usize).filter(|e| *e <= length) else {
            return bad("a chunk runs past the end");
        };
        match kind {
            JSON if json.is_none() => json = Some(&glb[start..end]),
            BIN if bin.is_none() => bin = Some(&glb[start..end]),
            _ => {}
        }
        // Chunks are aligned to four bytes.
        at = end.div_ceil(4) * 4;
    }
    match json {
        Some(json) => Ok((json, bin)),
        None => bad("no JSON chunk"),
    }
}

struct Reader<'a> {
    doc: &'a Value,
    bin: &'a [u8],
}

/// What one accessor reads.
struct Accessor<'a> {
    bytes: &'a [u8],
    /// Where the first element starts, and from one to the next.
    offset: usize,
    stride: usize,
    count: usize,
    components: usize,
    kind: u64,
    normalized: bool,
}

impl Accessor<'_> {
    fn size(kind: u64) -> Option<usize> {
        match kind {
            5120 | 5121 => Some(1),
            5122 | 5123 => Some(2),
            5125 | 5126 => Some(4),
            _ => None,
        }
    }

    /// Component `c` of element `i` as a number (normalized integers to their range).
    fn get(&self, i: usize, c: usize) -> f32 {
        let size = Self::size(self.kind).unwrap_or(4);
        let at = self.offset + i * self.stride + c * size;
        let b = &self.bytes[at..at + size];
        match (self.kind, self.normalized) {
            (5126, _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (5121, false) => f32::from(b[0]),
            (5121, true) => f32::from(b[0]) / 255.0,
            (5120, false) => f32::from(b[0] as i8),
            (5120, true) => (f32::from(b[0] as i8) / 127.0).max(-1.0),
            (5123, false) => f32::from(u16::from_le_bytes([b[0], b[1]])),
            (5123, true) => f32::from(u16::from_le_bytes([b[0], b[1]])) / 65535.0,
            (5122, false) => f32::from(i16::from_le_bytes([b[0], b[1]])),
            (5122, true) => (f32::from(i16::from_le_bytes([b[0], b[1]])) / 32767.0).max(-1.0),
            _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
        }
    }

    /// Element `i` as an index.
    fn index(&self, i: usize) -> u32 {
        let at = self.offset + i * self.stride;
        let b = &self.bytes[at..];
        match self.kind {
            5121 => u32::from(b[0]),
            5123 => u32::from(u16::from_le_bytes([b[0], b[1]])),
            _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        }
    }

    fn vec<const N: usize>(&self, i: usize) -> [f32; N] {
        std::array::from_fn(|c| {
            if c < self.components {
                self.get(i, c)
            } else {
                1.0
            }
        })
    }
}

fn uint(v: &Value, key: &str) -> Option<usize> {
    v.get(key)?.as_u64().and_then(|n| usize::try_from(n).ok())
}

fn floats<const N: usize>(v: &Value, key: &str) -> Option<[f32; N]> {
    let list = v.get(key)?.as_array()?;
    if list.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (o, x) in out.iter_mut().zip(list) {
        *o = x.as_f64()? as f32;
    }
    out.iter().all(|x| x.is_finite()).then_some(out)
}

impl<'a> Reader<'a> {
    fn list(&self, key: &str) -> &'a [Value] {
        self.doc
            .get(key)
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// The bytes of buffer view `n`, and its stride (`None`: tightly packed).
    fn view(&self, n: usize) -> Result<(&'a [u8], Option<usize>), Bad> {
        let Some(view) = self.list("bufferViews").get(n) else {
            return bad(format!("no buffer view {n}"));
        };
        let outside = self
            .list("buffers")
            .first()
            .is_none_or(|b| b.get("uri").is_some());
        if uint(view, "buffer") != Some(0) || outside {
            return bad("a buffer outside the file");
        }
        let start = uint(view, "byteOffset").unwrap_or(0);
        let length = uint(view, "byteLength").unwrap_or(0);
        let Some(bytes) = start
            .checked_add(length)
            .and_then(|end| self.bin.get(start..end))
        else {
            return bad(format!("buffer view {n} runs past the binary chunk"));
        };
        Ok((bytes, uint(view, "byteStride").filter(|s| *s > 0)))
    }

    fn accessor(&self, n: usize) -> Result<Accessor<'a>, Bad> {
        let Some(a) = self.list("accessors").get(n) else {
            return bad(format!("no accessor {n}"));
        };
        if a.get("sparse").is_some() {
            return bad("sparse accessors");
        }
        let Some(view) = uint(a, "bufferView") else {
            return bad("an accessor without data");
        };
        let (bytes, stride) = self.view(view)?;
        let kind = a.get("componentType").and_then(Value::as_u64).unwrap_or(0);
        let Some(size) = Accessor::size(kind) else {
            return bad(format!("component type {kind}"));
        };
        let components = match a.get("type").and_then(Value::as_str) {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            other => return bad(format!("accessor type {other:?}")),
        };
        let count = uint(a, "count").unwrap_or(0);
        let offset = uint(a, "byteOffset").unwrap_or(0);
        let element = size * components;
        let stride = stride.unwrap_or(element);
        // The last element must end within the view.
        let fits = count == 0
            || (count - 1)
                .checked_mul(stride)
                .and_then(|s| s.checked_add(offset))
                .and_then(|s| s.checked_add(element))
                .is_some_and(|end| end <= bytes.len());
        if !fits || stride < element {
            return bad(format!("accessor {n} runs past its view"));
        }
        Ok(Accessor {
            bytes,
            offset,
            stride,
            count,
            components,
            kind,
            normalized: a.get("normalized").and_then(Value::as_bool) == Some(true),
        })
    }

    fn model(&self) -> Result<Model, Bad> {
        let nodes = self.list("nodes");
        let roots: Vec<usize> = match self
            .list("scenes")
            .get(uint(self.doc, "scene").unwrap_or(0))
            .and_then(|s| s.get("nodes"))
            .and_then(Value::as_array)
        {
            Some(list) => list
                .iter()
                .filter_map(Value::as_u64)
                .map(|n| n as usize)
                .collect(),
            // No scene: every node that is no other's child.
            None => (0..nodes.len())
                .filter(|i| {
                    !nodes.iter().any(|n| {
                        n.get("children")
                            .and_then(Value::as_array)
                            .is_some_and(|c| c.iter().any(|c| c.as_u64() == Some(*i as u64)))
                    })
                })
                .collect(),
        };
        let mut out = Model::default();
        for root in roots {
            self.node(root, Mat4::IDENTITY, 0, &mut out)?;
        }
        if out.mesh.indices.is_empty() {
            return bad("no triangles");
        }
        for image in self.list("images") {
            out.images.push(self.image(image));
        }
        Ok(out)
    }

    fn node(&self, n: usize, parent: Mat4, depth: usize, out: &mut Model) -> Result<(), Bad> {
        if depth > DEPTH_MAX {
            return bad("nodes nested too deep");
        }
        let Some(node) = self.list("nodes").get(n) else {
            return bad(format!("no node {n}"));
        };
        let local = if let Some(m) = floats::<16>(node, "matrix") {
            Mat4::from_cols_array(&m)
        } else {
            let t = floats::<3>(node, "translation").unwrap_or([0.0; 3]);
            let r = floats::<4>(node, "rotation").unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let s = floats::<3>(node, "scale").unwrap_or([1.0; 3]);
            let r = Quat::from_array(r);
            let r = if r.length_squared() > 1e-12 {
                r.normalize()
            } else {
                Quat::IDENTITY
            };
            Mat4::from_scale_rotation_translation(Vec3::from(s), r, Vec3::from(t))
        };
        let world = parent * local;
        if let Some(mesh) = uint(node, "mesh") {
            let Some(primitives) = self
                .list("meshes")
                .get(mesh)
                .and_then(|m| m.get("primitives"))
                .and_then(Value::as_array)
            else {
                return bad(format!("no mesh {mesh}"));
            };
            for p in primitives {
                self.primitive(p, world, out)?;
            }
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for c in children.iter().filter_map(Value::as_u64) {
                self.node(c as usize, world, depth + 1, out)?;
            }
        }
        Ok(())
    }

    fn primitive(&self, p: &Value, world: Mat4, out: &mut Model) -> Result<(), Bad> {
        // Triangles only: points and lines are not part of a controller's look.
        if uint(p, "mode").unwrap_or(4) != 4 {
            return Ok(());
        }
        let attributes = p.get("attributes");
        let attribute = |name: &str| attributes.and_then(|a| uint(a, name));
        let Some(position) = attribute("POSITION") else {
            return bad("a primitive without positions");
        };
        let positions = self.accessor(position)?;
        if positions.components != 3 {
            return bad("positions not of three numbers");
        }
        let normals = attribute("NORMAL").map(|n| self.accessor(n)).transpose()?;
        let colors = attribute("COLOR_0").map(|n| self.accessor(n)).transpose()?;
        let uvs = attribute("TEXCOORD_0")
            .map(|n| self.accessor(n))
            .transpose()?;
        let indices = p.get("indices").and_then(Value::as_u64);
        let indices = indices.map(|n| self.accessor(n as usize)).transpose()?;
        let corners = positions.count;
        for a in [&normals, &colors, &uvs].into_iter().flatten() {
            if a.count < corners {
                return bad("an attribute with fewer corners than the positions");
            }
        }
        let first = out.mesh.positions.len();
        let triangles = indices.as_ref().map_or(corners, |i| i.count) / 3;
        if first + corners > MAX_VERTICES || out.mesh.triangles() + triangles > MAX_TRIANGLES {
            return bad("too large a model");
        }

        // The material: its base colour, and the picture it comes from.
        let material = uint(p, "material").and_then(|m| self.list("materials").get(m));
        let pbr = material.and_then(|m| m.get("pbrMetallicRoughness"));
        let base = pbr
            .and_then(|m| floats::<4>(m, "baseColorFactor"))
            .unwrap_or([1.0; 4]);
        let image = pbr
            .and_then(|m| m.get("baseColorTexture"))
            .and_then(|t| uint(t, "index"))
            .and_then(|t| self.list("textures").get(t))
            .and_then(|t| {
                uint(t, "source").or_else(|| {
                    t.get("extensions")
                        .and_then(|e| e.get("KHR_texture_basisu"))
                        .and_then(|b| uint(b, "source"))
                })
            })
            .filter(|i| *i < self.list("images").len());

        let normal_matrix = Mat3::from_mat4(world).inverse().transpose();
        let flip = Mat3::from_mat4(world).determinant() < 0.0;
        let mut local_indices = Vec::with_capacity(triangles * 3);
        match &indices {
            Some(ix) => {
                if ix.components != 1 || ![5121, 5123, 5125].contains(&ix.kind) {
                    return bad("indices not of whole numbers");
                }
                for i in 0..triangles * 3 {
                    let k = ix.index(i);
                    if k as usize >= corners {
                        return bad("an index past the corners");
                    }
                    local_indices.push(k);
                }
            }
            None => local_indices.extend(0..(triangles * 3) as u32),
        }
        if flip {
            for t in local_indices.as_chunks_mut::<3>().0 {
                t.swap(1, 2);
            }
        }
        let mut placed = Vec::with_capacity(corners);
        for i in 0..corners {
            let p = world.transform_point3(Vec3::from(positions.vec::<3>(i)));
            if !p.is_finite() {
                return bad("a corner out of reach");
            }
            placed.push(p.to_array());
        }
        let made = match normals {
            Some(_) => Vec::new(),
            None => smooth_normals(&placed, &local_indices),
        };
        for (i, p) in placed.iter().enumerate() {
            out.mesh.positions.push(*p);
            out.mesh.normals.push(match &normals {
                Some(n) => (normal_matrix * Vec3::from(n.vec::<3>(i)))
                    .try_normalize()
                    .unwrap_or(Vec3::Z)
                    .to_array(),
                None => made[i],
            });
            let c = colors.as_ref().map_or([1.0; 4], |c| c.vec::<4>(i));
            out.mesh
                .colors
                .push(std::array::from_fn(|k| base[k] * c[k]));
            out.uvs
                .push(uvs.as_ref().map_or([0.0; 2], |u| u.vec::<2>(i)));
        }
        out.mesh
            .indices
            .extend(local_indices.iter().map(|k| k + first as u32));
        if let Some(image) = image {
            out.parts.push(Part {
                corners: first..first + corners,
                image,
            });
        }
        Ok(())
    }

    /// A material's picture (empty when it cannot be found).
    fn image(&self, image: &Value) -> Image {
        let mime = image.get("mimeType").and_then(Value::as_str).unwrap_or("");
        let bytes = uint(image, "bufferView")
            .and_then(|v| self.view(v).ok())
            .map_or(&[][..], |(b, _)| b);
        let kind = match mime {
            "image/png" => ImageKind::Png,
            "image/jpeg" => ImageKind::Jpeg,
            "image/ktx2" => ImageKind::Ktx2,
            _ if bytes.starts_with(b"\x89PNG") => ImageKind::Png,
            _ if bytes.starts_with(&[0xFF, 0xD8]) => ImageKind::Jpeg,
            _ if bytes.starts_with(b"\xABKTX 20") => ImageKind::Ktx2,
            _ => ImageKind::Other,
        };
        Image {
            kind,
            bytes: bytes.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made by tests/models/make_sample.py (see there what it holds).
    const SAMPLE: &[u8] = include_bytes!("../../tests/models/sample.glb");

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    #[test]
    fn the_sample_reads_with_its_nodes_placed() {
        let m = Model::read(SAMPLE).expect("the sample reads");
        let mesh = &m.mesh;
        // The square, and the two triangles of the node in the scene; not the node
        // outside it.
        assert_eq!(mesh.positions.len(), 4 + 3 + 3);
        assert_eq!(mesh.triangles(), 2 + 1 + 1);
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.colors.len(), mesh.positions.len());
        // The square: scaled by 2, turned a quarter round z, raised by the root.
        assert!(
            close(mesh.positions[1], [1.0, 2.0, 0.0]),
            "{:?}",
            mesh.positions[1]
        );
        assert!(
            close(mesh.positions[3], [-1.0, 0.0, 0.0]),
            "{:?}",
            mesh.positions[3]
        );
        assert!(close(mesh.normals[0], [0.0, 0.0, 1.0]));
        // The triangle: moved by the matrix and the root; its normals made.
        assert!(close(mesh.positions[4], [3.0, 1.0, 0.0]));
        assert!(close(mesh.positions[6], [3.0, 2.0, 0.0]));
        assert!(close(mesh.normals[5], [0.0, 0.0, 1.0]));
        assert_eq!(mesh.colors[4], [0.5, 0.25, 1.0, 1.0]);
        // Indices of each primitive past the ones before.
        assert_eq!(&mesh.indices[..6], &[0, 1, 2, 0, 2, 3]);
        assert_eq!(&mesh.indices[6..], &[4, 5, 6, 7, 8, 9]);
        let (lo, hi) = mesh.bounds().expect("corners");
        assert!(
            close(lo, [-1.0, 0.0, 0.0]) && close(hi, [4.0, 2.0, 0.0]),
            "{lo:?} {hi:?}"
        );
        // The pictures: a PNG and a KTX2.
        let kinds: Vec<_> = m.images.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [ImageKind::Png, ImageKind::Ktx2]);
        assert!(m.images[0].bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn a_picture_paints_its_corners_and_a_tint_the_others() {
        let mut m = Model::read(SAMPLE).expect("the sample reads");
        // Red, green; blue, white (as the sample's PNG).
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        m.paint(0, 2, 2, &rgba);
        let c = &m.mesh.colors;
        assert_eq!(c[0], [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(c[1], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(c[2], [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(c[3], [1.0, 0.0, 0.0, 1.0]);
        // The one-colour triangle is not the picture's.
        assert_eq!(c[4], [0.5, 0.25, 1.0, 1.0]);
        // The KTX2 triangle is tinted instead.
        m.tint(1, [0.2, 0.2, 0.2]);
        assert_eq!(m.mesh.colors[7], [0.2, 0.2, 0.2, 1.0]);
        assert_eq!(m.mesh.colors[4], [0.5, 0.25, 1.0, 1.0]);
        // A picture of the wrong size changes nothing.
        let before = m.mesh.colors.clone();
        m.paint(0, 3, 3, &rgba);
        assert_eq!(m.mesh.colors, before);
    }

    /// Whatever a runtime gives, reading never panics: every cut of the sample, and
    /// the sample with each byte of its binary part changed, either reads or is an
    /// error.
    #[test]
    fn broken_models_are_errors_not_panics() {
        for len in 0..SAMPLE.len() {
            let _ = Model::read(&SAMPLE[..len]);
        }
        let mut bytes = SAMPLE.to_vec();
        for i in 12..bytes.len() {
            let was = bytes[i];
            for b in [0x00, 0xFF, was ^ 0x10] {
                bytes[i] = b;
                let _ = Model::read(&bytes);
            }
            bytes[i] = was;
        }
        assert!(Model::read(b"not a model").is_err());
        assert!(Model::read(&[]).is_err());
    }

    #[test]
    fn texels_repeat_and_stay_within_the_picture() {
        assert_eq!(texel(0.0, 4), 0);
        assert_eq!(texel(1.0, 4), 3);
        assert_eq!(texel(1.25, 4), 1);
        assert_eq!(texel(-0.25, 4), 3);
        assert_eq!(texel(f32::NAN, 4), 0);
    }
}
