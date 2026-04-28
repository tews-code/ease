//! Tier allocator using buddy and slab family

use core::alloc::GlobalAlloc;
use core::alloc::Layout;

use crate::kernel::alloc::buddy::Buddy;
use crate::kernel::alloc::slab::Slab;

const BASE_SIZE: usize = 4096;

pub struct KAlloc {
    pool_32: Slab<32>,
    pool_64: Slab<64>,
    pool_128: Slab<128>,
    pool_256: Slab<256>,
    buddy: Buddy,
}

impl KAlloc {
    pub const fn new() -> Self {
        Self {
            pool_32: Slab::new(),
            pool_64: Slab::new(),
            pool_128: Slab::new(),
            pool_256: Slab::new(),
            buddy: Buddy::new(),
        }
    }

    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        unsafe { self.buddy.init(start, size) };
    }
}

impl KAlloc {
    unsafe fn alloc_via<const N: usize>(&self, pool: &Slab<N>, layout: Layout) -> *mut u8 {
        let p = unsafe { pool.alloc(layout) };
        if !p.is_null() {
            return p;
        };
        // Need a new slab
        let region_layout = Layout::from_size_align(BASE_SIZE, BASE_SIZE).unwrap();
        let region = unsafe { self.buddy.alloc(region_layout) };
        if region.is_null() {
            return core::ptr::null_mut();
        }; // Buddy has no slabs to give
        unsafe {
            pool.add_slab(region, BASE_SIZE);
        };
        unsafe { pool.alloc(layout) }
    }

    /// Dealloc to a pool, then ask the pool whether the slab containing
    /// the freed slot is now empty. If so, hand its page back to buddy.
    unsafe fn dealloc_via<const N: usize>(&self, pool: &Slab<N>, ptr: *mut u8, layout: Layout) {
        unsafe { pool.dealloc(ptr, layout) };
        // The slab that owns this slot starts at the BASE_SIZE-aligned
        // address below ptr. Round down with the bitmask trick.
        let slab_base = ptr.with_addr(ptr.addr() & !(BASE_SIZE - 1));
        if let Some((page, size)) = unsafe { pool.reclaim_slab(slab_base) } {
            let page_layout = Layout::from_size_align(size, size).unwrap();
            unsafe { self.buddy.dealloc(page, page_layout) };
        }
    }
}

unsafe impl GlobalAlloc for KAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let required_size = layout.size().max(layout.align()); // 1 byte repr 64 needs 64 bytes
        match required_size {
            0..=32 => unsafe { self.alloc_via(&self.pool_32, layout) },
            33..=64 => unsafe { self.alloc_via(&self.pool_64, layout) },
            65..=128 => unsafe { self.alloc_via(&self.pool_128, layout) },
            129..=256 => unsafe { self.alloc_via(&self.pool_256, layout) },
            _ => unsafe { self.buddy.alloc(layout) },
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        if layout.size() == 0 {
            return;
        }
        let needed = layout.size().max(layout.align());
        match needed {
            0..=32 => unsafe { self.dealloc_via(&self.pool_32, ptr, layout) },
            33..=64 => unsafe { self.dealloc_via(&self.pool_64, ptr, layout) },
            65..=128 => unsafe { self.dealloc_via(&self.pool_128, ptr, layout) },
            129..=256 => unsafe { self.dealloc_via(&self.pool_256, ptr, layout) },
            _ => unsafe { self.buddy.dealloc(ptr, layout) },
        }
    }
}

// Host-runnable tests for KAlloc routing. The interesting question
// these answer is "did the size-class dispatch actually land in the
// right pool, or did everything silently fall through to buddy?"
//
// We answer it via address-range checks rather than the per-allocator
// counters, because the counters are global static AtomicU32s shared
// across all instances, and parallel test execution can interfere.
// Address ranges are robust: a slab-served allocation falls inside
// the 4 KiB-aligned page that buddy gave to the pool; a buddy-served
// allocation lives at its own buddy-aligned address.
#[cfg(all(test, not(target_os = "none"), feature = "test-alloc"))]
mod host_tests {
    use super::*;
    use core::alloc::Layout;

    // Buddy can handle up to 256 KiB, but a smaller test heap exercises
    // the same routing logic and keeps Miri quick. Power-of-two and
    // self-aligned (Layout::from_size_align with align == size).
    const TEST_HEAP_SIZE: usize = 64 * 1024;

    /// Owns a chunk of host memory aligned to TEST_HEAP_SIZE, satisfying
    /// the buddy and slab alignment preconditions.
    struct TestHeap {
        ptr: *mut u8,
        layout: Layout,
    }

