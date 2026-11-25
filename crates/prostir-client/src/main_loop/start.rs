//! The start of a run: OpenXR's instance and session, then what the loop works with:
//! the files read once, the room, the spaces, the pictures' layers, the hands, the
//! texts, what the headset can do, the threads, and the model.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use android_activity::AndroidApp;
use anyhow::{Result, anyhow, bail};
use jni::objects::JObject;
use log::{info, warn};
use openxr as xr;

use super::pictures::{Pictures, custom_panorama_path};
use super::shell::Shell;
use super::{Gpu, Selftest};
use crate::app::{Model, Start, headset_state};
use crate::net::NetEvent;
use crate::platform::android::system::System;
use crate::platform::meta::{self, passthrough};
use crate::platform::openxr::input::Controls;
use crate::platform::openxr::overlay::Overlay;
use crate::platform::openxr::{self as xrutil, Gfx, egl};
use crate::platform::room::Room;
use crate::platform::sound::audio::Gains;
use crate::platform::sound::mic::Mic;
use crate::platform::store::Store;
use crate::platform::video::SurfaceVideo;
use crate::platform::{self, android};
use crate::settings::HandMode;
use crate::{net, persist};

/// OpenXR as this run has it: the instance and its session, the extensions turned
/// on, and the GL the pictures are uploaded with.
pub struct Xr {
    /// The loader, kept for as long as the instance.
    pub _entry: xr::Entry,
    pub exts: xr::ExtensionSet,
    pub instance: xr::Instance,
    pub system: xr::SystemId,
    /// The EGL context the session draws with, kept for the whole session.
    pub _egl: egl::EglContext,
    pub session: xr::Session<Gfx>,
    pub gl: glow::Context,
    /// The headset as the system names it.
    pub device: String,
}

impl Xr {
    /// The instance with the extensions the app uses, the session, and the frames'
    /// waiter and stream.
    pub fn start(
        app: &AndroidApp,
        test: Option<&mut Selftest>,
    ) -> Result<(Self, xr::FrameWaiter, xr::FrameStream<Gfx>)> {
        // SAFETY: pointers come straight from the running NativeActivity.
        let platform =
            unsafe { xr::AndroidPlatformInfo::new(app.vm_as_ptr(), app.activity_as_ptr()) };
        // SAFETY: libopenxr_loader.so is the Khronos loader bundled in the APK.
        let entry = unsafe { xr::Entry::load(&platform) }
            .map_err(|e| anyhow!("cannot load OpenXR loader: {e}"))?;
        let available = entry.enumerate_extensions()?;
        if !available.khr_android_surface_swapchain {
            bail!("the runtime lacks XR_KHR_android_surface_swapchain");
        }
        // `caps=minimal` (the selftest's, or the intent's own extra for a check by
        // hand): the instance with only the extensions the app cannot do without, so
        // the app runs as on a headset without the others.
        let minimal = test.as_deref().is_some_and(Selftest::caps_minimal)
            || android::intent_extra(app, "caps").as_deref() == Some("minimal");
        if minimal {
            info!("caps=minimal: no optional extension is turned on");
        }
        let exts = xrutil::extensions(&available, minimal);
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
        let device = android::model();
        info!("headset: {device:?}");
        if let Some(t) = test {
            t.runtime(
                &instance,
                &available,
                minimal,
                exts.fb_composition_layer_image_layout,
            );
        }

        let egl = egl::EglContext::new()?;
        // SAFETY: the EGL handles stay alive (owned by `egl`) for the whole session.
        let (session, frame_waiter, frame_stream) = unsafe {
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
        let xr = Self {
            _entry: entry,
            exts,
            instance,
            system,
            _egl: egl,
            session,
            gl,
            device,
        };
        Ok((xr, frame_waiter, frame_stream))
    }
}

/// What the frames are drawn with: the spaces, the pictures, the hands' trackers and
/// their overlay.
pub struct Stage {
    /// Waits for the runtime's time to draw the next frame.
    pub waiter: xr::FrameWaiter,
    /// Begins and ends the frames.
    pub stream: xr::FrameStream<Gfx>,
    pub local: xr::Space,
    pub view: xr::Space,
    /// The colour format of the pictures' swapchains.
    pub format: u32,
    pub pictures: Pictures,
    /// Tracked hands and pointer rays, drawn on top of everything.
    pub hand_trackers: Option<[xr::HandTracker; 2]>,
    pub overlay: Option<Overlay>,
}

/// What the loop works with, made at the start.
pub struct Started<'s> {
    pub stage: Stage,
    pub net_rx: mpsc::Receiver<NetEvent>,
    pub vm: jni::JavaVM,
    /// Keeps Wi-Fi out of power save while it is held.
    pub wifi_lock: Option<jni::refs::Global<JObject<'static>>>,
    /// Where this build comes from, and its check that the app is the person's.
    pub store: Box<dyn Store>,
    /// Whether the system lets the app record, at the start.
    pub mic_permission: u8,
    pub shell: Shell<'s>,
}

