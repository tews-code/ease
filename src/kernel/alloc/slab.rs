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
//      Slab0
//    +------+------+------+------+------+--------+------+
//    | slot0| slot1| slot2| slot3| slot4| slotN-1| slotN|
//    |      |      |      |      |      |        |      |
//    |  64B |  64B |  64B |  64B |      |  64B   |      |
//    |      |
//    |Slab  |ptr->N| used |ptr->4|ptr->1|ptr:Null|  used|
//    +Header+------+------+------+--------+------+------+
//    |.count|      ^      ^
//    |.head |------|------+ head is pointer to start of free list
// +- |.next |      |
// |  +------+      |
// |                |
// |                + dealloc_ptr points to start of used slot
// +--+
//    |
//    v Slab1
//    +------+------+------+------+------+--------+------+
//    | slot0| slot1| slot2| slot3| slot4| slotN-1| slotN|
//    |      |      |      |      |      |        |      |
//    |  64B |  64B |  64B |  64B |      |  64B   |      |
//    |      |      |      |      |      |        |      |
//    |Slab  |ptr->N|used  |ptr->4|ptr->1|  used  | used |
//    +Header+------+------+------+------+--------+------+
//    |.count|             ^
//    |.head |-------------+
//    |.next:|
//    | Null |
//    +------+

/// Free slot pointer for linked list
struct FreeSlot {
    next: *mut FreeSlot,
}

/// Slab header (placed in first slot) linked list
struct SlabHeader {
    next: *mut SlabHeader,
    head: *mut FreeSlot,
    free_count: usize,
}

// Slab inner for interior mutability
struct SlabInner {
    next: *mut SlabHeader,
    size: usize,
}

// FreeList only references data on the heap
// No thread-local references
// Note that if SlabHeader is Send, Slab is Sync from AllocatorLock
unsafe impl Send for SlabInner {}

// Slab allocator
pub(crate) struct Slab<const SLOT_SIZE: usize> {
    inner: AllocatorLock<SlabInner>,
}

impl<const SLOT_SIZE: usize> Slab<SLOT_SIZE> {
    // The slot size must be large enough for the header
    const CHECK: () = assert!(core::mem::size_of::<SlabHeader>() <= SLOT_SIZE);

    pub const fn new() -> Self {
        Self {
            inner: AllocatorLock::new(SlabInner {
                next: core::ptr::null_mut(),
                size: 0,
            }),
        }
    }

    /// Add a slab to the allocator with a heap region of `size` bytes
    /// starting at `start`. Used to initialise or add additional slabs.
    ///
    /// # Safety
    /// Caller must guarantee:
    ///   - `[start, start + size)` is valid for reads and writes for
    ///     the entire lifetime of the allocator.
    ///   - The region is exclusively owned by this allocator.
    ///   - The slab is aligned to the slab size
    ///   - Each additional slab is the same size as the first slab
    /// # Panics
    /// Panics if:
    ///   - `size` is not a multiple of and greater than `SLOT_SIZE`
    ///   - The slot size is too small for the slab header list (8 bytes)
    ///   - Additional slab size is not the same as the first slab size
    pub unsafe fn add_slab(&self, start: *mut u8, size: usize) {
        // Heap size of the slab must must be a multiple of the slot size
        // Note that size must be larger than SLOT_SIZE as the first slot
        // is used for metadata
        assert!(size > SLOT_SIZE);
        // Slot must be aligned to support bitmask header trick
        assert!(size.is_power_of_two(), "size must be a power of two");
        assert!(
            start.addr().is_multiple_of(size),
            "start must be aligned to size"
        );

        // Get the slab list head details
        let mut slab = self.inner.lock();
        // Each additional slab must be the same size as the first slab
        // If we are not the first slab, make sure this is the case
        if !slab.next.is_null() {
            assert_eq!(size, slab.size, "all slabs must be the same size");
        }

        slab.size = size; // Will be overwritten but with the same value each time
        let slot_count = size / SLOT_SIZE;
        // We can insert the new slab into the list by pointing the head to
        // the begining of the new slab, and point the end of the new slab
        // to the current free slot (what head was pointing to)
        //
        // If we being called the first time (init) then the head is null
        // and the logic remains the same.

        // Update the header of the previous slab to point to the new slab
        unsafe {
            core::ptr::write(
                start as *mut SlabHeader,
                SlabHeader {
                    next: slab.next, // Prepend
                    head: start.with_addr(start.addr() + SLOT_SIZE) as *mut FreeSlot,
                    free_count: slot_count - 1,
                },
            );
        }

        // Now create a free slot list across the new slab
        // The last slot points to the old head pointee
        let base = start as *mut FreeSlot;
        // Fill the rest of the slab with a free list
        for i in 1..slot_count {
            let current = base.with_addr(base.addr() + i * SLOT_SIZE);
            let next = if i + 1 < slot_count {
                base.with_addr(base.addr() + (i + 1) * SLOT_SIZE)
            } else {
                core::ptr::null_mut()
            };
            unsafe {
                core::ptr::write(current, FreeSlot { next });
            }
        }

        // Save the start address and size
        slab.next = start as *mut SlabHeader;
    }

