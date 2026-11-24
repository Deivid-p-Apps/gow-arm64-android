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
are loaded by our Rust runtime (`runtime/src/graphics.rs`) and their Vulkan goes
through **libadrenotools** driving Turnip — the same mechanism emulators use for
custom Adreno drivers (see §"Custom driver (libadrenotools)" below). The old
`android/.../driver_loader.c` (a plain `dlopen` of the driver `.so`) is dead: it
never worked, because the Android loader ignores an ICD dropped into the process.

## Custom driver (libadrenotools)

**Why `dlopen` alone cannot work.** The Android Vulkan loader lives in
`/system/lib64/libvulkan.so` inside the classloader (isolated) linker namespace.
It enumerates ICDs through that namespace and loads the vendor driver by name; a
`dlopen("libvulkan_freedreno.so")` from our process loads the Turnip `.so` but
nothing ever calls `vk_icdGetInstanceProcAddr` on it, so it is inert.

**The correct path** is [`bylaws/libadrenotools`](https://github.com/bylaws/libadrenotools)
(BSD-2-Clause). `adrenotools_open_libvulkan(flags, ADRENOTOOLS_DRIVER_CUSTOM,
tmpLibDir, hookLibDir, driverDir, driverName, nullptr, nullptr)`:

1. creates a private linker namespace rooted at `hookLibDir` (= the app's
   `nativeLibraryDir`),
2. loads `libmain_hook.so` + `libhook_impl.so` into it; those intercept the
   loader's own `android_dlopen_ext` / `android_load_sphal_library` calls,
3. loads a **fresh copy of `libvulkan.so`** into that namespace with a patched
   SONAME (from a `memfd` on API ≥ 29), so the loader runs inside the namespace
   where the hook is active, and
4. returns a handle to *that* loader. Its driver load is redirected so the
   vendor ICD is replaced by `driverName` from `driverDir` (app-**private**
   storage only).

**Everything must then use the returned loader.** Its `vkGetInstanceProcAddr` is
the only way to reach an instance backed by the custom driver:

- `runtime/src/vk.rs` resolves every entry point through it when a driver is
  active, else through the system `libvulkan.so`.
- DXVK resolves Vulkan through one `vkGetInstanceProcAddr` (its
  `LibraryFn::sym`). We hand DXVK the adrenotools one with a small upstream
  patch, `tools/patches/0005-dxvk-adrenotools-loader-override.patch`, which adds
  the exported setter `dxvk_vulkan_set_get_instance_proc_addr(void*)`. The Rust
  runtime `dlsym`s it from **both** `libdxvk_dxgi.so` and `libdxvk_d3d11.so`
  (each `.so` has its own copy of the pointer, exactly like
  `dxvk_android_set_window`) and passes the loader's `vkGetInstanceProcAddr`
  before the first `D3D11CreateDevice`/`CreateDXGIFactory1` call, which is where
  DXVK lazily loads `libvulkan.so`.

So DXVK does **not** read an env var and is not patched to call adrenotools
itself (the older C&C Generals port relied on the hook catching DXVK's stock
`LoadLibraryA("libvulkan.so") = dlopen(...)`, which is fragile w.r.t. the
namespace the loader is dlopened from). We pass the loader pointer explicitly —
deterministic, and DXVK keeps working unchanged on the system driver when no
custom driver is selected.

**Packaging requirements (met):**

- `android/app/build.gradle.kts` keeps `useLegacyPackaging = true`, so
  `libadrenotools.so` and the four hook `.so`s are **extracted** into
  `nativeLibraryDir` (`libmain_hook.so`, `libhook_impl.so`,
  `libfile_redirect_hook.so`, `libgsl_alloc_hook.so`). If they were read from the
  APK (legacy packaging off) `adrenotools_open_libvulkan` would return a loader
  whose hook then fails and silently falls back to the vendor driver.
- `hookLibDir` is `nativeLibraryDir`. The Rust layer obtains it with `dladdr` on
  its own `.so`, so no JNI round-trip is needed.
- The driver `.so` must be in app-private storage; the launcher copies the chosen
  package there before calling `nativeLoadDriverFile` (see below).

**Verification:** green CI run **45473778373**. The `adrenotools` job builds
`libadrenotools.so` + hooks from the pinned commit
`8fae8ce254dfc1344527e05301e43f37dea2df80` (with the `lib/linkernsbypass`
submodule) and publishes them to release `adrenotools-latest`; the `android` job
stages them into `jniLibs/arm64-v8a/`. The APK's `lib/arm64-v8a/` then contains
`libadrenotools.so`, `libmain_hook.so`, `libhook_impl.so`,
`libfile_redirect_hook.so`, `libgsl_alloc_hook.so`, and DXVK exports
`dxvk_vulkan_set_get_instance_proc_addr`.

**Fallback.** With no driver chosen, or if `libadrenotools.so` is absent /
`adrenotools_open_libvulkan` fails, `adrenotools::instance_proc_addr()` returns
`None`, `vk.rs` and DXVK both use the system `libvulkan.so`, and the failure is
logged loudly. It never crashes.

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
