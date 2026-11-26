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
//!
//! The loop is [`run`]; its parts each have a module: the start (`start`), the
//! model's effects and the threads' news (`shell`), Android's and OpenXR's events
//! (`events`), the frame's input (`input`), the selftest's asks (`acts`), the
//! pictures (`pictures`), the hands and rays (`spheres`) and the layers (`compose`,
//! as the core's `layers::plan` orders them and keeps them within what the
//! compositor takes).

#![expect(
    clippy::disallowed_methods,
    reason = "the loop is the shell: it takes the time once a pass, starts the threads and looks for the person's files"
)]

mod acts;
mod compose;
mod events;
mod input;
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
mod no_selftest;
mod pictures;
mod shell;
mod spheres;
mod start;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use android_activity::AndroidApp;
use anyhow::Result;
use log::{error, info};
use openxr as xr;

use crate::interaction::{FrameInput, Hands};
use crate::lang::Moment;
use crate::platform::openxr::Gfx;
#[cfg(all(feature = "selftest", not(feature = "store")))]
use crate::selftest::device::{self as selftest, Act, Selftest};
use crate::view::View;
use events::Flow;
#[cfg(not(all(feature = "selftest", not(feature = "store"))))]
use no_selftest::{self as selftest, Act, Selftest};

/// What the pictures' layers are made and uploaded with.
#[derive(Clone, Copy)]
struct Gpu<'a> {
    session: &'a xr::Session<Gfx>,
    gl: &'a glow::Context,
    /// The colour format of the swapchains.
    format: u32,
}

/// What the loop keeps from pass to pass.
struct Kept {
    hands: Hands,
    view_cache: View,
    buffers: compose::Buffers,
    session_running: bool,
    /// The microphone's asks for access seen so far (see `Mic::asks`), and what was
    /// last said of its permission.
    mic_asks: u32,
    mic_seen: (u8, bool),
    /// When the screen was last read, while the app is put away.
    screen_read: Option<Instant>,
    /// The input is the app's: false while the system menu is open over it (the app
    /// keeps drawing, but shows no hands or rays of its own and takes no input).
    focused: bool,
    last_time: Option<xr::Time>,
    event_storage: xr::EventDataBuffer,
    /// The wall clock for the menu, and when it was read.
    wall: Option<Moment>,
    wall_read: Option<Instant>,
    /// What the selftest asked for, done in the next frame drawn.
    selftest_act: Act,
    allocations: crate::alloc_count::Tally,
}

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

