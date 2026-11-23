//! Minimal PE32+ image loader for the runtime: read the game executable, parse
//! its headers/sections and copy the section data into the already-mapped guest
//! image buffer (mapped at the original image base). No relocation is applied
//! because we map at the preferred base, so guest VAs equal host addresses.

use crate::glog;

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
    glog!("[gow.pe] mapped image: base=0x{image_base:x} size=0x{size_of_image:x} entry=0x{entry_rva:x}");
    Some(PeInfo { image_base, size_of_image, entry_rva })
}
