//! The only thing this app renders itself: tracked hands, pointer rays and the effects
//! that answer them (the core's `view::fx`), drawn as shaded sphere impostors, soft
//! lights and rings into a transparent projection layer on top of the monitors; and
//! the runtime's models of the controllers and hands (step 8.6), put on the GPU once
//! when they come, drawn with depth where the controller is held and skinned onto the
//! hand's joints. Frames with nothing to show render nothing. The instances go into a
//! buffer made once (larger only if a frame ever needs more), and a model's matrices
//! into an array kept, so a frame takes no memory.

use std::ptr;

use anyhow::{Context, Result, anyhow};
use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;
use openxr as xr;

use crate::model::Mesh;
use crate::model::hand::{HandMesh, JOINTS, Pose};
use crate::platform::openxr::{Gfx, quat, vec3};
use crate::view::fx::{self, Fx, Kind, Loaded, Meshes};

/// Floats of one instance as the shader takes them: centre and radius, colour, kind.
const FLOATS: usize = 9;

const VERTEX: &str = r"#version 300 es
layout(location = 0) in vec2 corner;
layout(location = 1) in vec4 sphere;
layout(location = 2) in vec4 color;
layout(location = 3) in float kind;
uniform mat4 view;
uniform mat4 proj;
out vec2 v_uv;
out vec4 v_color;
flat out float v_kind;
void main() {
    vec4 c = view * vec4(sphere.xyz, 1.0);
    c.xy += corner * sphere.w;
    gl_Position = proj * c;
    v_uv = corner;
    v_color = color;
    v_kind = kind;
}
";

const FRAGMENT: &str = r"#version 300 es
precision mediump float;
in vec2 v_uv;
in vec4 v_color;
flat in float v_kind;
out vec4 o;
void main() {
    float r2 = dot(v_uv, v_uv);
    if (r2 > 1.0) discard;
    float r = sqrt(r2);
    if (v_kind > 1.5) {
        // Ring: a thin band near the edge.
        float band = smoothstep(0.72, 0.84, r) * (1.0 - smoothstep(0.88, 1.0, r));
        float a = v_color.a * band;
        o = vec4(v_color.rgb * a, a * 0.5);
        return;
    }
    if (v_kind > 0.5) {
        // Glow: bright in the middle, fading out; mostly added light, so the menu
        // under it lights up rather than being covered.
        float f = 1.0 - r;
        float a = v_color.a * f * f;
        o = vec4(v_color.rgb * a, a * 0.25);
        return;
    }
    vec3 n = vec3(v_uv, sqrt(1.0 - r2));
    float light = 0.45 + 0.55 * max(dot(n, normalize(vec3(-0.35, 0.6, 0.7))), 0.0);
    float edge = 1.0 - smoothstep(0.82, 1.0, r2);
    float a = v_color.a * edge;
    o = vec4(v_color.rgb * light * a, a);
}
";

/// The models: each corner moved by up to four matrices of `bones` by its weights (a
/// controller: by the first alone, where it is held), lit from above; a hand as glass.
const MESH_VERTEX: &str = r"#version 300 es
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 normal;
layout(location = 2) in vec4 color;
layout(location = 3) in vec4 joints;
layout(location = 4) in vec4 weights;
uniform mat4 bones[26];
uniform mat4 view;
uniform mat4 proj;
out vec3 v_normal;
out vec3 v_at;
out vec4 v_color;
void main() {
    mat4 skin = bones[int(joints.x)] * weights.x + bones[int(joints.y)] * weights.y
        + bones[int(joints.z)] * weights.z + bones[int(joints.w)] * weights.w;
    vec4 eye = view * (skin * vec4(position, 1.0));
    v_at = eye.xyz;
    v_normal = mat3(view) * mat3(skin) * normal;
    v_color = color;
    gl_Position = proj * eye;
}
";

const MESH_FRAGMENT: &str = r"#version 300 es
precision mediump float;
in vec3 v_normal;
in vec3 v_at;
in vec4 v_color;
uniform vec4 tint;
uniform float glass;
out vec4 o;
void main() {
    vec3 n = normalize(v_normal);
    vec3 to_eye = normalize(-v_at);
    // Towards the eye, whichever way the model winds its triangles.
    if (dot(n, to_eye) < 0.0) n = -n;
    float light = 0.45 + 0.55 * max(dot(n, normalize(vec3(-0.35, 0.6, 0.7))), 0.0);
    vec4 c = v_color * tint;
    if (glass > 0.5) {
        // Glass: faint in the middle, its edge brighter where it turns from the eye.
        float rim = pow(1.0 - max(dot(n, to_eye), 0.0), 2.0);
        float a = clamp(c.a * (0.55 + 0.9 * rim), 0.0, 1.0);
        vec3 rgb = c.rgb * (0.65 + 0.35 * light) + vec3(0.3 * rim);
        o = vec4(rgb * a, a);
        return;
    }
    o = vec4(c.rgb * light * c.a, c.a);
}
";

