# DXVK Native → Android aarch64: exact patch inventory

Companion to `docs/DXVK_ANDROID.md`. Each entry is a concrete source change with
file path, the code, a **confidence** and a **verified source** where one exists.

Conventions:
- `dxvk/` = the DXVK checkout (pinned to **v3.1.1**, `--recursive`).
- Confidence: **V** = copied from a real project's patch (URL given);
  **I** = inferred/adjusted by us, needs testing.
- Line numbers drift between versions; match on the surrounding code, not lines.

Apply order: P0 (cross file / build dir), then P1–P6 in `dxvk/`, then P7–P9 in our
repo (P7–P9 are *runtime* work items, not DXVK patches; listed here so the whole
change set is in one place).

---

## P0 — Meson cross file (new file, written by CI; not in the DXVK tree)

Create `android-arm64.ini` (path relative to the build dir). **Confidence: V**
pattern (Mirror's Edge `android-arm64.cross.txt` + Generals
`meson-android-aarch64-cross.ini.in`).

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
system     = 'android'
cpu_family = 'aarch64'
cpu        = 'aarch64'
endian     = 'little'

[built-in options]
c_args        = ['-fPIC']
cpp_args      = ['-fPIC']
c_link_args   = ['-Wl,-z,max-page-size=16384']
cpp_link_args = ['-Wl,-z,max-page-size=16384']
```

Rationale: `system = 'android'` makes `platform == 'android'` true, which is what
P1/P3 key off, and takes us off the SDL/GLFW dependency path (P3). API level 29
matches `minSdk = 29`.

Note: Mirror's Edge used
`c = ['<ndk>/clang.exe','--target=aarch64-linux-android29','-fno-emulated-tls']`.
The NDK's `aarch64-linux-android29-clang` wrapper is equivalent and simpler;
`-fno-emulated-tls` is a Mirror's Edge-specific workaround, **not** included here.

---

## P1 — `meson.build` (root): gate SSE flags behind x86  *(OPTIONAL / defensive)*

File: `dxvk/meson.build`. **Confidence: V** (Generals `Patches/dxvk-android.patch`).

Context — the arg list contains:

```meson
compiler_args = [
  '-msse',
  '-msse2',
  '-msse3',
  '-mfpmath=sse',
  ...
]
```

Change (remove the four SSE flags from the literal list and re-add them
conditionally):

```diff
 compiler_args = [
-  '-msse',
-  '-msse2',
-  '-msse3',
-  '-mfpmath=sse',
   '-Wimplicit-fallthrough',
   ...
 ]

+if cpu_family == 'x86' or cpu_family == 'x86_64'
+  compiler_args += ['-msse', '-msse2', '-msse3', '-mfpmath=sse']
+endif
+
 link_args = []
