//! Headset microphone to the PC: captured with AAudio only while a PC app is
//! recording, in 10 ms slices, as an up stream (`UpStart`, `Up`, `UpEnd`).

#![expect(
    clippy::disallowed_methods,
    reason = "the microphone thread is part of the shell"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use android_activity::AndroidApp;
use jni::objects::{JObject, JValue};
use jni::sys::jobject;
use jni::{jni_sig, jni_str};
use log::{info, warn};
use ndk_sys as ffi;
use prostir_proto::state::{UpFormat, UpKind, permission};
use prostir_proto::{AUDIO_RATE, ClientMsg, MIC_CHANNELS};

use crate::net::Feedback;
use crate::platform::android::with_app_context;
use crate::platform::mic_permission::{LOOK_EVERY, PermissionWait, Prompt, Step};

const PERMISSION: &str = "android.permission.RECORD_AUDIO";
/// 10 ms per slice.
const FRAMES: usize = AUDIO_RATE as usize / 100;
/// The microphone of one start of the app: the user's switch, what the system allows,
/// the numbers of its up streams. Made at the start and shared: the menu flips the
/// switch, the network thread records while a PC app asks. A new start in the same
/// process makes its own.
#[derive(Debug)]
pub struct Mic {
    /// The number of the next recording's up stream: each one gets its own, so the
    /// end of an old one, still on its way when the microphone is switched off and on
    /// quickly, cannot end the new one on the PC.
    next_up: AtomicU8,
    /// Whether the system lets the app record, as last found (see
    /// `prostir_proto::state::permission`): the PC shows when it does not.
    allowed_by_system: AtomicU8,
    /// The user's microphone switch, from the headset's own settings. Recording
    /// happens only while this is on, whatever the PC asks.
    allowed: AtomicBool,
    /// Access to the microphone was refused: no more system prompts each time a PC
    /// app starts recording, and the menu says where to allow it.
    denied: AtomicBool,
    /// How many times the system was asked for access: its prompt pauses the app,
    /// which the main loop must not take for the user leaving.
    asks: AtomicU32,
    /// What the last prompt went through (a `Prompt`'s code): answered without access
    /// is a refusal at once.
    prompt: AtomicU8,
    /// The running activity, which shows the system's prompt.
    app: AndroidApp,
}

impl Mic {
    /// With the user's switch as the settings have it.
    pub fn new(allowed: bool, app: AndroidApp) -> Self {
        Self {
            next_up: AtomicU8::new(0),
            allowed_by_system: AtomicU8::new(permission::UNKNOWN),
            allowed: AtomicBool::new(allowed),
            denied: AtomicBool::new(false),
            asks: AtomicU32::new(0),
            prompt: AtomicU8::new(Prompt::None.code()),
            app,
        }
    }

    pub fn permission(&self) -> u8 {
        self.allowed_by_system.load(Ordering::Relaxed)
    }

    /// Looks whether the system lets the app record now (one call into Android).
    pub fn refresh_permission(&self) {
        let now = if has_permission() {
            permission::GRANTED
        } else if self.denied.load(Ordering::Relaxed) {
            permission::DENIED
        } else {
            permission::UNKNOWN
        };
        self.allowed_by_system.store(now, Ordering::Relaxed);
    }

    pub fn set_allowed(&self, allowed: bool) {
        // Switched on again by hand: the system may ask for access once more.
        if allowed && !self.allowed.load(Ordering::Relaxed) {
            self.denied.store(false, Ordering::Relaxed);
        }
        self.allowed.store(allowed, Ordering::Relaxed);
    }

    pub fn denied(&self) -> bool {
        self.denied.load(Ordering::Relaxed)
    }

    /// How many times the system was asked for access so far (see `asks`).
    pub fn asks(&self) -> u32 {
        self.asks.load(Ordering::Relaxed)
    }

    /// Shows the system's prompt (counted first, so the main loop knows the pause
    /// that follows is the prompt's).
    fn ask(&self) {
        self.asks.fetch_add(1, Ordering::Relaxed);
        self.prompt.store(Prompt::Asked.code(), Ordering::Relaxed);
        request_permission(&self.app);
    }

    /// The app was paused (`true`, the system's prompt may be up) or came back (it
    /// may have been answered).
    pub fn activity(&self, paused: bool) {
        let was = Prompt::from_code(self.prompt.load(Ordering::Relaxed));
        let now = was.activity(paused);
        if now != was {
            self.prompt.store(now.code(), Ordering::Relaxed);
        }
    }

