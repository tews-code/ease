//! Slab allocator

use crate::kernel::sync::AllocatorLock;
use core::alloc::GlobalAlloc;
#[cfg(test)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

// Heap is divided into equal sized slots
// const SLOT_SIZE: usize = 64; // 64 bytes per slot - this sets the minimum allocation
// const SLOT_COUNT: usize = 4096; // 4096 x 64 = 256KB - matches linker script

#[cfg(test)]
pub(crate) static ALLOC_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static ALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static DEALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static PADDING_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
pub(crate) static HEAP_TOP: AtomicUsize = AtomicUsize::new(0);

//  Heap Memory
//  +------+------+------+------+------+--------+----------+
//  | slot0| slot1| slot2| slot3| slot4| slotN-1|  slotN   |
//  |      |      |      |      |      |        |          |
//  |  64B |  64B |  64B |  64B |      |  64B   |          |
//  |      |      |      |      |      |        |          |
//  | used |ptr->3| used |ptr->4|ptr->9| used   | ptr:Null |
//  +------+------+------+------+------+--------+----------+
//         ^      ^
//         |      + dealloc_ptr points to start of used slab
//         |
//         +--- head is pointer to start of free list
//
//  Slot0 is used
//  Slot1 points to next free slot index 3 (and free_head points here)
//  Slot2 is used
//  Slot3 points to next free slot 4 etc.
//  Null pointer marks end of list

/// Free slab pointer
struct FreeSlab {
    next: *mut FreeSlab,
}

// Inner is protected by lock and allows interior mutability
struct SlabInner {
    heap: *mut u8, // base, for validation and provenance
    head: *mut FreeSlab,
}

// FreeList only references data on the heap
// No thread-local references
// Note that if SlabInner is Send, Slab is Sync from AllocatorLock
unsafe impl Send for SlabInner {}

// Slab allocator
pub(crate) struct Pool<const SLOT_SIZE: usize, const SLOT_COUNT: usize> {
    inner: AllocatorLock<SlabInner>,
}

impl<const SLOT_SIZE: usize, const SLOT_COUNT: usize> Pool<SLOT_SIZE, SLOT_COUNT> {
    pub const fn new() -> Self {
        Self {
            inner: AllocatorLock::new(SlabInner {
                heap: core::ptr::null_mut(),
                head: core::ptr::null_mut(),
            }),
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
    pub unsafe fn init(&self, heap_ptr: *mut u8, size: usize) {
        let mut inner = self.inner.lock();
        // Only initialise once
        assert!(inner.head.is_null());
        assert!(inner.heap.is_null());
        // Derive heap provenance from the heap pointer
        inner.heap = heap_ptr;
        inner.head = heap_ptr as *mut FreeSlab;
        // Heap slot array must match size of the heap from the linker script
        assert_eq!(SLOT_COUNT * SLOT_SIZE, size);
        // Set up the free list
        unsafe {
            let base = inner.heap as *mut FreeSlab;
            for i in 0..SLOT_COUNT {
                let current = base.with_addr(base.addr() + i * SLOT_SIZE);
                let next = if i + 1 < SLOT_COUNT {
                    base.with_addr(base.addr() + (i + 1) * SLOT_SIZE)
                } else {
                    core::ptr::null_mut()
                };
                core::ptr::write(current, FreeSlab { next });
            }
        }
    }
}

unsafe impl<const SLOT_SIZE: usize, const SLOT_COUNT: usize> GlobalAlloc
    for Pool<SLOT_SIZE, SLOT_COUNT>
{
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // Zero-size requests get a dangling, non-null, aligned pointer
        // per the GlobalAlloc convention. No slot is consumed, which
        // keeps the slab's full capacity available for real
        // allocations. dealloc mirrors this as a no-op when
        // layout.size() == 0.
        if layout.size() == 0 {
            return core::ptr::without_provenance_mut(layout.align());
        }
        // Early panic if allocation request is too large
        if layout.size() > SLOT_SIZE {
            return core::ptr::null_mut();
        }
        if layout.align() > SLOT_SIZE {
            return core::ptr::null_mut();
        }
        // Get the top of the list
        let mut inner = self.inner.lock();
        // Ensure we are not using alloc before init
        assert!(!inner.heap.is_null());
        let current = inner.head;
        // OOM check - current is pointing to last slab
        if current.is_null() {
            return core::ptr::null_mut();
        }

        // Allocate a slab
        // List head points to what current is pointing to
        inner.head = unsafe { (*current).next };

        #[cfg(test)]
        let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        let _ = ALLOCATED_BYTES.fetch_add(SLOT_SIZE as u32, Ordering::Relaxed);
        #[cfg(test)]
        let _ = PADDING_BYTES.fetch_add((SLOT_SIZE - layout.size()) as u32, Ordering::Relaxed);
        #[cfg(test)]
        let _ = HEAP_TOP.fetch_max(current.addr() + SLOT_SIZE, Ordering::Relaxed);

        // Return a pointer to the current slab
        current as *mut u8
    }

    unsafe fn dealloc(&self, dealloc_ptr: *mut u8, layout: core::alloc::Layout) {
        // Mirror alloc's zero-size path: the pointer is dangling (not
        // in the heap), so there's nothing to free and the free-list
        // must not be touched. Callers that received a dangling
        // pointer from alloc must pass the same zero-size layout
        // back here per the GlobalAlloc contract.
        if layout.size() == 0 {
            return;
        }
        // Calculate the number of slabs for dealloc_ptr's offset
        let mut inner = self.inner.lock();
        let dealloc_offset = dealloc_ptr.addr() - inner.heap.addr();
        // Is the pointer within the heap range?
        assert!(dealloc_offset < SLOT_COUNT * SLOT_SIZE);
        // Is the offset aligned to SLOT_SIZE?
        assert!(dealloc_offset.is_multiple_of(SLOT_SIZE));

        // SAFETY: `dealloc_ptr` was returned by a prior `alloc` on this
        // allocator (GlobalAlloc contract), so it points to a valid
        // SLOT_SIZE-byte slot within the heap. We re-derive the write
        // pointer from `inner.heap` to guarantee heap-wide provenance
        // regardless of the caller's pointer history — writing through
        // `dealloc_ptr` directly would use only the caller's (possibly
        // narrower) tag.
        unsafe {
            let slot_ptr = inner.heap.with_addr(dealloc_ptr.addr()) as *mut FreeSlab;
            core::ptr::write(slot_ptr, FreeSlab { next: inner.head });
            inner.head = slot_ptr;
        }
        #[cfg(test)]
        let _ = DEALLOCATED_BYTES.fetch_add(SLOT_SIZE as u32, Ordering::Relaxed);
    }
}

// Host-runnable unit tests for the FreeBlockList algorithm. These exercise
// the same `alloc` and `dealloc` code as the kernel uses, but against a
// host-allocated heap region. They are gated on `not(target_os = "none")`
// so they only build inside the lib crate (`cargo test --lib`) and not in
// the kernel binary build.
//
// Run with:
//     cargo test --lib --target $HOST_TARGET
// Run under Miri to verify soundness:
//     cargo +nightly miri test --lib --target $HOST_TARGET
#[cfg(all(test, not(target_os = "none")))]
mod host_tests {
    use super::*;
    use core::alloc::Layout;

