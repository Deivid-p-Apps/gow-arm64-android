//! Graphics bridge: routes the game's D3D11/DXGI/D3DCompiler IAT calls to the
//! loaded backend (DXVK Native in phase 1; a native Rust Vulkan backend in
//! phase 2). The backend is a host shared library exposing the Windows D3D11
//! entry points with the Microsoft x64 calling convention DXVK Native provides.

use std::collections::HashMap;
use std::sync::Mutex;

/// The exact D3D11/DXGI bootstrap entry points the game imports (measured from
/// its IAT). Everything else is reached through the returned COM vtables, which
/// the backend (DXVK Native) implements.
pub const D3D_BOOTSTRAP: &[(&str, &str)] = &[
    ("d3d11.dll", "D3D11CreateDevice"),
    ("dxgi.dll", "CreateDXGIFactory1"),
    ("d3dcompiler_47.dll", "D3DReflect"),
];

pub struct GraphicsBridge {
    handles: Mutex<HashMap<String, *mut std::ffi::c_void>>,
    symbols: Mutex<HashMap<String, usize>>,
}

unsafe impl Send for GraphicsBridge {}
unsafe impl Sync for GraphicsBridge {}

impl Default for GraphicsBridge {
    fn default() -> Self { Self::new() }
}

impl GraphicsBridge {
    pub fn new() -> Self {
        Self { handles: Mutex::new(HashMap::new()), symbols: Mutex::new(HashMap::new()) }
    }

    /// Load a backend shared library (e.g. libdxvk_d3d11.so) from a private path.
    pub fn load_library(&self, dll: &str, path: &str) -> bool {
        let key = dll.to_ascii_lowercase();
        unsafe {
            let cpath = match std::ffi::CString::new(path) { Ok(c) => c, Err(_) => return false };
            let h = dlopen(cpath.as_ptr(), RTLD_NOW | RTLD_LOCAL);
            if h.is_null() {
                crate::glog!("[gow.gfx] dlopen failed for {path}");
                return false;
            }
            self.handles.lock().unwrap().insert(key, h);
            crate::glog!("[gow.gfx] loaded backend for {dll} from {path}");
            true
        }
    }

    /// Resolve a D3D entry point from the loaded backend.
    pub fn resolve(&self, dll: &str, func: &str) -> Option<usize> {
        let key = format!("{}!{}", dll.to_ascii_lowercase(), func);
        if let Some(&p) = self.symbols.lock().unwrap().get(&key) {
            return Some(p);
        }
        let handle = *self.handles.lock().unwrap().get(&dll.to_ascii_lowercase())?;
        unsafe {
            let cname = std::ffi::CString::new(func).ok()?;
            let sym = dlsym(handle, cname.as_ptr());
            if sym.is_null() { return None; }
            let addr = sym as usize;
            self.symbols.lock().unwrap().insert(key, addr);
            Some(addr)
        }
    }

    pub fn is_loaded(&self) -> bool { !self.handles.lock().unwrap().is_empty() }

    /// Attempt to bring up the D3D11 backend the way DXVK Native expects: load
    /// `libdxvk_d3d11.so` / `libdxvk_dxgi.so` from the app's native library dir.
    pub fn autoload(&self, native_lib_dir: &str) -> bool {
        let d3d11 = format!("{native_lib_dir}/libdxvk_d3d11.so");
        let dxgi = format!("{native_lib_dir}/libdxvk_dxgi.so");
        let mut any = false;
        if std::path::Path::new(&d3d11).exists() {
            any |= self.load_library("d3d11.dll", &d3d11);
        }
        if std::path::Path::new(&dxgi).exists() {
            any |= self.load_library("dxgi.dll", &dxgi);
        }
        if !any {
            crate::glog!("[gow.gfx] no DXVK Native libraries found in {native_lib_dir} (APK is runtime-only)");
        }
        any
    }

    /// Run `addr` with 6 raw args (Microsoft x64) and return RAX.
    pub unsafe fn call_addr(addr: usize, args: [u64; 6]) -> u64 {
        type F = unsafe extern "system" fn(u64, u64, u64, u64, u64, u64) -> u64;
        let f: F = std::mem::transmute(addr);
        f(args[0], args[1], args[2], args[3], args[4], args[5])
    }

    /// Forward a guest COM call (this in RCX, args in RDX/R8/R9/stack) to a host
    /// function using the Microsoft x64 convention.
    pub unsafe fn call_addr_raw(addr: usize, c: *mut crate::dispatch::GowCpu) -> u64 {
        let cpu = &*c;
        let a = [
            cpu.gpr.rcx, cpu.gpr.rdx, cpu.gpr.r8, cpu.gpr.r9,
            *((cpu.gpr.rsp as *const u8).add(0x28) as *const u64),
            *((cpu.gpr.rsp as *const u8).add(0x30) as *const u64),
        ];
        Self::call_addr(addr, a)
    }
}

const RTLD_NOW: i32 = 2;
const RTLD_LOCAL: i32 = 0;

extern "C" {
    fn dlopen(path: *const std::os::raw::c_char, mode: i32) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, sym: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}