/// Floats of a controller's corner: place, normal, colour.
const STATIC_FLOATS: usize = 10;
/// Floats of a hand's corner: place, normal, colour, joints, weights.
const SKINNED_FLOATS: usize = 18;

/// A model on the GPU: its corners and triangles, drawn in one call.
struct Gpu {
    vao: glow::VertexArray,
    buffers: [glow::Buffer; 2],
    count: i32,
}

impl Gpu {
    /// `corners` (the attributes in the shader's order, [`SKINNED_FLOATS`] to a corner
    /// when `skinned`, [`STATIC_FLOATS`] otherwise) and `indices` on the GPU.
    fn new(gl: &glow::Context, corners: &[f32], indices: &[u32], skinned: bool) -> Result<Self> {
        let floats = if skinned {
            SKINNED_FLOATS
        } else {
            STATIC_FLOATS
        };
        let count = i32::try_from(indices.len()).context("too many indices")?;
        // SAFETY: plain GL object creation and uploads on the current context; each
        // attribute lies within a corner's floats.
        unsafe {
            let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
            gl.bind_vertex_array(Some(vao));
            let vbo = gl.create_buffer().map_err(|e| anyhow!(e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes(corners), glow::STATIC_DRAW);
            let stride = (floats * 4) as i32;
            let attributes: &[(u32, i32, i32)] = if skinned {
                &[(0, 3, 0), (1, 3, 12), (2, 4, 24), (3, 4, 40), (4, 4, 56)]
            } else {
                &[(0, 3, 0), (1, 3, 12), (2, 4, 24)]
            };
            for &(loc, size, offset) in attributes {
                gl.enable_vertex_attrib_array(loc);
                gl.vertex_attrib_pointer_f32(loc, size, glow::FLOAT, false, stride, offset);
            }
            let ibo = gl.create_buffer().map_err(|e| anyhow!(e))?;
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ibo));
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                words(indices),
                glow::STATIC_DRAW,
            );
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            Ok(Self {
                vao,
                buffers: [vbo, ibo],
                count,
            })
        }
    }

    fn delete(self, gl: &glow::Context) {
        // SAFETY: objects this model made, on the current context.
        unsafe {
            gl.delete_vertex_array(self.vao);
            for b in self.buffers {
                gl.delete_buffer(b);
            }
        }
    }

    fn draw(&self, gl: &glow::Context) {
        // SAFETY: the model's own vertex array; its indices lie within its corners
        // (checked by `model` when it was read).
        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_elements(glow::TRIANGLES, self.count, glow::UNSIGNED_INT, 0);
        }
    }
}

/// Where the models go this frame, as the shell read it: each controller's grip and
/// each hand's joints, in the layer's space.
#[derive(Debug, Clone, Copy, Default)]
pub struct Poses {
    pub grips: [Option<xr::Posef>; 2],
    pub joints: [Option<[Pose; JOINTS]>; 2],
}

/// The models' program and its uniforms.
struct MeshProgram {
    program: glow::Program,
    view: glow::UniformLocation,
    proj: glow::UniformLocation,
    bones: glow::UniformLocation,
    tint: glow::UniformLocation,
    glass: glow::UniformLocation,
}

struct Eye {
    swapchain: xr::Swapchain<Gfx>,
    framebuffers: Vec<glow::Framebuffer>,
}

pub struct Overlay {
    program: glow::Program,
    vao: glow::VertexArray,
    instances: glow::Buffer,
    view_loc: glow::UniformLocation,
    proj_loc: glow::UniformLocation,
    eyes: Vec<Eye>,
    width: u32,
    height: u32,
    /// Whether the last frame drew something (and the layer should be shown).
    drawn: bool,
    /// The instances as the shader takes them, kept from frame to frame and filled
    /// again.
    data: Vec<f32>,
    /// How many instances the GL buffer holds.
    room: usize,
    meshes: MeshProgram,
    /// Each side's controller as the runtime models it, and its key (the same key is
    /// the same model, not put on the GPU again).
    controllers: [Option<(u64, Gpu)>; 2],
    /// Each side's hand as the runtime models it, and its bones.
    hands: [Option<(HandMesh, Gpu)>; 2],
    /// The matrices of a model this frame, kept, as the shader takes them.
    bones: [f32; JOINTS * 16],
}

