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

Not yet built in CI (needs the Android NDK + DXVK's Android patches). The runtime
already resolves the `d3d11.dll`/`dxgi.dll`/`D3DCOMPILER_47.dll` IAT slots and
will route them to the DXVK exports once the `.so`s are in the APK. Until then
the game cannot present a frame — and no FPS number is reported (see
`PERFORMANCE.md`).
