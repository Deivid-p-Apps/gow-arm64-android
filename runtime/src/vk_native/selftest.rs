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

/// Render the built-in triangle on the GPU and show it on **X11** (Xvfb and/or
/// the Termux:X11 app). This is the "see it on screen" path: it works even where
/// Turnip cannot present a swapchain (no DRI3), by rendering to a host-visible
/// image and `XPutImage`-ing it to every display in `GOW_X11_DISPLAYS`.
///
/// `frames == 0` presents a single frame (for capture). Returns 0 on success.
pub fn run_x11(width: u32, height: u32, frames: u32, log: &mut dyn FnMut(&str)) -> i32 {
    macro_rules! l {
        ($($a:tt)*) => { log(&format!($($a)*)) };
    }
    l!("[selftest.x11] bring-up for {width}x{height}, frames={frames}");
    let Some(dev) = NativeDevice::new(std::ptr::null_mut()) else {
        l!("[selftest.x11] FAIL: NativeDevice::new");
        return -1;
    };
    let Some(mut target) = super::render::OffscreenTarget::new(&dev, width, height) else {
        l!("[selftest.x11] FAIL: OffscreenTarget::new");
        return -2;
    };
    let Some(mut displays) =
        super::x11::X11Displays::create_default(width, height, "God of War (native Vulkan)")
    else {
        l!("[selftest.x11] FAIL: no X11 display (set GOW_X11_DISPLAYS/DISPLAY)");
        target.destroy(&dev);
        return -3;
    };
    l!("[selftest.x11] {} display(s) connected", displays.count());

    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let n = frames.max(1);
    for i in 0..n {
        if !target.render_and_wait(&dev) {
            l!("[selftest.x11] frame {i}: render FAILED");
            break;
        }
        if !target.read_into(&dev, &mut pixels) {
            l!("[selftest.x11] frame {i}: readback FAILED");
            break;
        }
        let shown = displays.present(&pixels);
        if i == 0 || i + 1 == n {
            l!("[selftest.x11] frame {i}: rendered + shown on {shown} display(s)");
        }
        displays.sync();
        // Pace a long run so the image stays on screen (and is capturable);
        // a 1-frame capture run does not sleep.
        if n > 10 {
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
    pixels.truncate(0);
    displays.sync();
    target.destroy(&dev);
    l!("[selftest.x11] done");
    0
}

/// Render an animated **3D** scene (depth-tested spinning cube) on the GPU and
/// show it on X11. Proves the renderer is a real 3D pipeline. Returns 0 on ok.
pub fn run_x11_scene(width: u32, height: u32, frames: u32, log: &mut dyn FnMut(&str)) -> i32 {
    macro_rules! l {
        ($($a:tt)*) => { log(&format!($($a)*)) };
    }
    l!("[selftest.scene] bring-up {width}x{height} frames={frames}");
    let Some(dev) = NativeDevice::new(std::ptr::null_mut()) else {
        l!("[selftest.scene] FAIL: NativeDevice::new");
        return -1;
    };
    let Some(mut scene) = super::scene::Scene3D::new(&dev, width, height) else {
        l!("[selftest.scene] FAIL: Scene3D::new");
        return -2;
    };
    let Some(mut displays) =
        super::x11::X11Displays::create_default(width, height, "God of War (native 3D)")
    else {
        l!("[selftest.scene] FAIL: no X11 display");
        scene.destroy(&dev);
        return -3;
    };
    l!("[selftest.scene] {} display(s)", displays.count());
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let n = frames.max(1);
    for i in 0..n {
        if !scene.render_frame(&dev) {
            l!("[selftest.scene] frame {i}: render FAILED");
            break;
        }
        if !scene.read_into(&dev, &mut pixels) {
            l!("[selftest.scene] frame {i}: readback FAILED");
            break;
        }
        let shown = displays.present(&pixels);
        if i == 0 || i + 1 == n {
            l!("[selftest.scene] frame {i}: 3D cube rendered + shown on {shown} display(s)");
        }
        displays.sync();
        if n > 10 {
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
    displays.sync();
    scene.destroy(&dev);
    l!("[selftest.scene] done");
    0
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

/// C-ABI: render the built-in triangle on the GPU and show it on X11
/// (Xvfb/Termux:X11). `frames == 0` = one frame.
#[cfg(feature = "vkprobe")]
#[no_mangle]
pub unsafe extern "C" fn gow_vk_native_x11_demo(width: u32, height: u32, frames: u32) -> i32 {
    let rc = run_x11(width, height, frames, &mut |m| crate::glog!("{m}"));
    crate::glog!("[selftest] gow_vk_native_x11_demo -> {rc}");
    rc
}