impl Overlay {
    pub fn new(
        gl: &glow::Context,
        instance: &xr::Instance,
        system: xr::SystemId,
        session: &xr::Session<Gfx>,
        format: u32,
    ) -> Result<Self> {
        let views = instance.enumerate_view_configuration_views(
            system,
            xr::ViewConfigurationType::PRIMARY_STEREO,
        )?;
        let view = views.first().context("no stereo views")?;
        // Hands and dots do not need full resolution; the compositor scales them up.
        let width = (view.recommended_image_rect_width as f32 * 0.7) as u32;
        let height = (view.recommended_image_rect_height as f32 * 0.7) as u32;
        // SAFETY: plain GL object creation on the current context.
        unsafe {
            let program = program_of(gl, VERTEX, FRAGMENT)?;
            let mesh_program = program_of(gl, MESH_VERTEX, MESH_FRAGMENT)?;
            let uniform = |name: &str| {
                gl.get_uniform_location(mesh_program, name)
                    .with_context(|| format!("{name} uniform"))
            };
            let meshes = MeshProgram {
                program: mesh_program,
                view: uniform("view")?,
                proj: uniform("proj")?,
                bones: uniform("bones")?,
                tint: uniform("tint")?,
                glass: uniform("glass")?,
            };
            let view_loc = gl
                .get_uniform_location(program, "view")
                .context("view uniform")?;
            let proj_loc = gl
                .get_uniform_location(program, "proj")
                .context("proj uniform")?;

            let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
            gl.bind_vertex_array(Some(vao));
            let quad = gl.create_buffer().map_err(|e| anyhow!(e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
            let corners: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes(&corners), glow::STATIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            let instances = gl.create_buffer().map_err(|e| anyhow!(e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(instances));
            gl.buffer_data_size(
                glow::ARRAY_BUFFER,
                (fx::CAPACITY * FLOATS * 4) as i32,
                glow::DYNAMIC_DRAW,
            );
            for (loc, size, offset) in [(1, 4, 0), (2, 4, 16), (3, 1, 32)] {
                gl.enable_vertex_attrib_array(loc);
                gl.vertex_attrib_pointer_f32(
                    loc,
                    size,
                    glow::FLOAT,
                    false,
                    (FLOATS * 4) as i32,
                    offset,
                );
                gl.vertex_attrib_divisor(loc, 1);
            }
            gl.bind_vertex_array(None);

            let mut eyes = Vec::new();
            for _ in 0..2 {
                let swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
                    create_flags: xr::SwapchainCreateFlags::EMPTY,
                    usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                        | xr::SwapchainUsageFlags::SAMPLED,
                    format,
                    sample_count: 1,
                    width,
                    height,
                    face_count: 1,
                    array_size: 1,
                    mip_count: 1,
                })?;
                let images = swapchain.enumerate_images()?;
                // The depth the models are drawn with, one for the eye's images.
                let depth = gl.create_renderbuffer().map_err(|e| anyhow!(e))?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
                gl.renderbuffer_storage(
                    glow::RENDERBUFFER,
                    glow::DEPTH_COMPONENT24,
                    width as i32,
                    height as i32,
                );
                gl.bind_renderbuffer(glow::RENDERBUFFER, None);
                let mut framebuffers = Vec::new();
                for &image in &images {
                    let fb = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
                    let texture = glow::NativeTexture(
                        std::num::NonZeroU32::new(image).context("null texture")?,
                    );
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(texture),
                        0,
                    );
                    gl.framebuffer_renderbuffer(
                        glow::FRAMEBUFFER,
                        glow::DEPTH_ATTACHMENT,
                        glow::RENDERBUFFER,
                        Some(depth),
                    );
                    framebuffers.push(fb);
                }
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                eyes.push(Eye {
                    swapchain,
                    framebuffers,
                });
            }
            Ok(Self {
                program,
                vao,
                instances,
                view_loc,
                proj_loc,
                eyes,
                width,
                height,
                drawn: false,
                data: Vec::with_capacity(fx::CAPACITY * FLOATS),
                room: fx::CAPACITY,
                meshes,
                controllers: [None, None],
                hands: [None, None],
                bones: [0.0; JOINTS * 16],
            })
        }
    }

