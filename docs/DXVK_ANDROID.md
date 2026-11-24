# DXVK Native for Android (aarch64) — build recipe (VERIFIED)

Phase 1 of `docs/GRAPHICS.md`: build `libdxvk_d3d11.so` + `libdxvk_dxgi.so`
(D3D11/DXGI → Vulkan) for `aarch64-linux-android`, ship them in the APK, and let
`runtime/src/graphics.rs` (`GraphicsBridge`) route the guest's `D3D11CreateDevice`
/ `CreateDXGIFactory1` IAT slots to them.

> **Status: VERIFIED.** The recipe in §10 is the one that actually ran green in
> CI (job `dxvk` in `.github/workflows/build.yml`) and produced both `.so`s with
> the expected exports. Everything below §10 is the original research notes that
> led there; where a claim was corrected by the real build it is marked
> **[built]**. The exact patches are `tools/patches/*.patch` (git-applyable), not
> prose.

## 10. The verified recipe (what actually builds)

This is the authoritative, working procedure. The CI job `dxvk` implements it
verbatim.

**Inputs**

| thing | value |
|---|---|
| DXVK source | `https://github.com/doitsujin/dxvk`, tag **`v3.1.1`** (`--recursive`) |
| target | `aarch64-linux-android29-clang` (NDK **r27 / 27.2.12479018**) |
| host | `ubuntu-latest`, `meson` (≥1.0), `ninja-build`, `glslang-tools`, `python3` |
| patches | `tools/patches/000{1,2,3,4}-*.patch` (P1–P6, see §11) |
| output | `libdxvk_d3d11.so`, `libdxvk_dxgi.so` (unversioned SONAME) |

**Steps** (exactly what the workflow does):

```bash
# 1. host tools
sudo apt-get install -y ninja-build glslang-tools meson
[ "$(meson --version | cut -d. -f1)" -lt 1 ] && pip3 install --break-system-packages --upgrade meson

# 2. source (submodules include dxbc-spirv, libdisplay-info, Vulkan/SPIRV headers)
git clone --recursive --depth 1 --branch v3.1.1 https://github.com/doitsujin/dxvk.git dxvk-src

# 3. patches
cd dxvk-src && for p in ../tools/patches/*.patch; do git apply "$p"; done

# 4. cross file (written by CI into android-arm64.ini), see §10.1

# 5. configure — NOTE: there is no -Dnative=true; native mode follows from
#    system != 'windows'. The Android WSI patch removes the SDL3/GLFW requirement.
meson setup build dxvk-src --cross-file android-arm64.ini --buildtype release \
  -Dnative_sdl2=disabled -Dnative_sdl3=disabled -Dnative_glfw=disabled \
  -Denable_d3d8=false -Denable_d3d9=false -Denable_d3d10=false \
  -Denable_d3d11=true -Denable_dxgi=true

# 6. build only the two libs
ninja -C build src/d3d11/libdxvk_d3d11.so src/dxgi/libdxvk_dxgi.so

# 7. verify + strip
llvm-readelf --dyn-syms -W build/src/dxgi/libdxvk_dxgi.so   | grep -E 'CreateDXGIFactory1|dxvk_android_set_window'
llvm-readelf --dyn-syms -W build/src/d3d11/libdxvk_d3d11.so | grep -E 'D3D11CreateDevice|dxvk_android_set_window'
llvm-strip --strip-debug build/src/d3d11/libdxvk_d3d11.so build/src/dxgi/libdxvk_dxgi.so
```

The artifacts are published to the release named by the repo variable
**`DXVK_OUT_TAG`** (`dxvk-latest`) and consumed by the `android` job, which
downloads them into `android/app/src/main/jniLibs/arm64-v8a/` so they land in
`lib/arm64-v8a/` in the APK.

### 10.1 Meson cross file (`android-arm64.ini`)

```ini
[binaries]
c          = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang'
cpp        = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang++'
ar         = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar'
strip      = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip'
pkg-config = 'pkg-config'

[properties]
needs_exe_wrapper = false

[host_machine]
system     = 'android'      # keys P1/P3; also selects the Android WSI
kernel     = 'linux'
cpu_family = 'aarch64'
cpu        = 'aarch64'
endian     = 'little'

[built-in options]
c_args        = ['-fPIC']
cpp_args      = ['-fPIC']
c_link_args   = ['-Wl,-z,max-page-size=16384']
cpp_link_args = ['-Wl,-z,max-page-size=16384']
```

### 10.2 What the first real build confirmed / corrected

- **[built]** `-Dnative=true` does not exist and is not needed: `-Dnative_sdl2
  /sdl3/glfw=disabled` are the real options and native mode is chosen by
  `platform != 'windows'` automatically. The old `tools/build_dxvk_android.sh`
  has been updated to drop `-Dnative=true`/`-Denable_glslang`.
- **[built]** With `system='android'` the P3 patch replaces the
  *"SDL3, SDL2, or GLFW are required"* `error()` with `-DDXVK_WSI_ANDROID`, so
  **no SDL3/GLFW is linked at all**.
