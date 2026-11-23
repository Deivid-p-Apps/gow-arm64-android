//! x87 FPU emulation: an 8-slot register stack with the control/status words the
//! game's imports and a few lifted instructions touch. Only operations the
//! lifted code actually uses are implemented; the rest are recorded as misses.

use crate::dispatch::GowCpu;

/// x87 stack entry: value + tag (empty/valid).
#[derive(Clone, Copy)]
pub struct St80(pub f64, pub bool);

impl Default for St80 {
    fn default() -> Self { St80(0.0, false) }
}

#[repr(C)]
pub struct Fpu {
    pub st: [St80; 8],
    pub top: u8,
    pub cw: u16,
    pub sw: u16,
    pub tag: u16,
    pub tw: u16,
}

impl Default for Fpu {
    fn default() -> Self {
        Fpu { st: [St80::default(); 8], top: 0, cw: 0x037f, sw: 0, tag: 0xffff, tw: 0xffff }
    }
}

impl Fpu {
    fn idx(&self, i: u8) -> usize { ((self.top + i) & 7) as usize }

    pub fn push(&mut self, v: f64) {
        self.top = (self.top.wrapping_sub(1)) & 7;
        self.st[self.top as usize] = St80(v, true);
    }
    pub fn pop(&mut self) -> f64 {
        let v = self.st[self.top as usize].0;
        self.st[self.top as usize] = St80::default();
        self.top = (self.top + 1) & 7;
        v
    }
    pub fn get(&self, i: u8) -> f64 { self.st[self.idx(i)].0 }
    pub fn set(&mut self, i: u8, v: f64) { let k = self.idx(i); self.st[k] = St80(v, true); }

    /// Update C0-C3 in the status word's condition bits from a comparison.
    pub fn set_cond(&mut self, c3: bool, c2: bool, c0: bool) {
        self.sw &= !0x4700;
        if c3 { self.sw |= 0x4000; }
        if c2 { self.sw |= 0x0400; }
        if c0 { self.sw |= 0x0100; }
    }
}

