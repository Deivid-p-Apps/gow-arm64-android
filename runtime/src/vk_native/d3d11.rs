//! D3D11 device/context schematics for the native backend.
//!
//! These are deliberately **stubs**: this delivery proves device + swapchain +
//! clear + present works, not a full D3D11 implementation. The shapes here are
//! the contract the import resolver will hand the game once the native backend
//! is wired in (`GOW_GFX_BACKEND=native`): a `D3D11CreateDevice` that returns an
//! `ID3D11Device` whose vtable exposes `CreateRenderTargetView`, etc.
//!
//! Every method that is not implemented yet logs once and returns
//! `E_NOTIMPL`/`None` — never a silent fake success, so a game call that lands
//! on a stub is visible in the log instead of corrupting state.

use ash::vk;

/// A native D3D11 device: one Vulkan device plus the D3D11-level bookkeeping the
/// guest expects. Today it just carries the swapchain/queue links.
pub struct Device {
    pub vk: std::sync::Arc<super::NativeDevice>,
    pub swapchain: std::sync::Mutex<Option<super::swapchain::Swapchain>>,
    /// Immediate context, as D3D11 exposes exactly one by default.
    pub context: std::sync::Arc<Context>,
}

/// A native D3D11 immediate context. The clear/present path already works via
/// [`super::swapchain::Swapchain::present_clear`]; state and draws are stubs.
pub struct Context {
    pub device: std::sync::Weak<Device>,
}

impl Context {
    /// `ID3D11DeviceContext::ClearRenderTargetView`.
    ///
    /// Not implemented: the swapchain's built-in clear is used to prove present.
    pub fn clear_render_target_view(&self) -> i32 {
        crate::glog!("[gow.vk.native.d3d11] ClearRenderTargetView: stub (E_NOTIMPL)");
        E_NOTIMPL
    }

    /// `ID3D11DeviceContext::OMSetRenderTargets`.
    pub fn om_set_render_targets(&self) -> i32 {
        crate::glog!("[gow.vk.native.d3d11] OMSetRenderTargets: stub (E_NOTIMPL)");
        E_NOTIMPL
    }

    /// `ID3D11DeviceContext::Draw`.
    pub fn draw(&self) -> i32 {
        crate::glog!("[gow.vk.native.d3d11] Draw: stub (E_NOTIMPL)");
        E_NOTIMPL
    }

    /// `IDXGISwapChain::Present`.
    ///
    /// Forwards to the real native present when a swapchain exists, so this one
    /// method is genuinely wired end to end.
    pub fn present(&self, red: bool) -> i32 {
        let Some(dev) = self.device.upgrade() else {
            return E_FAIL;
        };
        let sc = dev.swapchain.lock().unwrap();
        let Some(sc) = sc.as_ref() else {
            crate::glog!("[gow.vk.native.d3d11] Present: no swapchain (E_FAIL)");
            return E_FAIL;
        };
        let color = if red { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] };
        match sc.present_clear(&dev.vk, color) {
            Some(_) => S_OK,
            None => E_FAIL,
        }
    }
}

pub const S_OK: i32 = 0;
pub const E_FAIL: i32 = 0x8000_4005u32 as i32;
pub const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;

/// A render target view the guest can hold: a swapchain image view + metadata.
/// Created but not yet bound to a pipeline.
pub struct RenderTargetView {
    pub image_view: vk::ImageView,
    pub format: vk::Format,
}
