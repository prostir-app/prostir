//! The selftest on the headset: started by the intent extra `selftest`
//! (`am start ... --es selftest "seconds=30"`), it runs the app against a fake PC
//! (`prostir_testkit::pc::FakePc`) in this process, on `127.0.0.1` at a port of its
//! own, with its own data (`files/selftest/`), and writes its report to the app's
//! external files (`selftest.txt`) and to the log (tag `prostir-selftest`).
//!
//! The app does what it always does; the selftest only watches (events, frame
//! times) and, at set moments, asks for what a person would do in the menu
//! ([`Act`]), while the fake PC breaks things on purpose.

#![expect(
    clippy::disallowed_methods,
    reason = "the selftest is part of the shell: it measures time, starts its fake PC and writes its report"
)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use android_activity::AndroidApp;
use jni::objects::{JObject, JString, JValue};
use jni::{Env, jni_sig, jni_str};
use log::{info, warn};
use openxr as xr;
use prostir_proto::state::{DisplayId, Gen, HeadsetCaps};
use prostir_testkit::pc::{Clip, Event, FakePc, Happened, PcConfig, trust_each_other};

use super::{Exit, Measured, Options, Probe, Report, Samples, check, is_app_thread, lock};
use crate::net::NetEvent;

/// How many times `android_main` started in this process (the first is 1).
static RUNS: AtomicU32 = AtomicU32::new(0);

/// Counts a start of the app in this process; which one it is.
pub fn count_run() -> u32 {
    RUNS.fetch_add(1, Ordering::Relaxed) + 1
}

/// The OpenXR session must be focused (worn, the app in front) this soon.
const FOCUS_WITHIN: Duration = Duration::from_secs(10);
/// Every stream must show its first picture this soon after the fake PC started.
const PICTURES_WITHIN: Duration = Duration::from_secs(15);
/// After it starts streaming, before measuring.
const SETTLE: Duration = Duration::from_secs(2);
/// How long a monitor's move, the menu and each breakage get.
const MOVE_FOR: Duration = Duration::from_secs(2);
const MENU_FOR: Duration = Duration::from_millis(1500);
const RECOVER_WITHIN: Duration = Duration::from_secs(5);
const RECONNECT_WITHIN: Duration = Duration::from_secs(10);
/// Beyond the calm part, the scenario takes about 20 s; past this it timed out.
const EXTRA: Duration = Duration::from_secs(60);
/// Frames kept for percentiles: two minutes at 120 Hz (a longer run keeps its first
/// two minutes of frames; the counts go on).
const FRAMES_KEPT: usize = 120 * 120;
/// `Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY`: started again from the recent apps,
/// with the intent of the last start, which must not run a selftest again.
const FROM_HISTORY: i32 = 0x0010_0000;
/// The monitors the fake PC streams (it picks clips for each).
const MONITORS: usize = 2;

/// The report's keys, in their order: a report that ends early has them all.
const KEYS: &[&str] = &[
    "selftest",
    "app",
    "pid",
    "run",
    "seconds",
    "manufacturer",
    "model",
    "runtime",
    "extensions",
    "caps",
    "decoders",
    "streams_max",
    "layers_max",
    "volume_steps",
    "refresh_rates",
    "refresh_hz",
    "period_ms",
    "perf",
    "threads_left",
    "battery.start",
    "connect_ms",
    "snapshot_ms",
    "stream.1.clip",
    "stream.1.first_frame_ms",
    "stream.1.frames",
    "stream.1.sent",
    "stream.2.clip",
    "stream.2.first_frame_ms",
    "stream.2.frames",
    "stream.2.sent",
    "decode_ms.p50",
    "decode_ms.p95",
    "decode_ms.p99",
    "decode_ms.max",
    "net_to_decoder_ms.p50",
    "net_to_decoder_ms.p95",
    "net_to_decoder_ms.p99",
    "net_to_decoder_ms.max",
    "decoder_queue_max",
    "frames_dropped_to_keyframe",
    "decoder_wakeups_per_s",
    "keyframe_asked_ms",
    "decoder_recovered_ms",
    "surface_recreated_ms",
    "stream_restart_ms",
    "reconnect_ms",
    "reconnect_picture_ms",
    "frame_samples",
    "frame_cpu_ms.p50",
    "frame_cpu_ms.p95",
    "frame_cpu_ms.p99",
    "frame_cpu_ms.max",
    "frame_wall_ms.p50",
    "frame_wall_ms.p95",
    "frame_wall_ms.p99",
    "frame_wall_ms.max",
    "frame_cpu_share.p95",
    "frame_cpu_share.p99",
    "menu_redraws",
    "menu_redraw_ms.p95",
    "menu_redraw_ms.max",
    "menu_redraw_share.p95",
    "menu_redraw_share.max",
    "image_uploads",
    "image_upload_ms.p95",
    "image_upload_ms.max",
    "frames",
    "frames_missed",
    "frames_missed_moving",
    "focus_lost",
    "layers.max",
    "persist_ms.max",
    "alloc_per_frame",
    "alloc_per_frame.max",
    "alloc_all_threads_per_s",
    "battery.end",
    "fail",
    "result",
    "exit",
];

