//! Guards the C-visible layout of GowCpu against the Rust definition. The
//! generated C (gow_game.c) writes `cpu->rip` per instruction and reads
//! `cpu->next_rva`; if these offsets drift, diagnostics point at the wrong
//! instruction. This test pins the offsets.

use gow_runtime::dispatch::GowCpu;
use std::mem::offset_of;

#[test]
fn cpu_prefix_offsets_match_c_header() {
    // gow_cpu.h lays out: gpr(16*8), xmm[17](17*16), rip, flags, mxcsr, next_rva.
    assert_eq!(offset_of!(GowCpu, gpr), 0);
    assert_eq!(offset_of!(GowCpu, xmm), 16 * 8);
    // XMM array is 17 * 16 = 272 bytes.
    assert_eq!(offset_of!(GowCpu, rip), 16 * 8 + 17 * 16);
    assert_eq!(offset_of!(GowCpu, flags), 16 * 8 + 17 * 16 + 8);
    assert_eq!(offset_of!(GowCpu, mxcsr), 16 * 8 + 17 * 16 + 16);
    assert_eq!(offset_of!(GowCpu, next_rva), 16 * 8 + 17 * 16 + 20);
}
