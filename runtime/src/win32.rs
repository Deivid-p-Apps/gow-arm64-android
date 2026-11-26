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
//!
//! Heap policy (D013/D017): every allocation is tracked (ptr -> size/align) so
//! free/HeapFree/HeapReAlloc/VirtualFree actually return memory. Small blocks
//! come from a committed budget (`GOW_HEAP_MB`, default 3072 MiB); blocks of
//! 32 MiB or more are mmap'd with MAP_NORESERVE instead, so a multi-GiB engine
//! arena reserves address space and materialises zeroed pages on first touch
//! (Windows-like lazy commit) instead of being refused — a refused arena left
//! the game booting from NULL and crashing with wild pointers.
//! Module policy (D014): LoadLibrary hands out
//! pseudo-module handles and GetProcAddress serves hostcall thunks (DXVK Native
//! for d3d11/dxgi/d3dcompiler), so games that resolve D3D11 dynamically work.
//!
//! Build #155 post-mortem (D018):
//! - VirtualAlloc(arena_addr, size, MEM_COMMIT) ignored lpAddress and allocated
//!   FRESH memory, so the engine's 8 GiB arena + a 128 MiB in-arena commit
//!   produced a pointer OUTSIDE the arena → wild negative addresses → the
//!   badstore cascade → SIGSEGV at 0x80. Commits inside a tracked reservation
//!   now return the requested base, exactly like Windows.
//! - Every guest pointer written through by a handler is validated with
//!   `okp` (null page / kernel range / wrap-around rejected): the degraded
//!   engine state passed pointers like 0x80 to out-parameters and killed the
//!   host inside the runtime.
//!
//! Build #157 post-mortem (D019):
//! - CreateFiber was unimplemented and left RAX untouched, so the engine's job
//!   manager stored GARBAGE as 30 fiber handles and later CALLED them (the
//!   [gow.MISSCALL] indirect target_va=0xb400...0010/0090/0110... family from
//!   the worker threads — each "handle" was the address of its own table
//!   slot). CreateFiber now returns a hostcall thunk that runs the fiber proc
//!   nested on the calling thread (rcx = param), and SwitchToFiber runs the
//!   target thunk the same way.
//! - ResumeThread returned garbage for the six _beginthreadex worker handles;
//!   _beginthreadex now routes through the ThreadTable so the handle is real
//!   and supports ResumeThread/WaitForSingleObject/GetThreadId.
//! - GetFileVersionInfo/VerQueryValue (VERSION.dll) were unimplemented; the
//!   engine's version probe read garbage and walked into its TerminateProcess
//!   error path. A minimal VS_FIXEDFILEINFO is served now.
//! - TerminateProcess is answered with success but does NOT kill the host:
//!   the engine called it during init and still made progress afterwards;
//!   dying there would just move the black screen 30 ms earlier.
//! - Real counting semaphores (ReleaseSemaphore signals waiters — the old
//!   "always signalled" event made every WaitForSingleObject a busy spin),
//!   condition variables keyed by guest address (Initialize/Sleep/Wake/WakeAll
//!   from kernel32 and api-ms-win-core-synch-l1-2-0), a real
//!   WaitForMultipleObjects, and SRW-lock no-ops.
//! - LoadLibrary/GetModuleHandle of a path whose basename is empty (e.g.
//!   "C:\") returns 0 like Windows instead of registering a "" pseudo-module.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

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

    pub fn slot_count(&self) -> usize { self.slot_to_import.len() }

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
        self.call_named(cpu, &dll, &func)
    }

    /// Dispatch an import by (dll, func) — the path shared by IAT calls and the
    /// GetProcAddress thunks. Same Microsoft x64 ABI as `call`.
    pub unsafe fn call_named(&self, cpu: *mut GowCpu, dll: &str, func: &str) -> bool {
        let c = &mut *cpu;
        let a0 = c.gpr.rcx;
        let a1 = c.gpr.rdx;
        let a2 = c.gpr.r8;
        let a3 = c.gpr.r9;
        let arg = |n: u64| -> u64 { (c.gpr.rsp + 0x28 + n * 8) as *const u64 as u64 };
        // 5th/6th args live on the shadow stack (after the 4 register args).
        // Validate the guest stack slot before reading it: with a degraded
        // engine state (build #155) the guest rsp could go wild and a raw
        // read here killed the host.
        let a4 = if crate::dispatch::plausible_mem(arg(0), 8) { *(arg(0) as *const u64) } else { 0 };
        let a5 = if crate::dispatch::plausible_mem(arg(1), 8) { *(arg(1) as *const u64) } else { 0 };

        let dll_l = dll.to_ascii_lowercase();
        let handled = match (dll_l.as_str(), func) {
            ("kernel32.dll", "VirtualAlloc") => {
                if a1 >= (1024 * 1024) && std::env::var_os("GOW_MEMTRACE").is_some() {
                    crate::glog!("[gow.VirtualAlloc] base=0x{a0:x} size=0x{a1:x} type=0x{a2:x} prot=0x{a3:x}");
                }
                // VirtualAlloc(lpAddress, dwSize, flAllocationType, flProtect):
                // lpAddress in RCX, dwSize in RDX. The old handler ignored
                // lpAddress whenever dwSize != 0 — which is every real commit —
                // so an in-arena MEM_COMMIT (engine reserved 8 GiB, then
                // committed a 128 MiB piece) allocated FRESH memory instead of
                // returning the arena address. The engine's memory manager then
                // ran on pointers outside its arena → wild negative addresses →
                // the badstore cascade and the SIGSEGV at 0x80 (build #155).
                // Windows semantics: committing inside an existing reservation
                // returns the requested base; the mapping is already RW.
                let mut p: *mut u8 = std::ptr::null_mut();
                if a0 != 0 {
                    if heap_block_containing(a0).is_some() {
                        p = a0 as *mut u8; // commit within a reservation: in place
                    } else if a1 == 0 {
                        p = a0 as *mut u8; // old quirk: marker commit with size 0
                    }
                }
                if p.is_null() && a1 != 0 {
                    if a0 != 0 {
                        let n = MISSING.fetch_add(1, Ordering::Relaxed);
                        if n < 32 {
                            crate::glog!("[gow.heap] VirtualAlloc(0x{a0:x}, 0x{a1:x}) at untracked address -> fresh allocation");
                        }
                    }
                    p = host_alloc_pages(a1 as usize);
                }
                if std::env::var_os("GOW_MEMTRACE").is_some() || (p.is_null() && a1 >= (1024 * 1024)) {
                    crate::glog!("[gow.heap] VirtualAlloc(base=0x{a0:x}, size=0x{a1:x}, type=0x{a2:x}) -> 0x{:x}", p as u64);
                }
                c.gpr.rax = p as u64;
                true
            }
            ("kernel32.dll", "VirtualFree") => {
                // VirtualFree(lpAddress, dwSize, dwFreeType): type in R8.
                // MEM_RELEASE (0x8000) frees the whole block (base required);
                // MEM_DECOMMIT (0x4000) on a sub-range is a no-op success — the
                // lazy pages simply stay untouched (Windows would decommit
                // them; we never committed eagerly). The old code freed on ANY
                // call type and logged every engine sub-range decommit as an
                // error.
                const MEM_DECOMMIT: u64 = 0x4000;
                if a2 & MEM_DECOMMIT == 0 {
                    let freed = heap_free_raw(a0 as *mut u8);
                    if !freed && a0 != 0 {
                        let n = MISSING.fetch_add(1, Ordering::Relaxed);
                        if n < 32 { crate::glog!("[gow.heap] VirtualFree(0x{a0:x}) of untracked/interior address ignored"); }
                    }
                }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "GetModuleHandleA") | ("kernel32.dll", "GetModuleHandleW") => {
                // The exe itself -> image base; anything else -> pseudo-module
                // (D014) so GetProcAddress has something to resolve against.
                let name = read_cstr(a0, func.ends_with('W'));
                let h = if name.is_empty() {
                    c.image_base
                } else {
                    let b = dll_basename(&name);
                    if b.is_empty() {
                        0 // a path that names no module (e.g. "C:\"): fail like Windows
                    } else if b.ends_with(".exe") { c.image_base } else { module_handle_for(&b) }
                };
                c.gpr.rax = h;
                true
            }
            ("kernel32.dll", "GetModuleHandleExA") | ("kernel32.dll", "GetModuleHandleExW") => {
                // (flags, name, out): name in RDX, handle out in R8.
                // GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS (0x4): RDX is an
                // address inside the module, NOT a string — the old code read
                // it as one and registered garbage pseudo-modules.
                const FROM_ADDRESS: u64 = 0x4;
                let h = if a0 & FROM_ADDRESS != 0 {
                    if a1 >= c.image_base && a1 < c.image_base + c.image_size { c.image_base } else { 0 }
                } else {
                    let name = read_cstr(a1, func.ends_with('W'));
                    if name.is_empty() {
                        c.image_base
                    } else {
                        let b = dll_basename(&name);
                        if b.is_empty() {
                            0
                        } else if b.ends_with(".exe") { c.image_base } else { module_handle_for(&b) }
                    }
                };
                if okp(a2, 8) { *(a2 as *mut u64) = h; }
                c.gpr.rax = if h != 0 { 1 } else { 0 };
                true
            }
            ("kernel32.dll", "GetProcAddress") => {
                c.gpr.rax = self.get_proc_address(cpu, a0, a1);
                true
            }
            ("kernel32.dll", "GetCurrentThreadId") => { c.gpr.rax = tid(); true }
            ("kernel32.dll", "GetThreadId") => {
                // The stable id behind a CreateThread handle (D017): the CRT and
                // the engine's job system ask for this right after spawning;
                // 0 (the old unimplemented answer) made them take error paths.
                c.gpr.rax = self.threads.thread_id(a0);
                true
            }
            ("kernel32.dll", "GetCurrentProcessId") => { c.gpr.rax = std::process::id() as u64; true }
            ("kernel32.dll", "QueryPerformanceCounter") => {
                if okp(a0, 8) { *(a0 as *mut u64) = qpc(); }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "QueryPerformanceFrequency") => {
                if okp(a0, 8) { *(a0 as *mut u64) = 1_000_000_000; }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "Sleep") => { std::thread::sleep(std::time::Duration::from_millis(a0)); true }
            ("kernel32.dll", "SleepEx") => {
                // Sleep in slices of <= 20 ms (alertable waits are moot here).
                let mut left = a0;
                while left > 0 {
                    let d = left.min(20);
                    std::thread::sleep(std::time::Duration::from_millis(d));
                    left -= d;
                }
                c.gpr.rax = 0; // WAIT_OBJECT_0
                true
            }
            ("kernel32.dll", "GetTickCount") | ("kernel32.dll", "GetTickCount64") => { c.gpr.rax = qpc() / 1_000_000; true }
            ("kernel32.dll", "InitializeCriticalSection") | ("kernel32.dll", "InitializeCriticalSectionAndSpinCount") => {
                if okp(a0, 8) { *(a0 as *mut u64) = 0; }
                true
            }
            ("kernel32.dll", "EnterCriticalSection") | ("kernel32.dll", "LeaveCriticalSection")
            | ("kernel32.dll", "DeleteCriticalSection") => true,
            ("kernel32.dll", "InitializeSRWLock") => {
                // SRW locks are advisory in this port: zero the 8-byte lock.
                if okp(a0, 8) { *(a0 as *mut u64) = 0; }
                true
            }
            ("kernel32.dll", "AcquireSRWLockExclusive") | ("kernel32.dll", "ReleaseSRWLockExclusive")
            | ("kernel32.dll", "AcquireSRWLockShared") | ("kernel32.dll", "ReleaseSRWLockShared") => true,
            // --- Condition variables (kernel32 + the api-ms-win-core-synch
            // set the CRT resolves through GetProcAddress). Keyed by the guest
            // address of the CONDITION_VARIABLE; wakes bump a generation
            // counter, sleepers wait for the generation to move. ---
            ("kernel32.dll", "InitializeConditionVariable")
            | ("api-ms-win-core-synch-l1-2-0.dll", "InitializeConditionVariable") => {
                if okp(a0, 8) { *(a0 as *mut u64) = 0; }
                self.sync.cv_touch(a0);
                true
            }
            ("kernel32.dll", "SleepConditionVariableCS") | ("kernel32.dll", "SleepConditionVariableSRW")
            | ("api-ms-win-core-synch-l1-2-0.dll", "SleepConditionVariableCS")
            | ("api-ms-win-core-synch-l1-2-0.dll", "SleepConditionVariableSRW") => {
                // (pcv, pcs/psrw, dwMilliseconds): timeout in R8. Our critical
                // sections/SRW locks are no-ops, so only the CV matters.
                let woken = self.sync.cv_wait(a0, a2);
                c.gpr.rax = if woken { 1 } else { 0 };
                true
            }
            ("kernel32.dll", "WakeConditionVariable") | ("kernel32.dll", "WakeAllConditionVariable")
            | ("api-ms-win-core-synch-l1-2-0.dll", "WakeConditionVariable")
            | ("api-ms-win-core-synch-l1-2-0.dll", "WakeAllConditionVariable") => {
                self.sync.cv_wake(a0, func.starts_with("WakeAll"));
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "TlsAlloc") => { c.gpr.rax = tls_alloc(); true }
            ("kernel32.dll", "TlsGetValue") => {
                // Read the per-thread slot from the SAME array the guest's raw
                // `mov rax,[gs:0x58]; ... [rax+idx*8]` access uses: the
                // ThreadLocalStoragePointer (a.k.a. the expansion slots). The old
                // code kept indices <64 in a separate inline area at TEB+0x148,
                // so a value written by TlsSetValue/FlsSetValue was invisible to
                // the engine's own compiled TLS reads (it read the expansion
                // array) — the job system stored its current fiber via FLS and
                // then read back 0, calling SwitchToFiber(0) (D039).
                let idx = a0;
                let teb = c.teb;
                let tls_ptr = if teb != 0 { *((teb + 0x58) as *const u64) } else { 0 };
                c.gpr.rax = if tls_ptr != 0 && idx < 4096 {
                    *((tls_ptr + idx * 8) as *const u64)
                } else { 0 };
                true
            }
            ("kernel32.dll", "TlsSetValue") => {
                let idx = a0;
                let val = a1;
                let teb = c.teb;
                let tls_ptr = if teb != 0 { *((teb + 0x58) as *const u64) } else { 0 };
                if tls_ptr != 0 && idx < 4096 {
                    *((tls_ptr + idx * 8) as *mut u64) = val;
                }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "TlsFree") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "FlsAlloc") => { c.gpr.rax = tls_alloc(); true }
            ("kernel32.dll", "FlsGetValue") => {
                let idx = a0;
                let teb = c.teb;
                let tls_ptr = if teb != 0 { *((teb + 0x58) as *const u64) } else { 0 };
                c.gpr.rax = if tls_ptr != 0 && idx < 4096 { *((tls_ptr + idx * 8) as *const u64) } else { 0 };
                true
            }
            ("kernel32.dll", "FlsSetValue") => {
                let idx = a0; let val = a1;
                let tls_ptr = if c.teb != 0 { *((c.teb + 0x58) as *const u64) } else { 0 };
                if tls_ptr != 0 && idx < 4096 { *((tls_ptr + idx * 8) as *mut u64) = val; }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "FlsFree") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetLastError") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetStartupInfoA") | ("kernel32.dll", "GetStartupInfoW") => {
                // Fill STARTUPINFO (zeroed is valid); STARTF_USESHOWWINDOW not set.
                let n = if func.ends_with('W') { 104 } else { 68 };
                if okp(a0, n as u64) {
                    std::ptr::write_bytes(a0 as *mut u8, 0, n);
                    // cb = sizeof(STARTUPINFO)
                    *(a0 as *mut u32) = n as u32;
                }
                true
            }
            ("kernel32.dll", "GetStdHandle") => { c.gpr.rax = 0xffff_fff4; true } // INVALID_HANDLE_VALUE-ish
            ("kernel32.dll", "SetConsoleCtrlHandler") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "IsDebuggerPresent") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "DebugBreak") | ("kernel32.dll", "RaiseException") => { true }
            ("kernel32.dll", "SetLastError") => { true }
            ("kernel32.dll", "InitializeSListHead") => {
                // SLIST_HEADER is 16 bytes; zero it.
                if okp(a0, 16) { std::ptr::write_bytes(a0 as *mut u8, 0, 16); }
                true
            }
            ("kernel32.dll", "CreateSemaphoreA") | ("kernel32.dll", "CreateSemaphoreW") => {
                // (attrs, lInitialCount, lMaximumCount, name): a real counting
                // semaphore (D019) — waiters block at 0 and ReleaseSemaphore
                // wakes them. The old "always signalled" event made every
                // WaitForSingleObject a busy spin and hid lost wakeups.
                c.gpr.rax = self.sync.create_semaphore(a1 as i64);
                true
            }
            ("kernel32.dll", "OpenSemaphoreA") | ("kernel32.dll", "OpenSemaphoreW") => {
                // Opening a semaphore we cannot see by name: hand out one
                // optimistically signalled (count 1) so a single wait passes.
                c.gpr.rax = self.sync.create_semaphore(1);
                true
            }
            ("kernel32.dll", "ReleaseSemaphore") => {
                // (hSemaphore, lReleaseCount, lpPreviousCount): mem in R8 is a
                // lie — the count is RDX and the out pointer is R8.
                // Signature: ReleaseSemaphore(h, count, prev): h=RCX, count=RDX, prev=R8.
                let mut prev: i64 = 0;
                let ok = self.sync.release_semaphore(a0, a1 as i64, &mut prev);
                if ok && okp(a2, 4) { *(a2 as *mut i32) = prev as i32; }
                c.gpr.rax = ok as u64;
                true
            }
            ("kernel32.dll", "GetCurrentProcessorNumber") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "SetThreadAffinityMask") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "WaitForSingleObjectEx") => {
                // (hObject, dwMilliseconds, bAlertable): same as WaitForSingleObject.
                let signalled = if self.threads.is_thread(a0) {
                    self.threads.wait_done(a0, a1)
                } else {
                    self.sync.wait(a0, a1)
                };
                c.gpr.rax = if signalled { 0 } else { 0x102 };
                true
            }
            ("user32.dll", "GetKeyboardLayout") => { c.gpr.rax = 0x0409; true }
            ("user32.dll", "LoadKeyboardLayoutA") | ("user32.dll", "LoadKeyboardLayoutW") => { c.gpr.rax = 0x0409; true }
            ("user32.dll", "GetKeyboardLayoutNameA") | ("user32.dll", "GetKeyboardLayoutNameW") => { c.gpr.rax = 1; true }
            // --- Windows version compatibility (the game checks it at startup;
            // returning "not supported" makes it show an error and exit). ---
            ("kernel32.dll", "VerifyVersionInfoW") | ("kernel32.dll", "VerifyVersionInfoA") => {
                // Claim the requested condition is satisfied: the game only wants
                // "Windows 7 or newer". We report success without parsing the mask.
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "VerSetConditionMask") => {
                // (condition_mask, type_mask, condition) -> new mask
                let out = a0 | (a1 & 0xff) * 0 | a1; // conservative combine
                c.gpr.rax = if a0 != 0 { a0 } else { out };
                true
            }
            ("kernel32.dll", "GetVersionExW") | ("kernel32.dll", "GetVersionExA") => {
                // OSVERSIONINFOEX: dwOSVersionInfoSize@0, major@4, minor@8, build@0xC,
                // platform@0x10. Report Windows 10 (10.0.19041).
                if okp(a0, 0x18) {
                    let p = a0 as *mut u32;
                    *p.add(1) = 10;
                    *p.add(2) = 0;
                    *p.add(3) = 19041;
                    *p.add(4) = 2; // VER_PLATFORM_WIN32_NT
                }
                c.gpr.rax = 1;
                true
            }
            // --- Unicode / locale (startup logging + paths) ---
            ("kernel32.dll", "MultiByteToWideChar") => {
                // (cp, flags, src, src_len, dst, dst_len) -> wide chars written
                let (src, src_len, dst, dst_len) = (a2, a3 as i64, a4, a5 as i64);
                if !okp(src, 1) {
                    c.gpr.rax = 0;
                } else {
                    let n = if src_len < 0 { read_cstr(src, false).len() } else { src_len as usize };
                    let out = if dst_len < 0 { n } else { n.min(dst_len as usize) };
                    for i in 0..out {
                        if !okp(src + i as u64, 1) { break; }
                        let ch = *(src as *const u8).add(i) as u16;
                        if okp(dst + i as u64 * 2, 2) { *(dst as *mut u16).add(i) = ch; }
                    }
                    c.gpr.rax = out as u64;
                }
                true
            }
            ("kernel32.dll", "WideCharToMultiByte") => {
                // (cp, flags, src, src_len, dst, dst_len, def, used)
                let (src, src_len, dst, dst_len) = (a2, a3 as i64, a4, a5 as i64);
                if !okp(src, 2) {
                    c.gpr.rax = 0;
                } else {
                    let n = if src_len < 0 {
                        let mut n = 0usize;
                        while n < 4096 && okp(src + n as u64 * 2, 2) && *(src as *const u16).add(n) != 0 { n += 1; }
                        n
                    } else { src_len as usize };
                    let out = if dst_len < 0 { n } else { n.min(dst_len as usize) };
                    for i in 0..out {
                        if !okp(src + i as u64 * 2, 2) { break; }
                        let ch = *(src as *const u16).add(i) as u8;
                        if okp(dst + i as u64, 1) { *(dst as *mut u8).add(i) = ch; }
                    }
                    c.gpr.rax = out as u64;
                }
                true
            }
            ("kernel32.dll", "GetUserDefaultLocaleName") => {
                // Write "en-US" as UTF-16; return length incl. NUL.
                let s: Vec<u16> = "en-US\0".encode_utf16().collect();
                if okp(a0, 16) {
                    for (i, ch) in s.iter().enumerate() { *(a0 as *mut u16).add(i) = *ch; }
                }
                c.gpr.rax = s.len() as u64;
                true
            }
            ("kernel32.dll", "GetSystemDefaultLocaleName") | ("kernel32.dll", "GetUserDefaultUILanguage") => {
                c.gpr.rax = 0x0409; // en-US
                true
            }
            ("kernel32.dll", "RtlCaptureContext") => {
                // Fill a CONTEXT with the current guest state (used by SEH/unwind
                // probes). Zero it and set Rip to the current guest rip.
                if okp(a0, 0x4d0) {
                    std::ptr::write_bytes(a0 as *mut u8, 0, 0x4d0);
                    *((a0 as *mut u8).add(0xf8) as *mut u64) = c.rip; // Rip
                }
                true
            }
            ("kernel32.dll", "RtlLookupFunctionEntry") => {
                // No unwind data on the host: report "not found" (0), which callers
                // treat as a leaf function.
                if okp(a2, 8) { *(a2 as *mut u64) = 0; }
                c.gpr.rax = 0;
                true
            }
            ("kernel32.dll", "RtlVirtualUnwind") => { c.gpr.rax = 0; true }
            ("user32.dll", "MessageBoxA") | ("user32.dll", "MessageBoxW") => {
                // Never block the port on a modal dialog: log the text and return IDOK.
                let text = read_cstr(a1, func.ends_with('W'));
                let cap = read_cstr(a2, func.ends_with('W'));
                crate::glog!("[gow.msgbox] {cap}: {text}");
                c.gpr.rax = 1; // IDOK
                true
            }
            ("user32.dll", "RegisterClassExA") | ("user32.dll", "RegisterClassExW") => {
                c.gpr.rax = 1; true
            }
            ("user32.dll", "CreateWindowExA") | ("user32.dll", "CreateWindowExW") => {
                // (exStyle, class, title, style, x, y, w, h, parent, menu, inst, param)
                let wide = func.ends_with('W');
                let title = read_cstr(a2, wide);
                let w = arg(2) as i32;
                let h = arg(3) as i32;
                let hwnd = crate::sdl2win::create_window(&title, w, h);
                crate::glog!("[gow.window] CreateWindowEx title={title:?} size={w}x{h} -> hwnd=0x{hwnd:x}");
                // 0 means SDL2 is unavailable (device/APK path): hand out an
                // opaque non-null handle so the engine still has a window object.
                c.gpr.rax = if hwnd != 0 { hwnd } else { 0x20001 };
                true
            }
            ("user32.dll", "ShowWindow") => {
                crate::sdl2win::show(a0, a1 != 0); // SW_HIDE == 0
                c.gpr.rax = 1; true
            }
            ("user32.dll", "DestroyWindow") => {
                crate::sdl2win::destroy(a0);
                c.gpr.rax = 1; true
            }
            ("user32.dll", "GetClientRect") | ("user32.dll", "GetWindowRect") => {
                let (w, h) = crate::sdl2win::window_size(a0).unwrap_or((1280, 720));
                if okp(a1, 16) {
                    *(a1 as *mut i32) = 0;
                    *((a1 + 4) as *mut i32) = 0;
                    *((a1 + 8) as *mut i32) = w;
                    *((a1 + 12) as *mut i32) = h;
                }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "AdjustWindowRect") | ("user32.dll", "AdjustWindowRectEx") => { c.gpr.rax = 1; true }
            ("user32.dll", "ClientToScreen") | ("user32.dll", "ScreenToClient") => { c.gpr.rax = 1; true }
            ("user32.dll", "SetWindowPos") => {
                let (w, h) = (a4 as i32, a5 as i32);
                if w > 0 && h > 0 { crate::sdl2win::resize(a0, w, h); }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "MoveWindow") => {
                let (w, h) = (a3 as i32, a4 as i32);
                if w > 0 && h > 0 { crate::sdl2win::resize(a0, w, h); }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "PeekMessageA") | ("user32.dll", "PeekMessageW") => {
                crate::sdl2win::pump();
                c.gpr.rax = 0; true // no synthesised Win32 messages yet
            }
            ("user32.dll", "DispatchMessageA") | ("user32.dll", "DispatchMessageW")
            | ("user32.dll", "DefWindowProcA") | ("user32.dll", "DefWindowProcW")
            | ("user32.dll", "CallWindowProcW")
            | ("user32.dll", "SendMessageA") | ("user32.dll", "SendMessageW") => {
                c.gpr.rax = 0; true
            }
            ("user32.dll", "TranslateMessage") => { c.gpr.rax = 1; true }
            ("user32.dll", "IsWindow") => { c.gpr.rax = if a0 != 0 { 1 } else { 0 }; true }
            ("user32.dll", "GetWindowLongA") | ("user32.dll", "GetWindowLongPtrW") => { c.gpr.rax = 0; true }
            ("user32.dll", "SetWindowLongA") | ("user32.dll", "SetWindowLongPtrW") => { c.gpr.rax = 0; true }
            ("user32.dll", "GetWindowThreadProcessId") => {
                if okp(a1, 4) { *(a1 as *mut u32) = std::process::id(); }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "MonitorFromWindow") => { c.gpr.rax = 0x10001; true }
            ("user32.dll", "GetMonitorInfoA") | ("user32.dll", "GetMonitorInfoW") => {
                if okp(a1, 40) {
                    *(a1 as *mut u32) = 40;                 // cbSize
                    for off in [4u64, 20u64] {              // rcMonitor, rcWork
                        *((a1 + off) as *mut i32) = 0;
                        *((a1 + off + 4) as *mut i32) = 0;
                        *((a1 + off + 8) as *mut i32) = 1280;
                        *((a1 + off + 12) as *mut i32) = 720;
                    }
                    *((a1 + 36) as *mut u32) = 1;           // MONITORINFOF_PRIMARY
                }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "EnumDisplayMonitors") => { c.gpr.rax = 0; true }
            ("user32.dll", "EnumDisplayDevicesA") | ("user32.dll", "EnumDisplayDevicesW") => { c.gpr.rax = 0; true }
            ("user32.dll", "EnumDisplaySettingsA") | ("user32.dll", "EnumDisplaySettingsW") => {
                if okp(a2, 220) {                           // DEVMODEA/W
                    std::ptr::write_bytes(a2 as *mut u8, 0, 220);
                    *((a2 + 72) as *mut u32) = 1280;        // dmPelsWidth
                    *((a2 + 76) as *mut u32) = 720;         // dmPelsHeight
                    *((a2 + 80) as *mut u32) = 32;          // dmBitsPerPel
                    *((a2 + 84) as *mut u32) = 60;          // dmDisplayFrequency
                }
                c.gpr.rax = 1; true
            }
            ("user32.dll", "GetWindowPlacement") | ("user32.dll", "SetWindowPlacement") => { c.gpr.rax = 1; true }
            ("user32.dll", "BeginPaint") => { c.gpr.rax = 0; true }
            ("user32.dll", "EndPaint") => { c.gpr.rax = 1; true }
            ("user32.dll", "InvalidateRect") => { c.gpr.rax = 1; true }
            ("user32.dll", "GetParent") | ("user32.dll", "GetAncestor") => { c.gpr.rax = 0; true }
            ("user32.dll", "SetCapture") => { c.gpr.rax = 0x10001; true }
            ("user32.dll", "ReleaseCapture") => { c.gpr.rax = 1; true }
            ("user32.dll", "GetCapture") => { c.gpr.rax = 0; true }
            ("user32.dll", "TrackMouseEvent") => { c.gpr.rax = 1; true }
            ("user32.dll", "RegisterRawInputDevices") => { c.gpr.rax = 1; true }
            ("user32.dll", "GetRawInputData") => { c.gpr.rax = 0; true }
            ("user32.dll", "RegisterHotKey") | ("user32.dll", "UnregisterHotKey") => { c.gpr.rax = 1; true }
            ("user32.dll", "RegisterDeviceNotificationW") => { c.gpr.rax = 0; true }
            ("user32.dll", "UnregisterDeviceNotification") => { c.gpr.rax = 1; true }
            ("user32.dll", "SetProcessDpiAwarenessContext") | ("user32.dll", "SetThreadDpiAwarenessContext") => { c.gpr.rax = 1; true }
            ("user32.dll", "GetCursorInfo") => { c.gpr.rax = 0; true }
            ("user32.dll", "PtInRect") => {
                let (l, t, r, b) = if okp(a0, 16) {
                    (*((a0) as *const i32), *((a0 + 4) as *const i32), *((a0 + 8) as *const i32), *((a0 + 12) as *const i32))
                } else { (0, 0, 0, 0) };
                let x = a1 as u32 as i32;
                let y = (a1 >> 32) as u32 as i32;
                c.gpr.rax = if x >= l && x < r && y >= t && y < b { 1 } else { 0 };
                true
            }
            ("user32.dll", "VkKeyScanA") => { c.gpr.rax = 0; true }
            ("user32.dll", "MapVirtualKeyA") | ("user32.dll", "MapVirtualKeyExA") => { c.gpr.rax = 0; true }
            ("user32.dll", "LoadCursorA") | ("user32.dll", "LoadCursorW")
            | ("user32.dll", "LoadIconA") | ("user32.dll", "LoadIconW") => { c.gpr.rax = 1; true }
            ("user32.dll", "PostQuitMessage") => { c.gpr.rax = 0; true }
            ("user32.dll", "GetDesktopWindow") | ("user32.dll", "GetForegroundWindow") => { c.gpr.rax = 0x10001; true }
            ("kernel32.dll", "GetConsoleWindow") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "FreeConsole") | ("kernel32.dll", "AttachConsole") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "AllocConsole") => { c.gpr.rax = 0; true }
            // SHGetKnownFolderPath(rfid, flags, hToken, PWSTR* ppszPath): the
            // engine asks for the Saved Games (and Documents) folder at boot;
            // the old unimplemented answer failed the call and the engine looped
            // on a "Permissions Error: Unable to access Saved Games" MessageBox
            // (device/host build #171). Create the folder under the game dir and
            // return it as a heap-allocated wide string. FOLDERID_SavedGames =
            // {4C5C32FF-BB9D-43b0-B5B4-2D72E54EAAA4}, Documents =
            // {FDD39AD0-238F-46AF-ADB4-6C85480369C7}: we key off the first
            // Guid dword + the data4 to pick the subfolder name.
            ("shell32.dll", "SHGetKnownFolderPath") => {
                // rfid is a pointer to the GUID (16 bytes) in the guest image.
                let mut name = "SavedGames";
                if okp(a0, 16) {
                    let d1 = *(a0 as *const u32);
                    let d4 = *((a0 + 12) as *const u32);
                    if d1 == 0xFDD39AD0 { name = "Documents"; }
                    let _ = d4;
                }
                let folder = self.files.game_dir().join(name);
                let _ = std::fs::create_dir_all(&folder);
                let path_w: Vec<u16> = folder.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = path_w.len() * 2;
                let mem = crate::win32::host_alloc(bytes);
                if !mem.is_null() {
                    std::ptr::copy_nonoverlapping(path_w.as_ptr() as *const u8, mem, bytes);
                }
                if okp(a3, 8) { *(a3 as *mut u64) = mem as u64; }
                crate::glog!("[gow.shell] SHGetKnownFolderPath -> {name} = {folder:?}");
                c.gpr.rax = 0; // S_OK
                true
            }
            ("kernel32.dll", "GetProcessHeap") => { c.gpr.rax = 0xdead0001; true }
            // GetDiskFreeSpaceExW(lpDirectoryName, lpFreeBytesAvailableToCaller,
            // lpTotalNumberOfBytes, lpTotalNumberOfFreeBytes): out pointers in
            // RDX/R8/R9 (ULARGINTEGER 64-bit). The engine's "Not Enough Disk
            // Space" check read our uninitialized 0 and refused to boot
            // (host/device build #171); report a large free space on the host
            // filesystem (statvfs on the game dir).
            ("kernel32.dll", "GetDiskFreeSpaceExW") | ("kernel32.dll", "GetDiskFreeSpaceExA") => {
                let (free, total) = host_disk_space(self.files.game_dir());
                if okp(a1, 8) { *(a1 as *mut u64) = free; }
                if okp(a2, 8) { *(a2 as *mut u64) = total; }
                if okp(a3, 8) { *(a3 as *mut u64) = free; }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "GetDiskFreeSpaceW") | ("kernel32.dll", "GetDiskFreeSpaceA") => {
                // Old API: (root, &sectorsPerCluster, &bytesPerSector,
                // &freeClusters, &totalClusters) — DWORD outputs.
                if okp(a1, 4) { *(a1 as *mut u32) = 8; }        // sectors/cluster
                if okp(a2, 4) { *(a2 as *mut u32) = 512; }      // bytes/sector
                if okp(a3, 4) { *(a3 as *mut u32) = 0x100000; } // free clusters (~4 GiB)
                if okp(a4, 4) { *(a4 as *mut u32) = 0x200000; } // total clusters
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "LocalAlloc") | ("kernel32.dll", "GlobalAlloc") => {
                // (flags, bytes): bytes in RDX. The result is only ever passed to
                // LocalFree/GlobalFree, so a tracked host allocation is enough.
                c.gpr.rax = crate::win32::host_alloc(a1 as usize) as u64; true
            }
            ("kernel32.dll", "LocalFree") | ("kernel32.dll", "GlobalFree") => {
                if a0 != 0 { crate::win32::host_free(a0 as *mut u8); }
                c.gpr.rax = 0; true
            }
            ("ole32.dll", "CoTaskMemAlloc") => { c.gpr.rax = crate::win32::host_alloc(a0 as usize) as u64; true }
            ("ole32.dll", "CoTaskMemFree") => { if a0 != 0 { crate::win32::host_free(a0 as *mut u8); } c.gpr.rax = 0; true }
            ("ole32.dll", "CoTaskMemRealloc") => { c.gpr.rax = crate::win32::host_realloc(a0 as *mut u8, a1 as usize) as u64; true }
            ("kernel32.dll", "HeapCreate") => { c.gpr.rax = 0xdead0002; true }
            ("kernel32.dll", "HeapDestroy") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "HeapAlloc") => {
                // HeapAlloc(hHeap, dwFlags, dwBytes): bytes in R8. Tracked, so
                // HeapFree/HeapSize/HeapReAlloc know the block (D013).
                let p = heap_alloc_raw(a2 as usize, 0x10, false) as u64;
                if a2 >= (1024 * 1024) && std::env::var_os("GOW_MEMTRACE").is_some() {
                    crate::glog!("[gow.HeapAlloc] {} KiB -> 0x{p:x}", a2 / 1024);
                }
                c.gpr.rax = p;
                true
            }
            ("kernel32.dll", "malloc") => {
                // malloc(bytes): the size is arg 1 (RCX), NOT R8. Sharing the
                // HeapAlloc handler here read the wrong register, so a 32 MiB
                // malloc returned a tiny block — the engine's free-list then ran
                // off the end (f_40a610 badstore / SIGSEGV).
                c.gpr.rax = heap_alloc_raw(a0 as usize, 0x10, false) as u64;
                true
            }
            ("kernel32.dll", "HeapReAlloc") => {
                // HeapReAlloc(hHeap, dwFlags, lpMem, dwBytes). The old code copied
                // `dwBytes` from the old block regardless of its real size — a
                // heap overread. Tracked realloc copies min(old, new) and frees
                // the old block.
                c.gpr.rax = host_realloc(a2 as *mut u8, a3 as usize) as u64;
                true
            }
            ("kernel32.dll", "HeapFree") => {
                // HeapFree(hHeap, dwFlags, lpMem): mem in R8.
                host_free(a2 as *mut u8);
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "HeapSize") => {
                // HeapSize(hHeap, dwFlags, lpMem): mem in R8 -> the tracked size.
                c.gpr.rax = heap_size_raw(a2 as *mut u8) as u64;
                true
            }
            ("kernel32.dll", "HeapSetInformation") | ("kernel32.dll", "HeapCompact") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetLargePageMinimum") => {
                // Windows returns 0 when large pages are unavailable; a real
                // 2 MiB value makes the engine reserve huge pages that our
                // virtual allocator cannot back. Answer 0 (not supported) — the
                // truthful value here, not a fake success.
                c.gpr.rax = 0; true
            }
            ("kernel32.dll", "GlobalMemoryStatusEx") => {
                // MEMORYSTATUSEX: dwLength@0, dwMemoryLoad@4, ullTotalPhys@8,
                // ullAvailPhys@0x10, ullTotalPageFile@0x18, ullAvailPageFile@0x20,
                // ullTotalVirtual@0x28, ullAvailVirtual@0x30,
                // ullAvailExtendedVirtual@0x38. The engine's VRAM/RAM probe reads
                // these; leaving them 0 made its arena math degenerate and it
                // overran a block (f_40a610 free-list). Report the host's real
                // /proc/meminfo values plus a large virtual space.
                if okp(a0, 64) {
                    let b = a0 as *mut u8;
                    *((b as *mut u32)) = 64;                 // dwLength
                    let (total, avail) = host_meminfo();
                    *((b.add(4)) as *mut u32) = if total > 0 {
                        (100 - (avail * 100 / total)) as u32
                    } else { 50 };
                    *((b.add(8)) as *mut u64) = total;       // ullTotalPhys
                    *((b.add(0x10)) as *mut u64) = avail;    // ullAvailPhys
                    *((b.add(0x18)) as *mut u64) = total * 2;    // ullTotalPageFile
                    *((b.add(0x20)) as *mut u64) = avail * 2;    // ullAvailPageFile
                    *((b.add(0x28)) as *mut u64) = 0x7fff_0000_0000; // ullTotalVirtual
                    *((b.add(0x30)) as *mut u64) = 0x7f00_0000_0000; // ullAvailVirtual
                    *((b.add(0x38)) as *mut u64) = 0;        // ullAvailExtendedVirtual
                }
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "GetSystemInfo") | ("kernel32.dll", "GetNativeSystemInfo") => {
                // SYSTEM_INFO x64: wProcessorArchitecture@0 (u16), dwPageSize@4,
                // lpMinimumApplicationAddress@8, lpMaximumApplicationAddress@0x10,
                // dwActiveProcessorMask@0x18, dwNumberOfProcessors@0x20,
                // dwProcessorType@0x24, dwAllocationGranularity@0x28.
                if okp(a0, 48) {
                    let p = a0 as *mut u32;
                    *p.add(0) = 9;                       // PROCESSOR_ARCHITECTURE_AMD64
                    *p.add(1) = 4096;                    // dwPageSize
                    *p.add(2) = 0x10000;                 // lpMinimumApplicationAddress (low)
                    *p.add(3) = 0;                       // high
                    *p.add(4) = 0x7fff_0000;             // lpMaximumApplicationAddress (low)
                    *p.add(5) = 0;
                    *p.add(6) = 0xffff_ffff;             // dwActiveProcessorMask (low)
                    *p.add(7) = 0xffff_ffff;             // dwActiveProcessorMask (high)
                    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
                    *p.add(8) = cores as u32;            // dwNumberOfProcessors
                    *p.add(9) = 8664;                    // dwProcessorType (PROCESSOR_AMD_X8664)
                    *p.add(0xa) = 0x10000;               // dwAllocationGranularity
                }
                true
            }
            ("kernel32.dll", "GetSystemTime") | ("kernel32.dll", "GetLocalTime") => {
                if okp(a0, 16) { fill_systemtime(a0); }
                true
            }
            ("kernel32.dll", "GetSystemTimeAsFileTime") => {
                if okp(a0, 8) { let ft = (qpc() / 100) + 116444736000000000; *(a0 as *mut u64) = ft; } true
            }
            ("kernel32.dll", "IsProcessorFeaturePresent") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetDateFormatA") | ("kernel32.dll", "GetTimeFormatA")
            | ("kernel32.dll", "GetDateFormatW") | ("kernel32.dll", "GetTimeFormatW") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetCurrentDirectoryA") | ("kernel32.dll", "GetCurrentDirectoryW") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "SetCurrentDirectoryA") | ("kernel32.dll", "SetCurrentDirectoryW") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "CreateFileA") | ("kernel32.dll", "CreateFileW") => {
                // CreateFile(name, access, share, security, disposition, flags, template):
                // dwCreationDisposition is the 5th argument ([rsp+0x28]) — the
                // old code read it from the wrong stack slot.
                let name = read_cstr(a0, func.ends_with('W'));
                let disp = a4 as u32;
                let h = self.files.create(&name, a1 as u32, disp);
                c.gpr.rax = h;
                true
            }
            ("kernel32.dll", "ReadFile") => {
                // a0=h, a1=buf, a2=len, a3=&read
                let len = (a2 as usize).min(64 * 1024 * 1024);
                let mut buf = vec![0u8; len];
                let n = self.files.read(a0, &mut buf) as usize;
                if n > 0 && okp(a1, n as u64) {
                    std::ptr::copy_nonoverlapping(buf.as_ptr(), a1 as *mut u8, n);
                }
                if okp(a3, 4) { *(a3 as *mut u32) = n as u32; }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "WriteFile") => {
                let len = if okp(a1, a2) { a2 as usize } else { 0 };
                let buf = std::slice::from_raw_parts(a1 as *const u8, len);
                let n = self.files.write(a0, buf);
                if okp(a3, 4) { *(a3 as *mut u32) = n; }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "SetFilePointer") | ("kernel32.dll", "SetFilePointerEx") => {
                // SetFilePointer(h, off, &hi, method); SetFilePointerEx(h, &off, &new, method).
                // The move method is the 4th register arg (R9) in both; Ex takes
                // the offset by pointer (the old code read both from wrong slots).
                let off = if func.ends_with("Ex") {
                    if okp(a1, 8) { *(a1 as *const i64) } else { 0 }
                } else { a1 as i64 };
                let pos = self.files.seek(a0, off, a3 as u32);
                if func.ends_with("Ex") && okp(a2, 8) { *(a2 as *mut u64) = pos; }
                c.gpr.rax = pos;
                true
            }
            ("kernel32.dll", "GetFileSize") | ("kernel32.dll", "GetFileSizeEx") => {
                let sz = self.files.size(a0);
                if func.ends_with('x') && okp(a1, 8) { *(a1 as *mut u64) = sz; }
                c.gpr.rax = sz;
                true
            }
            ("kernel32.dll", "CloseHandle") => { self.files.close(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "CreateFileMappingA") | ("kernel32.dll", "CreateFileMappingW") => {
                // (file, attrs, protect, size_hi, size_lo): size_lo is the 5th
                // argument ([rsp+0x28]).
                let size = ((a3 & 0xffff_ffff) << 32) | (a4 & 0xffff_ffff);
                c.gpr.rax = self.files.create_mapping(a0, size);
                true
            }
            ("kernel32.dll", "MapViewOfFile") | ("kernel32.dll", "MapViewOfFileEx") => {
                // (mapping, access, off_hi, off_lo, count): count is the 5th arg.
                let off = ((a2 & 0xffff_ffff) << 32) | (a3 & 0xffff_ffff);
                c.gpr.rax = self.files.map_view(a0, off, a4);
                true
            }
            ("kernel32.dll", "UnmapViewOfFile") => { self.files.unmap_view(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "CreateMutexA") | ("kernel32.dll", "CreateMutexW") => { c.gpr.rax = self.sync.create_mutex(); true }
            ("kernel32.dll", "CreateEventA") | ("kernel32.dll", "CreateEventW") => { c.gpr.rax = self.sync.create_event(a2 != 0); true }
            ("kernel32.dll", "WaitForSingleObject") => {
                // Thread handles report completion (D019); sync objects as before.
                let signalled = if self.threads.is_thread(a0) {
                    self.threads.wait_done(a0, a1)
                } else {
                    self.sync.wait(a0, a1)
                };
                c.gpr.rax = if signalled { 0 } else { 0x102 }; // WAIT_OBJECT_0 / WAIT_TIMEOUT
                true
            }
            ("kernel32.dll", "WaitForMultipleObjects") | ("kernel32.dll", "WaitForMultipleObjectsEx") => {
                // (nCount, lpHandles, bWaitAll, dwMilliseconds). The old stub
                // always returned WAIT_OBJECT_0 (index 0) — good enough for a
                // single-object boot, wrong for every real multi-wait (D019).
                const WAIT_FAILED: u64 = 0xFFFF_FFFF;
                let n = (a0 as usize).min(64);
                if n == 0 || !okp(a1, (n as u64) * 8) {
                    c.gpr.rax = WAIT_FAILED;
                } else {
                    let mut hs: Vec<u64> = Vec::with_capacity(n);
                    for i in 0..n {
                        hs.push(*(a1 as *const u64).add(i));
                    }
                    // Poll (cheap; the sync layer has no cross-object wait):
                    // 2 ms slices until a handle is signalled or the deadline.
                    let infinite = a3 == 0xFFFF_FFFF;
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(a3.min(0xFFFF_FFFE));
                    let mut result: u64 = 0x102; // WAIT_TIMEOUT
                    loop {
                        if a2 != 0 {
                            if hs.iter().all(|h| self.obj_signalled(*h)) {
                                result = 0; // WAIT_OBJECT_0
                            }
                        } else {
                            for (i, h) in hs.iter().enumerate() {
                                if self.obj_signalled(*h) {
                                    result = i as u64; // WAIT_OBJECT_0 + index
                                    break;
                                }
                            }
                        }
                        if result != 0x102 || (!infinite && std::time::Instant::now() >= deadline) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    c.gpr.rax = result;
                }
                true
            }
            ("kernel32.dll", "SetEvent") | ("kernel32.dll", "ResetEvent") => { self.sync.set_event(a0, func == "SetEvent"); c.gpr.rax = 1; true }
            ("kernel32.dll", "ReleaseMutex") => { self.sync.release_mutex(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "CreateThread") => {
                // CreateThread(attr, stack_size, start_address, parameter, flags, out):
                // start = R8, parameter = R9, flags = 5th arg. CREATE_SUSPENDED
                // (0x4) means the engine wants to finish wiring the thread's
                // struct before it runs, so honour it.
                let start = a2;
                let param = a3;
                let flags = if okp(arg(0), 4) { *(arg(0) as *const u32) as u64 } else { 0 };
                let suspended = flags & 0x4 != 0;
                let h = self.threads.spawn_ex(cpu, start, param, suspended);
                c.gpr.rax = h;
                true
            }
            ("kernel32.dll", "ResumeThread") => {
                // Releases a CREATE_SUSPENDED thread's start gate. Returns the
                // previous suspend count (1 if it was suspended).
                c.gpr.rax = self.threads.resume(a0);
                true
            }
            ("kernel32.dll", "SuspendThread") => { c.gpr.rax = self.threads.suspend(a0); true }
            ("kernel32.dll", "TerminateThread") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "TerminateProcess") => {
                // The engine calls this during init (device log, build #157) —
                // its error/teardown path. Killing the host here would just end
                // the boot 30 ms earlier; the engine demonstrably continues and
                // creates its fibers/threads afterwards. Log loudly, say yes,
                // stay alive.
                let n = MISSING.fetch_add(1, Ordering::Relaxed);
                if n < 8 {
                    crate::glog!("[gow.win32] TerminateProcess(0x{a0:x}, status=0x{a1:x}) ignored — caller_rva=0x{:x} rsp=0x{:x}",
                        crate::crashlog::current_rva(), c.gpr.rsp);
                }
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "ExitProcess") => {
                crate::glog!("[gow.win32] ExitProcess({a0}) -> exiting");
                std::process::exit(a0 as i32);
            }
            // --- Fibers (D019). The engine's job manager creates ~30 fibers at
            // boot and its worker threads call the stored "handles" directly.
            // CreateFiber returns a hostcall THUNK: when called, it runs the
            // fiber proc nested on the calling thread with rcx = param, and
            // returns to the caller when the proc returns. SwitchToFiber runs
            // the target thunk the same way (cooperative approximation: a real
            // context switch-back is not modelled — a fiber that never returns
            // keeps its thread, which is what a worker loop wants anyway). ---
            ("kernel32.dll", "CreateFiber") => {
                // (dwStackSize, lpStartAddress, lpParameter)
                let start = a1;
                let param = a2;
                if start < c.image_base || start >= c.image_base + c.image_size {
                    let n = MISSING.fetch_add(1, Ordering::Relaxed);
                    if n < 16 { crate::glog!("[gow.fiber] CreateFiber start 0x{start:x} outside the image -> NULL"); }
                    c.gpr.rax = 0;
                } else {
                    let rva = (start - c.image_base) as u32;
                    c.gpr.rax = crate::fibers::create(cpu, rva as u64, param);
                }
                true
            }
            ("kernel32.dll", "CreateFiberEx") => {
                // (reserve, commit, flags, lpStartAddress, lpParameter)
                let start = a3;
                let param = a4;
                let rva = if start >= c.image_base && start < c.image_base + c.image_size {
                    (start - c.image_base) as u32
                } else { 0 };
                c.gpr.rax = if rva != 0 { crate::fibers::create(cpu, rva as u64, param) } else { 0 };
                true
            }
            ("kernel32.dll", "DeleteFiber") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "SwitchToFiber") => {
                // A real cooperative handoff (D040): the calling host thread
                // blocks until the target fiber grants it the baton back. This is
                // the job scheduler's yield/resume path.
                crate::fibers::switch(a0);
                c.gpr.rax = 1;
                true
            }
            ("kernel32.dll", "ConvertThreadToFiber") | ("kernel32.dll", "ConvertThreadToFiberEx") => {
                // Give the calling thread its own fiber handle so other fibers can
                // switch back to it.
                c.gpr.rax = crate::fibers::convert_current();
                true
            }
            ("kernel32.dll", "ConvertFiberToThread") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetCurrentFiber") => {
                c.gpr.rax = crate::fibers::current_handle();
                true
            }
            ("kernel32.dll", "GetCurrentThread") | ("kernel32.dll", "GetCurrentProcess") => { c.gpr.rax = 0xffff_ffff_ffff_ffff; true }
            ("kernel32.dll", "DuplicateHandle") => {
                // (hSrcProcess, hSource, hTargetProcess, lpTarget, access,
                // inherit, options): the CRT/engine pattern duplicates a handle
                // within the same process. Our handles are table keys, so the
                // duplicate is the same value; DUPLICATE_CLOSE_SOURCE (0x1,
                // 7th argument at [rsp+0x38]) additionally closes the source.
                let a6 = if crate::dispatch::plausible_mem(arg(2), 8) { *(arg(2) as *const u64) } else { 0 };
                let ok = a1 != 0;
                if ok {
                    if okp(a3, 8) { *(a3 as *mut u64) = a1; }
                    if a6 & 1 != 0 { self.files.close(a1); }
                }
                c.gpr.rax = ok as u64;
                true
            }
            ("kernel32.dll", "SwitchToThread") | ("kernel32.dll", "YieldProcessor") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "VirtualProtect") => { if okp(a3, 4) { *(a3 as *mut u32) = 0x40; } c.gpr.rax = 1; true }
            ("kernel32.dll", "VirtualQuery") => {
                // Minimal MEMORY_BASIC_INFORMATION (48 bytes on x64), answered
                // from the tracked-heap table.
                if okp(a1, 48) {
                    let b = a1 as *mut u8;
                    std::ptr::write_bytes(b, 0, 48);
                    match heap_block_containing(a0) {
                        Some((base, size)) => {
                            let off = a0 - base as u64;
                            *(b as *mut u64) = base as u64;                  // BaseAddress
                            *((b as *mut u64).add(1)) = base as u64;         // AllocationBase
                            *((b as *mut u32).add(4)) = 0x04;                // AllocationProtect = PAGE_READWRITE
                            *((b as *mut u64).add(3)) = size as u64 - off;   // RegionSize
                            *((b as *mut u32).add(8)) = 0x1000;              // State = MEM_COMMIT
                            *((b as *mut u32).add(9)) = 0x04;                // Protect = PAGE_READWRITE
                            *((b as *mut u32).add(10)) = 0x20000;            // Type = MEM_PRIVATE
                        }
                        None => {
                            *((b as *mut u64).add(3)) = 0x1000;              // RegionSize
                            *((b as *mut u32).add(8)) = 0x10000;             // State = MEM_FREE
                            *((b as *mut u32).add(9)) = 0x01;                // Protect = PAGE_NOACCESS
                        }
                    }
                }
                c.gpr.rax = 48;
                true
            }
            ("kernel32.dll", "LoadLibraryA") | ("kernel32.dll", "LoadLibraryW")
            | ("kernel32.dll", "LoadLibraryExA") | ("kernel32.dll", "LoadLibraryExW") => {
                // A non-null pseudo-module for any DLL (D014): the game keeps
                // running and GetProcAddress can serve real thunks. The Ex
                // variants keep the name in RCX (flags in RDX are irrelevant).
                let name = read_cstr(a0, func.ends_with('W'));
                if name.is_empty() {
                    c.gpr.rax = 0;
                } else {
                    let b = dll_basename(&name);
                    if b.is_empty() {
                        // A path that names no module ("C:\", "\"): Windows
                        // fails the load; a "" pseudo-module only polluted the
                        // module table (device log: LoadLibrary("") -> handle).
                        c.gpr.rax = 0;
                    } else {
                        let h = module_handle_for(&b);
                        crate::glog!("[gow.module] LoadLibrary(\"{b}\") -> 0x{h:x}");
                        c.gpr.rax = h;
                    }
                }
                true
            }
            ("kernel32.dll", "FreeLibrary") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "SetEndOfFile") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "FlushFileBuffers") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "DeleteFileA") | ("kernel32.dll", "DeleteFileW") => {
                let name = read_cstr(a0, func.ends_with('W'));
                c.gpr.rax = if std::fs::remove_file(self.files.translate(&name)).is_ok() { 1 } else { 0 };
                true
            }
            ("kernel32.dll", "CreateDirectoryA") | ("kernel32.dll", "CreateDirectoryW") => {
                let name = read_cstr(a0, func.ends_with('W'));
                c.gpr.rax = if std::fs::create_dir_all(self.files.translate(&name)).is_ok() { 1 } else { 0 };
                true
            }
            ("kernel32.dll", "GetFileAttributesA") | ("kernel32.dll", "GetFileAttributesW") => {
                let name = read_cstr(a0, func.ends_with('W'));
                let p = self.files.translate(&name);
                c.gpr.rax = if p.is_dir() { 0x10 } else if p.exists() { 0x80 } else { 0xffff_ffff };
                true
            }
            ("kernel32.dll", "FindFirstFileA") | ("kernel32.dll", "FindFirstFileW") => {
                let name = read_cstr(a0, func.ends_with('W'));
                c.gpr.rax = self.files.find_first(&name);
                true
            }
            ("kernel32.dll", "FindNextFileA") | ("kernel32.dll", "FindNextFileW") => {
                c.gpr.rax = if a1 == 0 || okp(a1, 0x230) { self.files.find_next(a0, a1) as u64 } else { 0 };
                true
            }
            ("kernel32.dll", "FindClose") => { self.files.find_close(a0); c.gpr.rax = 1; true }
            ("kernel32.dll", "GetModuleFileNameA") | ("kernel32.dll", "GetModuleFileNameW") => {
                // (module, lpFilename, nSize): the buffer is RDX (the old code
                // wrote into the module handle in RCX).
                let s: String = match module_name_of(a0) {
                    Some(dll) => format!("C:\\Windows\\System32\\{dll}"),
                    None => self.files.translate("GoW.exe").to_string_lossy().into_owned(),
                };
                let width = if func.ends_with('W') { 2 } else { 1 };
                let bytes = s.as_bytes();
                let n = (a2 as usize / width).saturating_sub(1).min(bytes.len());
                if okp(a1, (n * width + width) as u64) {
                    for i in 0..n { *(a1 as *mut u8).add(i * width) = bytes[i]; }
                    *(a1 as *mut u8).add(n * width) = 0;
                }
                c.gpr.rax = n as u64;
                true
            }
            ("kernel32.dll", "GetCommandLineA") | ("kernel32.dll", "GetCommandLineW") => {
                c.gpr.rax = self.files.cmdline_ptr(func.ends_with('W')); true
            }
            ("kernel32.dll", "GetEnvironmentStringsA") | ("kernel32.dll", "GetEnvironmentStringsW") => {
                // A stable EMPTY environment block (double NUL). Returning 0
                // left CRT/env walkers operating on NULL.
                c.gpr.rax = empty_env_block(func.ends_with('W'));
                true
            }
            ("kernel32.dll", "FreeEnvironmentStringsA") | ("kernel32.dll", "FreeEnvironmentStringsW") => { c.gpr.rax = 1; true }
            ("kernel32.dll", "GetEnvironmentVariableA") | ("kernel32.dll", "GetEnvironmentVariableW") => {
                c.gpr.rax = 0; true
            }
            ("kernel32.dll", "SetUnhandledExceptionFilter") | ("kernel32.dll", "UnhandledExceptionFilter") => {
                c.gpr.rax = 1; true
            }
            ("kernel32.dll", "SetErrorMode") | ("kernel32.dll", "SetThreadPriority") => { c.gpr.rax = 0; true }
            ("kernel32.dll", "GetVersion") => { c.gpr.rax = 0x0a00_0000; true }
            ("xinput1_4.dll", f) | ("xinput1_3.dll", f) | ("xinput9_1_0.dll", f) => {
                let input = crate::input::global();
                match f {
                    "ord#2" | "XInputGetState" => {
                        // a0 = user index, a1 = XINPUT_STATE*
                        match input.xinput_get_state(a0 as u32) {
                            Some(st) => {
                                if okp(a1, 24) { st.write_to(a1 as *mut u8); c.gpr.rax = 0; }
                                else { c.gpr.rax = 0x48f; }
                            }
                            None => { c.gpr.rax = 0x48f; } // ERROR_DEVICE_NOT_CONNECTED
                        }
                        true
                    }
                    "ord#3" | "XInputSetState" => {
                        let (motors, right) = if okp(a1, 4) {
                            (*(a1 as *const u16), *((a1 as *const u16).add(1)))
                        } else { (0, 0) };
                        c.gpr.rax = input.xinput_set_state(a0 as u32, motors, right) as u64;
                        true
                    }
                    "XInputEnable" => { c.gpr.rax = 0; true }
                    "ord#1" | "XInputGetCapabilities" => { c.gpr.rax = 0x48f; true }
                    _ => { c.gpr.rax = 0x48f; true }
                }
            }
            ("steam_api64.dll", f) | ("steam_api.dll", f) => {
                // Steamworks stubs (Init succeeds, callbacks accepted). Real
                // behaviour is unnecessary: the game boots without Steam.
                c.gpr.rax = crate::steam::steam_api64(f).unwrap_or(0);
                true
            }
            (dll, _) if dll.starts_with("libsce") => {
                // Sony compatibility DLLs (libSceGnm/Pad/JobManager/GpuAddress):
                // return a non-null handle so callers don't deref null.
                c.gpr.rax = crate::steam::sce(func).unwrap_or(0);
                true
            }
            ("user32.dll", "GetAsyncKeyState") | ("user32.dll", "GetKeyState") => {
                let vk = (a0 & 0xff) as u16;
                c.gpr.rax = if crate::input::global().vk_down(vk) { 0x8000 } else { 0 };
                true
            }
            ("user32.dll", "GetCursorPos") => {
                let ms = crate::input::global().mouse_state(); let (x, y) = (ms.x, ms.y);
                if okp(a0, 8) { *(a0 as *mut i32) = x; *((a0 as *mut i32).add(1)) = y; }
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
                match func {
                    "XAudio2Create" => {
                        let vt = crate::audio::xaudio2_create_vtable(crate::audio::global());
                        if okp(a0, 8) { *(a0 as *mut u64) = vt; }
                        c.gpr.rax = if vt == 0 { 0x8000_4005 } else { 0 };
                        true
                    }
                    "CreateAudioReverb" | "CreateAudioVolumeMeter" => { c.gpr.rax = 0x8000_4005; true } // E_NOINTERFACE (honest)
                    _ => { c.gpr.rax = 0x8000_4005; true }
                }
            }
            (dll, _) if dll.starts_with("api-ms-win-crt-") || dll == "msvcp140.dll"
                || dll == "vcruntime140.dll" || dll == "ucrtbase.dll" => {
                if crate::crt::call(cpu, &func) { true } else {
                    let n = MISSING.fetch_add(1, Ordering::Relaxed);
                    if n < 200 { crate::glog!("[gow.crt] unimplemented: {dll}!{func}"); }
                    c.gpr.rax = 0;
                    true
                }
            }
            // --- VERSION.dll (D019). The engine probes its own/driver version
            // at boot; the old unimplemented answers handed it garbage and it
            // walked into the TerminateProcess error path (build #157). We
            // serve a minimal VS_FIXEDFILEINFO (1.0.0.0) plus the common
            // StringFileInfo entries, and log WHICH file was asked about. ---
            ("version.dll", "GetFileVersionInfoSizeA") | ("version.dll", "GetFileVersionInfoSizeW")
            | ("version.dll", "GetFileVersionInfoSizeExA") | ("version.dll", "GetFileVersionInfoSizeExW") => {
                // (lpstrFilename, lpdwHandle) — Ex forms put the filename in RDX.
                let path = if func.contains("Ex") { read_cstr(a1, func.ends_with('W')) } else { read_cstr(a0, func.ends_with('W')) };
                crate::glog!("[gow.version] GetFileVersionInfoSize(\"{path}\")");
                if okp(a1, 4) { *(a1 as *mut u32) = 0; }
                c.gpr.rax = VERSION_BLOB_SIZE as u64;
                true
            }
            ("version.dll", "GetFileVersionInfoA") | ("version.dll", "GetFileVersionInfoW")
            | ("version.dll", "GetFileVersionInfoExA") | ("version.dll", "GetFileVersionInfoExW") => {
                // (lpstrFilename, dwHandle, dwSize, lpData); Ex adds flags up
                // front: (flags, lpstrFilename, dwHandle, dwSize, lpData) so the
                // buffer is the 5th arg.
                let path = if func.ends_with("ExA") || func.ends_with("ExW") { read_cstr(a1, func.ends_with('W')) } else { read_cstr(a0, func.ends_with('W')) };
                let buf = if func.ends_with("ExA") || func.ends_with("ExW") { a4 } else { a3 };
                crate::glog!("[gow.version] GetFileVersionInfo(\"{path}\")");
                if okp(buf, VERSION_BLOB_SIZE as u64) {
                    write_version_blob(buf);
                    c.gpr.rax = 1;
                } else {
                    c.gpr.rax = 0;
                }
                true
            }
            ("version.dll", "VerQueryValueA") | ("version.dll", "VerQueryValueW") => {
                // (pBlock, lpSubBlock, &lplpBuffer, &puLen)
                let sub = read_cstr(a1, func.ends_with('W'));
                match ver_query(a0, &sub, func.ends_with('W')) {
                    Some((p, len)) => {
                        if okp(a2, 8) { *(a2 as *mut u64) = p; }
                        if okp(a3, 4) { *(a3 as *mut u32) = len; }
                        c.gpr.rax = 1;
                    }
                    None => { c.gpr.rax = 0; }
                }
                true
            }
            ("d3dcompiler_47.dll", "D3DReflect") => {
                // Parse the DXBC blob and hand back an ID3D11ShaderReflection.
                let blob = a0;
                let size = a1 as usize;
                let out = a3;
                let bytes = if blob != 0 { std::slice::from_raw_parts(blob as *const u8, size) } else { &[] };
                let desc = crate::dxbc::parse(bytes).unwrap_or_default();
                let obj = crate::dxbc::make_object(desc);
                if okp(out, 8) { *(out as *mut u64) = obj; }
                c.gpr.rax = 0;
                true
            }
            ("d3d11.dll", _) | ("dxgi.dll", _) | ("d3dcompiler_47.dll", _) => {
                // Native Vulkan backend (GOW_GFX_BACKEND=native): build the D3D11
                // COM objects ourselves instead of routing to DXVK. DXVK stays the
                // default and is untouched when the env var is unset.
                if crate::vk_native::bridge::is_enabled() && dll_l != "d3dcompiler_47.dll" {
                    c.gpr.rax = crate::vk_native::d3d11_objects::call_entry(&func, cpu);
                    return true;
                }
                // Route to the loaded graphics backend (DXVK Native). The COM
                // vtables it returns are what the game calls afterwards.
                match self.graphics.resolve(&dll_l, &func) {
                    Some(addr) => {
                        crate::glog!("[gow.gfx] call {dll}!{func}");
                        let r = crate::graphics::GraphicsBridge::call_addr_raw(addr, cpu);
                        crate::glog!("[gow.gfx] {func} -> 0x{r:x}");
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

    /// Non-blocking "is this object signalled right now" used by
    /// WaitForMultipleObjects: thread handles report their done flag, sync
    /// objects their state (D019).
    fn obj_signalled(&self, h: u64) -> bool {
        if self.threads.is_thread(h) {
            self.threads.poll_done(h)
        } else {
            self.sync.poll(h)
        }
    }

    /// GetProcAddress(hModule, lpProcName): serve graphics entry points from
    /// the loaded DXVK Native backend and everything else through the import
    /// resolver, both behind hostcall thunks that marshal the MS x64 arguments
    /// (RCX/RDX/R8/R9 + [rsp+0x28]...). This is what lets a game that resolves
    /// `D3D11CreateDevice` dynamically actually call it instead of storing NULL
    /// and jumping to it (the `[gow.MISSCALL] target_va=0x0` storm in the
    /// device logs).
    unsafe fn get_proc_address(&self, cpu: *mut GowCpu, module: u64, name_ptr: u64) -> u64 {
        let image_base = (*cpu).image_base;
        if name_ptr == 0 { return 0; }
        let func = if name_ptr < 0x10000 {
            // MAKEINTRESOURCEA ordinal.
            format!("ord#{}", name_ptr & 0xffff)
        } else {
            read_cstr(name_ptr, false)
        };
        if func.is_empty() { return 0; }
        let dll = if module == 0 || module == image_base {
            None
        } else {
            module_name_of(module)
        };
        let Some(dll) = dll else {
            let n = MISSING.fetch_add(1, Ordering::Relaxed);
            if n < 64 {
                crate::glog!("[gow.proc] GetProcAddress(exe, \"{func}\") -> 0 (game exports by name are not served)");
            }
            return 0;
        };
        let key = (dll.clone(), func.clone());
        if let Some(&a) = proc_thunks().lock().unwrap().get(&key) { return a; }

        let is_gfx = dll.starts_with("d3d11") || dll.starts_with("dxgi") || dll.starts_with("d3dcompiler");
        if is_gfx {
            // Native Vulkan backend: serve D3D11/DXGI entry points ourselves.
            if crate::vk_native::bridge::is_enabled() && !dll.starts_with("d3dcompiler") {
                let func_c = func.clone();
                let thunk = crate::hostcall::register(move |c| {
                    let r = unsafe {
                        crate::vk_native::d3d11_objects::call_entry(&func_c, c)
                    };
                    unsafe { (*c).gpr.rax = r; }
                });
                crate::glog!("[gow.vk.native] GetProcAddress({dll}!{func}) -> thunk 0x{thunk:x}");
                proc_thunks().lock().unwrap().insert(key, thunk);
                return thunk;
            }
            // DXVK Native export, called through a marshalling thunk.
            if let Some(addr) = self.graphics.resolve(&dll, &func) {
                let thunk = crate::hostcall::register(move |c| {
                    let r = unsafe { crate::graphics::GraphicsBridge::call_addr_raw(addr, c) };
                    unsafe { (*c).gpr.rax = r; }
                });
                crate::glog!("[gow.gfx] GetProcAddress({dll}!{func}) -> thunk 0x{thunk:x} (dxvk 0x{addr:x})");
                proc_thunks().lock().unwrap().insert(key, thunk);
                return thunk;
            }
            let n = MISSING.fetch_add(1, Ordering::Relaxed);
            if n < 64 { crate::glog!("[gow.gfx] GetProcAddress({dll}!{func}) -> 0 (no such export in the backend)"); }
            return 0;
        }
        // Any other DLL: dispatch by (dll, func) exactly like an IAT import.
        let dll_c = dll.clone();
        let func_c = func.clone();
        let thunk = crate::hostcall::register(move |c| {
            let handled = crate::dispatch::import_resolver()
                .map(|r| unsafe { r.call_named(c, &dll_c, &func_c) })
                .unwrap_or(false);
            if !handled { unsafe { (*c).gpr.rax = 0; } }
        });
        crate::glog!("[gow.proc] GetProcAddress({dll}!{func}) -> thunk 0x{thunk:x}");
        proc_thunks().lock().unwrap().insert(key, thunk);
        thunk
    }
}

impl Default for ImportResolver {
    fn default() -> Self { Self::new_host() }
}

/// Plausibility guard for guest out-pointers: same policy as the dispatcher's
/// memory validator (reject the null page, wrap-around and the kernel range).
/// Every handler that writes through a guest pointer must check this first —
/// the degraded engine state passed pointers like 0x80 and killed the host
/// (SEGV at 0x80, build #155).
fn okp(p: u64, len: u64) -> bool { crate::dispatch::plausible_mem(p, len) }

/// (total, available) physical RAM in bytes, from /proc/meminfo.
fn host_meminfo() -> (u64, u64) {
    let (mut total, mut avail) = (0u64, 0u64);
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            let kb = |l: &str| -> u64 {
                l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024
            };
            if line.starts_with("MemTotal:") { total = kb(line); }
            else if line.starts_with("MemAvailable:") { avail = kb(line); }
        }
    }
    if total == 0 { total = 4 << 30; avail = 2 << 30; }
    (total, avail)
}

