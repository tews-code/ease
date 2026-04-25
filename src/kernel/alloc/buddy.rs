//! Buddy Allocator

use core::alloc::GlobalAlloc;
#[cfg(test)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::kernel::collection::{Bitmap, bitmap_words_for};
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
// For a 256 KiB heap with 8-byte minimum blocks:
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
const BITS: usize = (1 << MAX_ORDER) - 1; // 32767
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
    heap_addr: usize,
    pair_bits: Bitmap<BITS, { bitmap_words_for(BITS) }>,
    // Highest order usable for this heap. Equals `MAX_ORDER` when the heap
    // is the full `MIN_BLOCK_SIZE << MAX_ORDER` bytes, and less when the
    // allocator is initialised with a smaller (still power-of-two) region.
    top_order: usize,
}

// Helper function to calculate the start bit for an order
const fn order_bit_offset(order: usize) -> usize {
    (1 << MAX_ORDER) - (1 << (MAX_ORDER - order))
}

// Helper function which takes the order and heap offset and returns the bitmap bit
pub const fn pair_bit(order: usize, offset: usize) -> usize {
    debug_assert!(order < MAX_ORDER);
    let block_index = offset >> (MIN_BLOCK_SIZE.ilog2() as usize + order);
    let pair_index = block_index >> 1;
    order_bit_offset(order) + pair_index
}

// Helper function which returns the heap offset of the buddy
pub const fn buddy_offset(order: usize, offset: usize) -> usize {
    offset ^ (MIN_BLOCK_SIZE << order)
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

        // Toggle the allocated pair bit
        if order < MAX_ORDER {
            let bit = pair_bit(order, block.addr() - self.heap_addr);
            self.pair_bits.toggle(bit);
        }

        block
    }

    // Pushes a free block into the free block list for that order
    //
    // Returns the bit status for that bit after status change
    //
    // Safety: Caller must ensure that block is non-null and points at writable FreeBlock-sized memory

    pub unsafe fn push(&mut self, order: usize, block: *mut FreeBlock) -> bool {
        let old_head = self.list_heads[order];
        unsafe {
            (*block).next = old_head;
            (*block).prev = core::ptr::null_mut();
            if !old_head.is_null() {
                (*old_head).prev = block;
            }
        };

        self.list_heads[order] = block;

        // Toggle the freed pair bit
        if order < MAX_ORDER {
            let bit = pair_bit(order, block.addr() - self.heap_addr);
            self.pair_bits.toggle(bit)
        } else {
            false
        }
    }

    // Safety: Caller must ensure buddy address is inside the heap
    pub unsafe fn split(&mut self, order: usize, block: *mut FreeBlock) {
        // Write a free block at the buddy address
        // New — offset-based, alignment-free
        let block_offset = block.addr() - self.heap_addr;
        let buddy_offset = block_offset ^ (MIN_BLOCK_SIZE << order);
        let buddy = block.with_addr(self.heap_addr + buddy_offset);
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

    // Safety: Caller must ensure block is currently in the freelist at order and no other references to the block or its neighbours exist.
    //
    // Does not toggle the bit flag: called only in coalesce context.
    // Pair ceases to exist at order O, bit stays at 0 from the prior push
    unsafe fn remove_from_list(&mut self, order: usize, block: *mut FreeBlock) {
        unsafe {
            let prev = (*block).prev;
            let next = (*block).next;
            if prev.is_null() {
                self.list_heads[order] = next;
            } else {
                (*prev).next = next;
            }
            if !next.is_null() {
                (*next).prev = prev;
            }
        }
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
                heap_addr: 0,
                pair_bits: Bitmap::<BITS, { bitmap_words_for(BITS) }>::new(),
                top_order: 0,
            }),
        }
    }

    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        // Take the lock on the allocator list
        let mut inner = self.inner.lock();
        // Ensure init is only called once
        assert!(inner.heap_addr == 0);
        // Buddy allocator size must be a power of two, at least one minimum
        // block, and no larger than the allocator's maximum order.
        assert!(size.is_power_of_two());
        assert!(size >= MIN_BLOCK_SIZE);
        assert!(size <= MIN_BLOCK_SIZE << MAX_ORDER);
        // Get the provenance of the heap pointer
        inner.heap_addr = start.addr();
        // Derive the top order from the heap size. `size / MIN_BLOCK_SIZE` is
        // a power of two, so its log2 is the order whose block spans the heap.
        let top_order = (size / MIN_BLOCK_SIZE).ilog2() as usize;
        inner.top_order = top_order;
        // Set up the largest order free block to cover the entire heap
        unsafe {
            core::ptr::write(
                start as *mut FreeBlock,
                FreeBlock {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
            );
        }
        let block = start as *mut FreeBlock;
        unsafe { inner.push(top_order, block) };
    }
}

unsafe impl GlobalAlloc for Buddy {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // Take lock on the allocator free list
        let mut inner = self.inner.lock();
        // Ensure already initialised
        assert!(inner.heap_addr != 0);
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

        let mut inner = self.inner.lock();
        let mut block = dealloc_ptr as *mut FreeBlock;
        let mut order = alloc_order;
        while !unsafe { inner.push(order, block) } {
            if order + 1 > MAX_ORDER {
                break;
            }
            let block_off = block.addr() - inner.heap_addr;
            let buddy_off = buddy_offset(order, block_off);
            // let buddy = inner
            //     .heap
            //     .with_addr(inner.heap.addr() + buddy_off)
            //     .cast::<FreeBlock>();
            let buddy = block.with_addr(inner.heap_addr + buddy_off);
            unsafe { inner.remove_from_list(order, buddy) };
            unsafe { inner.remove_from_list(order, block) };
            // Move up an order to see if we can coalesce again
            block = block.min(buddy);
            order += 1;
        }