/// What the selftest asks the app to do, as a person would in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Nothing,
    /// Swap the first monitor with the next one (the menu's move right), which saves
    /// the arrangement.
    MoveMonitor,
    OpenMenu,
    /// Another tab of the menu (saved in the settings, and redrawn).
    SwitchTab,
    CloseMenu,
    /// Done: leave the main loop.
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Until the session is focused and every stream shows a picture.
    Waiting,
    Settle,
    /// Calm: frame times, allocations, missed frames.
    Steady,
    Move,
    Menu,
    /// A frame its decoder cannot take, then waiting for the picture to come back.
    Broken,
    /// A stream started over in a new generation.
    Restart,
    /// The fake PC hangs up; waiting for the headset to come back.
    Dropped,
    Done,
}

/// One stream as the fake PC started it.
struct Seen {
    generation: Gen,
    started: Instant,
    /// Its first picture out of the decoder.
    shown: Option<Instant>,
    /// The fake PC's session it belongs to.
    session: u32,
}

/// Where one frame's measuring began (see [`Selftest::frame_start`]).
pub struct FrameStart {
    cpu: Duration,
    wall: Instant,
}

pub struct Selftest {
    options: Options,
    report: Report,
    begun: Instant,
    /// Set when it has to end before its scenario is over.
    exit: Option<Exit>,
    fails: Vec<String>,
    pc: Option<FakePc>,
    probe: Option<Arc<Mutex<Probe>>>,
    phase: Phase,
    phase_since: Instant,
    focused: bool,
    focused_at: Option<Instant>,
    focus_lost: u32,
    connected_at: Option<Instant>,
    displays_at: Option<Instant>,
    /// The fake PC's sessions so far, and the monitors of the last snapshot.
    sessions: u32,
    monitors: Vec<DisplayId>,
    streams: BTreeMap<DisplayId, Seen>,
    first_frames: BTreeMap<DisplayId, f64>,
    /// The broken frame: its stream, number and when it went out.
    broken: Option<(DisplayId, Gen, u32, Instant)>,
    keyframe_asked_ms: Option<f64>,
    recovered_ms: Option<f64>,
    restart: Option<DisplayId>,
    restart_ms: Option<f64>,
    dropped_at: Option<Instant>,
    reconnect_ms: Option<f64>,
    reconnect_picture_ms: Option<f64>,
    /// The display period of the frame before, and of the first calm one.
    period: Option<Duration>,
    steady_period: Option<Duration>,
    last_display_ns: Option<i64>,
    frame_cpu: Samples,
    frame_wall: Samples,
    frame_share: Samples,
    menu_ms: Samples,
    menu_share: Samples,
    allocs: Samples,
    persist_ms: Samples,
    missed_steady: u32,
    missed_moving: u32,
    frames: u64,
    layers_max: usize,
    /// The main thread's allocations at the top of this pass of the main loop, and
    /// whether the pass before drew a calm frame (its allocations then count).
    allocs_at_top: Option<u64>,
    calm_frame: bool,
    /// All threads' allocations and when, at the start of the calm part.
    allocs_all: Option<(u64, Instant)>,
    allocs_all_per_s: Option<f64>,
    /// The decoder threads' context switches (by thread) and when, at the start of the
    /// calm part, and all of them together a second over it.
    switches: Option<(BTreeMap<u32, u64>, Instant)>,
    decoder_wakeups_per_s: Option<f64>,
    /// The menu's tab was switched (once, half way through its time).
    tab_switched: bool,
}

