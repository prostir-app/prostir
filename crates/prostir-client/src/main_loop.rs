//! The shell's loop on the headset (step 7, 7): OpenXR, Android and the threads. Each
//! pass takes the time once, hands what came (Android, OpenXR, the network, the system
//! and the files' writer) to the model as events, reads the input, lets the hands act,
//! does the effects the model asked for, and draws: the menu's picture when its state
//! changed, the hands and rays, the layers. The model (`app`) decides; this only does.
//!
//! Each monitor of the PC is decoded by MediaCodec straight into an Android surface
//! owned by the compositor (XR_KHR_android_surface_swapchain) and shown as a quad or
//! cylinder layer. The menu, its button, pointer dots and the panorama are small
//! CPU-drawn images; nothing is rendered per frame.

#![expect(
    clippy::disallowed_methods,
    reason = "the loop is the shell: it takes the time once a pass, starts the threads and looks for the person's files"
)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use android_activity::{AndroidApp, InputStatus, MainEvent, PollEvent};
use anyhow::{Result, anyhow, bail};
use glam::Vec3;
use log::{error, info, warn};
use openxr as xr;
use openxr::sys::Handle as _;

use crate::app::{
    Can, Effect, Event, MicCmd, Model, PlatformEvent, SoundCmd, Start, SystemCmd, TOAST_WIDTH,
    VideoCmd, XrCmd, headset_state,
};
use crate::audio::Gains;
use crate::ids::{DisplayKey, PcId, Target, Thing};
use crate::input::Controls;
use crate::interaction::{FrameInput, Hands, Head, INDEX_TIP, Menu, Menus, POKE_HOVER};
use crate::lang::{Moment, Texts};
use crate::layout::{self, Cylinder};
use crate::mic::Mic;
use crate::net::{Feedback, NetEvent};
use crate::overlay::{self, Overlay, Sphere};
use crate::persist::PersistEvent;
#[cfg(all(feature = "selftest", not(feature = "store")))]
use crate::selftest::device::{self as selftest, Act, Selftest};
use crate::settings::{Background, HandMode};
use crate::store::StoreCheck;
use crate::system::{System, SystemEvent};
use crate::ui::{self, Action, Panel, Tab};
use crate::view::View;
use crate::world::{
    ATTACH_PX, ATTACH_WIDTH, BUTTON_SIZE, KEYBOARD_ASPECT, WRIST_MENU_WIDTH, attach_height,
    panel_height, panel_width, wrist_menu_height,
};
use crate::xrutil::{self, Gfx, ImageLayer, as_layer, pose, posef, vec3};
use crate::{app, background, decoder, egl, net, passthrough, persist};
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
use no_selftest::{self as selftest, Act, Selftest};

const CURSOR_PX: u32 = 64;
const BUTTON_PX: u32 = 128;
/// Toast texture.
const TOAST_PX: (u32, u32) = (1024, 132);
/// While the app is put away, how often the screen is read (see
/// `system::interactive`) until the headset is known to be asleep.
const SCREEN_EVERY: Duration = Duration::from_millis(250);
/// How often the wall clock is read for the menu's clock.
const WALL_EVERY: Duration = Duration::from_secs(1);
/// The layers the compositor takes at most.
const LAYERS_MAX: usize = 16;
/// Width of the placeholder's picture, in pixels (its height follows the monitor's
/// shape).
const PLACEHOLDER_PX: u32 = 1024;

#[unsafe(no_mangle)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "the signature android-activity calls"
)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(if cfg!(debug_assertions) {
                log::LevelFilter::Debug
            } else {
                log::LevelFilter::Info
            })
            .with_tag("prostir"),
    );
    crate::alloc_count::set_main_thread();
    let start = selftest::count_run();
    let mut test = Selftest::requested(&app, start);
    let result = run(&app, test.as_mut());
    if let Err(e) = &result {
        error!("{e:#}");
    }
    if let Some(test) = test {
        let external = app.external_data_path();
        let (stay, code) = test.finish(external.as_deref(), result.as_ref().err());
        // The activity finishes once this returns; without `relaunch=1` the process
        // goes too, so the next start is a new one.
        if !stay {
            let _ = std::thread::Builder::new()
                .name("selftest-exit".into())
                .spawn(move || {
                    std::thread::sleep(Duration::from_millis(500));
                    // SAFETY: ends the process at once, without the exit handlers
                    // and destructors other threads may still be in the middle of.
                    unsafe { libc::_exit(code) };
                });
        }
    }
}

/// 90 Hz (or the nearest below), or 72 Hz to save battery.
fn set_refresh_rate(session: &xr::Session<Gfx>, saver: bool) {
    let limit = if saver { 72.0 } else { 90.0 };
    let rates = session
        .enumerate_display_refresh_rates()
        .unwrap_or_default();
    info!("display rates offered: {rates:?}");
    let best = rates.into_iter().filter(|&r| r <= limit).reduce(f32::max);
    if let Some(rate) = best {
        info!("display at {rate} Hz");
        let _ = session.request_display_refresh_rate(rate);
    }
}

/// The headset's local time, broken down.
fn local_time() -> Option<libc::tm> {
    // SAFETY: plain libc time calls on a zeroed `tm`.
    unsafe {
        let now = libc::time(ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&raw const now, &raw mut tm).is_null()).then_some(tm)
    }
}

/// A moment of the local time, as the menu writes it.
fn moment(tm: &libc::tm) -> Moment {
    let small = |v: libc::c_int, max: u8| v.clamp(0, libc::c_int::from(max)) as u8;
    Moment {
        year: tm.tm_year + 1900,
        month: small(tm.tm_mon, 11),
        day: small(tm.tm_mday, 31),
        weekday: small(tm.tm_wday, 6),
        hour: small(tm.tm_hour, 23),
        minute: small(tm.tm_min, 59),
    }
}

/// One stream: the compositor surface it is decoded into and its decoder thread.
struct Monitor {
    width_px: u32,
    height_px: u32,
    decoder: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    swapchain: xr::Swapchain<Gfx>,
}

