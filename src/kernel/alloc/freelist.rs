// Free list allocator

use core::{alloc::GlobalAlloc, ptr::NonNull};

#[cfg(test)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::kernel::alloc::align_up;
use crate::kernel::sync::IrqSpinLock;

// Choose a base multiple for alignments to prevent padding mem leaks on 32-bit system
const BASE_ALIGN: usize = 8;

// BASE_ALIGN is chosen to have enough space for a free block
const _: () = assert!(BASE_ALIGN >= core::mem::size_of::<FreeBlock>());
// BASE_ALIGN is chosen to have the same alignment as a free block
const _: () = assert!(BASE_ALIGN >= core::mem::align_of::<FreeBlock>());
// BASE_ALIGN is must be power of two
const _: () = assert!(BASE_ALIGN.is_power_of_two());

#[cfg(test)]
static ALLOC_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static ALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static DEALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static PADDING_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static HEAP_TOP: AtomicUsize = AtomicUsize::new(0);

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
        let new_ptr = (self.addr() + offset) as *mut FreeBlock;
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

// Protect the list with a spinlock that disables interrupts to serialise access
pub struct FreeBlockList {
    pub(crate) sentinel: IrqSpinLock<FreeBlock>,
}

impl FreeBlockList {
    /// Construct an uninitialised allocator. Call `init` before any
    /// `alloc`/`dealloc` operations.
    pub const fn new() -> Self {
        Self {
            sentinel: IrqSpinLock::new(FreeBlock {
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
            let padding_size =
                align_up(current.addr(), BASE_ALIGN.max(layout.align())) - current.addr();
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
                let _ = HEAP_TOP.fetch_max(
                    current.addr() + padding_size + aligned_size,
                    Ordering::Relaxed,
                );

                // Return the allocation pointer
                return align_up(current.addr(), layout.align().max(BASE_ALIGN)) as *mut u8;
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

#[cfg(all(test, feature = "test-alloc"))]
pub mod test {
    use alloc::boxed::Box;
    use alloc::string::ToString;
    use alloc::vec::Vec;
    use core::alloc::Layout;
    use core::hint::black_box;

    use super::*;

    use crate::println;

    mod baseline {
        pub(super) const ONE_BYTE_ALLOC: u64 = 1_000;
        pub(super) const ONE_BYTE_ALLOC_ITERS: u32 = 100_000;
        pub(super) const AWKWARD_ALLOC: u64 = 50_000;
        pub(super) const AWKWARD_ALLOC_ITERS: u32 = 20;
        pub(super) const CORE_SYNC_BASE: u64 = 300;
        pub(super) const CORE_SYNC_ITERS: u32 = 200_000;
    }

    const TOLERANCE_PERC: u64 = 50;

    fn allocate_one_byte() {
        let layout = Layout::new::<u8>();
        let alloc_ptr = black_box(unsafe { crate::FREE_BLOCK_LIST.alloc(layout) });
        let _ = black_box(unsafe { crate::FREE_BLOCK_LIST.dealloc(alloc_ptr, layout) });
    }

    #[repr(C, align(4096))]
    struct BigAlloc {
        val: bool,
    }
    fn allocate_deallocate_awkward() {
        let _a = black_box(Box::new([0x5u128; 13]));
        let _b = black_box(Box::new([0x3u16; 1_001]));
        let _c = black_box(Box::new(true));
        let _d = black_box(Box::new([0x7u64; 513]));
        let _e = black_box(Box::new(false));
        let _f = black_box(Box::new([0x9u32; 257]));
        let _g = black_box(Box::new([0xbu8; 2_017]));
        let _h = black_box(Box::new(BigAlloc { val: true }));
    }

    fn bare_sync_timing() {
        let _ = black_box(crate::FREE_BLOCK_LIST.sentinel.lock());
    }

    // Simulate allocations typical of shell activity
    fn allocator_benchmark() {
        let s = "This is a string that contains characters that fill most of the line";
        for _ in 0..11 {
            let _b = Box::new([1u8; 1024]);
            for _ in 0..5 {
                let _s = s.to_string();
                let mut v = Vec::new();
                for n in 0..512 {
                    v.push(n);
                }
                for _ in 0..512 {
                    let _ = v.pop();
                }
            }
        }
    }

    #[allow(dead_code)]
    fn allocate_or_bust() {
        for i in 0..1_000_000 {
            let _b = black_box(Box::new([1u8; 1024]));
            if i % 50 == 0 {
                println!("  allocate_or_bust: {} allocations", i);
            }
        }
    }

    #[test_case]
    fn alloc_benchmarks() {
        println!();
        println!("====== ALLOCATOR ====== ");
        println!();
        // check_regression includes warm up call which will also initialise if needs be; no need for additional initialisation
        crate::bench::check_regression(
            "alloc_one_byte",
            baseline::ONE_BYTE_ALLOC,
            TOLERANCE_PERC,
            baseline::ONE_BYTE_ALLOC_ITERS,
            || allocate_one_byte(),
        );

        println!();

        crate::bench::check_regression(
            "alloc_awkward",
            baseline::AWKWARD_ALLOC,
            TOLERANCE_PERC,
            baseline::AWKWARD_ALLOC_ITERS,
            || allocate_deallocate_awkward(),
        );

        println!();
        println!("  Total padding: {}", PADDING_BYTES.load(Ordering::Relaxed));

        crate::bench::check_regression(
            "core_sync_timing",
            baseline::CORE_SYNC_BASE,
            TOLERANCE_PERC,
            baseline::CORE_SYNC_ITERS,
            || bare_sync_timing(),
        );

        println!();
        allocator_benchmark();
        println!();
        println!(
            "  Allocation count: {}",
            ALLOC_COUNT.load(Ordering::Relaxed)
        );
        println!(
            "  Allocated: {} bytes",
            ALLOCATED_BYTES.load(Ordering::Relaxed) - DEALLOCATED_BYTES.load(Ordering::Relaxed)
        );
        let heap_used = HEAP_TOP.load(Ordering::Relaxed) - crate::heap_start_addr();
        println!("  Heap used: {} bytes", heap_used);

        // Comment out to continue CI
        //allocate_or_bust();

        println!();
        println!("===================== ");
        println!();
    }
}