    /// Controller `side`'s model as the runtime gave it (`key`, the same key is the
    /// same model), put on the GPU (`None`: none any more).
    pub fn set_controller(
        &mut self,
        gl: &glow::Context,
        side: usize,
        model: Option<(u64, &Mesh)>,
    ) -> Result<()> {
        let key = model.map(|(k, _)| k);
        if self.controllers[side].as_ref().map(|(k, _)| *k) == key {
            return Ok(());
        }
        if let Some((_, old)) = self.controllers[side].take() {
            old.delete(gl);
        }
        let Some((key, mesh)) = model else {
            return Ok(());
        };
        let mut corners = Vec::with_capacity(mesh.positions.len() * STATIC_FLOATS);
        for ((p, n), c) in mesh.positions.iter().zip(&mesh.normals).zip(&mesh.colors) {
            corners.extend_from_slice(p);
            corners.extend_from_slice(n);
            corners.extend_from_slice(c);
        }
        self.controllers[side] = Some((key, Gpu::new(gl, &corners, &mesh.indices, false)?));
        Ok(())
    }

    /// Hand `side`'s model as the runtime gave it, put on the GPU (`None`: none).
    pub fn set_hand(
        &mut self,
        gl: &glow::Context,
        side: usize,
        mesh: Option<HandMesh>,
    ) -> Result<()> {
        if let Some((_, old)) = self.hands[side].take() {
            old.delete(gl);
        }
        let Some(mesh) = mesh else {
            return Ok(());
        };
        let mut corners = Vec::with_capacity(mesh.positions.len() * SKINNED_FLOATS);
        for (((p, n), j), w) in mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .zip(&mesh.joints)
            .zip(&mesh.weights)
        {
            corners.extend_from_slice(p);
            corners.extend_from_slice(n);
            corners.extend_from_slice(&[1.0; 4]);
            corners.extend(j.iter().map(|j| f32::from(*j)));
            corners.extend_from_slice(w);
        }
        let gpu = Gpu::new(gl, &corners, &mesh.indices, true)?;
        self.hands[side] = Some((mesh, gpu));
        Ok(())
    }

    /// The models on the GPU, for the core to draw them in place of balls and dots.
    pub fn loaded(&self) -> Loaded {
        Loaded {
            controllers: [0, 1].map(|h| self.controllers[h].is_some()),
            hands: [0, 1].map(|h| self.hands[h].is_some()),
        }
    }

