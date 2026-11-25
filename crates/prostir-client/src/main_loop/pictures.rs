//! The small pictures drawn on the CPU and shown as layers: the menus, the menu's
//! button, the pointer dot, the toast, "Attach", the placeholder of a monitor being
//! added, what is wrong with a monitor's picture, the black layer over a dimmed room
//! and the panorama. Each is drawn and uploaded only when what it shows changed. The
//! looks of the PCs' mouse pointers come as they are, each made into a picture of its
//! own once, kept while its PC's pointer may show it again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Gpu;
use crate::app::Model;
use crate::background;
use crate::ids::{DisplayKey, PcId};
use crate::interaction::{Hands, Menu, Menus};
use crate::lang::Texts;
#[cfg(feature = "preview")]
use crate::layers::METRICS_PX;
use crate::layers::TOAST_PX;
use crate::platform::openxr::ImageLayer;
#[cfg(feature = "preview")]
use crate::platform::video::SurfaceVideo;
use crate::settings::Background;
use crate::ui::{self, Action, Panel};
use crate::view::View;
use crate::world::ATTACH_PX;
use anyhow::Result;
use log::{info, warn};

pub const CURSOR_PX: u32 = 64;
pub const BUTTON_PX: u32 = 128;
/// Width of the placeholder's picture, in pixels (its height follows the monitor's
/// shape).
const PLACEHOLDER_PX: u32 = 1024;

#[expect(
    clippy::disallowed_methods,
    reason = "looked for at the start and when the menu opens, off the frame's work"
)]
pub fn custom_panorama_path(dir: Option<&Path>) -> Option<PathBuf> {
    let d = dir?;
    ["background.jpg", "background.jpeg", "background.png"]
        .iter()
        .map(|n| d.join(n))
        .find(|p| p.exists())
}

/// Width, height and RGBA pixels of a panorama.
type Panorama = (u32, u32, Vec<u8>);

/// Pixels for a panorama background: generated sky or the user's picture.
fn load_panorama(choice: Background, dir: Option<&Path>) -> Panorama {
    if choice == Background::Custom
        && let Some(path) = custom_panorama_path(dir)
    {
        match image::open(&path) {
            Ok(img) => {
                // Too wide or too tall for the compositor's layer: smaller, as it is.
                let img = match background::fit_within(img.width(), img.height()) {
                    Some((w, h)) => img.resize(w, h, image::imageops::FilterType::Triangle),
                    None => img,
                };
                let rgba = img.to_rgba8();
                info!("panorama from {}", path.display());
                return (rgba.width(), rgba.height(), rgba.into_raw());
            }
            Err(e) => warn!("cannot read {}: {e}", path.display()),
        }
    }
    let sky = match choice {
        Background::Sky(sky) => sky,
        _ => background::Sky::Night,
    };
    (
        background::WIDTH,
        background::HEIGHT,
        background::generate(sky),
    )
}

/// The side of the picture a PC's pointer is drawn in: the largest look fits.
const POINTER_PX: u32 = prostir_proto::CURSOR_SIDE_MAX as u32;

/// The picture a PC's mouse pointer is drawn from: one that changes, as large as the
/// largest look, each look drawn at its bottom left when it is shown (the layer shows
/// that part only).
pub struct PcPointer {
    pub pc: PcId,
    /// None where it could not be made (said once; no pointer is drawn for that PC).
    pub layer: Option<ImageLayer>,
    /// The look in it now, and its size (0: none yet).
    pub shape: u32,
    pub size: (u32, u32),
    /// Its pixels, kept to put the next look in without new memory.
    pixels: Vec<u8>,
}

impl PcPointer {
    /// The picture of PC `pc`'s pointer, made for it; none if the runtime refused it.
    fn new(gpu: &Gpu<'_>, pc: PcId) -> Self {
        let layer = ImageLayer::new(gpu.session, gpu.format, POINTER_PX, POINTER_PX, false)
            .inspect_err(|e| warn!("no picture for the PC's pointer: {e:#}"))
            .ok();
        Self {
            pc,
            layer,
            shape: 0,
            size: (0, 0),
            pixels: Vec::new(),
        }
    }

    /// Draws look `shape` in it, unless it is in it already. A look that could not be
    /// uploaded is not shown (and tried again next frame).
    fn show(&mut self, gl: &glow::Context, shape: u32, look: &crate::cursors::Look) {
        let Some(layer) = self.layer.as_mut().filter(|_| self.shape != shape) else {
            return;
        };
        let (w, h) = (u32::from(look.w), u32::from(look.h));
        let side = POINTER_PX as usize;
        self.pixels.resize(side * side * 4, 0);
        // At the bottom left, row by row.
        let row = usize::from(look.w) * 4;
        let top = side - usize::from(look.h);
        for (y, line) in look.rgba.chunks_exact(row).enumerate() {
            let at = (top + y) * side * 4;
            self.pixels[at..at + row].copy_from_slice(line);
        }
        let part = ui::Area {
            x: 0,
            y: top as u32,
            w,
            h,
        };
        match layer.upload_part(gl, &self.pixels, Some(part)) {
            Ok(()) => {
                self.shape = shape;
                self.size = (w, h);
            }
            Err(e) => {
                warn!("the PC's pointer could not be drawn: {e:#}");
                self.shape = 0;
            }
        }
    }