    /// Reclaim an empty slab
    ///
    /// Returns None if there are used slots, or a pointer and size if
    /// the slab can be reclaimed
    pub unsafe fn reclaim_slab(&self, reclaim_slab: *mut u8) -> Option<(*mut u8, usize)> {
        // Get the slab list head details
        let mut slab = self.inner.lock();

        // Walk the list to find the prev and next in the slab header list
        // We don't trust the reclaim slab pointer that we've been given
        let mut current = slab.next;
        let mut prev: *mut SlabHeader = core::ptr::null_mut();
        while !current.is_null() {
            if current == reclaim_slab as *mut SlabHeader {
                if unsafe { (*(reclaim_slab as *mut SlabHeader)).free_count }
                    != (slab.size / SLOT_SIZE) - 1
                {
                    return None;
                }
                // Unlink this slab header from the list and return
                if !prev.is_null() {
                    unsafe { (*prev).next = (*current).next };
                } else {
                    slab.next = unsafe { (*current).next };
                }
                return Some((current as *mut u8, slab.size));
            }
            prev = current;
            current = unsafe { (*current).next };
        }
        // Did not find the slab in the list
        None
    }
}

unsafe impl<const SLOT_SIZE: usize> GlobalAlloc for Slab<SLOT_SIZE> {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // Zero-size requests get a dangling, non-null, aligned pointer
        // per the GlobalAlloc convention. No slot is consumed, which
        // keeps the slab's full capacity available for real
        // allocations. dealloc mirrors this as a no-op when
        // layout size is 0.
        if layout.size() == 0 {
            return core::ptr::without_provenance_mut(layout.align());
        }
        // Early return if allocation request is too large
        if layout.size() > SLOT_SIZE {
            return core::ptr::null_mut();
        }
        if layout.align() > SLOT_SIZE {
            return core::ptr::null_mut();
        }
        // Get the slab list header
        let slab = self.inner.lock();

        // Pop a free slot by walking all the slabs
        let mut current = slab.next;
        while !current.is_null() {
            if unsafe { !(*current).head.is_null() } {
                let allocated = unsafe { (*current).head };
                unsafe {
                    (*current).head = (*allocated).next;
                    (*current).free_count -= 1;
                }

                #[cfg(test)]
                let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                #[cfg(test)]
                let _ = ALLOCATED_BYTES.fetch_add(SLOT_SIZE as u32, Ordering::Relaxed);
                #[cfg(test)]
                let _ =
                    PADDING_BYTES.fetch_add((SLOT_SIZE - layout.size()) as u32, Ordering::Relaxed);
                #[cfg(test)]
                let _ = HEAP_TOP.fetch_max(current.addr() + SLOT_SIZE, Ordering::Relaxed);

                // Return a pointer to the current slot
                return allocated as *mut u8;
            } else {
                current = unsafe { (*current).next };
            }
        }

