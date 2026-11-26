//! Standalone runner for the native Vulkan backend smoke test, on Termux.
//!
//! ```text
//! export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
//! export LD_LIBRARY_PATH=$PREFIX/lib
//! cargo run -p gow-runtime --example vk_native_selftest --release
//! ```
//!
//! Uses the headless surface (no ANativeWindow outside an Android app), so the
//! swapchain is real but pixels are not shown on screen.

fn main() {
    let frames: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let rc = gow_runtime::vk_native::selftest::run(
        std::ptr::null_mut(),
        frames,
        &mut |m| println!("{m}"),
    );
    std::process::exit(if rc == 0 { 0 } else { rc });
}