/// Free and total bytes of the filesystem backing `dir` (statvfs), so the
/// engine's disk-space gate passes. Falls back to a large constant if the
/// syscall fails (e.g. an unreadable path).
fn host_disk_space(dir: &std::path::Path) -> (u64, u64) {
    // libc::statvfs is not in the crate's deps; call it via a tiny decl.
    #[repr(C)]
    struct Statvfs {
        f_bsize: u64, f_frsize: u64, f_blocks: u64, f_bfree: u64, f_bavail: u64,
        f_files: u64, f_ffree: u64, f_favail: u64, f_fsid: u64, f_flag: u64,
        f_namemax: u64, __spare: [u32; 6],
    }
    extern "C" { fn statvfs(path: *const std::os::raw::c_char, buf: *mut Statvfs) -> i32; }
    let c = std::ffi::CString::new(dir.to_string_lossy().as_bytes()).unwrap_or_default();
    let mut st: Statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { statvfs(c.as_ptr(), &mut st) };
    if rc == 0 && st.f_blocks > 0 {
        let bs = if st.f_frsize != 0 { st.f_frsize } else { st.f_bsize };
        let total = st.f_blocks.saturating_mul(bs);
        let free = st.f_bavail.saturating_mul(bs);
        (free, total)
    } else {
        (32u64 << 30, 64u64 << 30)
    }
}

