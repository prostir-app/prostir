//! A hand as the runtime models it (the step 8 note, 9.1): its skin's corners, each
//! moved by up to four of the 26 joints of OpenXR's hand (`XR_EXT_hand_tracking`'s
//! order) by their weights, and where each joint is in the mesh's own space (its
//! bind pose). The shell puts the mesh on the GPU once; each frame [`HandMesh::skin`]
//! turns the joints as they are tracked into the 26 matrices the vertex shader moves
//! the corners by.

use glam::{Mat4, Quat, Vec3};

use super::{Bad, MAX_TRIANGLES, MAX_VERTICES, bad};

/// The joints of a hand, as OpenXR numbers them.
pub const JOINTS: usize = 26;
/// The wrist, the middle finger's metacarpal and its first bone's joint: their
/// distances say how much larger the tracked hand is than the model's.
const WRIST: usize = 1;
const MIDDLE_METACARPAL: usize = 11;
const MIDDLE_PROXIMAL: usize = 12;
/// How much larger or smaller than the model a hand may be drawn.
const SCALE_RANGE: (f32, f32) = (0.6, 1.6);

/// A joint's pose: where, and turned how.
pub type Pose = (Vec3, Quat);

/// A hand's mesh and its bones, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct HandMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// The four joints that move each corner, and by how much (adding up to 1).
    pub joints: Vec<[u8; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// Each joint's bind pose, inverted: from the mesh's space into the joint's.
    bind_inverse: [Mat4; JOINTS],
    /// The length from the wrist to the middle finger's first joint, in the model.
    reach: f32,
}

/// What the runtime gives for a hand's mesh, as it gives it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Given {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub joints: Vec<[i16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<i16>,
    pub bind: Vec<Pose>,
}

impl HandMesh {
    /// The mesh `given`, checked: as many of each as corners, joints and indices within
    /// their ranges, weights that add up to 1 (a corner of no weight follows the
    /// wrist).
    pub fn new(given: Given) -> Result<Self, Bad> {
        let n = given.positions.len();
        if n == 0 || given.indices.is_empty() {
            return bad("an empty hand");
        }
        if given.normals.len() != n || given.joints.len() != n || given.weights.len() != n {
            return bad("a hand's corners of different counts");
        }
        if given.bind.len() != JOINTS {
            return bad(format!("a hand of {} joints", given.bind.len()));
        }
        if n > MAX_VERTICES || given.indices.len() / 3 > MAX_TRIANGLES {
            return bad("too large a hand");
        }
        if !given.indices.len().is_multiple_of(3) {
            return bad("a hand's triangles cut short");
        }
        let mut indices = Vec::with_capacity(given.indices.len());
        for &i in &given.indices {
            // The runtime's indices are 16-bit, signed in the type: read them as
            // unsigned, so a mesh of up to 65 536 corners is whole.
            let i = u32::from(i as u16);
            if i as usize >= n {
                return bad("a hand's index past its corners");
            }
            indices.push(i);
        }
        let mut joints = Vec::with_capacity(n);
        let mut weights = Vec::with_capacity(n);
        for (j, w) in given.joints.iter().zip(&given.weights) {
            let mut jj = [WRIST as u8; 4];
            let mut ww = [0.0f32; 4];
            for k in 0..4 {
                if (0..JOINTS as i16).contains(&j[k]) && w[k].is_finite() && w[k] > 0.0 {
                    jj[k] = j[k] as u8;
                    ww[k] = w[k];
                }
            }
            let sum: f32 = ww.iter().sum();
            if sum > 1e-6 {
                for w in &mut ww {
                    *w /= sum;
                }
            } else {
                ww = [1.0, 0.0, 0.0, 0.0];
                jj = [WRIST as u8; 4];
            }
            joints.push(jj);
            weights.push(ww);
        }
        let all_finite = |p: &[f32; 3]| p.iter().all(|x| x.is_finite());
        if !given.positions.iter().all(all_finite) || !given.normals.iter().all(all_finite) {
            return bad("a hand's corner out of reach");
        }
        let mut bind_inverse = [Mat4::IDENTITY; JOINTS];
        for (inv, (p, q)) in bind_inverse.iter_mut().zip(&given.bind) {
            if !p.is_finite() || !q.is_finite() || q.length_squared() < 1e-6 {
                return bad("a hand's joint out of reach");
            }
            *inv = Mat4::from_rotation_translation(q.normalize(), *p).inverse();
        }
        let points: Vec<Vec3> = given.bind.iter().map(|(p, _)| *p).collect();
        let reach = reach(&points);
        if reach.is_nan() || reach <= 1e-4 {
            return bad("a hand with no reach");
        }
        Ok(Self {
            positions: given.positions,
            normals: given.normals,
            joints,
            weights,
            indices,
            bind_inverse,
            reach,
        })
    }

    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    /// How much larger the hand tracked (`joints`) is than the model, within
    /// [`SCALE_RANGE`]: the model is of a hand of usual size, the tracked one as large
    /// as the person's.
    pub fn scale(&self, joints: &[Pose; JOINTS]) -> f32 {
        let points: [Vec3; JOINTS] = std::array::from_fn(|i| joints[i].0);
        let s = reach(&points) / self.reach;
        if s.is_finite() {
            s.clamp(SCALE_RANGE.0, SCALE_RANGE.1)
        } else {
            1.0
        }
    }

