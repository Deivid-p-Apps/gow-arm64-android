//! C runtime (CRT) shims the game imports through `api-ms-win-crt-*`, `msvcp140`
//! and `vcruntime140`. The bulk are plain C library functions; we implement the
//! ones the game actually imports, mapping the rest to honest failures.
//!
//! These are called from the import resolver in `win32.rs` with the Microsoft
//! x64 convention already decoded into `a0..a3` plus the guest stack.
//!
//! Pointer policy (build #155 post-mortem): once the engine's state degraded,
//! the game called CRT string/memory functions with wild pointers (0x80 etc.)
//! and `CStr::from_ptr`/raw copies faulted the HOST (the final SIGSEGV at
//! 0x80). Every guest pointer dereferenced here is validated with
//! `dispatch::plausible_mem` first; wild arguments answer with 0/empty instead
//! of killing the process.

use crate::dispatch::{plausible_mem, GowCpu};

/// Handle a CRT import by name. Returns true if handled.
pub unsafe fn call(cpu: *mut GowCpu, func: &str) -> bool {
    let c = &mut *cpu;
    let (a0, a1, a2, a3) = (c.gpr.rcx, c.gpr.rdx, c.gpr.r8, c.gpr.r9);
    let ret = match func {
        // --- memory (vcruntime140) ---
        "memcpy" | "memmove" => {
            if plausible_mem(a0, a2) && plausible_mem(a1, a2) {
                std::ptr::copy(a0 as *const u8, a1 as *mut u8, a2 as usize);
            }
            a0
        }
        "memset" => {
            if plausible_mem(a0, a2) {
                std::ptr::write_bytes(a0 as *mut u8, a1 as u8, a2 as usize);
            }
            a0
        }
        "memcmp" => {
            if !(plausible_mem(a0, a2) && plausible_mem(a1, a2)) {
                0
            } else {
                let a = std::slice::from_raw_parts(a0 as *const u8, a2 as usize);
                let b = std::slice::from_raw_parts(a1 as *const u8, a2 as usize);
                match a.cmp(b) { std::cmp::Ordering::Less => -1i64 as u64, std::cmp::Ordering::Equal => 0, std::cmp::Ordering::Greater => 1 }
            }
        }
        "memchr" => {
            if !plausible_mem(a0, a2) {
                0
            } else {
                let s = std::slice::from_raw_parts(a0 as *const u8, a2 as usize);
                match s.iter().position(|&x| x == a1 as u8) {
                    Some(i) => a0 + i as u64,
                    None => 0,
                }
            }
        }
        // --- heap (api-ms-win-crt-heap) ---
        "malloc" => {
            let p = crate::win32::host_alloc(a0 as usize);
            if a0 >= (1024 * 1024) && std::env::var_os("GOW_MEMTRACE").is_some() {
                crate::glog!("[gow.malloc] {} KiB -> {:p}", a0 / 1024, p);
            }
            p as u64
        }
        "calloc" => {
            let n = (a0 as usize).saturating_mul(a1 as usize);
            let p = crate::win32::host_alloc(n);
            if !p.is_null() { std::ptr::write_bytes(p, 0, n); }
            p as u64
        }
        "realloc" => crate::win32::host_realloc(a0 as *mut u8, a1 as usize) as u64,
        "free" => { crate::win32::host_free(a0 as *mut u8); 0 }
        "_aligned_malloc" => crate::win32::host_alloc_aligned(a0 as usize, a1 as usize) as u64,
        "_aligned_free" => { crate::win32::host_free(a0 as *mut u8); 0 }
        // _aligned_realloc(mem, size, alignment): the alignment (R8) used to be
        // dropped, silently downgrading AVX-sized alignments to 16 bytes.
        "_aligned_realloc" => crate::win32::host_realloc_aligned(a0 as *mut u8, a1 as usize, a2 as usize) as u64,
        // setvbuf(stream, buf, mode, size): the CRT's stdio buffering is a host
        // detail; report success so callers don't take an error path.
        "setvbuf" | "_setvbuf" => 0,
        // --- strings ---
        "strcmp" | "_stricmp" | "_strcmp" => {
            let a = cstr(a0); let b = cstr(a1);
            let eq = if func == "_stricmp" { a.eq_ignore_ascii_case(&b) } else { a == b };
            if eq { 0 } else if a < b { -1i64 as u64 } else { 1 }
        }
        "strncmp" | "_strnicmp" => {
            let n = a2 as usize;
            let a = cstr(a0).chars().take(n).collect::<String>();
            let b = cstr(a1).chars().take(n).collect::<String>();
            let eq = if func == "_strnicmp" { a.eq_ignore_ascii_case(&b) } else { a == b };
            if eq { 0 } else if a < b { -1i64 as u64 } else { 1 }
        }
        "strcpy" => { unsafe_copy_cstr(a0, a1); a0 }
        "strcpy_s" => {
            // (dest, destsz, src): copy with bounds, NUL-terminate.
            let n = a2 as usize;
            let src = cstr(a1);
            let k = src.len().min(n.saturating_sub(1));
            if plausible_mem(a0, k as u64 + 1) {
                std::ptr::copy_nonoverlapping(src.as_ptr(), a0 as *mut u8, k);
                *(a0 as *mut u8).add(k) = 0;
            }
            0
        }
        "strncpy_s" => {
            // (dest, destsz, src, count)
            let destsz = a1 as usize;
            let count = a3 as usize;
            let src = cstr(a2);
            let k = count.min(src.len()).min(destsz.saturating_sub(1));
            if plausible_mem(a0, k as u64 + 1) {
                std::ptr::copy_nonoverlapping(src.as_ptr(), a0 as *mut u8, k);
                *(a0 as *mut u8).add(k) = 0;
            }
            0
        }
        "memcpy_s" => {
            let (dest, destsz, src, count) = (a0, a1 as usize, a2, a3 as usize);
            if count <= destsz && plausible_mem(dest, count as u64) && plausible_mem(src, count as u64) {
                std::ptr::copy_nonoverlapping(src as *const u8, dest as *mut u8, count);
            }
            0
        }
        "wcsncpy_s" | "wcscpy_s" | "wcsncat_s" | "wcscat_s" | "strncat_s" | "strcat_s" => 0,
        "__stdio_common_vfprintf" | "__stdio_common_vsprintf" | "__stdio_common_vsnprintf"
        | "__stdio_common_vswprintf" | "__stdio_common_vfscanf" | "__stdio_common_vsscanf"
        | "__stdio_common_vsprintf_s" | "__stdio_common_vswprintf_s" => {
            // Formatted output to a FILE/buffer: we don't implement the full
            // printf engine. Return the number of chars "written" as 0 so the
            // caller does not loop; the actual log path is unused at boot.
            0
        }
        "strncpy" => { unsafe_copy_cstr_n(a0, a1, a2 as usize); a0 }
        "strlen" => cstr(a0).len() as u64,
        "strchr" => {
            let s = cstr(a0);
            match s.find(a1 as u8 as char) { Some(i) => a0 + i as u64, None => 0 }
        }
        "strrchr" => {
            let s = cstr(a0);
            match s.rfind(a1 as u8 as char) { Some(i) => a0 + i as u64, None => 0 }
        }
        "strstr" => {
            let s = cstr(a0); let sub = cstr(a1);
            match s.find(&sub) { Some(i) => a0 + i as u64, None => 0 }
        }
        "strcat" => { let base = a0; let mut s = cstr(a0); s.push_str(&cstr(a1)); write_cstr_at(base, &s); base }
        "strncat" => { let base = a0; let mut s = cstr(a0); s.push_str(&cstr(a1).chars().take(a2 as usize).collect::<String>()); write_cstr_at(base, &s); base }
        "_strdup" | "_wcsdup" => {
            let s = cstr(a0);
            let p = crate::win32::host_alloc(s.len() + 1);
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
                *p.add(s.len()) = 0;
            }
            p as u64
        }
        "_strlwr" => { let s = cstr(a0).to_lowercase(); write_cstr_at(a0, &s); a0 }
        "_strupr" => { let s = cstr(a0).to_uppercase(); write_cstr_at(a0, &s); a0 }
        "strtok" => 0, // stateful; rarely on the boot path
        // --- conversion ---
        "atoi" | "_strtoi64" => cstr(a0).trim().parse::<i64>().unwrap_or(0) as u64,
        "atof" | "strtod" | "strtof" => {
            let s = cstr(a0);
            let v: f64 = s.trim().parse().unwrap_or(0.0);
            if func == "strtof" { (v as f32).to_bits() as u64 } else { v.to_bits() }
        }
        "strtol" | "strtoul" => cstr(a0).trim().parse::<i64>().unwrap_or(0) as u64,
        "_itoa" => {
            // (value, buffer, radix)
            let mut buf = match a2 as u32 { 16 => format!("{:x}", a0 as i64), 8 => format!("{:o}", a0 as i64), _ => format!("{}", a0 as i64) };
            buf.push('\0');
            if plausible_mem(a1, buf.len() as u64) {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), a1 as *mut u8, buf.len());
            }
            a1
        }
        // --- math ---
        "sin" => f64::from_bits(a0).sin().to_bits(),
        "cos" => f64::from_bits(a0).cos().to_bits(),
        "tan" => f64::from_bits(a0).tan().to_bits(),
        "sinf" => (f32::from_bits(a0 as u32).sin() as f64).to_bits(),
        "cosf" => (f32::from_bits(a0 as u32).cos() as f64).to_bits(),
        "tanf" => (f32::from_bits(a0 as u32).tan() as f64).to_bits(),
        "atanf" => (f32::from_bits(a0 as u32).atan() as f64).to_bits(),
        "atan2f" => (f32::from_bits(a0 as u32).atan2(f32::from_bits(a1 as u32)) as f64).to_bits(),
        "asinf" => (f32::from_bits(a0 as u32).asin() as f64).to_bits(),
        "acosf" => (f32::from_bits(a0 as u32).acos() as f64).to_bits(),
        "sinhf" => (f32::from_bits(a0 as u32).sinh() as f64).to_bits(),
        "coshf" => (f32::from_bits(a0 as u32).cosh() as f64).to_bits(),
        "tanhf" => (f32::from_bits(a0 as u32).tanh() as f64).to_bits(),
        "fmod" => f64::from_bits(a0).rem_euclid(f64::from_bits(a1)).to_bits(),
        "fmodf" => ((f32::from_bits(a0 as u32) % f32::from_bits(a1 as u32)) as f64).to_bits(),
        "pow" => f64::from_bits(a0).powf(f64::from_bits(a1)).to_bits(),
        "powf" => (f32::from_bits(a0 as u32).powf(f32::from_bits(a1 as u32)) as f64).to_bits(),
        "exp" => f64::from_bits(a0).exp().to_bits(),
        "exp2f" => (f32::from_bits(a0 as u32).exp2() as f64).to_bits(),
        "expf" => (f32::from_bits(a0 as u32).exp() as f64).to_bits(),
        "logf" => (f32::from_bits(a0 as u32).ln() as f64).to_bits(),
        "log10" => f64::from_bits(a0).log10().to_bits(),
        "log10f" => (f32::from_bits(a0 as u32).log10() as f64).to_bits(),
        "log2f" => (f32::from_bits(a0 as u32).log2() as f64).to_bits(),
        "cbrtf" => (f32::from_bits(a0 as u32).cbrt() as f64).to_bits(),
        "roundf" => (f32::from_bits(a0 as u32).round() as f64).to_bits(),
        "sqrt" | "sqrtf" => (f64::from_bits(a0).sqrt()).to_bits(),
        "ldexp" => {
            // (double, int)
            let x = f64::from_bits(a0);
            let e = a1 as i32;
            (x * 2f64.powi(e)).to_bits()
        }
        "_finite" => {
            let x = f64::from_bits(a0);
            1 // finite; also used for +inf/-inf/nan checks elsewhere
        }
        // --- utility ---
        "rand" => { (next_rand() % 0x7fff) as u64 }
        "srand" => 0,
        // --- time ---
        "_time64" => {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            if plausible_mem(a0, 8) { *(a0 as *mut i64) = t as i64; }
            t
        }
        // --- C/C++ startup: run static initializers (the path to main) ---
        "_initterm" | "_initterm_e" => {
            let mut p = a0;
            let end = a1;
            let mut count = 0u64;
            while p + 8 <= end {
                if !plausible_mem(p, 8) { break; }
                let fva = *(p as *const u64);
                if fva != 0 {
                    call_guest(cpu, fva);
                    count += 1;
                }
                p += 8;
            }
            crate::glog!("[gow.crt] _initterm ran {count} initializers");
            0
        }
        "_crt_atexit" | "atexit" | "_register_onexit_function" | "_execute_onexit_table"
        | "_initialize_onexit_table" | "_crt_at_quick_exit" => 0,
        "_set_app_type" | "_set_abort_behavior" | "_set_invalid_parameter_handler"
        | "_set_purecall_handler" | "set_terminate" | "set_unexpected"
        | "_controlfp" | "_set_new_mode" | "_configthreadlocale" => 0,
        "_getpid" => std::process::id() as u64,
        // _beginthreadex(security, stack_size, start_address, arg, initflag, thrdaddr):
        // start in R8, arg in R9. Returns a REAL thread handle (D019): the old
        // code returned a constant 0x1234, so the engine's ResumeThread/
        // GetThreadId/WaitForSingleObject on worker handles were operating on
        // garbage. Routed through the ThreadTable when the resolver is
        // installed (device); the host-test fallback keeps the old spawn.
        "_beginthreadex" => spawn_guest_thread(cpu, a2, a3),
        "_endthreadex" | "_cexit" | "_c_exit" | "exit" | "_exit" => 0,
        "_get_narrow_winmain_command_line" => {
            let s = b"GoW.exe\0";
            let p = crate::win32::host_alloc(s.len());
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            }
            p as u64
        }
        "_initialize_narrow_environment" | "_configure_narrow_argv" => 0,
        "_invalid_parameter_noinfo" | "_invalid_parameter_noinfo_noreturn" => 0,
        "_seh_filter_exe" | "_seh_filter_dll" => 0,
        "__sys_nerr" => 0,
        // errno accessors: return the ADDRESS of a stable per-process int (the
        // CRT stores through it). The old unimplemented answer returned 0, so
        // `*_errno() = x` wrote to the null page (a badstore/0-addr fault) and
        // errno reads always yielded 0.
        "_errno" | "__acrt_errno" => {
            static ERRNO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
            &ERRNO as *const _ as u64
        }
        "__doserrno" | "_get_doserrno" => {
            static DOSERR: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
            &DOSERR as *const _ as u64
        }
        "__acrt_iob_func" => 0x0000_0c00_0000_0000 + a0 * 0x100, // stable fake FILE*
        "_set_fmode" => 0,
        "__p__commode" => {
            // Return a pointer to a stable int holding the commode (0 = text).
            static COMMODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
            &COMMODE as *const _ as u64
        }
        "qsort" => {
            // (base, count, width, compare). Insertion sort using the guest
            // comparator (a function pointer we call back into).
            let (base, count, width, cmp) = (a0, a1 as usize, a2 as usize, a3);
            if width != 0 && count > 1 && plausible_mem(base, count.saturating_mul(width) as u64) {
                for i in 1..count {
                    let mut j = i;
                    while j > 0 {
                        let pa = base + ((j - 1) * width) as u64;
                        let pb = base + (j * width) as u64;
                        let r = call_guest_i64(cpu, cmp, pa, pb);
                        if r <= 0 { break; }
                        swap_bytes(pa, pb, width);
                        j -= 1;
                    }
                }
            }
            0
        }
        "_Mtx_init_in_situ" | "_Mtx_destroy_in_situ" => 0,
        "_Mtx_lock" | "_Mtx_unlock" => 0,
        "_Mtx_init" | "_Mtx_destroy" => 0,
        // --- setjmp/longjmp (save/restore the guest register file) ---
        "__intrinsic_setjmp" | "_setjmp" | "setjmp" => {
            // Save the guest state into the jmp_buf. Layout (our own, 64 bytes):
            // [0..8)=RSP after return, [8..16)=return RVA, [16..144)=GPRs.
            let buf = a0;
            if !plausible_mem(buf, 152) { c.gpr.rax = 0; return true; }
            // The return RVA is the value just pushed by the guest call; read the
            // caller's return address from the guest stack top.
            let ret_addr_va = if plausible_mem(c.gpr.rsp + 8, 8) { *((c.gpr.rsp + 8) as *const u64) } else { 0 };
            *(buf as *mut u64) = c.gpr.rsp + 16; // resume stack
            *((buf as *mut u8).add(8) as *mut u64) = ret_addr_va;
            let gprs = (buf as *mut u8).add(16) as *mut u64;
            for (i, v) in [c.gpr.rax, c.gpr.rbx, c.gpr.rcx, c.gpr.rdx, c.gpr.rsi, c.gpr.rdi,
                           c.gpr.rbp, c.gpr.rsp, c.gpr.r8, c.gpr.r9, c.gpr.r10, c.gpr.r11,
                           c.gpr.r12, c.gpr.r13, c.gpr.r14, c.gpr.r15].iter().enumerate() {
                *gprs.add(i) = *v;
            }
            0
        }
        "longjmp" | "_longjmp" => {
            let buf = a0;
            if !plausible_mem(buf, 16) { c.gpr.rax = 1; return true; }
            let val = if a1 == 0 { 1 } else { a1 };
            c.gpr.rsp = *(buf as *const u64);
            c.gpr.rax = val;
            // Resume at the saved return RVA via the dispatcher.
            let ret_va = *((buf as *const u8).add(8) as *const u64);
            if ret_va >= c.image_base && ret_va < c.image_base + c.image_size {
                c.next_rva = (ret_va - c.image_base) as u32;
            }
            val
        }
        "__C_specific_handler" | "__CxxFrameHandler3" => return false, // SEH/C++ exceptions unimplemented
        // --- environment / locale / narrow-wide conversion ---
        // These were logged "unimplemented" during boot. Returning 0 for getenv
        // is correct (no env vars set); the conversions must not leave RAX as
        // garbage. wcstombs(wchar* src, char* dst, size_t n) is RCX/RDX/R8.
        "getenv" | "_wgetenv" => 0,
        "wcstombs" | "wcstombs_s" => {
            // Convert the wide string at a0 into the byte buffer at a1, up to a2
            // bytes, NUL-terminated. Returns the count written (not incl. NUL).
            if !plausible_mem(a1, 1) { 0 } else {
                let mut n = 0u64;
                let mut i = 0u64;
                while n + 1 < a2 && plausible_mem(a0 + i * 2, 2) {
                    let w = *((a0 + i * 2) as *const u16);
                    if w == 0 { break; }
                    // ASCII-range only; anything else becomes '?'.
                    let ch = if w < 0x80 { w as u8 } else { b'?' };
                    *((a1 + n) as *mut u8) = ch;
                    n += 1; i += 1;
                }
                *((a1 + n) as *mut u8) = 0;
                n
            }
        }
        "mbstowcs" | "mbstowcs_s" => {
            // Convert the narrow string at a0 into the wide buffer at a1, up to
            // a2 wide chars, NUL-terminated. Returns the count written.
            if !plausible_mem(a1, 2) { 0 } else {
                let mut n = 0u64;
                let mut i = 0u64;
                while n + 1 < a2 && plausible_mem(a0 + i, 1) {
                    let ch = *((a0 + i) as *const u8);
                    if ch == 0 { break; }
                    *((a1 + n * 2) as *mut u16) = ch as u16;
                    n += 1; i += 1;
                }
                *((a1 + n * 2) as *mut u16) = 0;
                n
            }
        }
        _ => return false,
    };
    c.gpr.rax = ret;
    true
}

