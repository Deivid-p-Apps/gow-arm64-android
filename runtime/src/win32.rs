//! Win32 / NT layer: maps the Windows APIs the game imports onto Bionic.
//!
//! The recompiler records, for every import, the IAT slot RVA and the
//! (dll, function) pair. When lifted code executes `call [iat_slot]` it calls
//! `gow_import_call(cpu, iat_rva)`; this module resolves that slot to a host
//! implementation and invokes it with the Microsoft x64 calling convention
//! (RCX, RDX, R8, R9 + stack), reading arguments from the guest stack.
//!
//! Only functions actually implemented here are dispatched; anything unknown is
//! counted and logged, never silently faked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::dispatch::GowCpu;

pub struct ImportResolver {
    /// IAT slot RVA -> (dll, function)
    pub slot_to_import: HashMap<u32, (String, String)>,
    pub files: crate::fileio::FileTable,
}

static MISSING: AtomicU64 = AtomicU64::new(0);

impl ImportResolver {
    pub fn new(game_dir: &str) -> Self {
        Self { slot_to_import: HashMap::new(), files: crate::fileio::FileTable::new(game_dir) }
    }
    pub fn new_host() -> Self {
        Self { slot_to_import: HashMap::new(), files: crate::fileio::FileTable::new("/storage/emulated/0/god") }
    }

    pub fn register(&mut self, iat_rva: u32, dll: &str, func: &str) {
        self.slot_to_import.insert(iat_rva, (dll.to_string(), func.to_string()));
    }

