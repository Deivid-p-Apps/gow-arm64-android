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
