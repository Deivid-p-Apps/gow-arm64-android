//! X11 presentation for the native Vulkan backend under Termux / Xvfb.
//!
//! Why this exists: Turnip refuses to *present* a `VkSwapchainKHR` on an Xvfb
//! server because it has no DRI3 ("MESA: vulkan: No DRI3 support detected -
//! required for presentation"), so `vkGetPhysicalDeviceSurfaceSupportKHR`
//! reports `present=false` for every queue family. The workaround that works on
//! *any* X server is to render on the GPU into a host-visible linear image and
//! push the pixels to the window with `XPutImage`. That is how this module shows
//! real GPU output under Termux-X11 / Xvfb.
//!
//! Presentation is **multi-display**: [`X11Display`] opens one server (e.g. an
//! Xvfb `:1` *and* the Termux:X11 app's `:0`), and every `present` copies the
//! same pixels to all of them. That way the image shows up both in a headless
//! Xvfb (for tests/capture) and in the on-device Termux:X11 app window.
//!
//! `libX11` is `dlopen`ed (never linked) so the Android device build — which has
//! no X11 — is unaffected; a missing library just yields `None`.

use std::ffi::{c_char, c_void, CString};

const LIBX11: &[&str] = &["libX11.so.6", "libX11.so"];

type XOpenDisplay = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type XDefaultScreen = unsafe extern "C" fn(*mut c_void) -> i32;
type XDefaultDepth = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type XDefaultGC = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type XDefaultVisual = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type XRootWindow = unsafe extern "C" fn(*mut c_void, i32) -> u64;
type XCreateSimpleWindow =
    unsafe extern "C" fn(*mut c_void, u64, i32, i32, u32, u32, u32, u64, u64) -> u64;
type XStoreName = unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> i32;
type XMapWindow = unsafe extern "C" fn(*mut c_void, u64) -> i32;
type XFlush = unsafe extern "C" fn(*mut c_void) -> i32;
type XSync = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type XCreateImage = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    u32,
    i32,
    i32,
    *mut c_char,
    u32,
    u32,
    i32,
    i32,
) -> *mut c_void;
type XPutImage = unsafe extern "C" fn(
    *mut c_void,
    u64,
    *mut c_void,
    *mut c_void,
    i32,
    i32,
    i32,
    i32,
    u32,
    u32,
) -> i32;
type XResizeWindow = unsafe extern "C" fn(*mut c_void, u64, u32, u32) -> i32;

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const c_char) -> *mut c_void;
}

const RTLD_NOW: i32 = 2;

/// Loaded libX11 entry points (all resolved at open time).
struct X11Lib {
    display: XOpenDisplay,
    default_screen: XDefaultScreen,
    default_depth: XDefaultDepth,
    default_gc: XDefaultGC,
    default_visual: XDefaultVisual,
    root_window: XRootWindow,
    create_simple_window: XCreateSimpleWindow,
    store_name: XStoreName,
    map_window: XMapWindow,
    flush: XFlush,
    sync: XSync,
    create_image: XCreateImage,
    put_image: XPutImage,
    resize_window: XResizeWindow,
}

unsafe fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = CString::new(name).unwrap();
    dlsym(handle, c.as_ptr())
}