    /// Draws `list` and the models `meshes` says (where `poses` puts them) for both
    /// eyes. With nothing to draw, nothing is rendered and [`Overlay::layer_views`]
    /// returns `None`.
    pub fn render(
        &mut self,
        gl: &glow::Context,
        views: &[xr::View],
        (list, meshes): (&mut [Fx], &Meshes),
        poses: &Poses,
    ) -> Result<()> {
        self.drawn = false;
        if views.len() < 2 {
            return Ok(());
        }
        // The models that can be drawn: a controller where it is held, a hand whose
        // joints are all tracked (moved as the core says).
        let controllers: [Option<Mat4>; 2] = [0, 1].map(|h| {
            let grip = poses.grips[h].filter(|_| meshes.controllers[h])?;
            self.controllers[h].as_ref()?;
            Some(Mat4::from_rotation_translation(
                quat(grip.orientation),
                vec3(grip.position),
            ))
        });
        let hands = [0, 1].map(|h| {
            let draw = meshes.hands[h]?;
            self.hands[h].as_ref()?;
            let joints = poses.joints[h]?.map(|(p, q)| (p + draw.push, q));
            Some((draw.color, joints))
        });
        let models = controllers.iter().any(Option::is_some) || hands.iter().any(Option::is_some);
        if list.is_empty() && !models {
            return Ok(());
        }
        for (eye, view) in self.eyes.iter_mut().zip(views) {
            let view_matrix = Mat4::from_rotation_translation(
                quat(view.pose.orientation),
                vec3(view.pose.position),
            )
            .inverse();
            let proj = projection(view.fov);
            // Far to near: correct overlap without a depth buffer (sorted in place:
            // a stable sort would take memory every frame).
            list.sort_unstable_by(|a, b| {
                let za = view_matrix.transform_point3(a.center).z;
                let zb = view_matrix.transform_point3(b.center).z;
                za.total_cmp(&zb)
            });
            self.data.clear();
            self.data.extend(list.iter().flat_map(|s| {
                let kind = match s.kind {
                    Kind::Ball => 0.0,
                    Kind::Glow => 1.0,
                    Kind::Ring => 2.0,
                };
                [
                    s.center.x, s.center.y, s.center.z, s.radius, s.color[0], s.color[1],
                    s.color[2], s.color[3], kind,
                ]
            }));
            let index = eye.swapchain.acquire_image()? as usize;
            eye.swapchain.wait_image(xr::Duration::INFINITE)?;
            // SAFETY: framebuffer of the acquired image; GL context current on this thread.
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(eye.framebuffers[index]));
                gl.viewport(0, 0, self.width as i32, self.height as i32);
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::DEPTH_TEST);
                gl.color_mask(true, true, true, true);
                gl.depth_mask(true);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear_depth_f32(1.0);
                gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            }
            if models {
                let m = &self.meshes;
                // SAFETY: the models' own program and arrays on the current context;
                // each matrix upload is whole matrices of the array the shader has.
                unsafe {
                    gl.enable(glow::DEPTH_TEST);
                    gl.depth_func(glow::LESS);
                    gl.disable(glow::CULL_FACE);
                    gl.use_program(Some(m.program));
                    gl.uniform_matrix_4_f32_slice(
                        Some(&m.view),
                        false,
                        &view_matrix.to_cols_array(),
                    );
                    gl.uniform_matrix_4_f32_slice(Some(&m.proj), false, &proj.to_cols_array());
                    // A controller's corners have no joints: all by the first matrix.
                    gl.vertex_attrib_4_f32(3, 0.0, 0.0, 0.0, 0.0);
                    gl.vertex_attrib_4_f32(4, 1.0, 0.0, 0.0, 0.0);
                    gl.uniform_1_f32(Some(&m.glass), 0.0);
                    gl.uniform_4_f32(Some(&m.tint), 1.0, 1.0, 1.0, 1.0);
                }
                for (held, model) in controllers.iter().zip(&self.controllers) {
                    let (Some(held), Some((_, gpu))) = (held, model) else {
                        continue;
                    };
                    self.bones[..16].copy_from_slice(&held.to_cols_array());
                    // SAFETY: as above.
                    unsafe {
                        gl.uniform_matrix_4_f32_slice(Some(&m.bones), false, &self.bones[..16]);
                    }
                    gpu.draw(gl);
                }
                for (hand, model) in hands.iter().zip(&self.hands) {
                    let (Some((color, joints)), Some((mesh, gpu))) = (hand, model) else {
                        continue;
                    };
                    let mut skin = [Mat4::IDENTITY; JOINTS];
                    mesh.skin(joints, &mut skin);
                    for (to, from) in self.bones.as_chunks_mut::<16>().0.iter_mut().zip(&skin) {
                        from.write_cols_to_slice(to);
                    }
                    // SAFETY: as above. Glass: first only its depth, so the fingers
                    // over one another do not darken, then its colour where it is
                    // nearest.
                    unsafe {
                        gl.uniform_matrix_4_f32_slice(Some(&m.bones), false, &self.bones);
                        gl.uniform_1_f32(Some(&m.glass), 1.0);
                        gl.uniform_4_f32(Some(&m.tint), color[0], color[1], color[2], color[3]);
                        gl.color_mask(false, false, false, false);
                        gpu.draw(gl);
                        gl.color_mask(true, true, true, true);
                        gl.depth_func(glow::LEQUAL);
                        gl.depth_mask(false);
                        gpu.draw(gl);
                        gl.depth_mask(true);
                        gl.depth_func(glow::LESS);
                    }
                }
                // SAFETY: plain GL state on the current context.
                unsafe {
                    gl.disable(glow::DEPTH_TEST);
                }
            }
            // SAFETY: the effects' own program, arrays and buffer on the current
            // context; the buffer is made larger before more than it holds is written.
            unsafe {
                if !list.is_empty() {
                    gl.use_program(Some(self.program));
                    gl.uniform_matrix_4_f32_slice(
                        Some(&self.view_loc),
                        false,
                        &view_matrix.to_cols_array(),
                    );
                    gl.uniform_matrix_4_f32_slice(
                        Some(&self.proj_loc),
                        false,
                        &proj.to_cols_array(),
                    );
                    gl.bind_vertex_array(Some(self.vao));
                    gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.instances));
                    if list.len() > self.room {
                        // More than ever before: a larger buffer, once.
                        self.room = list.len().next_power_of_two();
                        gl.buffer_data_size(
                            glow::ARRAY_BUFFER,
                            (self.room * FLOATS * 4) as i32,
                            glow::DYNAMIC_DRAW,
                        );
                    }
                    gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytes(&self.data));
                    gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, list.len() as i32);
                }
                gl.bind_vertex_array(None);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
            eye.swapchain.release_image()?;
        }
        self.drawn = true;
        Ok(())
    }

    /// Projection views for the frame just rendered, if anything was drawn.
    pub fn layer_views(
        &self,
        views: &[xr::View],
    ) -> Option<[xr::sys::CompositionLayerProjectionView; 2]> {
        if !self.drawn {
            return None;
        }
        let view = |i: usize| xr::sys::CompositionLayerProjectionView {
            ty: xr::sys::CompositionLayerProjectionView::TYPE,
            next: ptr::null(),
            pose: views[i].pose,
            fov: views[i].fov,
            sub_image: xr::sys::SwapchainSubImage {
                swapchain: self.eyes[i].swapchain.as_raw(),
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di {
                        width: self.width as i32,
                        height: self.height as i32,
                    },
                },
                image_array_index: 0,
            },
        };
        Some([view(0), view(1)])
    }
}