    /// Whether it shows look `shape` of PC `pc`.
    pub fn shows(&self, pc: PcId, shape: u32) -> bool {
        self.pc == pc && self.shape == shape && shape != 0 && self.layer.is_some()
    }
}

/// The two menus' pictures, which know where their buttons are.
pub struct Panels {
    pub main: Panel,
    pub palm: Panel,
    /// The model's change each was last given the state of (see `View::rev`).
    pub main_rev: Option<u64>,
    pub palm_rev: Option<u64>,
}

impl Menus for Panels {
    fn hover(&mut self, menu: Menu, at: Option<(f32, f32)>) {
        match menu {
            Menu::Main => self.main.hover(at),
            Menu::Palm => self.palm.hover(at),
        }
    }

    fn press(&mut self, menu: Menu, at: (f32, f32)) -> Option<Action> {
        match menu {
            Menu::Main => {
                self.main.hover(Some(at));
                self.main.press()
            }
            Menu::Palm => {
                self.palm.hover(Some(at));
                self.palm.hovered_action()
            }
        }
    }
}

/// The pictures and the layers they are shown on.
pub struct Pictures {
    pub panel_layer: ImageLayer,
    pub button_layer: ImageLayer,
    pub cursor_layer: ImageLayer,
    pub toast_layer: ImageLayer,
    pub attach_layer: ImageLayer,
    /// What the "Attach" texture shows now: language and lit (pointed at).
    pub attach_drawn: Option<(Texts, bool)>,
    /// The black layer over a dimmed room blended with the frame, and how dark.
    pub dim_layer: Option<(u8, ImageLayer)>,
    /// The placeholder of a monitor being added: its layer, made for a size and
    /// language.
    pub placeholder: Option<(ImageLayer, (u32, u32), Texts)>,
    /// What is wrong with each monitor's picture, in words, for what it is now and the
    /// language it was drawn in.
    pub trouble_layers: HashMap<DisplayKey, (crate::video::Health, Texts, ImageLayer)>,
    /// Each PC's mouse pointer: a picture of the look it shows now (see `cursors`).
    pub pc_cursors: Vec<PcPointer>,
    /// The toast drawn last (see `Toast::n`).
    toast_drawn: u64,
    button_hovered: bool,
    pub panels: Panels,
    pub wrist_layer: ImageLayer,
    /// The numbers of "For development" next to the monitor looked at (the preview
    /// build only), and what they say (`None`: not drawn yet).
    #[cfg(feature = "preview")]
    pub metrics_layer: ImageLayer,
    #[cfg(feature = "preview")]
    pub metrics: crate::metrics::Label,
    /// The panorama shown and the background it was made for.
    pub pano_layer: Option<(Background, ImageLayer)>,
    pano_job: Option<(Background, JoinHandle<Panorama>)>,
    /// The background whose panorama could not be shown (not tried again while it is
    /// chosen): the person's picture falls back to the generated sky first.
    pano_failed: Option<Background>,
}

impl Pictures {
    /// The layers of the pictures, the pointer dot and the button drawn.
    pub fn new(gpu: Gpu<'_>) -> Result<Self> {
        let Gpu {
            session,
            gl,
            format,
        } = gpu;
        let panel_layer = ImageLayer::new(session, format, ui::WIDTH, ui::HEIGHT, false)?;
        let mut button_layer = ImageLayer::new(session, format, BUTTON_PX, BUTTON_PX, false)?;
        let mut cursor_layer = ImageLayer::new(session, format, CURSOR_PX, CURSOR_PX, true)?;
        let toast_layer = ImageLayer::new(session, format, TOAST_PX.0, TOAST_PX.1, false)?;
        let attach_layer = ImageLayer::new(session, format, ATTACH_PX.0, ATTACH_PX.1, false)?;
        #[cfg(feature = "preview")]
        let metrics_layer = ImageLayer::new(session, format, METRICS_PX.0, METRICS_PX.1, false)?;
        cursor_layer.upload(gl, &ui::cursor_image(CURSOR_PX))?;
        button_layer.upload(gl, &ui::menu_button_image(BUTTON_PX, false))?;
        let panels = Panels {
            main: Panel::new(),
            palm: Panel::wrist(),
            main_rev: None,
            palm_rev: None,
        };
        let wrist_layer =
            ImageLayer::new(session, format, ui::WRIST_WIDTH, ui::WRIST_HEIGHT, false)?;
        Ok(Self {
            panel_layer,
            button_layer,
            cursor_layer,
            toast_layer,
            attach_layer,
            attach_drawn: None,
            dim_layer: None,
            placeholder: None,
            trouble_layers: HashMap::new(),
            pc_cursors: Vec::new(),
            toast_drawn: 0,
            button_hovered: false,
            panels,
            wrist_layer,
            #[cfg(feature = "preview")]
            metrics_layer,
            #[cfg(feature = "preview")]
            metrics: crate::metrics::Label::new(),
            pano_layer: None,
            pano_job: None,
            pano_failed: None,
        })
    }