// ---------------------------------------------------------------------------
// VERSION.dll: a minimal fake version resource (D019)
// ---------------------------------------------------------------------------

/// Size of the fake version blob handed out by GetFileVersionInfo.
const VERSION_BLOB_SIZE: usize = 0x1000;

/// StringFileInfo entries served by VerQueryValue, in the order they are laid
/// out in the blob (narrow copies from 0x40, wide copies from 0x400).
const VERSION_STRINGS: [(&str, &str); 7] = [
    ("CompanyName", "Sony Interactive Entertainment"),
    ("FileDescription", "God of War"),
    ("FileVersion", "1.0.0.0"),
    ("InternalName", "GoW"),
    ("OriginalFilename", "GoW.exe"),
    ("ProductName", "God of War"),
    ("ProductVersion", "1.0.0.0"),
];

/// Fill `buf` with: VS_FIXEDFILEINFO at +0 (52 bytes), the translation pair at
/// +0x38, narrow strings from +0x40 (0x40 bytes each) and wide strings from
/// +0x400 (0x100 bytes each).
fn write_version_blob(buf: u64) {
    unsafe {
        std::ptr::write_bytes(buf as *mut u8, 0, VERSION_BLOB_SIZE);
        let p = buf as *mut u32;
        *p.add(0) = 0xFEEF_04BD;        // dwSignature
        *p.add(1) = 0x0001_0000;        // dwStrucVersion (1.0)
        *p.add(2) = 0x0001_0000;        // dwFileVersionMS (1.0)
        *p.add(3) = 0x0000_0000;        // dwFileVersionLS
        *p.add(4) = 0x0001_0000;        // dwProductVersionMS (1.0)
        *p.add(5) = 0x0000_0000;        // dwProductVersionLS
        *p.add(6) = 0x0000_003F;        // dwFileFlagsMask
        *p.add(7) = 0x0000_0000;        // dwFileFlags
        *p.add(8) = 0x0004_0004;        // dwFileOS (VOS_NT_WINDOWS64)
        *p.add(9) = 0x0000_0001;        // dwFileType (VFT_APP)
        *p.add(10) = 0;                 // dwFileSubtype
        // dwFileDateMS/LS stay 0.
        let tr = (buf + 0x38) as *mut u16;
        *tr.add(0) = 0x0409;            // US English
        *tr.add(1) = 0x04B0;            // Unicode
        for (i, (_k, v)) in VERSION_STRINGS.iter().enumerate() {
            let na = (buf + 0x40 + i as u64 * 0x40) as *mut u8;
            for (j, b) in v.bytes().enumerate() {
                if j >= 0x3F { break; }
                *na.add(j) = b;
            }
            let wa = (buf + 0x400 + i as u64 * 0x100) as *mut u16;
            for (j, ch) in v.encode_utf16().enumerate() {
                if j >= 0x7F { break; }
                *wa.add(j) = ch;
            }
        }
    }
}