- **[built]** P1 (gate `-msse*` behind x86) is included even though
  `get_supported_arguments()` would filter `-mfpmath=sse` anyway; it makes the
  build independent of meson's probe behaviour.
- **[built]** P5b is required: without it meson emits
  `libdxvk_dxgi.so.0.31.1` with `SONAME libdxvk_dxgi.so.0` and Android packages
  neither the version suffix nor a loadable `DT_NEEDED`. The patch makes both
  libs unversioned (`libdxvk_dxgi.so`) with matching `DT_NEEDED`.
- **[built]** `dxvk_android_set_window` needs the **version-script** edit in
  `d3d11.sym`/`dxgi.sym` (P5a); the `visibility("default")` attribute alone is
  not enough because the scripts default to `local: *`.
- **[built]** The `dxbc-spirv` and `libdisplay-info` submodules are fetched via
  `--recursive`; there is no `.wrap` fallback, so a non-recursive clone fails at
  `subproject()`.

### 10.3 Runtime wiring (what the host must do) — implemented

`runtime/src/graphics.rs` + `runtime/src/jni.rs`:

1. `GraphicsBridge` keeps its loaded libraries in **process-global** state so the
   JNI surface callbacks and the import resolver share one set of `dlopen`
   handles (the two libs must not be loaded twice).
