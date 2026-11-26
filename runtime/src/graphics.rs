//! Graphics bridge: routes the game's D3D11/DXGI/D3DCompiler IAT calls to the
//! loaded backend (DXVK Native in phase 1; a native Rust Vulkan backend in
//! phase 2). The backend is a host shared library exposing the Windows D3D11
//! entry points with the Microsoft x64 calling convention DXVK Native provides.
//!
//! The loaded libraries live in **process-global** state (not per-`GraphicsBridge`
//! instance) so that the JNI surface callbacks (`runtime/src/jni.rs`) can push the
//! `ANativeWindow` and set `DXVK_WSI_DRIVER` into the very same DXVK libraries the
//! import resolver later calls through. `GraphicsBridge` stays a zero-sized handle
//! so the existing `ImportResolver` ownership in `win32.rs` is unchanged.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The D3D11/DXGI bootstrap entry points the game imports (measured from its
/// IAT). Everything else is reached through the returned COM vtables, which the
/// backend (DXVK Native) implements.
///
/// Note: `d3dcompiler_47.dll!D3DReflect` is **not** a DXVK export — DXVK does not
/// implement reflection. It is served by our own Rust DXBC reflection object in
/// `runtime/src/dxbc.rs` (see `win32.rs`), so it is listed here only so the whole
/// bootstrap set is documented in one place; it is never routed to a DXVK `.so`.
pub const D3D_BOOTSTRAP: &[(&str, &str)] = &[
    ("d3d11.dll", "D3D11CreateDevice"),
    ("dxgi.dll", "CreateDXGIFactory1"),
    ("d3dcompiler_47.dll", "D3DReflect"),
];

/// Process-global backend handles, keyed by lowercased dll name (`dxgi.dll`).
/// Stored as `usize` so the map is `Send + Sync` without unsafe impls.
fn handles() -> &'static Mutex<HashMap<String, usize>> {
    static HANDLES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolved symbol addresses, keyed by `dll!func`.
fn symbols() -> &'static Mutex<HashMap<String, usize>> {
    static SYMBOLS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    SYMBOLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Zero-sized facade over the global backend registry. Keeping the type and its
/// method signatures identical means `win32.rs` (which owns one inside
/// `ImportResolver`) does not change.
#[derive(Default)]
pub struct GraphicsBridge;

impl GraphicsBridge {
    pub fn new() -> Self {
        Self
    }

    /// dlopen a backend shared library (e.g. `libdxvk_d3d11.so`) if not already
    /// loaded. `RTLD_GLOBAL` so the two DXVK libs share one loaded copy and their
    /// inter-dependencies (`libdxvk_d3d11.so` -> `libdxvk_dxgi.so`) resolve.
    pub fn load_library(&self, dll: &str, path: &str) -> bool {
        let key = dll.to_ascii_lowercase();
        if handles().lock().unwrap().contains_key(&key) {
            return true;
        }
        unsafe {
            let cpath = match std::ffi::CString::new(path) {
                Ok(c) => c,
                Err(_) => return false,
            };
            let h = dlopen(cpath.as_ptr(), RTLD_NOW | RTLD_GLOBAL);
            if h.is_null() {
                let err = dlerror();
                let msg = if err.is_null() {
                    "unknown".to_string()
                } else {
                    std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
                };
                crate::glog!("[gow.gfx] dlopen failed for {path}: {msg}");
                return false;
            }
            handles().lock().unwrap().insert(key, h as usize);
            crate::glog!("[gow.gfx] loaded backend for {dll} from {path}");
            // Register the library's executable + data ranges so the dispatcher
            // recognizes COM vtable methods that live inside it (DXVK returns
            // objects whose virtual methods are host code) and calls them
            // instead of recording a MISSCALL.
            register_host_ranges(h as usize, path);
            true
        }
    }

    /// Raw dlopen handle for a dll key (e.g. `"dxgi.dll"`), if loaded.
    pub fn handle(&self, dll: &str) -> Option<*mut std::ffi::c_void> {
        handles()
            .lock()
            .unwrap()
            .get(&dll.to_ascii_lowercase())
            .map(|&h| h as *mut std::ffi::c_void)
    }

    /// Resolve a D3D entry point from the loaded backend.
    pub fn resolve(&self, dll: &str, func: &str) -> Option<usize> {
        let key = format!("{}!{}", dll.to_ascii_lowercase(), func);
        if let Some(&p) = symbols().lock().unwrap().get(&key) {
            return Some(p);
        }
        let handle = self.handle(dll)?;
        unsafe {
            let cname = std::ffi::CString::new(func).ok()?;
            let sym = dlsym(handle, cname.as_ptr());
            if sym.is_null() {
                return None;
            }
            let addr = sym as usize;
            symbols().lock().unwrap().insert(key, addr);
            Some(addr)
        }
    }

    pub fn is_loaded(&self) -> bool {
        !handles().lock().unwrap().is_empty()
    }

