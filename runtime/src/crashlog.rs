//! A minimal SIGSEGV/SIGBUS handler that logs the faulting instruction pointer
//! and the guest RVA, so a crash in recompiled code points at the exact
//! instruction instead of requiring backtrace archaeology. It re-raises the
//! signal afterwards so the normal crash reporting still happens.

use crate::dispatch::GowCpu;

static mut CURRENT_CPU: *mut GowCpu = std::ptr::null_mut();

pub fn set_current_cpu(cpu: *mut GowCpu) {
    unsafe { CURRENT_CPU = cpu; }
}

#[repr(C)]
struct SigAction {
    sa_sigaction: usize,
    sa_mask: [u64; 16],
    sa_flags: i32,
    sa_restorer: usize,
}

extern "C" {
    fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
    fn sigemptyset(set: *mut u64) -> i32;
}

const SA_SIGINFO: i32 = 4;
const SIGSEGV: i32 = 11;
const SIGBUS: i32 = 7;
const SIGABRT: i32 = 6;

extern "C" fn handler(sig: i32, _info: *mut std::ffi::c_void, ucontext: *mut std::ffi::c_void) {
    unsafe {
        let cpu = CURRENT_CPU;
        // On aarch64 Linux/Android, ucontext->uc_mcontext.pc is at a known offset.
        // mcontext starts at offset 0x28 in ucontext_t; pc is the first field of
        // the aarch64 sigcontext (fault_address was at +0x0 of the private area,
        // but uc_mcontext.pc is the value we want).
        let pc = read_pc(ucontext);
        let rva = if !cpu.is_null() {
            // CPU rip holds the current guest function VA.
            ((*cpu).rip).wrapping_sub((*cpu).image_base)
        } else {
            0
        };
        let addr = if !cpu.is_null() {
            format!("guest_rip_rva=0x{rva:x} rsp=0x{:x} rbp=0x{:x} rdi=0x{:x} rsi=0x{:x} rbx=0x{:x}",
                (*cpu).gpr.rsp, (*cpu).gpr.rbp, (*cpu).gpr.rdi, (*cpu).gpr.rsi, (*cpu).gpr.rbx)
        } else {
            String::new()
        };
        crate::glog!("[gow.SIGSEGV] signal={sig} pc=0x{pc:x} {addr}");
        // Re-raise with default handler so the tombstone is still produced.
        let mut act = SigAction { sa_sigaction: 0, sa_mask: [0; 16], sa_flags: 0, sa_restorer: 0 };
        sigaction(sig, &act, std::ptr::null_mut());
    }
}

unsafe fn read_pc(ucontext: *mut std::ffi::c_void) -> u64 {
    // ucontext_t: uc_flags(0), uc_link(8), uc_stack(16..40), uc_mcontext at 0x28.
    // aarch64 mcontext: fault_address(0x0), regs[31](0x8), sp(0x100),
    // pc(0x108), pstate(0x110).
    let base = ucontext as *const u8;
    let mcontext = base.add(0x28) as *const u8;
    *((mcontext.add(0x108)) as *const u64)
}

/// Install the handler. Safe to call once at startup.
pub fn install() {
    unsafe {
        let act = SigAction {
            sa_sigaction: handler as usize,
            sa_mask: [0; 16],
            sa_flags: SA_SIGINFO,
            sa_restorer: 0,
        };
        for sig in [SIGSEGV, SIGBUS, SIGABRT] {
            sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}
