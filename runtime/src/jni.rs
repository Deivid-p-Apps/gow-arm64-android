//! Android JNI entry points. The APK ships the runtime (dispatcher, Win32/NT
//! layer, D3D11->Vulkan bridge, audio, input). The recompiled game code is a
//! separate .so the user builds locally from their own files and the app loads
//! at runtime.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use crate::dispatch::{install_registry, GowCpu, GowFuncEntry};

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeInit(
    env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    game_dir: *mut std::ffi::c_void,
) -> c_int {
    let dir = unsafe { jstring_to_string(env, game_dir) };
    crate::glog!("[gow] nativeInit game_dir={dir}");
    0
}

/// Load the recompiled game library and register its function table, then run
/// from the PE entry point.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeRun(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    lib_path: *const c_char,
    image_base: u64,
    image_size: u64,
    entry_rva: u32,
) -> c_int {
    let path = unsafe { std::ffi::CStr::from_ptr(lib_path).to_string_lossy().into_owned() };
    let _ = CString::new("").unwrap();
    crate::glog!("[gow] loading recompiled game: {path}");

    // dlopen the user-built game .so (arm64), fetch its table.
    let handle = unsafe {
        let cpath = CString::new(path.as_str()).unwrap();
        dlopen(cpath.as_ptr(), RTLD_NOW)
    };
    if handle.is_null() {
        crate::glog!("[gow] dlopen failed");
        return -1;
    }    let table_sym = unsafe { dlsym(handle, b"gow_func_table\0".as_ptr() as *const c_char) };
    let count_sym = unsafe { dlsym(handle, b"gow_func_count\0".as_ptr() as *const c_char) };
    if table_sym.is_null() || count_sym.is_null() {
        crate::glog!("[gow] game table symbols missing");
        return -2;
    }
    let count = unsafe { *(count_sym as *const u32) } as usize;
    let table = unsafe { std::slice::from_raw_parts(table_sym as *const GowFuncEntry, count) };
    let mut regs = Vec::with_capacity(count);
    for e in table {
        if let Some(f) = e.fnptr { regs.push((e.rva, f)); }
    }
    crate::glog!("[gow] registered {} functions", regs.len());
    install_registry(regs);

    let mut cpu = GowCpu::new(image_base, image_size as usize);
    // The window/graphics/audio layers are started here in later milestones.
    unsafe { crate::dispatch::gow_run(cpu.as_mut(), entry_rva) };
    0
}

/// Deliver an Android key event to the input layer.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeKey(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    source: i32,
    keycode: i32,
    down: bool,
) {
    crate::input::global().on_key_event(source, keycode, down);
}

/// Deliver an Android motion (joystick axis) event to the input layer.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeMotion(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    axis: i32,
    value: f32,
) {
    crate::input::global().on_motion(axis, value);
}

/// Deliver a touch event for the on-screen controller overlay.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeTouch(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    pointer_id: i32,
    x: f32,
    y: f32,
    down: bool,
) {
    crate::input::global().on_touch(pointer_id, x, y, down);
}

/// Load the Turnip (or other) Vulkan driver from an adrenotools package in the
/// game's drivers directory into the app's private storage, then make it the
/// Vulkan driver the process uses. Returns 0 on success.
///
/// An adrenotools package is a zip with `meta.json` + a driver `.so`
/// (commonly `libvulkan_freedreno.so`). Android cannot dlopen from external
/// storage, so the driver is copied into `priv_dir` first. The Vulkan loader
/// picks it up via the driver search path set here.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeLoadDriver(
    env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    drivers_dir: *mut std::ffi::c_void,
    priv_dir: *mut std::ffi::c_void,
) -> c_int {
    let ddir = unsafe { jstring_to_string(env, drivers_dir) };
    let pdir = unsafe { jstring_to_string(env, priv_dir) };
    crate::glog!("[gow.driver] scanning {ddir}");

    // Prefer an already-extracted driver .so, else the first package zip.
    let candidates = ["libvulkan_freedreno.so", "libvulkan.so", "vulkan.purple.so"];
    for name in candidates {
        let p = format!("{ddir}/{name}");
        if std::path::Path::new(&p).exists() {
            return stage_and_load_driver(&p, &pdir);
        }
    }
    // Otherwise look for a driver inside the first .zip/.adpkg in the folder.
    match std::fs::read_dir(&ddir) {
        Ok(rd) => {
            let mut seen = 0;
            for e in rd.flatten() {
                seen += 1;
                let path = e.path();
                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                crate::glog!("[gow.driver] entry {} ext={ext}", path.display());
                if ext == "zip" || ext == "adpkg" {
                    match extract_driver_from_zip(&path, &pdir) {
                        Some(so) => return stage_and_load_driver(&so, &pdir),
                        None => crate::glog!("[gow.driver] no .so inside {}", path.display()),
                    }
                }
            }
            crate::glog!("[gow.driver] scanned {seen} entries, none usable");
        }
        Err(e) => crate::glog!("[gow.driver] cannot read {ddir}: {e}"),
    }
    crate::glog!("[gow.driver] no driver package found; using system Vulkan");
    -1
}

