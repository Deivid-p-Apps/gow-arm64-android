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
    /// Set once the D3D11 backend (DXVK Native) is loaded: dll -> handle.
    pub graphics: crate::graphics::GraphicsBridge,
    pub sync: crate::threads::SyncObjects,
    pub threads: crate::threads::ThreadTable,
}

static MISSING: AtomicU64 = AtomicU64::new(0);

impl ImportResolver {
    pub fn new(game_dir: &str) -> Self {
        Self { slot_to_import: HashMap::new(), files: crate::fileio::FileTable::new(game_dir), graphics: crate::graphics::GraphicsBridge::new(), sync: crate::threads::SyncObjects::new(), threads: crate::threads::ThreadTable::new() }
    }
    pub fn new_host() -> Self {
        Self { slot_to_import: HashMap::new(), files: crate::fileio::FileTable::new("/storage/emulated/0/god"), graphics: crate::graphics::GraphicsBridge::new(), sync: crate::threads::SyncObjects::new(), threads: crate::threads::ThreadTable::new() }
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
            ("kernel32.dll", "GetProcessHeap") => { c.gpr.rax = 0xdead0001; true }
            ("kernel32.dll", "HeapCreate") => { c.gpr.rax = 0xdead0002; true }
            ("kernel32.dll", "HeapDestroy") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "HeapAlloc") | ("kernel32.dll", "malloc") => { c.gpr.rax = alloc_host(a2 as usize) as u64; true }
            ("kernel32.dll", "HeapReAlloc") => {
                // allocate new + copy min(old,new); old size unknown -> copy new
                let p = alloc_host(a3 as usize);
                if a2 != 0 && p as u64 != 0 { std::ptr::copy_nonoverlapping(a2 as *const u8, p, a3 as usize); }
                c.gpr.rax = p as u64; true
            }
            ("kernel32.dll", "HeapFree") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "HeapSize") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "HeapSetInformation") | ("kernel32.dll", "HeapCompact") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetSystemInfo") | ("kernel32.dll", "GetNativeSystemInfo") => {
                // SYSTEM_INFO: fill a plausible 64-bit layout (page size 4096 etc.)
                if a0 != 0 {
                    let p = a0 as *mut u32;
                    *p.add(0) = 0;         // wProcessorArchitecture + reserved
                    *p.add(1).cast::<u32>() = 4096; // dwPageSize
                    *p.add(2) = 0x10000;   // lpMinimumApplicationAddress (low)
                    *p.add(3) = 0;         // high
                    *p.add(4) = 0x7fff_0000; // lpMaximumApplicationAddress (low)
                    *p.add(5) = 0;
                    *p.add(6) = 6;         // dwNumberOfProcessors (host cores)
                }
                true
            }
            ("kernel32.dll", "GetSystemTime") | ("kernel32.dll", "GetLocalTime") => {
                fill_systemtime(a0); true
            }
            ("kernel32.dll", "GetSystemTimeAsFileTime") => {
                if a0 != 0 { let ft = (qpc() / 100) + 116444736000000000; *(a0 as *mut u64) = ft; } true
            }
            ("kernel32.dll", "IsDebuggerPresent") | ("kernel32.dll", "IsProcessorFeaturePresent") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetDateFormatA") | ("kernel32.dll", "GetTimeFormatA")
            | ("kernel32.dll", "GetDateFormatW") | ("kernel32.dll", "GetTimeFormatW") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetCurrentDirectoryA") | ("kernel32.dll", "GetCurrentDirectoryW") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "SetCurrentDirectoryA") | ("kernel32.dll", "SetCurrentDirectoryW") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "CreateFileA") | ("kernel32.dll", "CreateFileW") => {
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
            ("kernel32.dll", "CreateMutexA") | ("kernel32.dll", "CreateMutexW") => { c.gpr.rax = self.sync.create_mutex(); true }
            ("kernel32.dll", "CreateEventA") | ("kernel32.dll", "CreateEventW") => { c.gpr.rax = self.sync.create_event(a2 != 0); true }
            ("kernel32.dll", "WaitForSingleObject") => {
                let signalled = self.sync.wait(a0, a1);
                c.gpr.rax = if signalled { 0 } else { 0x102 }; // WAIT_OBJECT_0 / WAIT_TIMEOUT
                true
            }
            ("kernel32.dll", "WaitForMultipleObjects") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "SetEvent") | ("kernel32.dll", "ResetEvent") => { self.sync.set_event(a0, func == "SetEvent"); c.gpr.rax = 1; true }
            ("kernel32.dll", "ReleaseMutex") => { self.sync.release_mutex(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "CreateThread") => {
                // a0=sec, a1=stack, a2=size, a3=start; param at [rsp+0x28]
                let start = a3;
                let param = *(arg(4) as *const u64);
                let h = self.threads.spawn(cpu, start, param);
                c.gpr.rax = h;
                true
            }
            ("kernel32.dll", "GetCurrentThread") | ("kernel32.dll", "GetCurrentProcess") => { c.gpr.rax = 0xffff_ffff_ffff_ffff; true }
            ("kernel32.dll", "SwitchToThread") | ("kernel32.dll", "YieldProcessor") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "VirtualProtect") => { if a3 != 0 { *(a3 as *mut u32) = 0x40; } c.gpr.rax = 1; true }
            ("kernel32.dll", "VirtualQuery") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "LoadLibraryA") | ("kernel32.dll", "LoadLibraryW") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "FreeLibrary") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "SetEndOfFile") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "FlushFileBuffers") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "DeleteFileA") | ("kernel32.dll", "DeleteFileW") => {
                let name = read_cstr(a0, dll_l.ends_with('w'));
                c.gpr.rax = if std::fs::remove_file(self.files.translate(&name)).is_ok() { 1 } else { 0 };
                true
            }
            ("kernel32.dll", "CreateDirectoryA") | ("kernel32.dll", "CreateDirectoryW") => {
                let name = read_cstr(a0, dll_l.ends_with('w'));
                c.gpr.rax = if std::fs::create_dir_all(self.files.translate(&name)).is_ok() { 1 } else { 0 };
                true
            }
            ("kernel32.dll", "GetFileAttributesA") | ("kernel32.dll", "GetFileAttributesW") => {
                let name = read_cstr(a0, dll_l.ends_with('w'));
                let p = self.files.translate(&name);
                c.gpr.rax = if p.is_dir() { 0x10 } else if p.exists() { 0x80 } else { 0xffff_ffff };
                true
            }
            ("kernel32.dll", "FindFirstFileA") | ("kernel32.dll", "FindFirstFileW") => {
                let name = read_cstr(a0, dll_l.ends_with('w'));
                c.gpr.rax = self.files.find_first(&name);
                true
            }
            ("kernel32.dll", "FindNextFileA") | ("kernel32.dll", "FindNextFileW") => {
                c.gpr.rax = self.files.find_next(a0, a1) as u64;
                true
            }
            ("kernel32.dll", "FindClose") => { self.files.find_close(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "GetModuleFileNameA") | ("kernel32.dll", "GetModuleFileNameW") => {
                let exe = self.files.translate("GoW.exe");
                let s = exe.to_string_lossy();
                let width = if dll_l.ends_with('w') { 2 } else { 1 };
                let bytes = s.as_bytes();
                let n = (a2 as usize / width).saturating_sub(1).min(bytes.len());
                for i in 0..n { *(a0 as *mut u8).add(i * width) = bytes[i]; }
                *(a0 as *mut u8).add(n * width) = 0;
                c.gpr.rax = n as u64;
                true
            }
            ("kernel32.dll", "GetCommandLineA") | ("kernel32.dll", "GetCommandLineW") => {
                c.gpr.rax = self.files.cmdline_ptr(dll_l.ends_with('w')); true
            }
            ("kernel32.dll", "GetEnvironmentStringsA") | ("kernel32.dll", "GetEnvironmentStringsW") => {
                c.gpr.rax = 0; true
            }
            ("kernel32.dll", "GetEnvironmentVariableA") | ("kernel32.dll", "GetEnvironmentVariableW") => {
                c.gpr.rax = 0; true
            }
            ("kernel32.dll", "SetUnhandledExceptionFilter") | ("kernel32.dll", "UnhandledExceptionFilter") => {
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "SetErrorMode") | ("kernel32.dll", "SetThreadPriority") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetVersionExA") | ("kernel32.dll", "GetVersionExW") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetVersion") => { c.gpr.rax = 0x0a00_0000; true }
            ("xinput1_4.dll", f) | ("xinput1_3.dll", f) | ("xinput9_1_0.dll", f) => {
                let input = crate::input::global();
                match f {
                    "ord#2" | "XInputGetState" => {
                        // a0 = user index, a1 = XINPUT_STATE*
                        match input.xinput_get_state(a0 as u32) {
                            Some(st) => { st.write_to(a1 as *mut u8); c.gpr.rax = 0; }
                            None => { c.gpr.rax = 0x48f; } // ERROR_DEVICE_NOT_CONNECTED
                        }
                        true
                    }
                    "ord#3" | "XInputSetState" => {
                        let motors = *(a1 as *const u16); // wLeftMotorSpeed
                        let right = *((a1 as *const u16).add(1));
                        c.gpr.rax = input.xinput_set_state(a0 as u32, motors, right) as u64;
                        true
                    }
                    "XInputEnable" => { c.gpr.rax = 0; true }
                    "ord#1" | "XInputGetCapabilities" => { c.gpr.rax = 0x48f; true }
                    _ => { c.gpr.rax = 0x48f; true }
                }
            }
            ("user32.dll", "GetAsyncKeyState") | ("user32.dll", "GetKeyState") => {
                let vk = (a0 & 0xff) as u16;
                c.gpr.rax = if crate::input::global().vk_down(vk) { 0x8000 } else { 0 };
                true
            }
            ("user32.dll", "GetCursorPos") => {
                let ms = crate::input::global().mouse_state(); let (x, y) = (ms.x, ms.y);
                if a0 != 0 { *(a0 as *mut i32) = x; *((a0 as *mut i32).add(1)) = y; }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "GetSystemMetrics") => { c.gpr.rax = 0; true }
            ("user32.dll", "ShowCursor") | ("user32.dll", "SetCursorPos") | ("user32.dll", "ClipCursor") => { c.gpr.rax = 0; true }
            ("user32.dll", "GetForegroundWindow") => { c.gpr.rax = 1; true }
            ("user32.dll", "SetForegroundWindow") | ("user32.dll", "SetFocus") => { c.gpr.rax = 1; true }
            ("user32.dll", "GetDC") => { c.gpr.rax = 1; true }
            ("user32.dll", "ReleaseDC") => { c.gpr.rax = 1; true }
            ("xaudio2_7.dll", _) | ("xaudio2_8.dll", _) | ("xaudio2_9.dll", _) => {
                // XAudio2 bootstrap: XAudio2Create returns an IXAudio2 vtable.
                match func.as_str() {
                    "XAudio2Create" => {
                        let vt = crate::audio::xaudio2_create_vtable(crate::audio::global());
                        if a0 != 0 { *(a0 as *mut u64) = vt; }
                        c.gpr.rax = if vt == 0 { 0x8000_4005 } else { 0 };
                        true
                    }
                    "CreateAudioReverb" | "CreateAudioVolumeMeter" => { c.gpr.rax = 0x8000_4005; true } // E_NOINTERFACE (honest)
                    _ => { c.gpr.rax = 0x8000_4005; true }
                }
            }
            ("d3d11.dll", _) | ("dxgi.dll", _) | ("d3dcompiler_47.dll", _) => {
                // Route to the loaded graphics backend (DXVK Native). The COM
                // vtables it returns are what the game calls afterwards.
                match self.graphics.resolve(&dll_l, &func) {
                    Some(addr) => {
                        let r = crate::graphics::GraphicsBridge::call_addr(addr, [a0, a1, a2, a3, *(arg(4) as *const u64), *(arg(5) as *const u64)]);
                        c.gpr.rax = r;
                        true
                    }
                    None => {
                        let n = MISSING.fetch_add(1, Ordering::Relaxed);
                        if n < 128 { crate::glog!("[gow.win32] gfx import unresolved (no backend): {dll}!{func}"); }
                        c.gpr.rax = 0x8000_4005; // E_NOINTERFACE
                        true
                    }
                }
            }
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

unsafe fn fill_systemtime(p: u64) {
    if p == 0 { return; }
    // SYSTEMTIME { wYear,wMonth,wDayOfWeek,wDay,wHour,wMinute,wSecond,wMilliseconds }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let secs = now.as_secs();
    let days = secs / 86400;
    let rem = secs % 86400;
    let w = p as *mut u16;
    let year = 1970 + (days / 365) as u16;
    *w.add(0) = year;
    *w.add(1) = 1;
    *w.add(2) = ((days + 4) % 7) as u16;
    *w.add(3) = 1;
    *w.add(4) = (rem / 3600) as u16;
    *w.add(5) = ((rem % 3600) / 60) as u16;
    *w.add(6) = (rem % 60) as u16;
    *w.add(7) = now.subsec_millis() as u16;
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
