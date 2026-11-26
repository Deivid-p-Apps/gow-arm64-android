//! Cooperative fibers backed by host threads (D040).
//!
//! The engine's job scheduler (Sony `JobManager` on top of the Win32 fiber API)
//! treats `SwitchToFiber` as a *real* coroutine switch: a worker running a job
//! calls `SwitchToFiber(next)` from the *middle* of its execution and expects to
//! be resumed later at exactly that point, with its guest registers and stack
//! intact. The old emulation ran the target fiber as a *nested* `gow_run` call,
//! which only works when a fiber runs to completion — a yield/resume job loop
//! (which is what the scheduler does) deadlocks: the switcher never returns, so
//! the resumer's save/restore is a no-op and the current-fiber slot reads 0.
//!
//! Each fiber here gets its **own host thread** and its own `GowCpu` (guest
//! registers + stack + TEB), and `SwitchToFiber` performs a *handoff*: the
//! calling host thread grants the target's turn and blocks on its own until some
//! fiber grants it back. A fiber therefore stays pinned to one host thread for
//! its whole life, and a switch suspends exactly one thread and resumes exactly
//! one — the same observable behaviour as a stackful coroutine, without needing
//! `swapcontext` (absent from the Termux/bionic libc).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::dispatch::{GowCpu, GowFunc};

/// Fiber handles live in their own range so they are never confused with hostcall
/// thunks (`0x0a00_...`) or host code, and are non-null so the engine can store
/// them in its descriptors.
const FIBER_BASE: u64 = 0x0000_0f00_0000_0000;
const FIBER_STRIDE: u64 = 0x100;

/// One fiber's scheduling cell. `turn` is the baton: only the fiber whose turn
/// is true may run. `return_to` remembers who switched us in, so a fiber whose
/// start routine returns hands control back instead of hanging its thread.
struct Ctl {
    turn: Mutex<bool>,
    cv: Condvar,
    return_to: AtomicU64,
}

impl Ctl {
    fn new() -> Self {
        Self { turn: Mutex::new(false), cv: Condvar::new(), return_to: AtomicU64::new(0) }
    }

    /// Block until this fiber is granted the baton, then consume it.
    fn wait_turn(&self) {
        let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        while !*t {
            t = self.cv.wait(t).unwrap_or_else(|e| e.into_inner());
        }
        *t = false;
    }

    /// Hand the baton to this fiber and wake it.
    fn grant(&self) {
        let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        *t = true;
        self.cv.notify_all();
    }
}

thread_local! {
    /// The fiber currently executing on this host thread: (handle, ctl).
    static CURRENT: RefCell<Option<(u64, Arc<Ctl>)>> = const { RefCell::new(None) };
}

fn registry() -> &'static Mutex<HashMap<u64, Arc<Ctl>>> {
    static R: OnceLock<Mutex<HashMap<u64, Arc<Ctl>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_handle() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    FIBER_BASE + N.fetch_add(1, Ordering::Relaxed) * FIBER_STRIDE
}

fn set_current(handle: u64, ctl: Arc<Ctl>) {
    CURRENT.with(|c| *c.borrow_mut() = Some((handle, ctl)));
}

/// The calling thread's fiber, creating one on first use (the moral equivalent
/// of `ConvertThreadToFiber`).
fn current_or_convert() -> (u64, Arc<Ctl>) {
    CURRENT.with(|c| {
        if let Some(v) = c.borrow().clone() {
            return v;
        }
        let handle = next_handle();
        let ctl = Arc::new(Ctl::new());
        registry().lock().unwrap().insert(handle, ctl.clone());
        *c.borrow_mut() = Some((handle, ctl.clone()));
        (handle, ctl)
    })
}

/// CreateFiber(dwStackSize, lpStartAddress, lpParameter): start a host thread
/// that runs `start_rva` with `param` in RCX, parked until first switched to.
pub unsafe fn create(cpu: *mut GowCpu, start_rva: u64, param: u64) -> u64 {
    let c = &*cpu;
    let handle = next_handle();
    let ctl = Arc::new(Ctl::new());
    registry().lock().unwrap().insert(handle, ctl.clone());
    let image_base = c.image_base;
    let image_size = c.image_size as usize;
    let map: Vec<(u32, GowFunc)> = c.func_map.iter().map(|(k, v)| (*k, *v)).collect();
    let rva = start_rva as u32;
    std::thread::spawn(move || {
        let cpu = GowCpu::new(image_base, image_size);
        let cp = Box::into_raw(cpu);
        for (k, f) in map { (*cp).func_map.insert(k, f); }
        (*cp).gpr.rcx = param;
        set_current(handle, ctl.clone());
        ctl.wait_turn(); // parked until the scheduler first switches to us
        crate::glog!("[gow.fiber] run start_rva=0x{rva:x} param=0x{param:x} handle=0x{handle:x}");
        crate::dispatch::gow_run(cp, rva);
        crate::glog!("[gow.fiber] returned start_rva=0x{rva:x} handle=0x{handle:x}");
        // The start routine returned: hand the baton back to whoever ran us.
        let back = ctl.return_to.load(Ordering::Relaxed);
        if back != 0 {
            if let Some(tc) = registry().lock().unwrap().get(&back).cloned() {
                tc.grant();
            }
        }
        // Park this thread forever; the fiber is done.
        let park = Ctl::new();
        park.wait_turn();
    });
    crate::glog!("[gow.fiber] CreateFiber start_rva=0x{start_rva:x} param=0x{param:x} -> handle=0x{handle:x}");
    handle
}

/// ConvertThreadToFiber: give the calling thread a fiber handle so other fibers
/// can switch back to it.
pub fn convert_current() -> u64 {
    current_or_convert().0
}

/// Is `handle` one of our fiber handles?
pub fn is_fiber(handle: u64) -> bool {
    handle >= FIBER_BASE && registry().lock().unwrap().contains_key(&handle)
}

/// The calling fiber's handle (GetCurrentFiber), creating one on first use.
pub fn current_handle() -> u64 {
    current_or_convert().0
}

/// SwitchToFiber(lpFiber): suspend the calling fiber and resume `target`.
pub fn switch(target: u64) {
    let (my_handle, my_ctl) = current_or_convert();
    if target == my_handle {
        return; // switching to self is a no-op
    }
    let Some(tc) = registry().lock().unwrap().get(&target).cloned() else {
        crate::glog!("[gow.fiber] SwitchToFiber(0x{target:x}) unknown handle (current=0x{my_handle:x})");
        return;
    };
    // Remember us so a target that returns can hand back.
    tc.return_to.store(my_handle, Ordering::Relaxed);
    // Hand off: let the target run, then block until someone grants us again.
    tc.grant();
    my_ctl.wait_turn();
}
