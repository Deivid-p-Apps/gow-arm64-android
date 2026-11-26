//! x86-64 -> C static lifter.
//!
//! Model: every discovered function becomes `void f_<rva>(GowCpu*)`. Control
//! flow (call/ret/jmp/jcc) selects the next function through the runtime and
//! returns to the central dispatch loop, so guest call/ret are exact.
//!
//! Coverage is explicit: instructions we do not lift become `gow_unimpl(...)`
//! traps that record the RVA, so incomplete coverage is visible, never silent.

use anyhow::Result;
use iced_x86::{ConditionCode, Decoder, DecoderOptions, Formatter, Instruction, NasmFormatter, OpKind, Register};
use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::pe::PeImage;

pub struct LiftOutput {
    pub function_count: usize,
    pub instruction_count: usize,
    pub unimpl: BTreeSet<String>,
    pub unimpl_instances: u64,
}

/// Map a register to (base 64-bit name, width in bits, shift).
fn classify(r: Register) -> Option<(&'static str, u32, u32)> {
    use Register::*;
    Some(match r {
        // 8-bit low
        AL => ("rax", 8, 0), CL => ("rcx", 8, 0), DL => ("rdx", 8, 0), BL => ("rbx", 8, 0),
        SPL => ("rsp", 8, 0), BPL => ("rbp", 8, 0), SIL => ("rsi", 8, 0), DIL => ("rdi", 8, 0),
        R8L => ("r8", 8, 0), R9L => ("r9", 8, 0), R10L => ("r10", 8, 0), R11L => ("r11", 8, 0),
        R12L => ("r12", 8, 0), R13L => ("r13", 8, 0), R14L => ("r14", 8, 0), R15L => ("r15", 8, 0),
        // 8-bit high
        AH => ("rax", 8, 8), CH => ("rcx", 8, 8), DH => ("rdx", 8, 8), BH => ("rbx", 8, 8),
        // 16-bit
        AX => ("rax", 16, 0), CX => ("rcx", 16, 0), DX => ("rdx", 16, 0), BX => ("rbx", 16, 0),
        SP => ("rsp", 16, 0), BP => ("rbp", 16, 0), SI => ("rsi", 16, 0), DI => ("rdi", 16, 0),
        R8W => ("r8", 16, 0), R9W => ("r9", 16, 0), R10W => ("r10", 16, 0), R11W => ("r11", 16, 0),
        R12W => ("r12", 16, 0), R13W => ("r13", 16, 0), R14W => ("r14", 16, 0), R15W => ("r15", 16, 0),
        // 32-bit
        EAX => ("rax", 32, 0), ECX => ("rcx", 32, 0), EDX => ("rdx", 32, 0), EBX => ("rbx", 32, 0),
        ESP => ("rsp", 32, 0), EBP => ("rbp", 32, 0), ESI => ("rsi", 32, 0), EDI => ("rdi", 32, 0),
        R8D => ("r8", 32, 0), R9D => ("r9", 32, 0), R10D => ("r10", 32, 0), R11D => ("r11", 32, 0),
        R12D => ("r12", 32, 0), R13D => ("r13", 32, 0), R14D => ("r14", 32, 0), R15D => ("r15", 32, 0),
        // 64-bit
        RAX => ("rax", 64, 0), RCX => ("rcx", 64, 0), RDX => ("rdx", 64, 0), RBX => ("rbx", 64, 0),
        RSP => ("rsp", 64, 0), RBP => ("rbp", 64, 0), RSI => ("rsi", 64, 0), RDI => ("rdi", 64, 0),
        R8 => ("r8", 64, 0), R9 => ("r9", 64, 0), R10 => ("r10", 64, 0), R11 => ("r11", 64, 0),
        R12 => ("r12", 64, 0), R13 => ("r13", 64, 0), R14 => ("r14", 64, 0), R15 => ("r15", 64, 0),
        _ => return Option::None,
    })
}

fn xmm_index(r: Register) -> Option<u32> {
    use Register::*;
    Some(match r {
        XMM0 => 0, XMM1 => 1, XMM2 => 2, XMM3 => 3, XMM4 => 4, XMM5 => 5, XMM6 => 6, XMM7 => 7,
        XMM8 => 8, XMM9 => 9, XMM10 => 10, XMM11 => 11, XMM12 => 12, XMM13 => 13, XMM14 => 14, XMM15 => 15,
        // MMX registers alias the low 64 bits of the XMM file (hardware behaviour).
        MM0 => 0, MM1 => 1, MM2 => 2, MM3 => 3, MM4 => 4, MM5 => 5, MM6 => 6, MM7 => 7,
        _ => return Option::None,
    })
}

/// C expression reading a GPR operand of the given register.
fn reg_read(r: Register) -> String {
    if let Some((base, w, sh)) = classify(r) {
        if w == 64 {
            format!("GOW_REG(cpu, {base})")
        } else {
            let mask = if w == 32 { 0xffff_ffffu64 } else { (1u64 << w) - 1 };
            if sh == 0 {
                format!("(GOW_REG(cpu, {base}) & 0x{mask:x}ull)")
            } else {
                format!("((GOW_REG(cpu, {base}) >> {sh}) & 0x{mask:x}ull)")
            }
        }
    } else {
        "0".to_string()
    }
}

/// C statements writing a GPR operand from `val` (a C expression).
fn reg_write(r: Register, val: &str) -> String {
    if let Some((base, w, sh)) = classify(r) {
        match w {
            64 => format!("GOW_REG(cpu, {base}) = {val};"),
            32 => format!("GOW_REG(cpu, {base}) = (uint32_t)({val});"),
            _ => {
                let mask = (1u64 << w) - 1;
                format!(
                    "GOW_REG(cpu, {base}) = (GOW_REG(cpu, {base}) & ~(0x{mask:x}ull << {sh})) | (((uint64_t)({val}) & 0x{mask:x}ull) << {sh});"
                )
            }
        }
    } else {
        format!("(void)0; /*bad reg write*/")
    }
}

fn gpr_width(r: Register) -> u32 {
    classify(r).map(|(_, w, _)| w).unwrap_or(64)
}

fn mem_expr(insn: &Instruction, image_base: u64) -> String {
    let base = insn.memory_base();
    let index = insn.memory_index();
    let disp = insn.memory_displacement64();
    let scale = insn.memory_index_scale();
    let seg = insn.memory_segment();
    let mut e = String::new();
    if base != Register::None && base != Register::RIP {
        e.push_str(&format!("GOW_REG(cpu, {})", classify(base).map(|(b, _, _)| b).unwrap_or("rax")));
    }
    if index != Register::None {
        if !e.is_empty() { e.push_str(" + "); }
        e.push_str(&format!("(sign64(GOW_REG(cpu, {})) * {scale})", classify(index).map(|(b, _, _)| b).unwrap_or("rax")));
    }
    if base == Register::RIP {
        e.push_str(&format!("0x{disp:x}ull"));
    } else if disp != 0 {
        if !e.is_empty() { e.push_str(" + "); }
        e.push_str(&format!("0x{disp:x}ull"));
    }
    // Segment override: gs: -> TEB base, fs: -> TEB base (Windows uses GS for the
    // TEB; on x64 the fs base is used by the canary). Resolve via the runtime so
    // the TEB lives at the guest address the code expects.
    match seg {
        Register::GS => {
            let inner = if e.is_empty() { "0".to_string() } else { e.clone() };
            return format!("(gow_teb_base(cpu) + ({inner}))");
        }
        Register::FS => {
            let inner = if e.is_empty() { "0".to_string() } else { e.clone() };
            return format!("(gow_fs_base(cpu) + ({inner}))");
        }
        _ => {}
    }
    if e.is_empty() { e.push_str("0"); }
    let _ = image_base;
    e
}

/// Size in bytes of memory operand.
fn mem_size(insn: &Instruction) -> u32 {
    // iced's MemorySize::size() is already in BYTES (e.g. UInt32 -> 4,
    // Packed128 -> 16). The old code divided by 8, which made every memory
    // access the wrong width.
    let b = insn.memory_size().size();
    if b == 0 { 8 } else { b as u32 }
}

/// Correct size in bytes of the memory operand (iced's `MemorySize::size()` is
/// already in bytes). Used by handlers added later; the legacy `mem_size` above
/// predates this and is left untouched to avoid changing existing output.
fn mem_bytes(insn: &Instruction) -> u32 {
    let b = insn.memory_size().size() as u32;
    if b == 0 { 8 } else { b }
}

/// Load a GPR/imm/mem operand (op index 0/1/2) as a C expression of `width` bits.
fn operand_read(insn: &Instruction, idx: u32, image_base: u64) -> Option<String> {
    let kind = match idx { 0 => insn.op0_kind(), 1 => insn.op1_kind(), 2 => insn.op2_kind(), _ => return Option::None };
    match kind {
        OpKind::Register => Some(reg_read(reg_at(insn, idx))),
        OpKind::Memory => {
            let n = mem_size(insn);
            Some(format!("gow_load(cpu, {}, {n})", mem_expr(insn, image_base)))
        }
        OpKind::Immediate8 => Some(format!("0x{:x}ull", insn.immediate8() as u64)),
        OpKind::Immediate16 => Some(format!("0x{:x}ull", insn.immediate16() as u64)),
        OpKind::Immediate32 => Some(format!("0x{:x}ull", insn.immediate32() as u64)),
        OpKind::Immediate64 => Some(format!("0x{:x}ull", insn.immediate64())),
        OpKind::Immediate8to16 => Some(format!("0x{:x}ull", insn.immediate8to16() as i64 as u64)),
        OpKind::Immediate8to32 => Some(format!("0x{:x}ull", insn.immediate8to32() as i64 as u64)),
        OpKind::Immediate8to64 => Some(format!("0x{:x}ull", insn.immediate8to64() as u64)),
        OpKind::Immediate32to64 => Some(format!("0x{:x}ull", insn.immediate32to64() as u64)),
        _ => Option::None,
    }
}

fn reg_at(insn: &Instruction, idx: u32) -> Register {
    match idx { 0 => insn.op0_register(), 1 => insn.op1_register(), _ => insn.op2_register() }
}

fn op_width(insn: &Instruction, idx: u32) -> u32 {
    let kind = match idx { 0 => insn.op0_kind(), 1 => insn.op1_kind(), _ => insn.op2_kind() };
    match kind {
        OpKind::Register => gpr_width(reg_at(insn, idx)),
        OpKind::Memory => mem_size(insn) * 8,
        _ => insn.op0_register().size() as u32,
    }
}

fn cc_const(cc: ConditionCode) -> u32 {
    // iced's ConditionCode is O=1..G=16; gow_test_cc expects O=0..G=15.
    let v = cc as u32;
    if v == 0 { 0 } else { v - 1 }
}

