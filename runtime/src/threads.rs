//! Windows synchronisation objects (mutex/event/semaphore/condition variable)
//! and threads, mapped onto the host. Handles are small integers the guest
//! sees. A spawned thread runs recompiled code from a given RVA with a `param`
//! in RCX, using the same dispatcher as the main thread.
//!
//! D019: semaphores are real counting semaphores (the old CreateSemaphore
//! handed out a permanently-signalled event, so every wait succeeded instantly
//! and a ReleaseSemaphore did nothing — workers busy-spun and lost wakeups
//! were invisible); condition variables are keyed by the guest address of the
//! CONDITION_VARIABLE with a generation counter per wake; thread handles carry
//! a done-flag so WaitForSingleObject/WaitForMultipleObjects can actually wait
//! for a thread to finish.

use crate::dispatch::{GowCpu, GowFunc};
use crate::log::log_line;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub struct SyncObjects {
    next: AtomicU64,
    mutexes: Mutex<HashMap<u64, Arc<Mutex<()>>>>,
    events: Mutex<HashMap<u64, Arc<(Mutex<bool>, Condvar)>>>,
    /// Counting semaphores (D019): handle -> (count, condvar). A wait
    /// decrements and blocks at zero; a release increments and wakes.
    semas: Mutex<HashMap<u64, Arc<(Mutex<i64>, Condvar)>>>,
    /// Condition variables (D019) keyed by the guest address of the
    /// CONDITION_VARIABLE: value is a generation counter. Wake bumps it and
    /// notifies; sleepers wait for it to move (or time out). The guest memory
    /// behind the address is never interpreted, so the engine can initialise
    /// / zero it however it likes.
    cvs: Mutex<HashMap<u64, Arc<(Mutex<u64>, Condvar)>>>,
}

