//! Meta Horizon Store builds only (feature `store`): the entitlement check the store
//! requires within 10 seconds of launch, and where its builds are updated.
//!
//! The Meta Platform SDK is loaded at run time with `dlopen` from the APK's own
//! libraries (`libovrplatformloader.so`, added by `build-apk.sh` for store builds), so
//! nothing of it is linked into Prostir. Licensing: see LICENSE-EXCEPTION.md.

use std::ffi::{CString, c_char, c_void};

use log::{info, warn};

use super::{Store, StoreCheck, UpdateHint};

/// The Meta Horizon Store: the app is checked to be the user's at launch, and updated
/// from the headset's library.
pub struct MetaStore {
    /// `None` when the check could not even start: that counts as not owned.
    check: Option<Entitlement>,
}

impl MetaStore {
    /// Starts the check.
    pub fn start(app: &android_activity::AndroidApp) -> Self {
        Self {
            check: Entitlement::start(app),
        }
    }
}

impl Store for MetaStore {
    fn poll(&mut self) -> StoreCheck {
        match self.check.as_mut().map_or(Some(false), Entitlement::poll) {
            None => StoreCheck::Waiting,
            Some(true) => StoreCheck::Owned,
            Some(false) => StoreCheck::NotOwned { store: NAME },
        }
    }

    fn update_hint(&self) -> UpdateHint {
        UpdateHint {
            how: "store-update-library",
            place: NAME,
        }
    }
}

/// The store's name, as the person knows it.
const NAME: &str = "Meta Horizon Store";

/// Prostir's application id on the Meta Horizon Developer Dashboard.
const APP_ID: &str = match option_env!("PROSTIR_HORIZON_APP_ID") {
    Some(id) => id,
    None => "1489021784287834",
};
/// `ovrMessage_Entitlement_GetIsViewerEntitled` (OVR_MessageType.h).
const ENTITLEMENT_MESSAGE: u32 = 0x186B_58B1;

type Init = unsafe extern "C" fn(*const c_char, *mut c_void, *mut c_void) -> i32;
type Request = unsafe extern "C" fn() -> u64;
type Pop = unsafe extern "C" fn() -> *mut c_void;
type GetType = unsafe extern "C" fn(*const c_void) -> u32;
type IsError = unsafe extern "C" fn(*const c_void) -> bool;
type Free = unsafe extern "C" fn(*mut c_void);

/// The running check: poll it every frame until it answers.
struct Entitlement {
    pop: Pop,
    get_type: GetType,
    is_error: IsError,
    free: Free,
    answer: Option<bool>,
}

fn symbol<T>(lib: *mut c_void, name: &str) -> Option<T> {
    let name = CString::new(name).ok()?;
    // SAFETY: `lib` is a live handle; the symbol has the signature `T` per the SDK.
    let ptr = unsafe { libc::dlsym(lib, name.as_ptr()) };
    (!ptr.is_null()).then(|| {
        // SAFETY: `T` is a function pointer type of the same size as `ptr`, matching
        // the exported symbol's C signature.
        unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) }
    })
}

impl Entitlement {
    /// Initializes the platform and asks whether the user owns the app. `None` means
    /// the check could not even start: treat it as a failure.
    fn start(app: &android_activity::AndroidApp) -> Option<Self> {
        let name = CString::new("libovrplatformloader.so").ok()?;
        // SAFETY: plain dlopen; the handle is kept for the life of the process.
        let lib = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            warn!("store: libovrplatformloader.so is missing");
            return None;
        }
        let init: Init = symbol(lib, "ovr_PlatformInitializeAndroid")?;
        let request: Request = symbol(lib, "ovr_Entitlement_GetIsViewerEntitled")?;
        let this = Self {
            pop: symbol(lib, "ovr_PopMessage")?,
            get_type: symbol(lib, "ovr_Message_GetType")?,
            is_error: symbol(lib, "ovr_Message_IsError")?,
            free: symbol(lib, "ovr_FreeMessage")?,
            answer: None,
        };
        // The SDK wants the Activity. `ndk_context` holds the Application (that is
        // what android-activity puts there), so the activity comes from the app.
        // SAFETY: the VM comes from the running NativeActivity.
        let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
        let app_id = CString::new(APP_ID).ok()?;
        let result = vm
            .attach_current_thread(|env| -> jni::errors::Result<i32> {
                // SAFETY: valid app id, the running activity's object and the JNIEnv
                // of this thread.
                Ok(unsafe { init(app_id.as_ptr(), app.activity_as_ptr(), env.get_raw().cast()) })
            })
            .inspect_err(|e| warn!("store: {e}"))
            .ok()?;
        if result != 0 {
            warn!("store: platform initialization failed ({result})");
            return None;
        }
        // SAFETY: the platform is initialized.
        unsafe { request() };
        info!("store: entitlement check started");
        Some(this)
    }

    /// `Some(true)` once the user is entitled, `Some(false)` if not.
    fn poll(&mut self) -> Option<bool> {
        while self.answer.is_none() {
            // SAFETY: the SDK owns the message until it is freed below.
            let msg = unsafe { (self.pop)() };
            if msg.is_null() {
                break;
            }
            // SAFETY: `msg` is a live message from `pop`, freed exactly once here.
            unsafe {
                if (self.get_type)(msg) == ENTITLEMENT_MESSAGE {
                    self.answer = Some(!(self.is_error)(msg));
                    info!("store: entitled: {:?}", self.answer);
                }
                (self.free)(msg);
            }
        }
        self.answer
    }
}
