// Sanity test for the tracked heap (D013/D017/D018).
// The heap table is process-global, so the tests must NOT run concurrently
// (a parallel run sees another test's live blocks and fails spuriously):
// every test holds HEAP_LOCK for its whole body.
use gow_runtime::win32::{host_alloc, host_alloc_aligned, host_free, host_realloc};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn heap_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn heap_alloc_free_cycle_reuses_memory() {
    let _g = heap_lock();
    let mut peak_live = 0usize;
    // 1000 cycles of alloc/free must not grow without bound.
    for i in 0..1000 {
        let p = host_alloc(1024);
        assert!(!p.is_null(), "alloc failed at {i}");
        unsafe {
            std::ptr::write_volatile(p, 0xAB);
            host_free(p);
        }
        let (live, blocks, _peak) = gow_runtime::win32::heap_stats();
        assert_eq!(blocks, 0, "block leaked at cycle {i}");
        peak_live = peak_live.max(live);
    }
    assert!(peak_live < 1024 * 1024, "unexpected live bytes: {peak_live}");
}

#[test]
fn heap_realloc_copies_and_frees_old() {
    let _g = heap_lock();
    let p = host_alloc(64);
    assert!(!p.is_null());
    unsafe {
        for i in 0..64 { std::ptr::write(p.add(i), i as u8); }
        let q = host_realloc(p, 128);
        assert!(!q.is_null());
        for i in 0..64 {
            assert_eq!(std::ptr::read(q.add(i)), i as u8, "byte {i} not copied");
        }
        let (_, blocks, _) = gow_runtime::win32::heap_stats();
        assert_eq!(blocks, 1, "old block was not freed (blocks={blocks})");
        host_free(q);
    }
    let (_, blocks, _) = gow_runtime::win32::heap_stats();
    assert_eq!(blocks, 0);
}

#[test]
fn heap_small_allocs_are_not_page_rounded() {
    let _g = heap_lock();
    // 1000 x 100-byte allocations must stay far below 1000 x 4096.
    let mut ptrs = Vec::new();
    for _ in 0..1000 {
        let p = host_alloc(100);
        assert!(!p.is_null());
        ptrs.push(p);
    }
    let (live, blocks, _) = gow_runtime::win32::heap_stats();
    assert_eq!(blocks, 1000);
    // 100 -> rounded to 112 bytes each => ~112_000 total, way below 4 MiB.
    assert!(live <= 200_000, "live={live} suggests page rounding is back");
    for p in ptrs { unsafe { host_free(p); } }
    let (_, blocks, _) = gow_runtime::win32::heap_stats();
    assert_eq!(blocks, 0);
}

#[test]
fn heap_aligned_alloc_honors_alignment() {
    let _g = heap_lock();
    let p = host_alloc_aligned(64, 64);
    assert!(!p.is_null());
    assert_eq!(p as usize % 64, 0, "alignment not honored");
    unsafe { host_free(p); }
}

#[test]
fn heap_big_reservation_is_lazy_and_freeable() {
    // The engine's boot pattern (D017/D018): one multi-MiB arena comes from
    // mmap(MAP_NORESERVE) — zeroed pages materialise on first touch — and
    // freeing it returns the address space. Reservations are tracked in
    // RESERVED_BYTES (heap_reserved()), not in the committed-block counters.
    let _g = heap_lock();
    let reserved_before = gow_runtime::win32::heap_reserved();
    let p = gow_runtime::win32::host_alloc_pages(64 * 1024 * 1024);
    assert!(!p.is_null(), "64 MiB reservation failed");
    assert_eq!(
        gow_runtime::win32::heap_reserved(),
        reserved_before + 64 * 1024 * 1024,
        "reservation not accounted"
    );
    unsafe {
        // Touch first and last page: must read back zero (fresh pages) and
        // must not fault (the mapping is writable).
        assert_eq!(std::ptr::read_volatile(p), 0);
        let last = p.add(64 * 1024 * 1024 - 4096);
        assert_eq!(std::ptr::read_volatile(last), 0);
        std::ptr::write_volatile(last, 0xCD);
        assert_eq!(std::ptr::read_volatile(last), 0xCD);
        host_free(p);
    }
    assert_eq!(
        gow_runtime::win32::heap_reserved(),
        reserved_before,
        "reservation not freed (munmap path)"
    );
}