fn stage_and_load_driver(src: &str, priv_dir: &str) -> c_int {
    let dst = format!("{priv_dir}/libvulkan_freedreno.so");
    if let Err(e) = std::fs::copy(src, &dst) {
        crate::glog!("[gow.driver] copy failed: {e}");
        return -1;
    }
    let cpath = CString::new(dst.clone()).unwrap();
    let h = unsafe { dlopen(cpath.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
    if h.is_null() {
        let err = unsafe { dlerror() };
        crate::glog!("[gow.driver] dlopen {dst} failed: {:?}", err);
        return -2;
    }
    crate::glog!("[gow.driver] Turnip driver loaded from {dst}");
    0
}

/// Extract the driver `.so` from an adrenotools zip into `priv_dir`.
fn extract_driver_from_zip(zip_path: &std::path::Path, priv_dir: &str) -> Option<String> {
    let f = match std::fs::File::open(zip_path) {
        Ok(f) => f,
        Err(e) => { crate::glog!("[gow.driver] open {} failed: {e}", zip_path.display()); return None; }
    };
    let mut archive = match zip::ZipArchive::new(f) {
        Ok(a) => a,
        Err(e) => { crate::glog!("[gow.driver] zip parse {} failed: {e}", zip_path.display()); return None; }
    };
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        let name = entry.name().to_string();
        if name.ends_with(".so") {
            let out = format!("{priv_dir}/{}", name.rsplit('/').next().unwrap_or("libvulkan.so"));
            let mut of = std::fs::File::create(&out).ok()?;
            std::io::copy(&mut entry, &mut of).ok()?;
            crate::glog!("[gow.driver] extracted {name} -> {out}");
            return Some(out);
        }
    }
    None
}

/// Load the manifest (import list) and install the Win32 import resolver.
/// Prefers the copy embedded in the game .so (`gow_manifest_json`), else a
/// standalone `manifest.json` next to the library or in the game dir.
fn install_import_resolver(lib_handle: *mut std::ffi::c_void, pdir: &str, gdir: &str) {
    let mut json = None;
    unsafe {
        let sym = dlsym(lib_handle, b"gow_manifest_json\0".as_ptr() as *const c_char);
        if !sym.is_null() {
            json = Some(std::ffi::CStr::from_ptr(sym as *const c_char).to_string_lossy().into_owned());
        }
    }
    if json.is_none() {
        let cands = [format!("{gdir}/manifest.json"), format!("{pdir}/manifest.json")];
        json = cands.iter().find_map(|p| std::fs::read_to_string(p).ok());
    }
    let mut res = crate::win32::ImportResolver::new(gdir);
    match json {
        Some(j) => match res.load_manifest(&j) {
            Ok(_) => crate::glog!("[gow] import resolver installed ({} slots)", res.slot_count()),
            Err(e) => crate::glog!("[gow] manifest parse error: {e}"),
        },
        None => crate::glog!("[gow] WARNING: no manifest found; imports will not resolve"),
    }
    crate::dispatch::set_import_resolver(std::sync::Arc::new(res));
}

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;
const RTLD_GLOBAL: c_int = 0x100;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, sym: *const c_char) -> *mut std::ffi::c_void;
    fn dlerror() -> *const c_char;
}

/// JNI `GetStringUTFChars` / `ReleaseStringUTFChars` through the JNIEnv table
/// (indices 169/170), since native methods receive `jstring`, not `char*`.
unsafe fn jstring_to_string(env: *mut std::ffi::c_void, s: *mut std::ffi::c_void) -> String {
    if env.is_null() || s.is_null() {
        return String::new();
    }
    // JNIEnv is a pointer to a table of function pointers.
    let table = *(env as *const *const usize);
    let get = *table.add(169) as *const ();
    let release = *table.add(170) as *const ();
    if get.is_null() || release.is_null() {
        return String::new();
    }
    type GetStr = unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *mut u8) -> *const c_char;
    type RelStr = unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const c_char);
    let get: GetStr = std::mem::transmute(get);
    let release: RelStr = std::mem::transmute(release);
    let mut is_copy: u8 = 0;
    let ptr = get(env, s, &mut is_copy);
    if ptr.is_null() {
        return String::new();
    }
    let owned = std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned();
    release(env, s, ptr);
    owned
}

