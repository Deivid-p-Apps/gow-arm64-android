//! Dispatcher + guest memory for recompiled x86-64 code.
//!
//! Control-flow model
//! ------------------
//! Generated functions (gow_game.c) never call each other with the C call
//! mechanism. Each one sets the "next" function pointer/RVA in GowCpu and
//! returns to a central loop (`gow_run`). This makes x86 call / ret / tail-jmp
//! exact without relying on host stack frames:
//!   * call  -> push return RVA onto the *guest* stack, set next = callee
//!   * ret   -> pop the guest stack into next
//!   * jmp   -> set next = target (loop continues, no C recursion)
//! The loop is a trampoline; guest stack depth is bounded by the game itself.

use std::collections::HashMap;
use std::sync::OnceLock;

pub type GowFunc = unsafe extern "C" fn(*mut GowCpu);

pub const CF: u64 = 1 << 0;
pub const PF: u64 = 1 << 2;
pub const AF: u64 = 1 << 4;
pub const ZF: u64 = 1 << 6;
pub const SF: u64 = 1 << 7;
pub const DF: u64 = 1 << 10;
pub const OF: u64 = 1 << 11;
pub const FLAG_MASK: u64 = CF | PF | AF | ZF | SF | OF | DF;

#[inline]
fn parity(v: u64) -> u64 {
    if (v as u8).count_ones() % 2 == 0 { PF } else { 0 }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Gprs {
    pub rax: u64, pub rcx: u64, pub rdx: u64, pub rbx: u64,
    pub rsp: u64, pub rbp: u64, pub rsi: u64, pub rdi: u64,
    pub r8: u64, pub r9: u64, pub r10: u64, pub r11: u64,
    pub r12: u64, pub r13: u64, pub r14: u64, pub r15: u64,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Xmm(pub [u64; 2]);
impl Default for Xmm { fn default() -> Self { Xmm([0, 0]) } }

#[repr(C)]
pub struct GowCpu {
    pub gpr: Gprs,
    pub xmm: [Xmm; 17],  // XMM0..15 plus index 16 as a scratch
    pub fpu: crate::x87::Fpu,

    pub rip: u64,
    pub flags: u64,
    pub mxcsr: u32,
    /// Next function RVA selected by the last control-flow helper. u32::MAX = stop.
    pub next_rva: u32,
    pub heap_base: u64,
    pub heap_size: u64,
    pub image_base: u64,
    pub image_size: u64,
    pub image: *mut u8,
    /// Owns the backing allocation for `image` (kept alive with the CPU).
    pub image_buf: Vec<u8>,
    /// Owns the guest stack allocation.
    pub stack_buf: Vec<u8>,
    pub func_map: HashMap<u32, GowFunc>,
    pub teb: u64,
    /// Owns the TEB/PEB backing memory (valid for the CPU lifetime).
    pub teb_buf: Vec<u8>,
    pub call_depth: u32,
    pub missing_indirect_calls: u64,
    pub unimpl_hits: u64,
    /// Win32 import dispatch: IAT RVA -> host handler id.
    pub import_map: HashMap<u32, u32>,
}

unsafe impl Send for GowCpu {}

pub const STOP_RVA: u32 = u32::MAX;

#[inline]
pub fn LANES32(v: [u64; 2]) -> [u32; 4] {
    [v[0] as u32, (v[0] >> 32) as u32, v[1] as u32, (v[1] >> 32) as u32]
}

/// Allocate a zeroed, process-lifetime block (leaked on purpose) for guest
/// structures like per-module TLS blocks.
fn alloc_guest_block(size: usize) -> *mut u8 {
    let layout = match std::alloc::Layout::from_size_align(size.max(0x1000), 0x1000) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    unsafe { std::alloc::alloc_zeroed(layout) }
}

/// Map `size` bytes at the fixed address `base` (MAP_FIXED). Returns
/// (pointer, owned Vec, whether mapping succeeded). On failure, falls back to a
/// normal Vec so the program still runs (with a logged warning).
fn map_at(base: u64, size: usize) -> (*mut u8, Vec<u8>, bool) {
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
        }
        const PROT_READ: i32 = 1;
        const PROT_WRITE: i32 = 2;
        const PROT_EXEC: i32 = 4;
        const MAP_PRIVATE: i32 = 2;
        const MAP_ANONYMOUS: i32 = 0x20; // Linux
        const MAP_FIXED: i32 = 0x10;
        let p = mmap(
            base as *mut u8,
            size,
            PROT_READ | PROT_WRITE | PROT_EXEC,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            -1,
            0,
        );
        // MAP_FAILED is (void*)-1. On success the mapping lives for the process
        // lifetime (we never munmap); return an empty Vec so nothing tries to
        // free it.
        if p as isize != -1 {
            return (p, Vec::new(), true);
        }
    }
    let mut v = vec![0u8; size];
    let p = v.as_mut_ptr();
    (p, v, false)
}


/// Function table emitted by the recompiler (gow_game.c) is registered at
/// startup via `install_registry`, so no extern symbol is required here.

#[repr(C)]
pub struct GowFuncEntry { pub rva: u32, pub fnptr: Option<GowFunc> }

impl GowCpu {
    pub fn new(image_base: u64, image_size: usize) -> Box<Self> {
        // Map the guest image at its ORIGINAL virtual address (e.g. 0x140000000)
        // so guest pointers equal host pointers and absolute addresses in the
        // recompiled code (globals, vtables) are valid. Falls back to a normal
        // allocation if the fixed mapping is unavailable.
        let (image, image_buf, fixed) = map_at(image_base, image_size);
        let mut stack = vec![0u8; 16 * 1024 * 1024];
        // Leave headroom above the initial RSP: at a real process entry, [rsp]
        // holds a return address and callers' frames live at higher addresses.
        // Starting exactly at the buffer end makes writes like [rsp+8] fault.
        let rsp = unsafe { stack.as_mut_ptr().add(stack.len() - 0x8000) as u64 };
        // Allocate a real TEB/PEB/TLS block so gs:/fs: accesses land on valid memory.
        // Windows x64 TEB layout used here:
        //   0x30 -> PEB (Self)
        //   0x58 -> ThreadLocalStoragePointer (array of TLS slot values)
        //   0x148.. -> TlsSlots[64] (inline TLS storage)
        let mut teb_buf = vec![0u8; 0x14000];
        let teb = teb_buf.as_mut_ptr() as u64;
        let peb = teb + 0x8000;
        let tls_ptr = teb + 0xC000; // TLS pointer array (distinct from PEB at +0x8000)
        unsafe {
            *((teb + 0x30) as *mut u64) = peb;         // TEB->PEB
            *((peb + 0x10) as *mut u64) = image_base;  // PEB->ImageBaseAddress
            // 128 TLS slots. Each slot must point at a REAL per-module TLS block
            // (Windows' loader allocates one per module index). A null slot makes
            // code like `mov rax,[gs:0x58]; mov rdx,[rax]; mov [rdx+0x80],..` fault.
            std::ptr::write_bytes(tls_ptr as *mut u8, 0, 0x800);
            for i in 0..128u64 {
                let blk = alloc_guest_block(0x1000);
                if !blk.is_null() {
                    std::ptr::write_bytes(blk, 0, 0x1000);
                    *((tls_ptr + i * 8) as *mut u64) = blk as u64;
                }
            }
            *((teb + 0x58) as *mut u64) = tls_ptr;     // TEB->ThreadLocalStoragePointer
            // Inline TlsSlots array at 0x148 (already zeroed).
        }
        if !fixed {
            crate::glog!("[gow] WARNING: could not map image at 0x{image_base:x}; absolute addresses may fault");
        }
        Box::new(GowCpu {
            gpr: Gprs { rsp, ..Default::default() },
            xmm: [Xmm::default(); 17],
            fpu: crate::x87::Fpu::default(),
            rip: 0,
            flags: 0,
            mxcsr: 0x1f80,
            next_rva: STOP_RVA,
            heap_base: 0x1_0000_0000,
            heap_size: 0,
            image_base,
            image_size: image_size as u64,
            image,
            image_buf,
            stack_buf: stack,
            func_map: HashMap::new(),
            teb,
            teb_buf,
            call_depth: 0,
            missing_indirect_calls: 0,
            unimpl_hits: 0,
            import_map: HashMap::new(),
        })
    }