impl Monitor {
    fn aspect(&self) -> f32 {
        self.height_px as f32 / self.width_px as f32
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // The decoder must stop writing into the surface before the swapchain goes away.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.decoder.take() {
            // It sleeps until there is work: woken, it sees `stop` at once.
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "looked for at the start and when the menu opens, off the frame's work"
)]
fn custom_panorama_path(dir: Option<&Path>) -> Option<PathBuf> {
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
                let img = if img.width() > 4096 {
                    img.resize(4096, 2048, image::imageops::FilterType::Triangle)
                } else {
                    img
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

/// The two menus' pictures, which know where their buttons are.
struct Panels {
    main: Panel,
    palm: Panel,
    /// The model's change each was last given the state of (see `View::rev`).
    main_rev: Option<u64>,
    palm_rev: Option<u64>,
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

/// The model and what its effects are done with: the shell's handles (see
/// [`Shell::apply`]).
struct Shell<'s> {
    model: Model,
    session: &'s xr::Session<Gfx>,
    /// The runtime lets the app choose the display rate.
    refresh_rates: bool,
    /// The streams shown, each with its surface and decoder.
    monitors: HashMap<DisplayKey, Monitor>,
    /// Messages to each PC in a session.
    outboxes: BTreeMap<PcId, Feedback>,
    /// The files' writer of this start: what is changed last is written as this goes
    /// (see `Drop`), however `run` ends.
    saver: persist::Saver,
    network: net::Net,
    system: System,
    mic: Arc<Mic>,
    /// Where the PC's sound comes from, for the audio thread.
    gains: Arc<Gains>,
    controls: Controls,
    passthrough: Option<passthrough::Passthrough>,
    /// Where the person's own files are (none in a selftest).
    external_dir: Option<PathBuf>,
    /// The effects being done, kept to be filled again.
    fx: Vec<Effect>,
    /// What doing them came to, for the model, after them.
    later: Vec<Event>,
    /// The model asked the app to close.
    exit: bool,
}

impl Drop for Shell<'_> {
    /// What the last pass changed is written, and the files' writer ends, before the
    /// app does, also when `run` ends early with an error or a panic.
    fn drop(&mut self) {
        self.model.end_pass();
        self.apply(Instant::now());
        self.saver.stop(persist::Saver::STOPS_WITHIN);
    }
}

impl Shell<'_> {
    /// `event` to the model, then what it asks done.
    fn handle(&mut self, now: Instant, event: Event) {
        self.model.handle(now, event);
        self.apply(now);
    }

    /// Does what the model asked, in the order it asked, until it asks nothing more
    /// (what doing it came to goes back to it as events).
    fn apply(&mut self, now: Instant) {
        loop {
            self.model.take_effects(&mut self.fx);
            if self.fx.is_empty() {
                return;
            }
            let mut fx = std::mem::take(&mut self.fx);
            for effect in fx.drain(..) {
                self.effect(effect);
            }
            self.fx = fx;
            let mut later = std::mem::take(&mut self.later);
            for event in later.drain(..) {
                self.model.handle(now, event);
            }
            self.later = later;
        }
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::Send(pc, msg) => match self.outboxes.get(&pc).map(|f| f.send(msg)) {
                Some(Ok(())) => {}
                Some(Err(e)) => self.later.push(Event::SendFailed {
                    pc,
                    why: Some(e.to_string()),
                }),
                None => self.later.push(Event::SendFailed { pc, why: None }),
            },
            Effect::Net(cmd) => self.network.send(cmd),
            Effect::Save(file) => {
                let model = &self.model;
                self.saver.save(file, |out| model.write_file(file, out));
            }
            Effect::Haptic(hand) => self.controls.pulse(self.session, hand),
            Effect::Xr(XrCmd::RefreshRate { saver }) => {
                if self.refresh_rates {
                    set_refresh_rate(self.session, saver);
                }
            }
            Effect::Xr(XrCmd::Room { on, opacity }) => {
                if let Some(pt) = &mut self.passthrough
                    && let Err(e) = pt.set_room(on).and_then(|()| pt.set_room_opacity(opacity))
                {
                    self.later
                        .push(Event::Platform(PlatformEvent::CamerasFailed(format!(
                            "{e:#}"
                        ))));
                }
            }
            Effect::Sound(SoundCmd::Place(angle)) => self.gains.place(angle),
            Effect::Mic(MicCmd::Allowed(on)) => self.mic.set_allowed(on),
            Effect::Mic(MicCmd::AskAgain) => self.mic.ask_again(),
            Effect::Mic(MicCmd::RefreshPermission) => self.mic.refresh_permission(),
            Effect::Video(VideoCmd::Drop(id)) => {
                self.monitors.remove(&id);
            }
            Effect::System(SystemCmd::LoadLanguage(lang)) => self.system.load_language(lang),
            Effect::System(SystemCmd::PalmMenu(shown)) => self.system.palm_menu(shown),
            Effect::System(SystemCmd::LookForFiles) => {
                // The person may have copied a picture onto the headset meanwhile, or
                // a language file (listed again on the system thread).
                let custom = custom_panorama_path(self.external_dir.as_deref()).is_some();
                self.later
                    .push(Event::Platform(PlatformEvent::CustomPanorama(custom)));
                self.system.list_languages();
            }
            Effect::Exit => self.exit = true,
        }
    }

    /// Hands over what changed and waits until it is written (for
    /// [`persist::Saver::STOPS_WITHIN`] at most): the app is `why` (paused, or the
    /// headset taken off), and may not come back.
    fn flush(&mut self, now: Instant, why: &str) {
        self.model.end_pass();
        self.apply(now);
        if !self.saver.flush(persist::Saver::STOPS_WITHIN) {
            warn!(
                "{why}: the files were not all written within {} ms",
                persist::Saver::STOPS_WITHIN.as_millis()
            );
        }
    }

    /// What the network thread said: the handles it brings (a session's outbox, a
    /// stream's frames) stay here, the facts go to the model.
    fn net_event(&mut self, now: Instant, event: NetEvent, vm: &jni::JavaVM) {
        match event {
            NetEvent::Connected {
                pc,
                addr,
                feedback,
                language,
                opened_with,
            } => {
                self.outboxes.insert(pc, feedback);
                self.handle(
                    now,
                    Event::Connected {
                        pc,
                        addr,
                        language,
                        opened_with,
                    },
                );
            }
            NetEvent::StreamStart {
                pc,
                id,
                generation,
                width,
                height,
                codec,
                frames,
                feedback,
            } => {
                let stream = decoder::Stream { id, generation };
                let id = DisplayKey::new(pc, id);
                self.monitors.remove(&id);
                match xrutil::create_surface_swapchain(self.session, vm, width, height) {
                    Ok((swapchain, window)) => {
                        let stop = Arc::new(AtomicBool::new(false));
                        let decoder = decoder::spawn(decoder::DecoderJob {
                            stream,
                            codec,
                            width,
                            height,
                            window,
                            frames,
                            feedback,
                            stop: stop.clone(),
                        });
                        self.monitors.insert(
                            id,
                            Monitor {
                                width_px: width,
                                height_px: height,
                                decoder: Some(decoder),
                                stop,
                                swapchain,
                            },
                        );
                        let size = (width, height);
                        self.handle(now, Event::StreamStarted { id, size });
                    }
                    Err(e) => {
                        error!("monitor {id}: {e:#}");
                        self.handle(now, Event::StreamFailed(id));
                    }
                }
            }
            event => {
                // A session ended: its outbox goes with it.
                if let NetEvent::Ended { pc: Some(pc), .. } = &event {
                    self.outboxes.remove(pc);
                }
                self.handle(now, Event::Net(event));
            }
        }
    }

    /// Reads the screen where presence wants it (only while the app is put away, at
    /// once and then at most every `SCREEN_EVERY`; see `Presence::wants_screen`), then
    /// the PC is told what changed.
    fn presence_now(&mut self, now: Instant, screen_read: &mut Option<Instant>) {
        if !self.model.wants_screen() {
            *screen_read = None;
        } else if screen_read.is_none_or(|at| now.saturating_duration_since(at) >= SCREEN_EVERY) {
            *screen_read = Some(now);
            if let Some(on) = crate::system::interactive() {
                self.handle(now, Event::Platform(PlatformEvent::Interactive(on)));
                return;
            }
        }
        self.model.presence_due(now);
        self.apply(now);
    }
}

/// Buffers of a frame's layers, kept from frame to frame and filled again.
#[derive(Default)]
struct LayerBuffers {
    /// Where the eyes are, for the hands and rays.
    views: [xr::View; 2],
    spheres: Vec<Sphere>,
    dims: Vec<xr::sys::CompositionLayerColorScaleBiasKHR>,
    quads: Vec<xr::sys::CompositionLayerQuad>,
    cylinders: Vec<xr::sys::CompositionLayerCylinderKHR>,
    cursors: Vec<xr::sys::CompositionLayerQuad>,
    /// The layers' list for `end`: its memory only, empty between frames (see
    /// [`reuse`]).
    layers: Vec<&'static xr::CompositionLayerBase<'static, Gfx>>,
}