    /// The matrices that move the mesh's corners onto the hand tracked: from the
    /// mesh's space into each joint's, scaled to the hand, out to where the joint is
    /// now. Into `out`, kept by the caller: a frame takes no memory.
    pub fn skin(&self, joints: &[Pose; JOINTS], out: &mut [Mat4; JOINTS]) {
        let scale = Mat4::from_scale(Vec3::splat(self.scale(joints)));
        for ((o, (p, q)), inv) in out.iter_mut().zip(joints).zip(&self.bind_inverse) {
            let q = if q.length_squared() > 1e-6 {
                q.normalize()
            } else {
                Quat::IDENTITY
            };
            *o = Mat4::from_rotation_translation(q, *p) * scale * *inv;
        }
    }
}

/// The length from the wrist to the middle finger's first joint.
fn reach(points: &[Vec3]) -> f32 {
    points[WRIST].distance(points[MIDDLE_METACARPAL])
        + points[MIDDLE_METACARPAL].distance(points[MIDDLE_PROXIMAL])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand of joints along y, a centimetre and a half apart, a triangle on the
    /// wrist and one on the middle finger's first joint.
    fn given() -> Given {
        let bind: Vec<Pose> = (0..JOINTS)
            .map(|i| (Vec3::new(0.0, i as f32 * 0.015, 0.0), Quat::IDENTITY))
            .collect();
        let at = |j: usize| bind[j].0;
        let corner = |j: usize, dx: f32| (at(j) + Vec3::new(dx, 0.0, 0.01)).to_array();
        Given {
            positions: vec![
                corner(1, 0.0),
                corner(1, 0.01),
                corner(1, -0.01),
                corner(12, 0.0),
                corner(12, 0.01),
                corner(12, -0.01),
            ],
            normals: vec![[0.0, 0.0, 1.0]; 6],
            joints: vec![
                [1, 0, 0, 0],
                [1, 2, -1, 40],
                [1, 0, 0, 0],
                [12, 0, 0, 0],
                [12, 11, 0, 0],
                [0; 4],
            ],
            weights: vec![
                [1.0, 0.0, 0.0, 0.0],
                [2.0, 2.0, 5.0, 5.0],
                [0.0; 4],
                [1.0, 0.0, 0.0, 0.0],
                [0.5, 0.5, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            bind,
        }
    }

    fn bind_poses() -> [Pose; JOINTS] {
        std::array::from_fn(|i| (Vec3::new(0.0, i as f32 * 0.015, 0.0), Quat::IDENTITY))
    }

    /// Where `corner` of `mesh` goes by the matrices `m`, as the shader moves it.
    fn moved(mesh: &HandMesh, m: &[Mat4; JOINTS], corner: usize) -> Vec3 {
        let p = Vec3::from(mesh.positions[corner]);
        (0..4)
            .map(|k| {
                let j = mesh.joints[corner][k] as usize;
                m[j].transform_point3(p) * mesh.weights[corner][k]
            })
            .sum()
    }

    #[test]
    fn weights_and_joints_are_made_whole() {
        let h = HandMesh::new(given()).expect("a hand");
        // Joints out of range and weights of nothing are left out, the rest add up
        // to 1.
        assert_eq!(h.joints[1], [1, 2, 1, 1]);
        assert_eq!(h.weights[1], [0.5, 0.5, 0.0, 0.0]);
        // A corner of no weight at all follows the wrist.
        assert_eq!((h.joints[2], h.weights[2]), ([1; 4], [1.0, 0.0, 0.0, 0.0]));
        assert_eq!(h.triangles(), 2);
    }

    #[test]
    fn a_hand_at_its_bind_pose_stays_and_one_moved_moves() {
        let h = HandMesh::new(given()).expect("a hand");
        let mut m = [Mat4::ZERO; JOINTS];
        h.skin(&bind_poses(), &mut m);
        for i in 0..h.positions.len() {
            let p = moved(&h, &m, i);
            assert!(p.distance(Vec3::from(h.positions[i])) < 1e-5, "{i}: {p}");
        }
        // The whole hand half a metre forward and turned: each corner with it.
        let turn = Quat::from_rotation_y(1.0);
        let shift = Vec3::new(0.1, 1.2, -0.5);
        let poses: [Pose; JOINTS] = bind_poses().map(|(p, q)| (turn * p + shift, turn * q));
        h.skin(&poses, &mut m);
        for i in 0..h.positions.len() {
            let want = turn * Vec3::from(h.positions[i]) + shift;
            assert!(moved(&h, &m, i).distance(want) < 1e-5, "{i}");
        }
    }

    #[test]
    fn a_larger_hand_draws_the_mesh_larger() {
        let h = HandMesh::new(given()).expect("a hand");
        let poses = bind_poses().map(|(p, q)| (p * 1.2, q));
        assert!((h.scale(&poses) - 1.2).abs() < 1e-4);
        // Round each joint: a corner a centimetre off the wrist ends 1.2 cm off it.
        let mut m = [Mat4::ZERO; JOINTS];
        h.skin(&poses, &mut m);
        let off = moved(&h, &m, 0) - poses[1].0;
        assert!((off.length() - 0.012).abs() < 1e-5, "{off}");
        // No larger than the range.
        let huge = bind_poses().map(|(p, q)| (p * 10.0, q));
        assert_eq!(h.scale(&huge), SCALE_RANGE.1);
    }

    #[test]
    fn broken_hands_are_refused() {
        let mut g = given();
        g.indices.push(6);
        g.indices.extend([0, 0]);
        assert!(HandMesh::new(g).is_err());
        let mut g = given();
        g.bind.pop();
        assert!(HandMesh::new(g).is_err());
        let mut g = given();
        g.normals.pop();
        assert!(HandMesh::new(g).is_err());
        let mut g = given();
        g.positions[0][1] = f32::NAN;
        assert!(HandMesh::new(g).is_err());
        assert!(HandMesh::new(Given::default()).is_err());
    }
}
