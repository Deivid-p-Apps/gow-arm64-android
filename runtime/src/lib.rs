//! gow-runtime: guest dispatcher, memory, Win32/NT layer, and graphics.

pub mod dispatch;
pub mod hostcall;
pub mod crashlog;
pub mod pe_load;
pub mod crt;
pub mod log;
pub mod win32;
pub mod input;
pub mod audio;
pub mod fileio;
pub mod graphics;
pub mod adrenotools;
pub mod dxbc;
pub mod threads;
pub mod vk;
pub mod steam;
pub mod sse_extra;
pub mod x87;
#[cfg(target_os = "android")]
pub mod jni;

pub use dispatch::{GowCpu, GowFunc};

/// Host-side boot: run the real recompiled game through the runtime on the
/// host (no APK/device/DXVK). Used by tools/host_boot.c so lifter/Win32/CRT
/// bugs surface in seconds.
#[cfg(feature = "hostboot")]
#[no_mangle]
pub unsafe extern "C" fn host_boot_run(
    game_dir: *const std::os::raw::c_char,
    table: *const dispatch::GowFuncEntry,
) -> std::os::raw::c_int {
    use std::ffi::CStr;
    let gdir = CStr::from_ptr(game_dir).to_string_lossy().into_owned();
    dispatch::host_boot_with_table(&gdir, table)
}

