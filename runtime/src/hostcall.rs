//! Host thunks: callable addresses the recompiled guest can `call` that run
//! native Rust instead of guest code.
//!
//! Why this exists: the guest calls COM methods (`IUnknown::QueryInterface`,
//! `IXAudio2::CreateSourceVoice`, `IDXGISwapChain::Present`, ...) through
//! vtables. The guest's calling convention is Microsoft x64 (args in
//! RCX/RDX/R8/R9), but on an arm64 host those "registers" live in the
//! `GowCpu::gpr` file, not in CPU registers. A raw host function pointer cannot
//! receive them. So each virtual method is represented by a *pseudo-address*
//! (in a reserved range); when the generated code calls it, the dispatcher
//! recognises the address, looks up the registered handler and runs it with the
//! `GowCpu` pointer. The handler reads/writes the guest register file itself.

use crate::dispatch::GowCpu;
use std::sync::Mutex;

/// Base of the pseudo-address range used for host thunks. Must not overlap the
/// guest image (0x140000000) or real host memory.
pub const THUNK_BASE: u64 = 0x0000_0a00_0000_0000;
const THUNK_STRIDE: u64 = 0x10;

type Handler = Box<dyn Fn(*mut GowCpu) + Send + Sync>;

static THUNKS: Mutex<Vec<Option<Handler>>> = Mutex::new(Vec::new());

/// Register a handler and return its pseudo-address.
pub fn register<F: Fn(*mut GowCpu) + Send + Sync + 'static>(f: F) -> u64 {
    let mut t = THUNKS.lock().unwrap();
    let idx = t.len() as u64;
    t.push(Some(Box::new(f)));
    THUNK_BASE + idx * THUNK_STRIDE
}

pub fn is_thunk(addr: u64) -> bool {
    addr >= THUNK_BASE && addr < THUNK_BASE + (THUNKS.lock().unwrap().len() as u64 + 1) * THUNK_STRIDE
}

/// Run the thunk at `addr` with the given CPU. Returns false if no handler.
pub unsafe fn call(addr: u64, cpu: *mut GowCpu) -> bool {
    if !is_thunk(addr) {
        return false;
    }
    let idx = ((addr - THUNK_BASE) / THUNK_STRIDE) as usize;
    let t = THUNKS.lock().unwrap();
    if let Some(Some(h)) = t.get(idx) {
        h(cpu);
        true
    } else {
        false
    }
}

/// Build a COM-style vtable: an array of thunk pseudo-addresses laid out at the
/// given indices, with unused slots filled by a shared "not implemented" thunk.
/// Returns the address of the vtable allocation (leaked, lives for the process).
pub fn build_vtable(slots: &[(usize, u64)], len: usize) -> u64 {
    let notimpl = register(|cpu| {
        unsafe {
            crate::glog!("[gow.com] unimplemented COM method called (returning E_NOTIMPL)");
            (*cpu).gpr.rax = 0x8000_4001; // E_NOTIMPL
        }
    });
    let mut vt = vec![notimpl; len];
    for (idx, addr) in slots {
        if *idx < len {
            vt[*idx] = *addr;
        }
    }
    let boxed = vt.into_boxed_slice();
    let ptr = boxed.as_ptr() as u64;
    std::mem::forget(boxed);
    ptr
}

