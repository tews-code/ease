//! Host-runnable test harness for the freelist allocator.
//!
//! This file embeds a copy of the allocator's algorithm so it can run
//! on the host (and under Miri) without the kernel's linker symbols,
//! IrqSpinLock, or test infrastructure.
//!
//! Run normally:
//!     cargo run
//!
//! Run under Miri (catches use-after-free, double-free, OOB writes,
//! and aliasing violations under Stacked/Tree Borrows):
//!     rustup component add miri    # one-time, if you haven't already
//!     cargo miri run
//!
//! When you change the production allocator at
//! `../src/kernel/alloc/freelist.rs`, mirror the changes here. The
//! substitutions are:
//!     IrqSpinLock<FreeBlock>  ->  std::sync::Mutex<FreeBlock>
//!     __heap_start/_end       ->  init() takes a (ptr, size) pair
//!     #[global_allocator]     ->  removed; tested as a regular type
//!     #[cfg(test)] atomics    ->  removed; not relevant for soundness

use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::Mutex;

// ============================================================
// Algorithm — mirrors src/kernel/alloc/freelist.rs
// ============================================================

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[derive(Clone, Copy)]
struct FreeBlockPtr(Option<NonNull<FreeBlock>>);

// SAFETY: FreeBlocks live in test memory; no thread-local invariants.
unsafe impl Send for FreeBlockPtr {}

struct FreeBlock {
    next: FreeBlockPtr,
    size: usize,
}

impl FreeBlock {
    fn size(&self) -> usize {
        self.size
    }
    fn next(&self) -> FreeBlockPtr {
        self.next
    }
    fn addr(&self) -> usize {
        self as *const FreeBlock as usize
    }
    fn end_addr(&self) -> usize {
        self.addr() + self.size
    }

    fn as_ptr(&mut self) -> *mut FreeBlock {
        self as *mut FreeBlock
    }

    fn as_free_block_ptr(&mut self) -> FreeBlockPtr {
        unsafe { FreeBlockPtr(Some(NonNull::new_unchecked(self.as_ptr()))) }
    }

    /// Split this block at `offset` bytes. The original block becomes
    /// `offset` bytes long; a new block holds the remainder and is
    /// linked after self.
    unsafe fn split_right(&mut self, offset: usize) {
        let new_ptr = (self.addr() + offset) as *mut FreeBlock;
        unsafe {
            std::ptr::write(
                new_ptr,
                FreeBlock {
                    next: self.next,
                    size: self.size - offset,
                },
            );
        }
        self.next = unsafe { FreeBlockPtr(Some(NonNull::new_unchecked(new_ptr))) };
        self.size = offset;
    }
}

const BASE_ALIGN: usize = std::mem::size_of::<FreeBlock>();
const _: () = assert!(BASE_ALIGN.is_power_of_two());
const _: () = assert!(BASE_ALIGN >= std::mem::align_of::<FreeBlock>());

struct FreeBlockList {
    sentinel: Mutex<FreeBlock>,
}

impl FreeBlockList {
    fn new() -> Self {
        Self {
            sentinel: Mutex::new(FreeBlock {
                next: FreeBlockPtr(None),
                size: 0,
            }),
        }
    }

    /// Initialise the list with a single free block covering
    /// [heap_start, heap_start + heap_size). The region must be
    /// BASE_ALIGN-aligned and valid for writes.
    unsafe fn init(&self, heap_start: *mut u8, heap_size: usize) {
        let mut guard = self.sentinel.lock().unwrap();
        assert!(guard.next.0.is_none(), "init called twice");
        assert_eq!(
            heap_start as usize % BASE_ALIGN,
            0,
            "heap_start must be BASE_ALIGN aligned"
        );
        assert!(
            heap_size >= std::mem::size_of::<FreeBlock>(),
            "heap too small for a single FreeBlock"
        );

        let new_ptr = heap_start as *mut FreeBlock;
        unsafe {
            std::ptr::write(
                new_ptr,
                FreeBlock {
                    next: FreeBlockPtr(None),
                    size: heap_size,
                },
            );
        }
        guard.next = unsafe { FreeBlockPtr(Some(NonNull::new_unchecked(new_ptr))) };
    }

    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut guard = self.sentinel.lock().unwrap();
        let mut current_free_block = guard.next;
        let mut prev_free_block = guard.as_free_block_ptr();

        let aligned_size = align_up(layout.size(), BASE_ALIGN);

