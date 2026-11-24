//! libadrenotools integration (BSD-2-Clause, https://github.com/bylaws/libadrenotools).
//!
//! A custom Adreno driver (Turnip, `libvulkan_freedreno.so`) is **not** loaded by
//! `dlopen`-ing the driver `.so` into this process: the Android Vulkan loader
//! (`/system/lib64/libvulkan.so`) owns an isolated linker namespace and would never
//! pick that ICD up. The supported path is libadrenotools:
//!
//! 1. `adrenotools_open_libvulkan(...)` creates a private linker namespace, loads a
//!    *fresh copy* of `libvulkan.so` into it (soname-patched to a unique name, from
//!    a memfd on API ≥ 29) together with the hook libraries (`libmain_hook.so`
//!    etc.). Inside that namespace the hook intercepts the loader's own
//!    `android_dlopen_ext`/`android_load_sphal_library`, so when the loader tries to
//!    load its vendor ICD it gets the **chosen driver** instead.
//! 2. The function returns a handle to that loader. `vkGetInstanceProcAddr` resolved
//!    from the handle is the *only* way to reach an instance backed by the driver,
//!    so both this runtime (`vk.rs`) and DXVK (`graphics.rs`) must use it.
//!
//! Prerequisites that the APK guarantees: `useLegacyPackaging = true` (so the hook
//! libraries are extracted into `nativeLibraryDir`, not read from the APK), and the
//! chosen driver `.so` living in app-**private** storage (never sd-card).
//!
//! With no driver chosen, or if any of this fails, everything stays on the system
//! driver: `instance_proc_addr()` returns `None` and callers fall back.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};

const RTLD_NOW: i32 = 2;
const RTLD_LOCAL: i32 = 0;

/// `ADRENOTOOLS_DRIVER_CUSTOM` (include/adrenotools/priv.h).
const ADRENOTOOLS_DRIVER_CUSTOM: i32 = 1 << 0;

#[derive(Clone, Copy)]
struct Loader {
    /// The loader handle returned by `adrenotools_open_libvulkan` (kept alive).
    handle: *mut c_void,
    /// `vkGetInstanceProcAddr` resolved from that handle.
    gipa: usize,
}

// The handle/gipa are process-global and only written under the mutex; nothing
// else is shared across threads here.
unsafe impl Send for Loader {}

fn loader() -> &'static Mutex<Option<Loader>> {
    static LOADER: OnceLock<Mutex<Option<Loader>>> = OnceLock::new();
    LOADER.get_or_init(|| Mutex::new(None))
}

#[repr(C)]
struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> i32;
}

