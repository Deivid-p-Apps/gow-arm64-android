//! PE32+ (x86-64) parser sufficient for static recompilation.
//! Extracts headers, sections, entry point, imports (IAT), exports and the
//! exception directory (.pdata) which enumerates every function in the image.

use anyhow::{bail, Result};

pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
}

impl Section {
    pub fn is_executable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_EXECUTE != 0
    }
    /// Highest RVA touched by this section (mapped extent).
    pub fn end_rva(&self) -> u32 {
        self.virtual_address
            .saturating_add(self.virtual_size.max(self.raw_size))
    }
    pub fn contains_rva(&self, rva: u32) -> bool {
        rva >= self.virtual_address && rva < self.end_rva()
    }
}

#[derive(Debug, Clone)]
pub struct Import {
    pub dll: String,
    pub func: String,
    /// Index into the Import Address Table (by thunk slot).
    pub iat_rva: u32,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub name: String,
    pub rva: u32,
}

pub struct PeImage {
    pub bytes: Vec<u8>,
    pub image_base: u64,
    pub entry_rva: u32,
    pub section_alignment: u32,
    pub size_of_image: u32,
    pub sections: Vec<Section>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    /// RVA ranges from the exception directory (start, end) for every function.
    pub pdata_functions: Vec<(u32, u32)>,
    pub tls_callback_va: u64,
    pub load_config_rva: u32,
}

fn u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

fn cstr(b: &[u8], o: usize, lim: usize) -> String {
    let end = b[o..(o + lim).min(b.len())]
        .iter()
        .position(|&c| c == 0)
        .map(|p| o + p)
        .unwrap_or(o + lim);
    String::from_utf8_lossy(&b[o..end]).into_owned()
}

