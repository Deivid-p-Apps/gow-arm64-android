//! Runnable smoke test for the native Vulkan backend.
//!
//! Exercises the exact path the game will use: bring up instance + device +
//! queue on the real driver, create a swapchain, then run a few clear/present
//! frames. With no `ANativeWindow` (Termux, no Android app) it uses
//! `VK_EXT_headless_surface`, so swapchain + clear + present all run for real;
//! only the *pixels-on-screen* part is untestable there.
//!
//! Two ways to run it:
//! * `GOW_GFX_SELFTEST=1` before the runtime's normal init, or
//! * the C harness `tools/vk_native_probe.c` calling
//!   `gow_vk_native_selftest()` (feature `vkprobe`).

use super::NativeDevice;
use ash::vk;
use std::ffi::c_void;

/// Run the full bring-up + N clear/present frames. `window == null` selects the
/// headless surface. Returns 0 on success, a `VkResult`-ish code on failure.
pub fn run(window: *mut c_void, frames: u32, log: &mut dyn FnMut(&str)) -> i32 {
    run_inner(None, window, frames, log)
}

/// Like [`run`], but resolves every entry point through an externally supplied
/// `vkGetInstanceProcAddr` (the exact libadrenotools contract). Used by the
/// probe to prove the external-GIPA path: the caller does
/// `dlopen(libvulkan_freedreno.so)` -> `dlsym(vk_icdGetInstanceProcAddr)` and
/// hands the pointer here, and the whole instance/device/swapchain is then
/// backed by that driver.
pub fn run_with_gipa(
    gipa: usize,
    window: *mut c_void,
    frames: u32,
    log: &mut dyn FnMut(&str),
) -> i32 {
    run_inner(Some(gipa), window, frames, log)
}

fn run_inner(
    gipa: Option<usize>,
    window: *mut c_void,
    frames: u32,
    log: &mut dyn FnMut(&str),
) -> i32 {
    macro_rules! l {
        ($($a:tt)*) => { log(&format!($($a)*)) };
    }

    l!(
        "[selftest] native Vulkan bring-up (window={window:p} gipa={:?})",
        gipa
    );
    let dev = match gipa {
        Some(g) => unsafe { NativeDevice::new_with_gipa(g, window) },
        None => NativeDevice::new(window),
    };
    let Some(dev) = dev else {
        l!("[selftest] FAIL: NativeDevice::new");
        return -1;
    };
    l!(
        "[selftest] ok device={:?} api=1.3.{} kind={:?} sb8={}",
        dev.device_name,
        dev.api_version & 0xfff,
        dev.surface_kind,
        dev.storage_buffer_8bit
    );

    let extent = vk::Extent2D {
        width: 1280,
        height: 720,
    };
    let Some(mut sc) = dev.create_swapchain(extent, false) else {
        l!("[selftest] FAIL: create_swapchain");
        return -2;
    };
    l!(
        "[selftest] swapchain {:?} {}x{} images={}",
        dev.surface_kind,
        sc.extent.width,
        sc.extent.height,
        sc.images.len()
    );

    let mut presented = 0u32;
    for i in 0..frames {
        match sc.present_clear(&dev, [1.0, 0.0, 0.0, 1.0]) {
            Some(idx) => {
                l!("[selftest] frame {i}: acquired image {idx}, cleared red, presented");
                presented += 1;
            }
            None => {
                l!("[selftest] frame {i}: present path returned None (see log)");
                break;
            }
        }
    }

    sc.destroy(&dev);
    l!("[selftest] done: {presented}/{frames} frames presented");
    if presented == frames {
        0
    } else {
        -3
    }
}

/// C-ABI entry point for `tools/vk_native_probe.c` (feature `vkprobe`). Logs via
/// the runtime's `glog` (logcat/eprintln) and returns 0 on success.
///
/// # Safety
/// `window` may be null (headless). Not thread-safe with a concurrent backend
/// bring-up, but safe to call from a single harness thread.
#[cfg(feature = "vkprobe")]
#[no_mangle]
pub unsafe extern "C" fn gow_vk_native_selftest(window: *mut c_void, frames: u32) -> i32 {
    let rc = run(window, frames.max(1), &mut |m| crate::glog!("{m}"));
    crate::glog!("[selftest] gow_vk_native_selftest -> {rc}");
    rc
}

/// Same, but with an external `vkGetInstanceProcAddr` from the caller.
#[cfg(feature = "vkprobe")]
#[no_mangle]
pub unsafe extern "C" fn gow_vk_native_selftest_gipa(
    gipa: usize,
    window: *mut c_void,
    frames: u32,
) -> i32 {
    let rc = run_with_gipa(gipa, window, frames.max(1), &mut |m| crate::glog!("{m}"));
    crate::glog!("[selftest] gow_vk_native_selftest_gipa -> {rc}");
    rc
}