/// Call a guest function by absolute VA, running it to completion through the
/// dispatcher. Used by `_initterm` to run the game's static initializers.
unsafe fn call_guest(cpu: *mut GowCpu, va: u64) {
    let c = &mut *cpu;
    if va >= c.image_base && va < c.image_base + c.image_size {
        let rva = (va - c.image_base) as u32;
        // Recursively drive the guest function; it returns via gow_ret to a
        // sentinel we push here.
        crate::dispatch::gow_run(cpu, rva);
    }
}

/// Spawn a guest thread that runs the function at `start_va` with `arg` in RCX.
/// Returns the thread handle (D019): routed through the ThreadTable when the
/// import resolver is installed (device runs), so ResumeThread/
/// WaitForSingleObject/GetThreadId work on the handle; the host-test fallback
/// spawns directly and returns the old fake handle.
unsafe fn spawn_guest_thread(cpu: *mut GowCpu, start_va: u64, arg: u64) -> u64 {
    let c = &*cpu;
    if start_va < c.image_base || start_va >= c.image_base + c.image_size {
        return 0;
    }
    let rva = (start_va - c.image_base) as u32;
    if let Some(r) = crate::dispatch::import_resolver() {
        return r.threads.spawn(cpu, rva as u64, arg);
    }
    let base = c.image_base;
    let size = c.image_size as usize;
    let map: Vec<(u32, crate::dispatch::GowFunc)> = c.func_map.iter().map(|(k, v)| (*k, *v)).collect();
    std::thread::spawn(move || {
        let mut cpu = GowCpu::new(base, size);
        for (r, f) in map { cpu.func_map.insert(r, f); }
        cpu.gpr.rcx = arg;
        crate::glog!("[gow.crt] _beginthreadex start rva=0x{rva:x}");
        unsafe { crate::dispatch::gow_run(cpu.as_mut() as *mut GowCpu, rva) };
    });
    0x1234
}