    // Values for the test pool. Small enough that Miri finishes quickly
    // (each test allocates at most SLOT_COUNT slots and under Miri every
    // alloc/dealloc is slow), large enough to exercise OOM, free-order
    // independence, and the 32-iteration alignment test below. Kernel
    // production values are picked separately in main.rs.
    const SLOT_SIZE: usize = 64;
    const SLOT_COUNT: usize = 32;
    const HEAP_BYTES: usize = SLOT_COUNT * SLOT_SIZE;

    type TestPool = Pool<SLOT_SIZE, SLOT_COUNT>;

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

    fn make_allocator(heap: &TestHeap) -> TestPool {
        let allocator = TestPool::new();
        // SAFETY: The TestHeap region is exclusively owned by the
        // returned allocator for as long as the test holds a reference.
        unsafe { allocator.init(heap.ptr, heap.size) };
        allocator
    }

    #[test]
    fn basic_alloc_dealloc() {
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(8, 8).unwrap();
        let p1 = unsafe { a.alloc(layout) };
        let p2 = unsafe { a.alloc(layout) };
        assert!(!p1.is_null());
        assert!(!p2.is_null());
        assert_ne!(p1, p2);
        assert!(p2 as usize > p1 as usize);
        unsafe {
            a.dealloc(p1, layout);
            a.dealloc(p2, layout);
        }
    }

    #[test]
    fn zero_size_alloc_round_trips() {
        // Zero-size requests must succeed, return a non-null aligned
        // pointer, and round-trip through dealloc without panicking.
        // A real slot should NOT be consumed — all SLOT_COUNT slots must
        // still be available for subsequent real allocations.
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let zero_layout = Layout::from_size_align(0, 8).unwrap();
        let p = unsafe { a.alloc(zero_layout) };
        assert!(!p.is_null(), "zero-size alloc returned null");
        assert_eq!(
            p.addr() % 8,
            0,
            "zero-size pointer not aligned to requested alignment"
        );
        unsafe { a.dealloc(p, zero_layout) };

        // Every slot must still be available — no real slot was consumed.
        let real_layout = Layout::from_size_align(64, 8).unwrap();
        let mut allocations = Vec::new();
        loop {
            let q = unsafe { a.alloc(real_layout) };
            if q.is_null() {
                break;
            }
            allocations.push(q);
        }
        assert_eq!(
            allocations.len(),
            SLOT_COUNT,
            "zero-size alloc consumed a real slot"
        );
        for q in allocations {
            unsafe { a.dealloc(q, real_layout) };
        }
    }

