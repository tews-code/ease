//! Bump Allocator

// Rough Diagram of memory layout fo SRAM
//
// 0x8000_0000          +-------------------+       SRAM start
//                      |                   |
//                      |       text        |
//                      |       rodata      |
//                      |       data        |
//                      |       bss         |
//
//                      ~      ~100kb       ~
//                      +-------------------+
// 0x8002_0000          |     String        |       Heap start (aligns to 16 - ends 0 in hex) - linker symbol
//                      |     Vec           |
// 0x8002_0132          |                   |   <-  Next heap allocation
//                      ~                   ~           "
//                      |                   |           v (Grows upwards to end of SRAM)
//                      |                   |
// 0x8006_0000          |-------------------|   <- Top of heap
// 0x8006_0004          |                   |   <- Stack guard
//                      |                   |
//                      ~                   ~           ^ (Grows downwards toward start of SRAM)
//                      |                   |           "
//                      |   StackVec        |   <- Current stack pointer
//                      |   TrapFrame       |
// 0x80082000           +-------------------+   <- End of SRAM

use core::alloc::{GlobalAlloc, Layout};
#[cfg(test)]
use core::sync::atomic::AtomicU32;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::kernel::alloc::align_up;

#[cfg(test)]
pub(crate) static ALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static DEALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static ALLOC_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static PADDING_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static HEAP_TOP: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Bump {
    next: AtomicPtr<u8>,
    start: AtomicUsize,
    end: AtomicUsize,
}

impl Bump {
    pub const fn new() -> Self {
        Self {
            next: AtomicPtr::new(core::ptr::null_mut()),
            start: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }

    /// Initialise the allocator with a heap region of `size` bytes
    /// starting at `start`. Must be called exactly once before any
    /// allocations.
    ///
    /// # Safety
    /// Caller must guarantee:
    ///   - `[start, start + size)` is valid for reads and writes for
    ///     the entire lifetime of the allocator.
    ///   - The region is exclusively owned by this allocator.
    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        debug_assert!(self.start.load(Ordering::Relaxed) == 0);
        let start_addr = start as usize;
        let end_addr = start_addr + size;
        self.start.store(start_addr, Ordering::Relaxed);
        self.end.store(end_addr, Ordering::Relaxed);
        self.next.store(start, Ordering::Relaxed);
    }