    /// Panorama: generated or decoded off the main thread, uploaded once per choice
    /// (`equirect`: the runtime shows it).
    pub fn panorama(
        &mut self,
        gpu: Gpu<'_>,
        wanted: Background,
        equirect: bool,
        external_dir: Option<&Path>,
    ) {
        let Gpu {
            session,
            gl,
            format,
        } = gpu;
        let wants_pano = matches!(wanted, Background::Sky(_) | Background::Custom);
        if self.pano_failed.is_some_and(|f| f != wanted) {
            self.pano_failed = None;
        }
        if wants_pano
            && equirect
            && self.pano_layer.as_ref().is_none_or(|(b, _)| *b != wanted)
            && self.pano_job.is_none()
            && self.pano_failed != Some(wanted)
        {
            let dir = external_dir.map(Path::to_path_buf);
            self.pano_job = Some((
                wanted,
                std::thread::spawn(move || load_panorama(wanted, dir.as_deref())),
            ));
        }
        if let Some((made_for, job)) = self.pano_job.take_if(|(_, j)| j.is_finished()) {
            // A panorama that cannot be shown is no reason to stop the app.
            let made = match job.join() {
                Ok((w, h, pixels)) => ImageLayer::new(session, format, w, h, true)
                    .and_then(|mut layer| layer.upload(gl, &pixels).map(|()| layer))
                    .map_err(|e| format!("{e:#}")),
                Err(_) => Err("its making panicked".to_owned()),
            };
            match made {
                Ok(layer) => self.pano_layer = Some((made_for, layer)),
                Err(e) => {
                    warn!("the panorama for {made_for:?} cannot be shown: {e}");
                    let first = self.pano_failed != Some(made_for);
                    self.pano_failed = Some(made_for);
                    if made_for == Background::Custom && first {
                        info!("the generated sky instead of the picture");
                        self.pano_job = Some((
                            made_for,
                            std::thread::spawn(|| {
                                load_panorama(Background::Sky(background::Sky::Night), None)
                            }),
                        ));
                    }
                }
            }
        }
    }

    /// The menu button lit while pointed at.
    pub fn button(&mut self, gl: &glow::Context, over: bool) -> Result<()> {
        if over != self.button_hovered {
            self.button_hovered = over;
            self.button_layer
                .upload(gl, &ui::menu_button_image(BUTTON_PX, over))?;
        }
        Ok(())
    }