    /// The user pressed "Ask again": show the system prompt now.
    pub fn ask_again(&self) {
        self.denied.store(false, Ordering::Relaxed);
        if !has_permission() {
            self.ask();
        }
    }

    pub fn allowed(&self) -> bool {
        self.allowed.load(Ordering::Relaxed)
    }

    /// Waits until the system lets the app record, or `stop` (see `mic_permission`):
    /// the system is asked once unless a refusal stands; a refusal counts as soon as
    /// its prompt was answered without access; and the wait goes on after one, looking
    /// every 500 ms, so "Ask again" or access given in the system's settings bring the
    /// microphone back while the PC still records.
    fn wait_permission(&self, stop: &AtomicBool) -> bool {
        let mut wait = PermissionWait::new(self.asks());
        loop {
            if stop.load(Ordering::Relaxed) {
                // Not an answer: the recording was no longer wanted (the session ended,
                // the switch went off). The next recording looks again.
                info!("stopped waiting for the microphone permission");
                if !self.denied() {
                    self.allowed_by_system
                        .store(permission::UNKNOWN, Ordering::Relaxed);
                }
                return false;
            }
            let prompt = Prompt::from_code(self.prompt.load(Ordering::Relaxed));
            let step = wait.look(
                Instant::now(),
                has_permission(),
                self.denied(),
                self.asks(),
                prompt,
            );
            let status = match step {
                Step::Record => {
                    self.denied.store(false, Ordering::Relaxed);
                    self.allowed_by_system
                        .store(permission::GRANTED, Ordering::Relaxed);
                    return true;
                }
                Step::Ask => {
                    self.ask();
                    permission::ASKING
                }
                Step::Refused => {
                    warn!("no microphone permission");
                    self.denied.store(true, Ordering::Relaxed);
                    permission::DENIED
                }
                // Refused before: the menu says where to allow it; else a prompt is up.
                Step::Wait if self.denied() => permission::DENIED,
                Step::Wait => permission::ASKING,
            };
            self.allowed_by_system.store(status, Ordering::Relaxed);
            std::thread::sleep(LOOK_EVERY);
        }
    }
}

/// Records until dropped.
pub struct Recorder {
    stop: Arc<AtomicBool>,
}

impl Recorder {
    pub fn start(mic: Arc<Mic>, feedback: Feedback) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let up = mic.next_up.fetch_add(1, Ordering::Relaxed);
        let _ = std::thread::Builder::new()
            .name("mic".into())
            .spawn(move || {
                crate::platform::android::raise_thread_priority(
                    crate::platform::android::Priority::Audio,
                );
                if !mic.wait_permission(&flag) {
                    return;
                }
                let start = ClientMsg::UpStart {
                    up,
                    kind: UpKind::Mic,
                    format: UpFormat::Pcm16 {
                        rate: AUDIO_RATE,
                        channels: MIC_CHANNELS as u8,
                    },
                };
                if feedback.send(start).is_err() {
                    return;
                }
                // A broken input stream (an audio route change) is opened again.
                let mut failures = 0;
                while failures < 5 && !flag.load(Ordering::Relaxed) {
                    match Input::open() {
                        Ok(input) => {
                            if record(&input, up, &feedback, &flag) {
                                break;
                            }
                            failures = 0;
                        }
                        Err(e) => {
                            warn!("no microphone: AAudio error {e}");
                            failures += 1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
                let _ = feedback.send(ClientMsg::UpEnd { up });
            })
            .inspect_err(|e| warn!("no microphone: {e}"));
        Self { stop }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Records until stopped (`true`) or the stream breaks (`false`).
fn record(input: &Input, up: u8, feedback: &Feedback, stop: &AtomicBool) -> bool {
    let mut buf = vec![0i16; FRAMES * MIC_CHANNELS as usize];
    info!("headset microphone on");
    while !stop.load(Ordering::Relaxed) {
        match input.read(&mut buf) {
            Ok(0) => {}
            Ok(frames) => {
                let samples = &buf[..(frames * MIC_CHANNELS as usize).min(buf.len())];
                // Into a slice sent before (see `Feedback::up_buffer`): nothing new
                // for each 10 ms.
                let mut pcm = feedback.up_buffer(samples.len() * 2);
                for s in samples {
                    pcm.extend_from_slice(&s.to_le_bytes());
                }
                if feedback.send(ClientMsg::Up { up, data: pcm }).is_err() {
                    break;
                }
            }
            Err(e) => {
                warn!("microphone: AAudio error {e}");
                return false;
            }
        }
    }
    info!("headset microphone off");
    true
}

fn has_permission() -> bool {
    with_app_context(|env, context| {
        let name = env.new_string(PERMISSION)?;
        let granted = env
            .call_method(
                context,
                jni_str!("checkSelfPermission"),
                jni_sig!("(Ljava/lang/String;)I"),
                &[JValue::Object(&name)],
            )?
            .i()?;
        Ok(granted == 0) // PackageManager.PERMISSION_GRANTED
    })
    .unwrap_or(false)
}

/// Shows the system's prompt for the microphone. Only an Activity has
/// `requestPermissions` (the Application has not: asked there, the prompt never came),
/// and it is asked on the Java main thread, as Android expects; the answer is seen
/// later through [`has_permission`].
fn request_permission(app: &AndroidApp) {
    let on_main = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        // SAFETY: the VM of the running NativeActivity.
        let vm = unsafe { jni::JavaVM::from_raw(on_main.vm_as_ptr().cast()) };
        let asked = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            let raw: jobject = on_main.activity_as_ptr().cast();
            // SAFETY: the activity's object, valid while the activity runs; the cast
            // only borrows it, it never deletes it.
            let activity = unsafe { env.as_cast_raw::<JObject<'static>>(&raw) }?;
            let name = env.new_string(PERMISSION)?;
            let list = env.new_object_array(1, jni_str!("java/lang/String"), &name)?;
            env.call_method(
                &*activity,
                jni_str!("requestPermissions"),
                jni_sig!("([Ljava/lang/String;I)V"),
                &[JValue::Object(&list), JValue::Int(1)],
            )?;
            Ok(())
        });
        if let Err(e) = asked {
            warn!("cannot ask for the microphone: {e}");
        }
    }));
}

