//! The 3D models the hands' layer draws (step 8.6): a controller as the runtime gives
//! it (a glTF binary, read by [`glb`]) and a hand as the runtime gives it (a mesh with
//! its bones, skinned by [`hand`]). Who gave a model the core does not know: the
//! shell asks the runtime once, when the controllers or hands come, hands the bytes
//! or the mesh here, and puts what comes out on the GPU; a frame only moves it.
//!
//! All of it is plain data and arithmetic: read, checked and tested on any system.

#![forbid(unsafe_code)]

pub mod glb;
pub mod hand;

use std::fmt;

/// A model that cannot be drawn: what was wrong, for the log. The shell then draws
/// what it drew without a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bad(pub String);

impl fmt::Display for Bad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Bad {}

fn bad<T>(what: impl Into<String>) -> Result<T, Bad> {
    Err(Bad(what.into()))
}

/// The most triangles a model may have: far more than any controller the runtimes
/// give (the step 8 note's budget is 10 000), little enough for the hands' layer.
pub const MAX_TRIANGLES: usize = 60_000;
/// The most corners a model may have.
pub const MAX_VERTICES: usize = 65_536;

/// A model ready for the GPU, in its own space (metres; for a controller, its grip):
/// a corner's place, normal and colour (linear, straight alpha), three corners to a
/// triangle, counter-clockwise from the front.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    /// The smallest box round it: lowest and highest corner.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = *self.positions.first()?;
        Some(self.positions.iter().fold((first, first), |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
            )
        }))
    }
}

/// Normals for a mesh that has none: each corner's the mean of its triangles' (by
/// their area), so a smooth model looks smooth and a sharp edge stays nearly so.
pub fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    use glam::Vec3;
    let mut sum = vec![Vec3::ZERO; positions.len()];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| i as usize);
        let (Some(pa), Some(pb), Some(pc)) = (positions.get(a), positions.get(b), positions.get(c))
        else {
            continue;
        };
        let [pa, pb, pc] = [pa, pb, pc].map(|p| Vec3::from_array(*p));
        let n = (pb - pa).cross(pc - pa);
        for i in [a, b, c] {
            sum[i] += n;
        }
    }
    sum.into_iter()
        .map(|n| n.try_normalize().unwrap_or(Vec3::Z).to_array())
        .collect()
}

/// A colour of an sRGB picture (0 to 255) in linear light.
pub fn linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normals_of_a_flat_square_face_the_viewer() {
        let positions = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let n = smooth_normals(&positions, &[0, 1, 2, 0, 2, 3]);
        assert_eq!(n, vec![[0.0, 0.0, 1.0]; 4]);
        // An index past the corners is passed over, and a corner of no triangle
        // still gets a normal.
        let n = smooth_normals(&positions, &[0, 1, 9]);
        assert_eq!(n.len(), 4);
    }

    #[test]
    fn srgb_to_linear() {
        assert_eq!(linear(0), 0.0);
        assert!((linear(255) - 1.0).abs() < 1e-6);
        assert!((linear(188) - 0.5).abs() < 0.01);
    }
}
