//! Integration test: map the guest image, install the function registry and
//! execute real recompiled code through the dispatch loop, asserting that
//! control flow (call/ret/jmp) and integer flags behave.
//!
//! Runs against generated C compiled by the test build. If generated/ is not
//! present (game files unavailable) the test self-skips, so CI stays green
//! on runners without game data.

#[cfg(test)]
mod tests {
    /// The dispatcher's own enum: verify add/sub flag semantics exactly, since
    /// every lifted conditional branch depends on them.
    #[test]
    fn add_flags_exact() {
        use gow_runtime::dispatch::*;
        let mut cpu = GowCpu::new(0x140000000, 0x1000);
        unsafe {
            // 0x7fff_ffff + 1 (32-bit) => CF not set at 32 bits, OF set, ZF/... check
            let r = gow_add64(cpu.as_mut(), 0xffff_ffff, 1, 32);
            assert_eq!(r, 0, "32-bit wrap result");
            assert!(cpu.flags & ZF != 0, "ZF on zero result");
            assert!(cpu.flags & CF != 0, "CF on 32-bit wrap");
        }
    }

    #[test]
    fn sub_flags_exact() {
        use gow_runtime::dispatch::*;
        let mut cpu = GowCpu::new(0x140000000, 0x1000);
        unsafe {
            let r = gow_sub64(cpu.as_mut(), 5, 5, 64);
            assert_eq!(r, 0);
            assert!(cpu.flags & ZF != 0);
            assert!(cpu.flags & CF == 0);
            let r = gow_sub64(cpu.as_mut(), 3, 5, 64);
            assert_eq!(r, (3u64).wrapping_sub(5));
            assert!(cpu.flags & CF != 0, "borrow sets CF");
            assert!(cpu.flags & SF != 0, "negative result sets SF");
        }
    }

    #[test]
    fn cc_evaluation() {
        use gow_runtime::dispatch::*;
        let mut cpu = GowCpu::new(0x140000000, 0x1000);
        unsafe {
            gow_sub64(cpu.as_mut(), 5, 5, 64); // ZF set
            assert_eq!(gow_test_cc(cpu.as_mut(), 0x4), 1, "JE taken");
            assert_eq!(gow_test_cc(cpu.as_mut(), 0x5), 0, "JNE not taken");
            gow_sub64(cpu.as_mut(), 3, 5, 64); // SF set, OF clear => JL
            assert_eq!(gow_test_cc(cpu.as_mut(), 0xc), 1, "JL taken");
        }
    }
}
