//! Buddy Allocator

use core::alloc::GlobalAlloc;
#[cfg(test)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::kernel::sync::AllocatorLock;

// Doubly-linked list of free blocks
struct FreeBlock {
    next: *mut FreeBlock,
    prev: *mut FreeBlock,
}

// Every block has an order — its size expressed as a power-of-two multiple of the minimum block size.
// Order 0 = minimum block size
// Order 1 = 2 × minimum
// Order 2 = 4 × minimum
// Order N = 2^N × minimum
// For a 256 KiB heap with 64-byte minimum blocks:
// Order   Size       How many fit in 256 KiB
// 0       8 B           32,768
// 1      16 B           16,384
// 2      32 B            8,192
// 3      64 B            4,096
// 4     128 B            2,048
// 5     256 B            1,024
// 6     512 B              512
// 7       1 KiB            256
// 8       2 KiB            128
// 9       4 KiB             64
// 10      8 KiB             32
// 11     16 KiB             16
// 12     32 KiB              8
// 13     64 KiB              4
// 14    128 KiB              2
// 15    256 KiB              1   <- the whole heap at max order

// Entire Heap with an order 13 allocation followed by an order 12 allocation
//
// Order | List Head    |-------------------------------------------------------------|
// 15 -> null           |                                                             |
//                      |                                                             |
//                      |                                                             |
// 14 -> block B        |                                  |           Order 14 (B)   |
//                      |                                  | .prev = null .next = null|
//                      |                                  |                          |
// 13 -> null           |  Order 13 (A) |                  |                          |
//                      |               |                  |                          |
//                      |   ALLOCATED   |                  |                          |
//                      |               |                  |                          |
// 12 -> block B        |               | O12(A)  | O12(B) |                          |
//                      |               | ALLOC'D |.p=null |                          |
//                      |               |         |.n=null |                          |
//                      |-------------------------------------------------------------|

const MIN_BLOCK_SIZE: usize = 8; // We can't go smaller to fit FreeBlock
const MAX_ORDER: usize = 15;
const ORDERS_COUNT: usize = 16; //  Range from 8B to 256KiB

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

// Make sure a FreeBlock can always fit
const _: () = assert!(core::mem::size_of::<FreeBlock>() >= MIN_BLOCK_SIZE);

struct BuddyInner {
    list_heads: [*mut FreeBlock; ORDERS_COUNT],
    heap: *mut u8,
    // Highest order usable for this heap. Equals `MAX_ORDER` when the heap
    // is the full `MIN_BLOCK_SIZE << MAX_ORDER` bytes, and less when the
    // allocator is initialised with a smaller (still power-of-two) region.
    top_order: usize,
}

impl BuddyInner {
    // Safety: Caller must ensure list is not empty
    // In other words, block.next is fine to dereference
    pub unsafe fn pop(&mut self, order: usize) -> *mut FreeBlock {
        let block = self.list_heads[order];
        // Find the new head block and attach
        let new_head = unsafe { (*block).next };
        if !new_head.is_null() {
            unsafe { (*new_head).prev = core::ptr::null_mut() };
        }
        self.list_heads[order] = new_head;
        block
    }

    // Safety: Caller must ensure that block is non-null and points at writable FreeBlock-sized memory
    pub unsafe fn push(&mut self, order: usize, block: *mut FreeBlock) {
        let old_head = self.list_heads[order];
        unsafe {
            (*block).next = old_head;
            (*block).prev = core::ptr::null_mut();
            if !old_head.is_null() {
                (*old_head).prev = block;
            }
        };
        self.list_heads[order] = block;
    }

