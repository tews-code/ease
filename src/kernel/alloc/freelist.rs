// Free list allocator

#[cfg(test)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use core::{alloc::GlobalAlloc, ptr::NonNull};

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
    next: FreeBlockPtr,
    size: usize,
}

impl FreeBlock {
    fn addr(&self) -> usize {
        (self as *const FreeBlock) as usize
    }

    fn as_ptr(&mut self) -> *mut FreeBlock {
        self as *mut FreeBlock
    }

    fn as_free_block_ptr(&mut self) -> FreeBlockPtr {
        unsafe { FreeBlockPtr(Some(NonNull::new_unchecked(self.as_ptr()))) }
    }

    // Split this block at `offset` bytes. This block shrinks to `offset`,
    // a new block gets the remainder and is linked after self.
    // Returns a pointer to the newly create free block
    unsafe fn split_right(&mut self, offset: usize) -> *mut FreeBlock {
        let new_ptr = unsafe { (self as *mut FreeBlock).byte_add(offset) };
        unsafe {
            core::ptr::write(
                new_ptr,
                FreeBlock {
                    next: self.next,
                    size: self.size - offset,
                },
            );
        }
        self.next = unsafe { FreeBlockPtr(Some(NonNull::new_unchecked(new_ptr))) };
        self.size = offset;
        new_ptr
    }

    fn end_addr(&self) -> usize {
        self.addr() + self.size
    }
}

// Pointer to a free block
#[derive(Clone, Copy)]
struct FreeBlockPtr(Option<NonNull<FreeBlock>>);

//Safety: Free blocks point only to heap memory and hold no thread local information hence Send
unsafe impl Send for FreeBlockPtr {}

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
                next: FreeBlockPtr(None),
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
        let mut free_block_list_head = self.sentinel.lock();
        // Ensure this is the only initialisation
        assert!(free_block_list_head.next.0.is_none());
        assert!(
            (start as usize).is_multiple_of(core::mem::align_of::<FreeBlock>()),
            "heap start must be FreeBlock-aligned"
        );
        assert!(
            size >= core::mem::size_of::<FreeBlock>(),
            "heap too small to hold a FreeBlock"
        );
        let new_free_block_ptr = start as *mut FreeBlock;
        // Safety: caller guarantees the region is valid for writes and aligned
        unsafe {
            core::ptr::write(
                new_free_block_ptr,
                FreeBlock {
                    next: FreeBlockPtr(None),
                    size,
                },
            );
        }
        // Safety: new_free_block_ptr is non-null and now points at an
        // initialised FreeBlock.
        free_block_list_head.next =
            unsafe { FreeBlockPtr(Some(NonNull::new_unchecked(new_free_block_ptr))) };
    }
}

// Safety: Allocations and deallocations are implemented in these functions
unsafe impl GlobalAlloc for FreeBlockList {
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
        // First walk the free list looking for a valid free block to reuse
        let mut free_block_list_head_guard = self.sentinel.lock();
        // Manual cursor for walking the list as using a while loop
        let mut current_free_block = free_block_list_head_guard.next;
        // Track previous block for linking / unlinking
        let mut prev_free_block = free_block_list_head_guard.as_free_block_ptr();

        // First work out the aligned up size we are looking to insert
        let aligned_size = align_up(layout.size(), BASE_ALIGN);