/// Answer a VerQueryValue sub-block from a blob written by
/// `write_version_blob`. `"\\"` -> the fixed info; `"\VarFileInfo\Translation"`
/// -> the langid pair; `"\StringFileInfo\<lang>\<key>"` -> the string (narrow
/// for VerQueryValueA, wide for VerQueryValueW). Returns (pointer, len).
fn ver_query(block: u64, sub: &str, wide: bool) -> Option<(u64, u32)> {
    let s = sub.trim_start_matches('\\');
    if s.is_empty() {
        return Some((block, 52));
    }
    if s.eq_ignore_ascii_case("VarFileInfo\\Translation") {
        return Some((block + 0x38, 4));
    }
    let lower = s.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("stringfileinfo\\") {
        // rest is "<lang>\\<key>"; skip the lang id.
        if let Some(pos) = rest.find('\\') {
            let key = &rest[pos + 1..];
            for (i, (k, v)) in VERSION_STRINGS.iter().enumerate() {
                if k.eq_ignore_ascii_case(key) {
                    if wide {
                        let n = v.encode_utf16().count() as u32;
                        return Some((block + 0x400 + i as u64 * 0x100, n * 2));
                    } else {
                        return Some((block + 0x40 + i as u64 * 0x40, v.len() as u32));
                    }
                }
            }
        }
    }
    None
}

