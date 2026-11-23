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
#[no_mangle]
pub unsafe extern "C" fn gow_x87_fldenv(cpu: *mut GowCpu, addr: u64) {
    let _ = addr;
    let _ = cpu;
}