2. `autoload()` sets `DXVK_WSI_DRIVER=Android` **before** any D3D call, then
   `dlopen`s `libdxvk_dxgi.so` first (its SONAME is `d3d11`'s `DT_NEEDED`), then
   `libdxvk_d3d11.so`, both with `RTLD_NOW | RTLD_GLOBAL`. `autoload` is called
   from `run_game_library` in `jni.rs`, before the guest entry point runs.
3. `set_android_window(ANativeWindow*)` `dlsym`s `dxvk_android_set_window` in
   **both** libraries and calls it — each `.so` has its own `g_androidWindow`
   (dxgi owns the surface, d3d11 queries the window size), so pushing to only one
   is a silent no-op for the other. Missing symbol ⇒ loud log, never silent.
4. `jni.rs::nativeSurfaceCreated` resolves the `ANativeWindow` and pushes it to
   both libs; `nativeSurfaceChanged` re-pushes; `nativeSurfaceDestroyed` pushes
   `nullptr` so DXVK reports the display as occluded/minimized instead of
   presenting to a dead window.
5. `call_addr`/`call_addr_raw` forward **10** Microsoft-x64 arguments (args 1–4
   from `rcx,rdx,r8,r9`, args 5–10 from `[rsp+0x28]..[rsp+0x50]`), which is
   exactly the arity of `D3D11CreateDevice`.

### 10.4 Still open

- **`D3DReflect` / `d3dcompiler_47`.** DXVK does not export it (see §4). It is
  served by `runtime/src/dxbc.rs` (`win32.rs` intercepts
  `d3dcompiler_47.dll!D3DReflect` **before** the DXVK route), so it never needs a
  DXVK symbol. Whether GoW's usage exercises the parsing paths that `dxbc.rs`
  implements is still unverified against the real game.
- **Swapchain format.** GoW may create the swapchain from a format obtained via
  `IDXGIOutput::GetDisplayModeList`; the Android WSI reports one mode (the view
  size), so this *should* work, but it is unverified for GoW.
- **Frame pacing / vsync** on the Adreno 725 (`DXVK_FRAME_RATE`) is untuned.

---

## 0. Verdict up front

1. **DXVK Native builds for any non-Windows target by *configuration*, not by a
   flag.** There is **no `-Dnative=true`** option. Native mode is auto-selected
   when the cross/target system is not `windows` (`platform != 'windows'` in
   `meson.build`). Our `tools/build_dxvk_android.sh` passes `-Dnative=true`
   (ignored/fatal) and `-Denable_glslang=disabled` (not an option) — both are
   wrong and must be replaced. **[V]**
2. The two `.so`s are produced by the meson targets
   `src/d3d11/libdxvk_d3d11.so` and `src/dxgi/libdxvk_dxgi.so`. **[V]**
3. DXVK exports **`D3D11CreateDevice`** (from d3d11) and **`CreateDXGIFactory1`**
   (from dxgi) with exactly those names, unmangled, cdecl. **[V]**
4. **DXVK does not provide `D3DReflect` / `d3dcompiler_47`.** Our bridge lists
   `d3dcompiler_47.dll!D3DReflect` in `D3D_BOOTSTRAP`, but DXVK is not the source
   for it. This is a real, unresolved dependency — see §4.
5. Presentation: DXVK's Vulkan surface is created with `vkCreateAndroidSurfaceKHR`
   from an `ANativeWindow`. The SDL3 WSI route needs a permanent SDL3 dependency;
   the cleaner route for us is a 137-line **Android WSI backend** (already written
   by the Mirror's Edge port) that takes the `ANativeWindow` directly. §2/§5.
6. Two runtime-side gaps in *our* code make the bridge insufficient as-is: the
   argument-forwarding shim passes only 6 args (DXVK needs up to 10), and the
   two `.so`s must each be told the window. §6.

---

## 1. How DXVK Native builds (verified option names)

### 1.1 The `native` concept

`meson.build` branches on `platform = target_machine.system()`:

```meson
if platform == 'windows'
  ...  # PE/mingw path, dxvk_name_prefix = ''
else
  dxvk_include_dirs += ['./include/native', './include/native/windows', './include/native/directx']
  ...
  dxvk_name_prefix = 'dxvk_'      # -> libdxvk_d3d11.so, libdxvk_dxgi.so etc.
  dxvk_so_version  = {'version': '0' + <maj><min><pat>}   # SONAME libdxvk_d3d11.so.0
  lib_sdl3 = dependency('sdl3', required: get_option('native_sdl3'))
  lib_sdl2 = dependency('sdl2', required: get_option('native_sdl2'))
  lib_glfw = dependency('glfw3', required: get_option('native_glfw'))
  if lib_sdl3.found() -> -DDXVK_WSI_SDL3 ...
  if (not lib_sdl3.found() and not lib_sdl2.found() and not lib_glfw.found())
    error('SDL3, SDL2, or GLFW are required to build dxvk-native')   # 2.5+, 3.x
  endif
endif
```

So there is **no option named `native`**. This was verified in
`meson_options.txt` for 3.1.1, master, 2.6, 2.3, 2.0, 1.10.3, and the maintained
`Joshua-Ashton/dxvk-native` fork. **[V]**

### 1.2 Real option names, by version

| Version | WSI selection option(s) | Required? |
|---|---|---|
| `doitsujin/dxvk` **3.1.1 / master** | `-Dnative_sdl3=enabled\|disabled`, `-Dnative_sdl2=…`, `-Dnative_glfw=…` (features) | needs ≥1 enabled **unless** Android WSI patch is applied |
| `doitsujin/dxvk` **2.6–2.7** | `-Ddxvk_native_wsi=sdl3\|sdl2\|glfw` (string), plus legacy `dxvk_native_force` in the old `misyltoad` fork | needs SDL2/3/GLFW present |
| `doitsujin/dxvk` ≤ **2.3** | `-Ddxvk_native_wsi=…` (string) | needs SDL2/GLFW |
| `misyltoad/dxvk-native` (archived, 1.9.2) | `-Ddxvk_native_force=true` + `-Ddxvk_native_wsi=sdl2` | SDL2 |

**[V]** for all rows. **`-Ddxvk_native_force` was removed upstream after 1.9.2** — the
3.x tree does not define it (it has `native_sdl2`/`native_sdl3`/`native_glfw`/
`native_xcb`/`native_wayland` instead). **[V]**

Non-options (do not exist, will not fail loudly, will just be ignored or error):
`-Dnative=true`, `-Denable_glslang=…`, `-Denable_tests=…` (only in the old fork).

### 1.3 glslang and the DXBC subproject

- `glslang` is a **build-time host program**, not an option:
  `find_program('glslang','glslangValidator')` with no fallback → hard error if
  missing. It compiles DXVK's own GLSL (present/blit shaders) into `*.h`. It must
  be a **host** binary on the CI runner. **[V]**
- 3.x moved the internal DXBC parser out to a subproject:
  `subproject('dxbc-spirv')` (`.gitmodules` → `doitsujin/dxbc-spirv`). A clone
  must be `--recursive`, or meson fails at `subproject()`. Likewise
  `libdisplay-info` has a `fallback: ['libdisplay-info','di_dep']`, so its
  submodule must be present. **[V]**
- Relevant submodules (`.gitmodules`): `include/vulkan` (Vulkan-Headers),
  `include/spirv` (SPIRV-Headers), `include/native/directx`
  (`Joshua-Ashton/mingw-directx-headers`), `subprojects/libdisplay-info`,
  `subprojects/dxbc-spirv`. **[V]**

### 1.4 SSE flags — the "gate SSE" blocker is largely obsolete

Every version from 2.6 through master wraps the argument list in
`cpp.get_supported_arguments(compiler_args)` / `cc.get_supported_arguments(...)`,
where the list contains `-msse -msse2 -msse3 -mfpmath=sse`. On an aarch64 target
clang *rejects* `-mfpmath=sse` ("the 'sse' unit is not supported with this
instruction set"), so meson **filters it out automatically**. **[V]**

Therefore the C&C Generals "gate `-msse` behind `cpu_family == 'x86'`" patch is
**not required** on 2.6+/3.x. It is harmless/defensive. Our plan includes it as a
P-low patch only so the build is independent of meson probe behaviour. (In ≤2.3-
era forks the flags were added differently; if we pinned an old fork we would need
it. We pin a modern tree.)

---

## 2. Why Android is special: the window-system layer (WSI)

DXVK Native replaces Win32 windows with a "WSI" backend. The backend is chosen at
runtime by the `DXVK_WSI_DRIVER` environment variable:

```cpp
std::string hint = dxvk::env::getEnvVar("DXVK_WSI_DRIVER");
if (hint == "") {
#if defined(DXVK_WSI_WIN32)
  hint = "Win32";
#else
  throw DxvkError("DXVK_WSI_DRIVER environment variable unset");   // must be set!
#endif
}
```

**[V]** (`src/wsi/wsi_platform.cpp`). On Android we therefore **must** set
`DXVK_WSI_DRIVER` before the first DXVK call.

Each backend implements `WsiDriver` (`src/wsi/wsi_platform.h`), whose methods
include `getInstanceExtensions()` and `createSurface(...)`. **[V]**

### 2.1 Route A — SDL3 WSI (what C&C Generals shipped)

`SDLXWsiDriver` dlopens SDL3 and delegates:

- `getInstanceExtensions()` → `SDL_Vulkan_GetInstanceExtensions()`.
- `createSurface()` → `SDL_Vulkan_CreateSurface(window, instance, nullptr, pSurface)`.
- `HWND` is just a cast `SDL_Window*` (`native_sdl3.h`). **[V]**

On Android, SDL3's Android Vulkan backend returns
`{ VK_KHR_surface, VK_KHR_android_surface }` and implements
`SDL_Vulkan_CreateSurface` with `vkCreateAndroidSurfaceKHR` using the
`SDL_Window`'s `ANativeWindow`. **[V]** (`libsdl-org/SDL`,
`src/video/android/SDL_androidvulkan.c`.)

Generals' real fixes to this route (`Patches/dxvk-android.patch`): **[V]**

- `src/wsi/sdl3/wsi_platform_sdl3.cpp`: add an `#elif defined(__ANDROID__)`
  branch loading `"libSDL3.so"` (Android packs the *unversioned* soname; the
  Linux branch loads `"libSDL3.so.0"` which does not exist in an APK).
- `src/wsi/sdl3/wsi_window_sdl3.cpp`: use `SDL_GetWindowSizeInPixels` instead of
  `SDL_GetWindowSize` (high-DPI: size the swapchain in **pixels**, not logical
  points).
- `wsi_platform_sdl3_funcs.h`: declare `SDL_GetWindowSizeInPixels`.

**Cost:** we must ship and initialise SDL3 just for DXVK, although our app has its
own `SurfaceView`/`ANativeWindow`. Not attractive.

### 2.2 Route B — dedicated Android WSI (recommended)

The Mirror's Edge Android port (`singlemate/RunnerRecompiled`,
`pc/dxvk/dxvk-3.1.1-android.patch`) adds a self-contained backend
`src/wsi/android/wsi_platform_android.cpp` (~137 lines): **[V]**

- one monitor (the app's view), named `"Android"`;
- an exported entry point `dxvk_android_set_window(ANativeWindow*)` that stores
  the window in an atomic global;
- `getInstanceExtensions()` → `{ VK_KHR_surface, VK_KHR_android_surface }`;
- `createSurface()` → resolves `vkCreateAndroidSurfaceKHR` via
  `pfnVkGetInstanceProcAddr` and calls it with the stored `ANativeWindow`;
- display modes = the view size at 60 Hz (the engine picks its ResX/ResY and the
  swapchain scales).

The meson change replaces the SDL error with an Android branch:

```meson
if platform == 'android'
  compiler_args += ['-DDXVK_WSI_ANDROID']
elif (not lib_sdl3.found() and not lib_sdl2.found() and not lib_glfw.found())
  error('SDL3, SDL2, or GLFW are required to build dxvk-native')
endif
```

and `src/wsi/meson.build` adds the file and links `libandroid`
(`elif platform == 'android' → wsi_deps += [cpp.find_library('android')]`). **[V]**

This removes the SDL3 coupling entirely: our `SurfaceView` → `ANativeWindow`
(from `GowActivity.surfaceCreated` → `nativeSurfaceCreated` →
`ANativeWindow_fromSurface`, already implemented in `runtime/src/jni.rs`) is
handed straight to DXVK. **This is the route we plan to take.**

### 2.3 Answering "does Vulkan have to go through the Android WSI?"

**Yes.** On Android there is no `VK_KHR_xcb_surface`/`…_win32_surface`; the only
presentation path for a `SurfaceView`/`ANativeWindow` is
`VK_KHR_android_surface` + `vkCreateAndroidSurfaceKHR`. Both routes above
ultimately call exactly that. Our own `runtime/src/vk.rs` already does
`vkCreateAndroidSurfaceKHR` for the phase-2 bootstrap, so the pattern is proven in
our tree. **[V]**

### 2.4 Which `.so` owns the surface (important)

`wsi_lib` is a **static** library linked into `dxvk_lib` (static), which is
linked into **both** `libdxvk_dxgi.so` and `libdxvk_d3d11.so`. So:

- each `.so` contains its **own copy** of the WSI driver and of
  `g_androidWindow`;
- `DXVK_WSI_DRIVER` is read inside each lib's own `wsi::init()`;
- the swapchain/surface is created by DXGI (`dxgi_swapchain.cpp`), i.e. inside
  **`libdxvk_dxgi.so`**; D3D11 also queries the WSI (window size, monitor). **[I,
  from `src/dxvk/meson.build`, `src/dxgi/meson.build`, `src/wsi/wsi_platform.cpp`]**

**Consequence:** the host must call `dxvk_android_set_window()` on **both**
libraries, and each `.sym` must export it. See §6.

---

## 3. Blocker / fix matrix

| # | Blocker | Where | Fix | Status |
|---|---|---|---|---|
| B1 | `-mfpmath=sse` on aarch64 | `meson.build` arg list | auto-filtered by `get_supported_arguments` on 2.6+; optional explicit gating | **[V]** not needed |
| B2 | SDL3 WSI loads `libSDL3.so.0` | `src/wsi/sdl3/wsi_platform_sdl3.cpp` | patch to `libSDL3.so` on `__ANDROID__` | **[V]** (Generals) |
| B3 | swapchain sized in logical points on high-DPI | `src/wsi/sdl3/wsi_window_sdl3.cpp` | use `SDL_GetWindowSizeInPixels` | **[V]** (Generals) |
| B4 | no SDL/GLFW ⇒ meson `error()` on Android | `meson.build` | Android WSI patch replaces it with `if platform == 'android'` | **[V]** (Mirror's Edge) |
| B5 | DXVK needs an `ANativeWindow` | new `src/wsi/android/wsi_platform_android.cpp` | add the Android WSI backend + `dxvk_android_set_window` | **[V]** (Mirror's Edge) |
| B6 | `dxvk_android_set_window` not exported | `src/d3d9/d3d9.sym` (Mirror's Edge) | add to **`d3d11.sym`** and **`dxgi.sym`** too | **[I]** |
| B7 | versioned SONAME (`libdxvk_dxgi.so.0`) unloadable from an APK | `meson.build` `dxvk_so_version` | drop the `version` kwarg on Android (unversioned SONAME) or `patchelf` | **[I]** |
| B8 | engine's fullscreen format-selection path on non-Windows | the *game*, not DXVK | app-side: force windowed `_PresentParameters`, fall back to `D3DFMT_X8R8G8B8` when `GetAdapterDisplayMode` returns `UNKNOWN` | **[V]** (Generals, `dx8wrapper.cpp`) |
| B9 | D3DReflect/d3dcompiler_47 absent | — | separate implementation (Rust DXBC reflection) or stub | **[V]** gap, §4 |

B8 is about the game's D3D8/D3D9 setup, not GoW. GoW is D3D11 and calls
`D3D11CreateDevice` + `CreateDXGIFactory1`; the analogous "no desktop display
modes" trap for us is `CreateSwapChain` with a `DXGI_SWAP_CHAIN_DESC` whose
`BufferDesc.Format` came from a failed `EnumDisplayModes`. Watch for it in the
guest's swapchain setup; the DXGI WSI answers `getCurrentDisplayMode` with the
view size, so a sane engine will succeed. **[I]**

---

## 4. The `D3DReflect` / `d3dcompiler_47` gap (critical)

`runtime/src/graphics.rs` has:

```rust
pub const D3D_BOOTSTRAP: &[(&str, &str)] = &[
    ("d3d11.dll", "D3D11CreateDevice"),
    ("dxgi.dll", "CreateDXGIFactory1"),
    ("d3dcompiler_47.dll", "D3DReflect"),   // <-- DXVK cannot satisfy this
];
```

Verified facts:

- DXVK's `src/d3d11/d3d11.sym` exports only
  `D3D11CoreCreateDevice, D3D11CreateDevice, D3D11CreateDeviceAndSwapChain,
  D3D11On12CreateDevice`. **[V]**
- DXVK's `src/dxgi/dxgi.sym` exports only
  `CreateDXGIFactory2/1, CreateDXGIFactory, DXGIDeclareAdapterRemovalSupport,
  DXGIGetDebugInterface1`. **[V]**
- There is **no** `D3DReflect` symbol anywhere in DXVK master (the only match for
  "reflect"/"compiler" in the tree is the `dxbc-spirv` subproject). **[V]**
- `d3dcompiler_47.lib`/`libd3dcompiler_47.def` is referenced by **`misyltoad/
  dxvk-native` (the archived 1.9.2 fork)** as a *Windows* import library — it is
  not an Android implementation. **[V]**

So on Android there is no `d3dcompiler_47` to load. Options, in order of
preference:

1. **Implement reflection in Rust in our runtime (phase-2-aligned).** DXBC has a
   well-defined container with `RDEF`/`ISGN`/`ISG1`/`OSGN`/`OSG5`/`SHEX`/`SHDR`
   chunks. `ID3D11ShaderReflection` is a COM object; our runtime already models
   COM vtables as host thunks (`runtime/src/hostcall.rs`), so a Rust reflection
   object fits the existing design. Non-trivial but bounded.
2. **Port vkd3d's `D3DReflect`** (vkd3d-utils), cross-compiled for Android.
3. **Return `E_NOTIMPL` and see whether GoW still boots.** Engines that use
   `CreateInputLayout` with the raw shader blob (which DXVK implements via
   dxbc-spirv) do not need `D3DReflect`; engines that call `D3DReflect` to read
   the input signature to *build* the layout do. GoW's IAT imports
   `D3DReflect`, so it is likely used. **[I]**

Whatever we choose, `D3D_BOOTSTRAP`'s third entry must be routed to *our*
implementation, not to a DXVK `.so`. This is the single biggest unknown in the
phase-1 plan and is tracked as an open question (§8).

---

## 5. Build recipe (CI)

### 5.1 Prerequisites

- **Host:** `ubuntu-latest` x86-64 (matches `.github/workflows/build.yml`).
- **NDK:** r27 (`27.2.12479018`, already in the workflow). API 29 wrappers
  (`aarch64-linux-android29-clang`) — matches `minSdk = 29` in
  `android/app/build.gradle.kts`. **[V]**
- **meson ≥ 0.58**, **ninja**, **glslang** (host), **python3**, git with
  submodules. `apt-get install -y meson ninja-build glslang-tools` (or pip/brew).
- **DXVK source:** pin a tag. Choose **`v3.1.1`** (what the only *working* Android
  D3D port pinned) with `--recursive`. **[V]**

### 5.2 Meson cross file (new file, written by CI into the build dir)

`android-arm64.ini` (Route B — Android WSI):

```ini
[binaries]
c          = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang'
cpp        = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang++'
ar         = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar'
strip      = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip'
pkg-config = 'pkg-config'
windres    = ''

[properties]
needs_exe_wrapper = false

[host_machine]
system     = 'android'      # MUST be non-'windows'; 'android' unlocks the Android WSI patch
cpu_family = 'aarch64'
cpu        = 'aarch64'
endian     = 'little'

[built-in options]
c_args        = ['-fPIC']
cpp_args      = ['-fPIC']
c_link_args   = ['-Wl,-z,max-page-size=16384']
cpp_link_args = ['-Wl,-z,max-page-size=16384']
```

**Why `system = 'android'`:** the Android WSI patch keys off
`platform == 'android'`. If we instead used `system = 'linux'` we are on Route A
and **must** set `-Dnative_sdl3=enabled` and have an SDL3 `.pc` on
`PKG_CONFIG_PATH`, plus the B2/B3 patches. **[V]**

If we keep the SDL3 route temporarily, `# ANDROID` and `system = 'linux'` work;
the host must provide an SDL3 pkg-config file (Generals writes a shim `.pc`). **[V]**

### 5.3 Configure + build (Route B, DXVK 3.1.1)

```bash
git clone --recursive --depth 1 --branch v3.1.1 \
  https://github.com/doitsujin/dxvk.git "$OUT/dxvk-src"
git -C "$OUT/dxvk-src" submodule update --init --recursive

# apply tools/dxvk_android_patches.md (P1..P6) here

meson setup "$OUT/build" "$OUT/dxvk-src" \
  --cross-file "$OUT/android-arm64.ini" \
  --buildtype release \
  -Dnative_sdl2=disabled -Dnative_sdl3=disabled -Dnative_glfw=disabled \
  -Denable_d3d9=false -Denable_d3d8=false -Denable_d3d10=false \
  -Denable_d3d11=true -Denable_dxgi=true

ninja -C "$OUT/build" src/d3d11/libdxvk_d3d11.so src/dxgi/libdxvk_dxgi.so
```

Notes:
- On **2.6/2.7** replace the three `native_*` flags with
  `-Ddxvk_native_wsi=sdl3` (and use the SDL3 route, or port the Android patch
  down; the Android WSI patch as published targets 3.1.1). **[V]**
- `glslang` must be resolvable **on the host** (`find_program`); the generated
  `*.h` are consumed by the target build.
- Skip `-Denable_d3d9=false` etc. if you actually want those frontends; GoW needs
  only d3d11 + dxgi. `enable_d3d11` **requires** `enable_dxgi` (meson errors
  otherwise). **[V]**

### 5.4 Post-process

The built artefacts are **versioned**:
`libdxvk_dxgi.so.0.31.1` (SONAME `libdxvk_dxgi.so.0`),
`libdxvk_d3d11.so.0.31.1`, with `.so` symlinks. Android packages only
`lib<name>.so` from `jniLibs`. Two ways to fix (B7):

- **Preferred:** patch `meson.build` so `dxvk_so_version = {}` on Android
  (yields `libdxvk_d3d11.so` with SONAME `libdxvk_d3d11.so`). Patch P5.
- Or keep meson output and run `patchelf`:
  ```bash
  patchelf --set-soname libdxvk_dxgi.so  libdxvk_dxgi.so.0.31.1
  patchelf --replace-needed libdxvk_dxgi.so.0 libdxvk_dxgi.so libdxvk_d3d11.so.0.31.1
  mv libdxvk_dxgi.so.0.31.1  libdxvk_dxgi.so
  mv libdxvk_d3d11.so.0.31.1 libdxvk_d3d11.so
  ```
  `patchelf` in the runner: `apt-get install -y patchelf`. (Not needed if P5 is
  applied.) **[I]**

Verify the result:

```bash
llvm-readelf -d libdxvk_d3d11.so | grep -E 'SONAME|NEEDED'
# expect: NEEDED libdxvk_dxgi.so, libc.so, libdl.so, libm.so, libandroid.so, liblog.so
llvm-readelf --dyn-syms libdxvk_dxgi.so | grep -E 'CreateDXGIFactory1|dxvk_android_set_window'
llvm-readelf --dyn-syms libdxvk_d3d11.so | grep -E 'D3D11CreateDevice|dxvk_android_set_window'
```

### 5.5 Meson targets that produce the two `.so`s

| Target (ninja path) | Meson definition | Output |
|---|---|---|
| `src/dxgi/libdxvk_dxgi.so` | `shared_library('dxgi', …)` in `src/dxgi/meson.build`, name prefix `dxvk_` | `libdxvk_dxgi.so` |
| `src/d3d11/libdxvk_d3d11.so` | `shared_library('d3d11', …)` in `src/d3d11/meson.build`; links `dxgi_dep` | `libdxvk_d3d11.so` |

**[V]** (both `meson.build` files fetched). `ninja -C build` builds everything;
the explicit two targets avoid the d3d8/d3d9 frontends.

---

## 6. Runtime loading and COM interop

### 6.1 Web of dependencies

`libdxvk_d3d11.so` **depends on** `libdxvk_dxgi.so` (`dxgi_dep` →
`link_with: dxgi_dll`, producing a `DT_NEEDED`). Both depend on the system
`libvulkan.so` loader (loaded at runtime by `vulkan_loader.cpp`, which tries
`"libvulkan.so"` then `"libvulkan.so.1"` on non-Windows) and `libandroid`. **[V]**

So `libdxvk_dxgi.so` **must** be next to `libdxvk_d3d11.so` in the APK, and its
SONAME must match the `DT_NEEDED` (§5.4). To be safe, pre-`dlopen` dxgi with
`RTLD_GLOBAL` before d3d11, or just `dlopen("libdxvk_d3d11.so")` after patching
the SONAME; Android's per-app library search path includes `nativeLibraryDir`.

### 6.2 Symbol names (exact)

| dll key | symbol | exported by |
|---|---|---|
| `d3d11.dll` | `D3D11CreateDevice` | `libdxvk_d3d11.so` |
| `dxgi.dll` | `CreateDXGIFactory1` | `libdxvk_dxgi.so` |
| — | `dxvk_android_set_window` | both (after patch P4) |

`__stdcall` is `#define`d empty on non-Windows (`include/native/windows/
windows_base.h`), and the entry points are inside `extern "C" { … }`, so the
names are unmangled C symbols; `dlsym` resolves them directly. **[V]**

### 6.3 What `GraphicsBridge` already does vs. what it needs

`runtime/src/graphics.rs` already has `load_library` (dlopen), `resolve` (dlsym)
and `autoload(native_lib_dir)` which looks for `libdxvk_d3d11.so` +
`libdxvk_dxgi.so`. That matches the packaged names after §5.4. ✅

Two gaps must be closed **in our runtime** (not DXVK):

1. **Set the window on both libs, and set the WSI driver env var.** Before any
   D3D call:
   ```rust
   std::env::set_var("DXVK_WSI_DRIVER", "Android");   // else wsi::init throws
   // after dlopen of each handle:
   //   dlsym(h, "dxvk_android_set_window") -> set_window(anative_window)
   ```
   Both `libdxvk_d3d11.so` and `libdxvk_dxgi.so` hold their own `g_androidWindow`
   (§2.4). The window handle already arrives via
   `Java_com_gow_arm64_GowActivity_nativeSurfaceCreated` →
   `android_window_from_surface` (`runtime/src/jni.rs`). Store it and push it to
   both libs, and re-push on `surfaceChanged`/recreation.
2. **Forward up to 10 arguments, Microsoft-x64 → AAPCS64.** DXVK entry points
   take up to 10 args, e.g.
   `D3D11CreateDevice(pAdapter, DriverType, Software, Flags, pFeatureLevels,
   FeatureLevels, SDKVersion, ppDevice, pFeatureLevel, ppImmediateContext)`. The
   current shim (`call_addr`, `call_addr_raw`) forwards only 6 (`rcx,rdx,r8,r9` +
   two stack slots at `rsp+0x28/0x30`). Microsoft x64 places args ≥5 at
   `[rsp+0x28]`, `[rsp+0x30]`, `[rsp+0x38]`, `[rsp+0x40]`, `[rsp+0x48]`, so the
   shim must read at least `rsp+0x28 … rsp+0x48` (5 stack args ⇒ 12 total) and
   pass them in the host's `x0..x7` + host stack. **[V of the DXVK signature and
   of our current 6-arg shim; I for the exact fix]**

   > Note: DXVK's return value is a plain `HRESULT` in `x0` (→ guest `rax`);
   > nothing else needs marshalling for the bootstrap calls. COM vtables returned
   > by DXVK are real host pointers into DXVK's `.so`, so subsequent
   > guest→vtable calls are ordinary host calls through our pseudo-address/`hostcall`
   > machinery, provided we register the vtable addresses.

### 6.4 Calling convention summary (for the bridge author)

- Guest: Microsoft x64 — `RCX,RDX,R8,R9`, then stack; 32-byte shadow space.
- Host DXVK on aarch64: AAPCS64 — `x0..x7`, then stack.
- Our `GowCpu` keeps the guest's "registers" as fields; `call_addr` currently
  transmutes to `extern "system" fn(u64×6)` (on aarch64 `system` == `C`).
  Extend it to 12 args (or 10) so DXVK's bootstrap calls are complete.

---

## 7. Packaging

- Put the two `.so`s under `android/app/src/main/jniLibs/arm64-v8a/`
  (`libdxvk_d3d11.so`, `libdxvk_dxgi.so`). The Gradle config already uses
  `src/main/jniLibs` and `useLegacyPackaging = true`. **[V]**
- `-Wl,-z,max-page-size=16384` in the cross file future-proofs for Android 15
  16 KB-page devices (our test device is Android 14 / 4 KB, so not strictly
  required today). **[V]**
- Strip debug info with the NDK `llvm-strip` before packaging (`~11.6 MB` for
  d3d9 in the Mirror's Edge build is a useful size reference; d3d11+dxgi will be
  larger). **[V]**
- `libadrenotools`/Turnip loading is unchanged: DXVK uses the process Vulkan
  loader (`libvulkan.so`), which our `driver_loader.c` already redirects to
  Turnip. **[I]**

---

## 8. Open questions / risks

1. **`D3DReflect` (§4).** Highest risk. Decide between a Rust DXBC reflection
   object, a vkd3d port, or a stub. Must be resolved for GoW if it reflects.
2. **Swapchain format path.** GoW may create the swapchain from a format obtained
   via `IDXGIOutput::GetDisplayModeList`; if it returns empty on Android it may
   pass `DXGI_FORMAT_UNKNOWN`. The WSI reports one mode (the view size), so this
   should be fine, but it is unverified for GoW. **[I]**
3. **`IPresent` / frame pacing.** DXVK vsync default and `DXVK_FRAME_RATE` may
   need tuning on the Adreno 725; watch for pacing stalls. **[I]**
4. **Thread model.** DXVK creates internal threads and uses `std::thread`; the
   runtime must have a working `pthread` layer (it does for the game already).
   The RunnerRecompiled patch also adds `std::lock_guard lock(m_mutex)` in
   `dxvk_memory.cpp::importBufferResource/importImageResource` (a thread-safety
   fix that matters with multithreaded device creation). Consider whether GoW
   needs it — include it as P6. **[V of the patch; I of the need]**
5. **Which DXVK version to pin.** 3.1.1 is the only version with a published
   *working* Android D3D patch; 2.6/2.7 would require porting P1–P5. Recommend
   3.1.1. **[V]**
6. **glslang on the runner.** Must be a native host binary; the NDK's own
   `glslangValidator` (if any) targets android and cannot run on the runner.
   **[I]**
7. **`libdisplay-info` / `dxbc-spirv` submodules** must be fetched in CI (shallow
   recursive clone). **[V]**

---

## 9. Sources (all fetched during research)

- DXVK upstream `meson.build` / `meson_options.txt` (master, v3.1.1, v2.6, v2.3,
  v2.0, v1.10.3):
  `https://github.com/doitsujin/dxvk` — e.g.
  `.../raw/master/meson.build`, `.../raw/v3.1.1/meson_options.txt`.
- DXVK WSI: `src/wsi/wsi_platform.cpp`, `src/wsi/wsi_platform.h`,
  `src/wsi/sdl3/wsi_platform_sdl3.cpp`, `src/wsi/sdl3/wsi_window_sdl3.cpp`,
  `src/wsi/meson.build`.
- DXVK exports: `src/d3d11/d3d11.sym`, `src/dxgi/dxgi.sym`, `src/d3d11/d3d11.def`,
  `src/dxgi/dxgi.def`, `src/d3d11/d3d11_main.cpp`, `src/dxgi/dxgi_main.cpp`.
- DXVK build targets: `src/d3d11/meson.build`, `src/dxgi/meson.build`,
  `src/dxvk/meson.build`, `src/meson.build`.
- DXVK native headers: `include/native/windows/windows_base.h`,
  `include/native/wsi/native_sdl3.h`, `include/native/meson.build`,
  `src/dxgi/dxgi_include.h` (`DLLEXPORT`).
- DXVK vulkan loader: `src/vulkan/vulkan_loader.cpp`.
- DXVK submodules: `.gitmodules`.
- C&C Generals Zero Hour Android: `https://github.com/tarek369/GeneralsZH-Android`
  — `android.md`, `Patches/dxvk-android.patch`, `cmake/dx8.cmake`,
  `cmake/meson-android-aarch64-cross.ini.in`.
- Mirror's Edge Android: `https://github.com/singlemate/RunnerRecompiled` —
  `pc/dxvk/README.md`, `pc/dxvk/android-arm64.cross.txt`,
  `pc/dxvk/dxvk-3.1.1-android.patch`, `pc/runtime/rt_host_android.c`,
  `android/native/pc_jni.c`.
- `Joshua-Ashton/dxvk-native` (README) and the archived
  `misyltoad/dxvk-native` (`meson.build`, `meson_options.txt`,
  `package-native.sh`).
- SDL3 Android Vulkan: `libsdl-org/SDL`,
  `src/video/android/SDL_androidvulkan.c`.
- Wine spec (shows `D3DReflect` really lives in `d3dcompiler_47`, not DXVK):
  `wine-mirror/wine`, `dlls/d3dcompiler_47/d3dcompiler_47.spec`.