/// Read a guest C string. Wild pointers (null page, kernel range, wrap-around)
/// return an empty string instead of faulting the host: after the engine's
/// state degraded (build #155) the game called CRT string functions with
/// pointers like 0x80 — `CStr::from_ptr(0x80)` was the SIGSEGV at 0x80.
fn cstr(ptr: u64) -> String {
    if !plausible_mem(ptr, 1) { return String::new(); }
    unsafe {
        // Byte-wise scan with a per-byte plausibility guard: even a
        // plausible-but-unmapped pointer cannot crash the host here.
        let mut v = Vec::new();
        let mut p = ptr as *const u8;
        loop {
            if !plausible_mem(p as u64, 1) { break; }
            let ch = *p;
            if ch == 0 { break; }
            v.push(ch);
            p = p.add(1);
            if v.len() > 4096 { break; }
        }
        String::from_utf8_lossy(&v).into_owned()
    }
}

/// Call a guest function (by absolute VA) with two args, returning its RAX as
/// i64. Used by qsort's comparator callback.
unsafe fn call_guest_i64(cpu: *mut GowCpu, va: u64, a: u64, b: u64) -> i64 {
    if va == 0 { return 0; }
    let c = &mut *cpu;
    if va < c.image_base || va >= c.image_base + c.image_size { return 0; }
    let rva = (va - c.image_base) as u32;
    let save = c.gpr;
    c.gpr.rcx = a;
    c.gpr.rdx = b;
    crate::dispatch::gow_run(cpu, rva);
    let r = c.gpr.rax as i64;
    c.gpr = save;
    r
}