    impl TestHeap {
        fn new() -> Self {
            let layout = Layout::from_size_align(TEST_HEAP_SIZE, TEST_HEAP_SIZE).unwrap();
            // SAFETY: layout is non-zero and a valid power-of-two alignment.
            let ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!ptr.is_null(), "host alloc failed");
            Self { ptr, layout }
        }
    }

    impl Drop for TestHeap {
        fn drop(&mut self) {
            // SAFETY: ptr/layout match the values returned by std::alloc::alloc
            // in `new`, and the region is no longer in use.
            unsafe { std::alloc::dealloc(self.ptr, self.layout) };
        }
    }

    fn make_kalloc(heap: &TestHeap) -> KAlloc {
        let k = KAlloc::new();
        // SAFETY: heap is owned by us for the test's duration and is
        // exclusively given to this allocator.
        unsafe { k.init(heap.ptr, TEST_HEAP_SIZE) };
        k
    }

    /// Round an address down to the slab-page boundary.
    fn page_of(ptr: *mut u8) -> usize {
        (ptr as usize) & !(BASE_SIZE - 1)
    }

    #[test]
    fn small_allocations_share_a_slab_page() {
        // A burst of 16-byte allocations should all be served by pool_16,
        // which means they share a single 4 KiB slab page (one page holds
        // 255 usable 16-byte slots, more than enough for 100 allocs).
        let heap = TestHeap::new();
        let k = make_kalloc(&heap);
        let layout = Layout::from_size_align(16, 8).unwrap();

        let mut ptrs = Vec::new();
        for _ in 0..100 {
            let p = unsafe { k.alloc(layout) };
            assert!(!p.is_null());
            ptrs.push(p);
        }

        let expected_page = page_of(ptrs[0]);
        for (i, &p) in ptrs.iter().enumerate() {
            assert_eq!(
                page_of(p),
                expected_page,
                "allocation {} (ptr {:p}) is in a different page than allocation 0 (ptr {:p}) — \
                 small allocs should all be in the same slab page",
                i,
                p,
                ptrs[0]
            );
        }

        for p in ptrs {
            unsafe { k.dealloc(p, layout) };
        }
    }

    #[test]
    fn large_allocation_is_outside_slab_pages() {
        // A 1024-byte allocation exceeds every slab class, so it must
        // come straight from buddy. Its page must be different from
        // any slab page populated for the small-class workload.
        let heap = TestHeap::new();
        let k = make_kalloc(&heap);

        // Touch each pool to force buddy to hand out one slab page per class.
        let small_layout = Layout::from_size_align(8, 8).unwrap();
        let small_p = unsafe { k.alloc(small_layout) };
        assert!(!small_p.is_null());
        let small_page = page_of(small_p);

        // Now a large alloc — must not share a page with the small one.
        let big_layout = Layout::from_size_align(1024, 8).unwrap();
        let big_p = unsafe { k.alloc(big_layout) };
        assert!(!big_p.is_null());
        assert_ne!(
            page_of(big_p),
            small_page,
            "large alloc landed in the same page as small alloc — routing is wrong"
        );

        unsafe {
            k.dealloc(big_p, big_layout);
            k.dealloc(small_p, small_layout);
        }
    }

    #[test]
    fn each_size_class_lands_in_a_distinct_pool_page() {
        // Allocating into each size class triggers a separate pool refill,
        // each from a different 4 KiB buddy block. So the four returned
        // pointers should land on four distinct slab pages.
        let heap = TestHeap::new();
        let k = make_kalloc(&heap);

        let layouts = [
            Layout::from_size_align(24, 8).unwrap(),  // pool_32
            Layout::from_size_align(48, 8).unwrap(),  // pool_64
            Layout::from_size_align(96, 8).unwrap(),  // pool_128
            Layout::from_size_align(200, 8).unwrap(), // pool_256
        ];

        let mut ptrs = [core::ptr::null_mut::<u8>(); 4];
        for (i, layout) in layouts.iter().enumerate() {
            ptrs[i] = unsafe { k.alloc(*layout) };
            assert!(!ptrs[i].is_null(), "alloc for size class {} failed", i);
        }

        let pages: [usize; 4] = core::array::from_fn(|i| page_of(ptrs[i]));
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert_ne!(
                    pages[i], pages[j],
                    "size class {} and {} share a page — pools should be in distinct buddy pages",
                    i, j
                );
            }
        }

        for (i, &p) in ptrs.iter().enumerate() {
            unsafe { k.dealloc(p, layouts[i]) };
        }
    }

    #[test]
    fn small_alloc_pointer_is_inside_buddy_heap() {
        // The slab page itself must come from buddy, so a small allocation's
        // pointer must lie within the test heap region. Catches the case
        // where dispatch silently returns null or a wild pointer.
        let heap = TestHeap::new();
        let k = make_kalloc(&heap);
        let layout = Layout::from_size_align(8, 8).unwrap();

        let p = unsafe { k.alloc(layout) };
        assert!(!p.is_null());

        let heap_start = heap.ptr as usize;
        let heap_end = heap_start + TEST_HEAP_SIZE;
        let addr = p as usize;
        assert!(
            addr >= heap_start && addr < heap_end,
            "allocation {:p} fell outside the test heap [{:p}, {:p})",
            p,
            heap.ptr,
            (heap_start + TEST_HEAP_SIZE) as *mut u8
        );

        unsafe { k.dealloc(p, layout) };
    }
}