/// Discover the recompiled game library next to the game files, copy it into the
/// app's private directory (Android cannot dlopen from external storage) and run
/// it from the PE entry point. `priv_dir` is the app's private files path.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeLoadAndRun(
    env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    game_dir: *mut std::ffi::c_void,
    priv_dir: *mut std::ffi::c_void,
    image_base: u64,
    image_size: u64,
    entry_rva: u32,
) -> c_int {
    let gdir = unsafe { jstring_to_string(env, game_dir) };
    let pdir = unsafe { jstring_to_string(env, priv_dir) };
    crate::glog!("[gow] nativeLoadAndRun gdir={gdir} pdir={pdir}");

    // If the game library ships inside the APK it lives in the app's
    // nativeLibraryDir; prefer it, then fall back to the game directory.
    let candidates = [
        format!("{pdir}/libgow_game.so"),
        format!("{gdir}/libgow_game.so"),
        format!("{gdir}/drivers/libgow_game.so"),
    ];
    let src = match candidates.iter().find(|p| std::path::Path::new(p).exists()) {
        Some(p) => p.clone(),
        None => {
            crate::glog!("[gow] no libgow_game.so found (build it with tools/build_game_so.sh)");
            return -3;
        }
    };
    crate::glog!("[gow] found game library: {src}");
    // If it is already in nativeLibraryDir it is loadable directly.
    let load_path = if src.starts_with(&pdir) && pdir.contains("/lib") {
        src
    } else {
        let dst = format!("{pdir}/libgow_game.so");
        if let Err(e) = std::fs::copy(&src, &dst) {
            crate::glog!("[gow] copy {src} -> {dst} failed: {e}");
            return -4;
        }
        crate::glog!("[gow] staged: {dst}");
        dst
    };
    run_game_library(CString::new(load_path).unwrap(), &pdir, &gdir, image_base, image_size, entry_rva)
}

fn run_game_library(path: CString, pdir: &str, gdir: &str, image_base: u64, image_size: u64, entry_rva: u32) -> c_int {
    // Expose the runtime's symbols globally so the game .so's references to
    // gow_store/gow_load/gow_* (provided by libgow_runtime.so) resolve.
    unsafe {
        let self_path = CString::new(format!("{pdir}/libgow_runtime.so")).unwrap();
        let _ = dlopen(self_path.as_ptr(), RTLD_NOW | RTLD_GLOBAL);
    }
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if handle.is_null() {
        let err = unsafe { dlerror() };
        let msg = if err.is_null() { "unknown".to_string() } else {
            unsafe { std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned() }
        };
        crate::glog!("[gow] dlopen libgow_game.so failed: {msg}");
        return -1;
    }
    let table_sym = unsafe { dlsym(handle, b"gow_func_table\0".as_ptr() as *const c_char) };
    let count_sym = unsafe { dlsym(handle, b"gow_func_count\0".as_ptr() as *const c_char) };
    if table_sym.is_null() || count_sym.is_null() {
        crate::glog!("[gow] game table symbols missing");
        return -2;
    }
    let count = unsafe { *(count_sym as *const u32) } as usize;
    let table = unsafe { std::slice::from_raw_parts(table_sym as *const GowFuncEntry, count) };
    let mut regs = Vec::with_capacity(count);
    for e in table {
        if let Some(f) = e.fnptr { regs.push((e.rva, f)); }
    }
    crate::glog!("[gow] registered {} functions", regs.len());
    install_registry(regs);

    // Install the Win32 import resolver so the recompiled code's
    // `call [iat_slot]` reaches real implementations. The import list comes
    // from the manifest the recompiler emitted next to the shards.
    install_import_resolver(handle, &pdir, &gdir);

    // Load the DXVK Native backend from the APK's native library dir and hand it
    // the current ANativeWindow, before the entry point runs.
    start_graphics_backend(&pdir);

    let mut cpu = GowCpu::new(image_base, image_size as usize);
    // Load the PE image (sections) into the guest memory mapped at the original
    // base, so absolute addresses in the recompiled code are valid.
    let exe_path = format!("{gdir}/GodOfWar/GoW.exe");
    let exe_path = if std::path::Path::new(&exe_path).exists() { exe_path } else { format!("{gdir}/GoW.exe") };
    match crate::pe_load::load_into(&exe_path, cpu.image, cpu.image_size as usize) {
        Some(_) => crate::glog!("[gow] PE image loaded from {exe_path}"),
        None => crate::glog!("[gow] WARNING: could not load PE image at {exe_path}"),
    }
    crate::glog!("[gow] entry=0x{entry_rva:x} running");
    unsafe { crate::dispatch::gow_run(cpu.as_mut() as *mut GowCpu, entry_rva) };
    crate::glog!("[gow] returned from entry (unimpl hits reported above)");
    0
}

