//! Remaining SSE integer/utility helpers referenced by lifted code.
//! Each is a `#[no_mangle] extern "C"` symbol the generated C calls into.

use crate::dispatch::GowCpu;

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pack(cpu: *mut GowCpu, dst: i32, src: i32, elem: u32, signed_: u32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0;
    let b = c.xmm[src as usize].0;
    let mut out = [0u8; 16];
    match elem {
        1 => {
            // packsswb/packuswb: 8 x i16 -> 8 x i8 per half
            for half in 0..2 {
                let av = if half == 0 { a[0] } else { a[1] };
                let bv = if half == 0 { b[0] } else { b[1] };
                let ab = av.to_le_bytes();
                let bb = bv.to_le_bytes();
                let off = half * 8;
                for i in 0..4 {
                    let x = i16::from_le_bytes([ab[i * 2], ab[i * 2 + 1]]);
                    out[off + i] = if signed_ != 0 { x.clamp(-128, 127) as i8 as u8 } else { x.clamp(0, 255) as u8 };
                }
                for i in 0..4 {
                    let x = i16::from_le_bytes([bb[i * 2], bb[i * 2 + 1]]);
                    out[off + 4 + i] = if signed_ != 0 { x.clamp(-128, 127) as i8 as u8 } else { x.clamp(0, 255) as u8 };
                }
            }
        }
        2 => {
            // packssdw/packusdw: 4 x i32 -> 4 x i16
            let mut o = [0u16; 8];
            for i in 0..4 {
                let x = (a[i / 2] >> (32 * (i % 2))) as u32 as i32;
                o[i] = if signed_ != 0 { x.clamp(-32768, 32767) as i16 as u16 } else { x.clamp(0, 65535) as u16 };
            }
            for i in 0..4 {
                let x = (b[i / 2] >> (32 * (i % 2))) as u32 as i32;
                o[4 + i] = if signed_ != 0 { x.clamp(-32768, 32767) as i16 as u16 } else { x.clamp(0, 65535) as u16 };
            }
            for i in 0..4 { out[i * 2..i * 2 + 2].copy_from_slice(&o[i].to_le_bytes()); }
            for i in 0..4 { out[8 + i * 2..8 + i * 2 + 2].copy_from_slice(&o[4 + i].to_le_bytes()); }
        }
        _ => {}
    }
    c.xmm[dst as usize].0[0] = u64::from_le_bytes(out[0..8].try_into().unwrap());
    c.xmm[dst as usize].0[1] = u64::from_le_bytes(out[8..16].try_into().unwrap());
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pshufb(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0;
    let mask = c.xmm[src as usize].0;
    let mut ab = [0u8; 16];
    ab[0..8].copy_from_slice(&a[0].to_le_bytes());
    ab[8..16].copy_from_slice(&a[1].to_le_bytes());
    let mut mb = [0u8; 16];
    mb[0..8].copy_from_slice(&mask[0].to_le_bytes());
    mb[8..16].copy_from_slice(&mask[1].to_le_bytes());
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = if mb[i] & 0x80 != 0 { 0 } else { ab[(mb[i] & 0x0f) as usize] };
    }
    c.xmm[dst as usize].0[0] = u64::from_le_bytes(out[0..8].try_into().unwrap());
    c.xmm[dst as usize].0[1] = u64::from_le_bytes(out[8..16].try_into().unwrap());
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pabs(cpu: *mut GowCpu, dst: i32, elem: u32) {
    let c = &mut *cpu;
    let a = &mut c.xmm[dst as usize].0;
    let mut o = [0u64; 2];
    for w in 0..2 {
        let x = a[w].to_le_bytes();
        let mut ob = [0u8; 8];
        let lanes = 8 / elem;
        for i in 0..lanes as usize {
            let off = i * elem as usize;
            let v = match elem {
                4 => { let v = i32::from_le_bytes([x[off], x[off + 1], x[off + 2], x[off + 3]]); v.wrapping_abs().to_le_bytes().to_vec() }
                2 => { let v = i16::from_le_bytes([x[off], x[off + 1]]); v.wrapping_abs().to_le_bytes().to_vec() }
                _ => { let v = x[off] as i8; (v.wrapping_abs() as u8).to_le_bytes().to_vec() }
            };
            ob[off..off + elem as usize].copy_from_slice(&v);
        }
        o[w] = u64::from_le_bytes(ob);
    }
    *a = o;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pmulld(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    let mut o = [0u64; 2];
    for w in 0..2 {
        let xb = a[w].to_le_bytes();
        let yb = b[w].to_le_bytes();
        let mut ob = [0u8; 8];
        for i in 0..2 {
            let x = i32::from_le_bytes([xb[i * 4], xb[i * 4 + 1], xb[i * 4 + 2], xb[i * 4 + 3]]);
            let y = i32::from_le_bytes([yb[i * 4], yb[i * 4 + 1], yb[i * 4 + 2], yb[i * 4 + 3]]);
            ob[i * 4..i * 4 + 4].copy_from_slice(&x.wrapping_mul(y).to_le_bytes());
        }
        o[w] = u64::from_le_bytes(ob);
    }
    *a = o;
}

/// pminsw/pmaxsw/pminsd?/pminub/pmaxub etc.: elem = 1/2/4 bytes, max = 1 for max.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pminmax(cpu: *mut GowCpu, dst: i32, src: i32, elem: u32, max: u32, signed_: u32) {
    let c = &mut *cpu;
    let b = c.xmm[src as usize].0;
    let a = &mut c.xmm[dst as usize].0;
    match elem {
        4 => {
            for w in 0..2 {
                let xb = a[w].to_le_bytes();
                let yb = b[w].to_le_bytes();
                let mut o = [0u8; 8];
                for i in 0..2 {
                    let x = i32::from_le_bytes([xb[i * 4], xb[i * 4 + 1], xb[i * 4 + 2], xb[i * 4 + 3]]);
                    let y = i32::from_le_bytes([yb[i * 4], yb[i * 4 + 1], yb[i * 4 + 2], yb[i * 4 + 3]]);
                    let r: u32 = if signed_ != 0 {
                        (if max != 0 { x.max(y) } else { x.min(y) }) as u32
                    } else {
                        let (xu, yu) = (x as u32, y as u32);
                        if max != 0 { xu.max(yu) } else { xu.min(yu) }
                    };
                    o[i * 4..i * 4 + 4].copy_from_slice(&r.to_le_bytes());
                }
                a[w] = u64::from_le_bytes(o);
            }
        }
        2 => {
            for w in 0..2 {
                let xb = a[w].to_le_bytes();
                let yb = b[w].to_le_bytes();
                let mut o = [0u8; 8];
                for i in 0..4 {
                    let x = i16::from_le_bytes([xb[i * 2], xb[i * 2 + 1]]);
                    let y = i16::from_le_bytes([yb[i * 2], yb[i * 2 + 1]]);
                    let r: u16 = if signed_ != 0 {
                        (if max != 0 { x.max(y) } else { x.min(y) }) as u16
                    } else {
                        let (xu, yu) = (x as u16, y as u16);
                        if max != 0 { xu.max(yu) } else { xu.min(yu) }
                    };
                    o[i * 2..i * 2 + 2].copy_from_slice(&r.to_le_bytes());
                }
                a[w] = u64::from_le_bytes(o);
            }
        }
        _ => {
            for w in 0..2 {
                let xb = a[w].to_le_bytes();
                let yb = b[w].to_le_bytes();
                let mut o = [0u8; 8];
                for i in 0..8 {
                    let (xu, yu) = (xb[i], yb[i]);
                    o[i] = if signed_ != 0 {
                        let (xs, ys) = (xu as i8, yu as i8);
                        (if max != 0 { xs.max(ys) } else { xs.min(ys) }) as u8
                    } else if max != 0 { xu.max(yu) } else { xu.min(yu) };
                }
                a[w] = u64::from_le_bytes(o);
            }
        }
    }
}

/// pshuflw (high=0) / pshufhw (high=1): shuffle four 16-bit lanes within one half.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pshuf_hw(cpu: *mut GowCpu, dst: i32, sel: u32, high: u32) {
    let c = &mut *cpu;
    let a = &mut c.xmm[dst as usize].0;
    let (idx, hi) = if high == 0 { (0usize, false) } else { (1usize, true) };
    let w = a[idx];
    let mut out = 0u64;
    for i in 0..4 {
        let s = (sel >> (2 * i)) & 3;
        let v = (w >> (16 * s)) & 0xffff;
        out |= v << (16 * i);
    }
    if hi { a[1] = (a[1] & 0xffff_ffff_0000_0000) | out; } else { a[0] = (a[0] & 0xffff_ffff_0000_0000) | out; }
}