pub fn lift(pe: &PeImage, out_dir: &std::path::Path) -> Result<LiftOutput> {
    let mut entries: BTreeSet<u32> = BTreeSet::new();
    for (start, _end) in &pe.pdata_functions {
        entries.insert(*start);
    }
    entries.insert(pe.entry_rva);
    if pe.tls_callback_va != 0 {
        if let Some(rva) = pe.va_to_rva(pe.tls_callback_va) {
            let mut i = 0;
            while let Some(slot) = pe.read_rva(rva + i * 8, 8) {
                let va = u64::from_le_bytes(slot.try_into().unwrap());
                if va == 0 { break; }
                if let Some(cb) = pe.va_to_rva(va) { entries.insert(cb); }
                i += 1;
            }
        }
    }

    // Discover call/jmp targets that are missing from `.pdata` (leaf/thunk code
    // the MSVC linker emits without exception data). Decode each KNOWN function
    // body with iced (correct instruction boundaries), collect direct targets,
    // add them, and iterate to a fixpoint. This is robust where a linear .text
    // sweep desyncs on embedded data.
    //
    // IMPORTANT: direct call/jmp targets are NOT enough. The engine stores
    // function pointers in .rdata/.data (vtables, handler tables, jump tables)
    // and calls them indirectly. On device those produced
    // `[gow.MISSFN] dispatch to rva=... no lifted entry` for 512 functions and
    // the main thread then hung waiting on work those functions should do
    // (device build #171). Windows documents that x64 functions are 16-byte
    // aligned; scan every non-executable section for 8-byte values that point
    // at a 16-aligned RVA inside an executable section and seed those as
    // entries too. False positives are harmless: an entry inside an existing
    // function is merged by the fall-through logic (D020); an entry at a data
    // byte then just decodes a few instructions and traps.
    {
        let exec_ranges: Vec<(u32, u32)> = pe
            .sections
            .iter()
            .filter(|s| s.is_executable())
            .map(|s| (s.virtual_address, s.end_rva()))
            .collect();
        let in_exec = |rva: u32| exec_ranges.iter().any(|&(a, b)| rva >= a && rva < b);
        for s in &pe.sections {
            if s.is_executable() { continue; }
            let n = s.raw_size.min(s.virtual_size.max(s.raw_size)) as usize;
            let base = s.raw_offset as usize;
            if base + n > pe.bytes.len() { continue; }
            let mut off = 0usize;
            while off + 8 <= n {
                let v = u64::from_le_bytes(pe.bytes[base + off..base + off + 8].try_into().unwrap());
                if v >= pe.image_base && v < pe.image_base + pe.size_of_image as u64 {
                    let rva = (v - pe.image_base) as u32;
                    if rva % 16 == 0 && in_exec(rva) {
                        entries.insert(rva);
                    }
                }
                off += 8;
            }
        }
        eprintln!("[discover] after pointer-table scan: {} entries", entries.len());
    }

    {
        // Sorted pdata map: start -> end (for the initial body bound).
        let mut pd: Vec<(u32, u32)> = pe.pdata_functions.clone();
        pd.sort_by_key(|r| r.0);
        let text_sec = pe.sections.iter().find(|s| s.name == ".text");
        let (tva, tend) = text_sec.map(|s| (s.virtual_address, s.end_rva())).unwrap_or((0, pe.size_of_image));
        for _round in 0..6 {
            let mut added = 0usize;
            let snapshot: Vec<u32> = entries.iter().cloned().collect();
            for &start in &snapshot {
                // Body end: pdata end if known, else the next entry (capped).
                let next = entries.range((start + 1)..).next().copied().unwrap_or(tend);
                let end = next.min(tend);
                if end <= start { continue; }
                let off = match PeImage::rva_to_off(&pe.sections, start) { Some(o) => o, None => continue };
                let len = (end - start) as usize;
                if len == 0 || pe.bytes.len() < off + len { continue; }
                let code = &pe.bytes[off..off + len];
                let mut dec = Decoder::with_ip(64, code, pe.image_base + start as u64, DecoderOptions::NONE);
                let mut insn = Instruction::default();
                while dec.can_decode() {
                    dec.decode_out(&mut insn);
                    if insn.is_invalid() { dec.set_position(dec.position() - insn.len() + 1); continue; }
                    if insn.op0_kind() == OpKind::NearBranch64
                        && matches!(insn.mnemonic(), iced_x86::Mnemonic::Call | iced_x86::Mnemonic::Jmp)
                    {
                        let t = insn.near_branch64().wrapping_sub(pe.image_base);
                        if t < 0xffff_ffff {
                            let t = t as u32;
                            if t >= tva && t < tend && entries.insert(t) { added += 1; }
                        }
                    }
                    // Address-taken functions: `lea reg, [rip+disp]` materialises a
                    // function pointer the engine stores in vtables/job tables and
                    // calls indirectly. These have no `.pdata` entry and no absolute
                    // pointer in a data section (the reloc is a RIP-relative lea), so
                    // the scans above miss them and the indirect call became
                    // `[gow.MISSFN] dispatch to rva=... no lifted entry` — the main
                    // thread then spun in the job manager waiting on work that
                    // function should do (e.g. 0x156250, a std::less comparator with
                    // 369 lea references). Seed every in-.text, 16-aligned target.
                    if insn.mnemonic() == iced_x86::Mnemonic::Lea
                        && insn.is_ip_rel_memory_operand()
                    {
                        let t = (insn.ip_rel_memory_address().wrapping_sub(pe.image_base)) as u64;
                        if t < 0xffff_ffff {
                            let t = t as u32;
                            if t >= tva && t < tend && t % 16 == 0
                                && entries.insert(t) { added += 1; }
                        }
                    }
                }
            }
            eprintln!("[discover] round +{added} (total {})", entries.len());
            if added == 0 { break; }
        }
    }

    let mut c = String::new();
    writeln!(c, "/* Auto-generated by gowrecomp. Do not edit. */")?;
    writeln!(c, "#include \"gow_guest.h\"\n")?;
    writeln!(c, "static inline int64_t sign64(uint64_t v) {{ return (int64_t)v; }}")?;
    writeln!(c, "#define GOW_NORET() return\n")?;

    let mut fmt = NasmFormatter::new();
    fmt.options_mut().set_hex_prefix("0x");
    fmt.options_mut().set_uppercase_hex(false);

    let sections = pe.sections.clone();
    let image_base = pe.image_base;
    let mut func_count = 0usize;
    let mut insn_count = 0usize;
    let mut unimpl: BTreeSet<String> = BTreeSet::new();
    let mut unimpl_instances: u64 = 0;

    // Build IAT slot map: VA of each IAT slot -> import index.
    let mut iat: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for (i, imp) in pe.imports.iter().enumerate() {
        iat.insert(imp.iat_rva, imp.iat_rva);
    }

    // Split functions into fixed-size shards so each translation unit stays
    std::fs::create_dir_all(out_dir)?;
    // Remove shard/table files from a previous run. Shards are numbered by
    // emitted byte size, so a code change can yield FEWER shards; a leftover
    // `gow_shard_NNN.c` would define the same `f_<rva>` symbols twice and the
    // link would fail with duplicate symbols.
    if let Ok(rd) = std::fs::read_dir(out_dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if (name.starts_with("gow_shard_") || name.starts_with("gow_table")) && name.ends_with(".c") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let entries_vec: Vec<u32> = entries.iter().cloned().collect();
    // Sorted pdata map for fast end lookup.
    let pdata_sorted: Vec<(u32, u32)> = {
        let mut v = pe.pdata_functions.clone();
        v.sort_by_key(|r| r.0);
        v
    };
    let text_end = pe.sections.iter().find(|s| s.name == ".text").map(|s| s.end_rva()).unwrap_or(pe.size_of_image);
    let pdata_end_of = |start: u32| -> Option<u32> {
        pdata_sorted.binary_search_by_key(&start, |r| r.0).ok().map(|i| pdata_sorted[i].1)
    };
    // MSVC emits ONE logical function as several contiguous `.pdata` chunks that
    // FALL THROUGH into one another (e.g. the std::string helper: 0xb3810 ends
    // mid-logic at 0xb386b, which continues into 0xb38d9). A chunk "continues"
    // when it has no trailing padding, its last real instruction is not a
    // terminator, and the next chunk starts exactly where it ends. The emitter
    // must decode a whole fall-through chain as one function: otherwise the
    // chunk returns early with the guest registers (rbp!) left clobbered — the
    // frame corruption whose canary check produced 0xC0000409 on device.
    let chunk_continues = |start: u32| -> bool {
        let e = match pdata_end_of(start) { Some(e) if e > start => e, _ => return false };
        if !entries.contains(&e) { return false; }
        let off = match PeImage::rva_to_off(&pe.sections, start) { Some(o) => o, None => return false };
        let len = (e - start) as usize;
        if pe.bytes.len() < off + len { return false; }
        let code = &pe.bytes[off..off + len];
        let mut d = Decoder::with_ip(64, code, pe.image_base + start as u64, DecoderOptions::NONE);
        let mut insn = Instruction::default();
        let mut last_end = 0u32;
        let mut last_m = iced_x86::Mnemonic::INVALID;
        while d.can_decode() {
            d.decode_out(&mut insn);
            if insn.is_invalid() { break; }
            let m = insn.mnemonic();
            if !matches!(m, iced_x86::Mnemonic::Int3 | iced_x86::Mnemonic::Nop) {
                last_end = (insn.ip() + insn.len() as u64 - pe.image_base) as u32;
                last_m = m;
            }
        }
        use iced_x86::Mnemonic as Mn;
        last_end == e
            && !matches!(last_m, Mn::Ret | Mn::Retf | Mn::Jmp | Mn::Iretd | Mn::Iretq | Mn::Iret)
    };
    // For a function start, its code ends at its .pdata end if known, else at the
    // next known entry (capped) — never a blind 64 KB — EXTENDED through any
    // fall-through chain.
    let func_end = |start: u32| -> u32 {
        let mut cur = start;
        let mut hops = 0;
        loop {
            let pd_end = pdata_end_of(cur);
            let next = entries.range((cur + 1)..).next().copied().unwrap_or(text_end);
            let bound = match pd_end { Some(e) => e.max(next), None => next };
            let bound = bound.min(text_end).max(cur + 1);
            // Follow a genuine fall-through into the next contiguous chunk.
            if hops < 64 && chunk_continues(cur) {
                if let Some(e) = pd_end {
                    if e > cur { cur = e; hops += 1; continue; }
                }
            }
            return bound;
        }
    };

    let mut shard_protos = String::new();
    for &rva in &entries_vec {
        writeln!(shard_protos, "void f_{rva:x}(GowCpu* cpu);")?;
    }
    // Shard by emitted BYTE size so no translation unit is huge (keeps compile
    // memory bounded and parallelizable). Functions are emitted in order.
    const SHARD_MAX_BYTES: usize = 4 * 1024 * 1024;
    let mut sc = String::new();
    let mut si = 0usize;
    let mut sc_has_includes = false;
    let mut flush = |sc: &mut String, si: &mut usize, has: &mut bool| -> Result<()> {
        if *has {
            std::fs::write(out_dir.join(format!("gow_shard_{:03}.c", *si)), &sc)?;
            *si += 1;
            sc.clear();
            *has = false;
        }
        Ok(())
    };
    for &entry_rva in &entries_vec {
        if !sc_has_includes {
            writeln!(sc, "/* Auto-generated by gowrecomp (shard {si}). Do not edit. */")?;
            writeln!(sc, "#include \"gow_protos.h\"")?;
            writeln!(sc, "static inline int64_t sign64(uint64_t v) {{ return (int64_t)v; }}\n")?;
            sc_has_includes = true;
        }
        let range_end = func_end(entry_rva);
        let off = match PeImage::rva_to_off(&sections, entry_rva) { Some(o) => o, None => continue };
        let len = (range_end - entry_rva) as usize;
        if len == 0 || pe.bytes.len() < off + len { continue; }
        let code = &pe.bytes[off..off + len];

        writeln!(sc, "void f_{entry_rva:x}(GowCpu* cpu) {{")?;
        func_count += 1;
        let mut starts: BTreeSet<u32> = BTreeSet::new();
        {
            let mut d = Decoder::with_ip(64, code, image_base + entry_rva as u64, DecoderOptions::NONE);
            let mut ii = Instruction::default();
            while d.can_decode() {
                let ip = d.ip();
                d.decode_out(&mut ii);
                if !ii.is_invalid() { starts.insert((ip - image_base) as u32); }
                else { d.set_position((d.position() - ii.len()) + 1); }
            }
        }
        let mut dec = Decoder::with_ip(64, code, image_base + entry_rva as u64, DecoderOptions::NONE);
        let mut insn = Instruction::default();
        // The last emitted instruction that is not int3/nop padding, with its
        // end RVA. MSVC emits one logical function as several contiguous
        // `.pdata` chunks that FALL THROUGH into one another (e.g. the
        // std::string helper 0xb3810 ends mid-logic at 0xb386b). If this chunk's
        // last real instruction is not a terminator, control continues at
        // `range_end`; we must transfer there or the C function returns early
        // with the guest registers (rbp!) left clobbered — the frame-corruption
        // / 0xC0000409 canary failure seen on device.
        let mut last_real: Option<(iced_x86::Mnemonic, u32)> = None;
        while dec.can_decode() {
            let pos_before = dec.position();
            dec.decode_out(&mut insn);
            let rva = (insn.ip().wrapping_sub(image_base)) as u32;
            if insn.is_invalid() { dec.set_position(pos_before + 1); continue; }
            writeln!(sc, "L_{rva:x}: ; cpu->rip = 0x{:x}ull;", image_base + rva as u64)?;
            insn_count += 1;
            let mut text = String::new();
            fmt.format(&insn, &mut text);
            writeln!(sc, "  /* 0x{rva:x}: {text} */")?;
            emit(&mut sc, &insn, image_base, &iat, &starts, entry_rva, range_end, &entries, &mut unimpl, &mut unimpl_instances)?;
            let m = insn.mnemonic();
            if !matches!(m, iced_x86::Mnemonic::Int3 | iced_x86::Mnemonic::Nop) {
                let end = (insn.ip() + insn.len() as u64).wrapping_sub(image_base) as u32;
                last_real = Some((m, end));
            }
        }
        if let Some((m, end)) = last_real {
            use iced_x86::Mnemonic as Mn;
            let is_term = matches!(m, Mn::Ret | Mn::Retf | Mn::Jmp | Mn::Iretd | Mn::Iretq | Mn::Iret);
            // Only when the last real instruction ends exactly at the chunk
            // boundary (no trailing padding) and the boundary is a known entry.
            if !is_term && end == range_end && entries.contains(&range_end) {
                writeln!(sc, "  f_{range_end:x}(cpu); return;")?;
            }
        }
        writeln!(sc, "}}\n")?;
        if sc.len() >= SHARD_MAX_BYTES {
            flush(&mut sc, &mut si, &mut sc_has_includes)?;
        }
    }
    flush(&mut sc, &mut si, &mut sc_has_includes)?;
    let n_shards = si;

    std::fs::write(out_dir.join("gow_protos.h"), format!("#ifndef GOW_PROTOS_H\n#define GOW_PROTOS_H\n#include \"gow_guest.h\"\n{shard_protos}#endif\n"))?;

    // Function table in its own shard.
    let mut tbl = String::new();
    writeln!(tbl, "/* Auto-generated by gowrecomp. Do not edit. */")?;
    writeln!(tbl, "#include \"gow_guest.h\"")?;
    writeln!(tbl, "{}", shard_protos)?;
    writeln!(tbl, "const GowFuncEntry gow_func_table[] = {{")?;
    for &rva in &entries {
        writeln!(tbl, "  {{ 0x{rva:x}u, f_{rva:x} }},")?;
    }
    writeln!(tbl, "  {{ 0u, 0 }}")?;
    writeln!(tbl, "}};")?;
    writeln!(tbl, "const unsigned gow_func_count = {};", entries.len())?;
    // Embed the import manifest so the runtime can install the Win32 resolver
    // without a separate file on the device.
    let manifest = build_manifest(pe);
    // Emit the manifest as a C string literal (raw strings are C++, and the CI
    // compiles the generated code as C). Escape backslash and quote, and split
    // across lines to stay within compiler line limits.
    write!(tbl, "const char gow_manifest_json[] =")?;
    for line in manifest.split_inclusive('\n') {
        let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
        write!(tbl, "\n  \"{}\"", escaped.trim_end_matches('\n'))?;
    }
    writeln!(tbl, ";")?;
    std::fs::write(out_dir.join("gow_table.c"), &tbl)?;
    // Also write it standalone for tooling/inspection.
    let _ = std::fs::write(out_dir.join("manifest.json"), &manifest);

    // Keep `c` for the returned source only as a small summary; do not write a
    // monolithic file (shards are the build unit).
    let _ = &mut c;
    let _ = &mut shard_protos;
    let _ = n_shards;

    Ok(LiftOutput { function_count: func_count, instruction_count: insn_count, unimpl, unimpl_instances })
}

fn build_manifest(pe: &PeImage) -> String {
    let mut m = String::new();
    m.push_str("{\n");
    m.push_str(&format!("  \"image_base\": {},\n", pe.image_base));
    m.push_str(&format!("  \"entry_rva\": {},\n", pe.entry_rva));
    m.push_str(&format!("  \"size_of_image\": {},\n", pe.size_of_image));
    m.push_str("  \"imports\": [\n");
    for (i, imp) in pe.imports.iter().enumerate() {
        let comma = if i + 1 == pe.imports.len() { "" } else { "," };
        m.push_str(&format!(
            "    {{\"dll\":\"{}\",\"func\":\"{}\",\"iat_rva\":{}}}{comma}\n",
            imp.dll.replace('\\', "\\\\"), imp.func.replace('\\', "\\\\"), imp.iat_rva
        ));
    }
    m.push_str("  ]\n}\n");
    m
}

fn trap(c: &mut String, insn: &Instruction, image_base: u64, unimpl: &mut BTreeSet<String>, n: &mut u64) -> Result<()> {
    let key = format!("{:?}", insn.mnemonic());
    unimpl.insert(key);
    *n += 1;
    let rva = (insn.ip().wrapping_sub(image_base)) as u32;
    writeln!(c, "  gow_unimpl(cpu, 0x{rva:x}u, {});", insn.mnemonic() as u32)?;
    Ok(())
}

/// Emit C for one instruction. Returns true if it ended the basic block.
fn emit(c: &mut String, insn: &Instruction, image_base: u64, iat: &std::collections::HashMap<u32,u32>, starts: &BTreeSet<u32>, func_start: u32, func_end: u32, entries: &BTreeSet<u32>, unimpl: &mut BTreeSet<String>, un: &mut u64) -> Result<bool> {
    use iced_x86::Mnemonic::*;
    let m = insn.mnemonic();

    // Inline a direct `jmp`/`jcc` target that is inside this function and is a
    // real decoded instruction as a goto; otherwise tail-call / trampoline.
    let inline_target = |tgt_rva: u32| -> Option<String> {
        if tgt_rva >= func_start && tgt_rva < func_end && starts.contains(&tgt_rva) {
            Some(format!("L_{tgt_rva:x}"))
        } else {
            None
        }
    };

    // --- control flow ---
    match m {
        Loop | Loope | Loopne => {
            let t = (insn.near_branch64() - image_base) as u32;
            let cc = match m { Loope => Some(0x4u32), Loopne => Some(0x5u32), _ => None };
            let inlined = t >= func_start && t < func_end && starts.contains(&t);
            writeln!(c, "  {{ uint64_t _c = (GOW_REG(cpu, rcx) & 0xffffffffull); _c = (_c - 1) & 0xffffffffull; GOW_REG(cpu, rcx) = (GOW_REG(cpu, rcx) & 0xffffffff00000000ull) | _c;")?;
            let cond = match cc {
                Some(ccv) => format!("_c != 0 && gow_test_cc(cpu, {ccv})"),
                None => "_c != 0".to_string(),
            };
            if inlined {
                writeln!(c, "     if ({cond}) goto L_{t:x}; }}")?;
            } else {
                writeln!(c, "     if ({cond}) {{ gow_trampoline(cpu, 0x{t:x}u); }} }}")?;
            }
            return Ok(false);
        }
        Jrcxz | Jecxz => {
            let t = (insn.near_branch64() - image_base) as u32;
            let reg = if m == Jrcxz { "rcx" } else { "rcx" };
            if t >= func_start && t < func_end && starts.contains(&t) {
                writeln!(c, "  if ((GOW_REG(cpu, {reg}) & 0xffffffffull) == 0) goto L_{t:x};")?;
            } else {
                writeln!(c, "  if ((GOW_REG(cpu, {reg}) & 0xffffffffull) == 0) {{ gow_trampoline(cpu, 0x{t:x}u); }}")?;
            }
            return Ok(false);
        }
        Jmp => {
            if insn.op0_kind() == OpKind::NearBranch64 {
                let t = (insn.near_branch64() - image_base) as u32;
                if let Some(lbl) = inline_target(t) {
                    writeln!(c, "  goto {lbl};")?;
                } else if entries.contains(&t) {
                    writeln!(c, "  f_{t:x}(cpu); return;")?;   // tail call
                } else {
                    // Unknown in-image target (e.g. computed): route through trampoline.
                    writeln!(c, "  gow_jump(cpu, 0x{t:x}u); return;")?;
                }
            } else if insn.op0_kind() == OpKind::Memory {
                // `jmp qword [rip+disp]` is an import thunk (`jmp [__imp_X]`).
                // The IAT slot holds no host pointer, so dispatch it as an import
                // call just like `call [IAT]`.
                if let Some(va) = mem_target_va(insn, image_base) {
                    if let Some(slot_rva) = iat_slot(iat, va, image_base) {
                        writeln!(c, "  gow_import_call(cpu, 0x{slot_rva:x}u); return;")?;
                        return Ok(true);
                    }
                }
                match operand_read(insn, 0, image_base) {
                    Some(e) => writeln!(c, "  gow_call_indirect(cpu, (uint64_t)({e})); return;")?,
                    None => { trap(c, insn, image_base, unimpl, un)?; }
                }
            } else {
                match operand_read(insn, 0, image_base) {
                    Some(e) => writeln!(c, "  gow_call_indirect(cpu, (uint64_t)({e})); return;")?,
                    None => { trap(c, insn, image_base, unimpl, un)?; }
                }
            }
            return Ok(true);
        }
        Call => {
            let ret = (insn.next_ip() - image_base) as u32;
            if insn.op0_kind() == OpKind::NearBranch64 {
                let t = (insn.near_branch64() - image_base) as u32;
                // Emulate the x86 `call`: push the return address so the callee's
                // [rsp]/shadow-space layout matches real hardware, then pop it.
                writeln!(c, "  gow_push64(cpu, 0x{ret:x}u);")?;
                writeln!(c, "  gow_trace_enter(cpu, 0x{t:x}u);")?;
                if entries.contains(&t) {
                    writeln!(c, "  f_{t:x}(cpu);")?;
                } else {
                    writeln!(c, "  gow_trampoline(cpu, 0x{t:x}u);")?;
                }
                writeln!(c, "  gow_pop64(cpu);")?;
            } else if insn.op0_kind() == OpKind::Memory {
                // `call qword [rip+disp]` — usually an IAT import thunk.
                if let Some(va) = mem_target_va(insn, image_base) {
                    if let Some(slot_rva) = iat_slot(iat, va, image_base) {
                        // Push the return address first so the import handler sees
                        // the standard MS x64 layout (5th arg at [rsp+0x28]).
                        writeln!(c, "  gow_push64(cpu, 0x{ret:x}u);")?;
                        writeln!(c, "  gow_import_call(cpu, 0x{slot_rva:x}u);")?;
                        writeln!(c, "  gow_pop64(cpu);")?;
                        return Ok(false);
                    }
                }
                // `call qword [mem]` loads the target pointer FROM [mem] and
                // calls it — it is NOT a call to the address of [mem]. Using
                // mem_expr here passed the slot address (e.g. rbx+0x10) as if it
                // were the function, so an engine object-relative virtual call
                // `call [rbx+0x10]` dispatched to 0xb400...10 and mis-called.
                // operand_read emits the proper gow_load of the slot.
                let loaded = operand_read(insn, 0, image_base)
                    .unwrap_or_else(|| format!("gow_load(cpu, {}, 8)", mem_expr(insn, image_base)));
                writeln!(c, "  gow_push64(cpu, 0x{ret:x}u);")?;
                writeln!(c, "  gow_call_ptr(cpu, (uint64_t)({loaded}));")?;
                writeln!(c, "  gow_pop64(cpu);")?;
            } else {
                writeln!(c, "  gow_push64(cpu, 0x{ret:x}u);")?;
                match operand_read(insn, 0, image_base) {
                    Some(e) => writeln!(c, "  gow_call_ptr(cpu, (uint64_t)({e}));")?,
                    None => { trap(c, insn, image_base, unimpl, un)?; }
                }
                writeln!(c, "  gow_pop64(cpu);")?;
            }
            return Ok(false);
        }
        Ret => {
            writeln!(c, "  return;")?;
            return Ok(true);
        }
        Leave => {
            writeln!(c, "  GOW_REG(cpu, rsp) = GOW_REG(cpu, rbp);")?;
            writeln!(c, "  GOW_REG(cpu, rbp) = gow_pop64(cpu);")?;
            return Ok(false);
        }
        j if is_jcc(j) => {
            let cc = cc_const(insn.condition_code());
            let t = (insn.near_branch64() - image_base) as u32;
            if let Some(lbl) = inline_target(t) {
                writeln!(c, "  if (gow_test_cc(cpu, {cc})) goto {lbl};")?;
            } else if entries.contains(&t) {
                writeln!(c, "  if (gow_test_cc(cpu, {cc})) {{ f_{t:x}(cpu); return; }}")?;
            } else {
                writeln!(c, "  if (gow_test_cc(cpu, {cc})) {{ gow_trampoline(cpu, 0x{t:x}u); return; }}")?;
            }
            return Ok(false);
        }
        cm if is_cmov(cm) => {
            let cc = cc_const(insn.condition_code());
            let dst = reg_at(insn, 0);
            if let Some(src) = operand_read(insn, 1, image_base) {
                writeln!(c, "  if (gow_test_cc(cpu, {cc})) {{ {} }}", reg_write(dst, &src))?;
            }
            return Ok(false);
        }
        set if is_setcc(set) => {
            let cc = cc_const(insn.condition_code());
            let dst = reg_at(insn, 0);
            writeln!(c, "  {{ uint64_t _c = gow_test_cc(cpu, {cc}); {} }}", reg_write(dst, "_c"))?;
            return Ok(false);
        }
        _ => {}
    }

    // --- data movement ---
    match m {
        Mov => {
            let dst = insn.op0_kind();
            let dst_reg = reg_at(insn, 0);
            let src = operand_read(insn, 1, image_base);
            if dst == OpKind::Memory {
                let n = mem_size(insn);
                let v = src.unwrap_or_else(|| "0".into());
                writeln!(c, "  gow_store(cpu, {}, {v}, {n});", mem_expr(insn, image_base))?;
            } else if let Some(v) = src {
                writeln!(c, "  {}", reg_write(dst_reg, &v))?;
            }
            return Ok(false);
        }
        Movzx | Movsx | Movsxd => {
            let dst_reg = reg_at(insn, 0);
            let w = gpr_width(dst_reg);
            let srcw = op_width(insn, 1);
            let v = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            let mask = if srcw >= 64 { u64::MAX } else { (1u64 << srcw) - 1 };
            let sign = matches!(m, Movsx | Movsxd);
            if sign {
                let shift = 64 - srcw.min(63);
                let ex = if srcw >= 64 { v.clone() } else { format!("((int64_t)(int{srcw}_t)({v}))") };
                let _ = (mask, shift);
                writeln!(c, "  {}", reg_write(dst_reg, &ex))?;
            } else {
                let _ = w;
                writeln!(c, "  {}", reg_write(dst_reg, &format!("((uint64_t)({v}) & 0x{mask:x}ull)")))?;
            }
            return Ok(false);
        }
        Lea => {
            let dst = reg_at(insn, 0);
            writeln!(c, "  {}", reg_write(dst, &mem_expr(insn, image_base)))?;
            return Ok(false);
        }
        Push => {
            let v = operand_read(insn, 0, image_base).unwrap_or_else(|| "0".into());
            writeln!(c, "  gow_push64(cpu, (uint64_t)({v}));")?;
            return Ok(false);
        }
        Pop => {
            let dst = reg_at(insn, 0);
            writeln!(c, "  {}", reg_write(dst, "gow_pop64(cpu)"))?;
            return Ok(false);
        }
        Xchg => {
            let a = reg_at(insn, 0);
            let b = reg_at(insn, 1);
            writeln!(c, "  {{ uint64_t _t = {}; {} {} }}",
                reg_read(a), reg_write(a, &reg_read(b)), reg_write(b, "_t"))?;
            return Ok(false);
        }
        _ => {}
    }

    // --- arithmetic / logic with flags ---
    match m {
        Add | Sub | And | Or | Xor | Cmp | Test => {
            let dst = insn.op0_kind();
            let a_expr = if dst == OpKind::Memory {
                format!("gow_load(cpu, {}, {})", mem_expr(insn, image_base), mem_size(insn))
            } else {
                reg_read(reg_at(insn, 0))
            };
            let b_expr = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            let bits = if dst == OpKind::Memory { mem_size(insn) * 8 } else { gpr_width(reg_at(insn, 0)) };
            let call = match m {
                Add => format!("gow_add64(cpu, _a, _b, {bits})"),
                Sub | Cmp => format!("gow_sub64(cpu, _a, _b, {bits})"),
                And | Test => format!("gow_logic64(cpu, _a & _b, {bits})"),
                Or => format!("gow_logic64(cpu, _a | _b, {bits})"),
                Xor => format!("gow_logic64(cpu, _a ^ _b, {bits})"),
                _ => unreachable!(),
            };
            let store = if m == Cmp || m == Test {
                String::new()
            } else if dst == OpKind::Memory {
                format!("gow_store(cpu, {}, _r, {});", mem_expr(insn, image_base), mem_size(insn))
            } else {
                reg_write(reg_at(insn, 0), "_r")
            };
            writeln!(c, "  {{ uint64_t _a = {a_expr}; uint64_t _b = {b_expr}; uint64_t _r = {call}; {store} }}")?;
            return Ok(false);
        }
        Inc | Dec => {
            let dst = insn.op0_kind();
            let a_expr = if dst == OpKind::Memory {
                format!("gow_load(cpu, {}, {})", mem_expr(insn, image_base), mem_size(insn))
            } else {
                reg_read(reg_at(insn, 0))
            };
            let bits = if dst == OpKind::Memory { mem_size(insn) * 8 } else { gpr_width(reg_at(insn, 0)) };
            let call = if m == Inc { format!("gow_add64(cpu, _a, 1, {bits})") } else { format!("gow_sub64(cpu, _a, 1, {bits})") };
            let store = if dst == OpKind::Memory {
                format!("gow_store(cpu, {}, _r, {});", mem_expr(insn, image_base), mem_size(insn))
            } else {
                reg_write(reg_at(insn, 0), "_r")
            };
            writeln!(c, "  {{ uint64_t _a = {a_expr}; uint64_t _r = {call}; {store} }}")?;
            return Ok(false);
        }
        Not => {
            let dst = insn.op0_kind();
            if dst == OpKind::Memory {
                let n = mem_size(insn);
                let mask = if n == 8 { u64::MAX } else { (1u64 << (n * 8)) - 1 };
                writeln!(c, "  gow_store(cpu, {}, ~gow_load(cpu, {}, {n}) & 0x{mask:x}ull, {n});", mem_expr(insn, image_base), mem_expr(insn, image_base))?;
            } else {
                let r = reg_at(insn, 0);
                let w = gpr_width(r);
                let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
                writeln!(c, "  {}", reg_write(r, &format!("(~{} & 0x{mask:x}ull)", reg_read(r))))?;
            }
            return Ok(false);
        }
        Neg => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _r = gow_sub64(cpu, 0, _a, {w}); {} }}", reg_write(r, "_r"))?;
            return Ok(false);
        }
        Adc | Sbb => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            let b = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            let sub = m == Sbb;
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _b = {b}; uint64_t _c = (GOW_FLAGS(cpu) & 1);")?;
            if sub {
                writeln!(c, "     uint64_t _r = gow_sub64(cpu, _a, _b + _c, {w}); {} }}", reg_write(r, "_r"))?;
            } else {
                writeln!(c, "     uint64_t _r = gow_add64(cpu, _a, _b + _c, {w}); {} }}", reg_write(r, "_r"))?;
            }
            return Ok(false);
        }
        Shl | Sal | Shr | Sar | Rol | Ror => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            let cnt = operand_read(insn, 1, image_base).unwrap_or_else(|| "1".into());
            let op = match m {
                Shl | Sal => "shl",
                Shr => "shr",
                Sar => "sar",
                Rol => "rol",
                Ror => "ror",
                _ => "shl",
            };
            let store = reg_write(r, "_r");
            let id = shift_id(op);
            let _ = a;
            writeln!(c, "  {{ uint64_t _n = ({cnt}) & 0x3f; uint64_t _a = {a}; uint64_t _r = gow_shift(cpu, _a, _n, {w}, {id}); {store} }}")?;
            return Ok(false);
        }
        Rcl | Rcr => {
            // Rotate through CF; the helper masks the count and sets CF (and OF
            // for a single-bit rotate) exactly.
            let r = reg_at(insn, 0);
            let mem = insn.op0_kind() == OpKind::Memory;
            let w = if mem { mem_bytes(insn) * 8 } else { gpr_width(r) };
            let a = if mem {
                format!("gow_load(cpu, {}, {})", mem_expr(insn, image_base), mem_bytes(insn))
            } else { reg_read(r) };
            let cnt = operand_read(insn, 1, image_base).unwrap_or_else(|| "1".into());
            let right = if m == Rcr { 1 } else { 0 };
            let store = if mem {
                format!("gow_store(cpu, {}, _r, {});", mem_expr(insn, image_base), mem_bytes(insn))
            } else { reg_write(r, "_r") };
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _r = gow_rcl_rcr(cpu, _a, ({cnt}), {w}, {right}); {store} }}")?;
            return Ok(false);
        }
        Bt | Bts | Btr | Btc => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            let idx = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            let op = match m { Bt => 0, Bts => 1, Btr => 2, Btc => 3, _ => 0 };
            let store = reg_write(r, "_r");
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _i = ({idx}) & 0x3f; uint64_t _r = gow_bt(cpu, _a, _i, {w}, {op}); {store} }}")?;
            return Ok(false);
        }
        Bsf | Bsr => {
            let r = reg_at(insn, 0);
            let a = reg_read(r);
            let b = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            let f = if m == Bsf { "gow_bsf" } else { "gow_bsr" };
            writeln!(c, "  {}", reg_write(r, &format!("{f}(cpu, {b})")))?;
            let _ = a;
            return Ok(false);
        }
        Bswap => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            let bits = w;
            let e = if bits == 64 { format!("__builtin_bswap64({a})") } else { format!("((uint64_t)__builtin_bswap32((uint32_t)({a})))") };
            writeln!(c, "  {}", reg_write(r, &e))?;
            return Ok(false);
        }
        _ => {}
    }

    // --- more integer ops ---
    match m {
        Imul => {
            // 2- or 3-operand integer multiply
            let dst = insn.op0_kind();
            let r = reg_at(insn, 0);
            let w = if dst == OpKind::Memory { mem_size(insn) * 8 } else { gpr_width(r) };
            let a = if insn.op_count() >= 3 {
                operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into())
            } else {
                reg_read(r)
            };
            let b = if insn.op_count() >= 3 {
                operand_read(insn, 2, image_base).unwrap_or_else(|| "0".into())
            } else {
                operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into())
            };
            let store = if dst == OpKind::Memory {
                format!("gow_store(cpu, {}, _r, {});", mem_expr(insn, image_base), mem_size(insn))
            } else {
                reg_write(r, "_r")
            };
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _b = {b}; uint64_t _r = gow_imul64(cpu, _a, _b, {w}); {store} }}")?;
            return Ok(false);
        }
        Mul => {
            let b = operand_read(insn, 0, image_base).unwrap_or_else(|| "0".into());
            let w = if insn.op0_kind() == OpKind::Memory { mem_size(insn) * 8 } else { gpr_width(reg_at(insn, 0)) };
            writeln!(c, "  gow_mul_rax_rdx(cpu, {b}, {w});")?;
            return Ok(false);
        }
        Div | Idiv => {
            let b = operand_read(insn, 0, image_base).unwrap_or_else(|| "0".into());
            let w = if insn.op0_kind() == OpKind::Memory { mem_size(insn) * 8 } else { gpr_width(reg_at(insn, 0)) };
            let s = if m == Idiv { 1 } else { 0 };
            writeln!(c, "  gow_div_rax_rdx(cpu, {b}, {w}, {s});")?;
            return Ok(false);
        }
        Popcnt => {
            let r = reg_at(insn, 0);
            let a = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            writeln!(c, "  {{ uint64_t _v = {a}; gow_logic64(cpu, _v, 64); gow_logic64(cpu, 0, 64); {} }}",
                reg_write(r, &format!("(uint64_t)__builtin_popcountll(_v)")))?;
            return Ok(false);
        }
        Xadd => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            let b = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _b = {b}; {} gow_add64(cpu, _b, _a, {w}); {} }}",
                reg_write(insn.op0_register(), "_b"), reg_write(insn.op1_register(), "_a"))?;
            return Ok(false);
        }
        Cmpxchg => {
            let r = reg_at(insn, 0);
            let w = gpr_width(r);
            let a = reg_read(r);
            let b = operand_read(insn, 1, image_base).unwrap_or_else(|| "0".into());
            writeln!(c, "  {{ uint64_t _a = {a}; uint64_t _b = {b}; uint64_t _acc = GOW_REG(cpu, rax) & 0x{:x}ull; gow_sub64(cpu, _acc, _a, {w}); if (GOW_FLAGS(cpu) & (1<<6)) {{ {} gow_store(cpu, {}, _b, {}); }} else {{ {} }} }}",
                if w==64 { u64::MAX } else { (1u64<<w)-1 },
                reg_write(insn.op0_register(), "_b"),
                mem_expr(insn, image_base), w/8,
                reg_write(Register::RAX, "_a"))?;
            return Ok(false);
        }
        Lahf => { writeln!(c, "  gow_lahf(cpu);")?; return Ok(false); }
        Sahf => { writeln!(c, "  gow_sahf(cpu);")?; return Ok(false); }
        Pushf | Pushfq => { writeln!(c, "  gow_push64(cpu, gow_popfq(cpu));")?; return Ok(false); }
        Popf | Popfq => { writeln!(c, "  GOW_FLAGS(cpu) = gow_pop64(cpu);")?; return Ok(false); }
        Cpuid => { writeln!(c, "  gow_cpuid(cpu);")?; return Ok(false); }
        Ldmxcsr => { writeln!(c, "  gow_ldmxcsr(cpu, {});", mem_expr(insn, image_base))?; return Ok(false); }
        Stmxcsr => { writeln!(c, "  gow_stmxcsr(cpu, {});", mem_expr(insn, image_base))?; return Ok(false); }
        Prefetchnta | Prefetcht0 | Prefetcht1 | Prefetcht2 | Prefetchw | Prefetchwt1 => {
            return Ok(false); // architectural hint; no state change
        }
        _ => {}
    }

    // --- SSE (subset with runtime support) ---
    if let Some(line) = emit_sse(insn, image_base) {
        write!(c, "{line}")?;
        return Ok(false);
    }

    // --- misc ---
    match m {
        Cdq | Cqo => {
            if m == Cdq {
                writeln!(c, "  GOW_REG(cpu, rdx) = (GOW_REG(cpu, rax) & 0x80000000ull) ? 0xffffffffull : 0ull;")?;
            } else {
                writeln!(c, "  GOW_REG(cpu, rdx) = ((int64_t)GOW_REG(cpu, rax) < 0) ? 0xffffffffffffffffull : 0ull;")?;
            }
            return Ok(false);
        }
        Cdqe => { writeln!(c, "  GOW_REG(cpu, rax) = (int64_t)(int32_t)(uint32_t)GOW_REG(cpu, rax);")?; return Ok(false); }
        Cwde => { writeln!(c, "  GOW_REG(cpu, rax) = (uint32_t)(int16_t)(uint16_t)GOW_REG(cpu, rax) & 0xffffffff;")?; return Ok(false); }
        Cbw => { writeln!(c, "  GOW_REG(cpu, rax) = (GOW_REG(cpu, rax) & ~0xffffull) | (uint16_t)(int8_t)(uint8_t)(GOW_REG(cpu, rax));")?; return Ok(false); }
        Cwd => { writeln!(c, "  GOW_REG(cpu, rdx) = (GOW_REG(cpu, rdx) & ~0xffffull) | (uint16_t)(((int16_t)GOW_REG(cpu, rax) < 0) ? 0xffff : 0);")?; return Ok(false); }
        Stc => { writeln!(c, "  GOW_FLAGS(cpu) |= 1;")?; return Ok(false); }
        Clc => { writeln!(c, "  GOW_FLAGS(cpu) &= ~1ull;")?; return Ok(false); }
        Cmc => { writeln!(c, "  GOW_FLAGS(cpu) ^= 1;")?; return Ok(false); }
        Cld => { writeln!(c, "  GOW_FLAGS(cpu) &= ~(1ull<<10);")?; return Ok(false); }
        Std => { writeln!(c, "  GOW_FLAGS(cpu) |= (1ull<<10);")?; return Ok(false); }
        Nop | Int3 | Endbr64 | Pause | Cli | Sti | Cmpsb | Cmpsw | Cmpsd | Cmpsq | Stosb | Stosw | Stosd | Stosq => {
            if matches!(m, Stosb | Stosw | Stosd | Stosq) {
                let sz = match m { Stosb => 1, Stosw => 2, Stosd => 4, _ => 8 };
                writeln!(c, "  gow_stos(cpu, {sz});")?;
            }
            return Ok(false);
        }
        Movsb | Movsw | Movsd | Movsq => {
            let sz = match m { Movsb => 1, Movsw => 2, Movsd => 4, _ => 8 };
            // movsd is ambiguous with the SSE scalar load (handled in emit_sse
            // before this match); reaching here means the string form.
            writeln!(c, "  gow_movs(cpu, {sz});")?;
            return Ok(false);
        }
        Lodsb | Lodsw | Lodsd | Lodsq => {
            let sz = match m { Lodsb => 1, Lodsw => 2, Lodsd => 4, _ => 8 };
            writeln!(c, "  gow_lods(cpu, {sz});")?;
            return Ok(false);
        }
        Scasb | Scasw | Scasd | Scasq => {
            let sz = match m { Scasb => 1, Scasw => 2, Scasd => 4, _ => 8 };
            writeln!(c, "  gow_scas(cpu, {sz});")?;
            return Ok(false);
        }
        Cmpsq | Cmpsd | Cmpsw | Cmpsb => {
            let sz = match m { Cmpsb => 1, Cmpsw => 2, Cmpsd => 4, _ => 8 };
            writeln!(c, "  gow_cmps(cpu, {sz});")?;
            return Ok(false);
        }
        Enter => {
            let frame = insn.immediate16() as u64;
            let level = insn.immediate8() as u64 & 0x1f;
            writeln!(c, "  gow_enter(cpu, {frame}, {level});")?;
            return Ok(false);
        }
        Xlatb => { writeln!(c, "  GOW_REG(cpu, rax) = (GOW_REG(cpu, rax) & 0xffffffffffffff00ull) | (uint64_t)(*(uint8_t*)(GOW_REG(cpu, rbx) + (GOW_REG(cpu, rax) & 0xff)));")?; return Ok(false); }
        // --- x87 ---
        Fld => {
            if insn.op0_kind() == OpKind::Memory {
                let is32 = insn.memory_size().size() == 4;
                let m = if is32 { format!("(double)*(float*)(uintptr_t)({})", mem_expr(insn, image_base)) } else { format!("*(double*)(uintptr_t)({})", mem_expr(insn, image_base)) };
                writeln!(c, "  gow_x87_ld(cpu, {m}, {});", if is32 {1} else {0})?;
            } else {
                let i = insn.op0_register() as u32;
                writeln!(c, "  gow_x87_push(cpu, gow_x87_get(cpu, {}));", x87_index(i))?;
            }
            return Ok(false);
        }
        Fld1 => { writeln!(c, "  gow_x87_push(cpu, 1.0);")?; return Ok(false); }
        Fldz => { writeln!(c, "  gow_x87_push(cpu, 0.0);")?; return Ok(false); }
        Fild => {
            let n = mem_bytes(insn);
            let addr = mem_expr(insn, image_base);
            writeln!(c, "  gow_x87_fild(cpu, gow_load(cpu, {addr}, {n}), {});", n * 8)?;
            return Ok(false);
        }
        Fist | Fistp | Fisttp => {
            let n = mem_bytes(insn);
            let (pop, trunc) = match m { Fist => (0, 0), Fistp => (1, 0), _ => (1, 1) };
            let addr = mem_expr(insn, image_base);
            writeln!(c, "  gow_store(cpu, {addr}, (uint64_t)gow_x87_fist(cpu, {}, {pop}, {trunc}), {n});", n * 8)?;
            return Ok(false);
        }
        Fiadd | Fimul | Fisub | Fisubr | Fidiv | Fidivr => {
            // Integer memory operand is converted to f64, then the existing
            // st0-op-m helper performs the arithmetic exactly.
            let op = match m { Fiadd=>0u32, Fisub=>1, Fimul=>2, Fidiv=>3, Fisubr=>4, Fidivr=>5, _=>0 };
            let n = mem_bytes(insn);
            let addr = mem_expr(insn, image_base);
            writeln!(c, "  gow_x87_arith_m(cpu, gow_x87_i2f(cpu, gow_load(cpu, {addr}, {n}), {}), {op}, 0);", n * 8)?;
            return Ok(false);
        }
        Ficomp => {
            let n = mem_bytes(insn);
            let addr = mem_expr(insn, image_base);
            writeln!(c, "  gow_x87_com_m(cpu, gow_x87_i2f(cpu, gow_load(cpu, {addr}, {n}), {}), 1);", n * 8)?;
            return Ok(false);
        }
        Fcmovb | Fcmove | Fcmovbe | Fcmovu | Fcmovnb | Fcmovne | Fcmovnbe | Fcmovnu => {
            let i = x87_index(insn.op1_register() as u32);
            let cc = match m {
                Fcmovb => 2u32, Fcmove => 4, Fcmovbe => 6, Fcmovu => 10,
                Fcmovnb => 3, Fcmovne => 5, Fcmovnbe => 7, _ => 11,
            };
            writeln!(c, "  gow_x87_fcmov(cpu, {i}, {cc});")?;
            return Ok(false);
        }
        Fstpnce => {
            let i = x87_index(insn.op0_register() as u32);
            writeln!(c, "  gow_x87_set(cpu, {i}, gow_x87_get(cpu, 0));")?;
            writeln!(c, "  gow_x87_pop(cpu);")?;
            return Ok(false);
        }
        Fst | Fstp => {
            let pop = m == Fstp;
            if insn.op0_kind() == OpKind::Memory {
                let is32 = insn.memory_size().size() == 4;
                let addr = mem_expr(insn, image_base);
                if is32 {
                    writeln!(c, "  *(float*)(uintptr_t)({addr}) = (float)gow_x87_get(cpu, 0);")?;
                } else {
                    writeln!(c, "  *(double*)(uintptr_t)({addr}) = gow_x87_get(cpu, 0);")?;
                }
                if pop { writeln!(c, "  gow_x87_pop(cpu);")?; }
            } else {
                let i = x87_index(insn.op0_register() as u32);
                writeln!(c, "  gow_x87_set(cpu, {i}, gow_x87_get(cpu, 0));")?;
                if pop { writeln!(c, "  gow_x87_pop(cpu);")?; }
            }
            return Ok(false);
        }
        Fadd | Fsub | Fmul | Fdiv | Fsubr | Fdivr => {
            let op = match m { Fadd=>0u32, Fsub=>1, Fmul=>2, Fdiv=>3, Fsubr=>4, Fdivr=>5, _=>0 };
            let pop = false;
            // Detect the "Faddp/Fsubp..." forms: destination is ST(0) with pop encoded in mnemonic name suffix handled by iced as separate mnemonics (Faddp etc).
            if insn.op0_kind() == OpKind::Memory {
                let is32 = insn.memory_size().size() == 4;
                let md = if is32 { format!("(double)*(float*)(uintptr_t)({})", mem_expr(insn, image_base)) } else { format!("*(double*)(uintptr_t)({})", mem_expr(insn, image_base)) };
                writeln!(c, "  gow_x87_arith_m(cpu, {md}, {op}, 0);")?;
            } else {
                let i = x87_index(insn.op0_register() as u32);
                writeln!(c, "  gow_x87_arith(cpu, {i}, {op}, 0);")?;
            }
            let _ = pop;
            return Ok(false);
        }
        Faddp | Fsubp | Fmulp | Fdivp | Fsubrp | Fdivrp => {
            let op = match m { Faddp=>0u32, Fsubp=>1, Fmulp=>2, Fdivp=>3, Fsubrp=>4, Fdivrp=>5, _=>0 };
            let i = x87_index(insn.op0_register() as u32);
            writeln!(c, "  gow_x87_arith(cpu, {i}, {op}, 1);")?;
            return Ok(false);
        }
        Fcom | Fcomp => {
            let pop = m == Fcomp;
            if insn.op0_kind() == OpKind::Memory {
                let is32 = insn.memory_size().size() == 4;
                let md = if is32 { format!("(double)*(float*)(uintptr_t)({})", mem_expr(insn, image_base)) } else { format!("*(double*)(uintptr_t)({})", mem_expr(insn, image_base)) };
                writeln!(c, "  gow_x87_com_m(cpu, {md}, {pop2});", pop2 = if pop {1} else {0})?;
            } else {
                let i = x87_index(insn.op0_register() as u32);
                writeln!(c, "  gow_x87_com(cpu, {i}, {});", if pop {1} else {0})?;
            }
            return Ok(false);
        }
        Fucomip | Fcomip => { let i = x87_index(insn.op0_register() as u32); writeln!(c, "  gow_x87_fucomi(cpu, {i}, 1);")?; return Ok(false); }
        Fucomi | Fcomi => { let i = x87_index(insn.op0_register() as u32); writeln!(c, "  gow_x87_fucomi(cpu, {i}, 0);")?; return Ok(false); }
        Fxch => { let i = x87_index(insn.op0_register() as u32); writeln!(c, "  gow_x87_fxch(cpu, {i});")?; return Ok(false); }
        Fnstsw | Fstsw => { writeln!(c, "  *(uint16_t*)(uintptr_t)({}) = (uint16_t)gow_x87_fstsw(cpu);", mem_expr(insn, image_base))?; return Ok(false); }
        Fnstenv => { writeln!(c, "  gow_x87_fnstenv(cpu, {});", mem_expr(insn, image_base))?; return Ok(false); }
        Fldenv => { writeln!(c, "  gow_x87_fldenv(cpu, {});", mem_expr(insn, image_base))?; return Ok(false); }
        Frstor => { writeln!(c, "  gow_x87_frstor(cpu, {});", mem_expr(insn, image_base))?; return Ok(false); }
        Fnstcw | Fstcw => { writeln!(c, "  *(uint16_t*)(uintptr_t)({}) = (uint16_t)gow_x87_fnstcw(cpu);", mem_expr(insn, image_base))?; return Ok(false); }
        Fldcw => { writeln!(c, "  gow_x87_fldcw(cpu, gow_load(cpu, {}, 2));", mem_expr(insn, image_base))?; return Ok(false); }
        Fninit | Finit => { writeln!(c, "  gow_x87_init(cpu);")?; return Ok(false); }
        Ffree | Fdecstp | Fincstp | Fnop | Fclex | Fnclex | Wait => { return Ok(false); }
        Int3 | Int | Int1 => { return Ok(false); }  // debugger traps: ignore in a port
        // --- privileged / no-state ops ------------------------------------
        // These only turn up in the image in regions that decode as data; the
        // game never relies on their effect, so a no-op (or a plausible value
        // for XGETBV) is the honest, harmless lowering.
        Reservednop | Hlt | Clts | Invd | Wbinvd => { return Ok(false); }
        Sgdt => {
            // m16&64 (10 bytes in 64-bit mode): 2-byte limit then 8-byte base.
            // We do not model the GDT, so store a null descriptor rather than
            // inventing one.
            let addr = mem_expr(insn, image_base);
            writeln!(c, "  gow_store(cpu, {addr}, 0, 2);")?;
            writeln!(c, "  gow_store(cpu, {addr} + 2, 0, 8);")?;
            return Ok(false);
        }
        Xgetbv => {
            // Return XCR0 = x87|SSE|AVX (all supported OS state bits) in RDX:RAX.
            writeln!(c, "  {{ uint32_t _x = (uint32_t)(GOW_REG(cpu, rcx) & 0xffffffffull); uint64_t _v = (_x == 0) ? 0x7ull : 0ull; GOW_REG(cpu, rax) = _v; GOW_REG(cpu, rdx) = 0; }}")?;
            return Ok(false);
        }
        _ => {}
    }

    // --- fallback: explicit trap ---
    trap(c, insn, image_base, unimpl, un)?;
    Ok(false)
}