    /// `libdxvk_d3d11.so` / `libdxvk_dxgi.so` from the app's native library dir.
    ///
    /// dxgi is loaded first: `libdxvk_d3d11.so` has `DT_NEEDED libdxvk_dxgi.so`,
    /// so loading its dependency explicitly first keeps one shared copy.
    pub fn autoload(&self, native_lib_dir: &str) -> bool {        // DXVK reads this inside wsi::init() and throws if it is unset/unmatched.
        // Must be set before the first D3D11/DXGI call.
        self.set_wsi_driver("Android");
        // On-screen HUD with FPS and frame times (DXVK reads DXVK_HUD at init).
        self.set_hud("fps,frametimes,gpuload,devinfo");
        // Capture DXVK's own log to a file in the game folder so a failed
        // CreateDXGIFactory1 (device: 0x80004005) explains itself instead of
        // only showing the generic E_FAIL. Harmless env vars.
        self.set_env_default("DXVK_LOG_LEVEL", "info");
        self.set_env_default("DXVK_LOG_PATH", "/storage/emulated/0/god/Port");
        self.set_env_default("DXVK_LOG", "1");
        self.set_env_default("DXVK_DEBUG", "markers");

        let dxgi = format!("{native_lib_dir}/libdxvk_dxgi.so");
        let d3d11 = format!("{native_lib_dir}/libdxvk_d3d11.so");
        let mut any = false;
        if std::path::Path::new(&dxgi).exists() {
            any |= self.load_library("dxgi.dll", &dxgi);
        }
        if std::path::Path::new(&d3d11).exists() {
            any |= self.load_library("d3d11.dll", &d3d11);
        }
        if !any {
            crate::glog!(
                "[gow.gfx] no DXVK Native libraries found in {native_lib_dir} (APK is runtime-only)"
            );
        }
        // Point DXVK's Vulkan at the libadrenotools loader when a custom driver was
        // opened. Must happen before the first D3D11/DXGI call, which is where DXVK
        // lazily loads `libvulkan.so` and constructs its loader.
        self.set_vulkan_loader();
        any
    }

    /// Hand DXVK the `vkGetInstanceProcAddr` of the loader libadrenotools opened
    /// for the chosen driver, so the whole renderer runs on that driver instead of
    /// the process-wide system loader.
    ///
    /// DXVK resolves every Vulkan entry point through one `vkGetInstanceProcAddr`
    /// (its `LibraryFn::sym`), and the patch in
    /// `tools/patches/0005-dxvk-adrenotools-loader-override.patch` adds the setter
    /// `dxvk_vulkan_set_get_instance_proc_addr`. Both `.so`s carry their own copy
    /// of that pointer, exactly like `dxvk_android_set_window`, so it is pushed to
    /// both. No driver ⇒ nothing to do: DXVK loads `libvulkan.so` itself.
    pub fn set_vulkan_loader(&self) -> bool {
        let Some(gipa) = crate::adrenotools::instance_proc_addr() else {
            crate::glog!("[gow.gfx] no custom driver; DXVK uses the system Vulkan loader");
            return false;
        };
        let mut any = false;
        for dll in ["dxgi.dll", "d3d11.dll"] {
            let Some(handle) = self.handle(dll) else { continue };
            let sym = unsafe {
                dlsym(
                    handle,
                    b"dxvk_vulkan_set_get_instance_proc_addr\0".as_ptr()
                        as *const std::os::raw::c_char,
                )
            };
            if sym.is_null() {
                crate::glog!(
                    "[gow.gfx] {dll}: dxvk_vulkan_set_get_instance_proc_addr missing (DXVK built without tools/patches/0005?)"
                );
                continue;
            }
            type SetGipa = unsafe extern "C" fn(*mut std::ffi::c_void);
            let f: SetGipa = unsafe { std::mem::transmute(sym) };
            unsafe { f(gipa as *mut std::ffi::c_void) };
            crate::glog!(
                "[gow.gfx] {dll}: vkGetInstanceProcAddr=0x{gipa:x} pushed (adrenotools loader)"
            );
            any = true;
        }
        any
    }

    /// Set `DXVK_WSI_DRIVER` before DXVK loads, so `wsi::init()` selects our
    /// Android backend (P3/P4). Harmless if the backend is not present.
    pub fn set_wsi_driver(&self, driver: &str) {
        // Called once from autoload() before any DXVK call and before the guest
        // spawns threads, so there is no concurrent env reader.
        if std::env::var_os("DXVK_WSI_DRIVER").is_none() {
            std::env::set_var("DXVK_WSI_DRIVER", driver);
            crate::glog!("[gow.gfx] DXVK_WSI_DRIVER={driver}");
        }
    }

    /// Configure the DXVK HUD (on-screen overlay). `spec` follows DXVK's syntax,
    /// e.g. `"fps,frametimes,gpuload,devinfo"`. Set before DXVK loads.
    pub fn set_hud(&self, spec: &str) {
        if std::env::var_os("DXVK_HUD").is_none() {
            std::env::set_var("DXVK_HUD", spec);
            crate::glog!("[gow.gfx] DXVK_HUD={spec}");
        }
    }

