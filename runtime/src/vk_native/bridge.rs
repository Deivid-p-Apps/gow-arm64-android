//! Integration facade: the single object the import resolver talks to when
//! `GOW_GFX_BACKEND=native`.
//!
//! This keeps *all* native-backend state in the new module, so the two call
//! sites in `graphics.rs`/`win32.rs` stay one-liners (see the delivery report):
//!
//! ```text
//! // win32.rs, the d3d11/dxgi branch:
//! if crate::vk_native::is_enabled() {
//!     crate::vk_native::bridge::dispatch(&dll_l, &func, cpu);
//! } else { /* existing DXVK path */ }
//! ```
//!
//! A process-global (not per-instance) singleton mirrors how the DXVK handles
//! are held in `graphics.rs`: the game creates one device, and the JNI surface
//! callback pushes the window from another thread.

use std::sync::{Mutex, OnceLock};

use super::swapchain::Swapchain;
use super::NativeDevice;

/// The live native backend, if one was created.
struct Backend {
    device: NativeDevice,
    swapchain: Swapchain,
    extent: ash::vk::Extent2D,
}

unsafe impl Send for Backend {}
unsafe impl Sync for Backend {}

fn backend() -> &'static Mutex<Option<Backend>> {
    static B: OnceLock<Mutex<Option<Backend>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(None))
}

/// The `ANativeWindow` pushed by the JNI surface callback (0 until then). Stored
/// as `usize` so the static is `Send + Sync` (raw pointers are not), matching how
/// `graphics.rs` keeps its dlopen handles.
fn window() -> &'static Mutex<usize> {
    static W: OnceLock<Mutex<usize>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(0))
}

/// Whether the native backend is selected by `GOW_GFX_BACKEND`. Cheap to call.
pub fn is_enabled() -> bool {
    super::Backend::from_env().is_native()
}

/// Push the Android window, exactly like `GraphicsBridge::set_android_window`
/// does for DXVK. Stored until [`create`] runs.
pub fn set_android_window(window_ptr: *mut std::ffi::c_void) {
    if !is_enabled() {
        return;
    }
    *window().lock().unwrap() = window_ptr as usize;
    crate::glog!("[gow.vk.native] ANativeWindow={window_ptr:p} stored");
}

/// Create the native device + swapchain. `window == 0` (or none stored) selects
/// the headless surface, which is what the Termux harness uses. Returns true on
/// success. Idempotent: a second call keeps the existing backend.
pub fn create() -> bool {
    if !is_enabled() {
        return false;
    }
    let mut guard = backend().lock().unwrap();
    if guard.is_some() {
        return true;
    }
    let window_ptr = *window().lock().unwrap() as *mut std::ffi::c_void;
    let Some(device) = NativeDevice::new(window_ptr) else {
        crate::glog!("[gow.vk.native] device bring-up failed");
        return false;
    };
    let extent = ash::vk::Extent2D {
        width: 1280,
        height: 720,
    };
    let Some(swapchain) = device.create_swapchain(extent, false) else {
        crate::glog!("[gow.vk.native] swapchain creation failed");
        return false;
    };
    let extent = swapchain.extent;
    crate::glog!(
        "[gow.vk.native] backend ready: {} {}x{}",
        device.device_name,
        extent.width,
        extent.height
    );
    *guard = Some(Backend {
        device,
        swapchain,
        extent,
    });
    true
}

/// Present the red-clear frame (proof path). Returns true if a frame was
/// presented. A real context will replace the colour with the D3D11 state.
pub fn present() -> bool {
    let guard = backend().lock().unwrap();
    let Some(b) = guard.as_ref() else {
        return false;
    };
    b.swapchain.present_clear(&b.device, [1.0, 0.0, 0.0, 1.0]).is_some()
}

/// Recreate the swapchain at a new size (surface changed / rotated).
pub fn resize(width: u32, height: u32) -> bool {
    let mut guard = backend().lock().unwrap();
    let Some(b) = guard.as_mut() else {
        return false;
    };
    b.swapchain.destroy(&b.device);
    let extent = ash::vk::Extent2D { width, height };
    match b.device.create_swapchain(extent, false) {
        Some(sc) => {
            b.extent = sc.extent;
            b.swapchain = sc;
            true
        }
        None => false,
    }
}

/// D3D11/DXGI bootstrap entries the game calls, routed here by the resolver.
///
/// Returns `true` when `func` was handled (a real result or a logged stub),
/// `false` when the resolver should fall through to its DXVK/NULL path. Only
/// `D3D11CreateDevice`/`CreateDXGIFactory1` do real work today; everything else
/// is a documented stub so a call is never silently faked.
pub fn dispatch(dll: &str, func: &str) -> bool {
    let dll = dll.to_ascii_lowercase();
    match (dll.as_str(), func) {
        ("d3d11.dll", "D3D11CreateDevice") => {
            crate::glog!("[gow.vk.native] D3D11CreateDevice -> native backend");
            create()
        }
        ("dxgi.dll", "CreateDXGIFactory1") => {
            // A factory is where DXVK refuses (0x80004005). Here we just ensure
            // the device exists and report success.
            crate::glog!("[gow.vk.native] CreateDXGIFactory1 -> native backend");
            create()
        }
        ("d3dcompiler_47.dll", "D3DReflect") => {
            // Served by our own Rust DXBC reflection (crate::dxbc), not here.
            false
        }
        _ => {
            crate::glog!("[gow.vk.native] unhandled native import: {dll}!{func} (stub)");
            false
        }
    }
}
