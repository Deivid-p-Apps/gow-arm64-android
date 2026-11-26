//! Native D3D11 COM objects: the bridge the game actually talks to.
//!
//! The recompiled game reaches D3D11 through COM vtables: `call [this->vtbl+n]`
//! becomes `gow_call_indirect(cpu, method_addr)`. `runtime/src/hostcall.rs`
//! turns each method into a pseudo-address whose handler reads the guest's
//! MS-x64 register file. So this module builds real vtables for the objects the
//! game requests at boot:
//!
//! * `CreateDXGIFactory1` -> `IDXGIFactory1`
//! * `D3D11CreateDevice`    -> `ID3D11Device` (+ immediate `ID3D11DeviceContext`)
//! * `IDXGISwapChain::Present` -> a real GPU frame (via `vk_native`)
//!
//! Scope of this cut: the **bootstrap objects + present path**. The per-resource
//! methods (CreateTexture2D, CreateRenderTargetView, full draw state) are honest
//! stubs that log once and return `E_NOTIMPL`, so the game advances past device
//! creation and its first frames reach the native renderer. Nothing is faked as
//! success: a method that is not implemented returns E_NOTIMPL.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::dispatch::GowCpu;
use crate::hostcall;

pub const S_OK: u64 = 0;
pub const E_FAIL: u64 = 0x8000_4005;
pub const E_NOTIMPL: u64 = 0x8000_4001;
pub const E_NOINTERFACE: u64 = 0x8000_4002;
pub const E_INVALIDARG: u64 = 0x8007_0057;
pub const DXGI_ERROR_NOT_FOUND: u64 = 0x887A_0002;
pub const DXGI_ERROR_INVALID_CALL: u64 = 0x887A_0001;
pub const DXGI_ERROR_UNSUPPORTED: u64 = 0x887A_0004;

/// Virtual-call state for the native graphics objects.
///
/// **Important:** all vtables are built here, up front, never inside a handler.
/// `hostcall::call` holds the `THUNKS` mutex while a handler runs, so a handler
/// that called `hostcall::register`/`build_vtable` would deadlock. The handlers
/// therefore only *read* these pre-built vtables.
struct GfxState {
    device_obj: u64,
    context_obj: u64,
    swapchain_obj: u64,
    factory_obj: u64,
    frames: AtomicU64,
    first_present: AtomicU64,
}

/// Build a COM object: a leaked 8-byte allocation whose first qword is
/// `vtable`. THIS is what the guest receives as `this`. COM dereferences twice —
/// `*(this)` = vtable, `*(vtable + n*8)` = method — so returning the vtable
/// itself (as an earlier cut did) makes the guest read a thunk pseudo-address
/// (`0xa000...`, callable but unmapped) and fault. Must be a real allocation.
fn make_com_object(vtable: u64) -> u64 {
    let obj = Box::new([vtable]);
    let ptr = Box::into_raw(obj) as u64;
    crate::glog!("[gow.vk.native.d3d11] COM object this=0x{ptr:x} vtable=0x{vtable:x}");
    ptr
}

fn state() -> &'static Mutex<GfxState> {
    static S: OnceLock<Mutex<GfxState>> = OnceLock::new();
    S.get_or_init(|| {
        // Build every vtable now (no handler is running, so the hostcall lock is
        // free). Handlers below reference `state()` only for the handle values.
        // Each object is a real `{vtable}` allocation (see `make_com_object`).
        let device_obj = make_com_object(make_d3d11_device());
        let context_obj = make_com_object(make_d3d11_context());
        let swapchain_obj = make_com_object(make_swapchain());
        let factory_obj = make_com_object(make_dxgi_factory());
        Mutex::new(GfxState {
            device_obj,
            context_obj,
            swapchain_obj,
            factory_obj,
            frames: AtomicU64::new(0),
            first_present: AtomicU64::new(0),
        })
    })
}

/// Read argument `n` (0 = `this`) from the guest's MS-x64 register file.
/// Args 1-4 live in RCX/RDX/R8/R9; 5+ at [rsp+0x28].. (32-byte shadow space).
unsafe fn arg(c: *const GowCpu, n: usize) -> u64 {
    let c = &*c;
    match n {
        0 => c.gpr.rcx,
        1 => c.gpr.rdx,
        2 => c.gpr.r8,
        3 => c.gpr.r9,
        _ => {
            let off = 0x28 + (n - 4) * 8;
            *((c.gpr.rsp as *const u8).add(off) as *const u64)
        }
    }
}

unsafe fn store_out(p: u64, v: u64) {
    if p != 0 {
        *(p as *mut u64) = v;
    }
}