fn x87_index(r: u32) -> u32 {
    // iced reports ST(n) register numbers; map to stack index 0..7.
    // ST0 is the lowest of the ST registers' encoding range.
    use iced_x86::Register::*;
    match r {
        x if x == ST0 as u32 => 0, x if x == ST1 as u32 => 1, x if x == ST2 as u32 => 2,
        x if x == ST3 as u32 => 3, x if x == ST4 as u32 => 4, x if x == ST5 as u32 => 5,
        x if x == ST6 as u32 => 6, x if x == ST7 as u32 => 7, _ => 0,
    }
}

fn shift_id(op: &str) -> u32 {
    match op { "shl" => 0, "shr" => 1, "sar" => 2, "rol" => 3, "ror" => 4, _ => 0 }
}

/// If `insn` is `call/jmp [rip+disp]`, return the absolute VA of the slot.
fn mem_target_va(insn: &Instruction, _image_base: u64) -> Option<u64> {
    if insn.memory_base() != Register::RIP { return None; }
    Some(insn.memory_displacement64())
}

/// Look up which import index a memory target VA refers to, via the IAT map.
fn iat_slot(iat: &std::collections::HashMap<u32, u32>, va: u64, image_base: u64) -> Option<u32> {
    // Returns the IAT slot RVA (the key the runtime's import resolver uses).
    if va < image_base { return None; }
    let rva = (va - image_base) as u32;
    iat.get(&rva).copied()
}

