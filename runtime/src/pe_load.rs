//! Minimal PE32+ image loader for the runtime: read the game executable, parse
//! its headers/sections and copy the section data into the already-mapped guest
//! image buffer (mapped at the original image base). No relocation is applied
//! because we map at the preferred base, so guest VAs equal host addresses.

use crate::glog;

fn rva_to_off(b: &[u8], opt: usize, optsz: usize, nsec: usize, rva: u32) -> Option<usize> {
    let so = opt + optsz;
    for i in 0..nsec {
        let o = so + i * 40;
        if o + 40 > b.len() { break; }
        let vs = u32(b, o + 8);
        let va = u32(b, o + 12);
        let raw = u32(b, o + 20);
        let rs = u32(b, o + 16);
        if va <= rva && rva < va + vs.max(rs) {
            return Some((raw + (rva - va)) as usize);
        }
    }
    None
}

fn u16(b: &[u8], o: usize) -> u16 { u16::from_le_bytes([b[o], b[o + 1]]) }
fn u32(b: &[u8], o: usize) -> u32 { u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) }
fn u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

pub struct PeInfo {
    pub image_base: u64,
    pub size_of_image: u32,
    pub entry_rva: u32,
    /// VA of the TLS index (the DWORD the loader must fill with the slot index).
    pub tls_index_va: u64,
    /// TLS callback function VAs (DLL_PROCESS_ATTACH is called for each).
    pub tls_callbacks: Vec<u64>,
    /// The TLS slot index we assigned (written to tls_index_va).
    pub tls_index: u32,
    /// Start VA of the module's TLS template (StartAddressOfRawData). Windows
    /// copies [RawData, RawDataEnd) into every thread's TLS block for this
    /// module at thread start; we must do the same or `thread_local` variables
    /// read as zero (device log: a store through a null TLS pointer at
    /// 0x400cbb).
    pub tls_raw_va: u64,
    /// Size in bytes of the TLS template.
    pub tls_raw_size: u32,
}

/// Parse the headers and copy sections from `exe_path` into `dest` (a buffer
/// whose offset 0 corresponds to `image_base`). Returns image metadata.
pub fn load_into(exe_path: &str, dest: *mut u8, dest_len: usize) -> Option<PeInfo> {
    let b = std::fs::read(exe_path).ok()?;
    if b.len() < 0x40 || &b[0..2] != b"MZ" {
        glog!("[gow.pe] not MZ");
        return None;
    }
    let pe = u32(&b, 0x3c) as usize;
    if pe + 24 > b.len() || &b[pe..pe + 4] != b"PE\0\0" {
        glog!("[gow.pe] not PE");
        return None;
    }
    let coff = pe + 4;
    let nsec = u16(&b, coff + 2) as usize;
    let optsz = u16(&b, coff + 16) as usize;
    let opt = coff + 20;
    if u16(&b, opt) != 0x20b {
        glog!("[gow.pe] not PE32+");
        return None;
    }
    let entry_rva = u32(&b, opt + 16);
    let image_base = u64(&b, opt + 24);
    let size_of_image = u32(&b, opt + 56);
    let so = opt + optsz;

    // Copy headers, then each section's raw bytes into its virtual address.
    let hdr = b.len().min(0x1000).min(dest_len);
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), dest, hdr); }
    for i in 0..nsec {
        let o = so + i * 40;
        if o + 40 > b.len() { break; }
        let vsize = u32(&b, o + 8) as usize;
        let va = u32(&b, o + 12) as usize;
        let raw_size = u32(&b, o + 16) as usize;
        let raw_off = u32(&b, o + 20) as usize;
        let n = raw_size.min(vsize.max(raw_size));
        if raw_off + n <= b.len() && va + n <= dest_len {
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr().add(raw_off), dest.add(va), n); }
        }
    }
    // TLS directory: grab the index VA and the callback list, and assign a slot.
    let (mut tls_index_va, mut tls_callbacks) = (0u64, Vec::new());
    let (mut tls_raw_va, mut tls_raw_size) = (0u64, 0u32);
    let tls_rva = u32(&b, opt + 112 + 9 * 8);
    if tls_rva != 0 {
        if let Some(off) = rva_to_off(&b, opt, optsz, nsec, tls_rva) {
            tls_index_va = u64(&b, off + 16);
            // StartAddressOfRawData / EndAddressOfRawData / SizeOfZeroFill form
            // the template Windows copies into each thread's TLS block.
            let raw_start = u64(&b, off);
            let raw_end = u64(&b, off + 8);
            let zero_fill = u32(&b, off + 32);
            if raw_start >= image_base && raw_end > raw_start {
                tls_raw_va = raw_start;
                tls_raw_size = ((raw_end - raw_start) as u32).saturating_add(zero_fill);
            }
            let cbs = u64(&b, off + 24);
            if cbs >= image_base {
                let cbo = rva_to_off(&b, opt, optsz, nsec, (cbs - image_base) as u32);
                if let Some(mut p) = cbo {
                    loop {
                        let f = u64(&b, p);
                        if f == 0 { break; }
                        tls_callbacks.push(f);
                        p += 8;
                        if tls_callbacks.len() > 64 { break; }
                    }
                }
            }
        }
    }
    // Assign a TLS slot for this module (0-based, like the Windows loader).
    let tls_index: u32 = 0;
    if tls_index_va != 0 {
        let off = (tls_index_va - image_base) as usize;
        if off + 4 <= dest_len {
            unsafe { *((dest.add(off)) as *mut u32) = tls_index; }
        }
    }
    glog!("[gow.pe] mapped image: base=0x{image_base:x} size=0x{size_of_image:x} entry=0x{entry_rva:x} tls_idx=0x{tls_index_va:x} tls_callbacks={}", tls_callbacks.len());
    if tls_raw_va != 0 && tls_raw_size != 0 {
        glog!("[gow.pe] TLS template va=0x{tls_raw_va:x} size=0x{tls_raw_size:x} (copied into each thread's TLS block)");
        crate::dispatch::set_tls_template(tls_raw_va, tls_raw_size);
    }
    Some(PeInfo { image_base, size_of_image, entry_rva, tls_index_va, tls_callbacks, tls_index, tls_raw_va, tls_raw_size })
}