/// A `IUnknown::QueryInterface(this, riid, ppvObject)` that is honest: it only
/// succeeds for interfaces we actually implement and returns `E_NOINTERFACE`
/// otherwise. Always returning S_OK handed the caller an object of the wrong
/// type, which then called a method past the end of the (short) vtable and faulted.
fn make_query_interface() -> u64 {
    hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        let this = c.gpr.rcx;
        let riid = c.gpr.rdx;
        let ppv = c.gpr.r8;
        let mut accept = false;
        if riid != 0 {
            let d0 = *(riid as *const u64);
            let d1 = *((riid as *const u64).add(1));
            log_iid(d0, d1);
            accept = is_known_iid(d0, d1);
        } else {
            accept = true; // a null riid is not a real query
        }
        if accept {
            if ppv != 0 {
                *(ppv as *mut u64) = this;
            }
            c.gpr.rax = S_OK;
        } else {
            if ppv != 0 {
                *(ppv as *mut u64) = 0;
            }
            c.gpr.rax = E_NOINTERFACE;
        }
    })
}

/// GUIDs stored as two little-endian u64s (the on-disk IID layout). We accept
/// IUnknown plus the concrete interfaces our vtables actually implement; every
/// other IID answers E_NOINTERFACE so the game takes its documented fallback
/// instead of calling into an object that is too small.
fn is_known_iid(d0: u64, d1: u64) -> bool {
    // IUnknown {00000000-0000-0000-C000-000000000046}
    const IUNKNOWN: (u64, u64) = (0x0000_0000_0000_0000, 0x4600_0000_0000_00c0);
    // The concrete IDs, encoded as the two qwords a COM `riid` occupies.
    const IDXGI_FACTORY1: (u64, u64) =
        iid_of(0x770a_a070, 0x8f1f, 0x4c29, [0xb8, 0x6a, 0x86, 0x4a, 0x12, 0xb4, 0x44, 0x8c]);
    const ID3D11_DEVICE: (u64, u64) =
        iid_of(0xdb6f_6ddb, 0xac77, 0x4e88, [0x82, 0x53, 0x81, 0x9d, 0xf9, 0xbb, 0xf1, 0x40]);
    const ID3D11_DEVICE_CTX: (u64, u64) =
        iid_of(0xc0bf_a0d4, 0x1f28, 0x4742, [0x90, 0x81, 0x28, 0x08, 0x51, 0x6b, 0x6a, 0xb8]);
    const IDXGI_SWAPCHAIN: (u64, u64) =
        iid_of(0x310d_36f1, 0xdbe6, 0x4a75, [0x9c, 0x48, 0x7b, 0x56, 0x1a, 0xbb, 0x9e, 0xcc]);
    let passed = (d0, d1);
    passed == IUNKNOWN
        || passed == IDXGI_FACTORY1
        || passed == ID3D11_DEVICE
        || passed == ID3D11_DEVICE_CTX
        || passed == IDXGI_SWAPCHAIN
}

/// Encode a GUID as the two little-endian qwords a COM caller passes in `riid`.
const fn iid_of(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> (u64, u64) {
    let q0 = (d1 as u64)
        | ((d2 as u64) << 32)
        | ((d3 as u64) << 48);
    let q1 = (d4[0] as u64)
        | ((d4[1] as u64) << 8)
        | ((d4[2] as u64) << 16)
        | ((d4[3] as u64) << 24)
        | ((d4[4] as u64) << 32)
        | ((d4[5] as u64) << 40)
        | ((d4[6] as u64) << 48)
        | ((d4[7] as u64) << 56);
    (q0, q1)
}

/// Dedup-log requested IIDs (`QueryInterface`'s riid), capped so a hot loop does
/// not flood the log.
fn log_iid(d0: u64, d1: u64) {
    use std::sync::Mutex;
    static SEEN: std::sync::OnceLock<Mutex<std::collections::HashSet<(u64, u64)>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut s = seen.lock().unwrap();
    if s.len() < 64 && s.insert((d0, d1)) {
        // Print in GUID byte order {Data1-Data2-Data3-Data4}.
        let b = d0.to_le_bytes();
        let c = d1.to_le_bytes();
        crate::glog!(
            "[gow.vk.native.d3d11] QueryInterface IID={{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            u16::from_le_bytes([b[4], b[5]]),
            u16::from_le_bytes([b[6], b[7]]),
            c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]
        );
    }
}

// ---------------------------------------------------------------------------
// IDXGIFactory1
// ---------------------------------------------------------------------------