fn emit_sse(insn: &Instruction, image_base: u64) -> Option<String> {
    use iced_x86::Mnemonic::*;
    const SCRATCH: u32 = 16; // reserved scratch XMM slot in GowCpu
    let m = insn.mnemonic();
    let dreg = xmm_index(insn.op0_register());
    let sreg = xmm_index(insn.op1_register());
    let mut out = String::new();

    // mem_operand: if op1 is memory, load it into SCRATCH and return its index.
    let mut src_mem = false;
    let mut load_src = |out: &mut String| -> Option<u32> {
        if insn.op1_kind() == OpKind::Memory {
            let _ = writeln!(out, "  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base));
            src_mem = true;
            Some(SCRATCH)
        } else {
            sreg
        }
    };

    match m {
        Movaps | Movups | Movdqa | Movdqu | Movapd => {
            if insn.op0_kind() == OpKind::Memory {
                if let Some(s) = sreg { let _ = writeln!(out, "  gow_store_xmm(cpu, {s}, {});", mem_expr(insn, image_base)); return Some(out); }
            } else if insn.op1_kind() == OpKind::Memory {
                if let Some(d) = dreg { let _ = writeln!(out, "  gow_load_xmm(cpu, {d}, {});", mem_expr(insn, image_base)); return Some(out); }
            } else if let (Some(d), Some(s)) = (dreg, sreg) {
                let _ = writeln!(out, "  gow_xmm_movaps(cpu, {d}, {s});");
                return Some(out);
            }
            None
        }
        Movss | Movsd => {
            if insn.op0_kind() == OpKind::Memory {
                if let Some(s) = sreg { let _ = writeln!(out, "  gow_store_xmm(cpu, {s}, {});", mem_expr(insn, image_base)); return Some(out); }
            } else if insn.op1_kind() == OpKind::Memory {
                if let Some(d) = dreg { let _ = writeln!(out, "  gow_load_xmm(cpu, {d}, {});", mem_expr(insn, image_base)); return Some(out); }
            } else if let (Some(d), Some(s)) = (dreg, sreg) {
                let _ = writeln!(out, "  gow_xmm_movaps(cpu, {d}, {s});");
                return Some(out);
            }
            None
        }
        Pxor | Xorps | Xorpd => {
            let s = load_src(&mut out);
            if let (Some(d), Some(s)) = (dreg, s) { let _ = writeln!(out, "  gow_xmm_pxor(cpu, {d}, {s});"); return Some(out); }
            None
        }
        Pand | Andps | Andpd => {
            let s = load_src(&mut out);
            if let (Some(d), Some(s)) = (dreg, s) { let _ = writeln!(out, "  gow_xmm_pand(cpu, {d}, {s});"); return Some(out); }
            None
        }
        Pandn => {
            let s = load_src(&mut out);
            if let (Some(d), Some(s)) = (dreg, s) { let _ = writeln!(out, "  gow_xmm_pandn(cpu, {d}, {s});"); return Some(out); }
            None
        }
        Por | Orps | Orpd => {
            let s = load_src(&mut out);
            if let (Some(d), Some(s)) = (dreg, s) { let _ = writeln!(out, "  gow_xmm_por(cpu, {d}, {s});"); return Some(out); }
            None
        }
        Paddb => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_padd(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Paddw => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_padd(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Paddd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_padd(cpu, {d}, {s}, 4);"); return Some(out);} None }
        Paddq => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_padd(cpu, {d}, {s}, 8);"); return Some(out);} None }
        Psubb => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_psub(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Psubw => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_psub(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Psubd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_psub(cpu, {d}, {s}, 4);"); return Some(out);} None }
        Psubq => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_psub(cpu, {d}, {s}, 8);"); return Some(out);} None }
        Pcmpeqb => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pcmpeq(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Pcmpeqw => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pcmpeq(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Pcmpeqd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pcmpeq(cpu, {d}, {s}, 4);"); return Some(out);} None }
        Pcmpgtd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pcmpgt(cpu, {d}, {s}, 4);"); return Some(out);} None }
        Pcmpgtw => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pcmpgt(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Pmaxub => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pmax(cpu, {d}, {s}, 1, 0);"); return Some(out);} None }
        Pmaxsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pmax(cpu, {d}, {s}, 4, 1);"); return Some(out);} None }
        Addps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ps(cpu, {d}, {s}, 0);"); return Some(out);} None }
        Subps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ps(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Mulps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ps(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Divps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ps(cpu, {d}, {s}, 3);"); return Some(out);} None }
        Addss => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ss(cpu, {d}, {s}, 0);"); return Some(out);} None }
        Subss => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ss(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Mulss => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ss(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Divss => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_ss(cpu, {d}, {s}, 3);"); return Some(out);} None }
        Addpd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_pd(cpu, {d}, {s}, 0);"); return Some(out);} None }
        Subpd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_pd(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Mulpd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_pd(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Divpd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_pd(cpu, {d}, {s}, 3);"); return Some(out);} None }
        Addsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_sd(cpu, {d}, {s}, 0);"); return Some(out);} None }
        Subsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_sd(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Mulsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_sd(cpu, {d}, {s}, 2);"); return Some(out);} None }
        Divsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_arith_sd(cpu, {d}, {s}, 3);"); return Some(out);} None }
        Sqrtps => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_sqrt(cpu, {d}, 0, 0);"); return Some(out);} None }
        Sqrtss => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_sqrt(cpu, {d}, 0, 1);"); return Some(out);} None }
        Sqrtpd => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_sqrt(cpu, {d}, 1, 0);"); return Some(out);} None }
        Sqrtsd => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_sqrt(cpu, {d}, 1, 1);"); return Some(out);} None }
        Rsqrtps => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_rsqrt(cpu, {d});"); return Some(out);} None }
        Rcpps => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_rcp(cpu, {d});"); return Some(out);} None }
        Minps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 0, 0);"); return Some(out);} None }
        Maxps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 1, 0);"); return Some(out);} None }
        Minss => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 0, 1);"); return Some(out);} None }
        Maxss => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 1, 1);"); return Some(out);} None }
        Minsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 0, 2);"); return Some(out);} None }
        Maxsd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 1, 2);"); return Some(out);} None }
        Unpcklps | Unpcklpd => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_unpck(cpu, {d}, {s}, 4, 0);"); return Some(out);} None }
        Unpckhps | Unpckhpd => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_unpck(cpu, {d}, {s}, 4, 1);"); return Some(out);} None }
        Minpd => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 0, 3);"); return Some(out);} None }
        Maxpd => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_minmax(cpu, {d}, {s}, 1, 3);"); return Some(out);} None }
        Movhlps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_movhl(cpu, {d}, {s});"); return Some(out);} None }
        Movlhps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_movlh(cpu, {d}, {s});"); return Some(out);} None }
        Shufps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shufps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Shufpd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shufpd(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Pshufd => { if let (Some(d),Some(s))=(dreg,sreg.or(Some(SCRATCH))){ if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); } let _=writeln!(out,"  gow_xmm_pshufd(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Pslldq => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {}, 2, 1);", insn.immediate8()); return Some(out);} None }
        Psrldq => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {}, 0, 1);", insn.immediate8()); return Some(out);} None }
        Pmovmskb => { let s = xmm_index(insn.op1_register())?; let (base,_,_) = classify(insn.op0_register())?; let _=writeln!(out,"  GOW_REG(cpu, {base}) = gow_xmm_pmovmskb_val(cpu, {s});"); return Some(out); }
        Packsswb => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 1, 1);"); return Some(out);} None }
        Packuswb => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 1, 0);"); return Some(out);} None }
        Packssdw => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 2, 1);"); return Some(out);} None }
        Packusdw => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 2, 0);"); return Some(out);} None }
        Pshufb => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pshufb(cpu, {d}, {s});"); return Some(out);} None }
        Pabsd => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_pabs(cpu, {d}, 4);"); return Some(out);} None }
        Pabsw => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_pabs(cpu, {d}, 2);"); return Some(out);} None }
        Pabsb => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_pabs(cpu, {d}, 1);"); return Some(out);} None }
        Pmulld => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pmulld(cpu, {d}, {s});"); return Some(out);} None }
        Pminsw => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pminmax(cpu, {d}, {s}, 2, 0, 1);"); return Some(out);} None }
        Pmaxsw => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pminmax(cpu, {d}, {s}, 2, 1, 1);"); return Some(out);} None }
        Pminub => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pminmax(cpu, {d}, {s}, 1, 0, 0);"); return Some(out);} None }
        Pminsd => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_pminmax(cpu, {d}, {s}, 4, 0, 1);"); return Some(out);} None }
        Pshuflw => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_pshuf_hw(cpu, {d}, {}, 0);", insn.immediate8()); return Some(out);} None }
        Pshufhw => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_pshuf_hw(cpu, {d}, {}, 1);", insn.immediate8()); return Some(out);} None }
        Pextrw => {
            let s = xmm_index(insn.op1_register())?;
            let (base,_,_) = classify(insn.op0_register())?;
            let _=writeln!(out,"  GOW_REG(cpu, {base}) = gow_xmm_pextrw(cpu, {s}, {});", insn.immediate8()); return Some(out);
        }
        Pinsrw => { let val = if insn.op1_kind()==OpKind::Memory { format!("gow_load(cpu, {}, 2)", mem_expr(insn, image_base)) } else if let Some((b,_,_))=classify(insn.op1_register()) { format!("(GOW_REG(cpu, {b}) & 0xffff)") } else { "0".into() }; if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_pinsrw(cpu, {d}, {val}, {});", insn.immediate8()); return Some(out);} None }
        Pmovsxbd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pmovsx(cpu, {d}, {s}, 1, 1);"); return Some(out);} None }
        Pmovzxbd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pmovsx(cpu, {d}, {s}, 1, 0);"); return Some(out);} None }
        Pmovsxwd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pmovsx(cpu, {d}, {s}, 2, 1);"); return Some(out);} None }
        Pmovzxwd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pmovsx(cpu, {d}, {s}, 2, 0);"); return Some(out);} None }
        Movmskps | Movmskpd => {
            let s = xmm_index(insn.op1_register())?;
            let (base,_,_) = classify(insn.op0_register())?;
            let _=writeln!(out,"  GOW_REG(cpu, {base}) = gow_xmm_movmsk_val(cpu, {s});"); return Some(out);
        }
        // --- AVX/MMX aliases (2-operand forms) ---
        Vmovaps | Vmovups | Vmovapd | Vmovdqa | Vmovdqu => {
            if insn.op0_kind() == OpKind::Memory { if let Some(s)=sreg { let _=writeln!(out,"  gow_store_xmm(cpu, {s}, {});", mem_expr(insn, image_base)); return Some(out);} }
            else if insn.op1_kind() == OpKind::Memory { if let Some(d)=dreg { let _=writeln!(out,"  gow_load_xmm(cpu, {d}, {});", mem_expr(insn, image_base)); return Some(out);} }
            else if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_movaps(cpu, {d}, {s});"); return Some(out);}
            None
        }
        Vmovss => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_arith_ss(cpu, {d}, {s}, 4);"); return Some(out);} None }
        Vmovsd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_arith_sd(cpu, {d}, {s}, 4);"); return Some(out);} None }
        Comiss | Ucomiss => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_comis(cpu, {d}, {s}, 0);"); return Some(out);} None }
        Comisd | Ucomisd => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_comis(cpu, {d}, {s}, 1);"); return Some(out);} None }
        Andnps => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_andn(cpu, {d}, {s});"); return Some(out);} None }
        Cmpps | Cmppd | Cmpss | Cmpsd => {
            let s=load_src(&mut out);
            let pred = insn.immediate8() as u32 & 7;
            let kind = match m { Cmpps=>0u32, Cmpss=>1, Cmppd=>2, _=>3 };
            if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_cmpps(cpu, {d}, {s}, {pred}, {kind});"); return Some(out);} None }
        Cvtss2sd | Cvtsd2ss => {
            let s = if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); SCRATCH } else { match sreg { Some(x)=>x, None=>return None } };
            if let Some(d)=dreg { let op = if m==Cvtss2sd {4} else {5}; let _=writeln!(out,"  gow_xmm_cvt_xx(cpu, {d}, {s}, {op});"); return Some(out); }
            None
        }
        Cvtdq2ps | Cvtdq2pd | Cvtpd2ps | Cvtps2pd => {
            let s = if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); SCRATCH } else { match sreg { Some(x)=>x, None=>return None } };
            let op = match m { Cvtdq2ps=>0, Cvtdq2pd=>1, Cvtpd2ps=>2, _=>3 };
            if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_cvt_xx(cpu, {d}, {s}, {op});"); return Some(out); }
            None
        }
        Cvtps2dq | Cvttps2dq | Cvtpd2dq | Cvttpd2dq => {
            let s = if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); SCRATCH } else { match sreg { Some(x)=>x, None=>return None } };
            let op = match m { Cvtps2dq=>6, Cvttps2dq=>7, Cvtpd2dq=>8, _=>9 };
            if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_cvt_xx(cpu, {d}, {s}, {op});"); return Some(out); }
            None
        }
        Cvtsi2ss | Cvtsi2sd => {
            let d = match dreg { Some(x)=>x, None=>return None };
            let (src, mem) = if insn.op1_kind()==OpKind::Memory { (format!("gow_load(cpu, {}, 8)", mem_expr(insn, image_base)), true) } else { (reg_read(insn.op1_register()), false) };
            let _ = mem;
            let op = if m==Cvtsi2ss {0} else {1};
            let _=writeln!(out,"  gow_xmm_cvt_g2x(cpu, {d}, {op}, {src});"); return Some(out);
        }
        Cvtss2si | Cvtsd2si | Cvttss2si | Cvttsd2si => {
            let (base,_,_) = classify(insn.op0_register())?;
            let s = if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); SCRATCH } else { sreg? };
            let op = match m { Cvtss2si=>0, Cvtsd2si=>1, Cvttss2si=>2, _=>3 };
            let _=writeln!(out,"  GOW_REG(cpu, {base}) = gow_xmm_cvt_x2g(cpu, {s}, {op});"); return Some(out);
        }
        Movd | Movq => {
            let movq = if m==Movq {1} else {0};
            if insn.op0_kind()==OpKind::Memory {
                // XMM -> mem
                if let Some(s)=sreg { let _=writeln!(out,"  gow_store(cpu, {}, gow_xmm_lane0(cpu, {s}), {});", mem_expr(insn, image_base), if movq==1 {8} else {4}); return Some(out);} 
                return None;
            }
            if insn.op1_kind()==OpKind::Memory {
                if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_from_gpr(cpu, {d}, gow_load(cpu, {}, {}), {movq});", mem_expr(insn, image_base), if movq==1 {8} else {4}); return Some(out);} 
                return None;
            }
            if let (Some(d),Some(s))=(dreg,sreg) { let _=writeln!(out,"  gow_xmm_movd_reg(cpu, {d}, {s}, {movq});"); return Some(out); }
            if let Some(d)=dreg { // XMM <- GPR
                if let Some((base,_,_)) = classify(insn.op1_register()) {
                    let lp = if movq==1 {"(uint64_t)"} else {"(uint64_t)(uint32_t)"};
                    let _=writeln!(out,"  gow_xmm_from_gpr(cpu, {d}, {lp}GOW_REG(cpu, {base}), {movq});"); return Some(out);
                }
            }
            if let Some(s)=sreg { // GPR <- XMM
                if let Some((base,_,_)) = classify(insn.op0_register()) {
                    let _=writeln!(out,"  GOW_REG(cpu, {base}) = gow_xmm_lane0(cpu, {s}){};", if movq==0 {" & 0xffffffffull"} else {""}); return Some(out);
                }
            }
            None
        }
        Pslld => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 2, 4);"); return Some(out);} None }
        Psllw => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 2, 2);"); return Some(out);} None }
        Psllq => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 2, 8);"); return Some(out);} None }
        Psrld => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 0, 4);"); return Some(out);} None }
        Psrlw => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 0, 2);"); return Some(out);} None }
        Psrlq => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 0, 8);"); return Some(out);} None }
        Psrad => { let s = shift_count_expr(insn, sreg, image_base); if let (Some(d),Some(c))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {c}, 1, 4);"); return Some(out);} None }
        Unpcklps | Unpcklpd | Vunpcklps | Vunpcklpd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_unpck(cpu, {d}, {s}, 4, 0);"); return Some(out);} None }
        Unpckhps | Unpckhpd | Vunpckhps | Vunpckhpd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_unpck(cpu, {d}, {s}, 4, 1);"); return Some(out);} None }
        Punpckldq => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_punpck32(cpu, {d}, {s}, 4, 0);"); return Some(out);} None }
        Punpckhdq => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_punpck32(cpu, {d}, {s}, 4, 1);"); return Some(out);} None }
        Punpcklwd | Punpckhwd => { let hi = if m==Punpckhwd {1} else {0}; if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_punpck_wd(cpu, {d}, {s}, {hi});"); return Some(out);} None }
        Punpcklqdq | Punpckhqdq => { let hi = if m==Punpckhqdq {1} else {0}; if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_punpck_qdq(cpu, {d}, {s}, {hi});"); return Some(out);} None }
        Movshdup => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_movshdup(cpu, {d});"); return Some(out);} None }
        Movhpd => {
            if insn.op0_kind()==OpKind::Memory { if let Some(s)=sreg { let _=writeln!(out,"  gow_store(cpu, {}, gow_xmm_lane0(cpu,{s}), 8);", mem_expr(insn, image_base)); return Some(out);} }
            else if insn.op1_kind()==OpKind::Memory { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_set_lane1(cpu, {d}, gow_load(cpu, {}, 8));", mem_expr(insn, image_base)); return Some(out);} }
            else if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_movhpd(cpu, {d}, {s});"); return Some(out);}
            None
        }
        Movsldup => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_movsldup(cpu, {d});"); return Some(out);} None }
        Haddps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_hadd(cpu, {d}, {s});"); return Some(out);} None }
        Blendps | Vblendps => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_blendps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Vpermilps => {
            // permilps with imm8 control applies to one source; with a variable
            // control it uses two. Handle the immediate form (common).
            if insn.op2_kind() == OpKind::Immediate8 {
                if let Some(d)=dreg { let s = if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); SCRATCH } else { sreg? };
                    let _=writeln!(out,"  gow_xmm_permilps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out); }
            }
            None
        }
        Roundps => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_roundps(cpu, {d}, {});", insn.immediate8()); return Some(out);} None }
        Insertps => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_insertps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Addsubps => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_addsubps(cpu, {d}, {s});"); return Some(out);} None }
        Palignr => {
            // 64-bit MMX form is not used here; only lower the 128-bit XMM form.
            if insn.op0_register().size() != 16 { return None; }
            let s = load_src(&mut out);
            if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_palignr(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);}
            None
        }
        _ => None,
    }
}