/// The app; with `test`, the selftest's run of it (see `selftest::device`).
fn run(app: &AndroidApp, mut test: Option<&mut Selftest>) -> Result<()> {
    let (xr, frame_waiter, frame_stream) = start::Xr::start(app, test.as_deref_mut())?;
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
    let frames = (frame_waiter, frame_stream);
    let start::Started {
        mut stage,
        net_rx,
        vm,
        wifi_lock: _wifi_lock,
        mut store,
        mic_permission,
        mut shell,
    } = start::start(app, &xr, frames, test.as_deref_mut(), &data_dir, fake_pc)?;
    // The preview build's numbers of the input, from `feel.txt` where there is one.
    #[cfg(feature = "preview")]
    {
        let text = shell::read_feel(shell.external_dir.as_deref());
        let feel = crate::app::Event::Platform(crate::app::PlatformEvent::Feel(text));
        shell.handle(Instant::now(), feel);
    }
    let gpu = Gpu {
        session: &xr.session,
        gl: &xr.gl,
        format: stage.format,
    };
    let mut kept = Kept {
        hands: Hands::new(),
        view_cache: View::new(),
        buffers: compose::Buffers::default(),
        session_running: false,
        mic_asks: 0,
        mic_seen: (mic_permission, false),
        screen_read: None,
        focused: false,
        last_time: None,
        event_storage: xr::EventDataBuffer::new(),
        wall: None,
        wall_read: None,
        selftest_act: Act::Nothing,
        allocations: crate::alloc_count::Tally::default(),
    };

    'main: loop {
        if let Some(t) = test.as_deref_mut() {
            t.loop_top();
        }
        let polled = events::android(app, kept.session_running, &shell.mic);
        // The pass's time: the model's timers all count from it.
        let now = Instant::now();
        // What the last pass changed goes to the files' writer.
        shell.model.end_pass();
        shell.apply(now);
        if polled.destroy {
            break;
        }
        events::activity(&mut shell, now, &polled, &mut kept, &mut *store);
        if shell.exit {
            break;
        }
        if polled.input_waiting && events::hand_back_keys(app) {
            shell.system.volume_button();
        }
        if events::openxr(
            &xr.instance,
            &mut shell,
            &mut kept,
            test.as_deref_mut(),
            now,
        )? == Flow::Exit
        {
            break 'main;
        }
        events::presence_and_mic(&mut shell, now, &mut kept);
        shell.news(now, &net_rx, &vm, test.as_deref_mut());
        let wanted = shell.model.settings().background;
        let equirect = xr.exts.khr_composition_layer_equirect2;
        let external_dir = shell.external_dir.as_deref();
        stage.pictures.panorama(gpu, wanted, equirect, external_dir);

        if let Some(t) = test.as_deref_mut() {
            match t.step() {
                Act::Finish => break 'main,
                Act::Nothing => {}
                act => kept.selftest_act = act,
            }
        }
        input::wall_clock(now, &mut kept);

        // The model's time goes on with or without frames (the session not running,
        // the headset off the head, nothing to render): requests time out, a
        // placeholder goes, and the desktop's arrangement is told once it stood.
        if !kept.session_running {
            shell.model.tick(now, &FrameInput::hidden(kept.wall));
            shell.apply(now);
            continue;
        }

        let state = stage.waiter.wait()?;
        let frame_start = test.as_deref().map(Selftest::frame_start);
        stage.stream.begin()?;
        let time = state.predicted_display_time;
        if !state.should_render {
            shell.model.tick(now, &FrameInput::hidden(kept.wall));
            shell.apply(now);
            stage.stream.end(time, shell.room.blend_mode(), &[])?;
            continue;
        }
        let dt = kept
            .last_time
            .map_or(0.0f32, |t| (time.as_nanos() - t.as_nanos()) as f32 * 1e-9)
            .clamp(0.0, 0.1);
        kept.last_time = Some(time);

        // This frame's input.
        let input = input::read(&xr, &stage, &mut shell, &mut kept, now, time, dt)?;
        shell.model.tick(now, &input);
        shell.apply(now);

        // What the selftest asks for, as the menu would do it.
        acts::act(&mut shell, now, &mut kept.selftest_act, test.as_deref_mut());

        // The palm menu, the menu button and "Line up"; then the palm menu's picture is
        // pointed at with the model as it is now.
        let panels = &mut stage.pictures.panels;
        kept.hands.buttons(now, &input, &mut shell.model);
        shell.apply(now);
        let rev = Some(shell.model.rev());
        if shell.model.world().wrist().is_some() && rev != panels.palm_rev {
            panels.palm.set_state(kept.view_cache.state(&shell.model));
            panels.palm_rev = rev;
        }
        // Pointing, touching, clicking and dragging.
        kept.hands.step(now, &input, &mut shell.model, panels);
        shell.apply(now);
        stage.pictures.button(&xr.gl, kept.hands.over_button())?;
        shell.model.animate(now, &input);
        shell.apply(now);

        // Menu contents, how long the menu took to draw (for the selftest).
        let timed = test.is_some();
        let menu_drawn =
            stage
                .pictures
                .draw(gpu, &shell.model, &kept.hands, &mut kept.view_cache, timed)?;
        // The numbers of "For development" (the preview build, while chosen).
        #[cfg(feature = "preview")]
        stage
            .pictures
            .metrics(&xr.gl, &shell.model, &shell.video, now)?;
        // Hands and rays: only rendered when there is something to show.
        spheres::fill(
            &mut kept.buffers.spheres,
            &mut shell.model,
            &kept.hands,
            &input,
            now,
        );
        let layers_wanted = compose::end(&xr, &mut stage, &mut shell, &mut kept, now, time)?;
        if let (Some(t), Some(start)) = (test.as_deref_mut(), &frame_start) {
            t.frame_end(
                start,
                time.as_nanos(),
                Duration::from_nanos(state.predicted_display_period.as_nanos().max(0) as u64),
                menu_drawn,
                layers_wanted,
            );
        }
        kept.allocations.frame();
    }

    info!("exiting");
    // What waits is written, and the files' writer ends (see `Shell`'s `Drop`).
    drop(shell);
    Ok(())
}