impl X11Lib {
    fn load() -> Option<Self> {
        unsafe {
            let mut h = std::ptr::null_mut();
            for name in LIBX11 {
                let c = CString::new(*name).unwrap();
                h = dlopen(c.as_ptr(), RTLD_NOW);
                if !h.is_null() {
                    break;
                }
            }
            if h.is_null() {
                return None;
            }
            macro_rules! f {
                ($n:literal, $t:ty) => {{
                    let p = sym(h, $n);
                    if p.is_null() {
                        crate::glog!("[gow.vk.native.x11] missing symbol {}", $n);
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $t>(p)
                }};
            }
            Some(X11Lib {
                display: f!("XOpenDisplay", XOpenDisplay),
                default_screen: f!("XDefaultScreen", XDefaultScreen),
                default_depth: f!("XDefaultDepth", XDefaultDepth),
                default_gc: f!("XDefaultGC", XDefaultGC),
                default_visual: f!("XDefaultVisual", XDefaultVisual),
                root_window: f!("XRootWindow", XRootWindow),
                create_simple_window: f!("XCreateSimpleWindow", XCreateSimpleWindow),
                store_name: f!("XStoreName", XStoreName),
                map_window: f!("XMapWindow", XMapWindow),
                flush: f!("XFlush", XFlush),
                sync: f!("XSync", XSync),
                create_image: f!("XCreateImage", XCreateImage),
                put_image: f!("XPutImage", XPutImage),
                resize_window: f!("XResizeWindow", XResizeWindow),
            })
        }
    }
}

/// An X11 window that presents BGRA pixels via `XPutImage`.
pub struct X11Window {
    lib: X11Lib,
    dpy: *mut c_void,
    win: u64,
    gc: *mut c_void,
    depth: i32,
    visual: *mut c_void,
    /// Packed BGRA buffer, one `XCreateImage` per resize.
    image: *mut c_void,
    buf: Vec<u8>,
    width: u32,
    height: u32,
}

unsafe impl Send for X11Window {}

impl X11Window {
    /// Open `DISPLAY` (or `name`, e.g. ":1"), create a top-level window and map
    /// it. Returns `None` when X11 or the display is unavailable.
    pub fn create(name: &str, width: u32, height: u32, title: &str) -> Option<Self> {
        let lib = X11Lib::load()?;
        unsafe {
            let cname = CString::new(name).unwrap();
            let dpy = (lib.display)(cname.as_ptr());
            if dpy.is_null() {
                crate::glog!("[gow.vk.native.x11] XOpenDisplay({name}) failed");
                return None;
            }
            let screen = (lib.default_screen)(dpy);
            let root = (lib.root_window)(dpy, screen);
            let depth = (lib.default_depth)(dpy, screen);
            let visual = (lib.default_visual)(dpy, screen);
            let win = (lib.create_simple_window)(dpy, root, 0, 0, width, height, 0, 0, 0);
            let title = CString::new(title).unwrap();
            (lib.store_name)(dpy, win, title.as_ptr());
            (lib.map_window)(dpy, win);
            (lib.flush)(dpy);
            let gc = (lib.default_gc)(dpy, screen);
            crate::glog!(
                "[gow.vk.native.x11] window created win=0x{win:x} {width}x{height} depth={depth} display={name}"
            );
            Some(X11Window {
                lib,
                dpy,
                win,
                gc,
                depth,
                visual,
                image: std::ptr::null_mut(),
                buf: vec![0u8; (width * height * 4) as usize],
                width,
                height,
            })
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Ensure the backing pixel buffer + `XImage` match `width`x`height`.
    fn ensure_image(&mut self) -> bool {
        if !self.image.is_null() && self.buf.len() == (self.width * self.height * 4) as usize {
            return true;
        }
        self.buf = vec![0u8; (self.width * self.height * 4) as usize];
        unsafe {
            // XCreateImage takes ownership of the data pointer, so it must be a
            // pointer we can hand over and later read through `self.buf`.
            self.image = (self.lib.create_image)(
                self.dpy,
                self.visual,
                self.depth as u32,
                2, // ZPixmap
                0,
                self.buf.as_mut_ptr() as *mut c_char,
                self.width,
                self.height,
                32,
                (self.width * 4) as i32,
            );
            if self.image.is_null() {
                crate::glog!("[gow.vk.native.x11] XCreateImage failed");
                return false;
            }
        }
        true
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.image = std::ptr::null_mut();
        unsafe {
            (self.lib.resize_window)(self.dpy, self.win, width, height);
        }
    }

    /// Present `pixels` (BGRA, tightly packed, `width`x`height`). The buffer must
    /// be exactly `width*height*4` bytes.
    pub fn present(&mut self, pixels: &[u8]) -> bool {
        if pixels.len() != (self.width * self.height * 4) as usize {
            crate::glog!(
                "[gow.vk.native.x11] present size mismatch: {} != {}",
                pixels.len(),
                self.width * self.height * 4
            );
            return false;
        }
        if !self.ensure_image() {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.buf.as_mut_ptr(), pixels.len());
            (self.lib.put_image)(
                self.dpy,
                self.win,
                self.gc,
                self.image,
                0,
                0,
                0,
                0,
                self.width,
                self.height,
            );
            (self.lib.flush)(self.dpy);
        }
        true
    }

    pub fn sync(&self) {
        unsafe {
            (self.lib.sync)(self.dpy, 0);
        }
    }
}

/// Presents the same frame to **several** X servers at once (an Xvfb for
/// headless capture and the Termux:X11 app, running on different display
/// numbers). Every one gets the identical BGRA buffer.
///
/// Configure with `GOW_X11_DISPLAYS` (comma/space separated, e.g. `:1,:0`). When
/// unset it defaults to `$DISPLAY` if present, else `:0`.
pub struct X11Displays {
    windows: Vec<X11Window>,
    width: u32,
    height: u32,
}

impl X11Displays {
    /// Open every display in `GOW_X11_DISPLAYS` (or the default). Returns `None`
    /// when no server could be reached.
    pub fn create_default(width: u32, height: u32, title: &str) -> Option<Self> {
        let spec = std::env::var("GOW_X11_DISPLAYS").ok().or_else(|| {
            std::env::var("DISPLAY").ok()
        });
        let spec = spec.unwrap_or_else(|| ":0".to_string());
        let names: Vec<String> = spec
            .split([',', ' ', ';'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        crate::glog!("[gow.vk.native.x11] target displays: {names:?}");
        let mut windows = Vec::new();
        for n in &names {
            if let Some(w) = X11Window::create(n, width, height, title) {
                windows.push(w);
            } else {
                crate::glog!("[gow.vk.native.x11] display {n} unavailable; skipping");
            }
        }
        if windows.is_empty() {
            return None;
        }
        Some(X11Displays {
            windows,
            width,
            height,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn count(&self) -> usize {
        self.windows.len()
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Present `pixels` (tight BGRA, `width*height*4`) to every display.
    /// Returns the number of displays that accepted the frame.
    pub fn present(&mut self, pixels: &[u8]) -> usize {
        let mut ok = 0;
        for w in &mut self.windows {
            if w.present(pixels) {
                ok += 1;
            }
        }
        ok
    }

    /// Resize every window and drop their cached pixels (next present reallocs).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        for w in &mut self.windows {
            w.resize(width, height);
        }
    }

    pub fn sync(&self) {
        for w in &self.windows {
            w.sync();
        }
    }
}
