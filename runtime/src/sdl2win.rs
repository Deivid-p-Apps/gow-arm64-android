//! SDL2 window bridge for the host DXVK path.
//!
//! DXVK Native's SDL2 WSI treats the D3D11 `OutputWindow` HWND as a literal
//! `SDL_Window*` (`reinterpret_cast<SDL_Window*>(hWindow)`) and calls
//! `SDL_Vulkan_CreateSurface` on it. The guest, however, only knows Win32
//! `CreateWindowExA`, so this module creates a real SDL2 window on the host and
//! hands its pointer back to the guest as the HWND.
//!
//! Everything is resolved with `dlopen`/`dlsym`: the runtime does not link
//! against SDL2, so the device build (which never loads this) is unaffected and
//! the host simply logs and falls back to a fake handle when SDL2 is absent.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

const SDL_INIT_VIDEO: u32 = 0x0000_0020;
const SDL_WINDOW_SHOWN: u32 = 0x0000_0004;
const SDL_WINDOW_VULKAN: u32 = 0x1000_0000;

type InitFn = unsafe extern "C" fn(u32) -> c_int;
type QuitFn = unsafe extern "C" fn();
type CreateWindowFn = unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, c_int, u32) -> *mut c_void;
type DestroyWindowFn = unsafe extern "C" fn(*mut c_void);
type ShowWindowFn = unsafe extern "C" fn(*mut c_void);
type HideWindowFn = unsafe extern "C" fn(*mut c_void);
type SetWindowSizeFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
type GetWindowSizeFn = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int);
type SetWindowTitleFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type PumpEventsFn = unsafe extern "C" fn();
type GetErrorFn = unsafe extern "C" fn() -> *const c_char;

struct Sdl {
    handle: *mut c_void,
    init: InitFn,
    quit: QuitFn,
    create_window: CreateWindowFn,
    destroy_window: DestroyWindowFn,
    show_window: ShowWindowFn,
    hide_window: HideWindowFn,
    set_window_size: SetWindowSizeFn,
    get_window_size: GetWindowSizeFn,
    set_window_title: SetWindowTitleFn,
    pump_events: PumpEventsFn,
    get_error: GetErrorFn,
    initialized: bool,
}

// The function pointers are immutable after `load()` and SDL is only touched
// from the guest main thread; the `Mutex` below serialises access anyway.
unsafe impl Send for Sdl {}
unsafe impl Sync for Sdl {}

impl Sdl {
    unsafe fn load() -> Option<Sdl> {
        let name = CString::new("libSDL2-2.0.so.0").ok()?;
        let handle = dlopen(name.as_ptr(), RTLD_NOW | RTLD_GLOBAL);
        if handle.is_null() {
            let e = dlerror();
            let msg = if e.is_null() { "unknown".into() } else {
                std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
            };
            crate::glog!("[gow.sdl2] dlopen(libSDL2-2.0.so.0) failed: {msg}");
            return None;
        }
        macro_rules! sym {
            ($t:ty, $n:literal) => {{
                let c = CString::new($n).ok()?;
                let p = dlsym(handle, c.as_ptr());
                if p.is_null() {
                    crate::glog!("[gow.sdl2] missing symbol {}", $n);
                    return None;
                }
                std::mem::transmute::<*mut c_void, $t>(p)
            }};
        }
        Some(Sdl {
            handle,
            init: sym!(InitFn, "SDL_Init"),
            quit: sym!(QuitFn, "SDL_Quit"),
            create_window: sym!(CreateWindowFn, "SDL_CreateWindow"),
            destroy_window: sym!(DestroyWindowFn, "SDL_DestroyWindow"),
            show_window: sym!(ShowWindowFn, "SDL_ShowWindow"),
            hide_window: sym!(HideWindowFn, "SDL_HideWindow"),
            set_window_size: sym!(SetWindowSizeFn, "SDL_SetWindowSize"),
            get_window_size: sym!(GetWindowSizeFn, "SDL_GetWindowSize"),
            set_window_title: sym!(SetWindowTitleFn, "SDL_SetWindowTitle"),
            pump_events: sym!(PumpEventsFn, "SDL_PumpEvents"),
            get_error: sym!(GetErrorFn, "SDL_GetError"),
            initialized: false,
        })
    }

    unsafe fn ensure_init(&mut self) -> bool {
        if self.initialized {
            return true;
        }
        if (self.init)(SDL_INIT_VIDEO) != 0 {
            let e = (self.get_error)();
            let msg = if e.is_null() { "unknown".into() } else {
                std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
            };
            crate::glog!("[gow.sdl2] SDL_Init(VIDEO) failed: {msg}");
            return false;
        }
        self.initialized = true;
        true
    }
}