    /// Copy the mapped image into the guest buffer at the guest image base offset.
    pub fn load_image(&mut self, mapped: &[u8]) {
        let n = mapped.len().min(self.image_size as usize);
        unsafe { std::ptr::copy_nonoverlapping(mapped.as_ptr(), self.image, n); }
    }

    #[inline]
    pub fn host_ptr(&self, guest_va: u64) -> *mut u8 {
        let off = guest_va.wrapping_sub(self.image_base);
        unsafe { self.image.add(off as usize) }
    }
}

/// Registry populated by generated code at init.
static REGISTRY: OnceLock<Vec<(u32, GowFunc)>> = OnceLock::new();

pub fn install_registry(entries: Vec<(u32, GowFunc)>) {
    let _ = REGISTRY.set(entries);
}
pub fn registry() -> &'static [(u32, GowFunc)] {
    REGISTRY.get().map(|v| v.as_slice()).unwrap_or(&[])
}

// ---------------------------------------------------------------------------
// Integer op helpers (exact flags for add/sub/logic; caller uses result)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn gow_add64(cpu: *mut GowCpu, a: u64, b: u64, size_bits: u32) -> u64 {
    let bits = size_bits.clamp(8, 64);
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let full = a.wrapping_add(b);
    let r = full & mask;
    let msb = 1u64 << (bits - 1);
    let mut f = 0u64;
    if full > mask { f |= CF; }
    if r == 0 { f |= ZF; }
    if r & msb != 0 { f |= SF; }
    f |= parity(r);
    if ((!(a ^ b)) & (a ^ r)) & msb != 0 { f |= OF; }
    let c = &mut *cpu;
    c.flags = (c.flags & !FLAG_MASK) | (f & FLAG_MASK);
    r
}

#[no_mangle]
pub unsafe extern "C" fn gow_sub64(cpu: *mut GowCpu, a: u64, b: u64, size_bits: u32) -> u64 {
    let bits = size_bits.clamp(8, 64);
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let r = a.wrapping_sub(b) & mask;
    let msb = 1u64 << (bits - 1);
    let mut f = 0u64;
    if (a & mask) < (b & mask) { f |= CF; }
    if r == 0 { f |= ZF; }
    if r & msb != 0 { f |= SF; }
    f |= parity(r);
    if ((a ^ b) & (a ^ r)) & msb != 0 { f |= OF; }
    let c = &mut *cpu;
    c.flags = (c.flags & !FLAG_MASK) | (f & FLAG_MASK);
    r
}

#[no_mangle]
pub unsafe extern "C" fn gow_logic64(cpu: *mut GowCpu, r: u64, size_bits: u32) -> u64 {
    let bits = size_bits.clamp(8, 64);
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let r = r & mask;
    let msb = 1u64 << (bits - 1);
    let mut f = 0u64;
    if r == 0 { f |= ZF; }
    if r & msb != 0 { f |= SF; }
    f |= parity(r);
    let c = &mut *cpu;
    c.flags = (c.flags & !FLAG_MASK) | (f & FLAG_MASK);
    r
}

