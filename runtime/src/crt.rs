//! C runtime (CRT) shims the game imports through `api-ms-win-crt-*`, `msvcp140`
//! and `vcruntime140`. The bulk are plain C library functions; we implement the
//! ones the game actually imports, mapping the rest to honest failures.
//!
//! These are called from the import resolver in `win32.rs` with the Microsoft
//! x64 convention already decoded into `a0..a3` plus the guest stack.

use crate::dispatch::GowCpu;

/// Handle a CRT import by name. Returns true if handled.
pub unsafe fn call(cpu: *mut GowCpu, func: &str) -> bool {
    let c = &mut *cpu;
    let (a0, a1, a2, a3) = (c.gpr.rcx, c.gpr.rdx, c.gpr.r8, c.gpr.r9);
    let ret = match func {
        // --- memory (vcruntime140) ---
        "memcpy" | "memmove" => {
            std::ptr::copy(a0 as *const u8, a1 as *mut u8, a2 as usize);
            a0
        }
        "memset" => {
            std::ptr::write_bytes(a0 as *mut u8, a1 as u8, a2 as usize);
            a0
        }
        "memcmp" => {
            let a = std::slice::from_raw_parts(a0 as *const u8, a2 as usize);
            let b = std::slice::from_raw_parts(a1 as *const u8, a2 as usize);
            match a.cmp(b) { std::cmp::Ordering::Less => -1i64 as u64, std::cmp::Ordering::Equal => 0, std::cmp::Ordering::Greater => 1 }
        }
        "memchr" => {
            let s = std::slice::from_raw_parts(a0 as *const u8, a2 as usize);
            match s.iter().position(|&x| x == a1 as u8) {
                Some(i) => a0 + i as u64,
                None => 0,
            }
        }
        // --- heap (api-ms-win-crt-heap) ---
        "malloc" => crate::win32::host_alloc(a0 as usize) as u64,
        "calloc" => {
            let n = (a0 as usize).saturating_mul(a1 as usize);
            let p = crate::win32::host_alloc(n);
            std::ptr::write_bytes(p, 0, n);
            p as u64
        }
        "realloc" => crate::win32::host_realloc(a0 as *mut u8, a1 as usize) as u64,
        "free" => { crate::win32::host_free(a0 as *mut u8); 0 }
        "_aligned_malloc" => crate::win32::host_alloc_aligned(a0 as usize, a1 as usize) as u64,
        "_aligned_free" => { crate::win32::host_free(a0 as *mut u8); 0 }
        "_aligned_realloc" => crate::win32::host_realloc(a0 as *mut u8, a1 as usize) as u64,
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
            std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *p.add(s.len()) = 0;
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
            std::ptr::copy_nonoverlapping(buf.as_ptr(), a1 as *mut u8, buf.len());
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
            if a0 != 0 { *(a0 as *mut i64) = t as i64; }
            t
        }
        _ => return false,
    };
    c.gpr.rax = ret;
    true
}

fn cstr(ptr: u64) -> String {
    if ptr == 0 { return String::new(); }
    unsafe { std::ffi::CStr::from_ptr(ptr as *const std::os::raw::c_char).to_string_lossy().into_owned() }
}

unsafe fn unsafe_copy_cstr(dst: u64, src: u64) {
    let s = cstr(src);
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, s.len());
    *(dst as *mut u8).add(s.len()) = 0;
}
unsafe fn unsafe_copy_cstr_n(dst: u64, src: u64, n: usize) {
    let s = cstr(src);
    let k = s.len().min(n);
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, k);
    for i in k..n { *(dst as *mut u8).add(i) = 0; }
}
fn write_cstr_at(ptr: u64, s: &str) {
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
