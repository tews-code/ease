// Free list allocator

// Memory (1 byte wide)
//
// __heap_start/
// 0x8001_0000  +-----------+
//              |0  next    |   next = NonNull pointer to 0x8001_0014 (4 bytes)
//              +-----------+
//              |1  next    |
//              +-----------+
//              |2  next    |
//              +-----------+
//              |3  next    |
//              +-----------+
//              |4  size    |   size = 12 bytes - Note count of 12 starts at 0x8001_0000 index 0, not from size's address
//              +-----------+
//              |5  size    |
//              +-----------+
//              |6  size    |
//              +-----------+
//              |7  size    |   Free block is 8 bytes in total
//              +-----------+
//              |8 garbage  |
//              +-----------+
//              |9 garbage  |
//              +-----------+
//              |10 garbage |
//              +-----------+
//              |11 garbage |
// 0x8001_000c  +-----------+
//              |   alloc   |   1 byte allocated
//              +-----------+
//              |   pad     |   Minimum allocation is 8 bytes to accommodate a free block on dealloc
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
// 0x8001_0014  +-----------+
//              |0  next    |  next = None
//              +-----------+
//              |1  next    |
//              +-----------+
//              |2  next    |
//              +-----------+
//              |3  next    |
//              +-----------+
//              |4  size    |   size = 8 bytes (minimum size)
//              +-----------+
//              |5  size    |
//              +-----------+
//              |6  size    |
//              +-----------+
//              |7  size    |   Free block is 8 bytes in total
// 0x8001_001c  +-----------+
//              |   alloc   |   2 byte allocation
//              +-----------+
//              |   alloc   |
//              +-----------+
//              |   pad     |   Padding
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
//              +-----------+
//              |   pad     |
// 0x8001_0024/ +-----------+
// __heap_end

#[cfg(test)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::kernel::alloc::align_up;
use crate::kernel::sync::AllocatorLock;

// Base alignment for every allocation. Derived from `FreeBlock` so the
// padding-split invariant holds on every target: any non-zero padding is
// always at least as large as a `FreeBlock`, so the padding-split branch
// in `alloc` can always materialise a free block in the padding region
// rather than leaking it.
//
// On 32-bit RISC-V this evaluates to 8 (matching the original hardcoded
// value); on 64-bit hosts it evaluates to 16 so the host tests work too.
const BASE_ALIGN: usize = core::mem::size_of::<FreeBlock>();

// BASE_ALIGN must satisfy FreeBlock's natural alignment so a FreeBlock
// header can be written at any BASE_ALIGN-aligned address.
const _: () = assert!(BASE_ALIGN >= core::mem::align_of::<FreeBlock>());
// BASE_ALIGN must be a power of two for the alignment math (`align_up`).
const _: () = assert!(BASE_ALIGN.is_power_of_two());

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

// Each freed memory block has a size and a link to the next free block
pub(crate) struct FreeBlock {
    next: *mut FreeBlock,
    size: usize,
}

impl FreeBlock {
    // Split this block at `offset` bytes. This block shrinks to `offset`,
    // a new block gets the remainder and is linked after self.
    // Returns a pointer to the newly create free block
    //
    // Safety: Caller must ensure `this` points to a valid FreeBlock
    // and offset must be larger than a free block to allow for a valid split
    unsafe fn split_right(this: *mut FreeBlock, offset: usize) -> (*mut FreeBlock, *mut FreeBlock) {
        assert!(
            offset >= core::mem::size_of::<FreeBlock>(),
            "split offset too small for a FreeBlock"
        );
        assert!(
            unsafe { (*this).size - offset >= core::mem::size_of::<FreeBlock>() },
            "split remainder too small for a FreeBlock"
        );
        // Safety: This is a valid aligned pointer to deference and `next` and `size` are valid struct members
        unsafe {
            let new = this.byte_add(offset);
            core::ptr::write(
                new,
                FreeBlock {
                    next: (*this).next,
                    size: (*this).size - offset,
                },
            );
            (*this).next = new;
            (*this).size = offset;
            (this, new)
        }
    }

    // Computes the end address of a free block for comparison to next block or dealloc address
    //
    // Note - associated function to take *mut, in order to maintain pointer provenance
    //
    // Safety: Caller must ensure that `this` is a pointer to a valid FreeBlock
    #[allow(dead_code)]
    unsafe fn end_addr(this: *mut FreeBlock) -> usize {
        // Safety: All free blocks are aligned and valid for reading the `size` struct member
        unsafe { this.addr() + (*this).size }
    }
}

