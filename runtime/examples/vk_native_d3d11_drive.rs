//! Drives the native **D3D11 COM layer** with the exact Microsoft-x64 ABI the
//! recompiled game uses (register file + hostcall thunks), then shows a frame.
//!
//! This is the closest end-to-end proof short of the full game boot: it calls
//! `CreateDXGIFactory1` -> `IDXGIFactory1::CreateSwapChain` ->
//! `IDXGISwapChain::Present` precisely the way the guest's `call [this->vtbl+n]`
//! reaches them (`gow_call_indirect` -> `hostcall::call`), so if these work, the
//! game's D3D calls will too once wired in.
//!
//! ```text
//! export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
//! export LD_LIBRARY_PATH=$PREFIX/lib
//! export GOW_X11_DISPLAYS=":1"
//! GOW_GFX_BACKEND=native GOW_X11_DEMO_FRAMES=300 \
//!   cargo run -p gow-runtime --example vk_native_d3d11_drive --release
//! ```

use gow_runtime::dispatch::GowCpu;
use gow_runtime::hostcall;
use gow_runtime::vk_native::d3d11_objects as d3d;

fn main() {
    std::env::set_var("GOW_GFX_BACKEND", "native");
    if !d3d::ensure_backend() {
        eprintln!("[drive] native backend failed to start");
        std::process::exit(1);
    }
    println!("[drive] native backend up");

    // A guest CPU provides the register file the COM thunks read.
    let cpu = GowCpu::new(0x140000000, 0x1000);
    let cpu_ptr = cpu.as_ref() as *const GowCpu as *mut GowCpu;

    // --- CreateDXGIFactory1 -> IDXGIFactory1 object ---
    let factory = d3d::create_dxgi_factory();
    // COM: *(obj) = vtable, *(vtable + n*8) = method.
    let vtbl = unsafe { *(factory as *const u64) };
    println!("[drive] factory object = 0x{factory:x} vtable = 0x{vtbl:x}");

    // --- IDXGIFactory1::CreateSwapChain(this, pDevice, pDesc, &swapchain) ---
    // Slot 10. vtable[10] holds the method thunk pseudo-address.
    let method = unsafe { *((vtbl as *const u64).add(10)) };
    println!("[drive] CreateSwapChain thunk = 0x{method:x}");
    let device = d3d::create_device();
    // The swapchain out-pointer: a guest-visible stack slot. Use the CPU's own
    // stack (RSP-8 is not valid; allocate a tiny scratch on the stack top).
    let out_swapchain: u64 = 0;
    unsafe {
        (*cpu_ptr).gpr.rcx = factory; // this
        (*cpu_ptr).gpr.rdx = device; // pDevice
        (*cpu_ptr).gpr.r8 = 0; // pDesc
        (*cpu_ptr).gpr.r9 = &out_swapchain as *const u64 as u64; // ppSwapChain
        assert!(hostcall::call(method, cpu_ptr), "CreateSwapChain thunk missing");
    }
    let swapchain = out_swapchain;
    println!("[drive] CreateSwapChain -> swapchain=0x{swapchain:x}");
    if swapchain == 0 {
        eprintln!("[drive] no swapchain");
        std::process::exit(2);
    }

    // --- IDXGISwapChain::Present(this, syncInterval, flags) ---
    // Slot 8. COM: *(swapchain) = vtable, then slot 8.
    let sc_vtbl = unsafe { *(swapchain as *const u64) };
    let present = unsafe { *((sc_vtbl as *const u64).add(8)) };
    println!("[drive] Present thunk = 0x{present:x}");
    let frames: u32 = std::env::var("GOW_X11_DEMO_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    for i in 0..frames {
        unsafe {
            (*cpu_ptr).gpr.rcx = swapchain;
            (*cpu_ptr).gpr.rdx = 0; // sync interval
            (*cpu_ptr).gpr.r8 = 0; // flags
            assert!(hostcall::call(present, cpu_ptr), "Present thunk missing");
            let hr = (*cpu_ptr).gpr.rax;
            if hr != 0 {
                eprintln!("[drive] Present frame {i} -> HRESULT 0x{hr:x}");
                break;
            }
        }
        if i < 2 || i + 1 == frames {
            println!("[drive] Present frame {i} -> S_OK");
        }
        if frames > 10 {
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
    println!(
        "[drive] frames_presented={} has_presented={}",
        d3d::frames_presented(),
        d3d::has_presented()
    );
    std::process::exit(if d3d::has_presented() { 0 } else { 3 });
}
