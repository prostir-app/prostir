//! OpenXR main loop.
//!
//! Each monitor of the PC is decoded by MediaCodec straight into an Android surface
//! owned by the Quest compositor (XR_KHR_android_surface_swapchain) and shown as
//! a quad or cylinder layer. The menu, its button, pointer dots and the
//! panorama are small CPU-drawn images; nothing is rendered per frame.

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
use glam::{Vec2, Vec3};
use log::{error, info, warn};
use openxr as xr;
use openxr::sys::Handle as _;

use crate::app::{
    By, ClockFace, Scene, handle_action, handle_net_event, headset_state, tab_from_code,
    take_command,
};
use crate::audio::Gains;
use crate::ids::{DisplayKey, Target, Thing};
use crate::input::Controls;
use crate::lang::{Moment, Texts};
use crate::layout::{self, Cylinder, Grab, Hit, Placement};
use crate::mic::Mic;
use crate::net::NetEvent;
use crate::note::{self, Notes};
use crate::overlay::{self, Overlay, Sphere};
use crate::presence;
#[cfg(all(feature = "selftest", not(feature = "store")))]
use crate::selftest::device::{self as selftest, Act, Selftest};
use crate::session::{self, Attempt, Session};
use crate::settings::{Background, HandMode, Settings};
use crate::store::StoreCheck;
use crate::system::SystemEvent;
use crate::ui::{self, Action, Panel, Tab};
use crate::world::{
    ATTACH_PX, ATTACH_WIDTH, BUTTON_SIZE, KEYBOARD_ASPECT, Pickable, WRIST_MENU_WIDTH, World,
    attach_height, panel_height, panel_width, wrist_menu_height,
};
use crate::xrutil::{self, Gfx, ImageLayer, as_layer, pose, posef, vec3};
use crate::{background, egl, net, passthrough, persist};
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
use no_selftest::{self as selftest, Act, Selftest};

const CURSOR_PX: u32 = 64;
const BUTTON_PX: u32 = 128;
/// Toast texture and its size in the room.
const TOAST_PX: (u32, u32) = (1024, 132);
const TOAST_WIDTH: f32 = 0.46;
/// How long a toast stays.
const TOAST_FOR: Duration = Duration::from_secs(4);
/// How long a pinch must be held on a monitor before it starts dragging it.
const HOLD_TO_DRAG: Duration = Duration::from_millis(300);

/// `XR_HAND_JOINT_INDEX_TIP_EXT`.
const INDEX_TIP: usize = 10;
/// Touching the menu with a fingertip, like a touchscreen. Distances from the menu's
/// surface, positive in front of it.
const POKE_HOVER: f32 = 0.06;
const POKE_PRESS: f32 = 0.008;
const POKE_RELEASE: f32 = 0.025;
/// A finger further behind the menu than this is not touching it (reaching past).
const POKE_BEHIND: f32 = -0.06;

/// Width of the placeholder's picture, in pixels (its height follows the monitor's
/// shape).
const PLACEHOLDER_PX: u32 = 1024;

/// A monitor held with both hands: the distance between them sets its width.
#[derive(Clone, Copy)]
pub(crate) struct Scaling {
    pub(crate) id: DisplayKey,
    /// The hand that took hold second (the first one keeps its ordinary grab).
    pub(crate) hand: usize,
    pub(crate) start_distance: f32,
    pub(crate) start_width: f32,
}

/// One fingertip against the menu.
#[derive(Default, Clone, Copy)]
struct Poke {
    /// Distance to the menu in the previous frame, while in front of it.
    last_z: Option<f32>,
    /// Touched and not yet pulled back.
    pressed: bool,
    /// Dragging the menu: the touched point in the menu's own plane.
    drag: Option<Vec2>,
    /// The last touch: pulling the hand back often closes thumb and index into a
    /// pinch, which must not click whatever this hand's ray points at.
    touched_at: Option<Instant>,
}

/// How long the ring of a touch spreads.
const TOUCH_RING: Duration = Duration::from_millis(350);

/// Pinches of a hand that just touched the menu are ignored this long.
const AFTER_TOUCH: Duration = Duration::from_millis(600);

#[unsafe(no_mangle)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "the signature android-activity calls"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
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
pub(crate) fn local_time() -> Option<libc::tm> {
    // SAFETY: plain libc time calls on a zeroed `tm`.
    unsafe {
        let now = libc::time(ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&raw const now, &raw mut tm).is_null()).then_some(tm)
    }
}

/// A moment of the local time, as the menu writes it.
pub(crate) fn moment(tm: &libc::tm) -> Moment {
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
pub(crate) struct Monitor {
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    pub(crate) decoder: Option<JoinHandle<()>>,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) swapchain: xr::Swapchain<Gfx>,
}