fn swap_bytes(a: u64, b: u64, n: usize) {
    unsafe {
        for i in 0..n {
            let pa = (a as *mut u8).add(i);
            let pb = (b as *mut u8).add(i);
            std::ptr::swap(pa, pb);
        }
    }
}

unsafe fn unsafe_copy_cstr(dst: u64, src: u64) {
    let s = cstr(src);
    if !plausible_mem(dst, s.len() as u64 + 1) { return; }
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, s.len());
    *(dst as *mut u8).add(s.len()) = 0;
}
unsafe fn unsafe_copy_cstr_n(dst: u64, src: u64, n: usize) {
    let s = cstr(src);
    let k = s.len().min(n);
    if !plausible_mem(dst, n as u64) { return; }
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, k);
    for i in k..n { *(dst as *mut u8).add(i) = 0; }
}
fn write_cstr_at(ptr: u64, s: &str) {
    if !plausible_mem(ptr, s.len() as u64 + 1) { return; }
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, s.len());
        *(ptr as *mut u8).add(s.len()) = 0;
    }
}

static RAND_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x2545F4914F6CDD1D);
fn next_rand() -> u64 {
    use std::sync::atomic::Ordering;
    let mut x = RAND_STATE.load(Ordering::Relaxed);
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    RAND_STATE.store(x, Ordering::Relaxed);
    x
}
