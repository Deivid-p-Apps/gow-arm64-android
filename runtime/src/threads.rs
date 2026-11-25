//! Windows synchronisation objects (mutex/event) and threads, mapped onto the
//! host. Handles are small integers the guest sees. A spawned thread runs
//! recompiled code from a given RVA with a `param` in RCX, using the same
//! dispatcher as the main thread.

use crate::dispatch::{GowCpu, GowFunc};
use crate::log::log_line;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub struct SyncObjects {
    next: AtomicU64,
    mutexes: Mutex<HashMap<u64, Arc<Mutex<()>>>>,
    events: Mutex<HashMap<u64, Arc<(Mutex<bool>, Condvar)>>>,
}

impl SyncObjects {
    pub fn new() -> Self {
        Self { next: AtomicU64::new(0x4000), mutexes: Mutex::new(HashMap::new()), events: Mutex::new(HashMap::new()) }
    }
    pub fn create_mutex(&self) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.mutexes.lock().unwrap().insert(id, Arc::new(Mutex::new(())));
        id
    }
    pub fn create_event(&self, signalled: bool) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.events.lock().unwrap().insert(id, Arc::new((Mutex::new(signalled), Condvar::new())));
        id
    }
    /// Returns true if the object became signalled (WAIT_OBJECT_0), false on a
    /// zero-timeout wait (WAIT_TIMEOUT).
    pub fn wait(&self, handle: u64, timeout_ms: u64) -> bool {
        if let Some(m) = self.mutexes.lock().unwrap().get(&handle).cloned() {
            let _guard = m.lock().unwrap();
            return true;
        }
        if let Some(e) = self.events.lock().unwrap().get(&handle).cloned() {
            let (lock, cv) = &*e;
            let mut s = lock.lock().unwrap();
            if *s { return true; }
            if timeout_ms == 0 { return false; }
            let (ns, _) = cv.wait_timeout(s, std::time::Duration::from_millis(timeout_ms)).unwrap();
            s = ns;
            return *s;
        }
        false
    }
    pub fn set_event(&self, handle: u64, signalled: bool) {
        if let Some(e) = self.events.lock().unwrap().get(&handle).cloned() {
            let (lock, cv) = &*e;
            *lock.lock().unwrap() = signalled;
            if signalled { cv.notify_all(); }
        }
    }
    pub fn release_mutex(&self, _handle: u64) {}
}

impl Default for SyncObjects {
    fn default() -> Self { Self::new() }
}

/// Thread table: maps handle -> joinable host thread.
pub struct ThreadTable {
    next: AtomicU64,
    /// handle -> stable guest thread id (GetThreadId, D017).
    tids: Mutex<HashMap<u64, u64>>,
}

impl ThreadTable {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(0x8000),
            tids: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a guest thread running recompiled code at `start_rva` with `param`
    /// in RCX. The guest function table must already be installed.
    pub unsafe fn spawn(&self, parent: *mut GowCpu, start_rva: u64, param: u64) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        // Assign the thread's guest id up front so GetThreadId(handle) and the
        // thread's own GetCurrentThreadId() agree (D017).
        let tid = crate::win32::alloc_tid();
        self.tids.lock().unwrap().insert(id, tid);
        let image_base = (*parent).image_base;
        let image_size = (*parent).image_size as usize;
        let map: Vec<(u32, GowFunc)> = (*parent).func_map.iter().map(|(k, v)| (*k, *v)).collect();
        std::thread::spawn(move || {
            crate::win32::set_current_tid(tid);
            let cpu = GowCpu::new(image_base, image_size);
            let c = Box::into_raw(cpu);
            for (rva, f) in map { (*c).func_map.insert(rva, f); }
            (*c).gpr.rcx = param;
            log_line(&format!("[gow.thread] start rva=0x{start_rva:x} param=0x{param:x} tid={tid}"));
            crate::dispatch::gow_run(c, start_rva as u32);
            drop(Box::from_raw(c));
        });
        id
    }

    /// The stable guest id behind a CreateThread handle (GetThreadId).
    pub fn thread_id(&self, handle: u64) -> u64 {
        self.tids.lock().unwrap().get(&handle).copied().unwrap_or(0)
    }
}

impl Default for ThreadTable {
    fn default() -> Self { Self::new() }
}