#[no_mangle]
pub unsafe extern "C" fn gow_x87_push(cpu: *mut GowCpu, v: f64) {
    (*cpu).fpu.push(v);
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_pop(cpu: *mut GowCpu) -> f64 {
    (*cpu).fpu.pop()
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_get(cpu: *mut GowCpu, i: u32) -> f64 {
    (*cpu).fpu.get(i as u8)
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_set(cpu: *mut GowCpu, i: u32, v: f64) {
    (*cpu).fpu.set(i as u8, v);
}
/// arith: 0=add 1=sub 2=mul 3=div 4=subr 5=divr (st0 op sti -> st0)
#[no_mangle]
pub unsafe extern "C" fn gow_x87_arith(cpu: *mut GowCpu, i: u32, op: u32, pop: u32) {
    let c = &mut *cpu;
    let a = c.fpu.get(0);
    let b = c.fpu.get(i as u8);
    let r = match op {
        0 => a + b,
        1 => a - b,
        2 => a * b,
        3 => a / b,
        4 => b - a,
        5 => b / a,
        _ => a,
    };
    if pop != 0 {
        c.fpu.pop();
        c.fpu.set(0, r);
    } else {
        c.fpu.set(0, r);
    }
}
/// arith with memory operand: st0 op m
#[no_mangle]
pub unsafe extern "C" fn gow_x87_arith_m(cpu: *mut GowCpu, m: f64, op: u32, pop: u32) {
    let c = &mut *cpu;
    let a = c.fpu.get(0);
    let r = match op { 0 => a + m, 1 => a - m, 2 => a * m, 3 => a / m, 4 => m - a, _ => m / a };
    if pop != 0 { c.fpu.pop(); c.fpu.set(0, r); } else { c.fpu.set(0, r); }
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_ld(cpu: *mut GowCpu, m: f64, is_f32: u32) {
    let _ = is_f32;
    (*cpu).fpu.push(m);
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_com_m(cpu: *mut GowCpu, m: f64, pop: u32) {
    let c = &mut *cpu;
    let a = c.fpu.get(0);
    if a.is_nan() || m.is_nan() { c.fpu.set_cond(true, true, true); }
    else if a > m { c.fpu.set_cond(false, false, false); }
    else if a < m { c.fpu.set_cond(false, false, true); }
    else { c.fpu.set_cond(true, false, false); }
    if pop != 0 { c.fpu.pop(); }
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_init(cpu: *mut GowCpu) {
    (*cpu).fpu = Fpu::default();
}

/// compare st0 with sti: sets C3/C2/C0 like FCOM.
#[no_mangle]
pub unsafe extern "C" fn gow_x87_com(cpu: *mut GowCpu, i: u32, pop: u32) {
    let c = &mut *cpu;
    let a = c.fpu.get(0);
    let b = c.fpu.get(i as u8);
    if a.is_nan() || b.is_nan() { c.fpu.set_cond(true, true, true); }
    else if a > b { c.fpu.set_cond(false, false, false); }
    else if a < b { c.fpu.set_cond(false, false, true); }
    else { c.fpu.set_cond(true, false, false); }
    if pop != 0 { c.fpu.pop(); }
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fucomi(cpu: *mut GowCpu, i: u32, pop: u32) {
    let c = &mut *cpu;
    let a = c.fpu.get(0);
    let b = c.fpu.get(i as u8);
    c.flags &= !(crate::dispatch::CF | crate::dispatch::ZF | crate::dispatch::PF);
    if a.is_nan() || b.is_nan() { c.flags |= crate::dispatch::CF | crate::dispatch::ZF | crate::dispatch::PF; }
    else if a < b { c.flags |= crate::dispatch::CF; }
    else if a == b { c.flags |= crate::dispatch::ZF; }
    if pop != 0 { c.fpu.pop(); }
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fxch(cpu: *mut GowCpu, i: u32) {
    let c = &mut *cpu;
    let a = c.fpu.get(0);
    let b = c.fpu.get(i as u8);
    c.fpu.set(0, b);
    c.fpu.set(i as u8, a);
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fstsw(cpu: *mut GowCpu) -> u64 {
    (*cpu).fpu.sw as u64
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fldcw(cpu: *mut GowCpu, v: u64) {
    (*cpu).fpu.cw = v as u16;
}
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fnstcw(cpu: *mut GowCpu) -> u64 {
    (*cpu).fpu.cw as u64
}
// ---------------------------------------------------------------------------
// Integer memory operands (FILD/FIST/FISTP/FISTTP/FIADD../FICOMP)
// ---------------------------------------------------------------------------

/// Sign-extend `raw` (a value loaded from memory) to `size_bits` (16/32/64) and
/// convert to the f64 representation used by the stack.
fn i64_of(raw: u64, size_bits: u32) -> i64 {
    match size_bits {
        16 => raw as u16 as i16 as i64,
        32 => raw as u32 as i32 as i64,
        _ => raw as i64,
    }
}

/// FILD: push an integer memory operand (sign-extended) as a float.
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fild(cpu: *mut GowCpu, raw: u64, size_bits: u32) {
    (*cpu).fpu.push(i64_of(raw, size_bits) as f64);
}

/// Integer conversion used by FIADD/FIMUL/FISUB/R/FIDIV/R: never fails, so we
/// can reuse the existing `gow_x87_arith_m`.
#[no_mangle]
pub unsafe extern "C" fn gow_x87_i2f(_cpu: *mut GowCpu, raw: u64, size_bits: u32) -> f64 {
    i64_of(raw, size_bits) as f64
}

/// Round toward an integral value according to the FPU control word's RC field
/// (or truncate for FISTTP), then clamp to the destination integer size,
/// producing the integer indefinite value on overflow / NaN.
fn st0_to_int(c: &GowCpu, size_bits: u32, trunc: u32) -> i64 {
    let v = c.fpu.get(0);
    let r = if trunc != 0 {
        v.trunc()
    } else {
        match (c.fpu.cw >> 10) & 3 {
            0 => v.round_ties_even(),
            1 => v.floor(),
            2 => v.ceil(),
            _ => v.trunc(),
        }
    };
    match size_bits {
        16 => if r >= -32768.0 && r <= 32767.0 { r as i16 as i64 } else { i16::MIN as i64 },
        32 => if r >= -2147483648.0 && r <= 2147483647.0 { r as i32 as i64 } else { i32::MIN as i64 },
        _ => if r >= -9223372036854775808.0 && r < 9223372036854775808.0 { r as i64 } else { i64::MIN },
    }
}

/// FIST/FISTP (trunc=0) and FISTTP (trunc=1): return ST(0) rounded to an
/// integer of `size_bits`; pop the stack when `pop` is non-zero. The caller
/// stores the low bytes of the returned value.
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fist(cpu: *mut GowCpu, size_bits: u32, pop: u32, trunc: u32) -> i64 {
    let c = &mut *cpu;
    let r = st0_to_int(c, size_bits, trunc);
    if pop != 0 {
        c.fpu.pop();
    }
    r
}

/// FCMOVcc: if the condition (x86 cc numbering, 0..15) holds, ST(0) := ST(i).
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fcmov(cpu: *mut GowCpu, i: u32, cc: u32) {
    if crate::dispatch::gow_test_cc(cpu, cc) != 0 {
        let c = &mut *cpu;
        let v = c.fpu.get(i as u8);
        c.fpu.set(0, v);
    }
}

// ---------------------------------------------------------------------------
// x87 environment / state (FNSTENV/FLDENV/FRSTOR)
//
// The environment is stored in the standard 28-byte protected-mode layout:
//   0: FCW, 4: FSW, 8: FTW, 12: FIP(32), 16: FCS, 20: FDP(32), 24: FDS.
// FRSTOR additionally consumes the eight 80-bit registers that follow it
// (offset 28, 10 bytes each). The extra precision of 80-bit values beyond f64
// is not representable in this runtime, which is the same limitation that
// applies to every other x87 operation here.
// ---------------------------------------------------------------------------

#[inline]
unsafe fn rw16(p: u64) -> u16 { (p as *const u16).read_unaligned() }
#[inline]
unsafe fn ww16(p: u64, v: u16) { (p as *mut u16).write_unaligned(v); }
#[inline]
unsafe fn ww32(p: u64, v: u32) { (p as *mut u32).write_unaligned(v); }

/// Convert an 80-bit extended-precision float to f64 (precision-limited).
fn ext80_to_f64(b: &[u8]) -> f64 {
    let sig = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = se & 0x8000 != 0;
    let exp = (se & 0x7fff) as i32;
    let mag = if exp == 0x7fff {
        if sig == 0x8000_0000_0000_0000 { f64::INFINITY } else { f64::NAN }
    } else if exp == 0 && sig == 0 {
        0.0
    } else {
        sig as f64 * 2f64.powi(exp - 16383 - 63)
    };
    if sign { -mag } else { mag }
}

/// FNSTENV: store the x87 environment (28 bytes).
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fnstenv(cpu: *mut GowCpu, addr: u64) {
    let c = &*cpu;
    ww16(addr, c.fpu.cw);
    ww16(addr + 2, 0);
    ww16(addr + 4, c.fpu.sw);
    ww16(addr + 6, 0);
    ww16(addr + 8, c.fpu.tag);
    ww16(addr + 10, 0);
    ww32(addr + 12, 0); // instruction pointer
    ww16(addr + 16, 0); // code selector
    ww16(addr + 18, 0);
    ww32(addr + 20, 0); // data pointer
    ww16(addr + 24, 0); // data selector
    ww16(addr + 26, 0);
}

/// FLDENV: load the x87 environment (control/status/tag words).
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fldenv(cpu: *mut GowCpu, addr: u64) {
    let c = &mut *cpu;
    c.fpu.cw = rw16(addr);
    c.fpu.sw = rw16(addr + 4);
    c.fpu.tag = rw16(addr + 8);
}

/// FRSTOR: restore the environment plus the eight 80-bit registers that follow.
/// The registers are stored in logical order ST(0)..ST(7); TOP comes from FSW.
#[no_mangle]
pub unsafe extern "C" fn gow_x87_frstor(cpu: *mut GowCpu, addr: u64) {
    let c = &mut *cpu;
    c.fpu.cw = rw16(addr);
    c.fpu.sw = rw16(addr + 4);
    c.fpu.tag = rw16(addr + 8);
    c.fpu.top = ((c.fpu.sw >> 11) & 7) as u8;
    for i in 0..8u64 {
        let p = addr + 28 + i * 10;
        let mut b = [0u8; 10];
        std::ptr::copy_nonoverlapping(p as *const u8, b.as_mut_ptr(), 10);
        c.fpu.set(i as u8, ext80_to_f64(&b));
    }
}