    /// Set an env var if it is not already present (DXVK reads config from env).
    pub fn set_env_default(&self, key: &str, val: &str) {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, val);
            crate::glog!("[gow.gfx] {key}={val}");
        }
    }

    /// Push the `ANativeWindow` to **both** DXVK libraries.
    ///
    /// Each `.so` statically links its own WSI driver and therefore keeps its own
    /// `g_androidWindow` (see `docs/DXVK_ANDROID.md` §2.4): dxgi owns the surface
    /// creation, d3d11 queries window/monitor. Returns true only if at least the
    /// dxgi library accepted the window; a missing symbol is logged loudly, never
    /// silently ignored.
    pub fn set_android_window(&self, window: *mut std::ffi::c_void) -> bool {
        // A null window is a valid push: it makes DXVK report the display as
        // minimized/occluded and fail createSurface loudly instead of presenting
        // to a destroyed ANativeWindow.
        let mut dxgi_ok = false;
        for dll in ["dxgi.dll", "d3d11.dll"] {
            let Some(handle) = self.handle(dll) else { continue };
            let sym = unsafe {
                dlsym(
                    handle,
                    b"dxvk_android_set_window\0".as_ptr() as *const std::os::raw::c_char,
                )
            };
            if sym.is_null() {
                crate::glog!(
                    "[gow.gfx] {dll}: dxvk_android_set_window missing (DXVK built without P5?)"
                );
                continue;
            }
            type SetWindow = unsafe extern "C" fn(*mut std::ffi::c_void);
            let f: SetWindow = unsafe { std::mem::transmute(sym) };
            unsafe { f(window) };
            crate::glog!("[gow.gfx] {dll}: ANativeWindow={window:p} pushed");
            if dll == "dxgi.dll" {
                dxgi_ok = true;
            }
        }
        if !dxgi_ok && !self.is_loaded() {
            // Not an error before the DXVK libs are loaded; surfaceCreated can
            // fire before nativeLoadAndRun. Keep it quiet.
        } else if !dxgi_ok {
            crate::glog!("[gow.gfx] no DXVK library accepted the ANativeWindow (graphics will fail)");
        }
        dxgi_ok
    }

    /// Run `addr` with up to 10 args (Microsoft x64: first 4 in RCX/RDX/R8/R9,
    /// the rest on the shadow-space stack). Returns RAX.
    ///
    /// DXVK's widest bootstrap entry point is `D3D11CreateDevice` with 10
    /// parameters, so 10 slots is exactly enough.
    pub unsafe fn call_addr(addr: usize, args: [u64; 10]) -> u64 {
        type F = unsafe extern "system" fn(u64, u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
        let f: F = std::mem::transmute(addr);
        f(
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8], args[9],
        )
    }

    /// Forward a guest COM call to a host function, taking all arguments from
    /// the guest register file and stack per the Microsoft x64 convention.
    ///
    /// Args 1–4 live in `rcx,rdx,r8,r9`; args 5–10 at `[rsp+0x28]`..`[rsp+0x50]`
    /// (the 32-byte shadow space precedes them).
    pub unsafe fn call_addr_raw(addr: usize, c: *mut crate::dispatch::GowCpu) -> u64 {
        let cpu = &*c;
        let rsp = cpu.gpr.rsp;
        let a = [
            cpu.gpr.rcx,
            cpu.gpr.rdx,
            cpu.gpr.r8,
            cpu.gpr.r9,
            *((rsp as *const u8).add(0x28) as *const u64),
            *((rsp as *const u8).add(0x30) as *const u64),
            *((rsp as *const u8).add(0x38) as *const u64),
            *((rsp as *const u8).add(0x40) as *const u64),
            *((rsp as *const u8).add(0x48) as *const u64),
            *((rsp as *const u8).add(0x50) as *const u64),
        ];
        Self::call_addr(addr, a)
    }
}

const RTLD_NOW: i32 = 2;
const RTLD_GLOBAL: i32 = 0x100;

/// Register every executable mapping of the shared object at `path` so the guest
/// dispatcher can call COM vtable methods that live inside it. Uses
/// `/proc/self/maps`, matching lines whose file path ends with the basename.
fn register_host_ranges(_handle: usize, path: &str) {
    let base = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else { return };
    for line in maps.lines() {
        // "<start>-<end> <perms> <off> <dev> <inode> <path>"
        let mut it = line.split_whitespace();
        let (Some(range), Some(perms)) = (it.next(), it.next()) else { continue };
        if !perms.starts_with('r') || !perms.contains('x') {
            continue; // only executable segments hold callable code
        }
        let Some(file) = it.nth(3) else { continue }; // skip off, dev, inode
        if !file.ends_with(&base) {
            continue;
        }
        let Some((a, b)) = range.split_once('-') else { continue };
        if let (Ok(lo), Ok(hi)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) {
            crate::hostcall::register_host_code(lo, hi);
        }
    }
}

extern "C" {
    fn dlopen(path: *const std::os::raw::c_char, mode: i32) -> *mut std::ffi::c_void;
    fn dlsym(
        handle: *mut std::ffi::c_void,
        sym: *const std::os::raw::c_char,
    ) -> *mut std::ffi::c_void;
    fn dlerror() -> *const std::os::raw::c_char;
}