/// An AAudio input stream (through `ndk-sys`, like the player).
struct Input(*mut ffi::AAudioStream);

// SAFETY: used by the mic thread only.
unsafe impl Send for Input {}

impl Input {
    fn open() -> Result<Self, i32> {
        // SAFETY: plain AAudio calls; the builder is deleted on every path.
        unsafe {
            let mut builder = std::ptr::null_mut();
            check(ffi::AAudio_createStreamBuilder(&raw mut builder))?;
            ffi::AAudioStreamBuilder_setDirection(builder, ffi::AAUDIO_DIRECTION_INPUT as i32);
            ffi::AAudioStreamBuilder_setFormat(builder, ffi::AAUDIO_FORMAT_PCM_I16);
            ffi::AAudioStreamBuilder_setChannelCount(builder, MIC_CHANNELS as i32);
            ffi::AAudioStreamBuilder_setSampleRate(builder, AUDIO_RATE as i32);
            ffi::AAudioStreamBuilder_setPerformanceMode(
                builder,
                ffi::AAUDIO_PERFORMANCE_MODE_LOW_LATENCY as i32,
            );
            ffi::AAudioStreamBuilder_setInputPreset(
                builder,
                ffi::AAUDIO_INPUT_PRESET_VOICE_COMMUNICATION as i32,
            );
            let mut stream = std::ptr::null_mut();
            let opened = ffi::AAudioStreamBuilder_openStream(builder, &raw mut stream);
            ffi::AAudioStreamBuilder_delete(builder);
            check(opened)?;
            let input = Self(stream);
            check(ffi::AAudioStream_requestStart(stream))?;
            Ok(input)
        }
    }

    /// Reads up to `buf.len()` samples, waiting at most 100 ms; returns frames read.
    fn read(&self, buf: &mut [i16]) -> Result<usize, i32> {
        let frames = (buf.len() / MIC_CHANNELS as usize) as i32;
        // SAFETY: `buf` has room for `frames` frames; the stream is open.
        let n =
            unsafe { ffi::AAudioStream_read(self.0, buf.as_mut_ptr().cast(), frames, 100_000_000) };
        if n < 0 { Err(n) } else { Ok(n as usize) }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: opened by `Input::open`, closed once.
        unsafe {
            ffi::AAudioStream_requestStop(self.0);
            ffi::AAudioStream_close(self.0);
        }
    }
}

fn check(result: i32) -> Result<(), i32> {
    if result < 0 { Err(result) } else { Ok(()) }
}
