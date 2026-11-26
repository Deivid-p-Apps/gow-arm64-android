//! Probe the *external GIPA* path with the raw Turnip ICD.
//!
//! libadrenotools hands the native backend one `vkGetInstanceProcAddr` from a
//! loader it opens privately. This example reproduces that contract without
//! libadrenotools: it `dlopen`s the ICD `.so` directly and resolves
//! `vk_icdGetInstanceProcAddr` from it (the ICD exports it, see
//! `nm -D libvulkan_freedreno.so`), then drives the whole native bring-up
//! through that single pointer.
//!
//! ```text
//! export LD_LIBRARY_PATH=$PREFIX/lib
//! cargo run -p gow-runtime --example vk_native_gipa_probe --release
//! ```
//!
//! (No `VK_ICD_FILENAMES` needed: the raw ICD is used directly, no loader.)

use std::ffi::c_void;

extern "C" {
    fn dlopen(path: *const std::os::raw::c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const std::os::raw::c_char) -> *mut c_void;
    fn dlerror() -> *const std::os::raw::c_char;
}

const RTLD_NOW: i32 = 2;

fn main() {
    let path = std::env::var("VK_ICD_SO")
        .unwrap_or_else(|_| format!("{}/lib/libvulkan_freedreno.so", std::env::var("PREFIX").unwrap_or_else(|_| "/data/data/com.termux/files/usr".into())));
    println!("[gipa-probe] dlopen {path}");
    let cpath = std::ffi::CString::new(path.clone()).unwrap();
    let handle = unsafe { dlopen(cpath.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        let err = unsafe { dlerror() };
        let msg = if err.is_null() {
            "unknown".to_string()
        } else {
            unsafe { std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned() }
        };
        println!("[gipa-probe] dlopen failed: {msg}");
        std::process::exit(1);
    }
    println!("[gipa-probe] handle={handle:p}");

    // The ICD exports both `vkGetInstanceProcAddr` and `vk_icdGetInstanceProcAddr`.
    // libadrenotools' loader exposes the former; use it here to match.
    let mut gipa = unsafe {
        dlsym(
            handle,
            b"vkGetInstanceProcAddr\0".as_ptr() as *const std::os::raw::c_char,
        )
    };
    if gipa.is_null() {
        println!("[gipa-probe] vkGetInstanceProcAddr absent; trying vk_icdGetInstanceProcAddr");
        gipa = unsafe {
            dlsym(
                handle,
                b"vk_icdGetInstanceProcAddr\0".as_ptr() as *const std::os::raw::c_char,
            )
        };
    }
    if gipa.is_null() {
        println!("[gipa-probe] no GIPA in {path}");
        std::process::exit(1);
    }
    println!("[gipa-probe] vkGetInstanceProcAddr={gipa:p}");

    let rc = gow_runtime::vk_native::selftest::run_with_gipa(
        gipa as usize,
        std::ptr::null_mut(),
        3,
        &mut |m| println!("{m}"),
    );
    std::process::exit(if rc == 0 { 0 } else { rc });
}
