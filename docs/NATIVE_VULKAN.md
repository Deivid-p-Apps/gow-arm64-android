# Native Vulkan backend (`GOW_GFX_BACKEND=native`)

An alternative renderer that talks Vulkan directly from Rust, **in parallel with
DXVK** (which stays the default). Selected with one env var:

```
GOW_GFX_BACKEND=dxvk    (default)  -> libdxvk_d3d11.so + libdxvk_dxgi.so, as today
GOW_GFX_BACKEND=native             -> crate::vk_native (this code)
```

Both backends coexist; nothing is removed. The switch is read in
`vk_native::Backend::from_env` and branched on at three points:
`graphics.rs::autoload`, `win32.rs` (IAT + GetProcAddress), `jni.rs`
(`start_graphics_backend` + surface callbacks).

## Why it exists

On this device Turnip is refused by DXVK 3.1.1
(`storageBuffer8BitAccess=0`, see `docs/DECISIONS.md` D028). A parallel renderer
that does not depend on DXVK's adapter gates is the escape hatch.

## What works (verified on Termux, Turnip Adreno 660, Vulkan 1.3.363)

| Piece | File | Status |
|---|---|---|
| Instance / physical device / device / graphics+present queue | `vk_native/mod.rs` | works (loader GIPA **and** external/raw-ICD GIPA) |
| `VkSwapchainKHR` from `ANativeWindow*` (2–3 images) | `vk_native/swapchain.rs` | works (headless surface used for the Termux harness) |
| Clear + submit + present loop | `vk_native/swapchain.rs` | works (4/4 frames) |
| Real GPU pipeline (SPIR-V) | `vk_native/render.rs` | works (coloured triangle) |
| 3D scene: depth buffer, MVP push-constant, animation | `vk_native/scene.rs` | works (spinning cube, verified frames differ) |
| X11 presentation (`XPutImage`, multi-display) | `vk_native/x11.rs` | works (`:1` Xvfb + `:2`, same pixels) |
| D3D11/DXGI COM objects (factory/device/context/swapchain) | `vk_native/d3d11_objects.rs` | factory/device/context/swapchain + `Present` work |
| Integration into the game's import paths | `win32.rs`, `jni.rs`, `graphics.rs` | done |

### Rendering to the screen under Termux

Turnip cannot *present* a `VkSwapchainKHR` on an Xvfb server ("No DRI3 support
detected - required for presentation"), so `vkGetPhysicalDeviceSurfaceSupportKHR`
reports `present=false`. The working path is: render on the **GPU** into a
host-visible linear `B8G8R8A8_UNORM` image, then `XPutImage` it to the window.
That works on any X server — an Xvfb, or the Termux:X11 app (a separate APK that
is also an X server). Set the targets with:

```
export GOW_X11_DISPLAYS=":1,:0"   # Xvfb :1 and the Termux:X11 app
```

PNG screenshots of all of this are copied to `/storage/emulated/0/god/Screenshot/`.

## Build / run

```
export VK_ICD_FILENAMES=$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json
export LD_LIBRARY_PATH=$PREFIX/lib
cargo build -p gow-runtime --release
# or with the C-ABI probe entry points:
cargo build -p gow-runtime --release --features vkprobe
```

Harnesses (all headless-safe, no APK needed):

```
# device + swapchain + clear + present (headless surface)
cargo run -p gow-runtime --example vk_native_selftest --release 4

# same, through an *external* vkGetInstanceProcAddr taken from the raw Turnip ICD
cargo run -p gow-runtime --example vk_native_gipa_probe --release

# GPU triangle -> X11
GOW_X11_DISPLAYS=:1 cargo run -p gow-runtime --example vk_native_x11_demo --release 400

# 3D spinning cube (depth) -> X11
GOW_X11_DISPLAYS=:1 cargo run -p gow-runtime --example vk_native_scene --release 400

# drive the D3D11 COM layer exactly like the game (CreateDXGIFactory1 ->
# CreateSwapChain -> Present) and present a frame
GOW_GFX_BACKEND=native GOW_X11_DISPLAYS=:1 \
  cargo run -p gow-runtime --example vk_native_d3d11_drive --release
```

Shaders live in `vk_native/shaders/` (GLSL + the compiled `.spv`, built with
`glslc`, embedded via `include_bytes!`).

## D3D11 COM objects — the important detail

The guest reaches D3D11 through COM: `call [this->vtbl + n]`. COM dereferences
**twice** — `*(this)` is the vtable, `*(vtable + n*8)` is the method. The object
handed to the guest as `this` must therefore be a real allocation whose first
qword is the vtable. Returning the vtable itself makes the guest read a
`0xa000...` thunk pseudo-address (callable but unmapped) and fault. See
`make_com_object` in `d3d11_objects.rs`.

`QueryInterface` is honest: it returns `E_NOINTERFACE` for IIDs we do not
implement, instead of always S_OK (which handed the caller an object of the
wrong type that it then indexed past the end of a short vtable).

Handlers must not call `hostcall::register`/`build_vtable` — `hostcall::call`
holds the thunks mutex while a handler runs — so every vtable is built once,
up front, in `state()`.

Scope of the current cut: bootstrap objects (factory/device/context/swapchain)
and `Present`. Per-resource methods (`CreateTexture2D`,
`CreateRenderTargetView`, full draw state) are honest `E_NOTIMPL` stubs.

## Game progress

With `GOW_GFX_BACKEND=native` the game's boot now reaches
`CreateDXGIFactory1` -> `ID3D11CreateDevice` -> `QueryInterface` and the native
objects are created (visible in the log). It then crashes in **guest code**
(`f_40a610`, a `gow_store`) during the console/window-init import sequence
(`GetConsoleWindow`/`ShowWindow`/`DestroyWindow`). That crash is **pre-existing
and independent of the graphics backend** — it reproduces identically with DXVK
and with no backend. It is a lifter/Win32-coverage bug, not a renderer bug.

## Not done yet

- The D3D11 resource/pipeline layer the game needs for real geometry
  (textures, render-target views, shaders from DXBC, vertex/index buffers,
  draws). Today only bootstrap + present are wired.
- DXBC -> SPIR-V translation (the DXBC parser exists in `dxbc.rs`; the
  translation does not).
- On-device `ANativeWindow` swapchain present path is implemented but only
  exercised through the headless surface here (no Android app in Termux).