        // OOM
        core::ptr::null_mut()
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
        // Get the slab list header details
        let slab = self.inner.lock();

        // Find the slab header
        let header =
            dealloc_ptr.with_addr(dealloc_ptr.addr() & !(slab.size - 1)) as *mut SlabHeader;

        // Calculate the number of slots for dealloc_ptr's offset
        // SAFETY: `dealloc_ptr` was returned by a prior `alloc` on this
        // allocator and not yet freed, per the GlobalAlloc contract. It
        // therefore points to a writable SLOT_SIZE-byte slot with provenance
        // sufficient for the FreeSlab write below. The caller surrendered
        // ownership by calling dealloc, so no aliasing reference exists.
        unsafe {
            let slot_ptr = dealloc_ptr as *mut FreeSlot;
            core::ptr::write(
                slot_ptr,
                FreeSlot {
                    next: (*header).head,
                },
            );
            (*header).head = slot_ptr;
            (*header).free_count += 1;
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
#[cfg(all(test, not(target_os = "none"), feature = "test-alloc"))]
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
    // Slot 0 holds the slab header, so the number of slots a caller can
    // actually allocate is one less than SLOT_COUNT.
    const USABLE_SLOTS: usize = SLOT_COUNT - 1;

    type TestPool = Slab<SLOT_SIZE>;

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
        unsafe { allocator.add_slab(heap.ptr, heap.size) };
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
            USABLE_SLOTS,
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
        for _ in 0..USABLE_SLOTS {
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
                USABLE_SLOTS,
                "cycle {} did not return full capacity",
                cycle
            );
            for p in allocations {
                unsafe { a.dealloc(p, layout) };
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Multi-slab tests: add_slab beyond the first call, and reclaim_slab.
    //
    // For multi-slab tests we allocate a 2×HEAP_BYTES region and split
    // it into two HEAP_BYTES-sized halves. The TestHeap's Layout is
    // 4096-aligned, so each half is HEAP_BYTES-aligned (the
    // `add_slab` precondition).
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn add_two_slabs_doubles_capacity() {
        let big_heap = TestHeap::new(2 * HEAP_BYTES);
        let a = TestPool::new();
        let slab1 = big_heap.ptr;
        let slab2 = unsafe { big_heap.ptr.add(HEAP_BYTES) };
        unsafe {
            a.add_slab(slab1, HEAP_BYTES);
            a.add_slab(slab2, HEAP_BYTES);
        }

        let layout = Layout::from_size_align(8, 8).unwrap();
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
            2 * USABLE_SLOTS,
            "two slabs should give 2× usable capacity"
        );
        for p in allocations {
            unsafe { a.dealloc(p, layout) };
        }
    }

    #[test]
    fn reclaim_refuses_non_empty_slab() {
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());

        // Slab is non-empty — one slot allocated.
        let result = unsafe { a.reclaim_slab(heap.ptr) };
        assert!(result.is_none(), "reclaim must refuse a non-empty slab");

        unsafe { a.dealloc(p, layout) };
    }

    #[test]
    fn reclaim_returns_pointer_and_size() {
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        // No allocations yet — slab is fully empty.
        let result = unsafe { a.reclaim_slab(heap.ptr) };
        assert!(result.is_some(), "reclaim must succeed on an empty slab");
        let (p, size) = result.unwrap();
        assert_eq!(p as usize, heap.ptr as usize);
        assert_eq!(size, HEAP_BYTES);
    }