/// pextrw/pinsrw helper: extract/insert a 16-bit lane.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pextrw(cpu: *mut GowCpu, src: i32, imm: u32) -> u64 {
    let c = &*cpu;
    (c.xmm[src as usize].0[(imm as usize / 4) % 2] >> (16 * (imm % 4))) & 0xffff
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pinsrw(cpu: *mut GowCpu, dst: i32, val: u64, imm: u32) {
    let c = &mut *cpu;
    let w = imm as usize / 4;
    let sh = 16 * (imm % 4);
    let a = &mut c.xmm[dst as usize].0[w % 2];
    *a = (*a & !(0xffffu64 << sh)) | ((val & 0xffff) << sh);
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pmovsx(cpu: *mut GowCpu, dst: i32, src: i32, size: u32, sign: u32) {
    let c = &mut *cpu;
    let s = c.xmm[src as usize].0;
    let sb = [s[0].to_le_bytes(), s[1].to_le_bytes()].concat();
    let mut out = [0u32; 4];
    for i in 0..4 {
        let byte = sb[i];
        out[i] = match size {
            1 => if sign != 0 { (byte as i8) as i32 as u32 } else { byte as u32 },
            2 => { let v = u16::from_le_bytes([sb[i], sb[i + 1]]); if sign != 0 { (v as i16) as i32 as u32 } else { v as u32 } }
            _ => 0,
        };
    }
    c.xmm[dst as usize].0[0] = out[0] as u64 | (out[1] as u64) << 32;
    c.xmm[dst as usize].0[1] = out[2] as u64 | (out[3] as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_punpck32(cpu: *mut GowCpu, dst: i32, src: i32, elem: u32, hi: u32) {
    let _ = elem;
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[dst as usize].0);
    let b = crate::dispatch::LANES32(c.xmm[src as usize].0);
    let base = if hi != 0 { 2 } else { 0 };
    let r0 = a[base]; let r1 = b[base]; let r2 = a[base + 1]; let r3 = b[base + 1];
    c.xmm[dst as usize].0[0] = r0 as u64 | (r1 as u64) << 32;
    c.xmm[dst as usize].0[1] = r2 as u64 | (r3 as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_punpck_wd(cpu: *mut GowCpu, dst: i32, src: i32, hi: u32) {
    let c = &mut *cpu;
    let mut da = [0u8; 16];
    let mut db = [0u8; 16];
    da[0..8].copy_from_slice(&c.xmm[dst as usize].0[0].to_le_bytes());
    da[8..16].copy_from_slice(&c.xmm[dst as usize].0[1].to_le_bytes());
    db[0..8].copy_from_slice(&c.xmm[src as usize].0[0].to_le_bytes());
    db[8..16].copy_from_slice(&c.xmm[src as usize].0[1].to_le_bytes());
    let base = if hi != 0 { 8 } else { 0 };
    let mut out = [0u8; 16];
    for i in 0..4 {
        out[i * 4..i * 4 + 2].copy_from_slice(&da[base + i * 2..base + i * 2 + 2]);
        out[i * 4 + 2..i * 4 + 4].copy_from_slice(&db[base + i * 2..base + i * 2 + 2]);
    }
    c.xmm[dst as usize].0[0] = u64::from_le_bytes(out[0..8].try_into().unwrap());
    c.xmm[dst as usize].0[1] = u64::from_le_bytes(out[8..16].try_into().unwrap());
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_punpck_qdq(cpu: *mut GowCpu, dst: i32, src: i32, hi: u32) {
    let c = &mut *cpu;
    let a = c.xmm[dst as usize].0;
    let b = c.xmm[src as usize].0;
    let r0 = if hi != 0 { a[1] } else { a[0] };
    let r1 = if hi != 0 { b[1] } else { b[0] };
    c.xmm[dst as usize].0 = [r0, r1];
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movshdup(cpu: *mut GowCpu, reg: i32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[reg as usize].0);
    let o = [a[1], a[1], a[3], a[3]];
    c.xmm[reg as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[reg as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movsldup(cpu: *mut GowCpu, reg: i32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[reg as usize].0);
    let o = [a[0], a[0], a[2], a[2]];
    c.xmm[reg as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[reg as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_hadd(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[dst as usize].0);
    let b = crate::dispatch::LANES32(c.xmm[src as usize].0);
    let ax: [f32; 4] = [f32::from_bits(a[0]), f32::from_bits(a[1]), f32::from_bits(a[2]), f32::from_bits(a[3])];
    let bx: [f32; 4] = [f32::from_bits(b[0]), f32::from_bits(b[1]), f32::from_bits(b[2]), f32::from_bits(b[3])];
    let o = [ax[0] + ax[1], ax[2] + ax[3], bx[0] + bx[1], bx[2] + bx[3]];
    c.xmm[dst as usize].0[0] = o[0].to_bits() as u64 | (o[1].to_bits() as u64) << 32;
    c.xmm[dst as usize].0[1] = o[2].to_bits() as u64 | (o[3].to_bits() as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_blendps(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[dst as usize].0);
    let b = crate::dispatch::LANES32(c.xmm[src as usize].0);
    let mut o = [0u32; 4];
    for i in 0..4 { o[i] = if imm & (1 << i) != 0 { b[i] } else { a[i] }; }
    c.xmm[dst as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[dst as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_roundps(cpu: *mut GowCpu, reg: i32, imm: u32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[reg as usize].0);
    let mode = imm & 3;
    let mut o = [0u32; 4];
    for i in 0..4 {
        let x = f32::from_bits(a[i]);
        o[i] = match mode { 0 => x.round(), 1 => x.floor(), 2 => x.ceil(), _ => x.trunc() }.to_bits();
    }
    c.xmm[reg as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[reg as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_insertps(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[dst as usize].0);
    let b = crate::dispatch::LANES32(c.xmm[src as usize].0);
    let mut o = a;
    let src_lane = ((imm >> 6) & 3) as usize;
    let dst_lane = ((imm >> 4) & 3) as usize;
    o[dst_lane] = b[src_lane];
    c.xmm[dst as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[dst as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_movhpd(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let v = c.xmm[src as usize].0[0];
    c.xmm[dst as usize].0[1] = v;
}
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_pmovmskb_val(cpu: *mut GowCpu, src: i32) -> u64 {
    let c = &*cpu;
    let s = c.xmm[src as usize].0;
    let mut m = 0u64;
    for i in 0..16 {
        let byte = (s[i / 8] >> (8 * (i % 8))) & 0xff;
        if byte & 0x80 != 0 { m |= 1 << i; }
    }
    m
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_set_lane1(cpu: *mut GowCpu, reg: i32, v: u64) {
    (*cpu).xmm[reg as usize].0[1] = v;
}

#[no_mangle]
pub unsafe extern "C" fn gow_xmm_permilps(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[src as usize].0);
    let mut o = [0u32; 4];
    for i in 0..4 { o[i] = a[((imm >> (2 * i)) & 3) as usize]; }
    c.xmm[dst as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[dst as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

/// ADDSUBPS (SSE3): even lanes subtract, odd lanes add.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_addsubps(cpu: *mut GowCpu, dst: i32, src: i32) {
    let c = &mut *cpu;
    let a = crate::dispatch::LANES32(c.xmm[dst as usize].0);
    let b = crate::dispatch::LANES32(c.xmm[src as usize].0);
    let mut o = [0u32; 4];
    for i in 0..4 {
        let x = f32::from_bits(a[i]);
        let y = f32::from_bits(b[i]);
        let r = if i % 2 == 0 { x - y } else { x + y };
        o[i] = r.to_bits();
    }
    c.xmm[dst as usize].0[0] = o[0] as u64 | (o[1] as u64) << 32;
    c.xmm[dst as usize].0[1] = o[2] as u64 | (o[3] as u64) << 32;
}

/// PALIGNR: concatenate dst (high) and src (low) into 32 bytes, shift the pair
/// right by `imm` bytes and keep the low 16.
#[no_mangle]
pub unsafe extern "C" fn gow_xmm_palignr(cpu: *mut GowCpu, dst: i32, src: i32, imm: u32) {
    let c = &mut *cpu;
    let d = c.xmm[dst as usize].0;
    let s = c.xmm[src as usize].0;
    let mut cat = [0u8; 32];
    cat[0..8].copy_from_slice(&s[0].to_le_bytes());
    cat[8..16].copy_from_slice(&s[1].to_le_bytes());
    cat[16..24].copy_from_slice(&d[0].to_le_bytes());
    cat[24..32].copy_from_slice(&d[1].to_le_bytes());
    let mut out = [0u8; 16];
    let sh = (imm & 0xff) as usize;
    if sh < 32 {
        for i in 0..16 {
            out[i] = if i + sh < 32 { cat[i + sh] } else { 0 };
        }
    }
    c.xmm[dst as usize].0[0] = u64::from_le_bytes(out[0..8].try_into().unwrap());
    c.xmm[dst as usize].0[1] = u64::from_le_bytes(out[8..16].try_into().unwrap());
}

/// RCL/RCR: rotate `v` (size_bits: 8/16/32/64) through carry, updating CF and,
/// for a single-bit rotate, OF. Count is masked to 5/6 bits per x86 rules.
/// `right` != 0 selects RCR. Returns the new operand value.
#[no_mangle]
pub unsafe extern "C" fn gow_rcl_rcr(cpu: *mut GowCpu, v: u64, count: u64, size_bits: u32, right: u32) -> u64 {
    use crate::dispatch::{CF, OF};
    let bits = size_bits.clamp(8, 64);
    let mask: u64 = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let n = count & if bits == 64 { 0x3f } else { 0x1f };
    let c = &mut *cpu;
    if n == 0 {
        return v & mask; // zero-bit rotate changes no flags
    }
    let cf_in = if c.flags & CF != 0 { 1u128 } else { 0u128 };
    let width = bits + 1;
    let m = (1u128 << width) - 1;
    let mut q = (cf_in << bits) | (v as u128 & mask as u128);
    let sh = (n % width) as u32;
    q = if right != 0 {
        ((q >> sh) | (q << (width - sh))) & m
    } else {
        ((q << sh) | (q >> (width - sh))) & m
    };
    let result = (q as u64) & mask;
    let cf_out = ((q >> bits) & 1) as u64;
    c.flags = (c.flags & !(CF | OF)) | (if cf_out != 0 { CF } else { 0 });
    if n == 1 {
        let msb = (result >> (bits - 1)) & 1;
        let of = if right != 0 {
            msb ^ ((result >> (bits - 2)) & 1)
        } else {
            msb ^ cf_out
        };
        if of != 0 { c.flags |= OF; }
    }
    result
}