/// Condition codes matching x86 Jcc encoding (see header comment).
#[no_mangle]
pub unsafe extern "C" fn gow_test_cc(cpu: *mut GowCpu, cc: u32) -> u64 {
    let f = (*cpu).flags;
    let (cf, pf, zf, sf, of) = (
        f & CF != 0, f & PF != 0, f & ZF != 0, f & SF != 0, f & OF != 0,
    );
    let r = match cc {
        0x0 => of,                                 // O
        0x1 => !of,                                // NO
        0x2 => cf,                                 // B / C
        0x3 => !cf,                                // AE / NC
        0x4 => zf,                                 // E / Z
        0x5 => !zf,                                // NE / NZ
        0x6 => cf || zf,                           // BE
        0x7 => !cf && !zf,                         // A
        0x8 => sf,                                 // S
        0x9 => !sf,                                // NS
        0xa => pf,                                 // P
        0xb => !pf,                                // NP
        0xc => sf != of,                           // L
        0xd => sf == of,                           // GE
        0xe => zf || (sf != of),                   // LE
        0xf => !zf && (sf == of),                  // G
        _ => false,
    };
    if r { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// Memory (little-endian, unaligned-safe)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn gow_load(cpu: *mut GowCpu, addr: u64, size: u32) -> u64 {
    if !plausible_mem(addr, size as u64) {
        let c = &*cpu;
        crate::glog!("[gow.badload] load addr=0x{addr:x} size={size} rva=0x{:x} rsp=0x{:x} rbp=0x{:x} rcx=0x{:x} rax=0x{:x}",
            c.rip.wrapping_sub(c.image_base), c.gpr.rsp, c.gpr.rbp, c.gpr.rcx, c.gpr.rax);
        return 0;
    }
    let p = addr as *const u8;
    match size {
        1 => *p as u64,
        2 => (p as *const u16).read_unaligned() as u64,
        4 => (p as *const u32).read_unaligned() as u64,
        8 => (p as *const u64).read_unaligned(),
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_store(cpu: *mut GowCpu, addr: u64, v: u64, size: u32) {
    if !plausible_mem(addr, size as u64) {
        let c = &*cpu;
        crate::glog!("[gow.badstore] store addr=0x{addr:x} size={size} rva=0x{:x} rsp=0x{:x} rbp=0x{:x} rdi=0x{:x} rsi=0x{:x} rcx=0x{:x} rax=0x{:x}",
            c.rip.wrapping_sub(c.image_base), c.gpr.rsp, c.gpr.rbp, c.gpr.rdi, c.gpr.rsi, c.gpr.rcx, c.gpr.rax);
        return;
    }
    let p = addr as *mut u8;
    match size {
        1 => *p = v as u8,
        2 => (p as *mut u16).write_unaligned(v as u16),
        4 => (p as *mut u32).write_unaligned(v as u32),
        8 => (p as *mut u64).write_unaligned(v),
        _ => {}
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_load128(_cpu: *mut GowCpu, addr: u64, out: *mut u8) {
    std::ptr::copy_nonoverlapping(addr as *const u8, out, 16);
}
#[no_mangle]
pub unsafe extern "C" fn gow_store128(_cpu: *mut GowCpu, addr: u64, v: *const u8) {
    std::ptr::copy_nonoverlapping(v, addr as *mut u8, 16);
}

#[no_mangle]
pub unsafe extern "C" fn gow_load_xmm(cpu: *mut GowCpu, reg: u32, addr: u64) {
    let c = &mut *cpu;
    c.xmm[reg as usize].0[0] = (addr as *const u64).read_unaligned();
    c.xmm[reg as usize].0[1] = (addr as *const u64).add(1).read_unaligned();
}
#[no_mangle]
pub unsafe extern "C" fn gow_store_xmm(cpu: *mut GowCpu, reg: u32, addr: u64) {
    let c = &*cpu;
    // Validate the target: a wild address (not in any mapped region) means the
    // guest computed a bad pointer. Log the exact guest RVA and register state
    // instead of faulting, so the bug is visible.
    if !plausible_mem(addr, 16) {
        crate::glog!(
            "[gow.badstore] store_xmm reg={reg} addr=0x{addr:x} rva=0x{:x} rsp=0x{:x} rbp=0x{:x} rdi=0x{:x} rsi=0x{:x} rbx=0x{:x} rcx=0x{:x}",
            c.rip.wrapping_sub(c.image_base), c.gpr.rsp, c.gpr.rbp, c.gpr.rdi, c.gpr.rsi, c.gpr.rbx, c.gpr.rcx
        );
        return;
    }
    (addr as *mut u64).write_unaligned(c.xmm[reg as usize].0[0]);
    (addr as *mut u64).add(1).write_unaligned(c.xmm[reg as usize].0[1]);
}

/// Very loose check that an address range is plausibly writable: it must be in
/// the guest image, the guest heap/stack, or low user space — not a wild value
/// like 0xffffffff... or a small offset.
#[inline]
pub fn plausible_mem(addr: u64, len: u64) -> bool {
    if addr < 0x1000 { return false; }
    if addr.checked_add(len).is_none() { return false; }
    // Reject only the wild/negative canonical range: top 16 bits all ones
    // (0xffff_xxxx...) is the kernel/-1 range. Everything else (including the
    // Android 0xb400_... tagged stack/heap addresses and the guest image at
    // 0x1_40xx_xxxx) is allowed.
    let top = addr >> 48;
    if top == 0xffff { return false; }
    true
}

#[no_mangle]
pub unsafe extern "C" fn gow_pop64(cpu: *mut GowCpu) -> u64 {
    let c = &mut *cpu;
    let v = (c.gpr.rsp as *const u64).read_unaligned();
    c.gpr.rsp += 8;
    v
}
#[no_mangle]
pub unsafe extern "C" fn gow_push64(cpu: *mut GowCpu, v: u64) {
    let c = &mut *cpu;
    c.gpr.rsp -= 8;
    (c.gpr.rsp as *mut u64).write_unaligned(v);
}

// ---------------------------------------------------------------------------
// XMM minimal ops (used by lifted SSE)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movaps(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    c.xmm[dst as usize] = c.xmm[src as usize];
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pxor(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    c.xmm[dst as usize].0[0] ^= c.xmm[src as usize].0[0];
    c.xmm[dst as usize].0[1] ^= c.xmm[src as usize].0[1];
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pand(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    c.xmm[dst as usize].0[0] &= c.xmm[src as usize].0[0];
    c.xmm[dst as usize].0[1] &= c.xmm[src as usize].0[1];
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_por(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    c.xmm[dst as usize].0[0] |= c.xmm[src as usize].0[0];
    c.xmm[dst as usize].0[1] |= c.xmm[src as usize].0[1];
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_padd(cpu: *mut GowCpu, dst: i32, src: i32, size: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    match size {
        1 => { let (x, y) = (a[0].to_le_bytes(), b[0].to_le_bytes()); let mut o = [0u8;8]; for i in 0..8 { o[i] = x[i].wrapping_add(y[i]); } a[0]=u64::from_le_bytes(o); let (x,y)=(a[1].to_le_bytes(),b[1].to_le_bytes()); let mut o=[0u8;8]; for i in 0..8{o[i]=x[i].wrapping_add(y[i]);} a[1]=u64::from_le_bytes(o); }
        2 => { let (x, y) = (a[0].to_le_bytes(), b[0].to_le_bytes()); let mut o = [0u8;8]; for i in 0..4 { let xi=u16::from_le_bytes([x[i*2],x[i*2+1]]); let yi=u16::from_le_bytes([y[i*2],y[i*2+1]]); o[i*2..i*2+2].copy_from_slice(&xi.wrapping_add(yi).to_le_bytes()); } a[0]=u64::from_le_bytes(o); let (x,y)=(a[1].to_le_bytes(),b[1].to_le_bytes()); let mut o=[0u8;8]; for i in 0..4{let xi=u16::from_le_bytes([x[i*2],x[i*2+1]]);let yi=u16::from_le_bytes([y[i*2],y[i*2+1]]);o[i*2..i*2+2].copy_from_slice(&xi.wrapping_add(yi).to_le_bytes());} a[1]=u64::from_le_bytes(o); }
        4 => { a[0] = (a[0] as u32).wrapping_add(b[0] as u32) as u64 | (( (a[0]>>32) as u32).wrapping_add((b[0]>>32) as u32) as u64) << 32; a[1] = (a[1] as u32).wrapping_add(b[1] as u32) as u64 | (( (a[1]>>32) as u32).wrapping_add((b[1]>>32) as u32) as u64) << 32; }
        8 => { a[0] = a[0].wrapping_add(b[0]); a[1] = a[1].wrapping_add(b[1]); }
        _ => {}
    }
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_psub(cpu: *mut GowCpu, dst: i32, src: i32, size: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    match size {
        4 => { a[0] = (a[0] as u32).wrapping_sub(b[0] as u32) as u64 | (((a[0]>>32) as u32).wrapping_sub((b[0]>>32) as u32) as u64) << 32; a[1] = (a[1] as u32).wrapping_sub(b[1] as u32) as u64 | (((a[1]>>32) as u32).wrapping_sub((b[1]>>32) as u32) as u64) << 32; }
        8 => { a[0] = a[0].wrapping_sub(b[0]); a[1] = a[1].wrapping_sub(b[1]); }
        _ => {}
    }
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pcmpeq(cpu: *mut GowCpu, dst: i32, src: i32, size: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    match size {
        4 => { let m0 = if (a[0] as u32)==(b[0] as u32) {0xffff_ffffu64} else {0}; let m1 = if ((a[0]>>32) as u32)==((b[0]>>32) as u32) {0xffff_ffffu64} else {0}; a[0]=m0|(m1<<32); let m0 = if (a[1] as u32)==(b[1] as u32) {0xffff_ffffu64} else {0}; let m1 = if ((a[1]>>32) as u32)==((b[1]>>32) as u32) {0xffff_ffffu64} else {0}; a[1]=m0|(m1<<32); }
        _ => {}
    }
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_arith_ps(cpu: *mut GowCpu, dst: i32, src: i32, op: u32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0;
    let b = c.xmm[src as usize].0;
    let mut o = [0f32; 4];
    for i in 0..4 {
        let x = f32::from_bits((a[i / 2] >> (32 * (i % 2))) as u32);
        let y = f32::from_bits((b[i / 2] >> (32 * (i % 2))) as u32);
        let r = match op { 0 => x + y, 1 => x - y, 2 => x * y, 3 => x / y, _ => x };
        o[i] = r;
    }
    c.xmm[dst as usize].0[0] = o[0].to_bits() as u64 | (o[1].to_bits() as u64) << 32;
    c.xmm[dst as usize].0[1] = o[2].to_bits() as u64 | (o[3].to_bits() as u64) << 32;
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_arith_ss(cpu: *mut GowCpu, dst: i32, src: i32, op: u32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0[0] as u32;
    let b = c.xmm[src as usize].0[0] as u32;
    let x = f32::from_bits(a);
    let y = f32::from_bits(b);
    let r = match op { 0 => x + y, 1 => x - y, 2 => x * y, 3 => x / y, _ => x };
    c.xmm[dst as usize].0[0] = (c.xmm[dst as usize].0[0] & 0xffff_ffff_0000_0000) | r.to_bits() as u64;
}

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn gow_call(cpu: *mut GowCpu, target_rva: u32, return_rva: u32) {
    let c = &mut *cpu;
    gow_push64(cpu, return_rva as u64);
    c.next_rva = target_rva;
}

#[no_mangle]
pub unsafe extern "C" fn gow_jump(cpu: *mut GowCpu, target_rva: u32) {
    (*cpu).next_rva = target_rva;
}

/// Fallback for a `call` whose target is in-image but is not a known function
/// entry (computed target, thunk, or code the discovery pass did not list).
/// Records the miss and returns; it never silently executes the wrong code.
#[no_mangle]
pub unsafe extern "C" fn gow_trampoline(cpu: *mut GowCpu, target_rva: u32) {
    let c = &mut *cpu;
    c.missing_indirect_calls += 1;
    if c.missing_indirect_calls <= 64 {
        crate::glog!("[gow] trampoline miss -> rva=0x{target_rva:x} (no lifted entry)");
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_call_ptr(cpu: *mut GowCpu, target_va: u64) {
    gow_call_indirect(cpu, target_va);
}

#[no_mangle]
pub unsafe extern "C" fn gow_call_indirect(cpu: *mut GowCpu, target_va: u64) {
    let _c = &mut *cpu;
    // Host thunk (COM method / host-provided function): run it directly.
    if crate::hostcall::is_thunk(target_va) {
        crate::hostcall::call(target_va, cpu);
        return;
    }
    let c = &mut *cpu;
    if target_va >= c.image_base && target_va < c.image_base + c.image_size {
        c.next_rva = (target_va - c.image_base) as u32;
    } else {
        // Could be an import thunk (IAT slot pointing to a host stub) — handled by
        // gow_import_call from the lifted code; here record as unresolved.
        c.missing_indirect_calls += 1;
        c.next_rva = STOP_RVA;
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_ret(cpu: *mut GowCpu) {
    let c = &mut *cpu;
    let r = gow_pop64(cpu) as u32;
    // Sentinel pushed by gow_run when entering the outermost function ends the run.
    if r == STOP_RVA {
        c.next_rva = STOP_RVA;
        return;
    }
    c.next_rva = r;
}

#[no_mangle]
pub unsafe extern "C" fn gow_unimpl(cpu: *mut GowCpu, rva: u32, mnemonic: u32) {
    let c = &mut *cpu;
    c.unimpl_hits += 1;
    if c.unimpl_hits <= 64 {
        crate::glog!("[gow] UNIMPLEMENTED at rva=0x{rva:x} mnemonic={mnemonic}");
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_import_call(cpu: *mut GowCpu, iat_rva: u32) {
    // The recompiler emits the guest's `call [IAT]` as this direct host call, so
    // control continues at the next generated instruction. Just run the handler.
    let _ = match IMPORT_RESOLVER.get() {
        Some(res) => res.as_ref().map(|r| r.call(cpu, iat_rva)).unwrap_or(false),
        None => false,
    };
}

/// TEB base for `gs:` accesses. The Windows TEB is a per-thread block; we keep a
/// zeroed region and hand its address out so `mov rax,[gs:0x30]` (the PEB/self
/// pointer slot) and similar reads return sane, non-faulting memory.
#[no_mangle]
pub unsafe extern "C" fn gow_teb_base(cpu: *mut GowCpu) -> u64 {
    (*cpu).teb
}

/// FS base (x64 stack canary is at fs:[0x28]); reuse the same block.
#[no_mangle]
pub unsafe extern "C" fn gow_fs_base(cpu: *mut GowCpu) -> u64 {
    (*cpu).teb
}

static IMPORT_RESOLVER: std::sync::OnceLock<
    Option<std::sync::Arc<crate::win32::ImportResolver>>,
> = std::sync::OnceLock::new();

pub fn set_import_resolver(r: std::sync::Arc<crate::win32::ImportResolver>) {
    let _ = IMPORT_RESOLVER.set(Some(r));
}

// ---------------------------------------------------------------------------
// C ABI shims for host-side end-to-end tests (ctest/flow_runner.c)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn gow_cpu_new(image_base: u64, image_size: u64) -> *mut GowCpu {
    // Box keeps the image buffer alive for the CPU's lifetime.
    Box::into_raw(GowCpu::new(image_base, image_size as usize))
}

#[no_mangle]
pub unsafe extern "C" fn gow_register_fn(cpu: *mut GowCpu, rva: u32, f: GowFunc) {
    (*cpu).func_map.insert(rva, f);
}

#[no_mangle]
pub unsafe extern "C" fn gow_run_c(cpu: usize, entry_rva: u32) {
    let c = cpu as *mut GowCpu;
    // Route through gow_run but using the cpu's own func_map (already filled).
    (*c).call_depth = 1;
    gow_push64(c, STOP_RVA as u64);
    (*c).next_rva = entry_rva;
    let mut map_tmp: HashMap<u32, GowFunc> = std::mem::take(&mut (*c).func_map);
    std::mem::swap(&mut map_tmp, &mut (*c).func_map);
    gow_loop(c);
}

unsafe fn gow_loop(c: *mut GowCpu) {
    let cc = &mut *c;
    let mut steps: u64 = 0;
    while cc.next_rva != STOP_RVA {
        let rva = cc.next_rva;
        cc.rip = cc.image_base + rva as u64;
        cc.next_rva = STOP_RVA;
        if let Some(f) = cc.func_map.get(&rva).copied() {
            f(c);
        } else {
            cc.unimpl_hits += 1;
        }
        steps += 1;
        if steps > 5_000_000_000 { break; }
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_get_gpr(cpu: *mut GowCpu, idx: i32) -> u64 {
    let g = (*cpu).gpr;
    match idx { 0=>g.rax,1=>g.rcx,2=>g.rdx,3=>g.rbx,4=>g.rsp,5=>g.rbp,6=>g.rsi,7=>g.rdi,_=>0 }
}
#[no_mangle]
pub unsafe extern "C" fn gow_set_gpr(cpu: *mut GowCpu, idx: i32, v: u64) {
    let c = &mut *cpu;
    match idx { 0=>c.gpr.rax=v,1=>c.gpr.rcx=v,2=>c.gpr.rdx=v,3=>c.gpr.rbx=v,4=>c.gpr.rsp=v,5=>c.gpr.rbp=v,6=>c.gpr.rsi=v,7=>c.gpr.rdi=v,_=>{} }
}

#[no_mangle]
pub unsafe extern "C" fn gow_shift(cpu: *mut GowCpu, v: u64, n: u64, size_bits: u32, op: u32) -> u64 {
    let bits = size_bits.clamp(8, 64);
    let w = bits as u64;
    let n = n % 64;
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let r = match op {
        0 => (v << n) & mask,                        // shl
        1 => (v & mask) >> n.min(w - 1),             // shr
        2 => {                                       // sar
            let sv = if bits == 32 { (v as u32 as i32) as i64 } else if bits == 16 { (v as u16 as i16) as i64 } else if bits == 8 { (v as u8 as i8) as i64 } else { v as i64 };
            ((sv >> n.min(63)) as u64) & mask
        }
        3 => {                                       // rol
            let nn = n % w.max(1);
            ((v << nn) | (v >> (w - nn).max(0))) & mask
        }
        4 => {                                       // ror
            let nn = n % w.max(1);
            ((v >> nn) | (v << (w - nn).max(0))) & mask
        }
        _ => v,
    };
    let c = &mut *cpu;
    let _ = c;
    r
}

#[no_mangle]
pub unsafe extern "C" fn gow_bt(cpu: *mut GowCpu, a: u64, i: u64, _size_bits: u32, op: u32) -> u64 {
    let bit = (a >> (i % 64)) & 1;
    let c = &mut *cpu;
    c.flags = (c.flags & !CF) | (if bit != 0 { CF } else { 0 });
    match op {
        1 => a | (1u64 << (i % 64)),          // bts
        2 => a & !(1u64 << (i % 64)),         // btr
        3 => a ^ (1u64 << (i % 64)),          // btc
        _ => a,
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_bsf(cpu: *mut GowCpu, v: u64) -> u64 {
    let c = &mut *cpu;
    if v == 0 {
        c.flags |= ZF;
        0
    } else {
        c.flags &= !ZF;
        v.trailing_zeros() as u64
    }
}
#[no_mangle]
pub unsafe extern "C" fn gow_bsr(cpu: *mut GowCpu, v: u64) -> u64 {
    let c = &mut *cpu;
    if v == 0 {
        c.flags |= ZF;
        0
    } else {
        c.flags &= !ZF;
        63 - v.leading_zeros() as u64
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_stos(cpu: *mut GowCpu, size: u32) {
    let c = &mut *cpu;
    let dir = c.flags & DF != 0;
    let step = (size as u64) * if dir { u64::MAX } else { 1 };
    let src = if size == 1 { c.gpr.rax & 0xff } else if size == 2 { c.gpr.rax & 0xffff } else if size == 4 { c.gpr.rax & 0xffff_ffff } else { c.gpr.rax };
    gow_store(cpu, c.gpr.rdi, src, size);
    c.gpr.rdi = c.gpr.rdi.wrapping_add(step);
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_read_u32(cpu: *mut GowCpu, reg: i32, lane: u32) -> u64 {
    let c = &*cpu;
    let w = c.xmm[reg as usize].0[lane as usize / 2];
    (w >> (32 * (lane % 2))) & 0xffff_ffff
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_write_u32(cpu: *mut GowCpu, reg: i32, lane: u32, v: u64) {
    let c = &mut *cpu;
    let w = &mut c.xmm[reg as usize].0[lane as usize / 2];
    let sh = 32 * (lane % 2);
    *w = (*w & !(0xffff_ffffu64 << sh)) | ((v & 0xffff_ffff) << sh);
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_unpck(cpu: *mut GowCpu, dst: i32, src: i32, elem: u32, hi: u32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0;
    let b = c.xmm[src as usize].0;
    let mut out = [0u64; 2];
    for i in 0..2 {
        let ai = if hi != 0 { i + 2 } else { i };
        let av = c.xmm[dst as usize].0[ai / 2];
        let bv = b[(if hi != 0 { i + 2 } else { i }) / 2];
        let _ = (a, av, bv);
        out[i] = 0;
    }
    // For 32-bit lanes: dst = [a0,b0,a1,b1] (or a2,b2,a3,b3 for hi)
    let sel = |src: [u64; 2], lane: u32| -> u64 { (src[lane as usize / 2] >> (32 * (lane % 2))) & 0xffff_ffff };
    let base = if hi != 0 { 2 } else { 0 };
    let l0 = sel(a, base);
    let l1 = sel(b, base);
    let l2 = sel(a, base + 1);
    let l3 = sel(b, base + 1);
    out[0] = l0 | (l1 << 32);
    out[1] = l2 | (l3 << 32);
    let _ = elem;
    c.xmm[dst as usize].0 = out;
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_punpck(cpu: *mut GowCpu, dst: i32, src: i32, _elem: u32, _hi: u32) {
    gow_xmm_unpck(cpu, dst, src, 4, _hi);
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_shift(cpu: *mut GowCpu, dst: i32, bytes: u32, op: u32, size: u32) {
    let c = &mut *cpu;
    let mut data = [0u8; 16];
    for i in 0..16 {
        data[i] = ((c.xmm[dst as usize].0[i / 8] >> (8 * (i % 8))) & 0xff) as u8;
    }
    let mut out = [0u8; 16];
    let el = size.max(1) as usize;
    let n = (bytes as usize) * el;
    match op {
        0 => { /* psrl */
            for i in (n..16).rev() { out[i] = data[i - n]; } // byte-wise approx for sizes 1/2/4/8 below
            if el == 1 { out = [0u8;16]; for i in 0..16-n { out[i] = data[i+n]; } }
        }
        1 => { if el == 1 { out = [0u8;16]; for i in 0..16-n { out[i] = data[i+n]; } } }
        _ => {}
    }
    // Generic per-element shift for common cases
    match (op, el) {
        (0, 1) => { out = [0u8;16]; for i in 0..16 { out[i] = if i + n < 16 { data[i + n] } else { 0 }; } }
        (0, 2) => { for e in 0..8 { let v = u16::from_le_bytes([data[e*2], data[e*2+1]]); let r = if (n/2) >= 16 { 0 } else { v >> (n/2) }; out[e*2..e*2+2].copy_from_slice(&r.to_le_bytes()); } }
        (0, 4) => { for e in 0..4 { let v = u32::from_le_bytes([data[e*4],data[e*4+1],data[e*4+2],data[e*4+3]]); let r = if (n/4) >= 32 { 0 } else { v >> (n/4) }; out[e*4..e*4+4].copy_from_slice(&r.to_le_bytes()); } }
        (0, 8) => { for e in 0..2 { let off=e*8; let mut b=[0u8;8]; b.copy_from_slice(&data[off..off+8]); let v=u64::from_le_bytes(b); let r = if (n/8) >= 64 {0} else { v >> (n/8) }; out[off..off+8].copy_from_slice(&r.to_le_bytes()); } }
        (1, 2) => { for e in 0..8 { let v = u16::from_le_bytes([data[e*2], data[e*2+1]]) as i16; let r = (v >> (n/2)).min(0); let r = if (n/2) >= 16 { if v < 0 { -1i16 } else { 0 } } else { v >> (n/2) }; out[e*2..e*2+2].copy_from_slice(&r.to_le_bytes()); } }
        (1, 4) => { for e in 0..4 { let v = i32::from_le_bytes([data[e*4],data[e*4+1],data[e*4+2],data[e*4+3]]); let r = if (n/4) >= 32 { if v < 0 { -1i32 } else { 0 } } else { v >> (n/4) }; out[e*4..e*4+4].copy_from_slice(&r.to_le_bytes()); } }
        (2, 1) => { out = [0u8;16]; for i in 0..16 { out[i] = if i >= n { data[i-n] } else { 0 }; } }
        (2, 2) => { for e in 0..8 { let v = u16::from_le_bytes([data[e*2], data[e*2+1]]); let s = n/2; let r = if s >= 16 {0} else { v << s }; out[e*2..e*2+2].copy_from_slice(&r.to_le_bytes()); } }
        (2, 4) => { for e in 0..4 { let v = u32::from_le_bytes([data[e*4],data[e*4+1],data[e*4+2],data[e*4+3]]); let s = n/4; let r = if s >= 32 {0} else { v << s }; out[e*4..e*4+4].copy_from_slice(&r.to_le_bytes()); } }
        (2, 8) => { for e in 0..2 { let off=e*8; let mut b=[0u8;8]; b.copy_from_slice(&data[off..off+8]); let v=u64::from_le_bytes(b); let s=n/8; let r = if s>=64 {0} else { v << s }; out[off..off+8].copy_from_slice(&r.to_le_bytes()); } }
        _ => {}
    }
    for i in 0..2 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&out[i * 8..i * 8 + 8]);
        c.xmm[dst as usize].0[i] = u64::from_le_bytes(b);
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pshufd(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let s = c.xmm[src as usize].0;
    let l = |i: u32| -> u64 { (s[(i as usize) / 2] >> (32 * (i % 2))) & 0xffff_ffff };
    let r0 = l(imm & 3);
    let r1 = l((imm >> 2) & 3);
    let r2 = l((imm >> 4) & 3);
    let r3 = l((imm >> 6) & 3);
    c.xmm[dst as usize].0[0] = r0 | (r1 << 32);
    c.xmm[dst as usize].0[1] = r2 | (r3 << 32);
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pmovmskb(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let s = c.xmm[src as usize].0;
    let mut m = 0u64;
    for i in 0..16 {
        let byte = (s[i / 8] >> (8 * (i % 8))) & 0xff;
        if byte & 0x80 != 0 { m |= 1 << i; }
    }
    run_write_gpr(c, dst as usize, m);
}
unsafe fn run_write_gpr(c: &mut GowCpu, idx: usize, v: u64) {
    match idx { 0=>c.gpr.rax=v,1=>c.gpr.rcx=v,2=>c.gpr.rdx=v,3=>c.gpr.rbx=v,4=>c.gpr.rsp=v,5=>c.gpr.rbp=v,6=>c.gpr.rsi=v,7=>c.gpr.rdi=v,8=>c.gpr.r8=v,9=>c.gpr.r9=v,10=>c.gpr.r10=v,11=>c.gpr.r11=v,12=>c.gpr.r12=v,13=>c.gpr.r13=v,14=>c.gpr.r14=v,15=>c.gpr.r15=v,_=>{} }
}

#[no_mangle]
pub unsafe extern "C" fn gow_imul64(cpu: *mut GowCpu, a: u64, b: u64, size_bits: u32) -> u64 {
    let bits = size_bits.clamp(8, 64);
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let r = a.wrapping_mul(b) & mask;
    let c = &mut *cpu;
    c.flags &= !(CF | OF);
    // OF/CF set if the product does not fit in the destination width (signed).
    let sa = if bits == 64 { a as i64 } else { ((a << (64 - bits)) as i64) >> (64 - bits) };
    let sb = if bits == 64 { b as i64 } else { ((b << (64 - bits)) as i64) >> (64 - bits) };
    let p = (sa as i128) * (sb as i128);
    let lo = if bits == 64 { r as i64 } else { ((r << (64 - bits)) as i64) >> (64 - bits) };
    if p != lo as i128 { c.flags |= CF | OF; }
    r
}

#[no_mangle]
pub unsafe extern "C" fn gow_mul_rax_rdx(cpu: *mut GowCpu, b: u64, size_bits: u32) {
    let c = &mut *cpu;
    let a = c.gpr.rax;
    let p = (a as u128) * (b as u128);
    c.gpr.rax = p as u64;
    c.gpr.rdx = (p >> 64) as u64;
    let _ = size_bits;
}

#[no_mangle]
pub unsafe extern "C" fn gow_div_rax_rdx(cpu: *mut GowCpu, b: u64, size_bits: u32, signed_: u32) {
    let c = &mut *cpu;
    if signed_ != 0 {
        let num = ((c.gpr.rdx as i128) << 64) | c.gpr.rax as i128;
        let d = b as i64 as i128;
        if d != 0 {
            c.gpr.rax = (num / d) as u64;
            c.gpr.rdx = (num % d) as u64;
        } else { c.gpr.rax = 0; c.gpr.rdx = 0; }
    } else {
        let num = ((c.gpr.rdx as u128) << 64) | c.gpr.rax as u128;
        if b != 0 {
            c.gpr.rax = (num / b as u128) as u64;
            c.gpr.rdx = (num % b as u128) as u64;
        } else { c.gpr.rax = 0; c.gpr.rdx = 0; }
    }
    let _ = size_bits;
}

#[no_mangle]
pub unsafe extern "C" fn gow_div_sse(cpu: *mut GowCpu, a: i64, b: i64) -> i64 {
    let _ = cpu;
    if b == 0 { 0 } else { a / b }
}

#[no_mangle]
pub unsafe extern "C" fn gow_popfq(cpu: *mut GowCpu) -> u64 {
    let c = &mut *cpu;
    c.flags
}
#[no_mangle]
pub unsafe extern "C" fn gow_pushfq(cpu: *mut GowCpu) {
    let _ = cpu;
}

/// Lahf/Sahf: flags <-> AH.
#[no_mangle]
pub unsafe extern "C" fn gow_lahf(cpu: *mut GowCpu) {
    let c = &mut *cpu;
    let f = c.flags;
    let ah = ((f & 0x1) as u64)
        | (((f >> 2) & 1) << 2)
        | (((f >> 4) & 1) << 4)
        | (((f >> 6) & 1) << 6)
        | (((f >> 7) & 1) << 7)
        | 0x02;
    c.gpr.rax = (c.gpr.rax & !0xff00) | (ah << 8);
}
#[no_mangle]
pub unsafe extern "C" fn gow_sahf(cpu: *mut GowCpu) {
    let c = &mut *cpu;
    let ah = (c.gpr.rax >> 8) & 0xff;
    let mut f = c.flags;
    f = (f & !(CF | PF | AF | ZF | SF))
        | ((ah & 0x01) << 0)
        | ((ah & 0x04) >> 2 << 2)
        | ((ah & 0x10) >> 4 << 4)
        | ((ah & 0x40) >> 6 << 6)
        | ((ah & 0x80) >> 7 << 7);
    c.flags = f;
}
#[no_mangle]
pub unsafe extern "C" fn gow_cpuid(cpu: *mut GowCpu) {
    let c = &mut *cpu;
    match c.gpr.rax as u32 {
        0 => { c.gpr.rax = 0x16; c.gpr.rbx = 0x756e6547; c.gpr.rdx = 0x49656e69; c.gpr.rcx = 0x6c65746e; }
        1 => { c.gpr.rax = 0x000906e9; c.gpr.rbx = 0; c.gpr.rcx = 0x7ffafbff; c.gpr.rdx = 0x1f8bfbff; }
        7 => { c.gpr.rax = 0; c.gpr.rbx = 0x00000209; c.gpr.rcx = 0; c.gpr.rdx = 0; }
        _ => { c.gpr.rax = 0; c.gpr.rbx = 0; c.gpr.rcx = 0; c.gpr.rdx = 0; }
    }
}


// ---------------------------------------------------------------------------
// Additional SSE helpers
// ---------------------------------------------------------------------------

macro_rules! lanes32 {
    ($v:expr) => { [$v[0] as u32, ($v[0]>>32) as u32, $v[1] as u32, ($v[1]>>32) as u32] };
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pandn(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    a[0] = (!a[0]) & b[0];
    a[1] = (!a[1]) & b[1];
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pcmpgt(cpu: *mut GowCpu, dst: i32, src: i32, size: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    if size == 4 {
        let (x, y) = (lanes32!(*a), lanes32!(b));
        let mut out = [0u64; 2];
        let mut lo = [0u64; 4];
        for i in 0..4 { lo[i] = if (x[i] as i32) > (y[i] as i32) { 0xffff_ffff } else { 0 }; }
        out[0] = lo[0] | (lo[1] << 32); out[1] = lo[2] | (lo[3] << 32);
        *a = out;
    } else if size == 2 {
        let mut out = [0u64; 2];
        for w in 0..2 {
            let xb = a[w].to_le_bytes(); let yb = b[w].to_le_bytes();
            let mut o = [0u8; 8];
            for e in 0..4 {
                let x = i16::from_le_bytes([xb[e*2], xb[e*2+1]]);
                let y = i16::from_le_bytes([yb[e*2], yb[e*2+1]]);
                let r: u16 = if x > y { 0xffff } else { 0 };
                o[e*2..e*2+2].copy_from_slice(&r.to_le_bytes());
            }
            out[w] = u64::from_le_bytes(o);
        }
        *a = out;
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pmax(cpu: *mut GowCpu, dst: i32, src: i32, size: u32, signed_: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    if size == 1 {
        let (x, y) = (a[0].to_le_bytes(), b[0].to_le_bytes());
        let (x1, y1) = (a[1].to_le_bytes(), b[1].to_le_bytes());
        let mut o = [0u8; 8]; let mut o1 = [0u8; 8];
        for i in 0..8 {
            o[i] = if signed_ != 0 { (x[i] as i8).max(y[i] as i8) as u8 } else { x[i].max(y[i]) };
            o1[i] = if signed_ != 0 { (x1[i] as i8).max(y1[i] as i8) as u8 } else { x1[i].max(y1[i]) };
        }
        a[0] = u64::from_le_bytes(o); a[1] = u64::from_le_bytes(o1);
    } else if size == 4 {
        let (x, y) = (lanes32!(*a), lanes32!(b));
        let mut lo = [0u64; 4];
        for i in 0..4 { lo[i] = if signed_ != 0 { (x[i] as i32).max(y[i] as i32) as u32 as u64 } else { x[i].max(y[i]) as u64 }; }
        a[0] = lo[0] | (lo[1] << 32); a[1] = lo[2] | (lo[3] << 32);
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_arith_pd(cpu: *mut GowCpu, dst: i32, src: i32, op: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    let mut out = [0u64; 2];
    for i in 0..2 {
        let x = f64::from_bits(a[i]);
        let y = f64::from_bits(b[i]);
        let r = match op { 0 => x + y, 1 => x - y, 2 => x * y, 3 => x / y, _ => x };
        out[i] = r.to_bits();
    }
    *a = out;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_arith_sd(cpu: *mut GowCpu, dst: i32, src: i32, op: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0[0];
    {
        let a = &mut c.xmm[dst as usize].0;
        let x = f64::from_bits(a[0]);
        let y = f64::from_bits(b);
        let r = match op { 0 => x + y, 1 => x - y, 2 => x * y, 3 => x / y, _ => x };
        a[0] = r.to_bits();
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_sqrt(cpu: *mut GowCpu, dst: i32, is_double: u32, scalar: u32) {
    let c = &mut *cpu;
    let a = &mut c.xmm[dst as usize].0;
    if is_double != 0 {
        for i in 0..(if scalar != 0 { 1 } else { 2 }) { a[i] = f64::from_bits(a[i]).sqrt().to_bits(); }
    } else {
        let lanes = lanes32!(*a);
        let mut out = [0u64; 4];
        for i in 0..(if scalar != 0 { 1 } else { 4 }) { out[i] = f32::from_bits(lanes[i]).sqrt().to_bits() as u64; }
        a[0] = out[0] | (out[1] << 32); a[1] = out[2] | (out[3] << 32);
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_rsqrt(cpu: *mut GowCpu, dst: i32) {
    let c = &mut *cpu;
    let lanes = lanes32!(c.xmm[dst as usize].0);
    let mut o = [0u64; 4];
    for i in 0..4 { o[i] = (1.0f32 / (f32::from_bits(lanes[i])).sqrt()).to_bits() as u64; }
    c.xmm[dst as usize].0[0] = o[0] | (o[1] << 32);
    c.xmm[dst as usize].0[1] = o[2] | (o[3] << 32);
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_rcp(cpu: *mut GowCpu, dst: i32) {
    let c = &mut *cpu;
    let lanes = lanes32!(c.xmm[dst as usize].0);
    let mut o = [0u64; 4];
    for i in 0..4 { o[i] = (1.0f32 / f32::from_bits(lanes[i])).to_bits() as u64; }
    c.xmm[dst as usize].0[0] = o[0] | (o[1] << 32);
    c.xmm[dst as usize].0[1] = o[2] | (o[3] << 32);
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_minmax(cpu: *mut GowCpu, dst: i32, src: i32, max: u32, mode: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    match mode {
        0 => {
            let (x, y) = (lanes32!(*a), lanes32!(b));
            let mut o = [0u64; 4];
            for i in 0..4 {
                let (p, q) = (f32::from_bits(x[i]), f32::from_bits(y[i]));
                o[i] = (if max != 0 { p.max(q) } else { p.min(q) }).to_bits() as u64;
            }
            a[0] = o[0] | (o[1] << 32); a[1] = o[2] | (o[3] << 32);
        }
        1 => {
            let (p, q) = (f32::from_bits(a[0] as u32), f32::from_bits(b[0] as u32));
            let r = if max != 0 { p.max(q) } else { p.min(q) };
            a[0] = (a[0] & 0xffff_ffff_0000_0000) | r.to_bits() as u64;
        }
        _ => {
            if mode == 3 {
                // packed double (minpd/maxpd)
                let mut o = [0u64; 2];
                for i in 0..2 {
                    let p = f64::from_bits(a[i]);
                    let q = f64::from_bits(b[i]);
                    o[i] = (if max != 0 { p.max(q) } else { p.min(q) }).to_bits();
                }
                *a = o;
            } else {
                let (p, q) = (f64::from_bits(a[0]), f64::from_bits(b[0]));
                a[0] = (if max != 0 { p.max(q) } else { p.min(q) }).to_bits();
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movhl(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let s = c.xmm[src as usize].0[1];
    c.xmm[dst as usize].0[0] = s;
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movlh(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let s = c.xmm[src as usize].0[0];
    c.xmm[dst as usize].0[1] = s;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_shufps(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let a = lanes32!(c.xmm[dst as usize].0);
    let b = lanes32!(c.xmm[src as usize].0);
    let pick = |sel: u32| -> u32 { if sel < 4 { a[sel as usize] } else { b[(sel - 4) as usize] } };
    let r0 = pick(imm & 3); let r1 = pick((imm >> 2) & 3);
    let r2 = pick((imm >> 4) & 3); let r3 = pick((imm >> 6) & 3);
    c.xmm[dst as usize].0[0] = r0 as u64 | (r1 as u64) << 32;
    c.xmm[dst as usize].0[1] = r2 as u64 | (r3 as u64) << 32;
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_shufpd(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0;
    let b = c.xmm[src as usize].0;
    let r0 = if imm & 1 == 0 { a[0] } else { a[1] };
    let r1 = if imm & 2 == 0 { b[0] } else { b[1] };
    c.xmm[dst as usize].0 = [r0, r1];
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movmsk(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let a = c.xmm[src as usize].0;
    let lanes = lanes32!(a);
    let mut m = 0u64;
    for i in 0..4 { if lanes[i] & 0x8000_0000 != 0 { m |= 1 << i; } }
    run_write_gpr(c, dst as usize, m);
}


// ---------------------------------------------------------------------------
// SSE compare / convert / movd etc.
// ---------------------------------------------------------------------------

/// COMISS/COMISD/UCOMISS/UCOMISD: set ZF/PF/CF from unordered/equal/less.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_comis(cpu: *mut GowCpu, dst: i32, src: i32, is_double: u32) {
    let c = &mut *cpu;
    let (a, b) = if is_double != 0 {
        (f64::from_bits(c.xmm[dst as usize].0[0]), f64::from_bits(c.xmm[src as usize].0[0]))
    } else {
        (f32::from_bits(c.xmm[dst as usize].0[0] as u32) as f64, f32::from_bits(c.xmm[src as usize].0[0] as u32) as f64)
    };
    let mut f = c.flags & !(CF | PF | ZF | OF | SF | AF);
    if a.is_nan() || b.is_nan() {
        f |= CF | PF | ZF;
    } else if a == b {
        f |= ZF;
    } else if a < b {
        f |= CF;
    }
    c.flags = f;
}

/// CMPccPS/SS/PD/SD: pred = 0 EQ,1 LT,2 LE,3 UNORD,4 NEQ,5 NLT,6 NLE,7 ORD
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_cmpps(cpu: *mut GowCpu, dst: i32, src: i32, pred: u32, kind: u32) {
    // kind: 0=ps 1=ss 2=pd 3=sd
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    let cmp = |x: f64, y: f64| -> bool {
        match pred {
            0 => x == y, 1 => x < y, 2 => x <= y, 3 => x.is_nan() || y.is_nan(),
            4 => x != y, 5 => !(x < y), 6 => !(x <= y), 7 => !(x.is_nan() || y.is_nan()),
            _ => false,
        }
    };
    match kind {
        0 => { let x = lanes32!(*a); let y = lanes32!(b); let mut o = [0u64;4];
               for i in 0..4 { o[i] = if cmp(f32::from_bits(x[i]) as f64, f32::from_bits(y[i]) as f64) {0xffff_ffff} else {0}; }
               a[0] = o[0] | (o[1]<<32); a[1] = o[2] | (o[3]<<32); }
        1 => { let x = f32::from_bits(a[0] as u32); let y = f32::from_bits(b[0] as u32);
               let r: u64 = if cmp(x as f64, y as f64) {0xffff_ffff} else {0};
               a[0] = (a[0] & 0xffff_ffff_0000_0000) | r; }
        2 => { let mut o = [0u64;2];
               for i in 0..2 { o[i] = if cmp(f64::from_bits(a[i]), f64::from_bits(b[i])) {u64::MAX} else {0}; }
               *a = o; }
        _ => { let x = f64::from_bits(a[0]); let y = f64::from_bits(b[0]);
               a[0] = if cmp(x, y) {u64::MAX} else {0}; }
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_andn(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    a[0] = (!a[0]) & b[0];
    a[1] = (!a[1]) & b[1];
}

/// movd/movq between GPR and XMM. dir=0: GPR->XMM, 1: XMM->GPR.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movd(cpu: *mut GowCpu, xreg: i32, dir: u32, movq: u32) {
    let c = &mut *cpu;
    let mask = if movq != 0 { u64::MAX } else { 0xffff_ffff };
    if dir == 0 {
        let v = c.gpr.rax; // unused; value passed via store below
        let _ = v;
    }
    let _ = mask;
    // implemented through the two dedicated helpers below
    let _ = (c, xreg);
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_from_gpr(cpu: *mut GowCpu, xreg: i32, val: u64, movq: u32) {
    let c = &mut *cpu;
    if movq != 0 { c.xmm[xreg as usize].0 = [val, 0]; }
    else { c.xmm[xreg as usize].0[0] = (c.xmm[xreg as usize].0[0] & 0xffff_ffff_0000_0000) | (val & 0xffff_ffff); }
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_to_gpr(cpu: *mut GowCpu, xreg: i32, movq: u32, dst_idx: u32) {
    let c = &mut *cpu;
    let v = if movq != 0 { c.xmm[xreg as usize].0[0] } else { c.xmm[xreg as usize].0[0] & 0xffff_ffff };
    run_write_gpr_w(c, dst_idx, v, movq);
}
unsafe fn run_write_gpr_w(c: &mut GowCpu, idx: u32, v: u64, w64: u32) {
    let vv = if w64 != 0 { v } else { v & 0xffff_ffff };
    match idx { 0=>c.gpr.rax=vv,1=>c.gpr.rcx=vv,2=>c.gpr.rdx=vv,3=>c.gpr.rbx=vv,4=>c.gpr.rsp=vv,5=>c.gpr.rbp=vv,6=>c.gpr.rsi=vv,7=>c.gpr.rdi=vv,8=>c.gpr.r8=vv,9=>c.gpr.r9=vv,10=>c.gpr.r10=vv,11=>c.gpr.r11=vv,12=>c.gpr.r12=vv,13=>c.gpr.r13=vv,14=>c.gpr.r14=vv,15=>c.gpr.r15=vv,_=>{} }
}

/// Conversions from an XMM source.
/// op: 0=cvtdq2ps 1=cvtdq2pd 2=cvtpd2ps 3=cvtps2pd 4=cvtss2sd 5=cvtsd2ss
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_cvt_xx(cpu: *mut GowCpu, dst: i32, src: i32, op: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    match op {
        0 => { let x = lanes32!(b); let mut o=[0u64;4]; for i in 0..4 { o[i]=(x[i] as i32 as f32).to_bits() as u64; } a[0]=o[0]|(o[1]<<32); a[1]=o[2]|(o[3]<<32); }
        1 => { let x = lanes32!(b); a[0]=(x[0] as i32 as f64).to_bits(); a[1]=(x[1] as i32 as f64).to_bits(); }
        2 => { a[0]=((f64::from_bits(b[0]) as f32).to_bits() as u64)|(((f64::from_bits(b[1]) as f32).to_bits() as u64)<<32); a[1]=0; }
        3 => { let x = lanes32!(b); a[0]=(f32::from_bits(x[0]) as f64).to_bits(); a[1]=(f32::from_bits(x[1]) as f64).to_bits(); }
        4 => { a[0]=(f32::from_bits(b[0] as u32) as f64).to_bits(); }
        5 => { a[0]=(a[0] & 0xffff_ffff_0000_0000)|((f64::from_bits(b[0]) as f32).to_bits() as u64); }
        6 | 7 => { let x = lanes32!(b); let mut o=[0u64;4]; for i in 0..4 { let f=f32::from_bits(x[i]); let v= if op==7 {f.trunc()} else {f.round()} as i64 as i32 as u32; o[i]=v as u64; } a[0]=o[0]|(o[1]<<32); a[1]=o[2]|(o[3]<<32); }
        8 | 9 => { let mut o=[0u64;4]; for i in 0..2 { let f=f64::from_bits(b[i]); let v= if op==9 {f.trunc()} else {f.round()} as i64 as i32 as u32; o[i]=v as u64; } a[0]=o[0]|(o[1]<<32); a[1]=0; }
        _ => {}
    }
}

/// GPR/mem -> XMM: op 0=cvtsi2ss 1=cvtsi2sd
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_cvt_g2x(cpu: *mut GowCpu, dst: i32, op: u32, val: u64) {
    let c = &mut *cpu;
    let a = &mut c.xmm[dst as usize].0;
    match op {
        0 => { a[0] = (a[0] & 0xffff_ffff_0000_0000) | ((val as i32 as f32).to_bits() as u64); }
        1 => { a[0] = (val as i64 as f64).to_bits(); }
        _ => {}
    }
}

/// XMM -> GPR: op 0=cvtss2si 1=cvtsd2si 2=cvttss2si 3=cvttsd2si
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_cvt_x2g(cpu: *mut GowCpu, src: i32, op: u32) -> u64 {
    let c = &*cpu;
    let v = c.xmm[src as usize].0[0];
    match op {
        0 => f32::from_bits(v as u32).round() as i64 as u64,
        1 => f64::from_bits(v).round() as i64 as u64,
        2 => f32::from_bits(v as u32).trunc() as i64 as u64,
        3 => f64::from_bits(v).trunc() as i64 as u64,
        _ => 0,
    }
}


#[no_mangle]
pub unsafe extern "C" fn gow_xmm_to_gpr_val(cpu: *mut GowCpu, dst: i32, v: u64) {
    run_write_gpr_w(&mut *cpu, dst as u32, v, 0);
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_lane0(cpu: *mut GowCpu, reg: i32) -> u64 {
    (*cpu).xmm[reg as usize].0[0]
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movd_reg(cpu: *mut GowCpu, dst: i32, src: i32, movq: u32) {
    let c = &mut *cpu;
    if movq != 0 { c.xmm[dst as usize].0 = [c.xmm[src as usize].0[0], 0]; }
    else { c.xmm[dst as usize].0[0] = (c.xmm[dst as usize].0[0] & 0xffff_ffff_0000_0000) | (c.xmm[src as usize].0[0] & 0xffff_ffff); }
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movmsk_val(cpu: *mut GowCpu, src: i32) -> u64 {
    let c = &*cpu;
    let v = c.xmm[src as usize].0[0];
    let lanes = [v as u32, (v>>32) as u32];
    let mut m = 0u64;
    for i in 0..2 { if lanes[i] & 0x8000_0000 != 0 { m |= 1 << i; } }
    m
}

#[no_mangle]
pub unsafe extern "C" fn gow_ldmxcsr(cpu: *mut GowCpu, addr: u64) {
    (*cpu).mxcsr = *(addr as *const u32);
}
#[no_mangle]
pub unsafe extern "C" fn gow_stmxcsr(cpu: *mut GowCpu, addr: u64) {
    *(addr as *mut u32) = (*cpu).mxcsr;
}

#[no_mangle]
pub unsafe extern "C" fn gow_movs(cpu: *mut GowCpu, size: u32) {
    let c = &mut *cpu;
    let dir = c.flags & DF != 0;
    let step = (size as u64) * if dir { u64::MAX } else { 1 };
    let v = gow_load(cpu, c.gpr.rsi, size);
    gow_store(cpu, c.gpr.rdi, v, size);
    c.gpr.rsi = c.gpr.rsi.wrapping_add(step);
    c.gpr.rdi = c.gpr.rdi.wrapping_add(step);
}
#[no_mangle]
pub unsafe extern "C" fn gow_lods(cpu: *mut GowCpu, size: u32) {
    let c = &mut *cpu;
    let dir = c.flags & DF != 0;
    let step = (size as u64) * if dir { u64::MAX } else { 1 };
    let v = gow_load(cpu, c.gpr.rsi, size);
    let mask = if size == 8 { u64::MAX } else { (1u64 << (size*8)) - 1 };
    c.gpr.rax = (c.gpr.rax & !mask) | (v & mask);
    c.gpr.rsi = c.gpr.rsi.wrapping_add(step);
}
#[no_mangle]
pub unsafe extern "C" fn gow_scas(cpu: *mut GowCpu, size: u32) {
    let c = &mut *cpu;
    let dir = c.flags & DF != 0;
    let step = (size as u64) * if dir { u64::MAX } else { 1 };
    let v = gow_load(cpu, c.gpr.rdi, size);
    let a = if size == 8 { c.gpr.rax } else { c.gpr.rax & ((1u64 << (size*8)) - 1) };
    gow_sub64(cpu, a, v, size * 8);
    c.gpr.rdi = c.gpr.rdi.wrapping_add(step);
}
#[no_mangle]
pub unsafe extern "C" fn gow_cmps(cpu: *mut GowCpu, size: u32) {
    let c = &mut *cpu;
    let dir = c.flags & DF != 0;
    let step = (size as u64) * if dir { u64::MAX } else { 1 };
    let a = gow_load(cpu, c.gpr.rsi, size);
    let b = gow_load(cpu, c.gpr.rdi, size);
    gow_sub64(cpu, a, b, size * 8);
    c.gpr.rsi = c.gpr.rsi.wrapping_add(step);
    c.gpr.rdi = c.gpr.rdi.wrapping_add(step);
}
#[no_mangle]
pub unsafe extern "C" fn gow_enter(cpu: *mut GowCpu, frame: u64, level: u64) {
    let c = &mut *cpu;
    gow_push64(cpu, c.gpr.rbp);
    let sp = c.gpr.rsp;
    if level > 0 {
        for _ in 1..level { c.gpr.rbp = c.gpr.rbp.wrapping_sub(8); gow_push64(cpu, gow_load(cpu, c.gpr.rbp, 8)); }
        gow_push64(cpu, sp);
    }
    c.gpr.rbp = sp;
    c.gpr.rsp = c.gpr.rsp.wrapping_sub(frame);
}

/// Central dispatch loop. Runs until next_rva == STOP_RVA.
#[no_mangle]
pub unsafe extern "C" fn gow_run(cpu: *mut GowCpu, entry_rva: u32) {
    // NOTE: don't install a SIGSEGV handler here — ART wraps sigaction and
    // re-raising SIG_DFL kills the process mid-run. Diagnostics come from the
    // gow.badstore/badload validators instead.
    crate::crashlog::set_current_cpu(cpu);
    let c = &mut *cpu;
    let map: HashMap<u32, GowFunc> = registry().iter().cloned().collect();
    c.func_map = map;
    // Push a sentinel so the outermost `ret` ends the dispatch loop.
    gow_push64(cpu, STOP_RVA as u64);
    c.next_rva = entry_rva;
    let mut steps: u64 = 0;
    while c.next_rva != STOP_RVA {
        let rva = c.next_rva;
        c.rip = c.image_base + rva as u64;
        c.next_rva = STOP_RVA;
        match c.func_map.get(&rva).copied() {
            Some(f) => f(cpu),
            None => {
                // in-image target without a lifted function: try reading the first
                // bytes; if it is not a known entry we stop loudly.
                c.unimpl_hits += 1;
            }
        }
        steps += 1;
        if steps > 5_000_000_000 {
            crate::glog!("[gow] step budget exhausted");
            break;
        }
    }
}