// Safety: Free Block only references heap memory and holds no thread-local data, so Send
unsafe impl Send for FreeBlock {}

// Protect the list with the kernel's allocator lock. On the kernel target
// this is `IrqSpinLock` (disables interrupts in the critical section); on
// host builds it is a plain `SpinLock`.
pub struct FreeBlockList {
    pub(crate) sentinel: AllocatorLock<FreeBlock>,
}

impl FreeBlockList {
    /// Construct an uninitialised allocator. Call `init` before any
    /// `alloc`/`dealloc` operations.
    pub const fn new() -> Self {
        Self {
            sentinel: AllocatorLock::new(FreeBlock {
                next: core::ptr::null_mut(),
                size: 0,
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
    ///   - `start` is aligned to `align_of::<FreeBlock>()`.
    ///   - `size >= size_of::<FreeBlock>()`.
    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        // Initialise the sentinel (in the BSS segment) and the first free block covering the whole heap
        let mut sentinel_guard = self.sentinel.lock();
        // Only initialise once
        assert!(
            sentinel_guard.next.is_null(),
            "trying to initialise free list twice"
        );
        assert!(
            (start as usize).is_multiple_of(core::mem::align_of::<FreeBlock>()),
            "heap start must be FreeBlock-aligned"
        );
        assert!(
            size >= core::mem::size_of::<FreeBlock>(),
            "heap too small to hold a FreeBlock"
        );
        // First create the free block that covers the empty heap
        // Derive provenance from the heap via `start`
        let new = start as *mut FreeBlock;
        // Safety: Heap start is aligned and save for writes from linker script
        unsafe {
            core::ptr::write(
                new,
                FreeBlock {
                    next: core::ptr::null_mut(),
                    size,
                },
            )
        }
        // Now link this new block to the sentinel
        // Safety: new is non null as just created from heap pointer
        sentinel_guard.next = new;
        sentinel_guard.size = 0;
    }

    //   FREE_BLOCK_LIST.sentinel | alloc  | free_block |  alloc  | (de)alloc | alloc | free_block |
    //            .size: 0                 ^     .size                                ^    .size
    //            .next -------------------+     .next -------------------------------+    .next: None
    //            ^
    //            |                        ^                      ^
    //            |                        |                      |
    //            |                        |                      dealloc_ptr / new_free_block_ptr
    //            |                        |                      alloc ptr
    //            |                        current_free_block (copy)
    //            prev_free_block (copy)

    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // Zero-size requests get a dangling, non-null, aligned pointer
        // per the GlobalAlloc convention. No free block is consumed,
        // which keeps heap capacity intact for real allocations.
        // dealloc mirrors this with a no-op when layout.size() == 0.
        if layout.size() == 0 {
            return core::ptr::without_provenance_mut(layout.align());
        }
        let mut sentinel_guard = self.sentinel.lock();
        // Walk the free list looking for a valid slot to reuse; block at end is all remaining memory; otherwise OOM
        // Use pointers to keep heap provenance
        // At start the current needs to be the `.next` of the sentinel, while prev is the sentinel itself
        // let mut prev = &raw mut sentinel_guard;
        let mut prev = core::ptr::addr_of_mut!(*sentinel_guard);
        let mut current = unsafe { (*prev).next };
        // Work out the aligned size needed
        if let Some(alloc_size) = layout.size().checked_next_multiple_of(BASE_ALIGN) {
            while !current.is_null() {
                // Check if both alignment padding and minimum layout size would fit into this block
                let aligned_size_fits_padding = align_up(
                    current.addr(),
                    BASE_ALIGN.max(layout.align()),
                )
                .map(|aligned_addr| aligned_addr - current.addr())
                .filter(|&bottom_padding| unsafe { (*current).size } > bottom_padding + alloc_size);

                if let Some(mut bottom_padding_size) = aligned_size_fits_padding {
                    // It fits!
                    if bottom_padding_size >= core::mem::size_of::<FreeBlock>() {
                        // We can split off the padding as a new free block - waste not, want not
                        (prev, current) =
                            unsafe { FreeBlock::split_right(current, bottom_padding_size) };
                        bottom_padding_size = 0;
                    }

                    if unsafe { (*current).size } - alloc_size > core::mem::size_of::<FreeBlock>() {
                        // There is size at the end of the free block to spare
                        // Split that off as a new free block
                        (current, _) = unsafe { FreeBlock::split_right(current, alloc_size) };
                    }

                    // Now link previous to next, skipping over current which will be allocated
                    unsafe { (*prev).next = (*current).next };

                    #[cfg(test)]
                    let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    let _ = ALLOCATED_BYTES.fetch_add(alloc_size as u32, Ordering::Relaxed);
                    #[cfg(test)]
                    let _ = PADDING_BYTES.fetch_add(bottom_padding_size as u32, Ordering::Relaxed);
                    #[cfg(test)]
                    let _ = HEAP_TOP
                        .fetch_max(unsafe { FreeBlock::end_addr(current) }, Ordering::Relaxed);

                    // Return a pointer to the start of the allocated region
                    return unsafe { current.byte_add(bottom_padding_size) } as *mut u8;
                }
                // Block can't host this allocation (overflow or too small) — advance.
                // Move the cursor forward
                prev = current;
                current = unsafe { (*current).next };
            }
        }
        // We are out of free blocks and out of memory
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, dealloc_ptr: *mut u8, layout: core::alloc::Layout) {
        // Mirror alloc's zero-size path: the pointer is dangling (not in
        // the heap), so there's nothing to free and no free-list walk
        // to do. Callers that received a dangling pointer from alloc
        // must pass the same zero-size layout back here, per the
        // GlobalAlloc contract.
        if layout.size() == 0 {
            return;
        }
        debug_assert!(BASE_ALIGN.is_power_of_two());
        debug_assert!(
            dealloc_ptr.addr() & (BASE_ALIGN - 1) == 0,
            "dealloc: misaligned/foreign pointer"
        );
        let mut sentinel_guard = self.sentinel.lock();
        // Walk the free list looking for the address-related position to free;
        // Use pointers to keep heap provenance
        // At start the current needs to be the `.next` of the sentinel, while prev is the sentinel itself
        let mut prev = core::ptr::addr_of_mut!(*sentinel_guard);
        let mut current = unsafe { (*prev).next };
        // Deallocation size we're trying to fit
        let dealloc_size = align_up(layout.size(), BASE_ALIGN).unwrap(); // Size was created successfully by alloc

        while !current.is_null() {
            // Check if current free block address is after the dealloc address
            if current.addr() > dealloc_ptr.addr() {
                // We have found the right place to add the new free block
                // Sandwiched between two free blocks:  prev | dealloc | current
                // Check if we can merge with previous free block
                let can_merge_left = unsafe { FreeBlock::end_addr(prev) } == dealloc_ptr.addr();
                // Check if we can merge with the next free block
                let can_merge_right = dealloc_ptr.addr() + dealloc_size == current.addr();

                if can_merge_left {
                    if can_merge_right {
                        // We can extend the prev block across the dealloc region and the current block
                        // Safety: both current and prev have been identified as valid free blocks
                        unsafe {
                            (*prev).size += dealloc_size + (*current).size;
                            (*prev).next = (*current).next;
                        }
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    } else {
                        // We can extend the previous block over the dealloc region
                        // Safety: prev points to a valid free block
                        unsafe { (*prev).size += dealloc_size };
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    }
                } else {
                    if can_merge_right {
                        // Create a new block that extends over current
                        let new = dealloc_ptr as *mut FreeBlock;
                        // Safety: current is valid free block
                        unsafe {
                            let size = dealloc_size + (*current).size;
                            let next = (*current).next;
                            core::ptr::write(new, FreeBlock { next, size });
                            (*prev).next = new;
                        }
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    } else {
                        // Create a new block for the dealloc region and link it
                        let new = dealloc_ptr as *mut FreeBlock;
                        // Safety: current is valid free block
                        unsafe {
                            let size = dealloc_size;
                            let next_ptr = current;
                            core::ptr::write(
                                new,
                                FreeBlock {
                                    next: next_ptr,
                                    size,
                                },
                            );
                            (*prev).next = new;
                        }
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    }
                }
            } else {
                // Advance the cursor
                prev = current;
                // Safety: We know that next is not null and safe to derefence on a valid current block
                current = unsafe { (*current).next };
            }
        }
        // Reached the tail
        // Check if we can stretch previous block across the dealloc region
        // Safety: Prev_ptr points to a valid free block
        unsafe {
            if FreeBlock::end_addr(prev) == dealloc_ptr.addr() {
                // Stretch prev over the dealloc region
                (*prev).size += dealloc_size;
            } else {
                // Create a new block here and link it
                let new = dealloc_ptr as *mut FreeBlock;
                let size = dealloc_size;
                let next = core::ptr::null_mut();
                core::ptr::write(new, FreeBlock { next, size });
                (*prev).next = new;
            }
            #[cfg(test)]
            let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
        }
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

    fn make_allocator(heap: &TestHeap) -> FreeBlockList {
        let allocator = FreeBlockList::new();
        // SAFETY: The TestHeap region is exclusively owned by the
        // returned allocator for as long as the test holds a reference.
        unsafe { allocator.init(heap.ptr, heap.size) };
        allocator
    }

    #[test]
    fn basic_alloc_dealloc() {
        let heap = TestHeap::new(4096);
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
        // A real block should NOT be consumed — subsequent real
        // allocations must still succeed with the full heap available.
        let heap = TestHeap::new(4096);
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

        // Subsequent real allocation must succeed.
        let real_layout = Layout::from_size_align(64, 8).unwrap();
        let q = unsafe { a.alloc(real_layout) };
        assert!(!q.is_null(), "real alloc after zero-size alloc failed");
        unsafe { a.dealloc(q, real_layout) };
    }

    #[test]
    fn alloc_returns_unique_writable_memory() {
        // Each returned pointer must be a distinct, writable region.
        // We stamp each block with a recognisable pattern, then verify
        // the patterns are still intact at the end — proves no two
        // allocations overlap.
        let heap = TestHeap::new(4096);
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
        let heap = TestHeap::new(1024);
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
    fn coalesce_all_three_orders() {
        // Allocate three adjacent blocks, then free them in different
        // orders to exercise: free-only, merge-left, merge-right, merge-both.
        for free_order in &[[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]] {
            let heap = TestHeap::new(4096);
            let a = make_allocator(&heap);
            let layout = Layout::from_size_align(64, 8).unwrap();
            let p = [
                unsafe { a.alloc(layout) },
                unsafe { a.alloc(layout) },
                unsafe { a.alloc(layout) },
            ];
            assert!(p.iter().all(|p| !p.is_null()));
            for &i in free_order {
                unsafe { a.dealloc(p[i], layout) };
            }
            // Coalescing must have succeeded — a 2 KiB allocation must fit.
            let big = Layout::from_size_align(2048, 8).unwrap();
            let bp = unsafe { a.alloc(big) };
            assert!(
                !bp.is_null(),
                "coalesce failed for free order {:?}",
                free_order
            );
            unsafe { a.dealloc(bp, big) };
        }
    }

    #[test]
    fn high_alignment_padding() {
        let heap = TestHeap::new(8192);
        let a = make_allocator(&heap);

        // Bump the cursor with a small alloc so the next 4096-aligned
        // allocation needs significant padding.
        let small_layout = Layout::from_size_align(8, 8).unwrap();
        let small = unsafe { a.alloc(small_layout) };
        assert!(!small.is_null());

        let big_layout = Layout::from_size_align(64, 4096).unwrap();
        let big = unsafe { a.alloc(big_layout) };
        assert!(!big.is_null(), "high-alignment alloc failed");
        assert_eq!(big as usize % 4096, 0, "alignment not honoured");

        unsafe {
            a.dealloc(big, big_layout);
            a.dealloc(small, small_layout);
        }
        // After everything is freed, the padding block must have coalesced
        // back into the surrounding free space — a large allocation must fit.
        let full_layout = Layout::from_size_align(4096, 8).unwrap();
        let full = unsafe { a.alloc(full_layout) };
        assert!(
            !full.is_null(),
            "padding block did not coalesce with surrounding free space"
        );
        unsafe { a.dealloc(full, full_layout) };
    }

    #[test]
    fn fragmentation_pattern() {
        let heap = TestHeap::new(4096);
        let a = make_allocator(&heap);
        let layout = Layout::from_size_align(32, 8).unwrap();
        let mut ps = Vec::new();
        for _ in 0..16 {
            let p = unsafe { a.alloc(layout) };
            if !p.is_null() {
                ps.push(p);
            }
        }
        // Free even-indexed first (creates many small free blocks).
        for (i, &p) in ps.iter().enumerate() {
            if i % 2 == 0 {
                unsafe { a.dealloc(p, layout) };
            }
        }
        // Then free odd-indexed (each should merge with both neighbours).
        for (i, &p) in ps.iter().enumerate() {
            if i % 2 == 1 {
                unsafe { a.dealloc(p, layout) };
            }
        }
        // Heap should be fully consolidated; large alloc must fit.
        let big = Layout::from_size_align(1024, 8).unwrap();
        let bp = unsafe { a.alloc(big) };
        assert!(!bp.is_null(), "fragmented heap did not consolidate");
        unsafe { a.dealloc(bp, big) };
    }
}
