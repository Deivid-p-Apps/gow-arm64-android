# Graphics layer (D3D11 -> Vulkan)

God of War (2018) PC renders through **Direct3D 11** (`d3d11.dll` + `dxgi.dll` +
`D3DCOMPILER_47.dll` for DXBC, plus `nvngx_dlss` which we skip). The port needs a
D3D11 implementation on top of Vulkan.

## Strategy (from the prompt, kept)

- **Phase 1 — DXVK Native (d3d11 + dxgi -> Vulkan).** Get the game on screen
  fast. DXVK had never been built for Android until the C&C Generals Zero Hour
  port did it; the required work is known and small: gate x86-only SSE
  intrinsics, fix the SDL3 WSI soname, patch the WSI for high-DPI Android
  surfaces. `tools/build_dxvk_android.sh` drives this on a build host.
- **Phase 2 — native Rust Vulkan backend (ash).** Translate D3D11 pipeline state
  and DXBC shaders to SPIR-V directly, no DXVK. Kept as the goal; DXVK stays as a
  fallback until the native backend reaches parity. This is the backend the final
  deliverable must use.

DXVK Native ships as `libdxvk_d3d11.so` + `libdxvk_dxgi.so`. On the device they
are loaded with `libadrenotools` driving Turnip (see `android/.../driver_loader.c`),
the same mechanism emulators use for custom Adreno drivers.

## Why not just Vulkan directly

The game calls D3D11; the recompiled code calls the D3D11 COM entry points through
its IAT. Those calls must land on something that speaks Vulkan. Options: DXVK
(proven, phase 1) or a from-scratch translator (phase 2). There is no path that
skips a D3D11 implementation entirely, because the game's own code is what issues
the draw calls.

## Current state

**Phase 1 backend is built and shipped.** The CI `dxvk` job
(`.github/workflows/build.yml`) cross-compiles DXVK Native v3.1.1 for
`aarch64-linux-android29` with the Android WSI backend, publishes
`libdxvk_d3d11.so` + `libdxvk_dxgi.so` to the `dxvk-latest` release, and the
`android` job stages them into the APK (`lib/arm64-v8a/`). Verified green run:
`45410666526`. The runtime (`runtime/src/graphics.rs` + `jni.rs`) loads both
libraries, sets `DXVK_WSI_DRIVER=Android`, and pushes the `ANativeWindow` into
both. Full verified recipe: `docs/DXVK_ANDROID.md` §10.

What is **not** yet proven: that the game gets a frame on screen. `D3DReflect`
is served by our own Rust DXBC parser (`runtime/src/dxbc.rs`), and the guest's
actual usage of it plus the swapchain-format path are untested against the real
game. Until a frame is confirmed, no FPS number is reported (see
`PERFORMANCE.md`).
