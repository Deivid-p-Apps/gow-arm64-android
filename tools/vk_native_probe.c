// Native Vulkan backend harness: drives the runtime's smoke test from C, the
// same shape as tools/host_boot.c. Proves instance + device + swapchain +
// clear + present on the real Termux driver (Turnip), no DXVK, no APK.
//
// Build (from repo root), with the `vkprobe` feature so the entry point exists:
//   cargo build -p gow-runtime --release --features vkprobe
//   clang -O1 tools/vk_native_probe.c -L target/release -lgow_runtime \
//     -lpthread -ldl -lm -Wl,-rpath,target/release -o vk_native_probe
//   export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
//   export LD_LIBRARY_PATH=$PREFIX/lib
//   ./vk_native_probe            # headless surface, 3 frames
//   ./vk_native_probe 5          # 5 frames
//
// A non-null window pointer is not possible off an Android app (no
// ANativeWindow in Termux), so this always uses VK_EXT_headless_surface.
#include <stdio.h>
#include <stdlib.h>

// gow_vk_native_selftest(window, frames) -> 0 on success.
extern int gow_vk_native_selftest(void* window, unsigned frames);

int main(int argc, char** argv) {
    unsigned frames = argc > 1 ? (unsigned)atoi(argv[1]) : 3;
    fprintf(stderr, "[vk_native_probe] frames=%u (headless surface)\n", frames);
    int rc = gow_vk_native_selftest(NULL, frames);
    fprintf(stderr, "[vk_native_probe] returned %d\n", rc);
    return rc;
}