        #[cfg(test)]
        let _ = DEALLOCATED_BYTES.fetch_add(alloc_size as u32, Ordering::Relaxed);
    }
}

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

    fn make_allocator(heap: &TestHeap) -> Buddy {
        let allocator = Buddy::new();
        // SAFETY: The TestHeap region is exclusively owned by the
        // returned allocator for as long as the test holds a reference.
        unsafe { allocator.init(heap.ptr, heap.size) };
        allocator
    }

    // ─────────────────────────────────────────────────────────────────
    // Pure-math helpers: pair_bit, order_bit_offset, buddy_offset.
    // No heap state; these just verify the arithmetic we'll depend on
    // when the allocator starts toggling pair bits.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn order_bit_offset_known_values() {
        // Geometric layout: order k starts at (2^MAX_ORDER - 2^(MAX_ORDER-k)).
        assert_eq!(order_bit_offset(0), 0);
        assert_eq!(order_bit_offset(1), 16384);
        assert_eq!(order_bit_offset(2), 24576);
        assert_eq!(order_bit_offset(14), 32766);
        // MAX_ORDER is one past the last valid order — equals BITS.
        // Documented here rather than encouraged via pair_bit.
        assert_eq!(order_bit_offset(MAX_ORDER), BITS);
    }

    #[test]
    fn pair_bit_concrete_values() {
        // Order 0 blocks are 8 B; consecutive pairs sit at offsets
        // [0, 8], [16, 24], [32, 40], ...
        assert_eq!(pair_bit(0, 0), 0);
        assert_eq!(pair_bit(0, 8), 0);
        assert_eq!(pair_bit(0, 16), 1);
        assert_eq!(pair_bit(0, 24), 1);
        assert_eq!(pair_bit(0, 32), 2);
        // Order 1 blocks are 16 B; order-1 bits start at 16384.
        assert_eq!(pair_bit(1, 0), 16384);
        assert_eq!(pair_bit(1, 16), 16384);
        assert_eq!(pair_bit(1, 32), 16385);
    }

    #[test]
    fn pair_bit_buddies_share_bit() {
        // The two buddies of a pair must map to the same bit.
        for order in 0..MAX_ORDER {
            let block_size = MIN_BLOCK_SIZE << order;
            for pair in 0..4 {
                let left = 2 * pair * block_size;
                let right = left + block_size;
                assert_eq!(
                    pair_bit(order, left),
                    pair_bit(order, right),
                    "order {} pair {}: buddies don't share a bit",
                    order,
                    pair
                );
            }
        }
    }

    #[test]
    fn pair_bit_adjacent_pairs_are_adjacent_bits() {
        // Adjacent pairs at the same order must map to adjacent bits.
        for order in 0..MAX_ORDER {
            let pair_size = MIN_BLOCK_SIZE << (order + 1);
            let b0 = pair_bit(order, 0);
            let b1 = pair_bit(order, pair_size);
            assert_eq!(
                b1,
                b0 + 1,
                "order {}: consecutive pairs not adjacent",
                order
            );
        }
    }

    #[test]
    fn pair_bit_order_ranges_do_not_overlap() {
        // The last pair bit at order k must lie strictly below the
        // starting bit of order k+1.
        for order in 0..(MAX_ORDER - 1) {
            let pair_size = MIN_BLOCK_SIZE << (order + 1);
            let heap_size = MIN_BLOCK_SIZE << MAX_ORDER;
            let last_pair_offset = heap_size - pair_size;
            let last_bit = pair_bit(order, last_pair_offset);
            let next_start = order_bit_offset(order + 1);
            assert!(
                last_bit < next_start,
                "order {} last bit {} overlaps with order {} start {}",
                order,
                last_bit,
                order + 1,
                next_start
            );
        }
    }

    #[test]
    fn buddy_offset_is_involution() {
        // Applying buddy_offset twice returns the original offset.
        for order in 0..MAX_ORDER {
            let block_size = MIN_BLOCK_SIZE << order;
            for n in 0..8 {
                let a = n * block_size;
                let b = buddy_offset(order, a);
                assert_eq!(
                    buddy_offset(order, b),
                    a,
                    "buddy_offset not involutive at order {} offset {}",
                    order,
                    a
                );
            }
        }
    }

    #[test]
    fn buddy_offset_differs_by_block_size() {
        // The buddy differs from its partner by exactly the block size
        // at that order (one bit flipped in the offset).
        for order in 0..MAX_ORDER {
            let block_size = MIN_BLOCK_SIZE << order;
            for n in 0..4 {
                let a = n * block_size;
                assert_eq!(buddy_offset(order, a) ^ a, block_size);
            }
        }
    }

    #[test]
    fn buddies_share_pair_bit_via_both_helpers() {
        // Cross-check: the offset returned by buddy_offset and the
        // original offset must hash to the same pair bit — binding the
        // two helpers together into one coherent scheme.
        for order in 0..MAX_ORDER {
            let block_size = MIN_BLOCK_SIZE << order;
            for n in 0..8 {
                let a = n * block_size;
                let b = buddy_offset(order, a);
                assert_eq!(
                    pair_bit(order, a),
                    pair_bit(order, b),
                    "order {} offset {}: buddies don't share pair bit",
                    order,
                    a
                );
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Allocator integration tests (use TestHeap).
    // ─────────────────────────────────────────────────────────────────

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