    /// Reset the allocator and zero all benchmark counters. Called from
    /// the shared bench harness between phases.
    ///
    /// # Safety
    /// Caller must guarantee no allocations made before this call are
    /// still live — every prior pointer must be unreachable.
    #[cfg(all(test, feature = "test-alloc"))]
    pub(crate) unsafe fn reset_for_bench(&self) {
        let _ = self.next.fetch_ptr_sub(
            self.next.load(Ordering::Relaxed) as usize - self.start.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        ALLOC_COUNT.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        DEALLOCATED_BYTES.store(0, Ordering::Relaxed);
        PADDING_BYTES.store(0, Ordering::Relaxed);
        HEAP_TOP.store(0, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Zero-size requests get a dangling, non-null, aligned pointer
        // per the GlobalAlloc convention. No heap space is reserved, so
        // dealloc can mirror this as a no-op and the cursor is never
        // advanced for zero-size requests — avoiding aliasing with the
        // next real allocation.
        if layout.size() == 0 {
            return core::ptr::without_provenance_mut(layout.align());
        }
        let mut current = self.next.load(Ordering::Relaxed);
        debug_assert!(!current.is_null());
        let end = self.end.load(Ordering::Relaxed);
        loop {
            let next_padding = align_up(current.addr(), layout.align()) - current.addr();
            let next = current.wrapping_add(next_padding);
            // OOM check
            let new = next.wrapping_add(layout.size());
            if new.addr() > end {
                return core::ptr::null_mut();
            }
            match self.next.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    #[cfg(test)]
                    let _ = ALLOCATED_BYTES.fetch_add(layout.size() as u32, Ordering::Relaxed);
                    #[cfg(test)]
                    let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    let _ = PADDING_BYTES.fetch_add((next_padding) as u32, Ordering::Relaxed);
                    #[cfg(test)]
                    HEAP_TOP.store(new.addr(), Ordering::Relaxed);

                    return next;
                }
                Err(actual) => current = actual,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator does not dealloc
    }
}

// Host-runnable unit tests for the bump allocator. Run on the host
// target so they execute under both `cargo test --lib` and Miri.
#[cfg(all(test, not(target_os = "none")))]
mod host_tests {
    use super::*;
    use core::alloc::Layout;

    /// Owns a chunk of host memory that the test allocator treats as the heap.
    /// Freed automatically when the helper goes out of scope.
    struct TestHeap {
        ptr: *mut u8,
        size: usize,
        layout: Layout,
    }

    impl TestHeap {
        fn new(size: usize) -> Self {
            // 4096-aligned so high-alignment allocation tests have somewhere to land
            let layout = Layout::from_size_align(size, 4096).unwrap();
            // SAFETY: Layout is non-zero and aligned; std::alloc returns
            // a region we own until we call dealloc with the same layout.
            let ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!ptr.is_null(), "host alloc failed");
            Self { ptr, size, layout }
        }
    }

    impl Drop for TestHeap {
        fn drop(&mut self) {
            // SAFETY: ptr/layout match the values returned by std::alloc::alloc
            // in `new`, and the region is no longer in use.
            unsafe {
                std::alloc::dealloc(self.ptr, self.layout);
            }
        }
    }

    fn make_allocator(heap: &TestHeap) -> Bump {
        let allocator = Bump::new();
        // SAFETY: The TestHeap region is exclusively owned by the
        // returned allocator for as long as the test holds a reference.
        unsafe { allocator.init(heap.ptr, heap.size) };
        allocator
    }

    #[test]
    fn basic_alloc() {
        let heap = TestHeap::new(4096);
        let bump = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let p1 = unsafe { bump.alloc(layout) };
        let p2 = unsafe { bump.alloc(layout) };
        assert!(!p1.is_null());
        assert!(!p2.is_null());
        assert_ne!(p1, p2);
        assert!(
            p2 as usize > p1 as usize,
            "bump must hand out increasing addresses"
        );
    }

    #[test]
    fn zero_size_alloc_round_trips() {
        // Zero-size requests must succeed, return a non-null aligned
        // pointer, and round-trip through dealloc without panicking.
        // A real slot should NOT be consumed — subsequent real allocations
        // must still succeed with the full heap available.
        let heap = TestHeap::new(4096);
        let bump = make_allocator(&heap);
        let zero_layout = Layout::from_size_align(0, 8).unwrap();
        let p = unsafe { bump.alloc(zero_layout) };
        assert!(!p.is_null(), "zero-size alloc returned null");
        assert_eq!(
            p.addr() % 8,
            0,
            "zero-size pointer not aligned to requested alignment"
        );
        unsafe { bump.dealloc(p, zero_layout) };

        // Subsequent real allocation must succeed and must not overlap
        // with the dangling zero-size pointer.
        let real_layout = Layout::from_size_align(64, 8).unwrap();
        let q = unsafe { bump.alloc(real_layout) };
        assert!(!q.is_null(), "real alloc after zero-size alloc failed");
        assert_ne!(p, q, "real alloc overlapped with zero-size pointer");
    }

    #[test]
    fn dealloc_is_no_op() {
        // Bump's defining property: dealloc never returns memory to the
        // pool. After freeing a slot, the next allocation must come from
        // a fresh address, not the one we just released.
        let heap = TestHeap::new(4096);
        let bump = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let p1 = unsafe { bump.alloc(layout) };
        assert!(!p1.is_null());
        unsafe { bump.dealloc(p1, layout) };
        let p2 = unsafe { bump.alloc(layout) };
        assert!(!p2.is_null());
        assert_ne!(p1, p2, "bump reused a slot after dealloc");
        assert!(
            p2 as usize > p1 as usize,
            "bump must keep advancing past freed slots"
        );
    }

    #[test]
    fn alloc_until_oom_returns_null() {
        // Bump must signal OOM by returning null, not by panicking. This
        // is the property the higher-level `handle_alloc_error` panic
        // depends on — without a clean null return, the global-allocator
        // path would have undefined behaviour on exhaustion.
        let heap = TestHeap::new(4096);
        let bump = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let mut count = 0;
        loop {
            let p = unsafe { bump.alloc(layout) };
            if p.is_null() {
                break;
            }
            count += 1;
        }
        assert!(count > 0, "did not allocate even one block");
        // bump cannot free; another alloc must also be null
        let p = unsafe { bump.alloc(layout) };
        assert!(p.is_null(), "bump produced a slot after OOM");
    }
}