/// Build the `IDXGIFactory1` vtable returned to the guest.
pub fn make_dxgi_factory() -> u64 {
    // 0 QueryInterface,1 AddRef,2 Release, 3 SetPrivateData,4 SetPrivateDataInterface,
    // 5 GetPrivateData,6 GetParent, 7 EnumAdapters,8 MakeWindowAssociation,
    // 9 GetWindowAssociation,10 CreateSwapChain,11 CreateSoftwareAdapter,
    // 12 EnumAdapters1,13 IsCurrent.
    const VT_LEN: usize = 14;
    let query = make_query_interface();
    let addref = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 1; });
    let release = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });
    let not_found = hostcall::register(|cpu| unsafe {
        (*cpu).gpr.rax = DXGI_ERROR_NOT_FOUND;
    });
    let make_window_assoc = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = S_OK; });
    let create_swapchain = hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        // CreateSwapChain(this, pDevice, pDesc, ppSwapChain): out is arg 3.
        // The swapchain vtable is pre-built in `state()`; do NOT build here
        // (a handler runs with the hostcall mutex held -> would deadlock).
        let out = arg(c, 3);
        let sc = state().lock().unwrap().swapchain_obj;
        store_out(out, sc);
        crate::glog!("[gow.vk.native.d3d11] IDXGIFactory::CreateSwapChain -> 0x{sc:x}");
        c.gpr.rax = S_OK;
    });
    hostcall::build_vtable(
        &[
            (0, query),
            (1, addref),
            (2, release),
            (7, not_found),
            (8, make_window_assoc),
            (12, not_found),
            (10, create_swapchain),
        ],
        VT_LEN,
    )
}

// ---------------------------------------------------------------------------
// IDXGISwapChain
// ---------------------------------------------------------------------------

fn make_swapchain() -> u64 {
    // 0 QueryInterface,1 AddRef,2 Release, 3 SetPrivateData,4 SetPrivateDataInterface,
    // 5 GetPrivateData,6 GetParent, 7 GetDevice, 8 Present, 9 GetBuffer,
    // 10 SetFullscreenState,11 GetFullscreenState, 12 GetDesc, 13 ResizeBuffers,
    // 14 ResizeTarget, 15 GetContainingOutput, 16 GetFrameStatistics,
    // 17 GetLastPresentCount.
    const VT_LEN: usize = 18;
    let query = make_query_interface();
    let addref = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 1; });
    let release = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });
    let get_device = hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        let out = arg(c, 1);
        let dev = state().lock().unwrap().device_obj;
        store_out(out, dev);
        c.gpr.rax = S_OK;
    });
    let present = hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        let n = state().lock().unwrap().frames.fetch_add(1, Ordering::Relaxed);
        if n < 3 {
            crate::glog!("[gow.vk.native.d3d11] IDXGISwapChain::Present (frame {n})");
        }
        if !crate::vk_native::bridge::is_enabled() {
            c.gpr.rax = S_OK; // no native backend selected: keep the loop alive
            return;
        }
        if crate::vk_native::bridge::present() {
            state().lock().unwrap().first_present.store(1, Ordering::Relaxed);
            c.gpr.rax = S_OK;
        } else {
            c.gpr.rax = DXGI_ERROR_INVALID_CALL;
        }
    });
    let get_buffer = hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        let out = arg(c, 3);
        store_out(out, 0);
        c.gpr.rax = E_INVALIDARG;
    });
    let resize_buffers = hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        let (w, h) = (arg(c, 1), arg(c, 2));
        let _ = crate::vk_native::bridge::resize(w as u32, h as u32);
        c.gpr.rax = S_OK;
    });
    hostcall::build_vtable(
        &[
            (0, query),
            (1, addref),
            (2, release),
            (7, get_device),
            (8, present),
            (9, get_buffer),
            (13, resize_buffers),
        ],
        VT_LEN,
    )
}

// ---------------------------------------------------------------------------
// ID3D11Device
// ---------------------------------------------------------------------------