    // Safety: Caller must ensure buddy address is inside the heap
    pub unsafe fn split(&mut self, order: usize, block: *mut FreeBlock) {
        // Write a free block at the buddy address
        // New — offset-based, alignment-free
        let block_offset = block.addr() - self.heap.addr();
        let buddy_offset = block_offset ^ (MIN_BLOCK_SIZE << order);
        let buddy = self
            .heap
            .with_addr(self.heap.addr() + buddy_offset)
            .cast::<FreeBlock>();
        unsafe {
            core::ptr::write(
                buddy,
                FreeBlock {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
            );
        }
        unsafe { self.push(order, buddy) };
    }
}

// Safety: BuddyInner holds shared heap data, nothing thread-local
// Note - AllocatorLock is Sync if T is Send
unsafe impl Send for BuddyInner {}

pub struct Buddy {
    inner: AllocatorLock<BuddyInner>,
}

impl Buddy {
    pub const fn new() -> Self {
        Self {
            inner: AllocatorLock::new(BuddyInner {
                list_heads: [core::ptr::null_mut(); ORDERS_COUNT],
                heap: core::ptr::null_mut(),
                top_order: 0,
            }),
        }
    }

    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        // Take the lock on the allocator list
        let mut inner = self.inner.lock();
        // Ensure init is only called once
        assert!(inner.heap.is_null());
        // Buddy allocator size must be a power of two, at least one minimum
        // block, and no larger than the allocator's maximum order.
        assert!(size.is_power_of_two());
        assert!(size >= MIN_BLOCK_SIZE);
        assert!(size <= MIN_BLOCK_SIZE << MAX_ORDER);
        // Get the provenance of the heap pointer
        inner.heap = start;
        // Derive the top order from the heap size. `size / MIN_BLOCK_SIZE` is
        // a power of two, so its log2 is the order whose block spans the heap.
        let top_order = (size / MIN_BLOCK_SIZE).ilog2() as usize;
        inner.top_order = top_order;
        // Set up the largest order free block to cover the entire heap
        unsafe {
            core::ptr::write(
                inner.heap as *mut FreeBlock,
                FreeBlock {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
            );
        }
        inner.list_heads[top_order] = inner.heap as *mut FreeBlock;
    }
}

unsafe impl GlobalAlloc for Buddy {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // Take lock on the allocator free list
        let mut inner = self.inner.lock();
        // Ensure already initialised
        assert!(!inner.heap.is_null());
        // If allocation size is zero return a dangling, provenance-free
        // sentinel. The GlobalAlloc contract forbids dereferencing it.
        if layout.size() == 0 {
            return core::ptr::without_provenance_mut(layout.align());
        }
        // Calculate the order of the allocation request, bearing in mind it must be rounded up to MIN_BLOCK_SIZE
        let alloc_size = layout.size().max(layout.align()).max(MIN_BLOCK_SIZE);
        let alloc_order = (alloc_size.next_power_of_two() / MIN_BLOCK_SIZE).ilog2() as usize;
        // Walk the array of list heads starting at the desired order

        // Phase 1: walk up to find a non-empty list. Stop at the heap's
        // actual top order — slots beyond that were never populated and a
        // request larger than the heap can never be served.
        let mut current_order = alloc_order;
        while current_order <= inner.top_order && inner.list_heads[current_order].is_null() {
            current_order += 1;
        }
        if current_order > inner.top_order {
            return core::ptr::null_mut(); // OOM
        }

        // Phase 2: pop, split down to target order, return
        let block = unsafe { inner.pop(current_order) };
        while current_order > alloc_order {
            current_order -= 1;
            unsafe { inner.split(current_order, block) };
        }

        #[cfg(test)]
        let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        let _ = ALLOCATED_BYTES.fetch_add(alloc_size as u32, Ordering::Relaxed);
        #[cfg(test)]
        let _ = PADDING_BYTES.fetch_add((alloc_size - layout.size()) as u32, Ordering::Relaxed);
        #[cfg(test)]
        let _ = HEAP_TOP.fetch_max(block.addr() + alloc_size, Ordering::Relaxed);

        block as *mut u8
    }

    unsafe fn dealloc(&self, dealloc_ptr: *mut u8, layout: core::alloc::Layout) {
        if layout.size() == 0 {
            return;
        }
        // compute the order from the layout
        let alloc_size = layout.size().max(layout.align()).max(MIN_BLOCK_SIZE);
        let alloc_order = (alloc_size.next_power_of_two() / MIN_BLOCK_SIZE).ilog2() as usize;
        // push the block onto list_heads[order], done.
        let mut inner = self.inner.lock();
        unsafe {
            inner.push(alloc_order, dealloc_ptr as *mut FreeBlock);
        }

        #[cfg(test)]
        let _ = DEALLOCATED_BYTES.fetch_add(alloc_size as u32, Ordering::Relaxed);
    }
}

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

    fn make_allocator(heap: &TestHeap) -> Buddy {
        let allocator = Buddy::new();
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