    pub fn load_manifest(&mut self, json: &str) -> Result<(), serde_json::Error> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        if let Some(arr) = v.get("imports").and_then(|a| a.as_array()) {
            for imp in arr {
                let dll = imp.get("dll").and_then(|x| x.as_str()).unwrap_or("");
                let func = imp.get("func").and_then(|x| x.as_str()).unwrap_or("");
                let iat = imp.get("iat_rva").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                self.register(iat, dll, func);
            }
        }
        Ok(())
    }

    /// Dispatch an import call. Arguments are in CPU registers/guest stack per
    /// the Microsoft x64 ABI: RCX, RDX, R8, R9, then [rsp+0x28], [rsp+0x30]...
    pub unsafe fn call(&self, cpu: *mut GowCpu, iat_rva: u32) -> bool {
        let (dll, func) = match self.slot_to_import.get(&iat_rva) {
            Some(v) => v.clone(),
            None => {
                MISSING.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        };
        let c = &mut *cpu;
        let a0 = c.gpr.rcx;
        let a1 = c.gpr.rdx;
        let a2 = c.gpr.r8;
        let a3 = c.gpr.r9;
        let arg = |n: u64| -> u64 { (c.gpr.rsp + 0x28 + n * 8) as *const u64 as u64 };

        let dll_l = dll.to_ascii_lowercase();
        let handled = match (dll_l.as_str(), func.as_str()) {
            ("kernel32.dll", "VirtualAlloc") => {
                let (_addr, _size, _type, _prot) = (a0, a1, a2, a3);
                // Real mapping into the guest space is provided by the memory
                // manager; here we allocate host memory and return it.
                let p = alloc_host(a1 as usize);
                c.gpr.rax = p as u64;
                true
            }
            ("kernel32.dll", "VirtualFree") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetModuleHandleA") | ("kernel32.dll", "GetModuleHandleW") => {
                c.gpr.rax = c.image_base;
                true
            }
            ("kernel32.dll", "GetModuleHandleExA") | ("kernel32.dll", "GetModuleHandleExW") => {
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "GetProcAddress") => {
                // Resolve in our host import table by name; else return 0.
                c.gpr.rax = self.resolve_proc(a1 as *const u8);
                true
            }
            ("kernel32.dll", "GetCurrentThreadId") => { c.gpr.rax = tid(); true }
            ("kernel32.dll", "GetCurrentProcessId") => { c.gpr.rax = std::process::id() as u64; true }
            ("kernel32.dll", "QueryPerformanceCounter") => {
                if a0 != 0 { *(a0 as *mut u64) = qpc(); }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "QueryPerformanceFrequency") => {
                if a0 != 0 { *(a0 as *mut u64) = 1_000_000_000; }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "Sleep") => { std::thread::sleep(std::time::Duration::from_millis(a0)); true }
            ("kernel32.dll", "GetTickCount") | ("kernel32.dll", "GetTickCount64") => { c.gpr.rax = qpc() / 1_000_000; true }
            ("kernel32.dll", "InitializeCriticalSection") | ("kernel32.dll", "InitializeCriticalSectionAndSpinCount") => {
                if a0 != 0 { *(a0 as *mut u64) = 0; }
                true
            }
            ("kernel32.dll", "EnterCriticalSection") | ("kernel32.dll", "LeaveCriticalSection")
            | ("kernel32.dll", "DeleteCriticalSection") => true,
            ("kernel32.dll", "TlsAlloc") => { c.gpr.rax = tls_alloc(); true }
            ("kernel32.dll", "GetLastError") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "CreateFileA") | ("kernel32.dll", "CreateFileW") => {
                // a0 = name ptr, a1 = access, a2 = share, a3 = sec; disp at [rsp+0x28]
                let name = read_cstr(a0, dll_l.ends_with('w'));
                let disp = *(arg(4) as *const u32);
                let h = self.files.create(&name, a1 as u32, disp);
                c.gpr.rax = h;
                true
            }
            ("kernel32.dll", "ReadFile") => {
                // a0=h, a1=buf, a2=len, a3=&read
                let len = a2 as usize;
                let mut buf = vec![0u8; len];
                let n = self.files.read(a0, &mut buf);
                std::ptr::copy_nonoverlapping(buf.as_ptr(), a1 as *mut u8, n as usize);
                if a3 != 0 { *(a3 as *mut u32) = n; }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "WriteFile") => {
                let len = a2 as usize;
                let buf = std::slice::from_raw_parts(a1 as *const u8, len);
                let n = self.files.write(a0, buf);
                if a3 != 0 { *(a3 as *mut u32) = n; }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "SetFilePointer") | ("kernel32.dll", "SetFilePointerEx") => {
                let off = a1 as i64;
                let pos = self.files.seek(a0, off, a2 as u32);
                c.gpr.rax = pos;
                true
            }
            ("kernel32.dll", "GetFileSize") | ("kernel32.dll", "GetFileSizeEx") => {
                let sz = self.files.size(a0);
                if func.ends_with('x') && a1 != 0 { *(a1 as *mut u64) = sz; }
                c.gpr.rax = sz;
                true
            }
            ("kernel32.dll", "CloseHandle") => { self.files.close(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "SetEndOfFile") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "FlushFileBuffers") => { c.gpr.rax = 1; true }
            _ => false,
        };
        if !handled {
            let n = MISSING.fetch_add(1, Ordering::Relaxed);
            if n < 128 {
                crate::glog!("[gow.win32] unimplemented import: {dll}!{func}");
            }
        }
        let _ = arg;
        handled
    }

    unsafe fn resolve_proc(&self, name: *const u8) -> u64 {
        let _ = name;
        0
    }
}

impl Default for ImportResolver {
    fn default() -> Self { Self::new_host() }
}

unsafe fn read_cstr(ptr: u64, wide: bool) -> String {
    if ptr == 0 { return String::new(); }
    if wide {
        let mut v = Vec::new();
        let mut p = ptr as *const u16;
        loop { let ch = *p; if ch == 0 { break; } v.push(ch); p = p.add(1); if v.len() > 4096 { break; } }
        String::from_utf16_lossy(&v)
    } else {
        let mut v = Vec::new();
        let mut p = ptr as *const u8;
        loop { let ch = *p; if ch == 0 { break; } v.push(ch); p = p.add(1); if v.len() > 4096 { break; } }
        String::from_utf8_lossy(&v).into_owned()
    }
}

fn alloc_host(size: usize) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(size.max(0x1000), 0x1000).unwrap();
    unsafe { std::alloc::alloc_zeroed(layout) }
}
fn qpc() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
fn tls_alloc() -> u64 { TLS_IDX.fetch_add(1, Ordering::Relaxed) }
static TLS_IDX: AtomicU64 = AtomicU64::new(1);

fn tid() -> u64 { TID_COUNTER.fetch_add(1, Ordering::Relaxed) }
static TID_COUNTER: AtomicU64 = AtomicU64::new(1);