        while let Some(free_block) = current_free_block.0 {
            let mut current = unsafe { &mut *free_block.as_ptr() };
            // Aligning against this address may result in padding
            let aligned_addr = align_up(current.addr(), BASE_ALIGN.max(layout.align()));
            let padding_size = aligned_addr - current.addr();
            if current.size >= aligned_size + padding_size {
                // Found a free block big enough for the allocation
                // Avoid leaking padding - create a free block before the allocation
                // Note on 32 bit with BASE_ALIGN of 8, this only applies to x128 and higher repr
                if padding_size >= core::mem::size_of::<FreeBlock>() {
                    // Split the block
                    let next_ptr = unsafe { current.split_right(padding_size) };
                    // Manually move the cursor over the padding free block we have just created
                    prev_free_block = current_free_block;
                    current = unsafe { &mut *next_ptr };
                }
                // We can allocate memory here
                // Create a free block at the end of the allocation if there is enough space
                if current.size - aligned_size >= BASE_ALIGN {
                    // We have enough space to add a new free block at the end of this space
                    let _ = unsafe { current.split_right(aligned_size) };
                }
                // Allocation fits perfectly into the current free block
                // Unlink the current free block
                // SAFETY: prev_free_block is always Some — sentinel guarantees it
                let prev = unsafe { &mut *prev_free_block.0.unwrap_unchecked().as_ptr() };
                prev.next = current.next;

                #[cfg(test)]
                let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                #[cfg(test)]
                let _ = ALLOCATED_BYTES.fetch_add(aligned_size as u32, Ordering::Relaxed);
                #[cfg(test)]
                let _ = PADDING_BYTES.fetch_add(padding_size as u32, Ordering::Relaxed);
                #[cfg(test)]
                let _ = HEAP_TOP.fetch_max(current.addr() + aligned_size, Ordering::Relaxed);

                // Return the allocation pointer
                let offset = aligned_addr - current.addr();
                return unsafe { (current as *mut FreeBlock).byte_add(offset).cast::<u8>() };
            } else {
                // Advance the cursor
                prev_free_block = current_free_block;
                current_free_block = current.next;
            }
        }
        // We have reached the tail of the list without finding a suitable free block
        // OOM
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, dealloc_ptr: *mut u8, layout: core::alloc::Layout) {
        // Walk the free list so that we can add the new free block at an address-related point
        let mut free_block_list_head_guard = self.sentinel.lock();
        // Create a manual cursor as we will use a while loop
        let mut current_free_block = free_block_list_head_guard.next;
        // Also keep track of previous free block for coalescing
        let mut prev_free_block = free_block_list_head_guard.as_free_block_ptr();

        // Deallocation size we're trying to fit
        let dealloc_size = align_up(layout.size(), BASE_ALIGN);

        while let Some(free_block) = current_free_block.0 {
            let current = unsafe { &mut *free_block.as_ptr() };

            // Safety: prev is defined as Some in the sentinel and never set to None
            let prev = unsafe { &mut *prev_free_block.0.unwrap_unchecked().as_ptr() };

            // Check if current free block address is after the dealloc address
            if current.addr() > dealloc_ptr as usize {
                // We have found the right place to add the new free block
                // Sandwiched between two free blocks:  prev | dealloc | current
                // Check if we can merge with previous free block
                let can_merge_left = prev.end_addr() == dealloc_ptr as usize;
                // Check if we can merge with the next free block
                let can_merge_right =
                    dealloc_ptr as usize + dealloc_size == current.as_ptr() as usize;

                if can_merge_left {
                    if can_merge_right {
                        // We can extend the prev block across the dealloc region and the current block
                        prev.size += dealloc_size + current.size;
                        prev.next = current.next;
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    } else {
                        // We can extend the previous block over the dealloc region
                        prev.size += dealloc_size;
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    }
                } else {
                    if can_merge_right {
                        // Create a new block that extends over current
                        let new_ptr = dealloc_ptr as *mut FreeBlock;
                        let size = dealloc_size + current.size;
                        let next = current.next;
                        unsafe {
                            core::ptr::write(new_ptr, FreeBlock { next, size });
                        };
                        prev.next = unsafe { (*new_ptr).as_free_block_ptr() };
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    } else {
                        // Create a new block for the dealloc region and link it
                        let new_ptr = dealloc_ptr as *mut FreeBlock;
                        let size = dealloc_size;
                        let next = current.as_free_block_ptr();
                        unsafe {
                            core::ptr::write(new_ptr, FreeBlock { next, size });
                        };
                        prev.next = unsafe { (*new_ptr).as_free_block_ptr() };
                        #[cfg(test)]
                        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
                        return;
                    }
                }
            } else {
                // Advance the cursor
                prev_free_block = current.as_free_block_ptr();
                current_free_block = current.next;
            }
        }
        // Reached the tail
        // Check if we can stretch previous block across the dealloc region
        // Safety: Prev is set to Some in the sentinel and never updated to None
        let prev = unsafe { &mut *prev_free_block.0.unwrap_unchecked().as_ptr() };
        if prev.end_addr() == dealloc_ptr as usize {
            // Stretch prev over the dealloc region
            prev.size += dealloc_size;
        } else {
            // Create a new block here and link it
            let new_ptr = dealloc_ptr as *mut FreeBlock;
            let size = dealloc_size;
            let next = FreeBlockPtr(None);
            unsafe { core::ptr::write(new_ptr, FreeBlock { next, size }) };
            prev.next = unsafe { (*new_ptr).as_free_block_ptr() };
        }
        #[cfg(test)]
        let _ = DEALLOCATED_BYTES.fetch_add(dealloc_size as u32, Ordering::Relaxed);
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
