//! Graphics bridge: routes the game's D3D11/DXGI/D3DCompiler IAT calls to the
//! loaded backend (DXVK Native in phase 1; a native Rust Vulkan backend in
//! phase 2). The backend is a host shared library exposing the Windows D3D11
//! entry points with the stdcall/x64 calling convention DXVK Native provides.

use std::collections::HashMap;

#[derive(Default)]
pub struct GraphicsBridge {
    /// dll (lowercase, e.g. "d3d11.dll") -> dlopen handle
    handles: HashMap<String, *mut std::ffi::c_void>,
    /// "dll!func" -> resolved symbol
    symbols: HashMap<String, usize>,
    /// Vulkan driver handle (Turnip), loaded via libadrenotools.
    pub vk_driver: *mut std::ffi::c_void,
}

unsafe impl Send for GraphicsBridge {}

pub const D3D_DLLS: &[&str] = &["d3d11.dll", "dxgi.dll", "d3dcompiler_47.dll"];

/// The exact D3D11/DXGI bootstrap entry points the game imports (measured from
/// its IAT). Everything else is reached through the returned COM vtables, which
/// the backend (DXVK Native) implements.
pub const D3D_BOOTSTRAP: &[(&str, &str)] = &[
    ("d3d11.dll", "D3D11CreateDevice"),
    ("dxgi.dll", "CreateDXGIFactory1"),
    ("d3dcompiler_47.dll", "D3DReflect"),
];

impl GraphicsBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a backend shared library (e.g. libdxvk_d3d11.so) from a private path.
    pub fn load_library(&mut self, dll: &str, path: &str) -> bool {
        let key = dll.to_ascii_lowercase();
        unsafe {
            let cpath = std::ffi::CString::new(path).unwrap();
            let h = dlopen(cpath.as_ptr(), RTLD_NOW | RTLD_LOCAL);
            if h.is_null() {
                crate::glog!("[gow.gfx] dlopen failed for {path}");
                return false;
            }
            self.handles.insert(key, h);
            crate::glog!("[gow.gfx] loaded backend for {dll} from {path}");
            true
        }
    }

    /// Resolve a D3D entry point from the loaded backend.
    pub fn resolve(&mut self, dll: &str, func: &str) -> Option<usize> {
        let key = format!("{}!{}", dll.to_ascii_lowercase(), func);
        if let Some(&p) = self.symbols.get(&key) {
            return Some(p);
        }
        let handle = *self.handles.get(&dll.to_ascii_lowercase())?;
        unsafe {
            let cname = std::ffi::CString::new(func).ok()?;
            let sym = dlsym(handle, cname.as_ptr());
            if sym.is_null() { return None; }
            let addr = sym as usize;
            self.symbols.insert(key, addr);
            Some(addr)
        }
    }

    pub fn is_loaded(&self) -> bool { !self.handles.is_empty() }
}

const RTLD_NOW: i32 = 2;
const RTLD_LOCAL: i32 = 0;

extern "C" {
    fn dlopen(path: *const std::os::raw::c_char, mode: i32) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, sym: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}