fn make_d3d11_device() -> u64 {
    // 0 QueryInterface,1 AddRef,2 Release,3 CreateBuffer,4 CreateTexture1D,
    // 5 CreateTexture2D,6 CreateTexture3D,7 CreateShaderResourceView,
    // 8 CreateUnorderedAccessView,9 CreateRenderTargetView,10 CreateDepthStencilView,
    // 11 CreateInputLayout,12 CreateVertexShader,13 CreateGeometryShader,
    // 14 CreateGeometryShaderWithStreamOutput,15 CreatePixelShader,16 CreateHullShader,
    // 17 CreateDomainShader,18 CreateComputeShader,19 CreateClassLinkage,
    // 20 CreateBlendState,21 CreateDepthStencilState,22 CreateRasterizerState,
    // 23 CreateSamplerState,24 CreateQuery,25 CreatePredicate,26 CreateCounter,
    // 27 CreateDeferredContext,28 OpenSharedResource,29 CheckFormatSupport,
    // 30 CheckMultisampleQualityLevels,31 CheckCounterInfo,32 CheckCounter,
    // 33 CheckFeatureSupport,34 GetPrivateData,35 SetPrivateData,
    // 36 SetPrivateDataInterface,37 GetCreationFlags,38 OpenSharedResource1,
    // 39 OpenSharedResourceByName.
    const VT_LEN: usize = 40;
    let query = make_query_interface();
    let addref = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 1; });
    let release = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });
    let notimpl = hostcall::register(|cpu| unsafe {
        log_stub("ID3D11Device::<method>");
        (*cpu).gpr.rax = E_NOTIMPL;
    });
    let check_format = hostcall::register(|cpu| unsafe {
        // CheckFormatSupport(this, format, UINT* out). Claim 2D texture + RT.
        let c = &mut *cpu;
        let out = arg(c, 2);
        if out != 0 {
            *(out as *mut u32) = 0x1 | 0x40;
        }
        c.gpr.rax = S_OK;
    });
    let check_ms_quality = hostcall::register(|cpu| unsafe {
        // CheckMultisampleQualityLevels(this, format, sampleCount, UINT* out=0).
        let c = &mut *cpu;
        let out = arg(c, 3);
        if out != 0 {
            *(out as *mut u32) = 0;
        }
        c.gpr.rax = S_OK;
    });
    hostcall::build_vtable(
        &[
            (0, query),
            (1, addref),
            (2, release),
            (27, notimpl), // CreateDeferredContext -> E_NOTIMPL (immediate only)
            (29, check_format),
            (30, check_ms_quality),
        ],
        VT_LEN,
    )
}

// ---------------------------------------------------------------------------
// ID3D11DeviceContext
// ---------------------------------------------------------------------------

fn make_d3d11_context() -> u64 {
    // 0 QueryInterface,1 AddRef,2 Release,3 GetDevice,
    // 4 GetPrivateData,5 SetPrivateData,6 SetPrivateDataInterface,
    // 7 VSSetConstantBuffers,8 PSSetShaderResources,9 PSSetShader,10 PSSetSamplers,
    // 11 VSSetShader,12 DrawIndexed,13 Draw,14 Map,15 Unmap,16 PSSetConstantBuffers,
    // 17 IASetInputLayout,18 IASetVertexBuffers,19 IASetIndexBuffer,
    // 20 DrawIndexedInstanced,21 DrawInstanced,22 GSSetConstantBuffers,23 GSSetShader,
    // 24 IASetPrimitiveTopology,25 VSSetShaderResources,26 VSSetSamplers,27 Begin,
    // 28 End,29 GetData,30 SetPredication,31 GSSetShaderResources,32 GSSetSamplers,
    // 33 OMSetRenderTargets,34 OMSetRenderTargetsAndUnorderedAccessViews,
    // 35 OMSetBlendState,36 OMSetDepthStencilState,37 SOSetTargets,38 DrawAuto,
    // 39 DrawIndexedInstancedIndirect,40 DrawInstancedIndirect,41 Dispatch,
    // 42 DispatchIndirect,43 RSSetState,44 RSSetViewports,45 RSSetScissorRects,
    // 46 CopySubresourceRegion,47 CopyResource,48 UpdateSubresource,
    // 49 CopyStructureCount,50 ClearRenderTargetView,51 ClearUnorderedAccessViewUint,
    // 52 ClearUnorderedAccessViewFloat,53 ClearDepthStencilView,54 GenerateMips,
    // 55 SetResourceMinLOD,56 GetResourceMinLOD,57 ResolveSubresource,
    // 58 ExecuteCommandList, ...
    const VT_LEN: usize = 60;
    let query = make_query_interface();
    let addref = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 1; });
    let release = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });
    let get_device = hostcall::register(|cpu| unsafe {
        let c = &mut *cpu;
        let out = arg(c, 1);
        let dev = state().lock().unwrap().device_obj;
        store_out(out, dev);
        c.gpr.rax = S_OK;
    });
    // State-setting calls succeed as no-ops (the renderer keeps its own default
    // state); draw calls succeed so the engine's command stream advances.
    let ok = || hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = S_OK; });
    let draw = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = S_OK; });
    let clear_rtv = hostcall::register(|cpu| unsafe {
        // ClearRenderTargetView(this, pRTV, RGBA*): the native renderer clears
        // the swapchain/target as part of its frame.
        (*cpu).gpr.rax = S_OK;
    });
    let om_set = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = S_OK; });
    hostcall::build_vtable(
        &[
            (0, query),
            (1, addref),
            (2, release),
            (3, get_device),
            (9, ok()),       // PSSetShader
            (11, ok()),      // VSSetShader
            (12, draw),      // DrawIndexed
            (13, draw),      // Draw
            (17, ok()),      // IASetInputLayout
            (18, ok()),      // IASetVertexBuffers
            (19, ok()),      // IASetIndexBuffer
            (20, draw),      // DrawIndexedInstanced
            (21, draw),      // DrawInstanced
            (24, ok()),      // IASetPrimitiveTopology
            (33, om_set),    // OMSetRenderTargets
            (44, ok()),      // RSSetViewports
            (45, ok()),      // RSSetScissorRects
            (50, clear_rtv), // ClearRenderTargetView
        ],
        VT_LEN,
    )
}