/// Leaked, process-stable EMPTY environment blocks (narrow + wide): just the
/// double NUL terminator, as GetEnvironmentStrings must return.
fn empty_env_block(wide: bool) -> u64 {
    static NARROW: OnceLock<u64> = OnceLock::new();
    static WIDE: OnceLock<u64> = OnceLock::new();
    if wide {
        *WIDE.get_or_init(|| {
            let b: Vec<u8> = vec![0u8; 4]; // L"\0\0"
            let p = b.as_ptr() as u64;
            std::mem::forget(b);
            p
        })
    } else {
        *NARROW.get_or_init(|| {
            let b: Vec<u8> = vec![0u8; 2]; // "\0\0"
            let p = b.as_ptr() as u64;
            std::mem::forget(b);
            p
        })
    }
}

unsafe fn read_cstr(ptr: u64, wide: bool) -> String {
    // Never dereference a wild pointer here: after a failed boot allocation
    // the game passes garbage module/path pointers (device log: fatal SIGSEGV
    // at 0x80 inside the runtime with "KERNEL32.dll" still warm in x6/x7).
    // Reject the null page and the kernel/-1 range — the same policy as
    // `plausible_mem` — and check EVERY byte read, so even a
    // plausible-but-unmapped pointer cannot crash the host.
    if ptr < 0x1_0000 || (ptr >> 48) == 0xffff { return String::new(); }
    if wide {
        let mut v = Vec::new();
        let mut p = ptr as *const u16;
        loop {
            if !okp(p as u64, 2) { break; }
            let ch = *p; if ch == 0 { break; } v.push(ch); p = p.add(1); if v.len() > 4096 { break; }
        }
        String::from_utf16_lossy(&v)
    } else {
        let mut v = Vec::new();
        let mut p = ptr as *const u8;
        loop {
            if !okp(p as u64, 1) { break; }
            let ch = *p; if ch == 0 { break; } v.push(ch); p = p.add(1); if v.len() > 4096 { break; }
        }
        String::from_utf8_lossy(&v).into_owned()
    }
}