/// A projection layer over `views`, alpha-blended on top of what is behind it.
pub fn projection_layer(
    space: &xr::Space,
    views: &[xr::sys::CompositionLayerProjectionView; 2],
) -> xr::sys::CompositionLayerProjection {
    xr::sys::CompositionLayerProjection {
        ty: xr::sys::CompositionLayerProjection::TYPE,
        next: ptr::null(),
        layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
        space: space.as_raw(),
        view_count: 2,
        views: views.as_ptr(),
    }
}

/// OpenXR asymmetric-frustum projection for GL clip space.
fn projection(fov: xr::Fovf) -> Mat4 {
    let (l, r) = (fov.angle_left.tan(), fov.angle_right.tan());
    let (u, d) = (fov.angle_up.tan(), fov.angle_down.tan());
    let (n, f) = (0.02f32, 50.0f32);
    Mat4::from_cols(
        Vec4::new(2.0 / (r - l), 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / (u - d), 0.0, 0.0),
        Vec4::new(
            (r + l) / (r - l),
            (u + d) / (u - d),
            -(f + n) / (f - n),
            -1.0,
        ),
        Vec4::new(0.0, 0.0, -2.0 * f * n / (f - n), 0.0),
    )
}

fn bytes(v: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any bit pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), size_of_val(v)) }
}

fn words(v: &[u32]) -> &[u8] {
    // SAFETY: u32 has no padding and any bit pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), size_of_val(v)) }
}

/// A program of the two shaders' sources, compiled and linked.
///
/// # Safety
///
/// The GL context is current on this thread.
unsafe fn program_of(gl: &glow::Context, vertex: &str, fragment: &str) -> Result<glow::Program> {
    // SAFETY: plain GL object creation on the current context (the caller's promise).
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow!(e))?;
        for (kind, src) in [
            (glow::VERTEX_SHADER, vertex),
            (glow::FRAGMENT_SHADER, fragment),
        ] {
            let shader = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
            gl.shader_source(shader, src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(anyhow!("shader: {}", gl.get_shader_info_log(shader)));
            }
            gl.attach_shader(program, shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(anyhow!("program: {}", gl.get_program_info_log(program)));
        }
        Ok(program)
    }
}

/// The poses of a tracked hand's joints, for its model (`None`: not all of them
/// tracked).
pub fn joint_poses(locations: &xr::HandJointLocations) -> Option<[Pose; JOINTS]> {
    let valid = xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
    if !locations.iter().all(|j| j.location_flags.contains(valid)) {
        return None;
    }
    Some(std::array::from_fn(|i| {
        let p = locations[i].pose;
        (vec3(p.position), quat(p.orientation))
    }))
}

/// Joint positions and radii of a tracked hand, if the runtime has them.
pub fn joints(locations: &xr::HandJointLocations) -> Option<[(Vec3, f32); 26]> {
    let valid = xr::SpaceLocationFlags::POSITION_VALID;
    if !locations.iter().all(|j| j.location_flags.contains(valid)) {
        return None;
    }
    Some(std::array::from_fn(|i| {
        (vec3(locations[i].pose.position), locations[i].radius)
    }))
}
