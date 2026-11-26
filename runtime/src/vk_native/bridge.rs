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

use super::render::OffscreenTarget;
use super::swapchain::Swapchain;
use super::x11::X11Displays;
use super::NativeDevice;

/// How the native backend puts pixels where the user can see them.
enum Presenter {
    /// GPU image shown through X11 (`XPutImage`) — the Termux/desktop path,
    /// where Turnip cannot present a swapchain without DRI3.
    X11(X11Displays),
    /// Normal `VkSwapchainKHR` present (on-device via `ANativeWindow`).
    Swapchain(Swapchain),
}

/// Frame state the D3D11 context drives, applied when the next frame presents.
struct FrameState {
    /// Clear colour from `ClearRenderTargetView` (RGBA, linear).
    clear: [f32; 4],
    /// Pending streamed triangle (pos+colour, 5 floats per vertex), if any.
    vertices: Option<Vec<[f32; 5]>>,
}

impl Default for FrameState {
    fn default() -> Self {
        FrameState {
            clear: [0.02, 0.03, 0.06, 1.0],
            vertices: None,
        }
    }
}

/// The live native backend, if one was created.
struct Backend {
    device: NativeDevice,
    presenter: Presenter,
    /// GPU target used when presenting via X11.
    target: Option<OffscreenTarget>,
    /// Reusable readback buffer for the X11 path.
    pixels: Vec<u8>,
    extent: ash::vk::Extent2D,
    frame: FrameState,
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

    // Prefer an X11 window when one is reachable (Termux/Xvfb), because Turnip
    // cannot present a swapchain without DRI3. Fall back to the Android
    // swapchain when a real ANativeWindow is present.
    let presenter;
    let mut target = None;
    if let Some(displays) =
        X11Displays::create_default(extent.width, extent.height, "God of War (native Vulkan)")
    {
        match OffscreenTarget::new(&device, extent.width, extent.height) {
            Some(t) => {
                crate::glog!("[gow.vk.native] presenting via X11 ({} display(s))", displays.count());
                target = Some(t);
                presenter = Presenter::X11(displays);
            }
            None => {
                crate::glog!("[gow.vk.native] offscreen target failed");
                return false;
            }
        }
    } else if !window_ptr.is_null() {
        let Some(swapchain) = device.create_swapchain(extent, false) else {
            crate::glog!("[gow.vk.native] swapchain creation failed");
            return false;
        };
        let extent = swapchain.extent;
        presenter = Presenter::Swapchain(swapchain);
    } else {
        crate::glog!("[gow.vk.native] no X11 display and no ANativeWindow");
        return false;
    }

    let pixels = vec![0u8; (extent.width * extent.height * 4) as usize];
    crate::glog!(
        "[gow.vk.native] backend ready: {} {}x{}",
        device.device_name,
        extent.width,
        extent.height
    );
    *guard = Some(Backend {
        device,
        presenter,
        target,
        pixels,
        extent,
        frame: FrameState::default(),
    });
    true
}

/// `ID3D11DeviceContext::ClearRenderTargetView` -> remember the clear colour so
/// the next presented frame uses it. Returns false if no backend.
pub fn set_clear_color(rgba: [f32; 4]) -> bool {
    let mut guard = backend().lock().unwrap();
    let Some(b) = guard.as_mut() else {
        return false;
    };
    b.frame.clear = rgba;
    if let Some(t) = b.target.as_mut() {
        t.record(&b.device, rgba);
    }
    true
}

/// `ID3D11DeviceContext::Draw` with streamed positions+colours (5 floats/vertex)
/// -> feed the built-in scene pipeline. Returns false if no backend.
pub fn draw_triangle_strip_vertices(verts: Vec<[f32; 5]>) -> bool {
    let mut guard = backend().lock().unwrap();
    let Some(b) = guard.as_mut() else {
        return false;
    };
    if let Some(t) = b.target.as_mut() {
        t.set_vertices(&b.device, &verts);
        t.record(&b.device, b.frame.clear);
    }
    b.frame.vertices = Some(verts);
    true
}

/// Render the built-in test frame on the GPU and present it (X11 or swapchain).
/// Returns true if a frame was presented. A real context will replace the
/// triangle with the D3D11 pipeline's output.
pub fn present() -> bool {
    let mut guard = backend().lock().unwrap();
    let Some(b) = guard.as_mut() else {
        return false;
    };
    match &mut b.presenter {
        Presenter::Swapchain(sc) => sc.present_clear(&b.device, [1.0, 0.0, 0.0, 1.0]).is_some(),
        Presenter::X11(displays) => {
            let Some(t) = b.target.as_mut() else {
                return false;
            };
            if !t.render_and_wait(&b.device) {
                return false;
            }
            if !t.read_into(&b.device, &mut b.pixels) {
                return false;
            }
            displays.present(&b.pixels) > 0
        }
    }
}

/// Recreate the presentable surface at a new size (surface changed / rotated).
pub fn resize(width: u32, height: u32) -> bool {
    let mut guard = backend().lock().unwrap();
    let Some(b) = guard.as_mut() else {
        return false;
    };
    let extent = ash::vk::Extent2D { width, height };
    match &mut b.presenter {
        Presenter::Swapchain(sc) => {
            sc.destroy(&b.device);
            match b.device.create_swapchain(extent, false) {
                Some(newsc) => {
                    b.extent = newsc.extent;
                    *sc = newsc;
                    true
                }
                None => false,
            }
        }
        Presenter::X11(displays) => {
            if let Some(t) = b.target.as_mut() {
                t.destroy(&b.device);
            }
            match OffscreenTarget::new(&b.device, width, height) {
                Some(t) => {
                    displays.resize(width, height);
                    b.pixels = vec![0u8; (width * height * 4) as usize];
                    b.extent = extent;
                    b.target = Some(t);
                    true
                }
                None => false,
            }
        }
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