impl SyncObjects {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(0x4000),
            mutexes: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            semas: Mutex::new(HashMap::new()),
            cvs: Mutex::new(HashMap::new()),
        }
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
    /// Create a counting semaphore with `initial` units (D019).
    pub fn create_semaphore(&self, initial: i64) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.semas.lock().unwrap().insert(id, Arc::new((Mutex::new(initial.max(0)), Condvar::new())));
        id
    }
    /// ReleaseSemaphore(h, count, &prev): add `count` units, wake waiters,
    /// report the previous count. Returns false for an unknown handle.
    pub fn release_semaphore(&self, handle: u64, count: i64, prev: &mut i64) -> bool {
        if count <= 0 { return false; }
        let Some(s) = self.semas.lock().unwrap().get(&handle).cloned() else { return false; };
        let (lock, cv) = &*s;
        let mut c = lock.lock().unwrap();
        *prev = *c;
        *c += count;
        cv.notify_all();
        true
    }
    /// Register (or touch) the condition variable at guest address `addr`
    /// (InitializeConditionVariable): the entry exists so a later wake cannot
    /// be lost between a sleeper's check and its wait.
    pub fn cv_touch(&self, addr: u64) {
        let mut m = self.cvs.lock().unwrap();
        m.entry(addr).or_insert_with(|| Arc::new((Mutex::new(0u64), Condvar::new())));
    }
    /// SleepConditionVariableCS/SRW: wait until the generation at `addr`
    /// moves or the timeout expires. Returns true when woken, false on
    /// timeout.
    pub fn cv_wait(&self, addr: u64, timeout_ms: u64) -> bool {
        let cv = {
            let mut m = self.cvs.lock().unwrap();
            m.entry(addr).or_insert_with(|| Arc::new((Mutex::new(0u64), Condvar::new()))).clone()
        };
        let (lock, cvd) = &*cv;
        let mut g = lock.lock().unwrap();
        let mine = *g;
        if timeout_ms == 0 { return false; }
        if timeout_ms == 0xFFFF_FFFF {
            while *g == mine {
                g = cvd.wait(g).unwrap();
            }
            return true;
        }
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(timeout_ms.min(0xFFFF_FFFE));
        while *g == mine {
            let now = std::time::Instant::now();
            if now >= deadline { return false; }
            let (ng, _) = cvd.wait_timeout(g, deadline - now).unwrap();
            g = ng;
        }
        true
    }
    /// WakeConditionVariable / WakeAllConditionVariable: bump the generation
    /// and notify. Both use notify_all: a wake-one that races with a new
    /// sleeper is safer over-waking than under-waking.
    pub fn cv_wake(&self, addr: u64, _all: bool) {
        if let Some(cv) = self.cvs.lock().unwrap().get(&addr).cloned() {
            let (lock, cvd) = &*cv;
            *lock.lock().unwrap() += 1;
            cvd.notify_all();
        }
    }
    /// Returns true if the object became signalled (WAIT_OBJECT_0), false on a
    /// zero-timeout wait (WAIT_TIMEOUT).
    pub fn wait(&self, handle: u64, timeout_ms: u64) -> bool {
        if let Some(s) = self.semas.lock().unwrap().get(&handle).cloned() {
            let (lock, cv) = &*s;
            let mut c = lock.lock().unwrap();
            if *c > 0 { *c -= 1; return true; }
            if timeout_ms == 0 { return false; }
            if timeout_ms == 0xFFFF_FFFF {
                while *c <= 0 {
                    c = cv.wait(c).unwrap();
                }
                *c -= 1;
                return true;
            }
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(timeout_ms.min(0xFFFF_FFFE));
            while *c <= 0 {
                let now = std::time::Instant::now();
                if now >= deadline { return false; }
                let (nc, _) = cv.wait_timeout(c, deadline - now).unwrap();
                c = nc;
            }
            *c -= 1;
            return true;
        }
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
    /// Non-blocking "is this object signalled right now" for
    /// WaitForMultipleObjects (D019). Does not consume anything.
    pub fn poll(&self, handle: u64) -> bool {
        if let Some(s) = self.semas.lock().unwrap().get(&handle).cloned() {
            return *s.0.lock().unwrap() > 0;
        }
        if let Some(e) = self.events.lock().unwrap().get(&handle).cloned() {
            return *e.0.lock().unwrap();
        }
        if self.mutexes.lock().unwrap().contains_key(&handle) {
            return true; // unowned in our model
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
    /// handle -> "thread finished" flag (D019): WaitForSingleObject on a
    /// thread handle waits on this.
    done: Mutex<HashMap<u64, Arc<(Mutex<bool>, Condvar)>>>,
}

impl ThreadTable {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(0x8000),
            tids: Mutex::new(HashMap::new()),
            done: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a guest thread running recompiled code at `start_rva` with `param`
    /// in RCX. The guest function table must already be installed.
    pub unsafe fn spawn(&self, parent: *mut GowCpu, start_rva: u64, param: u64) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        // The engine's thread struct pointer arrives as the _beginthreadex arg.
        // A small value (i*0x80) means its backing array was allocated at 0 — a
        // failed allocation upstream — so record the guest return address of the
        // _beginthreadex call to locate the spawner.
        let caller_ra = {
            let sp = (*parent).gpr.rsp;
            if crate::dispatch::plausible_mem(sp, 8) { *(sp as *const u64) } else { 0 }
        };
        if param < 0x1000 {
            crate::glog!("[gow.thread] SUSPECT param=0x{param:x} (base-0 array?) caller_ra=0x{caller_ra:x} caller_rva=0x{:x}",
                caller_ra.wrapping_sub((*parent).image_base));
        }
        // Assign the thread's guest id up front so GetThreadId(handle) and the
        // thread's own GetCurrentThreadId() agree (D017).
        let tid = crate::win32::alloc_tid();
        self.tids.lock().unwrap().insert(id, tid);
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        self.done.lock().unwrap().insert(id, finished.clone());
        let image_base = (*parent).image_base;
        let image_size = (*parent).image_size as usize;
        let map: Vec<(u32, GowFunc)> = (*parent).func_map.iter().map(|(k, v)| (*k, *v)).collect();
        std::thread::spawn(move || {
            crate::win32::set_current_tid(tid);
            let cpu = GowCpu::new(image_base, image_size);
            let c = Box::into_raw(cpu);
            for (rva, f) in map { (*c).func_map.insert(rva, f); }
            (*c).gpr.rcx = param;
            log_line(&format!("[gow.thread] start rva=0x{start_rva:x} param=0x{param:x} caller_rva=0x{:x} tid={tid}",
                crate::crashlog::current_rva()));
            crate::dispatch::gow_run(c, start_rva as u32);
            *finished.0.lock().unwrap() = true;
            finished.1.notify_all();
            drop(Box::from_raw(c));
        });
        id
    }

    /// The stable guest id behind a CreateThread handle (GetThreadId).
    pub fn thread_id(&self, handle: u64) -> u64 {
        self.tids.lock().unwrap().get(&handle).copied().unwrap_or(0)
    }

    /// Is this handle one of our thread handles? (D019)
    pub fn is_thread(&self, handle: u64) -> bool {
        self.done.lock().unwrap().contains_key(&handle)
    }

    /// Has the thread behind this handle finished? (D019)
    pub fn poll_done(&self, handle: u64) -> bool {
        self.done.lock().unwrap().get(&handle).map(|f| *f.0.lock().unwrap()).unwrap_or(false)
    }

    /// Wait for the thread behind this handle to finish. Returns false for an
    /// unknown handle or a timeout. (D019)
    pub fn wait_done(&self, handle: u64, timeout_ms: u64) -> bool {
        let Some(f) = self.done.lock().unwrap().get(&handle).cloned() else { return false; };
        let (lock, cv) = &*f;
        let mut d = lock.lock().unwrap();
        if *d { return true; }
        if timeout_ms == 0 { return false; }
        if timeout_ms == 0xFFFF_FFFF {
            while !*d {
                d = cv.wait(d).unwrap();
            }
            return true;
        }
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(timeout_ms.min(0xFFFF_FFFE));
        while !*d {
            let now = std::time::Instant::now();
            if now >= deadline { return false; }
            let (nd, _) = cv.wait_timeout(d, deadline - now).unwrap();
            d = nd;
        }
        true
    }
}

impl Default for ThreadTable {
    fn default() -> Self { Self::new() }
}