impl Monitor {
    pub(crate) fn aspect(&self) -> f32 {
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

/// How long the "Attach" button under a pointed-at detached window stays after the
/// pointer left.
const ATTACH_LINGER: Duration = Duration::from_millis(1200);
/// How long an open palm faces the face before its menu appears.
const PALM_DWELL: Duration = Duration::from_millis(250);
/// How long the "where is the mouse" rings show.
const FINDER_FOR: Duration = Duration::from_millis(1600);

/// Where the palm menu goes: lying on the palm (a little above it, its top towards the
/// fingers), while an open palm is turned to the face. It takes a quarter of a second
/// of that to appear, so hands passing by (typing, reaching) do not bring it up.
/// `since`: when the palm started facing the face.
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
fn palm_menu_place(
    joints: Option<&[(Vec3, f32); 26]>,
    right_hand: bool,
    head: Vec3,
    previous: Option<Placement>,
    since: &mut Option<Instant>,
) -> Option<Placement> {
    let Some(j) = joints else {
        *since = None;
        return None;
    };
    let (palm, wrist) = (j[0].0, j[1].0);
    // Knuckles, a hand's width apart: a steady palm normal. For the left hand it is
    // little × index from the wrist (checked in the headset: the other order points
    // out of the back of the hand); the right hand is its mirror image.
    let (index, middle, little, middle_tip) = (j[7].0, j[12].0, j[22].0, j[15].0);
    let mut normal = (little - wrist).cross(index - wrist).normalize_or_zero();
    if right_hand {
        normal = -normal;
    }
    let facing = normal.dot((head - palm).normalize_or_zero());
    let open = middle_tip.distance(palm) > 0.07;
    let needed = if previous.is_some() { 0.55 } else { 0.8 };
    if facing < needed || !open {
        *since = None;
        return None;
    }
    if previous.is_none() && since.get_or_insert_with(Instant::now).elapsed() < PALM_DWELL {
        return None;
    }
    let up = (middle - wrist)
        .reject_from_normalized(normal)
        .normalize_or(Vec3::Y);
    let rotation = glam::Quat::from_mat3(&glam::Mat3::from_cols(up.cross(normal), up, normal));
    let mut at = palm + normal * 0.03;
    let mut rotation = rotation.normalize();
    // Hands shake a little: the menu follows smoothly.
    if let Some(p) = previous {
        at = p.position.lerp(at, 0.35);
        rotation = p.rotation.slerp(rotation, 0.35);
    }
    Some(Placement {
        position: at,
        rotation,
        width: WRIST_MENU_WIDTH,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "looked for when the menu opens, in the frame, until step 7.7 moves it out"
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

/// The app; with `test`, the selftest's run of it (see `selftest::device`).
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
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
    // and `Scene`'s `Drop` once the scene has it).
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
    let mut passthrough = if exts.fb_passthrough {
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
    cursor_layer.upload(&gl, &ui::cursor_image(CURSOR_PX))?;
    let mut button_hovered = false;
    // The left hand's menu gesture as last seen, to act once per gesture.
    let mut menu_gesture_held = false;
    // Whether the palm menu showed last frame (it shows the battery).
    let mut palm_was_shown = false;
    button_layer.upload(&gl, &ui::menu_button_image(BUTTON_PX, false))?;
    let mut panel = Panel::new();
    let mut wrist_panel = Panel::wrist();
    let mut wrist_layer =
        ImageLayer::new(&session, format, ui::WRIST_WIDTH, ui::WRIST_HEIGHT, false)?;
    // The right fingertip left the wrist menu since its last press.
    let mut wrist_poke_armed = true;
    // Since when an open palm faces the face (its menu shows after a moment).
    let mut palm_since: Option<Instant> = None;
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
    let mut custom_pano = custom_panorama_path(external_dir.as_deref()).is_some();
    let mut panel_was_open = false;

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
    let system_thread = crate::system::System::start(lang_loader);

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
        headset_state(&settings, true, battery, &mic, &model),
    )?;

    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let _wifi_lock = xrutil::acquire_low_latency_wifi(app, &vm)
        .inspect(|_| info!("Wi-Fi low-latency mode on"))
        .inspect_err(|e| warn!("Wi-Fi low-latency lock unavailable: {e:#}"))
        .ok();

    let world = World::new(
        desk,
        keyboard_place,
        exts.khr_composition_layer_cylinder,
        &settings,
    );
    let mut scene = Scene {
        monitors: HashMap::new(),
        world,
        mirror_wanted: true,
        mic_wanted: settings.mic,
        scaling: None,
        presence: presence::Presence::new(),
        settings,
        panel_open: false,
        grabs: [None, None],
        ip_input: None,
        session: Session::new(session::MAX_PCS, fake_pc, Instant::now()),
        outboxes: BTreeMap::new(),
        mic_permission: mic.permission(),
        mic,
        gains,
        error: None,
        confirm_remove: None,
        confirm_save: None,
        confirm_forget: None,
        tab: tab_from_code(settings.last_tab),
        attach_for: None,
        selected: None,
        headset_battery: battery,
        model,
        refresh_due: false,
        worn_for: Duration::ZERO,
        off_since: None,
        gaze_on: None,
        gaze_moved: None,
        finder: None,
        toast: None,
        notes: Notes::default(),
        press_in_menu: false,
        language_asked: false,
        forgetting: None,
        head: (Vec3::ZERO, -Vec3::Z),
        head_right: Vec3::X,
        view_eye: None,
        hands_in_view: false,
        system_lang,
        lang,
        clock: ClockFace::default(),
        language_page: 0,
        profiles: saved_profiles,
        active_profile: None,
        saves: persist::Dirty::default(),
        save_failures_told: persist::Dirty::default(),
        saver,
    };
    // The language's file had lines that could not be read: said once, in front.
    if let Some(warning) = scene.texts().warning() {
        let text = warning.to_owned();
        scene.show_toast(text);
    }
    let mut hold_since: [Option<Instant>; 2] = [None, None];
    // A hand's pinch on a menu button: clicked when let go, unless it was held long
    // enough to drag the menu instead (as on the Quest's own windows).
    let mut pending_click: [Option<(Action, Vec3)>; 2] = [None, None];
    // Grabs started on a monitor's bar (they end when the press ends).

    let mut pokes = [Poke::default(); 2];
    // Touches that just happened: (where, when), drawn as a ring spreading out.
    let mut rings: Vec<(Vec3, Instant)> = Vec::new();
    // Where this build comes from. A store build checks the user owns the app, as its
    // store requires at launch; a build from GitHub has nothing to check.
    let mut store = crate::store::of_this_build(app);
    // Since when the store says the app is not the user's: it closes after saying so.
    let mut not_owned_since: Option<Instant> = None;
    // What the person is told then, and the language it is in.
    let mut not_owned_text: Option<(Texts, String)> = None;
    let mut session_running = false;
    // The microphone's asks for access seen so far (see `Mic::asks`).
    let mut mic_asks = 0;
    // When the screen was last read, while the app is put away.
    let mut screen_read: Option<Instant> = None;
    // The input is the app's: false while the system menu is open over it (the app
    // keeps drawing, but shows no hands or rays of its own and takes no input).
    let mut focused = false;
    let mut last_time: Option<xr::Time> = None;
    let mut event_storage = xr::EventDataBuffer::new();
    // What the selftest asked for, done in the next frame drawn.
    let mut selftest_act = Act::Nothing;
    let mut allocations = crate::alloc_count::Tally::default();

    'main: loop {
        if let Some(t) = test.as_deref_mut() {
            t.loop_top();
        }
        // What the last pass changed goes to the files' writer, and what the core asked
        // of the network to its thread, however the pass ended.
        scene.save_due();
        for cmd in scene.session.take_cmds() {
            network.send(cmd);
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
        if destroy {
            break;
        }
        // The microphone asked the system for access: the prompt's pause is the app's
        // own, not the user leaving.
        let asks = scene.mic.asks();
        if asks != mic_asks {
            mic_asks = asks;
            scene.presence.asked(Instant::now());
        }
        if let Some(paused) = activity {
            // The system may freeze a paused app soon: the PC hears it first.
            scene.presence.paused(paused);
            scene.presence_now(Instant::now(), &mut screen_read);
        }
        if paused {
            // The system may end a paused app without a word: what waits is written now.
            scene.flush_saves("paused");
        }
        if let StoreCheck::NotOwned { store: name } = store.poll() {
            let since = *not_owned_since.get_or_insert_with(Instant::now);
            // Written once in each language, then compared with what the menu shows.
            if not_owned_text
                .as_ref()
                .is_none_or(|(texts, _)| texts != scene.texts())
            {
                let text = scene
                    .texts()
                    .f("store-not-confirmed", &[("store", name.into())]);
                not_owned_text = Some((scene.texts().clone(), text));
            }
            let what = not_owned_text
                .as_ref()
                .map_or("", |(_, text)| text.as_str());
            if scene.error.as_ref().is_none_or(|(t, _)| t != what) {
                scene.show_error(what.to_owned());
                // In front of the user, wherever the menu was last.
                let (head, forward) = scene.head;
                let hands = scene.hands_in_view;
                scene.open_panel(head, forward, hands);
                // The 6 s count from when the message is up, not from the failure.
                not_owned_since = Some(Instant::now());
            }
            // Long enough to read, then close as the store expects.
            if since.elapsed() > Duration::from_secs(6) {
                break;
            }
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
                        scene.grabs = [None, None];
                        scene.scaling = None;
                        scene.world.cancel_holds();
                    }
                    focused = now_focused;
                    // Hidden or stopped, the PC stops encoding until the app is shown
                    // again; told before the session ends and the files are written.
                    let shown = match e.state() {
                        xr::SessionState::VISIBLE | xr::SessionState::FOCUSED => {
                            Some(presence::Session::Shown)
                        }
                        xr::SessionState::READY | xr::SessionState::SYNCHRONIZED => {
                            Some(presence::Session::Hidden)
                        }
                        xr::SessionState::STOPPING | xr::SessionState::IDLE => {
                            Some(presence::Session::Stopped)
                        }
                        _ => None,
                    };
                    if let Some(shown) = shown {
                        scene.presence.session(shown);
                        scene.presence_now(Instant::now(), &mut screen_read);
                    }
                    match e.state() {
                        xr::SessionState::READY => {
                            session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                            session_running = true;
                            if exts.fb_display_refresh_rate {
                                set_refresh_rate(&session, scene.settings.battery_saver);
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
                            scene.flush_saves("stopping");
                        }
                        xr::SessionState::IDLE => {
                            scene.flush_saves("idle");
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => break 'main,
                        _ => {}
                    }
                }
                xr::Event::UserPresenceChangedEXT(e) => {
                    scene.presence.user(Instant::now(), e.is_user_present());
                }
                xr::Event::InstanceLossPending(_) => break 'main,
                _ => {}
            }
        }
        // The sensor's "away" counts once it lasted (see `presence::AWAY_GRACE`), and
        // the screen is read while the app is put away: looked at every pass, also
        // while nothing is drawn.
        scene.presence_now(Instant::now(), &mut screen_read);

        while let Ok(event) = net_rx.try_recv() {
            if let Some(t) = test.as_deref_mut() {
                t.net_event(&event);
            }
            match event {
                NetEvent::Command { pc, req, asked } => {
                    take_command(&mut scene, pc, req, &asked, &mut passthrough, &mut controls);
                }
                event => handle_net_event(&mut scene, &session, &vm, event),
            }
            scene.world_done();
        }
        // The search goes on with or without frames (see `Session::tick`).
        scene.session.tick(Instant::now());
        // Only the newest battery reading matters (several wait after a pause).
        let mut newest = None;
        while let Some(event) = system_thread.try_recv() {
            match event {
                SystemEvent::Battery(reading) => newest = Some(reading),
                SystemEvent::Texts(texts) => {
                    if scene.lang.loaded(texts) {
                        scene.language_shown();
                    }
                }
                SystemEvent::Languages(known) => {
                    scene.lang.listed(known);
                }
            }
        }
        if let Some(reading) = newest {
            scene.battery_read(reading);
        }
        scene.saves_told();
        // Another language chosen (or the PC's changed): loaded on the system thread,
        // the menu keeps the one it has until then.
        if let Some(lang) = scene.wanted_lang() {
            system_thread.load_language(lang);
        }
        scene.tick_clock();

        // Panorama: generated or decoded off the main thread, uploaded once per choice.
        let wanted = scene.settings.background;
        let wants_pano = matches!(wanted, Background::Sky(_) | Background::Custom);
        if wants_pano
            && exts.khr_composition_layer_equirect2
            && pano_layer.as_ref().is_none_or(|(b, _)| *b != wanted)
            && pano_job.is_none()
        {
            let dir = external_dir.clone();
            pano_job = Some((
                wanted,
                std::thread::spawn(move || load_panorama(wanted, dir.as_deref())),
            ));
        }
        if pano_job.as_ref().is_some_and(|(_, j)| j.is_finished()) {
            let (made_for, job) = pano_job.take().expect("checked just above");
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

        // The core's time goes on with or without frames (the session not running, the
        // headset off the head, nothing to render): requests time out, a placeholder
        // goes, and the desktop's arrangement is told once it stood.
        scene.expire_asks();
        scene.adding_due();
        scene.world.settle(Instant::now());
        scene.world_done();

        if !session_running {
            continue;
        }

        let state = frame_waiter.wait()?;
        let frame_start = test.as_deref().map(Selftest::frame_start);
        frame_stream.begin()?;
        let time = state.predicted_display_time;
        if !state.should_render {
            frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &[])?;
            continue;
        }
        // How long the menu took to draw in this frame, for the selftest.
        let mut menu_drawn: Option<Duration> = None;
        let dt = last_time
            .map_or(0.0f32, |t| (time.as_nanos() - t.as_nanos()) as f32 * 1e-9)
            .clamp(0.0, 0.1);
        last_time = Some(time);

        controls.sync(&session, &local, time)?;
        if !focused {
            controls.state = Default::default();
        }
        let (head, forward, right) =
            view.locate(&local, time)
                .map_or((Vec3::ZERO, -Vec3::Z, Vec3::X), |l| {
                    let turn = xrutil::quat(l.pose.orientation);
                    (vec3(l.pose.position), turn * -Vec3::Z, turn * Vec3::X)
                });

        scene.head = (head, forward);
        scene.head_right = right;
        if scene.view_eye.is_none_or(|e| e.distance(head) > 0.05) && head != Vec3::ZERO {
            scene.view_eye = Some(head);
        }
        scene.world.follow_head(&scene.settings, dt, scene.head);
        scene.place_sound();
        scene.remind_rest(dt);
        if std::mem::take(&mut scene.refresh_due) && exts.fb_display_refresh_rate {
            set_refresh_rate(&session, scene.settings.battery_saver);
        }
        if focused {
            scene.follow_gaze();
        }

        // Tracked hands: joints for drawing, fingertips for touching the menu.
        let joints: [Option<[(Vec3, f32); 26]>; 2] = match &hand_trackers {
            Some(trackers) if focused && scene.settings.hands != HandMode::Hidden => {
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
        let tips: [Option<Vec3>; 2] = std::array::from_fn(|h| joints[h].map(|j| j[INDEX_TIP].0));
        let hands_in_view = tips.iter().any(Option::is_some);
        scene.hands_in_view = hands_in_view;
        controls.hands_from_joints(&joints, head, forward);
        let menu_hand = usize::from(scene.settings.left_handed);
        let wrist = palm_menu_place(
            joints[menu_hand].as_ref(),
            menu_hand == 1,
            head,
            scene.world.wrist(),
            &mut palm_since,
        );
        scene.world.set_wrist(wrist);

        // Pairing needs the menu, and so do the first-run tips: open it where the
        // user is looking.
        let search = scene.session.search();
        let first_run = (!scene.settings.intro_seen && scene.session.host().is_some())
            || search.update.is_some();
        let asking = matches!(
            search.attempt,
            Some(Attempt::Pairing { .. } | Attempt::PairOffer { .. })
        );
        if (asking || first_run) && !scene.panel_open {
            scene.open_panel(head, forward, hands_in_view);
        }
        // Without a PC there is nothing to see: show the connection screen.
        if !scene.panel_open
            && scene
                .session
                .offline_for(Instant::now(), Duration::from_secs(4))
        {
            scene.open_panel(head, forward, hands_in_view);
        }
        // The left hand's menu gesture counts as the menu button.
        let gesture = match &hand_trackers {
            Some(t) if exts.fb_hand_tracking_aim => {
                xrutil::hand_menu_gesture(&instance, &t[0], &local, time)
            }
            _ => false,
        };
        let gesture_pressed = gesture && !menu_gesture_held;
        menu_gesture_held = gesture;
        if controls.menu_pressed(&session) || gesture_pressed {
            if scene.panel_open {
                scene.panel_open = false;
            } else {
                scene.open_panel(head, forward, hands_in_view);
            }
        }
        if controls.align_pressed(&session) {
            scene.tidy_up();
        }
        // What the selftest asks for, as the menu would do it.
        match std::mem::replace(&mut selftest_act, Act::Nothing) {
            Act::MoveMonitor => {
                if let Some(&(id, _, _)) = scene.world.monitors().first() {
                    let took = Instant::now();
                    let action = Action::Move { id, delta: 1 };
                    handle_action(
                        &mut scene,
                        action,
                        By::Elsewhere,
                        &mut passthrough,
                        &mut controls,
                    );
                    // Handed to the files' writer now, so the time is the whole of what
                    // saving costs the frame.
                    scene.save_due();
                    if let Some(t) = test.as_deref_mut() {
                        t.saved(took.elapsed());
                    }
                }
            }
            Act::OpenMenu => scene.open_panel(head, forward, hands_in_view),
            Act::SwitchTab => {
                let tab = match scene.tab {
                    Tab::Monitors => Tab::Quick,
                    _ => Tab::Monitors,
                };
                let took = Instant::now();
                handle_action(
                    &mut scene,
                    Action::Tab(tab),
                    By::Elsewhere,
                    &mut passthrough,
                    &mut controls,
                );
                scene.save_due();
                if let Some(t) = test.as_deref_mut() {
                    t.saved(took.elapsed());
                }
            }
            Act::CloseMenu => {
                handle_action(
                    &mut scene,
                    Action::Close,
                    By::Elsewhere,
                    &mut passthrough,
                    &mut controls,
                );
            }
            Act::AddMonitor(width, height) => {
                let add = Action::Add { width, height };
                handle_action(
                    &mut scene,
                    add,
                    By::Elsewhere,
                    &mut passthrough,
                    &mut controls,
                );
            }
            Act::RemoveMonitor(id) => {
                // Pressed twice, as the menu asks to confirm.
                if let Some(k) = scene.key(id) {
                    for _ in 0..2 {
                        let remove = Action::Remove(k);
                        handle_action(
                            &mut scene,
                            remove,
                            By::Elsewhere,
                            &mut passthrough,
                            &mut controls,
                        );
                    }
                }
            }
            Act::Nothing | Act::Finish => {}
        }
        scene.world.place_button(&scene.settings);
        // The system may let the app record now, or not any more.
        if scene.mic.permission() != scene.mic_permission {
            scene.mic_changed();
            if scene.mic_wanted
                && let Some(told) = note::permission_note(scene.mic_permission, scene.texts())
            {
                scene.tell(told.note, told.toast, true);
            }
        }
        // The palm menu shows the battery: the system thread reads it afresh when the
        // menu appears, and more often while it shows.
        if scene.world.wrist().is_some() != palm_was_shown {
            palm_was_shown = scene.world.wrist().is_some();
            system_thread.palm_menu(palm_was_shown);
        }

        // Pointing, clicking and dragging.
        let can = Pickable {
            menu: scene.panel_open,
            attach: scene.attach_for.map(|(id, _)| id),
        };
        let mut picks: [Option<Target>; 2] = std::array::from_fn(|h| {
            let session = &scene.session;
            controls.state[h].ray.and_then(|r| {
                scene.world.pick(r, &scene.settings, can, |k| {
                    session.waits(ui::Asked::Remove(k))
                })
            })
        });
        // A detached window pointed at shows "Attach" under it; it lingers so the
        // pointer can move down onto it.
        let pointed = picks.iter().flatten().find_map(|p| match p.thing {
            Thing::Display(id) | Thing::Attach(id) if scene.world.is_window(id) => Some(id),
            _ => None,
        });
        if let Some(id) = pointed {
            scene.attach_for = Some((id, Instant::now()));
        } else if scene
            .attach_for
            .is_some_and(|(id, t)| t.elapsed() > ATTACH_LINGER || !scene.monitors.contains_key(&id))
        {
            scene.attach_for = None;
        }

        // The wrist menu: pointed at, or touched with the right index finger.
        let wrist_point = |local: Vec2| {
            (
                (local.x / WRIST_MENU_WIDTH + 0.5) * ui::WRIST_WIDTH as f32,
                (0.5 - local.y / wrist_menu_height()) * ui::WRIST_HEIGHT as f32,
            )
        };
        let mut wrist_hover = picks
            .iter()
            .flatten()
            .find_map(|p| matches!(p.thing, Thing::Wrist).then(|| wrist_point(p.hit.local)));
        let mut wrist_pressed = false;
        let poking_hand = 1 - usize::from(scene.settings.left_handed);
        if let (Some(w), Some(tip)) = (scene.world.wrist(), tips[poking_hand]) {
            let local = w.rotation.inverse() * (tip - w.position);
            let inside =
                local.x.abs() < WRIST_MENU_WIDTH / 2.0 && local.y.abs() < wrist_menu_height() / 2.0;
            if inside && local.z < 0.04 {
                wrist_hover = Some(wrist_point(Vec2::new(local.x, local.y)));
            }
            if inside && local.z < 0.008 && local.z > -0.03 && wrist_poke_armed {
                wrist_poke_armed = false;
                wrist_pressed = true;
                rings.push((tip, Instant::now()));
            }
            if !inside || local.z > 0.02 {
                wrist_poke_armed = true;
            }
        } else {
            wrist_poke_armed = true;
        }
        if scene.world.wrist().is_some() {
            let ui_state = scene.ui_state(
                passthrough.is_some(),
                passthrough
                    .as_ref()
                    .is_some_and(passthrough::Passthrough::window_supported),
                exts.khr_composition_layer_equirect2,
                custom_pano,
                store.update_hint(),
            );
            wrist_panel.set_state(&ui_state);
            wrist_panel.hover(wrist_hover);
            if wrist_pressed && let Some(action) = wrist_panel.hovered_action() {
                controls.pulse(&session, poking_hand);
                handle_action(
                    &mut scene,
                    action,
                    By::Elsewhere,
                    &mut passthrough,
                    &mut controls,
                );
            }
        }
        let panel_point = |hit: &Hit, large: bool| {
            let x = (hit.local.x / panel_width(large) + 0.5) * ui::WIDTH as f32;
            let y = (0.5 - hit.local.y / panel_height(large)) * ui::HEIGHT as f32;
            (x, y)
        };

        // Touching the menu with a fingertip: hover when close, press on contact,
        // drag the menu by touching an empty spot.
        let mut poke_hover: [Option<(Vec2, f32)>; 2] = [None, None];
        for h in 0..2 {
            let poke = &mut pokes[h];
            let Some(tip) = tips[h].filter(|_| scene.panel_open) else {
                *poke = Poke {
                    touched_at: poke.touched_at,
                    ..Poke::default()
                };
                continue;
            };
            let place = scene.world.menu();
            let l = place.rotation.inverse() * (tip - place.position);
            let (half_w, half_h) = (
                panel_width(scene.settings.large_menu) / 2.0,
                panel_height(scene.settings.large_menu) / 2.0,
            );
            if let Some(anchor) = poke.drag {
                if l.z > POKE_RELEASE {
                    *poke = Poke {
                        touched_at: poke.touched_at,
                        ..Poke::default()
                    };
                    continue;
                }
                // The touched point follows the finger; pushing deeper moves it away.
                let shift = Vec3::new(l.x - anchor.x, l.y - anchor.y, (l.z + 0.03).min(0.0));
                let position = place.position + place.rotation * shift;
                scene.world.move_menu(position, head);
                picks[h] = None;
                continue;
            }
            let inside = l.x.abs() <= half_w && l.y.abs() <= half_h;
            if !inside || l.z > POKE_HOVER || l.z < POKE_BEHIND {
                poke.last_z = None;
                poke.pressed = false;
                continue;
            }
            // Near the menu, the finger is the pointer, not this hand's ray.
            picks[h] = None;
            let at = Vec2::new(l.x, l.y);
            poke_hover[h] = Some((at, l.z));
            let px = (
                (l.x / panel_width(scene.settings.large_menu) + 0.5) * ui::WIDTH as f32,
                (0.5 - l.y / panel_height(scene.settings.large_menu)) * ui::HEIGHT as f32,
            );
            if poke.pressed && l.z > POKE_RELEASE {
                poke.pressed = false;
            }
            let arrived = poke.last_z.is_some_and(|z| z >= POKE_PRESS);
            // Hand tracking jitters: a finger "bouncing" off the menu is not a second tap.
            let settled = poke
                .touched_at
                .is_none_or(|t| t.elapsed() > Duration::from_millis(400));
            if !poke.pressed && arrived && settled && l.z < POKE_PRESS {
                poke.pressed = true;
                poke.touched_at = Some(Instant::now());
                rings.push((
                    place.position + place.rotation * Vec3::new(l.x, l.y, 0.003),
                    Instant::now(),
                ));
                panel.hover(Some(px));
                match panel.press() {
                    Some(action) => {
                        handle_action(
                            &mut scene,
                            action,
                            By::Menu,
                            &mut passthrough,
                            &mut controls,
                        );
                    }
                    None => poke.drag = Some(at),
                }
            }
            poke.last_z = Some(l.z);
        }

        let poke_px = poke_hover.iter().flatten().next().map(|(at, _)| {
            (
                (at.x / panel_width(scene.settings.large_menu) + 0.5) * ui::WIDTH as f32,
                (0.5 - at.y / panel_height(scene.settings.large_menu)) * ui::HEIGHT as f32,
            )
        });
        let hover = poke_px.or_else(|| {
            [1, 0].into_iter().find_map(|h| match picks[h] {
                Some(Target {
                    thing: Thing::Menu,
                    hit,
                }) => Some(panel_point(&hit, scene.settings.large_menu)),
                _ => None,
            })
        });
        panel.hover(hover);
        let over_button = picks.iter().any(|p| {
            matches!(
                p,
                Some(Target {
                    thing: Thing::MenuButton,
                    ..
                })
            )
        });
        if over_button != button_hovered {
            button_hovered = over_button;
            button_layer.upload(&gl, &ui::menu_button_image(BUTTON_PX, over_button))?;
        }

        // Both hands on one monitor: the distance between them sets its width.
        if let Some(s) = scene.scaling
            && let (Some(a), Some(b)) = (controls.state[0].ray, controls.state[1].ray)
        {
            let d = a.origin.distance(b.origin);
            let width = (s.start_width * d / s.start_distance.max(0.05)).clamp(0.2, 8.0);
            scene.world.resize(&scene.settings, width);
        }

        #[allow(clippy::needless_range_loop)] // `hand` indexes several per-hand arrays
        for hand in 0..2 {
            let mut input = controls.state[hand];
            // A hand that just touched the menu: its next pinch is the hand pulling
            // back, not a click.
            if input.is_hand
                && pokes[hand]
                    .touched_at
                    .is_some_and(|t| t.elapsed() < AFTER_TOUCH)
            {
                input.select.pressed = false;
            }
            if input.select.pressed {
                let mut clicked = false;
                match picks[hand] {
                    Some(Target {
                        thing: Thing::Attach(id),
                        hit,
                    }) => {
                        clicked = true;
                        rings.push((hit.point, Instant::now()));
                        controls.pulse(&session, hand);
                        scene.attach_for = None;
                        handle_action(
                            &mut scene,
                            Action::CloseWindow(id),
                            By::Elsewhere,
                            &mut passthrough,
                            &mut controls,
                        );
                    }
                    Some(Target {
                        thing: Thing::Menu,
                        hit,
                    }) => {
                        panel.hover(Some(panel_point(&hit, scene.settings.large_menu)));
                        if let Some(action) = panel.press() {
                            if input.is_hand {
                                // Decided when the pinch opens: a click, or a drag.
                                pending_click[hand] = Some((action, hit.point));
                            } else {
                                rings.push((hit.point, Instant::now()));
                                clicked = true;
                                controls.pulse(&session, hand);
                                handle_action(
                                    &mut scene,
                                    action,
                                    By::Menu,
                                    &mut passthrough,
                                    &mut controls,
                                );
                            }
                        }
                    }
                    Some(Target {
                        thing: Thing::Wrist,
                        hit,
                    }) => {
                        clicked = true;
                        wrist_panel.hover(Some(wrist_point(hit.local)));
                        if let Some(action) = wrist_panel.hovered_action() {
                            rings.push((hit.point, Instant::now()));
                            controls.pulse(&session, hand);
                            handle_action(
                                &mut scene,
                                action,
                                By::Elsewhere,
                                &mut passthrough,
                                &mut controls,
                            );
                        }
                    }
                    Some(Target {
                        thing: Thing::MenuButton,
                        hit,
                    }) => {
                        rings.push((hit.point, Instant::now()));
                        clicked = true;
                        controls.pulse(&session, hand);
                        if scene.panel_open {
                            scene.panel_open = false;
                        } else {
                            scene.open_panel(head, forward, hands_in_view);
                        }
                    }
                    _ => {}
                }
                // Hands drag with pinch-and-hold, like the Quest's own windows.
                if input.is_hand && !clicked && picks[hand].is_some() {
                    hold_since[hand] = Some(Instant::now());
                }
            }
            if input.select.released {
                hold_since[hand] = None;
                if let Some((action, point)) = pending_click[hand].take() {
                    rings.push((point, Instant::now()));
                    controls.pulse(&session, hand);
                    handle_action(
                        &mut scene,
                        action,
                        By::Menu,
                        &mut passthrough,
                        &mut controls,
                    );
                }
            }
            let (start_grab, end_grab) = if input.is_hand {
                let held = input.select.down
                    && hold_since[hand].is_some_and(|t| t.elapsed() >= HOLD_TO_DRAG);
                (held && scene.grabs[hand].is_none(), input.select.released)
            } else {
                (input.grab.pressed, input.grab.released)
            };

            if end_grab && scene.scaling.is_some_and(|s| s.hand == hand) {
                scene.scaling = None;
                scene.world.resize_end();
            }
            if end_grab && let Some(released) = scene.grabs[hand].take() {
                // Letting go with the first hand ends resizing too.
                scene.scaling = None;
                // Let go next to another monitor: it sticks to it, edge to edge.
                if scene
                    .world
                    .let_go(released.id, &scene.settings, Instant::now())
                {
                    controls.pulse(&session, hand);
                }
                scene.world_done();
            }
            let Some(ray) = input.ray else { continue };
            if let Some(mut grab) = scene.grabs[hand] {
                // Moved from where it goes now; the world keeps the keyboard flat, a
                // monitor on a wall parallel to it, and moves the rest along with
                // "move all together".
                if let Some(mut p) = scene.world.place(grab.id) {
                    grab.update(&mut p, ray, head, input.stick, dt);
                    scene.world.drag(&scene.settings, grab.id, p, head);
                    scene.grabs[hand] = Some(grab);
                }
            } else if start_grab {
                hold_since[hand] = None;
                // Held on a button: the menu moves, the button is not pressed.
                pending_click[hand] = None;
                let other = scene.grabs[1 - hand].map(|g| g.id);
                let grabbed = match picks[hand] {
                    Some(Target {
                        thing: thing @ (Thing::Menu | Thing::Keyboard),
                        hit,
                    }) if other != Some(thing) => {
                        take(&mut scene.world, thing, hit, &scene.settings)
                    }
                    // The other hand already holds it: resize with both.
                    Some(Target {
                        thing: Thing::Display(id),
                        ..
                    }) if other == Some(Thing::Display(id)) => {
                        if let (Some(a), Some(b)) = (controls.state[0].ray, controls.state[1].ray)
                            && let Some(start_width) = scene.world.resize_start(id)
                        {
                            scene.scaling = Some(Scaling {
                                id,
                                hand,
                                start_distance: a.origin.distance(b.origin),
                                start_width,
                            });
                            controls.pulse(&session, hand);
                        }
                        None
                    }
                    Some(Target {
                        thing: thing @ Thing::Display(_),
                        hit,
                    }) => take(&mut scene.world, thing, hit, &scene.settings),
                    _ => None,
                }
                .map(|g| {
                    if input.is_hand {
                        g.with_reach(ray.origin, head)
                    } else {
                        g
                    }
                });
                if grabbed.is_some() {
                    controls.pulse(&session, hand);
                    scene.grabs[hand] = grabbed;
                }
            }
        }

        // Menu contents.
        if scene.panel_open && !panel_was_open {
            // The user may have copied a picture onto the headset meanwhile.
            custom_pano = custom_panorama_path(external_dir.as_deref()).is_some();
            // Or a language file: listed again on the system thread.
            system_thread.list_languages();
        }
        panel_was_open = scene.panel_open;
        // A new toast: drawn once, placed where the user looks now.
        if let Some((text, _, place @ None)) = &mut scene.toast {
            toast_layer.upload(&gl, &panel.toast_image(text, TOAST_PX.0, TOAST_PX.1))?;
            let (head, forward) = scene.head;
            let at = head + forward.normalize_or(-Vec3::Z) * 0.7 - Vec3::Y * 0.18;
            *place = Some(Placement {
                position: at,
                rotation: layout::facing(at, head),
                width: TOAST_WIDTH,
            });
        }
        if scene
            .toast
            .as_ref()
            .is_some_and(|t| t.1.elapsed() > TOAST_FOR)
        {
            scene.toast = None;
        }
        if scene.attach_for.is_some() {
            let lit = picks
                .iter()
                .flatten()
                .any(|p| matches!(p.thing, Thing::Attach(_)));
            if attach_drawn
                .as_ref()
                .is_none_or(|(t, l)| t != scene.texts() || *l != lit)
            {
                let label = scene.texts().t("attach");
                attach_layer
                    .upload(&gl, &panel.pill_image(label, ATTACH_PX.0, ATTACH_PX.1, lit))?;
                attach_drawn = Some((scene.texts().clone(), lit));
            }
        }
        if let Some(a) = scene.world.adding()
            && placeholder
                .as_ref()
                .is_none_or(|p| p.1 != a.size || p.2 != *scene.texts())
        {
            let (w, h) = a.size;
            let px_h = (PLACEHOLDER_PX * h / w.max(1)).clamp(64, 4 * PLACEHOLDER_PX);
            let mut layer = ImageLayer::new(&session, format, PLACEHOLDER_PX, px_h, true)?;
            let label = scene.texts().t("adding-monitor");
            layer.upload(&gl, &panel.placeholder_image(label, PLACEHOLDER_PX, px_h))?;
            placeholder = Some((layer, a.size, scene.texts().clone()));
        }
        if scene.world.wrist().is_some()
            && let Some(pixels) = wrist_panel.render_if_dirty()
        {
            wrist_layer.upload(&gl, pixels)?;
        }
        if scene.panel_open {
            let ui_state = scene.ui_state(
                passthrough.is_some(),
                passthrough
                    .as_ref()
                    .is_some_and(passthrough::Passthrough::window_supported),
                exts.khr_composition_layer_equirect2,
                custom_pano,
                store.update_hint(),
            );
            panel.set_state(&ui_state);
            let drawing = test.is_some().then(Instant::now);
            if let Some(pixels) = panel.render_if_dirty() {
                menu_drawn = drawing.map(|t| t.elapsed());
                panel_layer.upload(&gl, pixels)?;
            }
        }

        // Hands and rays: only rendered when there is something to show.
        let mut spheres: Vec<Sphere> = Vec::new();
        // Light on the menu where a fingertip is about to touch: wide and faint from a
        // few centimetres away, small and bright right before contact.
        let place = scene.world.menu();
        for (h, hover) in poke_hover.iter().enumerate() {
            let Some((at, z)) = hover else { continue };
            let near = (1.0 - z.max(0.0) / POKE_HOVER).clamp(0.0, 1.0);
            let pressed = if pokes[h].pressed { 0.25 } else { 0.0 };
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
        rings.retain(|(_, at)| at.elapsed() < TOUCH_RING);
        for (center, at) in &rings {
            let k = at.elapsed().as_secs_f32() / TOUCH_RING.as_secs_f32();
            spheres.push(Sphere::ring(
                *center,
                0.012 + 0.03 * k,
                [0.8, 0.9, 1.0, 0.9 * (1.0 - k)],
            ));
        }
        // Where a ray points: a small light on the menu, a wider faint one on monitors.
        for pick in picks.iter().flatten() {
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
        if let Some((id, uv, at)) = scene.finder {
            let age = at.elapsed();
            match (scene.world.shown(id), age < FINDER_FOR) {
                (Some(p), true) => {
                    let c = scene
                        .world
                        .curve_radius(&scene.settings)
                        .filter(|_| !scene.world.is_window(id));
                    let (point, _) = layout::image_point(p, scene.world.height_of(id), c, uv);
                    let t = age.as_secs_f32() / FINDER_FOR.as_secs_f32();
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
                _ => scene.finder = None,
            }
        }
        // The magnet while dragging: the edge the monitor would stick to lights up.
        for g in scene.grabs.iter().flatten() {
            let Some(s) =
                g.id.display()
                    .and_then(|id| scene.world.stick_of(&scene.settings, id))
            else {
                continue;
            };
            let Some(n) = scene.world.shown(s.neighbour).copied() else {
                continue;
            };
            let (half_w, half_h) = (n.width / 2.0, scene.world.height_of(s.neighbour) / 2.0);
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
        // Hands are drawn over everything (the compositor has no depth for them), so
        // near a menu they would cover its text: the hand holding the palm menu is not
        // drawn, and a hand about to touch a menu is see-through.
        for (h, j) in joints.iter().enumerate() {
            let Some(j) = j else { continue };
            if h == menu_hand && scene.world.wrist().is_some() {
                continue;
            }
            let tip = j[INDEX_TIP].0;
            let near_palm = scene
                .world
                .wrist()
                .is_some_and(|w| w.position.distance(tip) < 0.15);
            let near_panel = poke_hover[h].is_some();
            let alpha = if near_palm || near_panel { 0.3 } else { 1.0 };
            overlay::hand_spheres(j, alpha, &mut spheres);
        }
        for (input, pick) in controls.state.iter().zip(&picks) {
            if let Some(ray) = input.ray {
                let length = pick.map_or(1.2, |p| p.hit.t).min(4.0);
                overlay::ray_spheres(ray.origin, ray.dir, length, &mut spheres);
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
        let views = if spheres.is_empty() {
            None
        } else {
            session
                .locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, time, &local)
                .ok()
                .map(|(_, v)| v)
        };
        if let Some(ov) = &mut overlay {
            ov.render(&gl, views.as_deref().unwrap_or(&[]), &mut spheres)?;
        }
        let projection_views = overlay
            .as_ref()
            .zip(views.as_ref())
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
        if let Some(pt) = passthrough.as_mut().filter(|pt| pt.window_supported()) {
            let place = scene
                .settings
                .keyboard_window
                .then(|| scene.world.keyboard_here((head, forward)));
            let shown = place.map(|p| (posef(&p), p.width, p.width * KEYBOARD_ASPECT));
            if let Err(e) = pt.set_window(&local, time, shown) {
                warn!("keyboard window: {e:#}");
                scene.settings.keyboard_window = false;
            }
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
            passthrough
                .as_ref()
                .and_then(passthrough::Passthrough::window_layer),
        );
        let passthrough_layer = passthrough_layer(
            passthrough
                .as_ref()
                .and_then(passthrough::Passthrough::room_layer),
        );
        let pano = pano_layer
            .as_ref()
            .filter(|(b, _)| *b == scene.settings.background)
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
        scene.world.animate(&scene.settings, dt);
        // Keep the desktop's arrangement in step once nothing is being moved.
        scene.world.settle(Instant::now());
        scene.world_done();
        let session = &scene.session;
        // Gone from view as soon as asked to go; back if the PC does not remove it. Far
        // to near, two at about one distance (a monitor glued over a row and that row)
        // in the order they had, so where they overlap never flickers.
        scene
            .world
            .order_drawing(head, |k| session.waits(ui::Asked::Remove(k)));
        // Focus mode: monitors without the mouse fade to a dim level (colour scale on
        // the compositor layer, no extra rendering).
        let focus = scene.focus();
        scene.world.brighten(&scene.settings, dt, focus);
        let curve = scene.world.curve_radius(&scene.settings);
        let order = scene.world.draw_list();
        let dims: Vec<xr::sys::CompositionLayerColorScaleBiasKHR> = order
            .iter()
            .map(|(_, id)| {
                // Fading in: premultiplied, so colour and alpha scale together.
                let t = scene.world.appear(*id);
                let t = t * t * (3.0 - 2.0 * t);
                // Focus dimming, the brightness setting and fading in, together.
                let b =
                    scene.world.brightness(*id) * t * f32::from(scene.settings.brightness) / 100.0;
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
            })
            .collect();
        let mut quads = Vec::new();
        let mut cylinders = Vec::new();
        for (i, (_, id)) in order.iter().enumerate() {
            let scale = dims[i].color_scale;
            let fading = exts.khr_composition_layer_color_scale_bias && scale.a < 1.0;
            let layer_flags = if fading {
                xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA
            } else {
                xr::CompositionLayerFlags::EMPTY
            };
            let next = if exts.khr_composition_layer_color_scale_bias && scale.r < 1.0 {
                (&raw const dims[i]).cast()
            } else {
                video_next
            };
            let (Some(m), Some(p)) = (scene.monitors.get(id), scene.world.shown(*id)) else {
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
            if let Some(radius) = curve.filter(|_| !scene.world.is_window(*id)) {
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

        let button = scene
            .world
            .button()
            .map(|b| button_layer.quad(&local, posef(&b), BUTTON_SIZE, BUTTON_SIZE));
        let panel_quad = (scene.panel_open && panel_layer.ready()).then(|| {
            panel_layer.quad(
                &local,
                posef(&scene.world.menu()),
                panel_width(scene.settings.large_menu),
                panel_height(scene.settings.large_menu),
            )
        });
        let mut cursors: Vec<xr::sys::CompositionLayerQuad> = picks
            .iter()
            .flatten()
            .map(|&Target { hit, .. }| {
                let size = 0.011 * hit.t.clamp(0.3, 4.0);
                let at = hit.point + hit.normal * 0.004;
                cursor_layer.quad(
                    &local,
                    pose(at, layout::facing(at, at + hit.normal)),
                    size,
                    size,
                )
            })
            .collect();
        // Under a fingertip near the menu: a dot that shrinks as the finger gets closer.
        for (at, z) in poke_hover.iter().flatten() {
            let place = &scene.world.menu();
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

        let mut layers: Vec<&xr::CompositionLayerBase<'_, Gfx>> = Vec::with_capacity(16);
        if let Some(l) = &passthrough_layer {
            layers.push(as_layer(l));
        } else if let Some(l) = &pano {
            layers.push(as_layer(l));
        }
        layers.extend(keyboard_layer.iter().map(as_layer));
        layers.extend(cylinders.iter().map(as_layer));
        layers.extend(quads.iter().map(as_layer));
        let placeholder_quad = scene.world.adding().and_then(|a| {
            let (layer, _, _) = placeholder.as_ref()?;
            Some(layer.quad(&local, posef(&a.place), a.place.width, a.height()))
        });
        layers.extend(placeholder_quad.iter().map(as_layer));
        layers.extend(button.iter().map(as_layer));
        layers.extend(panel_quad.iter().map(as_layer));
        let toast_quad = scene.toast.as_ref().and_then(|t| t.2).map(|p| {
            toast_layer.quad(
                &local,
                posef(&p),
                TOAST_WIDTH,
                TOAST_WIDTH * TOAST_PX.1 as f32 / TOAST_PX.0 as f32,
            )
        });
        layers.extend(toast_quad.iter().map(as_layer));
        let attach_quad = scene
            .attach_for
            .filter(|_| attach_drawn.is_some())
            .and_then(|(id, _)| scene.world.attach_place(id))
            .map(|p| attach_layer.quad(&local, posef(&p), ATTACH_WIDTH, attach_height()));
        layers.extend(attach_quad.iter().map(as_layer));
        let wrist_quad = scene
            .world
            .wrist()
            .filter(|_| wrist_layer.ready())
            .map(|w| wrist_layer.quad(&local, posef(&w), WRIST_MENU_WIDTH, wrist_menu_height()));
        layers.extend(wrist_quad.iter().map(as_layer));
        layers.extend(hands_layer.iter().map(as_layer));
        layers.extend(cursors.iter().map(as_layer));
        let layers_wanted = layers.len();
        // The compositor supports 16 layers at most.
        layers.truncate(16);
        match frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &layers) {
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
    // What waits is written, and the files' writer ends (see `Scene`'s `Drop`).
    drop(scene);
    Ok(())
}

/// A hand takes `thing`, pointed at with `hit`, where it is drawn: a monitor easing
/// to its place goes on from there, under the hand (see `World::hold`).
fn take(world: &mut World, thing: Thing, hit: Hit, s: &Settings) -> Option<Grab> {
    world
        .hold(thing, s)
        .map(|at| Grab::start(thing, hit.t, hit.local, &at))
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