/// An empty list of references in the memory of `v` (emptied): a list kept from frame
/// to frame whose items borrow from one frame only. Collected into the same layout,
/// nothing is allocated.
fn reuse<'a, 'b, T, U: 'b>(mut v: Vec<&'a T>) -> Vec<&'b U> {
    v.clear();
    v.into_iter().filter_map(|_| None).collect()
}

/// The app; with `test`, the selftest's run of it (see `selftest::device`).
fn run(app: &AndroidApp, mut test: Option<&mut Selftest>) -> Result<()> {
    // SAFETY: pointers come straight from the running NativeActivity.
    let platform = unsafe { xr::AndroidPlatformInfo::new(app.vm_as_ptr(), app.activity_as_ptr()) };
    // SAFETY: libopenxr_loader.so is the Khronos loader bundled in the APK.
    let entry = unsafe { xr::Entry::load(&platform) }
        .map_err(|e| anyhow!("cannot load OpenXR loader: {e}"))?;
    let available = entry.enumerate_extensions()?;
    if !available.khr_android_surface_swapchain {
        bail!("the runtime lacks XR_KHR_android_surface_swapchain");
    }
    let mut exts = xr::ExtensionSet::default();
    exts.khr_android_create_instance = true;
    exts.khr_opengl_es_enable = true;
    exts.khr_android_surface_swapchain = true;
    exts.fb_passthrough = available.fb_passthrough;
    exts.fb_display_refresh_rate = available.fb_display_refresh_rate;
    exts.fb_composition_layer_settings = available.fb_composition_layer_settings;
    exts.fb_composition_layer_image_layout = available.fb_composition_layer_image_layout;
    exts.khr_composition_layer_cylinder = available.khr_composition_layer_cylinder;
    exts.khr_composition_layer_equirect2 = available.khr_composition_layer_equirect2;
    exts.ext_hand_interaction = available.ext_hand_interaction;
    exts.ext_hand_tracking = available.ext_hand_tracking;
    exts.fb_hand_tracking_aim = available.fb_hand_tracking_aim;
    exts.fb_triangle_mesh = available.fb_triangle_mesh;
    exts.khr_composition_layer_color_scale_bias = available.khr_composition_layer_color_scale_bias;
    exts.khr_android_thread_settings = available.khr_android_thread_settings;
    // Taken off, said at once by the headset's sensor (the session may stay shown
    // until the system sleeps).
    exts.ext_user_presence = available.ext_user_presence;
    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "Prostir",
            application_version: 1,
            engine_name: "prostir",
            engine_version: 1,
            api_version: xr::Version::new(1, 0, 0),
        },
        &exts,
        &[],
        &platform,
    )?;
    info!(
        "Prostir {}: cylinder layers: {}, equirect: {}, hand interaction: {}, user presence: {}",
        prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
        exts.khr_composition_layer_cylinder,
        exts.khr_composition_layer_equirect2,
        exts.ext_hand_interaction,
        exts.ext_user_presence
    );
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let _ = instance.graphics_requirements::<Gfx>(system)?;
    // As the system names the headset: only shown (the PC's list of headsets,
    // pairing) and logged.
    let model = crate::system::model();
    info!("headset: {model:?}");
    if let Some(t) = test.as_deref_mut() {
        t.runtime(app, &instance, system, &available);
        t.headset_caps(&crate::device::headset_caps(&model));
    }

    let egl = egl::EglContext::new()?;
    // SAFETY: the EGL handles stay alive (owned by `egl`) for the whole session.
    let (session, mut frame_waiter, mut frame_stream) = unsafe {
        instance.create_session::<Gfx>(
            system,
            &xr::opengles::SessionCreateInfo::Android {
                display: egl.display.as_ptr().cast(),
                config: egl.config.as_ptr().cast(),
                context: egl.context.as_ptr().cast(),
            },
        )
    }?;
    xrutil::mark_main_thread(&instance, &session);
    let gl = xrutil::load_gl(&egl.egl);

    let data_dir: PathBuf = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    // The selftest keeps its own data, and its fake PC is the only PC it connects to.
    let data_dir = match test.as_deref_mut() {
        Some(t) => t.data_dir(&data_dir),
        None => data_dir,
    };
    let fake_pc = test.as_deref_mut().and_then(|t| t.start_pc(app, &data_dir));
    if test.as_deref().is_some_and(Selftest::over) {
        return Ok(());
    }
    // A write cut short leaves its temporary file; the real one is whole.
    let persisted = persist::File::ALL.map(persist::File::name);
    for name in persisted.into_iter().chain(["identity", "paired_hosts"]) {
        prostir_proto::secure::remove_leftovers(&data_dir.join(name));
    }
    // The files are read here once; from now on the model in memory is what counts,
    // and the files' writer only writes them, off the frame. However this function
    // ends, what waits is written and the thread waited for (see `persist::Saver`,
    // and `Shell`'s `Drop` once the shell has it).
    let (loaded, saver) = persist::start(&data_dir);
    let persist::Loaded {
        mut settings,
        desk,
        profiles: saved_profiles,
        keyboard: keyboard_place,
        untouched: _,
    } = loaded;
    // This start's own: a new start in the same process begins afresh.
    let mic = Arc::new(Mic::new(settings.mic, app.clone()));
    let gains = Arc::new(Gains::default());

    // Passthrough keeps the cameras busy and costs battery: only when "Room" is chosen.
    let passthrough = if exts.fb_passthrough {
        passthrough::Passthrough::new(&session, settings.background.is_room())
            .inspect_err(|e| warn!("passthrough unavailable: {e:#}"))
            .ok()
    } else {
        None
    };

    let local =
        session.create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::Posef::IDENTITY)?;
    let view = session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?;
    let mut controls = Controls::new(&instance, &session, exts.ext_hand_interaction)?;
    controls.hands_enabled = settings.hands == HandMode::Control;

    let format = xrutil::color_format(&session);
    let mut panel_layer = ImageLayer::new(&session, format, ui::WIDTH, ui::HEIGHT, false)?;
    let mut button_layer = ImageLayer::new(&session, format, BUTTON_PX, BUTTON_PX, false)?;
    let mut cursor_layer = ImageLayer::new(&session, format, CURSOR_PX, CURSOR_PX, true)?;
    let mut toast_layer = ImageLayer::new(&session, format, TOAST_PX.0, TOAST_PX.1, false)?;
    let mut attach_layer = ImageLayer::new(&session, format, ATTACH_PX.0, ATTACH_PX.1, false)?;
    // What the "Attach" texture shows now: language and lit (pointed at).
    let mut attach_drawn: Option<(Texts, bool)> = None;
    // The placeholder of a monitor being added: its layer, made for a size and
    // language.
    let mut placeholder: Option<(ImageLayer, (u32, u32), Texts)> = None;
    // The toast drawn last (see `Toast::n`).
    let mut toast_drawn = 0;
    cursor_layer.upload(&gl, &ui::cursor_image(CURSOR_PX))?;
    let mut button_hovered = false;
    button_layer.upload(&gl, &ui::menu_button_image(BUTTON_PX, false))?;
    let mut panels = Panels {
        main: Panel::new(),
        palm: Panel::wrist(),
        main_rev: None,
        palm_rev: None,
    };
    let mut wrist_layer =
        ImageLayer::new(&session, format, ui::WRIST_WIDTH, ui::WRIST_HEIGHT, false)?;
    // Tracked hands and pointer rays, drawn on top of everything.
    let hand_trackers = if exts.ext_hand_tracking {
        let make = |h| session.create_hand_tracker(h);
        make(xr::Hand::LEFT)
            .and_then(|l| Ok([l, make(xr::Hand::RIGHT)?]))
            .inspect_err(|e| warn!("hand tracking unavailable: {e}"))
            .ok()
    } else {
        None
    };
    let mut overlay = Overlay::new(&gl, &instance, system, &session, format)
        .inspect_err(|e| warn!("hand/ray overlay unavailable: {e:#}"))
        .ok();
    // The panorama shown and the background it was made for.
    let mut pano_layer: Option<(Background, ImageLayer)> = None;
    let mut pano_job: Option<(Background, JoinHandle<Panorama>)> = None;
    // The selftest reads nothing of the person's there (`host.txt`, a panorama).
    let external_dir = app.external_data_path().filter(|_| test.is_none());
    let custom_pano = custom_panorama_path(external_dir.as_deref()).is_some();

    // The menu's language, before the first frame: the headset's own language once,
    // the files there are, and the language to show. From here on the system thread
    // reads the files.
    let system_lang = crate::system::language();
    #[expect(
        clippy::disallowed_methods,
        reason = "the start, before the first frame, reads the language files once"
    )]
    let (lang, lang_loader) = crate::lang::start(
        external_dir.as_ref().map(|d| d.join("lang")),
        settings.language,
        settings.pc_language,
        system_lang,
    );
    // A language chosen once whose file is gone is "like the PC" again.
    settings.language = crate::lang::kept(settings.language, lang.known());

    // The battery once now, for the first `Open`; from here on the system thread reads
    // it, away from the frame.
    let battery = crate::system::battery();
    let system_thread = System::start(lang_loader);

    let (net_tx, net_rx) = mpsc::channel();
    // A selftest connects to its fake PC and nothing else, never to a real PC.
    debug_assert!(test.is_none() || fake_pc.is_some());
    // However this function ends, the network thread and its sessions end with it, and
    // the thread is waited for (up to a second, see `Net`'s `Drop`): a new start in the
    // same process must not find this one's still connected to the PC.
    let network = net::Net::start(
        net::Config::in_dir(&data_dir, external_dir.as_deref(), &model),
        net_tx,
        Arc::new(crate::media::Sound {
            gains: gains.clone(),
            mic: mic.clone(),
        }),
        // What the first `Open` says, before the core says more.
        headset_state(&settings, true, battery, mic.permission(), &model),
    )?;

    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let _wifi_lock = xrutil::acquire_low_latency_wifi(app, &vm)
        .inspect(|_| info!("Wi-Fi low-latency mode on"))
        .inspect_err(|e| warn!("Wi-Fi low-latency lock unavailable: {e:#}"))
        .ok();

    // Where this build comes from. A store build checks the user owns the app, as its
    // store requires at launch; a build from GitHub has nothing to check.
    let mut store = crate::store::of_this_build(app);
    let can = Can {
        room: passthrough.is_some(),
        keyboard_window: passthrough
            .as_ref()
            .is_some_and(passthrough::Passthrough::window_supported),
        panorama: exts.khr_composition_layer_equirect2,
        curve: exts.khr_composition_layer_cylinder,
        custom_panorama: custom_pano,
        update_from: store.update_hint(),
    };
    let mic_permission = mic.permission();
    let mut shell = Shell {
        model: Model::new(Start {
            now: Instant::now(),
            settings,
            desk,
            keyboard: keyboard_place,
            profiles: saved_profiles,
            lang,
            system_lang,
            battery,
            model,
            can,
            mic_permission,
            only: fake_pc,
        }),
        session: &session,
        refresh_rates: exts.fb_display_refresh_rate,
        monitors: HashMap::new(),
        outboxes: BTreeMap::new(),
        saver,
        network,
        system: system_thread,
        mic,
        gains,
        controls,
        passthrough,
        external_dir,
        fx: Vec::new(),
        later: Vec::new(),
        exit: false,
    };
    let mut hands = Hands::new();
    let mut view_cache = View::new();
    let mut buffers = LayerBuffers::default();
    let mut session_running = false;
    // The microphone's asks for access seen so far (see `Mic::asks`), and what was last
    // said of its permission.
    let mut mic_asks = 0;
    let mut mic_seen = (mic_permission, false);
    // When the screen was last read, while the app is put away.
    let mut screen_read: Option<Instant> = None;
    // The input is the app's: false while the system menu is open over it (the app
    // keeps drawing, but shows no hands or rays of its own and takes no input).
    let mut focused = false;
    let mut last_time: Option<xr::Time> = None;
    let mut event_storage = xr::EventDataBuffer::new();
    // The wall clock for the menu, and when it was read.
    let mut wall: Option<Moment> = None;
    let mut wall_read: Option<Instant> = None;
    // What the selftest asked for, done in the next frame drawn.
    let mut selftest_act = Act::Nothing;
    let mut allocations = crate::alloc_count::Tally::default();

    'main: loop {
        if let Some(t) = test.as_deref_mut() {
            t.loop_top();
        }
        let mut destroy = false;
        let mut paused = false;
        // Paused (true) or resumed (false), the last one of this pass.
        let mut activity = None;
        let timeout = if session_running {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };
        let mut input_waiting = false;
        app.poll_events(Some(timeout), |event| match event {
            PollEvent::Main(MainEvent::Destroy) => destroy = true,
            PollEvent::Main(MainEvent::Pause) => {
                paused = true;
                activity = Some(true);
            }
            PollEvent::Main(MainEvent::Resume { .. }) => activity = Some(false),
            PollEvent::Main(MainEvent::InputAvailable) => input_waiting = true,
            _ => {}
        });
        // The pass's time: the model's timers all count from it.
        let now = Instant::now();
        // What the last pass changed goes to the files' writer.
        shell.model.end_pass();
        shell.apply(now);
        if destroy {
            break;
        }
        // The microphone asked the system for access: the prompt's pause is the app's
        // own, not the user leaving.
        let asks = shell.mic.asks();
        if asks != mic_asks {
            mic_asks = asks;
            shell.handle(now, Event::Platform(PlatformEvent::MicAsked));
        }
        if let Some(paused) = activity {
            // The system may freeze a paused app soon: the PC hears it first.
            shell.handle(now, Event::Platform(PlatformEvent::Paused(paused)));
            shell.presence_now(now, &mut screen_read);
        }
        if paused {
            // The system may end a paused app without a word: what waits is written now.
            shell.flush(now, "paused");
        }
        if let StoreCheck::NotOwned { store: name } = store.poll() {
            shell.handle(
                now,
                Event::Platform(PlatformEvent::NotOwned { store: name }),
            );
        }
        if shell.exit {
            break;
        }
        if input_waiting {
            // Android waits for every key event (e.g. the volume buttons) to be taken
            // and answers "not responding" after 5 s otherwise. Hand them all back to
            // the system so the volume buttons keep working.
            if let Ok(mut events) = app.input_events_iter() {
                while events.next(|_| InputStatus::Unhandled) {}
            }
        }

        while let Some(event) = instance.poll_event(&mut event_storage)? {
            match event {
                xr::Event::SessionStateChanged(e) => {
                    info!("session state: {:?}", e.state());
                    if let Some(t) = test.as_deref_mut() {
                        t.session_state(e.state());
                    }
                    let now_focused = e.state() == xr::SessionState::FOCUSED;
                    if focused && !now_focused {
                        // A drag cannot go on under the system menu.
                        hands.cancel_all(&mut shell.model);
                        shell.apply(now);
                    }
                    focused = now_focused;
                    // Hidden or stopped, the PC stops encoding until the app is shown
                    // again; told before the session ends and the files are written.
                    let shown = match e.state() {
                        xr::SessionState::VISIBLE | xr::SessionState::FOCUSED => {
                            Some(crate::presence::Session::Shown)
                        }
                        xr::SessionState::READY | xr::SessionState::SYNCHRONIZED => {
                            Some(crate::presence::Session::Hidden)
                        }
                        xr::SessionState::STOPPING | xr::SessionState::IDLE => {
                            Some(crate::presence::Session::Stopped)
                        }
                        _ => None,
                    };
                    if let Some(shown) = shown {
                        shell.handle(now, Event::Platform(PlatformEvent::Session(shown)));
                        // Told before the session ends and the files are written.
                        shell.presence_now(now, &mut screen_read);
                    }
                    match e.state() {
                        xr::SessionState::READY => {
                            session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                            session_running = true;
                            if exts.fb_display_refresh_rate {
                                set_refresh_rate(&session, shell.model.settings().battery_saver);
                            }
                            if let Some(t) = test.as_deref_mut()
                                && exts.fb_display_refresh_rate
                            {
                                t.refresh(
                                    session.enumerate_display_refresh_rates().ok(),
                                    session.get_display_refresh_rate().ok(),
                                );
                            }
                        }
                        xr::SessionState::STOPPING => {
                            session.end()?;
                            session_running = false;
                            // Taken off (the headset may sleep for long): saved now.
                            shell.flush(now, "stopping");
                        }
                        xr::SessionState::IDLE => {
                            shell.flush(now, "idle");
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => break 'main,
                        _ => {}
                    }
                }
                xr::Event::UserPresenceChangedEXT(e) => {
                    let present = e.is_user_present();
                    shell.handle(now, Event::Platform(PlatformEvent::UserPresent(present)));
                }
                xr::Event::InstanceLossPending(_) => break 'main,
                _ => {}
            }
        }
        // The sensor's "away" counts once it lasted (see `presence::AWAY_GRACE`), and
        // the screen is read while the app is put away: looked at every pass, also
        // while nothing is drawn.
        shell.presence_now(now, &mut screen_read);
        // The system may let the app record now, or not any more.
        let mic_now = (shell.mic.permission(), shell.mic.denied());
        if mic_now != mic_seen {
            mic_seen = mic_now;
            let (permission, denied) = mic_now;
            shell.handle(
                now,
                Event::Platform(PlatformEvent::Mic { permission, denied }),
            );
        }

        while let Ok(event) = net_rx.try_recv() {
            if let Some(t) = test.as_deref_mut() {
                t.net_event(&event);
            }
            shell.net_event(now, event, &vm);
        }
        // Only the newest battery reading matters (several wait after a pause).
        let mut newest = None;
        while let Some(event) = shell.system.try_recv() {
            match event {
                SystemEvent::Battery(reading) => newest = Some(reading),
                SystemEvent::Texts(texts) => {
                    shell.handle(now, Event::Platform(PlatformEvent::Texts(texts)));
                }
                SystemEvent::Languages(known) => {
                    shell.handle(now, Event::Platform(PlatformEvent::Languages(known)));
                }
            }
        }
        if let Some(reading) = newest {
            shell.handle(now, Event::Platform(PlatformEvent::Battery(reading)));
        }
        while let Some(event) = shell.saver.try_recv() {
            match event {
                PersistEvent::Failed { .. } => shell.handle(now, Event::Persist(event)),
            }
        }

        // Panorama: generated or decoded off the main thread, uploaded once per choice.
        let wanted = shell.model.settings().background;
        let wants_pano = matches!(wanted, Background::Sky(_) | Background::Custom);
        if wants_pano
            && exts.khr_composition_layer_equirect2
            && pano_layer.as_ref().is_none_or(|(b, _)| *b != wanted)
            && pano_job.is_none()
        {
            let dir = shell.external_dir.clone();
            pano_job = Some((
                wanted,
                std::thread::spawn(move || load_panorama(wanted, dir.as_deref())),
            ));
        }
        if let Some((made_for, job)) = pano_job.take_if(|(_, j)| j.is_finished()) {
            match job.join() {
                Ok((w, h, pixels)) => {
                    let mut layer = ImageLayer::new(&session, format, w, h, true)?;
                    layer.upload(&gl, &pixels)?;
                    pano_layer = Some((made_for, layer));
                }
                Err(_) => warn!("panorama generation panicked"),
            }
        }

        if let Some(t) = test.as_deref_mut() {
            match t.step() {
                Act::Finish => break 'main,
                Act::Nothing => {}
                act => selftest_act = act,
            }
        }
        // The menu's clock, about once a second.
        if wall_read.is_none_or(|at| now.saturating_duration_since(at) >= WALL_EVERY) {
            wall_read = Some(now);
            wall = local_time().map(|t| moment(&t));
        }

        // The model's time goes on with or without frames (the session not running,
        // the headset off the head, nothing to render): requests time out, a
        // placeholder goes, and the desktop's arrangement is told once it stood.
        if !session_running {
            shell.model.tick(now, &FrameInput::hidden(wall));
            shell.apply(now);
            continue;
        }

        let state = frame_waiter.wait()?;
        let frame_start = test.as_deref().map(Selftest::frame_start);
        frame_stream.begin()?;
        let time = state.predicted_display_time;
        if !state.should_render {
            shell.model.tick(now, &FrameInput::hidden(wall));
            shell.apply(now);
            frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &[])?;
            continue;
        }
        // How long the menu took to draw in this frame, for the selftest.
        let mut menu_drawn: Option<Duration> = None;
        let dt = last_time
            .map_or(0.0f32, |t| (time.as_nanos() - t.as_nanos()) as f32 * 1e-9)
            .clamp(0.0, 0.1);
        last_time = Some(time);

        // This frame's input.
        let hand_mode = shell.model.settings().hands;
        shell.controls.hands_enabled = hand_mode == HandMode::Control;
        shell.controls.sync(&session, &local, time)?;
        if !focused {
            shell.controls.state = Default::default();
        }
        let (head, forward, right) =
            view.locate(&local, time)
                .map_or((Vec3::ZERO, -Vec3::Z, Vec3::X), |l| {
                    let turn = xrutil::quat(l.pose.orientation);
                    (vec3(l.pose.position), turn * -Vec3::Z, turn * Vec3::X)
                });
        // Tracked hands: joints for drawing, fingertips for touching the menu.
        let joints: [Option<[(Vec3, f32); 26]>; 2] = match &hand_trackers {
            Some(trackers) if focused && hand_mode != HandMode::Hidden => {
                std::array::from_fn(|h| {
                    local
                        .locate_hand_joints(&trackers[h], time)
                        .ok()
                        .flatten()
                        .and_then(|l| overlay::joints(&l))
                })
            }
            _ => [None, None],
        };
        shell.controls.hands_from_joints(&joints, head, forward);
        // The left hand's menu gesture counts as the menu button.
        let menu_gesture = match &hand_trackers {
            Some(t) if exts.fb_hand_tracking_aim => {
                xrutil::hand_menu_gesture(&instance, &t[0], &local, time)
            }
            _ => false,
        };
        let input = FrameInput {
            head: Some(Head {
                position: head,
                forward,
                right,
            }),
            dt,
            wall,
            focused,
            hands: shell.controls.state,
            joints,
            menu_button: shell.controls.menu_pressed(&session),
            menu_gesture,
            align: shell.controls.align_pressed(&session),
        };
        shell.model.tick(now, &input);
        shell.apply(now);

        // What the selftest asks for, as the menu would do it.
        match std::mem::replace(&mut selftest_act, Act::Nothing) {
            Act::MoveMonitor => {
                if let Some(&(id, _, _)) = shell.model.world().monitors().first() {
                    let took = Instant::now();
                    let action = Action::Move { id, delta: 1 };
                    shell.handle(now, Event::Menu(action, app::By::Elsewhere));
                    // Handed to the files' writer now, so the time is the whole of what
                    // saving costs the frame.
                    shell.model.end_pass();
                    shell.apply(now);
                    if let Some(t) = test.as_deref_mut() {
                        t.saved(took.elapsed());
                    }
                }
            }
            Act::OpenMenu => {
                shell.handle(now, Event::Menu(Action::OpenMenu, app::By::Elsewhere));
            }
            Act::SwitchTab => {
                let tab = match shell.model.menu().tab {
                    Tab::Monitors => Tab::Quick,
                    _ => Tab::Monitors,
                };
                let took = Instant::now();
                shell.handle(now, Event::Menu(Action::Tab(tab), app::By::Elsewhere));
                shell.model.end_pass();
                shell.apply(now);
                if let Some(t) = test.as_deref_mut() {
                    t.saved(took.elapsed());
                }
            }
            Act::CloseMenu => shell.handle(now, Event::Menu(Action::Close, app::By::Elsewhere)),
            Act::AddMonitor(width, height) => {
                let add = Action::Add { width, height };
                shell.handle(now, Event::Menu(add, app::By::Elsewhere));
            }
            Act::RemoveMonitor(id) => {
                // Pressed twice, as the menu asks to confirm.
                if let Some(k) = shell.model.key(id) {
                    for _ in 0..2 {
                        shell.handle(now, Event::Menu(Action::Remove(k), app::By::Elsewhere));
                    }
                }
            }
            Act::Nothing | Act::Finish => {}
        }

        // The palm menu, the menu button and "Line up"; then the palm menu's picture is
        // pointed at with the model as it is now.
        hands.buttons(now, &input, &mut shell.model);
        shell.apply(now);
        let rev = Some(shell.model.rev());
        if shell.model.world().wrist().is_some() && rev != panels.palm_rev {
            panels.palm.set_state(view_cache.state(&shell.model));
            panels.palm_rev = rev;
        }
        // Pointing, touching, clicking and dragging.
        hands.step(now, &input, &mut shell.model, &mut panels);
        shell.apply(now);
        let over_button = hands.over_button();
        if over_button != button_hovered {
            button_hovered = over_button;
            button_layer.upload(&gl, &ui::menu_button_image(BUTTON_PX, over_button))?;
        }
        shell.model.animate(now, &input);
        shell.apply(now);
        let model = &shell.model;
        let world = model.world();
        let settings = *model.settings();

        // Menu contents.
        // A new toast: drawn once.
        if let Some(t) = model.toast()
            && t.n != toast_drawn
        {
            toast_drawn = t.n;
            toast_layer.upload(
                &gl,
                &panels.main.toast_image(&t.text, TOAST_PX.0, TOAST_PX.1),
            )?;
        }
        if hands.attach_for().is_some() {
            let lit = hands.attach_lit();
            if attach_drawn
                .as_ref()
                .is_none_or(|(t, l)| t != model.texts() || *l != lit)
            {
                let label = model.texts().t("attach");
                attach_layer.upload(
                    &gl,
                    &panels.main.pill_image(label, ATTACH_PX.0, ATTACH_PX.1, lit),
                )?;
                attach_drawn = Some((model.texts().clone(), lit));
            }
        }
        if let Some(a) = world.adding()
            && placeholder
                .as_ref()
                .is_none_or(|p| p.1 != a.size || p.2 != *model.texts())
        {
            let (w, h) = a.size;
            let px_h = (PLACEHOLDER_PX * h / w.max(1)).clamp(64, 4 * PLACEHOLDER_PX);
            let mut layer = ImageLayer::new(&session, format, PLACEHOLDER_PX, px_h, true)?;
            let label = model.texts().t("adding-monitor");
            layer.upload(
                &gl,
                &panels.main.placeholder_image(label, PLACEHOLDER_PX, px_h),
            )?;
            placeholder = Some((layer, a.size, model.texts().clone()));
        }
        if world.wrist().is_some()
            && let Some(pixels) = panels.palm.render_if_dirty()
        {
            wrist_layer.upload(&gl, pixels)?;
        }
        if model.menu().open {
            let rev = Some(model.rev());
            if rev != panels.main_rev {
                panels.main.set_state(view_cache.state(model));
                panels.main_rev = rev;
            }
            let drawing = test.is_some().then(Instant::now);
            if let Some(pixels) = panels.main.render_if_dirty() {
                menu_drawn = drawing.map(|t| t.elapsed());
                panel_layer.upload(&gl, pixels)?;
            }
        }

        // Hands and rays: only rendered when there is something to show.
        let spheres = &mut buffers.spheres;
        spheres.clear();
        // Light on the menu where a fingertip is about to touch: wide and faint from a
        // few centimetres away, small and bright right before contact.
        let place = world.menu();
        for (_, at, z, pressed) in hands.pokes() {
            let near = (1.0 - z.max(0.0) / POKE_HOVER).clamp(0.0, 1.0);
            let pressed = if pressed { 0.25 } else { 0.0 };
            spheres.push(Sphere::glow(
                place.position + place.rotation * Vec3::new(at.x, at.y, 0.003),
                0.034 - 0.020 * near,
                [
                    0.72,
                    0.84,
                    1.0,
                    (0.12 + 0.6 * near * near + pressed).min(1.0),
                ],
            ));
        }
        // A touch spreads out as a ring for a moment.
        for (center, k) in hands.rings(now) {
            spheres.push(Sphere::ring(
                center,
                0.012 + 0.03 * k,
                [0.8, 0.9, 1.0, 0.9 * (1.0 - k)],
            ));
        }
        // Where a ray points: a small light on the menu, a wider faint one on monitors.
        for pick in hands.picks().iter().flatten() {
            let (radius, alpha) = match pick.thing {
                Thing::Menu | Thing::MenuButton | Thing::Keyboard => (0.012, 0.55),
                Thing::Display(_) | Thing::Adding { .. } => (0.03, 0.3),
                Thing::Attach(_) => (0.01, 0.5),
                Thing::Wrist => (0.008, 0.55),
            };
            spheres.push(Sphere::glow(
                pick.hit.point,
                radius,
                [0.72, 0.84, 1.0, alpha],
            ));
        }
        // Where the mouse is: rings closing in on it, three times.
        if let Some((id, uv, t)) = model.finder()
            && let Some(p) = world.shown(id)
        {
            let c = world
                .curve_radius(&settings)
                .filter(|_| !world.is_window(id));
            let (point, _) = layout::image_point(p, world.height_of(id), c, uv);
            for k in 0..3 {
                let phase = (t * 3.0 - k as f32 * 0.33).rem_euclid(1.0);
                spheres.push(Sphere::ring(
                    point,
                    0.09 * (1.0 - phase) + 0.01,
                    [1.0, 0.85, 0.35, 0.9 * phase * (1.0 - t * 0.5)],
                ));
            }
            spheres.push(Sphere::glow(point, 0.02, [1.0, 0.85, 0.35, 0.8]));
        }
        // The magnet while dragging: the edge the monitor would stick to lights up.
        for thing in hands.held() {
            let Some(s) = thing.display().and_then(|id| shell.model.magnet(id)) else {
                continue;
            };
            let world = shell.model.world();
            let Some(n) = world.shown(s.neighbour).copied() else {
                continue;
            };
            let (half_w, half_h) = (n.width / 2.0, world.height_of(s.neighbour) / 2.0);
            let steps = 28;
            for i in 0..=steps {
                let k = i as f32 / steps as f32 * 2.0 - 1.0;
                let local = if s.across {
                    Vec3::new(s.side * half_w, k * half_h, 0.01)
                } else {
                    Vec3::new(k * half_w, s.side * half_h, 0.01)
                };
                spheres.push(Sphere::glow(
                    n.position + n.rotation * local,
                    0.03,
                    [0.55, 0.75, 1.0, 0.55],
                ));
            }
        }
        let model = &shell.model;
        let world = model.world();
        // Hands are drawn over everything (the compositor has no depth for them), so
        // near a menu they would cover its text: the hand holding the palm menu is not
        // drawn, and a hand about to touch a menu is see-through.
        let menu_hand = usize::from(settings.left_handed);
        for (h, j) in joints.iter().enumerate() {
            let Some(j) = j else { continue };
            if h == menu_hand && world.wrist().is_some() {
                continue;
            }
            let tip = j[INDEX_TIP].0;
            let near_palm = world
                .wrist()
                .is_some_and(|w| w.position.distance(tip) < 0.15);
            let near_panel = hands.poking(h);
            let alpha = if near_palm || near_panel { 0.3 } else { 1.0 };
            overlay::hand_spheres(j, alpha, spheres);
        }
        for (input, pick) in input.hands.iter().zip(hands.picks()) {
            if let Some(ray) = input.ray {
                let length = pick.map_or(1.2, |p| p.hit.t).min(4.0);
                overlay::ray_spheres(ray.origin, ray.dir, length, spheres);
                if !input.is_hand {
                    // Where the controller is, since it is not visible otherwise.
                    spheres.push(Sphere {
                        center: ray.origin,
                        radius: 0.012,
                        color: [0.30, 0.32, 0.38, 1.0],
                        kind: overlay::Kind::Ball,
                    });
                }
            }
        }
        let views = (!spheres.is_empty()
            && xrutil::locate_views(&session, time, &local, &mut buffers.views))
        .then_some(&buffers.views[..]);
        if let Some(ov) = &mut overlay {
            ov.render(&gl, views.unwrap_or(&[]), spheres)?;
        }
        let projection_views = overlay
            .as_ref()
            .zip(views)
            .and_then(|(o, v)| o.layer_views(v));
        let hands_layer = projection_views
            .as_ref()
            .map(|v| overlay::projection_layer(&local, v));

        // ---- Layers, back to front ----
        // Desktop text is denser than the display, so ask the compositor to supersample
        // and sharpen instead of shimmering through plain bilinear sampling.
        let quality = xr::sys::CompositionLayerSettingsFB {
            ty: xr::sys::CompositionLayerSettingsFB::TYPE,
            next: ptr::null(),
            layer_flags: xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SUPER_SAMPLING
                | xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SHARPENING,
        };
        let quality_next: *const std::ffi::c_void = if exts.fb_composition_layer_settings {
            (&raw const quality).cast()
        } else {
            ptr::null()
        };
        // Android surfaces store rows top-down while the compositor samples them
        // GL-style (bottom-up), which shows the desktop upside down.
        let flip = xr::sys::CompositionLayerImageLayoutFB {
            ty: xr::sys::CompositionLayerImageLayoutFB::TYPE,
            next: quality_next.cast_mut(),
            flags: xr::sys::CompositionLayerImageLayoutFlagsFB::VERTICAL_FLIP,
        };
        let video_next = if exts.fb_composition_layer_image_layout {
            (&raw const flip).cast()
        } else {
            quality_next
        };

        // The keyboard window: placed in front of the user at desk height the first
        // time, then wherever it was moved.
        let mut keyboard_failed = false;
        if let Some(pt) = shell
            .passthrough
            .as_mut()
            .filter(|pt| pt.window_supported())
        {
            let shown = shell
                .model
                .keyboard_window()
                .map(|p| (posef(&p), p.width, p.width * KEYBOARD_ASPECT));
            if let Err(e) = pt.set_window(&local, time, shown) {
                warn!("keyboard window: {e:#}");
                keyboard_failed = true;
            }
        }
        if keyboard_failed {
            shell.handle(now, Event::Platform(PlatformEvent::KeyboardWindowFailed));
        }
        let passthrough_layer = |handle: Option<xr::sys::PassthroughLayerFB>| {
            handle.map(|layer_handle| xr::sys::CompositionLayerPassthroughFB {
                ty: xr::sys::CompositionLayerPassthroughFB::TYPE,
                next: ptr::null(),
                flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
                space: xr::sys::Space::NULL,
                layer_handle,
            })
        };
        let keyboard_layer = passthrough_layer(
            shell
                .passthrough
                .as_ref()
                .and_then(passthrough::Passthrough::window_layer),
        );
        let passthrough_layer = passthrough_layer(
            shell
                .passthrough
                .as_ref()
                .and_then(passthrough::Passthrough::room_layer),
        );
        let model = &shell.model;
        let world = model.world();
        let pano = pano_layer
            .as_ref()
            .filter(|(b, _)| *b == model.settings().background)
            .map(|(_, l)| xr::sys::CompositionLayerEquirect2KHR {
                ty: xr::sys::CompositionLayerEquirect2KHR::TYPE,
                next: ptr::null(),
                layer_flags: xr::CompositionLayerFlags::EMPTY,
                space: local.as_raw(),
                eye_visibility: xr::EyeVisibility::BOTH,
                sub_image: l.sub_image(),
                pose: xr::Posef::IDENTITY,
                radius: 0.0,
                central_horizontal_angle: std::f32::consts::TAU,
                upper_vertical_angle: std::f32::consts::FRAC_PI_2,
                lower_vertical_angle: -std::f32::consts::FRAC_PI_2,
            });

        // Far monitors first so nearer (and grabbed) ones draw on top.
        let curve = world.curve_radius(&settings);
        let order = world.draw_list();
        let dims = &mut buffers.dims;
        dims.clear();
        dims.extend(order.iter().map(|(_, id)| {
            // Fading in: premultiplied, so colour and alpha scale together.
            let t = world.appear(*id);
            let t = t * t * (3.0 - 2.0 * t);
            // Focus dimming, the brightness setting and fading in, together.
            let b = world.brightness(*id) * t * f32::from(settings.brightness) / 100.0;
            xr::sys::CompositionLayerColorScaleBiasKHR {
                ty: xr::sys::CompositionLayerColorScaleBiasKHR::TYPE,
                next: video_next,
                color_scale: xr::Color4f {
                    r: b,
                    g: b,
                    b,
                    a: t,
                },
                color_bias: xr::Color4f {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                },
            }
        }));
        let (quads, cylinders) = (&mut buffers.quads, &mut buffers.cylinders);
        quads.clear();
        cylinders.clear();
        for (dim, (_, id)) in dims.iter().zip(order) {
            let scale = dim.color_scale;
            let fading = exts.khr_composition_layer_color_scale_bias && scale.a < 1.0;
            let layer_flags = if fading {
                xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA
            } else {
                xr::CompositionLayerFlags::EMPTY
            };
            let next = if exts.khr_composition_layer_color_scale_bias && scale.r < 1.0 {
                ptr::from_ref(dim).cast()
            } else {
                video_next
            };
            let (Some(m), Some(p)) = (shell.monitors.get(id), world.shown(*id)) else {
                continue;
            };
            let height = p.width * m.aspect();
            let sub_image = xr::sys::SwapchainSubImage {
                swapchain: m.swapchain.as_raw(),
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di {
                        width: m.width_px as i32,
                        height: m.height_px as i32,
                    },
                },
                image_array_index: 0,
            };
            // Window panels stay flat.
            if let Some(radius) = curve.filter(|_| !world.is_window(*id)) {
                let c = Cylinder::new(p, radius);
                cylinders.push(xr::sys::CompositionLayerCylinderKHR {
                    ty: xr::sys::CompositionLayerCylinderKHR::TYPE,
                    next,
                    layer_flags,
                    space: local.as_raw(),
                    eye_visibility: xr::EyeVisibility::BOTH,
                    sub_image,
                    pose: pose(c.center, p.rotation),
                    radius: c.radius,
                    central_angle: c.central_angle,
                    aspect_ratio: c.arc_length() / height,
                });
            } else {
                quads.push(xr::sys::CompositionLayerQuad {
                    ty: xr::sys::CompositionLayerQuad::TYPE,
                    next,
                    layer_flags,
                    space: local.as_raw(),
                    eye_visibility: xr::EyeVisibility::BOTH,
                    sub_image,
                    pose: posef(p),
                    size: xr::Extent2Df {
                        width: p.width,
                        height,
                    },
                });
            }
        }

        let button = world
            .button()
            .map(|b| button_layer.quad(&local, posef(&b), BUTTON_SIZE, BUTTON_SIZE));
        let panel_quad = (model.menu().open && panel_layer.ready()).then(|| {
            panel_layer.quad(
                &local,
                posef(&world.menu()),
                panel_width(settings.large_menu),
                panel_height(settings.large_menu),
            )
        });
        let cursors = &mut buffers.cursors;
        cursors.clear();
        cursors.extend(hands.picks().iter().flatten().map(|&Target { hit, .. }| {
            let size = 0.011 * hit.t.clamp(0.3, 4.0);
            let at = hit.point + hit.normal * 0.004;
            cursor_layer.quad(
                &local,
                pose(at, layout::facing(at, at + hit.normal)),
                size,
                size,
            )
        }));
        // Under a fingertip near the menu: a dot that shrinks as the finger gets closer.
        for (_, at, z, _) in hands.pokes() {
            let place = &world.menu();
            let normal = place.rotation * Vec3::Z;
            let point =
                place.position + place.rotation * Vec3::new(at.x, at.y, 0.0) + normal * 0.002;
            let size = 0.006 + z.max(0.0) * 0.15;
            cursors.push(cursor_layer.quad(
                &local,
                pose(point, layout::facing(point, point + normal)),
                size,
                size,
            ));
        }

        let mut layers = reuse(std::mem::take(&mut buffers.layers));
        if let Some(l) = &passthrough_layer {
            layers.push(as_layer(l));
        } else if let Some(l) = &pano {
            layers.push(as_layer(l));
        }
        layers.extend(keyboard_layer.iter().map(as_layer));
        layers.extend(cylinders.iter().map(as_layer));
        layers.extend(quads.iter().map(as_layer));
        let placeholder_quad = world.adding().and_then(|a| {
            let (layer, _, _) = placeholder.as_ref()?;
            Some(layer.quad(&local, posef(&a.place), a.place.width, a.height()))
        });
        layers.extend(placeholder_quad.iter().map(as_layer));
        layers.extend(button.iter().map(as_layer));
        layers.extend(panel_quad.iter().map(as_layer));
        let toast_quad = model.toast().and_then(|t| t.place).map(|p| {
            toast_layer.quad(
                &local,
                posef(&p),
                TOAST_WIDTH,
                TOAST_WIDTH * TOAST_PX.1 as f32 / TOAST_PX.0 as f32,
            )
        });
        layers.extend(toast_quad.iter().map(as_layer));
        let attach_quad = hands
            .attach_for()
            .filter(|_| attach_drawn.is_some())
            .and_then(|id| world.attach_place(id))
            .map(|p| attach_layer.quad(&local, posef(&p), ATTACH_WIDTH, attach_height()));
        layers.extend(attach_quad.iter().map(as_layer));
        let wrist_quad = world
            .wrist()
            .filter(|_| wrist_layer.ready())
            .map(|w| wrist_layer.quad(&local, posef(&w), WRIST_MENU_WIDTH, wrist_menu_height()));
        layers.extend(wrist_quad.iter().map(as_layer));
        layers.extend(hands_layer.iter().map(as_layer));
        layers.extend(cursors.iter().map(as_layer));
        let layers_wanted = layers.len();
        // The compositor supports 16 layers at most.
        layers.truncate(LAYERS_MAX);
        let ended = frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &layers);
        buffers.layers = reuse(layers);
        match ended {
            // One frame the compositor refused is not worth closing the app over.
            Err(
                e @ (xr::sys::Result::ERROR_POSE_INVALID
                | xr::sys::Result::ERROR_LAYER_INVALID
                | xr::sys::Result::ERROR_SWAPCHAIN_RECT_INVALID),
            ) => {
                warn!("frame refused: {e}");
            }
            other => other?,
        }
        if let (Some(t), Some(start)) = (test.as_deref_mut(), &frame_start) {
            t.frame_end(
                start,
                time.as_nanos(),
                Duration::from_nanos(state.predicted_display_period.as_nanos().max(0) as u64),
                menu_drawn,
                layers_wanted,
            );
        }
        allocations.frame();
    }

    info!("exiting");
    // What waits is written, and the files' writer ends (see `Shell`'s `Drop`).
    drop(shell);
    Ok(())
}