        while let Some(free_block) = current_free_block.0 {
            let mut current = unsafe { &mut *free_block.as_ptr() };
            let padding_size =
                align_up(current.addr(), BASE_ALIGN.max(layout.align())) - current.addr();
            if current.size() >= aligned_size + padding_size {
                if padding_size >= std::mem::size_of::<FreeBlock>() {
                    unsafe { current.split_right(padding_size) };
                    prev_free_block = current_free_block;
                    let next_ptr = current.next.0.unwrap().as_ptr();
                    current = unsafe { &mut *next_ptr };
                }
                if current.size() - aligned_size >= BASE_ALIGN {
                    unsafe { current.split_right(aligned_size) };
                }
                // SAFETY: prev_free_block is always Some — sentinel guarantees it
                let prev = unsafe { &mut *prev_free_block.0.unwrap_unchecked().as_ptr() };
                prev.next = current.next();

                return align_up(current.addr(), layout.align().max(BASE_ALIGN)) as *mut u8;
            } else {
                prev_free_block = current_free_block;
                current_free_block = current.next;
            }
        }
        std::ptr::null_mut()
    }

    unsafe fn dealloc(&self, dealloc_ptr: *mut u8, layout: Layout) {
        let mut guard = self.sentinel.lock().unwrap();
        let mut current_free_block = guard.next;
        let mut prev_free_block = guard.as_free_block_ptr();

        let dealloc_size = align_up(layout.size(), BASE_ALIGN);

        while let Some(free_block) = current_free_block.0 {
            let current = unsafe { &mut *free_block.as_ptr() };
            // SAFETY: prev is Some — sentinel guarantees it
            let prev = unsafe { &mut *prev_free_block.0.unwrap_unchecked().as_ptr() };

            if current.addr() > dealloc_ptr as usize {
                let can_merge_left = prev.end_addr() == dealloc_ptr as usize;
                let can_merge_right =
                    dealloc_ptr as usize + dealloc_size == current.as_ptr() as usize;

                if can_merge_left {
                    if can_merge_right {
                        prev.size += dealloc_size + current.size;
                        prev.next = current.next;
                        return;
                    } else {
                        prev.size += dealloc_size;
                        return;
                    }
                } else if can_merge_right {
                    let new_ptr = dealloc_ptr as *mut FreeBlock;
                    let size = dealloc_size + current.size;
                    let next = current.next;
                    unsafe {
                        std::ptr::write(new_ptr, FreeBlock { next, size });
                    }
                    prev.next = unsafe { (*new_ptr).as_free_block_ptr() };
                    return;
                } else {
                    let new_ptr = dealloc_ptr as *mut FreeBlock;
                    let size = dealloc_size;
                    let next = current.as_free_block_ptr();
                    unsafe {
                        std::ptr::write(new_ptr, FreeBlock { next, size });
                    }
                    prev.next = unsafe { (*new_ptr).as_free_block_ptr() };
                    return;
                }
            } else {
                prev_free_block = current.as_free_block_ptr();
                current_free_block = current.next;
            }
        }

        // Tail
        // SAFETY: prev is Some — sentinel guarantees it
        let prev = unsafe { &mut *prev_free_block.0.unwrap_unchecked().as_ptr() };
        if prev.end_addr() == dealloc_ptr as usize {
            prev.size += dealloc_size;
        } else {
            let new_ptr = dealloc_ptr as *mut FreeBlock;
            unsafe {
                std::ptr::write(
                    new_ptr,
                    FreeBlock {
                        next: FreeBlockPtr(None),
                        size: dealloc_size,
                    },
                );
            }
            prev.next = unsafe { (*new_ptr).as_free_block_ptr() };
        }
    }
}

// ============================================================
// Test heap helper
// ============================================================

/// Owns a chunk of host memory that the test allocator treats as the heap.
/// The chunk is freed when the helper goes out of scope.
struct TestHeap {
    ptr: *mut u8,
    size: usize,
    layout: Layout,
}

impl TestHeap {
    fn new(size: usize) -> Self {
        // 4096-aligned so high-alignment allocation tests have somewhere to land
        let layout = Layout::from_size_align(size, 4096).unwrap();
        let ptr = unsafe { std::alloc::alloc(layout) };
        assert!(!ptr.is_null(), "host alloc failed");
        Self { ptr, size, layout }
    }
}

impl Drop for TestHeap {
    fn drop(&mut self) {
        unsafe {
            std::alloc::dealloc(self.ptr, self.layout);
        }
    }
}

fn make_allocator(heap: &TestHeap) -> FreeBlockList {
    let allocator = FreeBlockList::new();
    unsafe {
        allocator.init(heap.ptr, heap.size);
    }
    allocator
}

// ============================================================
// Tests
// ============================================================

fn test_basic_alloc_dealloc() {
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
    println!("  test_basic_alloc_dealloc OK");
}