/// Runs `f` with the running activity (the Activity object of the NativeActivity,
/// not the Application `ndk_context` holds), in a local frame of its own.
fn with_activity<T>(
    app: &AndroidApp,
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Option<T> {
    // SAFETY: the VM of the running NativeActivity.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    // A Java exception on the way is caught and cleared, and comes back as the error.
    vm.attach_current_thread(|env| {
        let raw: jni::sys::jobject = app.activity_as_ptr().cast();
        // SAFETY: the activity's object, valid while the activity runs (this start);
        // the cast only borrows it, it never deletes it.
        let activity = unsafe { env.as_cast_raw::<JObject<'static>>(&raw) }?;
        f(env, &activity)
    })
    .inspect_err(|e| warn!("selftest: a call into Android failed: {e}"))
    .ok()
}

/// The `selftest` extra of the intent that started the activity, if there is one
/// (not when it was started again from the recent apps with an old intent).
fn intent_extra(app: &AndroidApp) -> Option<String> {
    with_activity(app, |env, activity| {
        let intent = env
            .call_method(
                activity,
                jni_str!("getIntent"),
                jni_sig!("()Landroid/content/Intent;"),
                &[],
            )?
            .l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let flags = env
            .call_method(&intent, jni_str!("getFlags"), jni_sig!("()I"), &[])?
            .i()?;
        if flags & FROM_HISTORY != 0 {
            return Ok(None);
        }
        let key = env.new_string("selftest")?;
        let value = env
            .call_method(
                &intent,
                jni_str!("getStringExtra"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                &[JValue::Object(&key)],
            )?
            .l()?;
        if value.is_null() {
            return Ok(None);
        }
        let value = env.cast_local::<JString<'_>>(value)?;
        Ok(Some(value.try_to_string(env)?))
    })
    .flatten()
}

/// `AudioManager.getStreamMaxVolume(STREAM_MUSIC)`: steps above silence.
fn volume_steps(app: &AndroidApp) -> Option<i32> {
    with_activity(app, |env, activity| {
        let name = env.new_string("audio")?;
        let manager = env
            .call_method(
                activity,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        if manager.is_null() {
            return Ok(None);
        }
        let steps = env
            .call_method(
                &manager,
                jni_str!("getStreamMaxVolume"),
                jni_sig!("(I)I"),
                &[JValue::Int(3)],
            )?
            .i()?;
        Ok(Some(steps))
    })
    .flatten()
}

/// A system property (`ro.product.model`), empty when there is none.
fn property(name: &CStr) -> String {
    let mut value = [0u8; 92]; // PROP_VALUE_MAX
    // SAFETY: a nul-terminated name, and a buffer of PROP_VALUE_MAX bytes, which is
    // what the call writes at most (with its nul).
    let n = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr().cast()) };
    let n = usize::try_from(n).unwrap_or(0).min(value.len());
    String::from_utf8_lossy(&value[..n]).into_owned()
}

/// A line to the log with the tag `prostir-selftest`.
fn log_line(text: &str) {
    let (Ok(tag), Ok(text)) = (CString::new("prostir-selftest"), CString::new(text)) else {
        return;
    };
    // SAFETY: both strings are nul-terminated and live through the call.
    unsafe {
        ndk_sys::__android_log_write(
            ndk_sys::android_LogPriority::ANDROID_LOG_INFO.0 as i32,
            tag.as_ptr(),
            text.as_ptr(),
        );
    }
}

/// The calling thread's CPU time.
fn thread_cpu() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes into the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// Threads of this app still alive from a start before this one.
fn threads_left() -> usize {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    tasks
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok())
        .filter(|name| is_app_thread(name.trim()))
        .count()
}

/// Context switches of each decoder thread so far (voluntary and not), by thread:
/// each one is the thread waking up.
fn decoder_switches() -> BTreeMap<u32, u64> {
    let mut found = BTreeMap::new();
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return found;
    };
    for task in tasks.flatten() {
        let path = task.path();
        let is_decoder = std::fs::read_to_string(path.join("comm"))
            .is_ok_and(|name| name.trim().starts_with("decoder-"));
        let tid = task
            .file_name()
            .to_str()
            .and_then(|t| t.parse::<u32>().ok());
        let (true, Some(tid)) = (is_decoder, tid) else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue;
        };
        let switches = status
            .lines()
            .filter(|l| l.contains("ctxt_switches:"))
            .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            .sum();
        found.insert(tid, switches);
    }
    found
}