fn last_dlerror() -> String {
    unsafe {
        let e = dlerror();
        if e.is_null() {
            "unknown".to_string()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

/// A stable address inside this runtime `.so` for `dladdr`. `#[no_mangle]` keeps
/// it from being merged away, so the returned path is always the runtime library.
#[no_mangle]
pub extern "C" fn gow_adrenotools_anchor() {}

/// The directory this runtime's own `.so` was loaded from — which on Android is
/// exactly `ApplicationInfo.nativeLibraryDir`, the `hookLibDir` libadrenotools
/// demands. Resolved with `dladdr` so no JNI round-trip is needed.
fn self_lib_dir() -> Option<String> {
    unsafe {
        let mut info: DlInfo = std::mem::zeroed();
        if dladdr(gow_adrenotools_anchor as *const c_void, &mut info) == 0 || info.dli_fname.is_null() {
            return None;
        }
        let path = CStr::from_ptr(info.dli_fname).to_string_lossy().into_owned();
        path.rsplit_once('/').map(|(dir, _)| dir.to_string())
    }
}

/// Open the Vulkan loader with `driver_name` (a `.so` in `driver_dir`) swapped in.
///
/// `driver_dir` must be app-private storage; a trailing `/` is added here because
/// libadrenotools concatenates `customDriverDir + customDriverName` with no
/// separator. Returns true on success; the resulting loader is then reachable
/// through [`instance_proc_addr`].
pub fn open(driver_dir: &str, driver_name: &str) -> bool {
    if driver_dir.is_empty() || driver_name.is_empty() {
        return false;
    }
    if let Some(l) = *loader().lock().unwrap() {
        if !l.handle.is_null() {
            crate::glog!("[gow.adrenotools] loader already open; keeping it");
            return true;
        }
    }

    let hook_dir = match self_lib_dir() {
        Some(d) => d,
        None => {
            crate::glog!("[gow.adrenotools] cannot resolve nativeLibraryDir (dladdr failed)");
            return false;
        }
    };

    // Remember the choice for diagnostics / other layers that read the env.
    std::env::set_var("GOW_DRIVER_DIR", driver_dir);
    std::env::set_var("GOW_DRIVER_NAME", driver_name);
    crate::glog!(
        "[gow.adrenotools] opening loader: driver={driver_dir}/{driver_name} hookLibDir={hook_dir}"
    );

    unsafe {
        // libadrenotools ships in the APK next to the runtime; try the exact path
        // first (nativeLibraryDir is not on the default search path for absolute
        // dlopen by name on all devices).
        let candidates = [
            format!("{hook_dir}/libadrenotools.so"),
            "libadrenotools.so".to_string(),
        ];
        let mut adr = std::ptr::null_mut();
        for c in &candidates {
            let Ok(cs) = CString::new(c.as_str()) else { continue };
            adr = dlopen(cs.as_ptr(), RTLD_NOW | RTLD_LOCAL);
            if !adr.is_null() {
                crate::glog!("[gow.adrenotools] loaded {c}");
                break;
            }
            crate::glog!("[gow.adrenotools] dlopen {c} failed: {}", last_dlerror());
        }
        if adr.is_null() {
            crate::glog!(
                "[gow.adrenotools] libadrenotools.so not in the APK; staying on the system Vulkan loader"
            );
            return false;
        }

        let sym = dlsym(adr, b"adrenotools_open_libvulkan\0".as_ptr() as *const c_char);
        if sym.is_null() {
            crate::glog!("[gow.adrenotools] adrenotools_open_libvulkan symbol missing");
            return false;
        }
        type OpenLibvulkan = unsafe extern "C" fn(
            i32,
            i32,
            *const c_char,
            *const c_char,
            *const c_char,
            *const c_char,
            *const c_char,
            *mut *mut c_void,
        ) -> *mut c_void;
        let f: OpenLibvulkan = std::mem::transmute(sym);

        let hook = match CString::new(hook_dir.as_str()) {
            Ok(c) => c,
            Err(_) => return false,
        };
        // libadrenotools concatenates dir + name, so the separator is mandatory.
        let cdir = match CString::new(format!("{}/", driver_dir.trim_end_matches('/'))) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let cname = match CString::new(driver_name) {
            Ok(c) => c,
            Err(_) => return false,
        };

        // tmpLibDir is only used on API < 29; minSdk is 29 so memfd is available.
        let handle = f(
            RTLD_NOW | RTLD_LOCAL,
            ADRENOTOOLS_DRIVER_CUSTOM,
            std::ptr::null(),
            hook.as_ptr(),
            cdir.as_ptr(),
            cname.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        if handle.is_null() {
            crate::glog!(
                "[gow.adrenotools] adrenotools_open_libvulkan failed (dlerror: {}); staying on the system Vulkan loader",
                last_dlerror()
            );
            return false;
        }

        let gipa = dlsym(handle, b"vkGetInstanceProcAddr\0".as_ptr() as *const c_char);
        if gipa.is_null() {
            crate::glog!("[gow.adrenotools] loader has no vkGetInstanceProcAddr; ignoring driver");
            return false;
        }

        crate::glog!(
            "[gow.adrenotools] custom driver active: {driver_name} (loader={handle:p}, vkGetInstanceProcAddr={gipa:p})"
        );
        *loader().lock().unwrap() = Some(Loader {
            handle,
            gipa: gipa as usize,
        });
        true
    }
}

/// `vkGetInstanceProcAddr` for the custom-driver loader, or `None` when the
/// system Vulkan loader should be used instead.
pub fn instance_proc_addr() -> Option<usize> {
    loader()
        .lock()
        .unwrap()
        .as_ref()
        .map(|l| l.gipa)
        .filter(|&p| p != 0)
}

/// Whether a custom driver was opened through libadrenotools.
pub fn is_active() -> bool {
    instance_proc_addr().is_some()
}

/// Open the driver named by `GOW_DRIVER_DIR`/`GOW_DRIVER_NAME` (set by the JNI
/// layer, or by the launcher in a future headless path). No-op when unset.
pub fn open_from_env() -> bool {
    let dir = std::env::var("GOW_DRIVER_DIR").unwrap_or_default();
    let name = std::env::var("GOW_DRIVER_NAME").unwrap_or_default();
    if dir.is_empty() || name.is_empty() {
        return false;
    }
    open(&dir, &name)
}
