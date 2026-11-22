//! Minimal EGL context. OpenXR needs a graphics binding to create a session,
//! even though all monitor pixels go straight from MediaCodec into
//! compositor-owned Android surfaces and this app never draws anything.

use anyhow::{Context, Result};
use khronos_egl as egl;

#[link(name = "EGL")]
unsafe extern "C" {}

pub struct EglContext {
    pub egl: egl::Instance<egl::Static>,
    pub display: egl::Display,
    pub config: egl::Config,
    pub context: egl::Context,
    surface: egl::Surface,
}

impl EglContext {
    pub fn new() -> Result<Self> {
        let egl = egl::Instance::new(egl::Static);
        // SAFETY: DEFAULT_DISPLAY is always a valid argument.
        let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.context("eglGetDisplay")?;
        egl.initialize(display).context("eglInitialize")?;
        #[rustfmt::skip]
        let attribs = [
            egl::RED_SIZE, 8,
            egl::GREEN_SIZE, 8,
            egl::BLUE_SIZE, 8,
            egl::ALPHA_SIZE, 8,
            egl::RENDERABLE_TYPE, egl::OPENGL_ES3_BIT,
            egl::SURFACE_TYPE, egl::PBUFFER_BIT,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &attribs)?
            .context("no EGL config")?;
        let context = egl.create_context(
            display,
            config,
            None,
            &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
        )?;
        let surface = egl.create_pbuffer_surface(
            display,
            config,
            &[egl::WIDTH, 16, egl::HEIGHT, 16, egl::NONE],
        )?;
        egl.make_current(display, Some(surface), Some(surface), Some(context))?;
        Ok(Self {
            egl,
            display,
            config,
            context,
            surface,
        })
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_surface(self.display, self.surface);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}
