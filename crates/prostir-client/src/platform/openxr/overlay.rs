//! The only thing this app renders itself: tracked hands, pointer rays and the effects
//! that answer them (the core's `view::fx`), drawn as shaded sphere impostors, soft
//! lights and rings into a transparent projection layer on top of the monitors.
//! Frames with nothing to show render nothing. The instances go into a buffer made
//! once (larger only if a frame ever needs more), so a frame takes no memory.

use std::ptr;

use anyhow::{Context, Result, anyhow};
use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;
use openxr as xr;

use crate::platform::openxr::{Gfx, quat, vec3};
use crate::view::fx::{self, Fx, Kind};

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
            let program = gl.create_program().map_err(|e| anyhow!(e))?;
            for (kind, src) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, FRAGMENT),
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
            })
        }
    }

    /// Draws `list` for both eyes. With nothing to draw, nothing is rendered and
    /// [`Overlay::layer_views`] returns `None`.
    pub fn render(
        &mut self,
        gl: &glow::Context,
        views: &[xr::View],
        list: &mut [Fx],
    ) -> Result<()> {
        self.drawn = false;
        if list.is_empty() || views.len() < 2 {
            return Ok(());
        }
        for (eye, view) in self.eyes.iter_mut().zip(views) {
            let view_matrix = Mat4::from_rotation_translation(
                quat(view.pose.orientation),
                vec3(view.pose.position),
            )
            .inverse();
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
                gl.disable(glow::DEPTH_TEST);
                gl.disable(glow::SCISSOR_TEST);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                gl.use_program(Some(self.program));
                gl.uniform_matrix_4_f32_slice(
                    Some(&self.view_loc),
                    false,
                    &view_matrix.to_cols_array(),
                );
                gl.uniform_matrix_4_f32_slice(
                    Some(&self.proj_loc),
                    false,
                    &projection(view.fov).to_cols_array(),
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