// ---------------------------------------------------------------------------
// Tracked heap: real alloc/free with a size table (D013) + lazy mmap for big
// reservations (D017)
// ---------------------------------------------------------------------------
//
// The first Win32 layer used a "bump" allocator that rounded every allocation
// up to 4 KiB and never freed (`host_free` was a no-op). A game the size of
// GoW does millions of small allocations during boot and loading; the process
// ballooned until Android's LMK killed it — the "eats all RAM and closes"
// symptom from the device logs. This table trades one hash lookup per
// alloc/free for correct lifetimes and a hard ceiling.
//
// D017: the ceiling itself caused the next crash. The engine reserves one
// multi-GiB arena at boot (a single HeapAlloc/_aligned_malloc/VirtualAlloc of
// >= 3072 MiB — seen in the log as "budget exceeded: 0 MiB live" with an
// otherwise empty heap). Refusing it left the game running from a NULL arena,
// which surfaced as the [gow.badstore]/[gow.badload] cascade of negative wild
// addresses and the final SIGSEGV at 0x80 while reading a module name. Blocks
// of MMAP_MIN or more are now served by mmap(MAP_NORESERVE): the reservation
// only occupies address space, zeroed pages arrive on first touch
// (Windows-like lazy commit), RSS grows with what the game actually uses, and
// a separate, higher reservation cap applies.
//
// D018: in-arena MEM_COMMIT requests (VirtualAlloc with a non-null
// lpAddress inside a tracked reservation) return the requested base and no
// longer consume the reservation cap — the engine's own sub-allocations are
// free, exactly like on Windows.

