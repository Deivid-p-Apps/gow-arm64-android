//! A minimal SIGSEGV/SIGBUS handler that logs the faulting instruction pointer
//! and the guest RVA, so a crash in recompiled code points at the exact
//! instruction instead of requiring backtrace archaeology. It re-raises the
//! signal afterwards so the normal crash reporting still happens.

use crate::dispatch::GowCpu;

static mut CURRENT_CPU: *mut GowCpu = std::ptr::null_mut();

pub fn set_current_cpu(cpu: *mut GowCpu) {
    unsafe { CURRENT_CPU = cpu; }
}

/// The guest image [base, base+size) of the running CPU, if set. Used by the
/// heap to refuse backing a page that would shadow real game code/data.
pub fn image_range() -> Option<(u64, u64)> {
    unsafe {
        if CURRENT_CPU.is_null() {
            return None;
        }
        let c = &*CURRENT_CPU;
        Some((c.image_base, c.image_base + c.image_size))
    }
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

extern "C" fn handler(sig: i32, info: *mut std::ffi::c_void, ucontext: *mut std::ffi::c_void) {
    unsafe {
        let cpu = CURRENT_CPU;
        let pc = read_pc(ucontext);
        // siginfo_t: si_signo(0), si_errno(4), si_code(8), then for a fault the
        // address is at offset 16 on aarch64 Linux/Android.
        let fault = if !info.is_null() {
            *((info as *const u8).add(16) as *const u64)
        } else { 0 };
        let (rva, msg) = if !cpu.is_null() {
            let rva = ((*cpu).rip).wrapping_sub((*cpu).image_base);
            (rva,
             format!("[gow.SIGSEGV] signal={sig} fault=0x{fault:x} pc=0x{pc:x} guest_rva=0x{rva:x} rsp=0x{:x} rbp=0x{:x} rdi=0x{:x} rsi=0x{:x} rcx=0x{:x} rax=0x{:x} r8=0x{:x}\n",
                (*cpu).gpr.rsp, (*cpu).gpr.rbp, (*cpu).gpr.rdi, (*cpu).gpr.rsi,
                (*cpu).gpr.rcx, (*cpu).gpr.rax, (*cpu).gpr.r8))
        } else {
            (0, format!("[gow.SIGSEGV] signal={sig} fault=0x{fault:x} pc=0x{pc:x}\n"))
        };
        let _ = rva;
        // Write with a raw syscall: the normal logger takes locks and allocates,
        // which is unsafe inside a signal handler.
        raw_write(2, msg.as_bytes());
        // Re-raise with the default handler so the tombstone is still produced.
        let mut act = SigAction { sa_sigaction: 0, sa_mask: [0; 16], sa_flags: 0, sa_restorer: 0 };
        sigaction(sig, &act, std::ptr::null_mut());
    }
}

/// Async-signal-safe write(2) via the raw syscall.
unsafe fn raw_write(fd: i32, buf: &[u8]) {
    extern "C" { fn write(fd: i32, buf: *const u8, n: usize) -> isize; }
    let _ = write(fd, buf.as_ptr(), buf.len());
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
