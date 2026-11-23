//! Android JNI entry points. The APK ships the runtime (dispatcher, Win32/NT
//! layer, D3D11->Vulkan bridge, audio, input). The recompiled game code is a
//! separate .so the user builds locally from their own files and the app loads
//! at runtime.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use crate::dispatch::{install_registry, GowCpu, GowFuncEntry};

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeInit(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    game_dir: *const c_char,
) -> c_int {
    let dir = unsafe {
        if game_dir.is_null() { String::new() }
        else { std::ffi::CStr::from_ptr(game_dir).to_string_lossy().into_owned() }
    };
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

const RTLD_NOW: c_int = 2;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, sym: *const c_char) -> *mut std::ffi::c_void;
}

/// Discover the recompiled game library next to the game files, copy it into the
/// app's private directory (Android cannot dlopen from external storage) and run
/// it from the PE entry point. `priv_dir` is the app's private files path.
#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeLoadAndRun(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    game_dir: *const c_char,
    priv_dir: *const c_char,
    image_base: u64,
    image_size: u64,
    entry_rva: u32,
) -> c_int {
    let gdir = unsafe { std::ffi::CStr::from_ptr(game_dir).to_string_lossy().into_owned() };
    let pdir = unsafe { std::ffi::CStr::from_ptr(priv_dir).to_string_lossy().into_owned() };

    // If the game library ships inside the APK it is already in the app's
    // nativeLibraryDir (priv_dir points at it); try that first.
    let candidates = [
        format!("{gdir}/libgow_game.so"),
        format!("{gdir}/drivers/libgow_game.so"),
        format!("{pdir}/libgow_game.so"),
    ];
    let src = match candidates.iter().find(|p| std::path::Path::new(p).exists()) {
        Some(p) => p.clone(),
        None => {
            crate::glog!("[gow] no libgow_game.so found in {gdir} (build it with tools/build_game_so.sh)");
            return -3;
        }
    };
    // Copy into private storage so dlopen is allowed.
    let dst = format!("{pdir}/libgow_game.so");
    if let Err(e) = std::fs::copy(&src, &dst) {
        crate::glog!("[gow] copy {src} -> {dst} failed: {e}");
        return -4;
    }
    crate::glog!("[gow] game library staged: {dst}");
    let cpath = CString::new(dst).unwrap();
    run_game_library(cpath, image_base, image_size, entry_rva)
}

fn run_game_library(path: CString, image_base: u64, image_size: u64, entry_rva: u32) -> c_int {
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        crate::glog!("[gow] dlopen failed (arch mismatch? arm64 required)");
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

    let mut cpu = GowCpu::new(image_base, image_size as usize);
    crate::glog!("[gow] entry=0x{entry_rva:x} running");
    unsafe { crate::dispatch::gow_run(cpu.as_mut() as *mut GowCpu, entry_rva) };
    crate::glog!("[gow] returned from entry (unimpl hits reported above)");
    0
}

/// ANativeWindow handle for the Vulkan swapchain (set from Java surface callbacks).
static mut WINDOW: *mut std::ffi::c_void = std::ptr::null_mut();

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

    // Bring up Vulkan on the window (Turnip driver via libadrenotools when
    // present). Failure here is reported, never hidden.
    if !android_win.is_null() {
        match crate::vk::VkContext::new(android_win, "GoW arm64") {
            Some(ctx) => unsafe { VK = Some(ctx); },
            None => crate::glog!("[gow] Vulkan init failed (driver missing?)"),
        }
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
}

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeSurfaceDestroyed(
    _env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
) {
    unsafe { WINDOW = std::ptr::null_mut(); }
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