/// The clips in the APK's `assets/selftest/` (every build but the store's has them).
fn clips(app: &AndroidApp) -> Vec<Clip> {
    let assets = app.asset_manager();
    let Some(dir) = assets.open_dir(c"selftest") else {
        return Vec::new();
    };
    let mut clips = Vec::new();
    for name in dir {
        let Ok(text) = name.to_str() else { continue };
        let Ok(path) = CString::new(format!("selftest/{text}")) else {
            continue;
        };
        let Some(mut asset) = assets.open(&path) else {
            continue;
        };
        let data = match asset.buffer() {
            Ok(data) => data.to_vec(),
            Err(e) => {
                warn!("selftest clip {text}: {e}");
                continue;
            }
        };
        match Clip::named(text, data) {
            Ok(clip) => clips.push(clip),
            Err(e) => warn!("selftest clip {text}: {e:#}"),
        }
    }
    // The biggest first: the fake PC picks the first ones the headset decodes.
    clips.sort_by_key(|c| {
        std::cmp::Reverse((c.width * c.height, c.codec == prostir_proto::Codec::H265))
    });
    clips
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn battery() -> String {
    crate::system::battery().map_or_else(|| "none".into(), |(p, _)| p.to_string())
}

impl Selftest {
    /// A selftest, if the intent that started the app asks for one; `run` is which
    /// start in this process this is.
    pub fn requested(app: &AndroidApp, run: u32) -> Option<Self> {
        let extra = intent_extra(app)?;
        let now = Instant::now();
        let mut test = Self {
            options: Options::default(),
            report: Report::default(),
            begun: now,
            exit: None,
            fails: Vec::new(),
            pc: None,
            probe: None,
            phase: Phase::Waiting,
            phase_since: now,
            focused: false,
            focused_at: None,
            focus_lost: 0,
            connected_at: None,
            displays_at: None,
            sessions: 0,
            monitors: Vec::new(),
            streams: BTreeMap::new(),
            first_frames: BTreeMap::new(),
            broken: None,
            keyframe_asked_ms: None,
            recovered_ms: None,
            restart: None,
            restart_ms: None,
            dropped_at: None,
            reconnect_ms: None,
            reconnect_picture_ms: None,
            period: None,
            steady_period: None,
            last_display_ns: None,
            frame_cpu: Samples::with_capacity(FRAMES_KEPT),
            frame_wall: Samples::with_capacity(FRAMES_KEPT),
            frame_share: Samples::with_capacity(FRAMES_KEPT),
            menu_ms: Samples::with_capacity(1024),
            menu_share: Samples::with_capacity(1024),
            allocs: Samples::with_capacity(FRAMES_KEPT),
            persist_ms: Samples::with_capacity(64),
            missed_steady: 0,
            missed_moving: 0,
            frames: 0,
            layers_max: 0,
            allocs_at_top: None,
            calm_frame: false,
            allocs_all: None,
            allocs_all_per_s: None,
            switches: None,
            decoder_wakeups_per_s: None,
            tab_switched: false,
        };
        let r = &mut test.report;
        r.lay_out(KEYS);
        r.set("selftest", 1);
        r.set("app", prostir_proto::app_version(env!("CARGO_PKG_VERSION")));
        r.set("pid", std::process::id());
        r.set("run", run);
        r.set("manufacturer", property(c"ro.product.manufacturer"));
        r.set("model", property(c"ro.product.model"));
        r.set("perf", "default");
        r.set("threads_left", threads_left());
        r.set("battery.start", battery());
        match Options::parse(&extra) {
            Ok(options) => test.options = options,
            Err(e) => test.end_early(Exit::NoStart, &e),
        }
        test.report.set("seconds", test.options.seconds);
        info!("selftest: {extra:?}, start {run} in this process");
        Some(test)
    }

    fn end_early(&mut self, exit: Exit, why: &str) {
        warn!("selftest: {why}");
        self.fails.push(why.to_owned());
        self.exit = Some(self.exit.map_or(exit, |e| e.max(exit)));
        self.phase = Phase::Done;
    }

    /// Whether the scenario is over (the app then leaves its main loop).
    pub fn over(&self) -> bool {
        self.phase == Phase::Done
    }

    /// The selftest's own data directory in `internal` (`selftest/`), emptied: what
    /// the app keeps there (keys, settings, the arrangement) never touches the
    /// person's own. Settings start with the first-run tips read, as after any first
    /// run, so the menu opens only when the scenario opens it.
    pub fn data_dir(&mut self, internal: &Path) -> PathBuf {
        let dir = internal.join("selftest");
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.end_early(
                Exit::NoStart,
                &format!("cannot make {}: {e}", dir.display()),
            );
        }
        let settings = crate::settings::Settings {
            intro_seen: true,
            last_tab: 0,
            ..crate::settings::Settings::default()
        };
        let file = crate::persist::File::Settings;
        if let Err(e) = crate::persist::write_now(&dir, file, &settings.to_text()) {
            warn!("cannot save {} in {}: {e}", file.name(), dir.display());
        }
        dir
    }

    /// Starts the fake PC with the clips of the APK, trusted by the headset's keys in
    /// `dir`: its address, which the app connects to and nothing else.
    pub fn start_pc(&mut self, app: &AndroidApp, dir: &Path) -> Option<std::net::SocketAddr> {
        if self.over() {
            return None;
        }
        let clips = clips(app);
        if clips.is_empty() {
            self.end_early(Exit::NoStart, "no clips in assets/selftest");
            return None;
        }
        let pc_key = dir.join("fake-pc-identity");
        let headset = match trust_each_other(dir, &pc_key, "Prostir selftest") {
            Ok((headset, _)) => headset,
            Err(e) => {
                self.end_early(Exit::NoStart, &format!("keys: {e:#}"));
                return None;
            }
        };
        let pc = FakePc::start(PcConfig {
            identity: pc_key,
            trusted: vec![headset],
            clips,
            monitors: MONITORS,
            name: "Prostir selftest".into(),
        });
        match pc {
            Ok(pc) => {
                let addr = pc.addr();
                info!("selftest: fake PC on {addr}");
                self.pc = Some(pc);
                self.probe = Some(super::probe_start());
                self.begun = Instant::now();
                Some(addr)
            }
            Err(e) => {
                self.end_early(Exit::NoStart, &format!("fake PC: {e:#}"));
                None
            }
        }
    }

    /// What the OpenXR runtime is and offers (every extension it lists, not only the
    /// ones the app turns on), and the system's volume steps.
    pub fn runtime(
        &mut self,
        app: &AndroidApp,
        instance: &xr::Instance,
        system: xr::SystemId,
        available: &xr::ExtensionSet,
    ) {
        if let Ok(p) = instance.properties() {
            self.report.set(
                "runtime",
                format!("{} {}", p.runtime_name, p.runtime_version),
            );
        }
        let names: Vec<String> = available
            .names()
            .iter()
            .map(|n| String::from_utf8_lossy(n.strip_suffix(&[0]).unwrap_or(n)).into_owned())
            .collect();
        self.report.set("extensions", names.join(","));
        // The app turns on every optional extension the runtime has.
        self.report.set("caps", "full");
        if let Ok(p) = instance.system_properties(system) {
            self.report
                .set("layers_max", p.graphics_properties.max_layer_count);
        }
        if let Some(steps) = volume_steps(app) {
            self.report.set("volume_steps", steps);
        }
    }

    /// What the app says it decodes (fixed in this build).
    pub fn headset_caps(&mut self, caps: &HeadsetCaps) {
        let decoders: Vec<String> = caps
            .decoders
            .iter()
            .map(|d| {
                let codec = match d.codec {
                    0 => "h264",
                    1 => "h265",
                    _ => "other",
                };
                format!("{codec}:{}x{}", d.max_w, d.max_h)
            })
            .collect();
        self.report.set("decoders", decoders.join(","));
        self.report.set("streams_max", caps.streams_max);
    }

    /// The display's refresh rates, once the session runs.
    pub fn refresh(&mut self, offered: Option<Vec<f32>>, now: Option<f32>) {
        if let Some(rates) = offered {
            let rates: Vec<String> = rates.iter().map(|r| format!("{r:.0}")).collect();
            self.report.set("refresh_rates", rates.join(","));
        }
        if let Some(hz) = now {
            self.report.set("refresh_hz", format!("{hz:.0}"));
        }
    }

    pub fn session_state(&mut self, state: xr::SessionState) {
        let focused = state == xr::SessionState::FOCUSED;
        if focused && self.focused_at.is_none() {
            self.focused_at = Some(Instant::now());
        }
        // Losing the focus in the middle (the system menu, the headset taken off)
        // makes the numbers incomparable.
        if self.focused && !focused && !matches!(self.phase, Phase::Waiting | Phase::Done) {
            self.focus_lost += 1;
        }
        self.focused = focused;
    }

    /// An event of the network thread, before the app handles it.
    pub fn net_event(&mut self, event: &NetEvent) {
        let now = Instant::now();
        match event {
            NetEvent::Connected { .. } if self.connected_at.is_none() => {
                self.connected_at = Some(now);
                self.report.set_ms("connect_ms", Some(ms(now - self.begun)));
            }
            NetEvent::Part(prostir_proto::HostMsg::Displays { .. })
                if self.displays_at.is_none() =>
            {
                self.displays_at = Some(now);
                if let Some(c) = self.connected_at {
                    self.report.set_ms("snapshot_ms", Some(ms(now - c)));
                }
            }
            _ => {}
        }
    }

    /// At the top of every pass of the main loop: a calm frame's allocations are all
    /// of the main thread's from one top to the next.
    pub fn loop_top(&mut self) {
        let now = crate::alloc_count::main_thread();
        if let (true, Some(before), Some(after)) = (self.calm_frame, self.allocs_at_top, now) {
            self.allocs.add(after.saturating_sub(before) as f64);
        }
        self.allocs_at_top = now;
        self.calm_frame = false;
    }

    /// Right after `xrWaitFrame` returned.
    pub fn frame_start(&self) -> FrameStart {
        FrameStart {
            cpu: thread_cpu(),
            wall: Instant::now(),
        }
    }

    /// Right after `xrEndFrame` returned: the frame's work (`menu`: how long the menu
    /// took to draw, if it was drawn), when it is shown, its period and its layers.
    pub fn frame_end(
        &mut self,
        start: &FrameStart,
        display_ns: i64,
        period: Duration,
        menu: Option<Duration>,
        layers: usize,
    ) {
        let cpu = thread_cpu().saturating_sub(start.cpu);
        let wall = start.wall.elapsed();
        let period = Some(period).filter(|p| !p.is_zero());
        self.period = period.or(self.period);
        self.frames += 1;
        self.layers_max = self.layers_max.max(layers);
        let share = |d: Duration| period.map(|p| d.as_secs_f64() / p.as_secs_f64());
        if let Some(menu) = menu {
            self.menu_ms.add(ms(menu));
            if let Some(s) = share(menu) {
                self.menu_share.add(s);
            }
        }
        let missed = match (self.last_display_ns, period) {
            (Some(last), Some(p)) => (display_ns - last) as f64 > 1.5 * p.as_nanos() as f64,
            _ => false,
        };
        self.last_display_ns = Some(display_ns);
        match self.phase {
            Phase::Steady => {
                self.missed_steady += u32::from(missed);
                if menu.is_none() {
                    self.frame_cpu.add(ms(cpu));
                    self.frame_wall.add(ms(wall));
                    if let Some(s) = share(cpu) {
                        self.frame_share.add(s);
                    }
                    self.calm_frame = true;
                }
            }
            Phase::Move => self.missed_moving += u32::from(missed),
            _ => {}
        }
    }

    /// An act that saves (the arrangement, the settings) took this long, in the frame.
    pub fn saved(&mut self, took: Duration) {
        self.persist_ms.add(ms(took));
    }

    /// Once a pass of the main loop, frame or not: what the app should do now.
    pub fn step(&mut self) -> Act {
        self.take_pc_events();
        self.take_pictures();
        let now = Instant::now();
        let in_phase = now - self.phase_since;
        if self.phase != Phase::Done
            && now - self.begun > Duration::from_secs(u64::from(self.options.seconds)) + EXTRA
        {
            self.end_early(
                Exit::Timeout,
                &format!("still {:?} at the deadline", self.phase),
            );
            return Act::Finish;
        }
        match self.phase {
            Phase::Waiting => {
                if self.focused_at.is_none() && now - self.begun > FOCUS_WITHIN {
                    self.end_early(
                        Exit::NoStart,
                        "the OpenXR session was not focused within 10 s (is the headset worn?)",
                    );
                    return Act::Finish;
                }
                if self.all_shown() {
                    self.next(Phase::Settle, now);
                } else if now - self.begun > PICTURES_WITHIN {
                    self.end_early(Exit::Fail, "no picture on every monitor within 15 s");
                    return Act::Finish;
                }
                Act::Nothing
            }
            Phase::Settle if in_phase >= SETTLE => {
                self.allocs_all = crate::alloc_count::all().map(|n| (n, now));
                self.switches = Some((decoder_switches(), now));
                self.steady_period = self.period;
                self.report.set_ms("period_ms", self.steady_period.map(ms));
                self.next(Phase::Steady, now);
                Act::Nothing
            }
            Phase::Steady if in_phase >= Duration::from_secs(u64::from(self.options.seconds)) => {
                if let (Some((before, at)), Some(after)) =
                    (self.allocs_all, crate::alloc_count::all())
                {
                    self.allocs_all_per_s =
                        Some(after.saturating_sub(before) as f64 / (now - at).as_secs_f64());
                }
                if let Some((before, at)) = self.switches.take() {
                    // Only the threads there all along: one that started or ended
                    // meanwhile is not a calm decoder.
                    let after = decoder_switches();
                    let woken: u64 = before
                        .iter()
                        .filter_map(|(tid, n)| after.get(tid).map(|m| m.saturating_sub(*n)))
                        .sum();
                    self.decoder_wakeups_per_s = Some(woken as f64 / (now - at).as_secs_f64());
                }
                self.next(Phase::Move, now);
                Act::MoveMonitor
            }
            Phase::Move if in_phase >= MOVE_FOR => {
                self.next(Phase::Menu, now);
                Act::OpenMenu
            }
            Phase::Menu if in_phase >= MENU_FOR * 2 => {
                if self.break_a_frame() {
                    self.next(Phase::Broken, now);
                } else {
                    self.end_early(Exit::Fail, "the fake PC has no session to break a frame in");
                }
                Act::CloseMenu
            }
            Phase::Menu if in_phase >= MENU_FOR && !self.tab_switched => {
                self.tab_switched = true;
                Act::SwitchTab
            }
            Phase::Broken if self.recovered_ms.is_some() || in_phase >= RECOVER_WITHIN => {
                let id = self
                    .monitors
                    .get(1)
                    .or_else(|| self.monitors.first())
                    .copied();
                if let (Some(id), Some(pc)) = (id, &self.pc)
                    && pc.restart_stream(id)
                {
                    self.restart = Some(id);
                }
                self.next(Phase::Restart, now);
                Act::Nothing
            }
            Phase::Restart if self.restart_ms.is_some() || in_phase >= RECOVER_WITHIN => {
                if let Some(pc) = &self.pc {
                    pc.drop_connection();
                }
                self.dropped_at = Some(now);
                self.next(Phase::Dropped, now);
                Act::Nothing
            }
            Phase::Dropped
                if self.reconnect_picture_ms.is_some() || in_phase >= RECONNECT_WITHIN =>
            {
                self.next(Phase::Done, now);
                Act::Finish
            }
            Phase::Done => Act::Finish,
            _ => Act::Nothing,
        }
    }

    /// Has the fake PC send a frame the first monitor's decoder cannot take, with the
    /// decoders' pictures of that stream watched from now on.
    fn break_a_frame(&self) -> bool {
        let Some(id) = self.monitors.first().copied() else {
            return false;
        };
        let Some(generation) = self.streams.get(&id).map(|s| s.generation) else {
            return false;
        };
        if let Some(probe) = &self.probe {
            lock(probe).watch(id.0, generation.0);
        }
        self.pc.as_ref().is_some_and(|pc| pc.send_broken_frame(id))
    }

    fn next(&mut self, phase: Phase, now: Instant) {
        info!("selftest: {phase:?}");
        self.phase = phase;
        self.phase_since = now;
    }

    /// Every monitor of the fake PC's latest session shows a picture.
    fn all_shown(&self) -> bool {
        !self.monitors.is_empty()
            && self.monitors.iter().all(|id| {
                self.streams
                    .get(id)
                    .is_some_and(|s| s.session == self.sessions && s.shown.is_some())
            })
    }

    /// What came out of the decoders since: first pictures, and the watched stream's
    /// first picture after its broken frame.
    fn take_pictures(&mut self) {
        let Some(probe) = self.probe.clone() else {
            return;
        };
        let p = lock(&probe);
        for (id, s) in &mut self.streams {
            if s.shown.is_some() {
                continue;
            }
            let Some(&at) = p.first.get(&(id.0, s.generation.0)) else {
                continue;
            };
            s.shown = Some(at);
            let first = ms(at.saturating_duration_since(s.started));
            if s.session == 1 && self.restart != Some(*id) {
                self.first_frames.entry(*id).or_insert(first);
            }
            if self.restart == Some(*id) && s.session == 1 && self.restart_ms.is_none() {
                self.restart_ms = Some(first);
            }
            if let Some(dropped) = self.dropped_at
                && s.session > 1
                && self.reconnect_picture_ms.is_none()
            {
                self.reconnect_picture_ms = Some(ms(at.saturating_duration_since(dropped)));
            }
        }
        if let Some((_, _, bad, since)) = self.broken
            && self.recovered_ms.is_none()
            && let Some(&(_, at)) = p.watched.iter().find(|(seq, _)| *seq > bad)
        {
            self.recovered_ms = Some(ms(at.saturating_duration_since(since)));
        }
    }

    fn take_pc_events(&mut self) {
        // Taken out meanwhile, so the events need no list of their own.
        let Some(pc) = self.pc.take() else { return };
        while let Ok(Event { at, what }) = pc.events().try_recv() {
            self.pc_event(at, what);
        }
        self.pc = Some(pc);
    }

    fn pc_event(&mut self, at: Instant, what: Happened) {
        match what {
            Happened::Opened { .. } => {
                self.sessions += 1;
                if let Some(dropped) = self.dropped_at
                    && self.sessions > 1
                    && self.reconnect_ms.is_none()
                {
                    self.reconnect_ms = Some(ms(at.saturating_duration_since(dropped)));
                }
            }
            Happened::Snapshot { monitors } => self.monitors = monitors,
            Happened::StreamStarted {
                id,
                generation,
                codec,
                width,
                height,
            } => {
                if self.sessions == 1 && !self.streams.contains_key(&id) {
                    let n = self
                        .monitors
                        .iter()
                        .position(|m| *m == id)
                        .map_or(0, |k| k + 1);
                    self.report.set(
                        &format!("stream.{n}.clip"),
                        format!("{codec:?} {width}x{height}"),
                    );
                }
                self.streams.insert(
                    id,
                    Seen {
                        generation,
                        started: at,
                        shown: None,
                        session: self.sessions,
                    },
                );
            }
            Happened::BrokenSent {
                id,
                generation,
                seq,
            } => self.broken = Some((id, generation, seq, at)),
            Happened::KeyframeAsked { id, generation } => {
                if let Some((b, g, _, since)) = self.broken
                    && (b, g) == (id, generation)
                    && self.keyframe_asked_ms.is_none()
                {
                    self.keyframe_asked_ms = Some(ms(at.saturating_duration_since(since)));
                }
            }
            _ => {}
        }
    }

    /// Ends the selftest: the report, to the log and to `external`'s `selftest.txt`.
    /// `error`: the app could not go on. Whether the process should stay (the next
    /// start runs in it), and the exit code otherwise.
    pub fn finish(mut self, external: Option<&Path>, error: Option<&anyhow::Error>) -> (bool, i32) {
        self.take_pc_events();
        self.take_pictures();
        if let Some(e) = error {
            let exit = if self.focused_at.is_some() {
                Exit::Fail
            } else {
                Exit::NoStart
            };
            self.end_early(exit, &format!("{e:#}"));
        }
        if self.exit.is_none() && self.phase != Phase::Done {
            let phase = self.phase;
            self.end_early(Exit::Fail, &format!("the app stopped during {phase:?}"));
        }
        if let Some(pc) = self.pc.take() {
            for (k, id) in self.monitors.iter().enumerate() {
                self.report
                    .set(&format!("stream.{}.sent", k + 1), pc.frames_sent(*id));
            }
            pc.stop();
        }
        super::probe_stop();
        self.fill_report();
        let exit = match self.exit {
            Some(exit) => exit,
            None => {
                let mut fails = check(&self.measured());
                if self.focus_lost > 0 {
                    fails.push(format!("the focus was lost {} times", self.focus_lost));
                }
                self.fails.extend(fails);
                if self.fails.is_empty() {
                    Exit::Pass
                } else {
                    Exit::Fail
                }
            }
        };
        let r = &mut self.report;
        r.set("battery.end", battery());
        if !self.fails.is_empty() {
            r.set("fail", self.fails.join("; "));
        }
        r.set("result", if exit == Exit::Pass { "pass" } else { "fail" });
        r.set("exit", exit.code());
        let text = r.text();
        for line in text.lines() {
            log_line(line);
        }
        if let Some(dir) = external {
            let path = dir.join("selftest.txt");
            let tmp = dir.join("selftest.txt.part");
            if let Err(e) = std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, &path))
            {
                warn!("selftest: cannot write {}: {e}", path.display());
            }
        }
        (self.options.relaunch && exit != Exit::NoStart, exit.code())
    }

    fn measured(&self) -> Measured {
        Measured {
            first_frames_ms: self
                .monitors
                .iter()
                .map(|id| self.first_frames.get(id).copied())
                .collect(),
            keyframe_asked_ms: self.keyframe_asked_ms,
            decoder_recovered_ms: self.recovered_ms,
            reconnect_ms: self.reconnect_ms,
            frame_share_p95: self.frame_share.percentile(0.95),
            frame_share_p99: self.frame_share.percentile(0.99),
        }
    }

    fn fill_report(&mut self) {
        let probe = self.probe.take().map(|p| std::mem::take(&mut *lock(&p)));
        let r = &mut self.report;
        for (k, id) in self.monitors.iter().enumerate() {
            let n = k + 1;
            r.set_ms(
                &format!("stream.{n}.first_frame_ms"),
                self.first_frames.get(id).copied(),
            );
            let decoded = probe
                .as_ref()
                .and_then(|p| p.decoded.get(&id.0).copied())
                .unwrap_or(0);
            r.set(&format!("stream.{n}.frames"), decoded);
        }
        if let Some(p) = &probe {
            for (key, h) in [("decode_ms", &p.decode), ("net_to_decoder_ms", &p.wait)] {
                if h.count() == 0 {
                    continue;
                }
                for (name, q) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
                    r.set_ms(&format!("{key}.{name}"), Some(ms(h.percentile(q))));
                }
                r.set_ms(&format!("{key}.max"), Some(ms(h.max())));
            }
            r.set("decoder_queue_max", p.queue_max);
            r.set("frames_dropped_to_keyframe", p.dropped_to_keyframe);
        }
        r.set_ms("keyframe_asked_ms", self.keyframe_asked_ms);
        r.set_ms("decoder_recovered_ms", self.recovered_ms);
        // The decoder's own fault hooks come with its rework (step 7.8b); until then
        // this stays `none`.
        r.set_ms("stream_restart_ms", self.restart_ms);
        r.set_ms("reconnect_ms", self.reconnect_ms);
        r.set_ms("reconnect_picture_ms", self.reconnect_picture_ms);
        r.set("frame_samples", self.frame_cpu.len());
        r.set_spread("frame_cpu_ms", &self.frame_cpu);
        r.set_spread("frame_wall_ms", &self.frame_wall);
        r.set_share("frame_cpu_share.p95", self.frame_share.percentile(0.95));
        r.set_share("frame_cpu_share.p99", self.frame_share.percentile(0.99));
        r.set("menu_redraws", self.menu_ms.len());
        r.set_ms("menu_redraw_ms.p95", self.menu_ms.percentile(0.95));
        r.set_ms("menu_redraw_ms.max", self.menu_ms.max());
        r.set_share("menu_redraw_share.p95", self.menu_share.percentile(0.95));
        r.set_share("menu_redraw_share.max", self.menu_share.max());
        if let Some(p) = &probe {
            r.set("image_uploads", p.upload.count());
            if p.upload.count() > 0 {
                r.set_ms("image_upload_ms.p95", Some(ms(p.upload.percentile(0.95))));
                r.set_ms("image_upload_ms.max", Some(ms(p.upload.max())));
            }
        }
        r.set("frames", self.frames);
        r.set("frames_missed", self.missed_steady);
        r.set("frames_missed_moving", self.missed_moving);
        r.set("focus_lost", self.focus_lost);
        r.set("layers.max", self.layers_max);
        r.set_ms("persist_ms.max", self.persist_ms.max());
        match self.allocs.mean() {
            Some(mean) => r.set("alloc_per_frame", format!("{mean:.1}")),
            None => r.set("alloc_per_frame", "none"),
        }
        if let Some(max) = self.allocs.max() {
            r.set("alloc_per_frame.max", format!("{max:.0}"));
        }
        if let Some(a) = self.allocs_all_per_s {
            r.set("alloc_all_threads_per_s", format!("{a:.0}"));
        }
        if let Some(w) = self.decoder_wakeups_per_s {
            r.set("decoder_wakeups_per_s", format!("{w:.0}"));
        }
    }
}