/// Everything else of a run, in `data_dir` (`fake_pc`: the selftest's only PC), with
/// the session's `frames` (its waiter and stream).
pub fn start<'s>(
    app: &AndroidApp,
    xr: &'s Xr,
    frames: (xr::FrameWaiter, xr::FrameStream<Gfx>),
    mut test: Option<&mut Selftest>,
    data_dir: &Path,
    fake_pc: Option<SocketAddr>,
) -> Result<Started<'s>> {
    let Xr {
        exts,
        instance,
        system,
        session,
        gl,
        device,
        ..
    } = xr;
    let (system, model) = (*system, device);
    // A write cut short leaves its temporary file; the real one is whole.
    let persisted = persist::File::ALL.map(persist::File::name);
    for name in persisted.into_iter().chain(["identity", "paired_hosts"]) {
        prostir_proto::secure::remove_leftovers(&data_dir.join(name));
    }
    // The files are read here once; from now on the model in memory is what counts,
    // and the files' writer only writes them, off the frame. However this function
    // ends, what waits is written and the thread waited for (see `persist::Saver`,
    // and `Shell`'s `Drop` once the shell has it).
    let (loaded, saver) = persist::start(data_dir);
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

    // The room behind the monitors, by the first way the runtime offers that can be
    // made (see `caps::room_ways`). The cameras keep busy and cost battery: on only
    // while a "Room" background is chosen.
    let blend_modes: Vec<crate::caps::BlendMode> = instance
        .enumerate_environment_blend_modes(system, xr::ViewConfigurationType::PRIMARY_STEREO)
        .inspect_err(|e| warn!("blend modes: {e}"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| match m {
            xr::EnvironmentBlendMode::OPAQUE => Some(crate::caps::BlendMode::Opaque),
            xr::EnvironmentBlendMode::ADDITIVE => Some(crate::caps::BlendMode::Additive),
            xr::EnvironmentBlendMode::ALPHA_BLEND => Some(crate::caps::BlendMode::AlphaBlend),
            _ => None,
        })
        .collect();
    info!("blend modes: {blend_modes:?}");
    let room_ways = crate::caps::room_ways(&crate::caps::RoomFound {
        blend_modes: &blend_modes,
        fb_passthrough: exts.fb_passthrough,
        htc_passthrough: exts.htc_passthrough,
    });
    // A frame without the cameras' room blended in: opaque where the runtime offers it.
    let plain = match crate::caps::plain_blend(&blend_modes) {
        crate::caps::BlendMode::Opaque => xr::EnvironmentBlendMode::OPAQUE,
        crate::caps::BlendMode::Additive => xr::EnvironmentBlendMode::ADDITIVE,
        crate::caps::BlendMode::AlphaBlend => xr::EnvironmentBlendMode::ALPHA_BLEND,
    };
    let room = Room::make(
        &room_ways,
        plain,
        session,
        settings.background.is_room(),
        settings.background.room_opacity(),
    );
    let room_caps = room.way().map(|way| crate::caps::RoomCaps {
        way,
        keyboard_window: room
            .fb_ref()
            .is_some_and(passthrough::Passthrough::window_supported),
    });

    let local =
        session.create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::Posef::IDENTITY)?;
    let view = session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?;
    let mut controls = Controls::new(instance, session, exts)?;
    controls.hands_enabled = settings.hands == HandMode::Control;

    let format = xrutil::color_format(session);
    let pictures = Pictures::new(Gpu {
        session,
        gl,
        format,
    })?;
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
    let overlay = Overlay::new(gl, instance, system, session, format)
        .inspect_err(|e| warn!("hand/ray overlay unavailable: {e:#}"))
        .ok();
    // The selftest reads nothing of the person's there (`host.txt`, a panorama).
    let external_dir = app.external_data_path().filter(|_| test.is_none());
    let custom_pano = custom_panorama_path(external_dir.as_deref()).is_some();

    // The menu's language, before the first frame: the headset's own language once,
    // the files there are, and the language to show. From here on the system thread
    // reads the files.
    let system_lang = android::language();
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
    let battery = android::battery();
    let system_thread = System::start(lang_loader);

    // What this headset can do, asked once: the extensions, the runtime, the cameras
    // and hands made above, the system's decoders and volume. The core decides by
    // nothing else.
    let caps = platform::caps(
        exts,
        instance,
        system,
        session,
        room_caps,
        hand_trackers.is_some(),
        model.clone(),
    );
    if let Some(t) = test.as_deref_mut() {
        t.caps(&caps);
        t.set_perf(session);
    }

    let (net_tx, net_rx) = mpsc::channel();
    // A selftest connects to its fake PC and nothing else, never to a real PC.
    debug_assert!(test.is_none() || fake_pc.is_some());
    // However this function ends, the network thread and its sessions end with it, and
    // the thread is waited for (up to a second, see `Net`'s `Drop`): a new start in the
    // same process must not find this one's still connected to the PC.
    let network = net::Net::start(
        net::Config::in_dir(data_dir, external_dir.as_deref(), model),
        net_tx,
        Arc::new(platform::sound::Sound {
            gains: gains.clone(),
            mic: mic.clone(),
        }),
        // What the first `Open` says, before the core says more.
        headset_state(&settings, true, battery, mic.permission(), &caps),
    )?;

    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let wifi_lock = android::acquire_low_latency_wifi(app, &vm)
        .inspect(|_| info!("Wi-Fi low-latency mode on"))
        .inspect_err(|e| warn!("Wi-Fi low-latency lock unavailable: {e:#}"))
        .ok();

    // Where this build comes from. A store build checks the user owns the app, as its
    // store requires at launch; a build from GitHub has nothing to check.
    let store = platform::store::of_this_build(app);
    let mic_permission = mic.permission();
    let shell = Shell {
        model: Model::new(Start {
            now: Instant::now(),
            settings,
            desk,
            keyboard: keyboard_place,
            profiles: saved_profiles,
            lang,
            system_lang,
            battery,
            caps,
            refresh_hz: meta::display_rate(session),
            custom_panorama: custom_pano,
            update_from: store.update_hint(),
            mic_permission,
            only: fake_pc,
        }),
        session,
        video: SurfaceVideo::new(),
        video_events: Vec::with_capacity(8),
        outboxes: BTreeMap::new(),
        saver,
        network,
        system: system_thread,
        mic,
        gains,
        controls,
        room,
        external_dir,
        fx: Vec::new(),
        later: Vec::new(),
        exit: false,
    };
    Ok(Started {
        stage: Stage {
            waiter: frames.0,
            stream: frames.1,
            local,
            view,
            format,
            pictures,
            hand_trackers,
            overlay,
        },
        net_rx,
        vm,
        wifi_lock,
        store,
        mic_permission,
        shell,
    })
}