impl PeImage {
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
            bail!("not MZ");
        }
        let peoff = u32(&bytes, 0x3c) as usize;
        if &bytes[peoff..peoff + 4] != b"PE\0\0" {
            bail!("not PE");
        }
        let coff = peoff + 4;
        let machine = u16(&bytes, coff);
        if machine != 0x8664 {
            bail!("not x86-64 (machine=0x{machine:04x})");
        }
        let nsec = u16(&bytes, coff + 2) as usize;
        let optsz = u16(&bytes, coff + 16) as usize;
        let opt = coff + 20;
        if u16(&bytes, opt) != 0x20b {
            bail!("not PE32+");
        }
        let entry_rva = u32(&bytes, opt + 16);
        let image_base = u64(&bytes, opt + 24);
        let section_alignment = u32(&bytes, opt + 32);
        let size_of_image = u32(&bytes, opt + 56);
        let ddir = opt + 112;

        let mut sections = Vec::with_capacity(nsec);
        let so = opt + optsz;
        for i in 0..nsec {
            let o = so + i * 40;
            sections.push(Section {
                name: cstr(&bytes, o, 8),
                virtual_size: u32(&bytes, o + 8),
                virtual_address: u32(&bytes, o + 12),
                raw_size: u32(&bytes, o + 16),
                raw_offset: u32(&bytes, o + 20),
                characteristics: u32(&bytes, o + 36),
            });
        }

        let dir = |i: usize| -> (u32, u32) {
            let o = ddir + i * 8;
            (u32(&bytes, o), u32(&bytes, o + 4))
        };

        // --- imports ---
        let mut imports = Vec::new();
        let (imp_rva, _) = dir(1);
        if imp_rva != 0 {
            if let Some(off) = Self::rva_to_off(&sections, imp_rva) {
                let mut p = off;
                loop {
                    let oft = u32(&bytes, p);
                    let name_rva = u32(&bytes, p + 12);
                    let first = u32(&bytes, p + 16);
                    if name_rva == 0 && oft == 0 && first == 0 {
                        break;
                    }
                    let dll = Self::rva_to_off(&sections, name_rva)
                        .map(|o| cstr(&bytes, o, 256))
                        .unwrap_or_default();
                    let thunk_rva = if oft != 0 { oft } else { first };
                    if let Some(mut t) = Self::rva_to_off(&sections, thunk_rva) {
                        let slot_base = first;
                        let mut slot = 0u32;
                        loop {
                            let ent = u64(&bytes, t);
                            if ent == 0 {
                                break;
                            }
                            let func = if ent & (1 << 63) != 0 {
                                format!("ord#{}", ent & 0xffff)
                            } else {
                                Self::rva_to_off(&sections, (ent & 0x7fff_ffff) as u32)
                                    .map(|fo| cstr(&bytes, fo + 2, 256))
                                    .unwrap_or_default()
                            };
                            imports.push(Import {
                                dll: dll.clone(),
                                func,
                                iat_rva: slot_base + slot * 8,
                            });
                            t += 8;
                            slot += 1;
                        }
                    }
                    p += 20;
                }
            }
        }

        // --- exports ---
        let mut exports = Vec::new();
        let (exp_rva, _) = dir(0);
        if exp_rva != 0 {
            if let Some(off) = Self::rva_to_off(&sections, exp_rva) {
                let nnames = u32(&bytes, off + 24);
                let funcs = u32(&bytes, off + 28);
                let names = u32(&bytes, off + 32);
                let ords = u32(&bytes, off + 36);
                for i in 0..nnames {
                    let nr = u32(&bytes, Self::rva_to_off(&sections, names).unwrap() + (i * 4) as usize);
                    let on = u16(&bytes, Self::rva_to_off(&sections, ords).unwrap() + (i * 2) as usize);
                    let fr = u32(
                        &bytes,
                        Self::rva_to_off(&sections, funcs).unwrap() + (on as usize * 4),
                    );
                    let name = Self::rva_to_off(&sections, nr)
                        .map(|o| cstr(&bytes, o, 256))
                        .unwrap_or_default();
                    exports.push(Export { name, rva: fr });
                }
            }
        }

        // --- exception directory: every function ---
        let mut pdata_functions = Vec::new();
        let (exc_rva, exc_size) = dir(3);
        if exc_rva != 0 {
            if let Some(off) = Self::rva_to_off(&sections, exc_rva) {
                let count = exc_size as usize / 12;
                for i in 0..count {
                    let o = off + i * 12;
                    let start = u32(&bytes, o);
                    let end = u32(&bytes, o + 4);
                    if start != 0 && end > start {
                        pdata_functions.push((start, end));
                    }
                }
            }
        }

        // --- TLS callbacks ---
        let mut tls_callback_va = 0;
        let (tls_rva, _) = dir(9);
        if tls_rva != 0 {
            if let Some(off) = Self::rva_to_off(&sections, tls_rva) {
                tls_callback_va = u64(&bytes, off + 24);
            }
        }

        let (load_config_rva, _) = dir(10);

        Ok(PeImage {
            bytes,
            image_base,
            entry_rva,
            section_alignment,
            size_of_image,
            sections,
            imports,
            exports,
            pdata_functions,
            tls_callback_va,
            load_config_rva,
        })
    }

    pub fn rva_to_off(sections: &[Section], rva: u32) -> Option<usize> {
        for s in sections {
            if s.virtual_address <= rva && rva < s.virtual_address.saturating_add(s.virtual_size.max(s.raw_size)) {
                return Some((s.raw_offset + (rva - s.virtual_address)) as usize);
            }
        }
        None
    }

    pub fn read_rva(&self, rva: u32, len: usize) -> Option<&[u8]> {
        let off = Self::rva_to_off(&self.sections, rva)?;
        self.bytes.get(off..off + len)
    }

    pub fn va_to_rva(&self, va: u64) -> Option<u32> {
        if va >= self.image_base && va < self.image_base + self.size_of_image as u64 {
            Some((va - self.image_base) as u32)
        } else {
            None
        }
    }

    pub fn rva_to_va(&self, rva: u32) -> u64 {
        self.image_base + rva as u64
    }

    /// Copy the image into a correctly-laid-out guest memory buffer at the
    /// original image base (headers + section virtual placement).
    pub fn map_image(&self) -> Vec<u8> {
        let mut mem = vec![0u8; self.size_of_image as usize];
        // headers: copy up to first section raw
        let hdr = self.sections.iter().map(|s| s.raw_offset).min().unwrap_or(0) as usize;
        let hdr = hdr.min(self.bytes.len());
        mem[..hdr].copy_from_slice(&self.bytes[..hdr]);
        for s in &self.sections {
            let src = s.raw_offset as usize;
            let dst = s.virtual_address as usize;
            let n = s.raw_size as usize;
            if src + n <= self.bytes.len() && dst + n <= mem.len() {
                mem[dst..dst + n].copy_from_slice(&self.bytes[src..src + n]);
            }
        }
        mem
    }
}