    #[test]
    fn alloc_returns_unique_writable_memory() {
        // Each returned pointer must be a distinct, writable region.
        // We stamp each block with a recognisable pattern, then verify
        // the patterns are still intact at the end — proves no two
        // allocations overlap.
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let mut ptrs = Vec::new();
        for i in 0..8 {
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null());
            unsafe { core::ptr::write_bytes(p, (i + 1) as u8, 64) };
            ptrs.push(p);
        }
        for (i, p) in ptrs.iter().enumerate() {
            let expected = (i + 1) as u8;
            for off in 0..64 {
                let byte = unsafe { *p.add(off) };
                assert_eq!(byte, expected, "block {} corrupted at offset {}", i, off);
            }
        }
        for p in ptrs {
            unsafe { a.dealloc(p, layout) };
        }
    }

    #[test]
    fn alloc_until_oom_then_recover() {
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let mut allocations = Vec::new();
        loop {
            let p = unsafe { a.alloc(layout) };
            if p.is_null() {
                break;
            }
            allocations.push(p);
        }
        assert!(!allocations.is_empty(), "did not allocate even one block");
        for &p in &allocations {
            unsafe { a.dealloc(p, layout) };
        }
        // After freeing everything we should be able to allocate again.
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null(), "could not allocate after freeing all blocks");
        unsafe { a.dealloc(p, layout) };
    }

    #[test]
    fn dealloc_in_any_order_returns_slot_to_pool() {
        // Slots are independent — freeing them in any order must put each
        // one back on the free list so it can be handed out again.
        for free_order in &[[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]] {
            let heap = TestHeap::new(HEAP_BYTES);
            let a = make_allocator(&heap);
            let layout = Layout::from_size_align(64, 8).unwrap();
            let original = [
                unsafe { a.alloc(layout) },
                unsafe { a.alloc(layout) },
                unsafe { a.alloc(layout) },
            ];
            assert!(original.iter().all(|p| !p.is_null()));
            for &i in free_order {
                unsafe { a.dealloc(original[i], layout) };
            }
            // After freeing all three, three more allocations must succeed
            // and must be drawn from exactly the same three slots.
            let reused = [
                unsafe { a.alloc(layout) },
                unsafe { a.alloc(layout) },
                unsafe { a.alloc(layout) },
            ];
            assert!(reused.iter().all(|p| !p.is_null()));
            let mut original_sorted = original;
            original_sorted.sort();
            let mut reused_sorted = reused;
            reused_sorted.sort();
            assert_eq!(
                original_sorted, reused_sorted,
                "freed slots not returned to pool for free order {:?}",
                free_order
            );
            for &p in &reused {
                unsafe { a.dealloc(p, layout) };
            }
        }
    }

    #[test]
    fn every_alloc_is_slot_aligned() {
        // The slab carves the heap into SLOT_SIZE-sized slots and ignores
        // `Layout::align`. Every returned pointer must be SLOT_SIZE-aligned
        // (the test heap is page-aligned, so slot alignment is absolute,
        // not just relative to the heap base). Oversized requests must
        // return null rather than corrupt the free list.
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);

        let layout = Layout::from_size_align(8, 8).unwrap();
        let mut ptrs = Vec::new();
        for _ in 0..32 {
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null());
            assert_eq!(
                (p as usize) % SLOT_SIZE,
                0,
                "slab handed out a non-slot-aligned pointer: {:p}",
                p
            );
            ptrs.push(p);
        }

        // Anything larger than a single slot must be refused.
        let oversize = Layout::from_size_align(SLOT_SIZE + 1, 8).unwrap();
        let too_big = unsafe { a.alloc(oversize) };
        assert!(too_big.is_null(), "oversized alloc was not refused");

        for p in ptrs {
            unsafe { a.dealloc(p, layout) };
        }
    }

    #[test]
    fn repeated_alloc_free_cycles_preserve_capacity() {
        // The slab has fixed-size slots and never fragments. Every cycle of
        // (allocate until OOM, free everything) must yield exactly the same
        // number of slots — equal to SLOT_COUNT — proving the free list is
        // fully restored each time and no slots leak.
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(32, 8).unwrap();
        for cycle in 0..3 {
            let mut allocations = Vec::new();
            loop {
                let p = unsafe { a.alloc(layout) };
                if p.is_null() {
                    break;
                }
                allocations.push(p);
            }
            assert_eq!(
                allocations.len(),
                SLOT_COUNT,
                "cycle {} did not return full capacity",
                cycle
            );
            for p in allocations {
                unsafe { a.dealloc(p, layout) };
            }
        }
    }
}
