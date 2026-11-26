//! Exercises the integration facade the import resolver will call
//! (`GOW_GFX_BACKEND=native` -> `vk_native::bridge`). Same headless caveat as the
//! other examples: no ANativeWindow in Termux, so the swapchain is headless but
//! real.
//!
//! ```text
//! export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
//! export LD_LIBRARY_PATH=$PREFIX/lib
//! GOW_GFX_BACKEND=native cargo run -p gow-runtime --example vk_native_bridge_smoke --release
//! ```

use gow_runtime::vk_native::bridge;

fn main() {
    std::env::set_var("GOW_GFX_BACKEND", "native");
    println!("[bridge] is_enabled={}", bridge::is_enabled());

    // Simulate the resolver hitting the two bootstrap entries.
    bridge::dispatch("dxgi.dll", "CreateDXGIFactory1");
    bridge::dispatch("d3d11.dll", "D3D11CreateDevice");

    let mut ok = 0;
    for i in 0..5 {
        if bridge::present() {
            println!("[bridge] frame {i} presented");
            ok += 1;
        } else {
            println!("[bridge] frame {i} FAILED");
            break;
        }
    }
    println!("[bridge] {ok}/5 frames");
    std::process::exit(if ok == 5 { 0 } else { 1 });
}