#[derive(Clone, Copy)]
struct Block { size: usize, alloc: usize, align: usize, mmap: bool }

/// Guard bytes placed before and after each committed (malloc-backed) block.
/// The guest's Win32 code freely writes a few bytes before/after a buffer (MSVC
/// `_aligned_malloc` stores its base pointer at `p-8`; several engine routines
/// round-trip small header structs just outside the allocation). Without slack
/// that lands in the native allocator's (scudo) metadata header and the later
/// `free` aborts the whole process with `reportHeaderCorruption`. The redzone
/// absorbs those accesses.
const GUARD: usize = 64;
fn guard_head(align: usize) -> usize { if align > GUARD { align } else { GUARD } }

#[cfg(unix)]
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn mincore(addr: *mut u8, len: usize, vec: *mut u8) -> i32;
}
#[cfg(unix)]
mod sys {
    pub const PROT_READ: i32 = 1;
    pub const PROT_WRITE: i32 = 2;
    pub const MAP_PRIVATE: i32 = 0x02;
    pub const MAP_ANONYMOUS: i32 = 0x20;
    pub const MAP_NORESERVE: i32 = 0x4000;
    pub const MAP_FIXED: i32 = 0x10;
    pub const MAP_FAILED: isize = -1;
}

/// Pointer hasher that skips SipHash (alloc/free are too hot for it).
#[derive(Default)]
struct PtrHasher(u64);
impl Hasher for PtrHasher {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 5).wrapping_sub(self.0).wrapping_add(b as u64);
        }
    }
}
type BlockMap = HashMap<usize, Block, BuildHasherDefault<PtrHasher>>;

fn alloc_table() -> &'static Mutex<BlockMap> {
    static T: OnceLock<Mutex<BlockMap>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::with_hasher(BuildHasherDefault::default())))
}

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
/// Bytes reserved through lazy mmap blocks (address space, not committed).
static RESERVED_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Last 128 MiB band of live bytes that was reported in the log.
static HEAP_BAND: AtomicUsize = AtomicUsize::new(0);
/// Last 128 MiB band of reserved bytes that was reported in the log.
static RESERVED_BAND: AtomicUsize = AtomicUsize::new(0);

