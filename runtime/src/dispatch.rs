//! Dispatcher + guest memory for recompiled x86-64 code.
//!
//! Register file, guest address space (mapped at the original PE image base so
//! guest pointers are real process pointers), the RVA->function map, and the
//! control-flow helpers the generated C calls into. Win32/NT APIs and the
//! D3D11->Vulkan layer plug in on top of this.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

pub type GowFunc = unsafe extern "C" fn(*mut GowCpu);

/// Flags word (x86 RFLAGS subset: CF PF AF ZF SF OF DF).
pub const CF: u64 = 1 << 0;
pub const PF: u64 = 1 << 2;
pub const AF: u64 = 1 << 4;
pub const ZF: u64 = 1 << 6;
pub const SF: u64 = 1 << 7;
pub const OF: u64 = 1 << 11;
pub const DF: u64 = 1 << 10;
pub const FLAG_MASK: u64 = CF | PF | AF | ZF | SF | OF | DF;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Gprs {
    pub rax: u64, pub rcx: u64, pub rdx: u64, pub rbx: u64,
    pub rsp: u64, pub rbp: u64, pub rsi: u64, pub rdi: u64,
    pub r8: u64, pub r9: u64, pub r10: u64, pub r11: u64,
    pub r12: u64, pub r13: u64, pub r14: u64, pub r15: u64,
}

#[repr(C)]
pub struct GowCpu {
    pub gpr: Gprs,
    pub rip: u64,
    pub flags: u64,
    /// Guest linear address of the start of the guest heap/stack region.
    pub heap_base: u64,
    pub heap_size: u64,
    /// Host pointer to the mapped image (guest_base[i] == image_base + i).
    pub image: *mut u8,
    pub image_size: u64,
    pub image_base: u64,
    pub func_map: HashMap<u32, GowFunc>,
    /// Per-thread TEB pointer (guest VA) for gs:[ ] accesses.
    pub teb: u64,
    pub call_depth: u32,
    pub missing_indirect_calls: u64,
    pub unimpl_hits: u64,
}

unsafe impl Send for GowCpu {}

impl GowCpu {
    pub fn new(image_base: u64, image_size: usize) -> Box<Self> {
        // Anonymous mapping; on Android/Linux we later request a fixed mapping at
        // image_base via mmap(MAP_FIXED) to preserve guest pointer identity.
        let mut image = vec![0u8; image_size];
        Box::new(GowCpu {
            gpr: Gprs { rsp: 0x7fff_0000_0000, ..Default::default() },
            rip: 0,
            flags: 0,
            heap_base: 0x1_0000_0000,
            heap_size: 0,
            image: image.as_mut_ptr(),
            image_size: image_size as u64,
            image_base,
            func_map: HashMap::new(),
            teb: 0x7fff_8000_0000,
            call_depth: 0,
            missing_indirect_calls: 0,
            unimpl_hits: 0,
        })
    }
}

static UNIMPL_COUNT: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub unsafe extern "C" fn gow_unimpl(cpu: *mut GowCpu, rva: u32, mnemonic: u32) {
    let c = &mut *cpu;
    c.unimpl_hits += 1;
    let n = UNIMPL_COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 64 {
        eprintln!("[gow] UNIMPLEMENTED at rva=0x{rva:x} mnemonic={mnemonic} (hit #{n})");
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_load64(cpu: *mut GowCpu, addr: u64) -> u64 {
    let _ = cpu;
    (addr as *const u64).read_unaligned()
}

#[no_mangle]
pub unsafe extern "C" fn gow_store64(cpu: *mut GowCpu, addr: u64, v: u64) {
    let _ = cpu;
    (addr as *mut u64).write_unaligned(v);
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

#[no_mangle]
pub unsafe extern "C" fn gow_setflags(cpu: *mut GowCpu, a: u64, b: u64, r: u64, size_bits: u32) {
    let c = &mut *cpu;
    let bits = size_bits.min(64);
    let msb = 1u64 << (bits - 1);
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let sr = r & mask;
    let mut f = 0u64;
    if sr == 0 { f |= ZF; }
    if sr & msb != 0 { f |= SF; }
    f |= (sr.count_ones() & 1) as u64 ^ 1; // PF
    // CF/OF computed by caller through wrappers; conservative here.
    let _ = (a, b);
    c.flags = (c.flags & !FLAG_MASK) | (f & FLAG_MASK);
}

#[no_mangle]
pub unsafe extern "C" fn gow_call(cpu: *mut GowCpu, target_rva: u32, return_rva: u32) {
    let c = &mut *cpu;
    gow_push64(cpu, return_rva as u64);
    if let Some(&f) = c.func_map.get(&target_rva) {
        c.call_depth += 1;
        f(cpu);
        c.call_depth -= 1;
    } else if target_rva < c.image_size as u32 && c.image_base != 0 {
        // Stub: unresolved in-image target.
        UNIMPL_COUNT.fetch_add(1, Ordering::Relaxed);
    } else {
        c.missing_indirect_calls += 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_call_indirect(cpu: *mut GowCpu, target_rva: u32) {
    let c = &mut *cpu;
    if let Some(&f) = c.func_map.get(&target_rva) {
        f(cpu);
    } else {
        c.missing_indirect_calls += 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_jump(_cpu: *mut GowCpu, _target_rva: u32) {}
#[no_mangle]
pub unsafe extern "C" fn gow_ret(cpu: *mut GowCpu) {
    let _ = gow_pop64(cpu);
}
