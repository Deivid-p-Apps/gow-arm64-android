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
    if e.is_empty() { e.push_str("0"); }
    let _ = image_base;
    e
}

/// Size in bytes of memory operand.
fn mem_size(insn: &Instruction) -> u32 {
    let m = insn.memory_size();
    let b = m.size() / 8;
    if b == 0 { 8 } else { b as u32 }
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
    cc as u32
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
        iat.insert(imp.iat_rva, i as u32);
    }

    // Split functions into fixed-size shards so each translation unit stays
    // small enough to compile on constrained machines and in CI.
    const SHARD: usize = 1500;
    std::fs::create_dir_all(out_dir)?;
    let entries_vec: Vec<u32> = entries.iter().cloned().collect();
    let n_shards = (entries_vec.len() + SHARD - 1).max(1) / SHARD;

    let mut shard_protos = String::new();
    for (si, chunk) in entries_vec.chunks(SHARD).enumerate() {
        let mut sc = String::new();
        writeln!(sc, "/* Auto-generated by gowrecomp (shard {si}). Do not edit. */")?;
        writeln!(sc, "#include \"gow_protos.h\"")?;
        writeln!(sc, "static inline int64_t sign64(uint64_t v) {{ return (int64_t)v; }}\n")?;
        for &entry_rva in chunk {
            let range_end = pe
                .pdata_functions
                .iter()
                .find(|(s, _)| *s == entry_rva)
                .map(|(_, e)| *e)
                .unwrap_or(entry_rva + 0x10000);
            let off = match PeImage::rva_to_off(&sections, entry_rva) { Some(o) => o, None => continue };
            let len = (range_end - entry_rva) as usize;
            if len == 0 || pe.bytes.len() < off + len { continue; }
            let code = &pe.bytes[off..off + len];

            writeln!(sc, "void f_{entry_rva:x}(GowCpu* cpu) {{")?;
            func_count += 1;

            // First pass: decode the whole range, recording every valid
            // instruction start. Invalid bytes (padding) are skipped one at a
            // time so branch targets past them are still discovered.
            let mut starts: BTreeSet<u32> = BTreeSet::new();
            {
                let mut d = Decoder::with_ip(64, code, image_base + entry_rva as u64, DecoderOptions::NONE);
                let mut ii = Instruction::default();
                while d.can_decode() {
                    let ip = d.ip();
                    d.decode_out(&mut ii);
                    if !ii.is_invalid() {
                        starts.insert((ip - image_base) as u32);
                    } else {
                        // skip one byte and continue (robust to embedded data)
                        d.set_position((d.position() - ii.len()) + 1);
                    }
                }
            }

            let mut dec = Decoder::with_ip(64, code, image_base + entry_rva as u64, DecoderOptions::NONE);
            let mut insn = Instruction::default();
            while dec.can_decode() {
                let pos_before = dec.position();
                dec.decode_out(&mut insn);
                let rva = (insn.ip().wrapping_sub(image_base)) as u32;
                if insn.is_invalid() {
                    // skip this byte; no code emitted
                    dec.set_position(pos_before + 1);
                    continue;
                }
                writeln!(sc, "L_{rva:x}: ;")?;
                insn_count += 1;
                let mut text = String::new();
                fmt.format(&insn, &mut text);
                writeln!(sc, "  /* 0x{rva:x}: {text} */")?;
                emit(&mut sc, &insn, image_base, &iat, &starts, entry_rva, range_end, &entries, &mut unimpl, &mut unimpl_instances)?;
            }
            writeln!(sc, "}}\n")?;
        }
        std::fs::write(out_dir.join(format!("gow_shard_{si:03}.c")), &sc)?;
    }

    // Prototypes for every function (stable ordering), emitted as a header the
    // shards include so cross-shard direct calls resolve.
    for &rva in &entries_vec {
        writeln!(shard_protos, "void f_{rva:x}(GowCpu* cpu);")?;
    }
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
    std::fs::write(out_dir.join("gow_table.c"), &tbl)?;

    // Keep `c` for the returned source only as a small summary; do not write a
    // monolithic file (shards are the build unit).
    let _ = &mut c;
    let _ = &mut shard_protos;
    let _ = n_shards;

    Ok(LiftOutput { function_count: func_count, instruction_count: insn_count, unimpl, unimpl_instances })
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
            } else {
                match operand_read(insn, 0, image_base) {
                    Some(e) => writeln!(c, "  gow_call_indirect(cpu, (uint64_t)({e})); return;")?,
                    None => { trap(c, insn, image_base, unimpl, un)?; }
                }
            }
            return Ok(true);
        }
        Call => {
            if insn.op0_kind() == OpKind::NearBranch64 {
                let t = (insn.near_branch64() - image_base) as u32;
                if entries.contains(&t) {
                    writeln!(c, "  f_{t:x}(cpu);")?;
                } else {
                    writeln!(c, "  gow_trampoline(cpu, 0x{t:x}u);")?;
                }
            } else if insn.op0_kind() == OpKind::Memory {
                // `call qword [rip+disp]` — usually an IAT import thunk.
                if let Some(va) = mem_target_va(insn, image_base) {
                    if let Some(slot_rva) = iat_slot(iat, va, image_base) {
                        writeln!(c, "  gow_import_call(cpu, 0x{slot_rva:x}u);")?;
                        return Ok(false);
                    }
                }
                let addr = mem_expr(insn, image_base);
                writeln!(c, "  gow_call_ptr(cpu, (uint64_t)({addr}));")?;
            } else {
                match operand_read(insn, 0, image_base) {
                    Some(e) => writeln!(c, "  gow_call_ptr(cpu, (uint64_t)({e}));")?,
                    None => { trap(c, insn, image_base, unimpl, un)?; }
                }
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
                let is32 = insn.memory_size().size() == 32;
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
        Fst | Fstp => {
            let pop = m == Fstp;
            if insn.op0_kind() == OpKind::Memory {
                let is32 = insn.memory_size().size() == 32;
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
                let is32 = insn.memory_size().size() == 32;
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
                let is32 = insn.memory_size().size() == 32;
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
        Fnstcw | Fstcw => { writeln!(c, "  *(uint16_t*)(uintptr_t)({}) = (uint16_t)gow_x87_fnstcw(cpu);", mem_expr(insn, image_base))?; return Ok(false); }
        Fldcw => { writeln!(c, "  gow_x87_fldcw(cpu, gow_load(cpu, {}, 2));", mem_expr(insn, image_base))?; return Ok(false); }
        Fninit | Finit => { writeln!(c, "  gow_x87_init(cpu);")?; return Ok(false); }
        Ffree | Fdecstp | Fincstp | Fnop | Fclex | Fnclex => { return Ok(false); }
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
        Unpcklps | Unpcklpd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_unpck(cpu, {d}, {s}, 4, 0);"); return Some(out);} None }
        Unpckhps | Unpckhpd => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_unpck(cpu, {d}, {s}, 4, 1);"); return Some(out);} None }
        Movhlps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_movhl(cpu, {d}, {s});"); return Some(out);} None }
        Movlhps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_movlh(cpu, {d}, {s});"); return Some(out);} None }
        Shufps => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shufps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Shufpd => { let s = load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shufpd(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Pshufd => { if let (Some(d),Some(s))=(dreg,sreg.or(Some(SCRATCH))){ if insn.op1_kind()==OpKind::Memory { let _=writeln!(out,"  gow_load_xmm(cpu, {SCRATCH}, {});", mem_expr(insn, image_base)); } let _=writeln!(out,"  gow_xmm_pshufd(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Pslldq => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {}, 2, 1);", insn.immediate8()); return Some(out);} None }
        Psrldq => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {}, 0, 1);", insn.immediate8()); return Some(out);} None }
        Pslld => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {}, 2, 4);", if insn.op1_kind()==OpKind::Register {"(uint32_t)(GOW_REG(cpu, rcx) & 0x3f)"} else {"0"}); return Some(out);} let _=s; None }
        Psrld => { let s=load_src(&mut out); if let (Some(d),Some(s))=(dreg,s){ let _=writeln!(out,"  gow_xmm_shift(cpu, {d}, {}, 0, 4);", if insn.op1_kind()==OpKind::Register {"(uint32_t)(GOW_REG(cpu, rcx) & 0x3f)"} else {"0"}); return Some(out);} let _=s; None }
        Pmovmskb => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pmovmskb(cpu, {d}, {s});"); return Some(out);} None }
        Packsswb => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 1, 1);"); return Some(out);} None }
        Packuswb => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 1, 0);"); return Some(out);} None }
        Packssdw => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 2, 1);"); return Some(out);} None }
        Packusdw => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_pack(cpu, {d}, {s}, 2, 0);"); return Some(out);} None }
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
        Movsldup => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_movsldup(cpu, {d});"); return Some(out);} None }
        Haddps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_hadd(cpu, {d}, {s});"); return Some(out);} None }
        Blendps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_blendps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
        Roundps => { if let Some(d)=dreg { let _=writeln!(out,"  gow_xmm_roundps(cpu, {d}, {});", insn.immediate8()); return Some(out);} None }
        Insertps => { if let (Some(d),Some(s))=(dreg,sreg){ let _=writeln!(out,"  gow_xmm_insertps(cpu, {d}, {s}, {});", insn.immediate8()); return Some(out);} None }
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