/// ANativeWindow handle for the Vulkan swapchain (set from Java surface callbacks).
static mut WINDOW: *mut std::ffi::c_void = std::ptr::null_mut();

/// The one `GraphicsBridge` the JNI layer and the import resolver share. All of
/// its state is process-global (see `graphics.rs`), so the surface callbacks can
/// push the window into the same DXVK libraries the resolver calls.
fn graphics_bridge() -> &'static crate::graphics::GraphicsBridge {
    static BRIDGE: std::sync::OnceLock<crate::graphics::GraphicsBridge> = std::sync::OnceLock::new();
    BRIDGE.get_or_init(crate::graphics::GraphicsBridge::new)
}

/// Load the DXVK Native backend (if shipped in the APK) and hand it the current
/// `ANativeWindow`. Called from `run_game_library` before the guest entry point
/// runs, so `D3D11CreateDevice` resolves instead of hitting the unimplemented
/// import path.
fn start_graphics_backend(native_lib_dir: &str) {
    let g = graphics_bridge();
    if !g.autoload(native_lib_dir) {
        return; // runtime-only APK; the game will report the unresolved import.
    }
    let win = unsafe { WINDOW };
    if !win.is_null() {
        g.set_android_window(win);
    } else {
        crate::glog!("[gow.gfx] backend loaded but no ANativeWindow yet; pushed on surfaceCreated");
    }
}

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeSurfaceCreated(
    env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    surface: *mut std::ffi::c_void,
) {
    // Resolve Java Surface -> ANativeWindow, the handle the swapchain binds to.
    let android_win = unsafe { android_window_from_surface(env, surface) };
    unsafe { WINDOW = android_win };
    crate::glog!("[gow] surface created: ANativeWindow={:p}", android_win);

    if android_win.is_null() {
        return;
    }

    // Push the window into both loaded DXVK libraries (each keeps its own copy).
    // If they are not loaded yet, `start_graphics_backend` does it after dlopen.
    graphics_bridge().set_android_window(android_win);

    // Bring up Vulkan on the window (Turnip driver via libadrenotools when
    // present). Failure here is reported, never hidden.
    match crate::vk::VkContext::new(android_win, "GoW arm64") {
        Some(ctx) => unsafe { VK = Some(ctx); },
        None => crate::glog!("[gow] Vulkan init failed (driver missing?)"),
    }
}

static mut VK: Option<crate::vk::VkContext> = None;

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeSurfaceChanged(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    w: c_int,
    h: c_int,
) {
    crate::glog!("[gow] surface changed: {w}x{h}");
    // Surface (and possibly the ANativeWindow) changed: re-push to both libs.
    let win = unsafe { WINDOW };
    if !win.is_null() {
        graphics_bridge().set_android_window(win);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeSurfaceDestroyed(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
) {
    unsafe { WINDOW = std::ptr::null_mut(); }
    // Tell DXVK the window is gone so createSurface fails loudly rather than
    // presenting to a stale ANativeWindow.
    graphics_bridge().set_android_window(std::ptr::null_mut());
    crate::glog!("[gow] surface destroyed");
}

/// ANativeWindow_fromSurface(JNIEnv*, jobject) from libandroid.so.
unsafe fn android_window_from_surface(env: *mut std::ffi::c_void, surface: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    type F = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    let lib = dlopen(b"libandroid.so\0".as_ptr() as *const c_char, RTLD_NOW);
    if lib.is_null() { return std::ptr::null_mut(); }
    let sym = dlsym(lib, b"ANativeWindow_fromSurface\0".as_ptr() as *const c_char);
    if sym.is_null() { return std::ptr::null_mut(); }
    let f: F = std::mem::transmute(sym);
    f(env, surface)
}