```

**Not required on 2.6+/3.x**: `add_project_arguments(cpp.get_supported_arguments(
compiler_args), …)` already filters `-mfpmath=sse` on aarch64. Include only if you
want the build to be independent of meson's probe. **V** (both the current
`get_supported_arguments` form and the Generals patch were fetched).

---

## P2 — `src/dxvk/dxvk_memory.cpp`: lock the allocation pool on import

File: `dxvk/src/dxvk/dxvk_memory.cpp`. **Confidence: V** (Mirror's Edge
`dxvk-3.1.1-android.patch`).

In `DxvkMemoryAllocator::importBufferResource` and
`importImageResource`, take the allocator mutex before touching the pool:

```diff
   Rc<DxvkResourceAllocation> DxvkMemoryAllocator::importBufferResource(
     const VkBufferCreateInfo&         createInfo,
     const DxvkAllocationInfo&         allocationInfo,
     const DxvkBufferImportInfo&       importInfo) {
+    std::lock_guard lock(m_mutex);
     Rc<DxvkResourceAllocation> allocation = m_allocationPool.create(this, nullptr);
     allocation->m_flags.set(DxvkAllocationFlag::Imported);
     allocation->m_resourceCookie = allocationInfo.resourceCookie;
```

and identically in `importImageResource` (the patch shows both sites). Verified
that v3.1.1 has these two `m_allocationPool.create(this, nullptr)` sites without
the lock. Only matters if GoW creates D3D devices/imports from multiple threads;
include it — it is cheap and matches the only working Android D3D port.

---

## P3 — `meson.build` (root): Android needs no SDL/GLFW

File: `dxvk/meson.build`. **Confidence: V** (Mirror's Edge patch).

Upstream 3.1.1 (inside the `else` non-Windows branch):

```meson
  if (not lib_sdl3.found() and not lib_sdl2.found() and not lib_glfw.found())
    error('SDL3, SDL2, or GLFW are required to build dxvk-native')
  endif
```

Change:

```diff
-  if (not lib_sdl3.found() and not lib_sdl2.found() and not lib_glfw.found())
+  if platform == 'android'
+    compiler_args += ['-DDXVK_WSI_ANDROID']
+  elif (not lib_sdl3.found() and not lib_sdl2.found() and not lib_glfw.found())
     error('SDL3, SDL2, or GLFW are required to build dxvk-native')
   endif
```

This is what selects the Android WSI backend at compile time and removes the SDL3
runtime dependency.

---

## P4 — new file `src/wsi/android/wsi_platform_android.cpp`

**Confidence: V** (verbatim from Mirror's Edge, zlib-licensed, license noted in
their README). This is the whole backend. It implements `WsiDriver` and exports
`dxvk_android_set_window`.

```cpp
#if defined(DXVK_WSI_ANDROID)

// Android WSI for DXVK Native. One display: the app's view. Windows are opaque
// handles of the host program; every one of them presents to the ANativeWindow
// it hands over with dxvk_android_set_window(). Modes: the view's size at 60 Hz.

#include "../wsi_platform.h"
#include "../wsi_window.h"
#include "../../util/util_error.h"
#include "../../util/util_string.h"
#include "../../util/log/log.h"

#include <android/native_window.h>
#include <vulkan/vulkan_android.h>

#include <atomic>

static std::atomic<ANativeWindow*> g_androidWindow { nullptr };

extern "C" __attribute__((visibility("default"))) void dxvk_android_set_window(ANativeWindow* window) {
  g_androidWindow.store(window);
}

namespace dxvk::wsi {

  static const HMONITOR AndroidMonitor = reinterpret_cast<HMONITOR>(uintptr_t(1));

  static void viewSize(uint32_t* w, uint32_t* h) {
    ANativeWindow* window = g_androidWindow.load();
    int32_t ww = window ? ANativeWindow_getWidth(window) : 0;
    int32_t hh = window ? ANativeWindow_getHeight(window) : 0;
    *w = uint32_t(ww > 0 ? ww : 1280);
    *h = uint32_t(hh > 0 ? hh : 720);
  }

  class AndroidWsiDriver : public WsiDriver {
  public:
    std::vector<const char *> getInstanceExtensions() {
      return { VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_ANDROID_SURFACE_EXTENSION_NAME };
    }

    HMONITOR getDefaultMonitor() { return AndroidMonitor; }

    HMONITOR enumMonitors(uint32_t index) { return index == 0 ? AndroidMonitor : nullptr; }

    HMONITOR enumMonitors(const LUID *adapterLUID[], uint32_t numLUIDs, uint32_t index) { return enumMonitors(index); }

    bool getDisplayName(HMONITOR hMonitor, WCHAR (&Name)[32]) {
      const WCHAR name[] = { '\\', '\\', '.', '\\', 'D', 'I', 'S', 'P', 'L', 'A', 'Y', '1', 0 };
      for (uint32_t i = 0; i < sizeof(name) / sizeof(name[0]); i++)
        Name[i] = name[i];
      return hMonitor == AndroidMonitor;
    }

    bool getDesktopCoordinates(HMONITOR hMonitor, RECT* pRect) {
      uint32_t w, h;
      viewSize(&w, &h);
      pRect->left = 0; pRect->top = 0; pRect->right = LONG(w); pRect->bottom = LONG(h);
      return hMonitor == AndroidMonitor;
    }

    // The surface size and smaller sizes of the same shape: the game renders in
    // the mode it chooses (ResX/ResY in its ini) and the swap chain scales to
    // the surface.
    bool getDisplayMode(HMONITOR hMonitor, uint32_t modeNumber, WsiMode* pMode) {
      static const uint32_t scale[][2] = { { 3, 8 }, { 1, 2 }, { 2, 3 }, { 3, 4 }, { 1, 1 } };
      if (modeNumber >= sizeof(scale) / sizeof(scale[0]))
        return false;
      if (!getCurrentDisplayMode(hMonitor, pMode))
        return false;
      pMode->width = pMode->width * scale[modeNumber][0] / scale[modeNumber][1] & ~7u;
      pMode->height = pMode->height * scale[modeNumber][0] / scale[modeNumber][1] & ~7u;
      return true;
    }

    bool getCurrentDisplayMode(HMONITOR hMonitor, WsiMode* pMode) {
      viewSize(&pMode->width, &pMode->height);
      pMode->refreshRate = WsiRational { 60, 1 };
      pMode->bitsPerPixel = 32;
      pMode->interlaced = false;
      return hMonitor == AndroidMonitor;
    }

    bool getDesktopDisplayMode(HMONITOR hMonitor, WsiMode* pMode) { return getCurrentDisplayMode(hMonitor, pMode); }

    WsiEdidData getMonitorEdid(HMONITOR hMonitor) { return {}; }

    void getWindowSize(HWND hWindow, uint32_t* pWidth, uint32_t* pHeight) {
      uint32_t w, h;
      viewSize(&w, &h);
      if (pWidth) *pWidth = w;
      if (pHeight) *pHeight = h;
    }

    void resizeWindow(HWND hWindow, DxvkWindowState* pState, uint32_t width, uint32_t height) { }
    void saveWindowState(HWND hWindow, DxvkWindowState* pState, bool saveStyle) { }
    void restoreWindowState(HWND hWindow, DxvkWindowState* pState, bool restoreCoordinates) { }
    bool setWindowMode(HMONITOR hMonitor, HWND hWindow, DxvkWindowState* pState, const WsiMode& mode) { return true; }
    bool enterFullscreenMode(HMONITOR hMonitor, HWND hWindow, DxvkWindowState* pState, bool modeSwitch) { return true; }
    bool leaveFullscreenMode(HWND hWindow, DxvkWindowState* pState) { return true; }
    bool restoreDisplayMode() { return true; }
    HMONITOR getWindowMonitor(HWND hWindow) { return AndroidMonitor; }
    bool isWindow(HWND hWindow) { return hWindow != nullptr; }
    bool isMinimized(HWND hWindow) { return g_androidWindow.load() == nullptr; }
    bool isOccluded(HWND hWindow) { return g_androidWindow.load() == nullptr; }
    void updateFullscreenWindow(HMONITOR hMonitor, HWND hWindow, bool forceTopmost) { }

    VkResult createSurface(HWND hWindow, PFN_vkGetInstanceProcAddr pfnVkGetInstanceProcAddr,
                           VkInstance instance, VkSurfaceKHR* pSurface) {
      ANativeWindow* window = g_androidWindow.load();
      if (!window) {
        Logger::err("Android WSI: no window (dxvk_android_set_window was not called)");
        return VK_ERROR_SURFACE_LOST_KHR;
      }
      auto create = reinterpret_cast<PFN_vkCreateAndroidSurfaceKHR>(
        pfnVkGetInstanceProcAddr(instance, "vkCreateAndroidSurfaceKHR"));
      if (!create)
        return VK_ERROR_EXTENSION_NOT_PRESENT;
      VkAndroidSurfaceCreateInfoKHR info = { VK_STRUCTURE_TYPE_ANDROID_SURFACE_CREATE_INFO_KHR };
      info.window = window;
      return create(instance, &info, nullptr, pSurface);
    }
  };

  static bool createAndroidWsiDriver(WsiDriver **driver) {
    *driver = new AndroidWsiDriver();
    return true;
  }

  WsiBootstrap AndroidWSI = {
    "Android",
    createAndroidWsiDriver
  };

}

#endif
```

Note on line count: the published patch header says `@@ -0,0 +1,137 @@`; the body
above is the complete file (the `diff` counts include the leading `+` markers).

### P4a — register it in `src/wsi/wsi_platform.h`

**Confidence: V** (Mirror's Edge patch).

```diff
 #if defined(DXVK_WSI_WAYLAND)
   extern WsiBootstrap WaylandWSI;
 #endif
+
+#if defined(DXVK_WSI_ANDROID)
+  extern WsiBootstrap AndroidWSI;
+#endif
```

### P4b — register it in `src/wsi/wsi_platform.cpp`

**Confidence: V** (Mirror's Edge patch).

```diff
 static const WsiBootstrap *wsiBootstrap[] = {
 #if defined(DXVK_WSI_WIN32)
   &Win32WSI,
 #endif
 ...
 #if defined(DXVK_WSI_WAYLAND)
   &WaylandWSI,
 #endif
+#if defined(DXVK_WSI_ANDROID)
+  &AndroidWSI,
+#endif
 };
```

### P4c — add the source and link `libandroid` in `src/wsi/meson.build`

**Confidence: V** (Mirror's Edge patch; note upstream 3.1.1 has no xcb/wayland
entries in `wsi_src`, only the sdl/glfw/win32 ones — match yours).

```diff
 wsi_src = [
   'wsi_edid.cpp',
   'wsi_platform.cpp',
   ...
   'glfw/wsi_window_glfw.cpp',
+  'android/wsi_platform_android.cpp',
 ]

 wsi_deps = [ dep_displayinfo ]

 if platform == 'windows'
   ...
+elif platform == 'android'
+  wsi_deps += [ cpp.find_library('android') ]
 else
   wsi_deps += [
     lib_sdl3.partial_dependency(compile_args: true, includes: true),
     ...
   ]
 endif
```

---

## P5 — export `dxvk_android_set_window` + drop the SONAME version on Android

### P5a — version scripts

**Confidence**: **V** for `d3d9.sym` (Mirror's Edge patched only d3d9, because
their host is D3D9); **I** for `d3d11.sym` and `dxgi.sym` — we need both because
GoW uses D3D11+DXGI and *each* `.so` keeps its own `g_androidWindow` (see
`docs/DXVK_ANDROID.md` §2.4).

`dxvk/src/d3d11/d3d11.sym`:

```diff
 {
   global:
     D3D11CoreCreateDevice;
     D3D11CreateDevice;
     D3D11CreateDeviceAndSwapChain;
     D3D11On12CreateDevice;
+    dxvk_android_set_window;

   local:
     *;
 };
```

`dxvk/src/dxgi/dxgi.sym`:

```diff
 {
   global:
     CreateDXGIFactory2;
     CreateDXGIFactory1;
     CreateDXGIFactory;
     DXGIDeclareAdapterRemovalSupport;
     DXGIGetDebugInterface1;
+    dxvk_android_set_window;

   local:
     *;
 };
```

(If both libs are built, `dxvk_android_set_window` is defined in each; the
`visibility("default")` attribute in P4 already exports it, but the
`-Wl,--version-script` makes `local: *` the default, so the explicit `global:`
entry is required for it to survive into `.dynsym`.)

### P5b — unversioned SONAME on Android

**Confidence: I.**

Upstream sets (in the non-Windows branch of `dxvk/meson.build`):

```meson
dxvk_so_version = {'version': dxvk_abi_version + '.' + dxvk_version[0] + dxvk_version[1] + dxvk_version[2]}
```

which makes meson emit `libdxvk_dxgi.so.0.31.1` with SONAME `libdxvk_dxgi.so.0`,
and `libdxvk_d3d11.so` gets `DT_NEEDED libdxvk_dxgi.so.0`. Android packages only
`lib<name>.so`, so either patch meson:

```diff
-if platform == 'windows'
+if platform == 'android'
+  dxvk_so_version = {}
+elif platform == 'windows'
   ...
```

(so both libs are named `libdxvk_d3d11.so`/`libdxvk_dxgi.so` with matching
SONAMEs) — **or** post-process with `patchelf --set-soname` /
`--replace-needed` as shown in `docs/DXVK_ANDROID.md` §5.4. The meson patch is
cleaner; the patchelf route needs no DXVK source change.

---

## P6 — (only if the SDL3 WSI route A is used) SDL3 WSI fixes

Skip these if P3/P4 (Android WSI) are applied. Listed for completeness; the source
is Generals `Patches/dxvk-android.patch`. **Confidence: V.**

### P6a — `src/wsi/sdl3/wsi_platform_sdl3.cpp`: soname

```diff
   libsdl = LoadLibraryA(
 #if defined(_WIN32)
       "SDL3.dll"
 #elif defined(__APPLE__)
       "libSDL3.0.dylib"
+#elif defined(__ANDROID__)
+      // Android packs the unversioned soname in the APK; ".so.0" does not exist.
+      "libSDL3.so"
 #else
       "libSDL3.so.0"
 #endif
     );
```

### P6b — `src/wsi/sdl3/wsi_window_sdl3.cpp`: high-DPI (pixels, not points)

```diff
-    if (!SDL_GetWindowSize(window, &w, &h))
-      Logger::err(str::format("SDL3 WSI: SDL_GetWindowSize: ", SDL_GetError()));
+    if (!SDL_GetWindowSizeInPixels(window, &w, &h))
+      Logger::err(str::format("SDL3 WSI: SDL_GetWindowSizeInPixels: ", SDL_GetError()));
```

(both occurrences of `SDL_GetWindowSize` → `SDL_GetWindowSizeInPixels`; patch
touches the one in `getWindowSize`.)

### P6c — `src/wsi/sdl3/wsi_platform_sdl3_funcs.h`: declare the symbol

```diff
 SDL_PROC(bool, SDL_GetClosestFullscreenDisplayMode, (SDL_DisplayID, int, int, float, bool, SDL_DisplayMode*))
+SDL_PROC(bool, SDL_GetWindowSizeInPixels, (SDL_Window*, int*, int*))
 SDL_PROC(bool, SDL_SetWindowSize, (SDL_Window*, int, int))
```

---

## P7 — runtime: set WSI driver + hand the `ANativeWindow` to BOTH libs

Not a DXVK patch — **our** change. File: `runtime/src/graphics.rs`
(and the call site in `runtime/src/jni.rs`). **Confidence: I** (design matches the
verified DXVK/Mirror's Edge contract).

Before any D3D call (after both libs are dlopened):

```rust
// 1) DXVK reads this, otherwise wsi::init() throws.
std::env::set_var("DXVK_WSI_DRIVER", "Android");

// 2) Each lib owns its own g_androidWindow -> push to both.
for handle in [self.handles["dxgi.dll"], self.handles["d3d11.dll"]] {
    if let Some(setwin) = self.sym(handle, "dxvk_android_set_window") {
        let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(setwin);
        unsafe { f(anative_window) };
    }
}
```

Re-push on `surfaceChanged` and whenever the `Surface` is recreated (store the
`ANativeWindow` alongside `WINDOW` in `runtime/src/jni.rs`; it is already
computed by `android_window_from_surface`). Missing `dxvk_android_set_window`
(no P5a patch) ⇒ log and fail explicitly, never silently.

---

## P8 — runtime: forward up to 10 Microsoft-x64 arguments to DXVK

Not a DXVK patch — **our** change. File: `runtime/src/graphics.rs`.
**Confidence: I** (gap is verified: `call_addr` only passes 6; DXVK's
`D3D11CreateDevice` takes 10).

`D3D11CreateDevice` signature (verified, `src/d3d11/d3d11_main.cpp`):

```c
HRESULT D3D11CreateDevice(
  IDXGIAdapter* pAdapter, D3D_DRIVER_TYPE DriverType, HMODULE Software,
  UINT Flags, const D3D_FEATURE_LEVEL* pFeatureLevels, UINT FeatureLevels,
  UINT SDKVersion, ID3D11Device** ppDevice,
  D3D_FEATURE_LEVEL* pFeatureLevel, ID3D11DeviceContext** ppImmediateContext);
```

Microsoft x64: args 1–4 in `RCX,RDX,R8,R9`; args 5+ at `[rsp+0x28]`,
`[rsp+0x30]`, `[rsp+0x38]`, `[rsp+0x40]`, `[rsp+0x48]`, `[rsp+0x50]`. The shim
must read all of them and pass them in AAPCS64 `x0..x7` + host stack. Concretely,
replace the 6-arg `call_addr`/`call_addr_raw` with a 12-arg variant:

```rust
type F12 = unsafe extern "system" fn(
    u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

pub unsafe fn call_addr_12(addr: usize, a: [u64; 12]) -> u64 {
    let f: F12 = std::mem::transmute(addr);
    f(a[0],a[1],a[2],a[3],a[4],a[5],a[6],a[7],a[8],a[9],a[10],a[11])
}
```

and in `call_addr_raw` gather `rcx,rdx,r8,r9` plus
`[rsp+0x28..=0x50]`. Also update `win32.rs`'s d3d11/dxgi/d3dcompiler arm
(the `*(arg(4) as *const u64)` trick must become a straight read of all six stack
slots). Return value is a plain `HRESULT` in `x0` → guest `rax`.

---

## P9 — runtime: resolve `D3DReflect` (not DXVK)

Not a DXVK patch — **our** change. **Confidence: V** that DXVK cannot provide it;
**I** on the chosen remedy.

- Remove/fix the assumption in `runtime/src/graphics.rs` `D3D_BOOTSTRAP` that
  `d3dcompiler_47.dll!D3DReflect` maps to a DXVK export (it does not; DXVK's
  `d3d11.sym`/`dxgi.sym` export only the four/ five names in P5a).
- Provide a host implementation and register it as a `hostcall` thunk (the
  mechanism already exists in `runtime/src/hostcall.rs`):
  1. **Preferred:** a Rust `ID3D11ShaderReflection` over DXBC (`RDEF`/`ISGN`/
     `OSGN`/`SHDR` chunk parsing), returning a COM vtable built by
     `hostcall::build_vtable`.
  2. Or cross-compile `vkd3d-utils` `D3DReflect` for Android.
  3. Or return `E_NOTIMPL` and observe whether GoW still initialises (risky).

`D3DReflect(ptr long ptr ptr)` — Wine `dlls/d3dcompiler_47/d3dcompiler_47.spec`
(verified); `void* pSrcData, SIZE_T SrcDataSize, REFIID pInterface,
void** ppReflector`.

---

## Summary table

| ID | File | Change | Confidence |
|---|---|---|---|
| P0 | (CI) `android-arm64.ini` | meson cross file, `system='android'`, API 29, 16 KB pages | V |
| P1 | `dxvk/meson.build` | gate `-msse*` behind x86 | V (optional) |
| P2 | `dxvk/src/dxvk/dxvk_memory.cpp` | `std::lock_guard` in importBuffer/ImageResource | V |
| P3 | `dxvk/meson.build` | `if platform == 'android'` → `-DDXVK_WSI_ANDROID` | V |
| P4 | `dxvk/src/wsi/android/wsi_platform_android.cpp` (new) | Android WSI backend + `dxvk_android_set_window` | V |
| P4a/b | `dxvk/src/wsi/wsi_platform.{h,cpp}` | register `AndroidWSI` | V |
| P4c | `dxvk/src/wsi/meson.build` | add source, link `libandroid` | V |
| P5a | `dxvk/src/d3d11/d3d11.sym`, `dxvk/src/dxgi/dxgi.sym` | export `dxvk_android_set_window` | I |
| P5b | `dxvk/meson.build` | unversioned SONAME on Android (or patchelf) | I |
| P6a–c | `dxvk/src/wsi/sdl3/*` | SDL3 soname + high-DPI (only route A) | V |
| P7 | `runtime/src/{graphics,jni}.rs` | set `DXVK_WSI_DRIVER=Android`, push window to both libs | I |
| P8 | `runtime/src/{graphics,win32}.rs` | forward up to 10 args (MS x64 → AAPCS64) | I |
| P9 | `runtime/src/graphics.rs` (+ new module) | implement/route `D3DReflect` | I |

**Do not** edit existing repo files for this task; P7–P9 describe the follow-up
runtime work so the full change set is documented in one place.