    /// What the frame shows of the pictures, drawn where it changed: the toast,
    /// "Attach", the placeholder, what is wrong with a monitor's picture, the palm menu
    /// and the menu. How long the menu took to draw, if it was drawn and `timed`.
    pub fn draw(
        &mut self,
        gpu: Gpu<'_>,
        model: &Model,
        hands: &Hands,
        view_cache: &mut View,
        timed: bool,
    ) -> Result<Option<Duration>> {
        let Gpu {
            session,
            gl,
            format,
        } = gpu;
        let world = model.world();
        let mut menu_drawn = None;
        // A new toast: drawn once.
        if let Some(t) = model.toast()
            && t.n != self.toast_drawn
        {
            self.toast_drawn = t.n;
            self.toast_layer.upload(
                gl,
                &self
                    .panels
                    .main
                    .toast_image(&t.text, TOAST_PX.0, TOAST_PX.1),
            )?;
        }
        if hands.attach_for().is_some() {
            let lit = hands.attach_lit();
            if self
                .attach_drawn
                .as_ref()
                .is_none_or(|(t, l)| t != model.texts() || *l != lit)
            {
                let label = model.texts().t("headset-attach");
                self.attach_layer.upload(
                    gl,
                    &self
                        .panels
                        .main
                        .pill_image(label, ATTACH_PX.0, ATTACH_PX.1, lit),
                )?;
                self.attach_drawn = Some((model.texts().clone(), lit));
            }
        }
        if let Some(a) = world.adding()
            && self
                .placeholder
                .as_ref()
                .is_none_or(|p| p.1 != a.size || p.2 != *model.texts())
        {
            let (w, h) = a.size;
            let px_h = (PLACEHOLDER_PX * h / w.max(1)).clamp(64, 4 * PLACEHOLDER_PX);
            let mut layer = ImageLayer::new(session, format, PLACEHOLDER_PX, px_h, true)?;
            let label = model.texts().t("headset-adding-monitor");
            layer.upload(
                gl,
                &self
                    .panels
                    .main
                    .placeholder_image(label, PLACEHOLDER_PX, px_h),
            )?;
            self.placeholder = Some((layer, a.size, model.texts().clone()));
        }
        // What is wrong with a monitor's picture, in words: over it while it recovers,
        // in its place while it cannot be shown. Made again only when that changes.
        let videos = model.videos();
        self.trouble_layers
            .retain(|id, (h, t, _)| videos.health(*id) == *h && t == model.texts());
        for &(id, w, h) in world.monitors() {
            let health = videos.health(id);
            if health == crate::video::Health::Fine || self.trouble_layers.contains_key(&id) {
                continue;
            }
            let text = match videos.failure(id) {
                Some(f) => crate::video::shown_text(&f, model.texts()),
                None => model.texts().t("headset-video-trouble").to_owned(),
            };
            let px_h = (PLACEHOLDER_PX * h / w.max(1)).clamp(64, 4 * PLACEHOLDER_PX);
            let mut layer = ImageLayer::new(session, format, PLACEHOLDER_PX, px_h, true)?;
            layer.upload(
                gl,
                &self
                    .panels
                    .main
                    .placeholder_image(&text, PLACEHOLDER_PX, px_h),
            )?;
            self.trouble_layers
                .insert(id, (health, model.texts().clone(), layer));
        }
        // The PCs' pointers: a picture for each PC whose pointer shows, made once, the
        // look drawn in it when it changes. Never the end of the frame.
        let cursors = model.cursors();
        self.pc_cursors.retain(|p| cursors.knows(p.pc));
        for &(id, _, _) in world.monitors() {
            let Some(shown) = cursors.on(id, model.now()) else {
                continue;
            };
            let i = match self.pc_cursors.iter().position(|p| p.pc == id.pc) {
                Some(i) => i,
                None => {
                    let gpu = Gpu {
                        session,
                        gl,
                        format,
                    };
                    self.pc_cursors.push(PcPointer::new(&gpu, id.pc));
                    self.pc_cursors.len() - 1
                }
            };
            self.pc_cursors[i].show(gl, shown.shape, shown.look);
        }
        // Only what changed is uploaded: pointing at another button changes its part.
        if world.wrist().is_some() && self.panels.palm.render_if_dirty().is_some() {
            self.wrist_layer.upload_part(
                gl,
                self.panels.palm.pixels(),
                self.panels.palm.damage(),
            )?;
        }
        if model.menu().open {
            let rev = Some(model.rev());
            if rev != self.panels.main_rev {
                self.panels.main.set_state(view_cache.state(model));
                self.panels.main_rev = rev;
            }
            let drawing = timed.then(Instant::now);
            if self.panels.main.render_if_dirty().is_some() {
                menu_drawn = drawing.map(|t| t.elapsed());
                self.panel_layer.upload_part(
                    gl,
                    self.panels.main.pixels(),
                    self.panels.main.damage(),
                )?;
            }
        }
        Ok(menu_drawn)
    }

    /// The numbers of "For development" for the monitor looked at (see
    /// `metrics::Label`): drawn and uploaded again only when what they say changes.
    #[cfg(feature = "preview")]
    pub fn metrics(
        &mut self,
        gl: &glow::Context,
        model: &Model,
        video: &SurfaceVideo,
        now: Instant,
    ) -> Result<()> {
        let received = |id| video.received(id);
        if let Some(picture) = self
            .metrics
            .update(&mut self.panels.main, model, received, now)
        {
            self.metrics_layer.upload(gl, picture)?;
        }
        Ok(())
    }

    /// The black layer over a room blended with the frame and dimmed, letting part of
    /// it through: made once for each darkness.
    pub fn dim(&mut self, gpu: Gpu<'_>, dim: Option<u8>) -> Result<()> {
        let Gpu {
            session,
            gl,
            format,
        } = gpu;
        if let Some(alpha) = dim
            && self.dim_layer.as_ref().is_none_or(|(a, _)| *a != alpha)
        {
            let mut layer = ImageLayer::new(session, format, 4, 4, true)?;
            layer.upload(gl, &[0, 0, 0, alpha].repeat(16))?;
            self.dim_layer = Some((alpha, layer));
        }
        Ok(())
    }
}