    #[test]
    fn reclaim_succeeds_after_full_alloc_dealloc_cycle() {
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(64, 8).unwrap();

        // Fill the slab.
        let mut allocations = Vec::new();
        loop {
            let p = unsafe { a.alloc(layout) };
            if p.is_null() {
                break;
            }
            allocations.push(p);
        }
        assert_eq!(allocations.len(), USABLE_SLOTS);

        // Free everything in reverse order to scramble the free list.
        while let Some(p) = allocations.pop() {
            unsafe { a.dealloc(p, layout) };
        }

        // Slab is empty again — reclaim should succeed.
        let result = unsafe { a.reclaim_slab(heap.ptr) };
        assert!(
            result.is_some(),
            "reclaim should succeed after all slots are returned"
        );
    }

    #[test]
    fn reclaim_makes_alloc_fail_when_only_slab() {
        // After reclaiming the sole slab, no memory remains: alloc must
        // return null.
        let heap = TestHeap::new(HEAP_BYTES);
        let a = make_allocator(&heap);
        let result = unsafe { a.reclaim_slab(heap.ptr) };
        assert!(result.is_some());

        let layout = Layout::from_size_align(8, 8).unwrap();
        let p = unsafe { a.alloc(layout) };
        assert!(p.is_null(), "alloc should return null when no slabs remain");
    }

    #[test]
    fn reclaim_head_slab_keeps_other_usable() {
        // Two slabs added; the second-added is at slab_header_start
        // (prepend semantics). Reclaim it; remaining slab still serves.
        let big_heap = TestHeap::new(2 * HEAP_BYTES);
        let a = TestPool::new();
        let slab1 = big_heap.ptr;
        let slab2 = unsafe { big_heap.ptr.add(HEAP_BYTES) };
        unsafe {
            a.add_slab(slab1, HEAP_BYTES);
            a.add_slab(slab2, HEAP_BYTES);
        }

        // slab2 was added second → it's the head of the slab list.
        let result = unsafe { a.reclaim_slab(slab2) };
        assert!(result.is_some(), "reclaim of head slab should succeed");

        // Remaining allocations must come from slab1 only.
        let layout = Layout::from_size_align(8, 8).unwrap();
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
            USABLE_SLOTS,
            "after reclaiming one of two slabs, capacity should drop to one slab's worth"
        );
        for p in &allocations {
            let addr = *p as usize;
            assert!(
                addr >= slab1 as usize && addr < slab1 as usize + HEAP_BYTES,
                "allocation {:p} should fall in slab1 [{:p}, +{}]",
                p,
                slab1,
                HEAP_BYTES
            );
        }
        for p in allocations {
            unsafe { a.dealloc(p, layout) };
        }
    }

    #[test]
    fn reclaim_tail_slab_keeps_head_usable() {
        // Reclaim the slab that's *not* at slab_header_start — exercises
        // the "previous != null" splice-out branch in the slab-list walk.
        let big_heap = TestHeap::new(2 * HEAP_BYTES);
        let a = TestPool::new();
        let slab1 = big_heap.ptr;
        let slab2 = unsafe { big_heap.ptr.add(HEAP_BYTES) };
        unsafe {
            a.add_slab(slab1, HEAP_BYTES);
            a.add_slab(slab2, HEAP_BYTES);
        }

        // slab1 was added first → it's the tail of the slab list (slab2
        // is head). Reclaim slab1.
        let result = unsafe { a.reclaim_slab(slab1) };
        assert!(result.is_some(), "reclaim of tail slab should succeed");

        // Remaining capacity is one slab's worth, drawn from slab2.
        let layout = Layout::from_size_align(8, 8).unwrap();
        let mut allocations = Vec::new();
        loop {
            let p = unsafe { a.alloc(layout) };
            if p.is_null() {
                break;
            }
            allocations.push(p);
        }
        assert_eq!(allocations.len(), USABLE_SLOTS);
        for p in &allocations {
            let addr = *p as usize;
            assert!(
                addr >= slab2 as usize && addr < slab2 as usize + HEAP_BYTES,
                "allocation should fall in slab2"
            );
        }
        for p in allocations {
            unsafe { a.dealloc(p, layout) };
        }
    }
}
