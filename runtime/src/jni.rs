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
    eprintln!("[gow] nativeInit game_dir={dir}");
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
    eprintln!("[gow] loading recompiled game: {path}");

    // dlopen the user-built game .so (arm64), fetch its table.
    let handle = unsafe {
        let cpath = CString::new(path.as_str()).unwrap();
        dlopen(cpath.as_ptr(), RTLD_NOW)
    };
    if handle.is_null() {
        eprintln!("[gow] dlopen failed");
        return -1;
    }
    let table_sym = unsafe { dlsym(handle, b"gow_func_table\0".as_ptr() as *const c_char) };
    let count_sym = unsafe { dlsym(handle, b"gow_func_count\0".as_ptr() as *const c_char) };
    if table_sym.is_null() || count_sym.is_null() {
        eprintln!("[gow] game table symbols missing");
        return -2;
    }
    let count = unsafe { *(count_sym as *const u32) } as usize;
    let table = unsafe { std::slice::from_raw_parts(table_sym as *const GowFuncEntry, count) };
    let mut regs = Vec::with_capacity(count);
    for e in table {
        if let Some(f) = e.fnptr { regs.push((e.rva, f)); }
    }
    eprintln!("[gow] registered {} functions", regs.len());
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