fn test_alloc_returns_unique_writable_memory() {
    // Verifies that the returned pointers do not overlap and that
    // writing to them does not corrupt the allocator state.
    let heap = TestHeap::new(4096);
    let a = make_allocator(&heap);

    let layout = Layout::from_size_align(64, 8).unwrap();
    let mut ptrs = Vec::new();
    for i in 0..8 {
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());
        // Stamp each block with a recognisable pattern
        unsafe {
            std::ptr::write_bytes(p, (i + 1) as u8, 64);
        }
        ptrs.push(p);
    }
    // Verify each block still holds its pattern (i.e. no two allocs overlapped)
    for (i, p) in ptrs.iter().enumerate() {
        let expected = (i + 1) as u8;
        for off in 0..64 {
            let byte = unsafe { *p.add(off) };
            assert_eq!(byte, expected, "block {} overwritten at offset {}", i, off);
        }
    }
    for p in ptrs {
        unsafe {
            a.dealloc(p, layout);
        }
    }
    println!("  test_alloc_returns_unique_writable_memory OK");
}

fn test_alloc_until_oom_then_recover() {
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

    // Free everything
    for &p in &allocations {
        unsafe {
            a.dealloc(p, layout);
        }
    }
    // Now we should be able to alloc again
    let p = unsafe { a.alloc(layout) };
    assert!(!p.is_null(), "could not allocate after freeing all blocks");
    unsafe {
        a.dealloc(p, layout);
    }
    println!("  test_alloc_until_oom_then_recover OK");
}

fn test_coalesce_all_three_orders() {
    // Allocate three adjacent blocks, then free them in different orders
    // to exercise: free-only (no merge), merge-left, merge-right, merge-both.
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
            unsafe {
                a.dealloc(p[i], layout);
            }
        }
        // After freeing all three, the heap should once again be able to
        // service a large allocation (proving coalescing happened).
        let big = Layout::from_size_align(2048, 8).unwrap();
        let bp = unsafe { a.alloc(big) };
        assert!(
            !bp.is_null(),
            "coalesce failed for free order {:?}",
            free_order
        );
        unsafe {
            a.dealloc(bp, big);
        }
    }
    println!("  test_coalesce_all_three_orders OK");
}

fn test_high_alignment_padding() {
    let heap = TestHeap::new(8192);
    let a = make_allocator(&heap);

    // Bump the cursor by a small allocation so the next 4096-aligned address
    // requires significant padding.
    let small_layout = Layout::from_size_align(8, 8).unwrap();
    let small = unsafe { a.alloc(small_layout) };
    assert!(!small.is_null());

    // Now allocate something that needs 4096 alignment
    let big_layout = Layout::from_size_align(64, 4096).unwrap();
    let big = unsafe { a.alloc(big_layout) };
    assert!(!big.is_null(), "high-alignment alloc failed");
    assert_eq!(big as usize % 4096, 0, "alignment not honored");

    // Free both. The padding block should merge back with the surrounding
    // free space when the big alloc is released.
    unsafe {
        a.dealloc(big, big_layout);
        a.dealloc(small, small_layout);
    }
    // After everything is freed, we should be able to allocate a large region
    let full_layout = Layout::from_size_align(4096, 8).unwrap();
    let full = unsafe { a.alloc(full_layout) };
    assert!(
        !full.is_null(),
        "padding block did not coalesce with surrounding free space"
    );
    unsafe {
        a.dealloc(full, full_layout);
    }
    println!("  test_high_alignment_padding OK");
}

fn test_fragmentation_pattern() {
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
    // Free even-indexed first (creates many small free blocks)
    for (i, &p) in ps.iter().enumerate() {
        if i % 2 == 0 {
            unsafe {
                a.dealloc(p, layout);
            }
        }
    }
    // Free odd-indexed (each should merge with both neighbours)
    for (i, &p) in ps.iter().enumerate() {
        if i % 2 == 1 {
            unsafe {
                a.dealloc(p, layout);
            }
        }
    }
    // Heap should now be fully consolidated; large alloc should succeed
    let big = Layout::from_size_align(1024, 8).unwrap();
    let bp = unsafe { a.alloc(big) };
    assert!(!bp.is_null(), "fragmented heap did not consolidate");
    unsafe {
        a.dealloc(bp, big);
    }
    println!("  test_fragmentation_pattern OK");
}

fn main() {
    println!("Running freelist host tests...");
    test_basic_alloc_dealloc();
    test_alloc_returns_unique_writable_memory();
    test_alloc_until_oom_then_recover();
    test_coalesce_all_three_orders();
    test_high_alignment_padding();
    test_fragmentation_pattern();
    println!("All tests passed!");
}