/// Builds without the selftest (the store's): the same calls, for a selftest that
/// never exists, so they do nothing and cost nothing.
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
mod no_selftest {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use android_activity::AndroidApp;
    use openxr as xr;
    use prostir_proto::state::{DisplayId, HeadsetCaps};

    use crate::net::NetEvent;

    pub fn count_run() -> u32 {
        0
    }

    #[expect(
        dead_code,
        reason = "made only by a selftest, which this build has not"
    )]
    pub enum Act {
        Nothing,
        MoveMonitor,
        OpenMenu,
        SwitchTab,
        CloseMenu,
        AddMonitor(u32, u32),
        RemoveMonitor(DisplayId),
        Finish,
    }

    pub struct FrameStart;

    /// No value of it can be made.
    pub enum Selftest {}

    impl Selftest {
        pub fn requested(_app: &AndroidApp, _run: u32) -> Option<Self> {
            None
        }

        pub fn finish(self, _: Option<&Path>, _: Option<&anyhow::Error>) -> (bool, i32) {
            match self {}
        }

        pub fn runtime(
            &self,
            _: &AndroidApp,
            _: &xr::Instance,
            _: xr::SystemId,
            _: &xr::ExtensionSet,
        ) {
            match *self {}
        }

        pub fn headset_caps(&self, _: &HeadsetCaps) {
            match *self {}
        }

        pub fn data_dir(&self, _: &Path) -> PathBuf {
            match *self {}
        }

        pub fn start_pc(&self, _: &AndroidApp, _: &Path) -> Option<std::net::SocketAddr> {
            match *self {}
        }

        pub fn over(&self) -> bool {
            match *self {}
        }

        pub fn session_state(&self, _: xr::SessionState) {
            match *self {}
        }

        pub fn refresh(&self, _: Option<Vec<f32>>, _: Option<f32>) {
            match *self {}
        }

        pub fn net_event(&self, _: &NetEvent) {
            match *self {}
        }

        pub fn loop_top(&self) {
            match *self {}
        }

        pub fn step(&self) -> Act {
            match *self {}
        }

        pub fn frame_start(&self) -> FrameStart {
            match *self {}
        }

        pub fn frame_end(
            &self,
            _: &FrameStart,
            _: i64,
            _: Duration,
            _: Option<Duration>,
            _: usize,
        ) {
            match *self {}
        }

        pub fn saved(&self, _: Duration) {
            match *self {}
        }
    }
}
