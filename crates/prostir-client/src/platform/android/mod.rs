//! What every headset on Android shares, whoever made it: calls into Java (one `jni`
//! for the app), the battery, the system's language, the headset's model, the Wi-Fi
//! lock, the threads' priorities, the system's decoders ([`codecs`]) and volume, and
//! what the app was started with. The `system` thread ([`system`]) asks the system now
//! and then, away from the frame; the rest is asked at the start, or on an event.

#![expect(
    clippy::disallowed_methods,
    reason = "the platform is the shell: it asks the system"
)]

pub mod codecs;
pub mod system;

use std::sync::atomic::{AtomicBool, Ordering};

use android_activity::AndroidApp;
use anyhow::Result;
use jni::objects::{JObject, JString, JValue};
use jni::strings::JNIStr;
use jni::sys::jobject;
use jni::{Env, jni_sig, jni_str};
use log::warn;

use crate::lang::Lang;

/// `Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY`: started again from the recent apps,
/// with the intent it was first started with.
const FROM_HISTORY: i32 = 0x0010_0000;
/// `AudioManager.STREAM_MUSIC`.
const STREAM_MUSIC: i32 = 3;

/// Runs `f` with the app's context: the Application that `ndk_context` holds
/// (android-activity puts it there), not the activity. Enough for system services and
/// checking permissions; what needs the Activity itself (asking for one) takes it from
/// the `AndroidApp`.
pub(crate) fn with_app_context<T>(
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Option<T> {
    let ctx = ndk_context::android_context();
    // SAFETY: the VM of the running NativeActivity.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    // In a local frame of its own; a Java exception on the way is caught and cleared,
    // and comes back as the error.
    vm.attach_current_thread(|env| {
        let raw: jobject = ctx.context().cast();
        // SAFETY: a global reference to the Application, kept for the life of the
        // process; the cast only borrows it, it never deletes it.
        let context = unsafe { env.as_cast_raw::<JObject<'static>>(&raw) }?;
        f(env, &context)
    })
    .inspect_err(|e| warn!("a call into Android failed: {e}"))
    .ok()
}

/// What the headset is, as the system names it (`Build.MANUFACTURER` and
/// `Build.MODEL`, see [`crate::device::model_name`]); empty when it does not say.
pub fn model() -> String {
    let said = with_app_context(|env, _| {
        let mut field = |name: &JNIStr| -> jni::errors::Result<String> {
            let value = env
                .get_static_field(
                    jni_str!("android/os/Build"),
                    name,
                    jni_sig!("Ljava/lang/String;"),
                )?
                .l()?;
            if value.is_null() {
                return Ok(String::new());
            }
            env.cast_local::<JString<'_>>(value)?.try_to_string(env)
        };
        Ok((field(jni_str!("MANUFACTURER"))?, field(jni_str!("MODEL"))?))
    });
    let (manufacturer, model) = said.unwrap_or_default();
    crate::device::model_name(&manufacturer, &model)
}

/// The headset's own language (`Locale.getDefault()`, else the system property
/// `persist.sys.locale`); none when the system does not say.
pub fn language() -> Option<Lang> {
    let tag = with_app_context(|env, _| {
        let locale = env
            .call_static_method(
                jni_str!("java/util/Locale"),
                jni_str!("getDefault"),
                jni_sig!("()Ljava/util/Locale;"),
                &[],
            )?
            .l()?;
        let tag = env
            .call_method(
                &locale,
                jni_str!("toLanguageTag"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        if tag.is_null() {
            return Ok(String::new());
        }
        env.cast_local::<JString<'_>>(tag)?.try_to_string(env)
    })
    .filter(|t| !t.is_empty() && t != "und")
    .or_else(|| {
        let out = std::process::Command::new("getprop")
            .arg("persist.sys.locale")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    })?;
    let lang = Lang::from_tag(&tag);
    log::info!("system language {tag:?}: {lang:?}");
    lang
}

/// Whether the screen is on for the user (`PowerManager.isInteractive`): false once
/// the system goes to sleep (its power button, or taken off for long enough). One
/// call into Android: asked only while the app is put away, never in a drawn frame.
pub fn interactive() -> Option<bool> {
    with_app_context(|env, context| {
        let name = env.new_string("power")?;
        let manager = env
            .call_method(
                context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        env.call_method(&manager, jni_str!("isInteractive"), jni_sig!("()Z"), &[])?
            .z()
    })
}

/// The headset's battery: percent and whether it is charging (`BatteryManager`).
pub fn battery() -> Option<(u8, bool)> {
    with_app_context(|env, context| {
        let name = env.new_string("batterymanager")?;
        let manager = env
            .call_method(
                context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        let property = |env: &mut Env<'_>, id| {
            env.call_method(
                &manager,
                jni_str!("getIntProperty"),
                jni_sig!("(I)I"),
                &[JValue::Int(id)],
            )?
            .i()
        };
        // BatteryManager.BATTERY_PROPERTY_CAPACITY
        let percent = property(env, 4)?;
        // BATTERY_PROPERTY_STATUS: 2 is charging (isCharging also says yes when full
        // and plugged in).
        let status = property(env, 6)?;
        let charging = status == 2;
        Ok((percent.clamp(0, 100) as u8, charging))
    })
}

/// Keeps Wi-Fi out of power save while the app runs. Without it the headset's
/// radio naps between beacons and downlink frames wait at the router.
pub fn acquire_low_latency_wifi(
    app: &AndroidApp,
    vm: &jni::JavaVM,
) -> Result<jni::refs::Global<JObject<'static>>> {
    /// WifiManager.WIFI_MODE_FULL_LOW_LATENCY (API 29).
    const WIFI_MODE_FULL_LOW_LATENCY: i32 = 4;
    // A Java exception on the way is caught and comes back as the error.
    let lock = vm.attach_current_thread(|env| -> jni::errors::Result<_> {
        let raw: jobject = app.activity_as_ptr().cast();
        // SAFETY: the activity reference stays valid while the app runs; the cast only
        // borrows it, it never deletes it.
        let activity = unsafe { env.as_cast_raw::<JObject<'static>>(&raw) }?;
        let context = env
            .call_method(
                &activity,
                jni_str!("getApplicationContext"),
                jni_sig!("()Landroid/content/Context;"),
                &[],
            )?
            .l()?;
        let service = env.new_string("wifi")?;
        let wifi = env
            .call_method(
                &context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&service)],
            )?
            .l()?;
        let tag = env.new_string("prostir")?;
        let lock = env
            .call_method(
                &wifi,
                jni_str!("createWifiLock"),
                jni_sig!("(ILjava/lang/String;)Landroid/net/wifi/WifiManager$WifiLock;"),
                &[
                    JValue::Int(WIFI_MODE_FULL_LOW_LATENCY),
                    JValue::Object(&tag),
                ],
            )?
            .l()?;
        env.call_method(
            &lock,
            jni_str!("setReferenceCounted"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(false)],
        )?;
        env.call_method(&lock, jni_str!("acquire"), jni_sig!("()V"), &[])?;
        env.new_global_ref(lock)
    })?;
    Ok(lock)
}

/// How urgent a thread of this app is to the scheduler, as Android ranks its own
/// threads of the same kind.
#[derive(Debug, Clone, Copy)]
pub enum Priority {
    /// Sound in and out (`THREAD_PRIORITY_AUDIO`).
    Audio,
    /// Pictures and the network that brings them (`THREAD_PRIORITY_VIDEO`).
    Video,
}

/// Whether a refused priority of each kind was logged (once per start of the
/// process is enough: threads of a kind are refused alike).
static REFUSAL_TOLD: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// Raises the calling thread to `priority`, so a busy moment elsewhere on the headset
/// delays it less. A refusal by the system is logged once for each kind.
pub fn raise_thread_priority(priority: Priority) {
    let (nice, kind) = match priority {
        Priority::Audio => (-16, 0),
        Priority::Video => (-10, 1),
    };
    // SAFETY: plain system call; on Linux `PRIO_PROCESS` with `who` 0 is the calling
    // thread alone.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } != 0
        && !REFUSAL_TOLD[kind].swap(true, Ordering::Relaxed)
    {
        warn!(
            "thread priority {nice}: {} (not told again)",
            std::io::Error::last_os_error()
        );
    }
}

/// Runs `f` with the running activity (the Activity object of the NativeActivity, not
/// the Application [`with_app_context`] gives), in a local frame of its own.
pub fn with_activity<T>(
    app: &AndroidApp,
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Option<T> {
    // SAFETY: the VM of the running NativeActivity.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    // A Java exception on the way is caught and cleared, and comes back as the error.
    vm.attach_current_thread(|env| {
        let raw: jobject = app.activity_as_ptr().cast();
        // SAFETY: the activity's object, valid while the activity runs (this start);
        // the cast only borrows it, it never deletes it.
        let activity = unsafe { env.as_cast_raw::<JObject<'static>>(&raw) }?;
        f(env, &activity)
    })
    .inspect_err(|e| warn!("a call into Android failed: {e}"))
    .ok()
}

/// The string extra `key` of the intent that started the activity, if it has one (not
/// when it was started again from the recent apps with an old intent): what the
/// selftest or a check by hand asks of this start (`adb shell am start ... --es caps
/// minimal`).
pub fn intent_extra(app: &AndroidApp, key: &str) -> Option<String> {
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
        let key = env.new_string(key)?;
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

/// The system's volume for media (`AudioManager.STREAM_MUSIC`): its steps above
/// silence (`getStreamMaxVolume`) and whether it is fixed (`isVolumeFixed`: then the
/// app cannot set it). `None` when the system could not be asked.
pub fn volume() -> Option<(i32, bool)> {
    with_app_context(|env, context| {
        let name = env.new_string("audio")?;
        let manager = env
            .call_method(
                context,
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
                &[JValue::Int(STREAM_MUSIC)],
            )?
            .i()?;
        let fixed = env
            .call_method(&manager, jni_str!("isVolumeFixed"), jni_sig!("()Z"), &[])?
            .z()?;
        Ok(Some((steps, fixed)))
    })
    .flatten()
}