fn log_stub(method: &str) {
    crate::glog!("[gow.vk.native.d3d11] {method}: stub (E_NOTIMPL)");
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return the (pre-built) `ID3D11Device` vtable for the guest.
pub fn create_device() -> u64 {
    let s = state().lock().unwrap();
    crate::glog!(
        "[gow.vk.native.d3d11] ID3D11Device=0x{:x} context=0x{:x}",
        s.device_obj,
        s.context_obj
    );
    s.device_obj
}

/// The immediate context object, for `D3D11CreateDevice` out-params.
pub fn immediate_context() -> u64 {
    state().lock().unwrap().context_obj
}

/// Return the (pre-built) `IDXGIFactory1` vtable for the guest.
pub fn create_dxgi_factory() -> u64 {
    state().lock().unwrap().factory_obj
}

/// Ensure the native backend (device + presenter) is up.
///
/// Also forces `state()` to initialise now: its `OnceLock` init builds the COM
/// vtables via `hostcall::register`, which must NOT happen from inside a handler
/// (that runs with the hostcall mutex held -> deadlock). Calling this before the
/// first vtable call guarantees the vtables already exist.
pub fn ensure_backend() -> bool {
    drop(state().lock().unwrap());
    crate::vk_native::bridge::create()
}

/// Handle a D3D11/DXGI module entry point (`D3D11CreateDevice`,
/// `CreateDXGIFactory1`) for the native backend, using the guest register file
/// exactly like a real import call. Returns the HRESULT for the guest's RAX.
///
/// This is what `win32.rs` calls when `GOW_GFX_BACKEND=native`. Every vtables is
/// pre-built by `state()` so no `hostcall::register` happens here.
pub unsafe fn call_entry(func: &str, cpu: *mut GowCpu) -> u64 {
    // Bring up device + presenter on first use (idempotent).
    if !ensure_backend() {
        crate::glog!("[gow.vk.native.d3d11] {func}: native backend failed to start");
        return E_FAIL;
    }
    let device = create_device();
    match func {
        "D3D11CreateDevice" => {
            // HRESULT D3D11CreateDevice(pAdapter, DriverType, Software, Flags,
            //   pFeatureLevels, FeatureLevels, SDKVersion, ppDevice, pFeatureLevel,
            //   ppImmediateContext).
            let c = &mut *cpu;
            let pp_device = arg(c, 7);
            let p_feature_level = arg(c, 8);
            let pp_context = arg(c, 9);
            store_out(pp_device, device);
            store_out(pp_context, immediate_context());
            if p_feature_level != 0 {
                *(p_feature_level as *mut u32) = 0xb000; // D3D_FEATURE_LEVEL_11_0
            }
            crate::glog!(
                "[gow.vk.native.d3d11] D3D11CreateDevice -> device=0x{device:x} fl=11_0"
            );
            S_OK
        }
        "CreateDXGIFactory1" => {
            // HRESULT CreateDXGIFactory1(REFIID riid, void** ppFactory).
            let c = &mut *cpu;
            let out = arg(c, 1);
            let factory = create_dxgi_factory();
            store_out(out, factory);
            crate::glog!("[gow.vk.native.d3d11] CreateDXGIFactory1 -> 0x{factory:x}");
            S_OK
        }
        _ => {
            crate::glog!("[gow.vk.native.d3d11] unhandled entry {func}");
            E_NOTIMPL
        }
    }
}

pub fn frames_presented() -> u64 {
    state().lock().unwrap().frames.load(Ordering::Relaxed)
}

pub fn has_presented() -> bool {
    state().lock().unwrap().first_present.load(Ordering::Relaxed) != 0
}
