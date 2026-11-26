//! Render the built-in triangle on the real GPU and present it over X11.
//!
//! Works on any X server (Xvfb for headless capture, the Termux:X11 app window
//! for the device). Turnip cannot present a swapchain without DRI3, so this uses
//! the render-to-image + `XPutImage` path.
//!
//! ```text
//! export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
//! export LD_LIBRARY_PATH=$PREFIX/lib
//! # one or both servers, e.g. an Xvfb :1 and the Termux:X11 app on :0:
//! export GOW_X11_DISPLAYS=":1,:0"
//! cargo run -p gow-runtime --example vk_native_x11_demo --release    # continuous
//! # or a fixed number of frames: ./vk_native_x11_demo 900  (args: frames)
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // GOW_X11_DEMO_FRAMES, else argv[1], else a short default for a quick check.
    let frames: u32 = std::env::var("GOW_X11_DEMO_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| args.get(1).and_then(|s| s.parse().ok()))
        .unwrap_or(120);
    let rc = gow_runtime::vk_native::selftest::run_x11(
        1280,
        720,
        frames,
        &mut |m| println!("{m}"),
    );
    std::process::exit(if rc == 0 { 0 } else { rc });
}