/// Blocks of at least this size are mmap'd (lazy zero pages) instead of
/// committed through the allocator (D017).
const MMAP_MIN: usize = 1024 * 1024;
/// Total guest-heap budget. Past the ceiling allocations return NULL (the game
/// sees an OOM and takes its own error path) instead of the kernel killing us.
/// Override with `GOW_HEAP_MB` (default 3072).
fn heap_budget() -> usize {
    static B: OnceLock<usize> = OnceLock::new();
    *B.get_or_init(|| {
        std::env::var("GOW_HEAP_MB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(3072) * 1024 * 1024
    })
}

/// Cap on *reserved* (mmap'd, lazily committed) bytes — intentionally much
/// higher than the committed budget: reserved pages only become RSS when the
/// game touches them, and the engine may reserve an 8-16 GiB VA arena on a
/// device with far less RAM. Build #155: the engine's own arena hit the old
/// 8192 MiB cap and the next in-arena commit fell back to the committed heap
/// (D018 makes in-arena commits free, and this cap now only counts genuinely
/// new reservations). Override with `GOW_RESERVE_MB` (default 32768).
fn reserve_budget() -> usize {
    static B: OnceLock<usize> = OnceLock::new();
    *B.get_or_init(|| {
        std::env::var("GOW_RESERVE_MB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(65536) * 1024 * 1024
    })
}

/// (live bytes, live blocks, peak bytes) — diagnostics for tests/sampler.
pub fn heap_stats() -> (usize, usize, usize) {
    (LIVE_BYTES.load(Ordering::Relaxed), LIVE_BLOCKS.load(Ordering::Relaxed), PEAK_BYTES.load(Ordering::Relaxed))
}

/// Reserved (mmap'd, lazy) bytes — diagnostics for the sampler.
pub fn heap_reserved() -> usize { RESERVED_BYTES.load(Ordering::Relaxed) }

const PAGE: usize = 0x1000;

fn heap_alloc_raw(size: usize, align: usize, page: bool) -> *mut u8 {
    let trace = std::env::var_os("GOW_MEMTRACE").is_some();
    if size >= (1024 * 1024) && trace {
        crate::glog!("[gow.heap] alloc {} KiB align={align} page={page} rva=0x{:x}", size / 1024, crate::crashlog::current_rva());
    }
    if trace && size >= 0x100000 {
        // nothing more; the return is logged at the end
    }
    // Diagnostic: dump every tracked block above 1 MiB that could hold the
    // engine's free-list base, so a badstore right outside a block is traceable.
    if trace && size >= (16 * 1024 * 1024) {
        let t = alloc_table().lock().unwrap();
        let list: Vec<String> = t.iter().filter(|(_, b)| b.size >= 1024 * 1024)
            .map(|(a, b)| format!("0x{a:x}+0x{:x}", b.size)).collect();
        crate::glog!("[gow.heap] blocks>=1MiB: {}", list.join(", "));
    }
    let (size, align) = if page {
        ((((size.saturating_add(PAGE - 1)) / PAGE) * PAGE).max(PAGE), PAGE)
    } else {
        ((((size.saturating_add(0xf)) / 0x10) * 0x10).max(0x10), align.max(0x10))
    };
    // D017: a big request is the engine's arena, not a leak. Reserve address
    // space with lazy zero pages; fall through to the committed path only if
    // mmap is unavailable (non-unix) or the reservation cap refuses it.
    if size >= MMAP_MIN && align <= PAGE {
        let p = mmap_reserve(size);
        if !p.is_null() {
            if std::env::var_os("GOW_MEMTRACE").is_some() {
                crate::glog!("[gow.heap] mmap {}-MiB -> {p:p}", size / (1024*1024));
            }
            return p;
        }
    }
    if size >= (8 * 1024 * 1024) && std::env::var_os("GOW_MEMTRACE").is_some() {
        crate::glog!("[gow.heap] large alloc {} KiB served committed (align={align})", size / 1024);
    }
    if size >= MMAP_MIN {
        // A 32 MiB+ request that MAPPED_FIXED failed (mmap unavailable / cap) —
        // log loudly: such a block is the engine's free-list arena, and the
        // committed heap may not give it a single contiguous run.
        crate::glog!("[gow.heap] WARNING: {}-MiB request served committed (mmap unavailable, align={align})", size / (1024*1024));
    }
    let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
    if live > heap_budget() {
        LIVE_BYTES.fetch_sub(size, Ordering::Relaxed);
        let n = REFUSED.fetch_add(1, Ordering::Relaxed);
        if n < 8 || n % 4096 == 0 {
            crate::glog!(
                "[gow.heap] budget exceeded: {} MiB requested, {} MiB live (cap {} MiB); allocation returns NULL",
                size / (1024 * 1024),
                LIVE_BYTES.load(Ordering::Relaxed) / (1024 * 1024),
                heap_budget() / (1024 * 1024)
            );
        }
        return std::ptr::null_mut();
    }
    let layout = match std::alloc::Layout::from_size_align(size + GUARD + guard_head(align), align) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    // Zeroed on purpose: the game's error paths are full of uninitialized-read
    // assumptions that pass on Windows' fresh pages.
    let raw = unsafe { std::alloc::alloc_zeroed(layout) };
    if raw.is_null() {
        let n = REFUSED.fetch_add(1, Ordering::Relaxed);
        if n < 64 {
            crate::glog!("[gow.heap] alloc_zeroed RETURNED NULL size={} KiB align={align} rva=0x{:x}",
                size / 1024, crate::crashlog::current_rva());
        }
        return raw;
    }
    // Hand the guest a pointer GUARD bytes into the block, leaving guard bytes
    // free on both sides so small over/under-runs do not corrupt the native
    // allocator's metadata (the scudo reportHeaderCorruption abort).
    let p = unsafe { raw.add(guard_head(align)) };
    alloc_table().lock().unwrap().insert(p as usize, Block { size, alloc: raw as usize, align, mmap: false });
    if size >= (1024 * 1024) {
        register_big_range(p as u64, p as u64 + size as u64);
    }
    LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
    // Report heap growth in 128 MiB bands: a leak shows up in the log without a
    // debugger and without flooding (one line per band crossed).
    let band = live / (128 * 1024 * 1024);
    let last = HEAP_BAND.fetch_max(band, Ordering::Relaxed);
    if band > 0 && band > last {
        crate::glog!(
            "[gow.heap] live={}MiB blocks={} peak={}MiB reserved={}MiB",
            live / (1024 * 1024),
            LIVE_BLOCKS.load(Ordering::Relaxed),
            PEAK_BYTES.load(Ordering::Relaxed) / (1024 * 1024),
            RESERVED_BYTES.load(Ordering::Relaxed) / (1024 * 1024)
        );
    }
    if size >= (1024*1024) && trace { crate::glog!("[gow.heap] alloc -> {p:p}"); }
    p
}

/// Serve a big block from mmap(MAP_NORESERVE): address space is reserved now,
/// zeroed pages materialise on first touch, and only touched pages count
/// towards RSS. Returns null when the reservation cap refuses or mmap fails
/// (the caller then falls back to the committed heap, D018).
#[cfg(unix)]
fn mmap_reserve(size: usize) -> *mut u8 {
    // Reserve with generous slack past the requested size (see comment) AND
    // 64 KiB alignment: Windows VirtualAlloc always returns an address aligned
    // to dwAllocationGranularity (64 KiB), and the engine's memory manager does
    // `base + n*0x10000` chunk arithmetic that assumes it.
    // A slack margin past the requested size. The engine's VRAM pool walks its
    // free-list a few GiB past the block it was handed; any page there is backed
    // lazily by `try_back_page`, so the reserve itself stays small and does not
    // exhaust the address-space budget across many reserves.
    const RESERVE_SLACK: usize = 64 * 1024 * 1024;
    const GRANULARITY: usize = 0x10000;
    let map_size = ((size + RESERVE_SLACK + GRANULARITY - 1) / GRANULARITY) * GRANULARITY;
    let rs = RESERVED_BYTES.fetch_add(map_size, Ordering::Relaxed) + map_size;
    if rs > reserve_budget() {
        RESERVED_BYTES.fetch_sub(map_size, Ordering::Relaxed);
        let n = REFUSED.fetch_add(1, Ordering::Relaxed);
        if n < 8 || n % 4096 == 0 {
            crate::glog!(
                "[gow.heap] reservation refused: {} MiB requested ({} MiB reserved, cap {} MiB); falling back to committed heap",
                size / (1024 * 1024),
                RESERVED_BYTES.load(Ordering::Relaxed) / (1024 * 1024),
                reserve_budget() / (1024 * 1024)
            );
        }
        return std::ptr::null_mut();
    }
    // Over-map by GRANULARITY so we can align the base up to 64 KiB.
    let raw = unsafe {
        mmap(std::ptr::null_mut(), map_size + GRANULARITY, sys::PROT_READ | sys::PROT_WRITE,
             sys::MAP_PRIVATE | sys::MAP_ANONYMOUS | sys::MAP_NORESERVE, -1, 0)
    };
    if raw as isize == sys::MAP_FAILED {
        RESERVED_BYTES.fetch_sub(map_size, Ordering::Relaxed);
        let n = REFUSED.fetch_add(1, Ordering::Relaxed);
        if n < 8 {
            crate::glog!("[gow.heap] mmap of {} MiB failed; allocation returns NULL", size / (1024 * 1024));
        }
        return std::ptr::null_mut();
    }
    // Align up to 64 KiB; unmap the unaligned head/tail so no VA is wasted.
    let aligned = (raw as u64 + (GRANULARITY as u64 - 1)) & !(GRANULARITY as u64 - 1);
    let head = aligned - raw as u64;
    if head > 0 {
        unsafe { munmap(raw, head as usize); }
    }
    let p = aligned as *mut u8;
    alloc_table().lock().unwrap().insert(p as usize, Block { size: map_size, alloc: p as usize, align: PAGE, mmap: true });
    // Publish the WHOLE mapped range (including slack) so the validator accepts
    // the pool's slightly-past-the-end accesses.
    register_big_range(p as u64, p as u64 + map_size as u64);
    let band = rs / (128 * 1024 * 1024);
    if std::env::var_os("GOW_MEMTRACE").is_some() {
        crate::glog!("[gow.heap] mmap_reserve req={}MiB mapped={}MiB -> 0x{:x}..0x{:x}",
            size / (1024*1024), map_size / (1024*1024), p as u64, p as u64 + map_size as u64);
    }
    let last = RESERVED_BAND.fetch_max(band, Ordering::Relaxed);
    if band > 0 && band > last {
        crate::glog!("[gow.heap] reserved={}MiB (mmap, zero pages on touch)", rs / (1024 * 1024));
    }
    p
}

#[cfg(not(unix))]
fn mmap_reserve(_size: usize) -> *mut u8 { std::ptr::null_mut() }

fn heap_free_raw(p: *mut u8) -> bool {
    if p.is_null() { return false; }
    let blk = alloc_table().lock().unwrap().remove(&(p as usize));
    match blk {
        Some(b) => {
            if b.mmap {
                // Reserved block: munmap returns the address space (D017).
                #[cfg(unix)]
                unsafe { munmap(p, b.size); }
                RESERVED_BYTES.fetch_sub(b.size, Ordering::Relaxed);
            } else {
                let raw = b.alloc as *mut u8;
                if let Ok(layout) = std::alloc::Layout::from_size_align(b.size + GUARD + guard_head(b.align), b.align) {
                    unsafe { std::alloc::dealloc(raw, layout) };
                }
                LIVE_BYTES.fetch_sub(b.size, Ordering::Relaxed);
                LIVE_BLOCKS.fetch_sub(1, Ordering::Relaxed);
            }
            true
        }
        None => false, // double free / foreign pointer: ignore (callers log)
    }
}

fn heap_size_raw(p: *mut u8) -> usize {
    if p.is_null() { return 0; }
    alloc_table().lock().unwrap().get(&(p as usize)).map(|b| b.size).unwrap_or(0)
}

/// Find the tracked block containing `addr` (for VirtualQuery and the
/// VirtualAlloc in-arena commit path, D018).
fn heap_block_containing(addr: u64) -> Option<(usize, usize)> {
    let t = alloc_table().lock().unwrap();
    let a = addr as usize;
    t.iter()
        .find(|(&base, b)| base <= a && a < base + b.size)
        .map(|(&base, b)| (base, b.size))
}

/// True if [addr, addr+len) fits inside a block the runtime allocated (arena,
/// VirtualAlloc, TLS/heap block). Those are mapped by construction; the kernel's
/// view (/proc/self/maps) on Android/MTE splits them with PROT_NONE guard granule
/// pages, so it cannot be relied on for them.
pub fn addr_in_tracked_block(addr: u64, len: u64) -> bool {
    let a = addr & 0x00ff_ffff_ffff_ffff;
    let b = a.wrapping_add(len.max(1));
    // Lock-free fast path: the big reserved ranges (arena, 200 MiB blocks, …).
    // These are added on mmap and never removed, so a validator running while
    // another thread holds the alloc-table lock (or mid-malloc) still sees them.
    if BIG_RANGES.iter().any(|(lo, hi)| {
        a >= lo && b <= hi
    }) {
        return true;
    }
    let t = match alloc_table().try_lock() {
        Ok(t) => t,
        Err(_) => return false, // in use (e.g. mid-malloc); fall back to maps
    };
    let hit = t.iter().any(|(&base, blk)| {
        let base = (base & 0x00ff_ffff_ffff_ffff) as u64;
        let end = base.wrapping_add(blk.size as u64);
        a >= base && b <= end
    });
    if !hit && std::env::var_os("GOW_MEMTRACE_MISS") .is_some() {
        crate::glog!("[gow.memtrace] addr_in_tracked_block MISS addr=0x{addr:x} real=0x{a:x} blocks={}", t.len());
    }
    hit
}

/// Append-only list of large reserved ranges, published atomically so a
/// validator never has to take the alloc-table lock. Fixed capacity (a handful
/// of GiB-scale regions per run).
static BIG_RANGES: BigRanges = BigRanges::new();

struct BigRanges {
    slots: [std::sync::atomic::AtomicU64; 64],
}
impl BigRanges {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        BigRanges { slots: [Z; 64] }
    }
    fn push(&self, lo: u64, hi: u64) {
        use std::sync::atomic::Ordering;
        // Find the first free pair (lo,hi): lo initially 0.
        for i in (0..self.slots.len() - 1).step_by(2) {
            if self.slots[i].load(Ordering::Relaxed) == 0 {
                if self.slots[i]
                    .compare_exchange(0, lo, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    self.slots[i + 1].store(hi, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
    fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        use std::sync::atomic::Ordering;
        (0..self.slots.len() - 1).step_by(2).filter_map(move |i| {
            let lo = self.slots[i].load(Ordering::Relaxed);
            if lo == 0 { None } else { Some((lo, self.slots[i + 1].load(Ordering::Relaxed))) }
        })
    }
}

/// Publish a large range so `addr_in_tracked_block` accepts it lock-free.
pub fn register_big_range(lo: u64, hi: u64) {
    BIG_RANGES.push(lo & 0x00ff_ffff_ffff_ffff, hi & 0x00ff_ffff_ffff_ffff);
}

/// Dump the registered big ranges (diagnostics: called from a failed store).
pub fn log_big_ranges() {
    let list: Vec<String> = BIG_RANGES.iter().map(|(lo, hi)| format!("0x{lo:x}-0x{hi:x}")).collect();
    crate::glog!("[gow.badstore] big_ranges({}): {}", list.len(), list.join(", "));
}

/// Pages we backed lazily OUTSIDE a reserved range (the engine's pool walks a
/// free-list a little past the block). Remembering them is essential: without
/// it, `plausible_mem` would back the same page on every access, and each
/// `MAP_FIXED|MAP_ANONYMOUS` remap ZEROES it — the engine would read 0, write,
/// read 0 forever (1.1M mmap/s observed in f_40a610).
fn backed_pages() -> &'static Mutex<std::collections::HashSet<u64>> {
    static T: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// True when the kernel already has a mapping at this (untagged, page-aligned)
/// address. `try_back_page` must never `MAP_FIXED` over such a page: the guest
/// TEB, guest stacks and per-module TLS blocks are ordinary host allocations
/// that can fall inside MARGIN of one of our big reserves, and remapping them
/// zeroes live guest state. That was the non-deterministic boot failure where
/// `gs:[0x58]` suddenly read 0 immediately after a 264 MiB heap reserve landed
/// near the malloc arena (0xb400_0077...): the TEB page was silently replaced
/// with a fresh zero page.
#[cfg(unix)]
fn page_already_mapped(page: u64) -> bool {
    let mut vec = [0u8; 1];
    unsafe { mincore(page as *mut u8, 0x1000, vec.as_mut_ptr()) == 0 }
}
#[cfg(not(unix))]
fn page_already_mapped(_page: u64) -> bool { false }

/// Lazily map the 4 KiB page holding `addr` if it is plausibly part of a range we
/// reserved (or a modest margin past one). Windows keeps a VirtualAlloc reserve
/// accessible to commit; the engine's memory manager walks a free-list that can
/// step a few MiB past the block it was handed. Backing the page here lets that
/// write land instead of aborting the boot. Returns true if the page is now
/// mapped and writable.
pub fn try_back_page(addr: u64) -> bool {
    const PAGE: u64 = 0x1000;
    const MARGIN: u64 = 8 * 1024 * 1024 * 1024; // plausible VA around a range
    let a = addr & 0x00ff_ffff_ffff_ffff;
    let page = a & !(PAGE - 1);
    // Never touch the guest image (that is real game data/code).
    if let Some((lo, hi)) = crate::crashlog::image_range() {
        if page >= lo && page < hi {
            return false;
        }
    }
    let mut near = false;
    for (lo, hi) in BIG_RANGES.iter() {
        if page >= lo && page < hi {
            // Already mapped by us (the original mmap covers the whole span):
            // nothing to do, and never remap (MAP_FIXED|ANON would zero it).
            return true;
        }
        if page >= lo.saturating_sub(MARGIN) && page < hi.saturating_add(MARGIN) {
            near = true;
        }
    }
    if !near {
        return false;
    }
    // Already backed once: it is mapped and holds the guest's data. Return true
    // without remapping (re-mapping would zero it and spin the engine).
    {
        let set = backed_pages().lock().unwrap_or_else(|e| e.into_inner());
        if set.contains(&page) {
            return true;
        }
    }
    // A page the kernel already maps (TEB, guest stack, TLS block, or any other
    // live host allocation inside MARGIN) must be left alone: MAP_FIXED|ANON
    // would replace it with zeros. Remember it as usable and return.
    if page_already_mapped(page) {
        backed_pages().lock().unwrap_or_else(|e| e.into_inner()).insert(page);
        return true;
    }
    #[cfg(unix)]
    unsafe {
        let r = mmap(page as *mut u8, PAGE as usize, sys::PROT_READ | sys::PROT_WRITE,
                     sys::MAP_PRIVATE | sys::MAP_ANONYMOUS | sys::MAP_FIXED | sys::MAP_NORESERVE, -1, 0);
        if r as isize != sys::MAP_FAILED {
            backed_pages().lock().unwrap_or_else(|e| e.into_inner()).insert(page);
            if std::env::var_os("GOW_MEMTRACE").is_some() {
                crate::glog!("[gow.back_page] OUTSIDE page=0x{page:x} rva=0x{:x}",
                    crate::crashlog::current_rva());
            }
            return true;
        }
    }
    false
}

/// Public allocation helpers used by the CRT shims.
pub fn host_alloc(size: usize) -> *mut u8 { heap_alloc_raw(size.max(1), 0x10, false) }
pub fn host_alloc_aligned(size: usize, align: usize) -> *mut u8 { heap_alloc_raw(size.max(1), align, false) }
/// Page-aligned, page-rounded allocation for VirtualAlloc semantics.
pub fn host_alloc_pages(size: usize) -> *mut u8 { heap_alloc_raw(size.max(1), PAGE, true) }
pub unsafe fn host_free(p: *mut u8) { heap_free_raw(p); }
pub unsafe fn host_realloc(p: *mut u8, size: usize) -> *mut u8 {
    host_realloc_aligned(p, size, 0x10)
}
pub unsafe fn host_realloc_aligned(p: *mut u8, size: usize, align: usize) -> *mut u8 {
    // realloc(nullptr, n) == malloc(n); realloc(p, 0) == free(p) + nullptr.
    if size == 0 {
        heap_free_raw(p);
        return std::ptr::null_mut();
    }
    if p.is_null() {
        return heap_alloc_raw(size, align, false);
    }
    // An mmap'd reservation that shrinks keeps its mapping (the tail pages
    // are simply never touched); growing falls through to alloc+copy (D017).
    if let Some(b) = alloc_table().lock().unwrap().get(&(p as usize)).copied() {
        if b.mmap && size <= b.size { return p; }
    }
    let old = heap_size_raw(p);
    let n = heap_alloc_raw(size, align, false);
    if !n.is_null() && old > 0 {
        // Copy min(old, new): the old code copied the full new size and could
        // read past the end of the old block (heap overread).
        let copy = old.min(size);
        std::ptr::copy_nonoverlapping(p, n, copy);
        heap_free_raw(p);
    }
    n
}

// ---------------------------------------------------------------------------
// Module table + GetProcAddress (D014)
// ---------------------------------------------------------------------------

const MODULE_HANDLE_BASE: u64 = 0x0000_0700_0000_0000;

static MODULES: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());

fn module_handle_for(dll: &str) -> u64 {
    let name = dll.to_ascii_lowercase();
    let mut m = MODULES.lock().unwrap();
    for (h, n) in m.iter() {
        if *n == name { return *h; }
    }
    let h = MODULE_HANDLE_BASE + ((m.len() as u64) + 1) * 0x10;
    m.push((h, name));
    h
}

fn module_name_of(handle: u64) -> Option<String> {
    MODULES.lock().unwrap().iter().find(|(h, _)| *h == handle).map(|(_, n)| n.clone())
}

/// "C:\\Windows\\System32\\d3d11.dll" (or "d3d11") -> "d3d11.dll" (lowercase).
fn dll_basename(name: &str) -> String {
    let p = name.replace('\\', "/");
    let mut base = p.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    if !base.is_empty() && !base.contains('.') {
        base.push_str(".dll");
    }
    base
}

/// Cached (dll, func) -> thunk address, so repeated GetProcAddress is O(1) and
/// each distinct proc gets exactly one (leaked) hostcall thunk.
fn proc_thunks() -> &'static Mutex<HashMap<(String, String), u64>> {
    static T: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
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

thread_local! {
    /// Stable id of the calling host thread (assigned on first use).
    static MY_TID: std::cell::Cell<u64> = std::cell::Cell::new(0);
}

/// GetCurrentThreadId: the id is stable for the life of the thread. The old
/// counter returned a NEW value on every call, so anything keying per-thread
/// state (CRT caches, engine thread registries) grew without bound.
/// Public: the dispatcher stamps the same value into TEB ClientId.ThreadId.
pub fn tid() -> u64 {
    MY_TID.with(|t| {
        let v = t.get();
        if v != 0 { return v; }
        let n = TID_COUNTER.fetch_add(1, Ordering::Relaxed);
        t.set(n);
        n
    })
}

/// Reserve a fresh thread id (guest-thread spawn).
pub fn alloc_tid() -> u64 { TID_COUNTER.fetch_add(1, Ordering::Relaxed) }

/// Pin a specific id to the calling thread (guest-thread spawn), so
/// GetThreadId(handle) and GetCurrentThreadId() agree on the value.
pub fn set_current_tid(t: u64) { MY_TID.with(|c| c.set(t)) }

static TID_COUNTER: AtomicU64 = AtomicU64::new(1);