fn sdl() -> &'static Mutex<Option<Sdl>> {
    static S: OnceLock<Mutex<Option<Sdl>>> = OnceLock::new();
    S.get_or_init(|| {
        let s = unsafe { Sdl::load() };
        Mutex::new(s)
    })
}

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: i32 = 2;
const RTLD_GLOBAL: i32 = 0x100;

/// Initialise the SDL video subsystem. DXVK's SDL2 WSI calls
/// `SDL_Vulkan_GetInstanceExtensions` (and `SDL_Vulkan_LoadLibrary`) while
/// creating the DXGI factory — *before* any window exists — and SDL crashes if
/// the video subsystem is not up yet. Call this right after loading the DXVK
/// backend, before the guest issues its first DXGI/D3D11 call.
///
/// Returns false when SDL2 is unavailable (device/APK path).
pub fn ensure_video() -> bool {
    let mut guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        Some(s) => unsafe { s.ensure_init() },
        None => false,
    }
}

/// Create a host window and return it as the guest's HWND. 0 means "no SDL2";
/// the caller then substitutes an opaque fake handle so the guest can continue.
pub fn create_window(title: &str, width: i32, height: i32) -> u64 {
    let w = if width > 0 { width.min(1280) } else { 1280 };
    let h = if height > 0 { height.min(720) } else { 720 };
    let mut guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = guard.as_mut() else { return 0 };
    unsafe {
        if !s.ensure_init() {
            return 0;
        }
        let ctitle = CString::new(title).unwrap_or_else(|_| CString::new("God of War").unwrap());
        let win = (s.create_window)(
            ctitle.as_ptr(),
            0,
            0,
            w,
            h,
            SDL_WINDOW_SHOWN | SDL_WINDOW_VULKAN,
        );
        if win.is_null() {
            let e = (s.get_error)();
            let msg = if e.is_null() { "unknown".into() } else {
                std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
            };
            crate::glog!("[gow.sdl2] SDL_CreateWindow({w}x{h}) failed: {msg}");
            return 0;
        }
        crate::glog!("[gow.sdl2] window created {w}x{h} -> {win:p} title={title:?}");
        win as u64
    }
}

pub fn window_size(hwnd: u64) -> Option<(i32, i32)> {
    if hwnd == 0 {
        return None;
    }
    let guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    let s = guard.as_ref()?;
    let mut w = 0;
    let mut h = 0;
    unsafe { (s.get_window_size)(hwnd as *mut c_void, &mut w, &mut h) };
    if w > 0 && h > 0 { Some((w, h)) } else { None }
}

pub fn show(hwnd: u64, visible: bool) {
    if hwnd == 0 {
        return;
    }
    let guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.as_ref() {
        unsafe {
            if visible { (s.show_window)(hwnd as *mut c_void) } else { (s.hide_window)(hwnd as *mut c_void) }
        }
    }
}

pub fn resize(hwnd: u64, w: i32, h: i32) {
    if hwnd == 0 || w <= 0 || h <= 0 {
        return;
    }
    let guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.as_ref() {
        unsafe { (s.set_window_size)(hwnd as *mut c_void, w, h) };
    }
}

pub fn set_title(hwnd: u64, title: &str) {
    if hwnd == 0 {
        return;
    }
    let Ok(c) = CString::new(title) else { return };
    let guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.as_ref() {
        unsafe { (s.set_window_title)(hwnd as *mut c_void, c.as_ptr()) };
    }
}

pub fn destroy(hwnd: u64) {
    if hwnd == 0 {
        return;
    }
    let guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.as_ref() {
        unsafe { (s.destroy_window)(hwnd as *mut c_void) };
    }
}

/// Pump the SDL event queue so the X11 window stays alive. Called from the
/// guest's PeekMessage loop.
pub fn pump() {
    let guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.as_ref() {
        unsafe { (s.pump_events)() };
    }
}

/// Best-effort cleanup (process exit).
pub fn quit() {
    let mut guard = sdl().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.as_mut() {
        if s.initialized {
            unsafe { (s.quit)() };
            s.initialized = false;
        }
    }
    let _ = s_handle_keepalive();
}

fn s_handle_keepalive() -> Option<()> {
    // Keep the library loaded for the process lifetime; SDL_Quit already ran.
    Some(())
}
