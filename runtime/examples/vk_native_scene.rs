//! Render an animated 3D scene (depth-tested spinning cube) on the real GPU and
//! present it over X11.
//!
//! ```text
//! export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
//! export LD_LIBRARY_PATH=$PREFIX/lib
//! export GOW_X11_DISPLAYS=":1"        # Xvfb; ":0" = Termux:X11 app
//! cargo run -p gow-runtime --example vk_native_scene --release 400
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let frames: u32 = std::env::var("GOW_X11_DEMO_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| args.get(1).and_then(|s| s.parse().ok()))
        .unwrap_or(400);
    let rc = gow_runtime::vk_native::selftest::run_x11_scene(
        1280,
        720,
        frames,
        &mut |m| println!("{m}"),
    );
    std::process::exit(if rc == 0 { 0 } else { rc });
}