/// Shift count expression: immediate, or the low qword of the source XMM.
fn shift_count_expr(insn: &Instruction, sreg: Option<u32>, image_base: u64) -> Option<String> {
    if insn.op1_kind() == OpKind::Immediate8 {
        Some(format!("{}", insn.immediate8()))
    } else if insn.op1_kind() == OpKind::Memory {
        Some(format!("(uint32_t)gow_load(cpu, {}, 8)", mem_expr(insn, image_base)))
    } else if let Some(s) = sreg {
        Some(format!("(uint32_t)gow_xmm_lane0(cpu, {s})"))
    } else {
        None
    }
}


// condition-code classification helpers
fn cc_in(a: u32, list: &[u32]) -> bool { list.contains(&a) }
fn is_jcc(m: iced_x86::Mnemonic) -> bool {
    use iced_x86::Mnemonic as M;
    cc_in(m as u32, &[M::Jo as u32, M::Jno as u32, M::Jb as u32, M::Jae as u32, M::Je as u32, M::Jne as u32, M::Jbe as u32, M::Ja as u32, M::Js as u32, M::Jns as u32, M::Jp as u32, M::Jnp as u32, M::Jl as u32, M::Jge as u32, M::Jle as u32, M::Jg as u32])
}
fn is_cmov(m: iced_x86::Mnemonic) -> bool {
    use iced_x86::Mnemonic as M;
    cc_in(m as u32, &[M::Cmovo as u32, M::Cmovno as u32, M::Cmovb as u32, M::Cmovae as u32, M::Cmove as u32, M::Cmovne as u32, M::Cmovbe as u32, M::Cmova as u32, M::Cmovs as u32, M::Cmovns as u32, M::Cmovp as u32, M::Cmovnp as u32, M::Cmovl as u32, M::Cmovge as u32, M::Cmovle as u32, M::Cmovg as u32])
}
fn is_setcc(m: iced_x86::Mnemonic) -> bool {
    use iced_x86::Mnemonic as M;
    cc_in(m as u32, &[M::Seto as u32, M::Setno as u32, M::Setb as u32, M::Setae as u32, M::Sete as u32, M::Setne as u32, M::Setbe as u32, M::Seta as u32, M::Sets as u32, M::Setns as u32, M::Setp as u32, M::Setnp as u32, M::Setl as u32, M::Setge as u32, M::Setle as u32, M::Setg as u32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::ConditionCode;

    #[test]
    fn cc_const_maps_to_gow_test_cc_space() {
        // gow_test_cc expects O=0,NO=1,B=2,AE=3,E=4,NE=5,BE=6,A=7,S=8,NS=9,P=10,
        // NP=11,L=12,GE=13,LE=14,G=15. iced uses O=1..G=16.
        assert_eq!(cc_const(ConditionCode::o), 0);
        assert_eq!(cc_const(ConditionCode::no), 1);
        assert_eq!(cc_const(ConditionCode::b), 2);
        assert_eq!(cc_const(ConditionCode::ae), 3);
        assert_eq!(cc_const(ConditionCode::e), 4);   // JE
        assert_eq!(cc_const(ConditionCode::ne), 5);  // JNE
        assert_eq!(cc_const(ConditionCode::l), 12);  // JL
        assert_eq!(cc_const(ConditionCode::ge), 13); // JGE
        assert_eq!(cc_const(ConditionCode::g), 15);  // JG
    }
}
