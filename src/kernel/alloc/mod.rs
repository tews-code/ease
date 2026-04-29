//! Allocators

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

// Each allocator compiles unconditionally in host builds so its
// `host_tests` module runs under every `cargo test --lib` and Miri
// invocation regardless of which allocator is active in the kernel
// binary. In kernel binary builds each is gated behind its own feature,
// so only one provides the `#[global_allocator]` static in main.rs.
mod buddy;
mod bump;
mod freelist;
mod kalloc;
mod slab;

#[cfg(target_os = "none")]
mod bare_metal_alloc {
    // =============================================================================
    // Heap and Global Allocator
    // =============================================================================
    //
    // The global allocator instance lives here in the binary crate (not in
    // kernel/alloc/freelist.rs) so that the `#[global_allocator]` attribute and
    // the linker-symbol references stay confined to code that is only ever
    // compiled for the kernel target. This keeps the FreeBlockList type itself
    // host-testable without polluting the lib crate.

    // Safety: Symbols are created in the linker script with valid addresses
    // and are FreeBlock-aligned (16-byte ALIGN in the .heap section).
    unsafe extern "C" {
        static __heap_start: u8;
        static __heap_end: u8;
    }

    /// Returns the address of `__heap_start` for diagnostics (e.g. computing
    /// heap-used in benchmarks). Only referenced from the `#[cfg(test)]`
    /// allocator benchmarks.
    #[cfg(all(test, feature = "test-alloc"))]
    pub(crate) fn heap_start_addr() -> usize {
        &raw const __heap_start as usize
    }

    #[cfg(feature = "alloc-buddy")]
    use crate::kernel::alloc::buddy::Buddy;
    #[cfg(feature = "alloc-bump")]
    use crate::kernel::alloc::bump::Bump;
    #[cfg(feature = "alloc-freelist")]
    use crate::kernel::alloc::freelist::FreeBlockList;
    #[cfg(feature = "alloc-kalloc")]
    use crate::kernel::alloc::kalloc::KAlloc;
    #[cfg(feature = "alloc-slab")]
    use crate::kernel::alloc::slab::Slab;

    #[global_allocator]
    #[cfg(feature = "alloc-freelist")]
    pub(crate) static FREE_BLOCK_LIST: FreeBlockList = FreeBlockList::new();

    #[global_allocator]
    #[cfg(feature = "alloc-slab")]
    pub(crate) static SLAB64: Slab</*SLOT_SIZE*/ 64> = Slab::new();

    #[global_allocator]
    #[cfg(feature = "alloc-bump")]
    pub(crate) static BUMP: Bump = Bump::new();

    #[global_allocator]
    #[cfg(feature = "alloc-buddy")]
    pub(crate) static BUDDY: Buddy = Buddy::new();

    #[global_allocator]
    #[cfg(feature = "alloc-kalloc")]
    pub(crate) static KALLOC: KAlloc = KAlloc::new();

    /// Initialise the global allocator from the linker-defined heap region.
    /// Must be called exactly once during boot, before any allocations.
    pub fn init_global_allocator() {
        let start = &raw const __heap_start as *mut u8;
        let size = &raw const __heap_end as usize - start as usize;
        // Safety: The heap region is defined by the linker, exclusively owned
        // by the allocator, FreeBlock-aligned, and large enough to hold a
        // FreeBlock header.
        #[cfg(feature = "alloc-freelist")]
        unsafe {
            FREE_BLOCK_LIST.init(start, size)
        };
        #[cfg(feature = "alloc-slab")]
        unsafe {
            // Slab requires each slab region to be aligned to its own size,
            // so split the heap into page-sized slabs (the heap is page-
            // aligned by the linker script). Calling add_slab repeatedly
            // grows the pool by one slab per call.
            const SLAB_SIZE: usize = 4096;
            let mut offset = 0;
            while offset + SLAB_SIZE <= size {
                SLAB64.add_slab(start.add(offset), SLAB_SIZE);
                offset += SLAB_SIZE;
            }
        };
        #[cfg(feature = "alloc-bump")]
        unsafe {
            BUMP.init(start, size)
        };
        #[cfg(feature = "alloc-buddy")]
        unsafe {
            BUDDY.init(start, size)
        };
        #[cfg(feature = "alloc-kalloc")]
        unsafe {
            KALLOC.init(start, size)
        };
    }
}

#[cfg(target_os = "none")]
#[cfg(all(test, feature = "test-alloc"))]
pub(crate) use bare_metal_alloc::heap_start_addr;
#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use bare_metal_alloc::init_global_allocator;

// Allocator-agnostic QEMU benchmark suite. Compiles only when one of the
// global allocators is active and the test-alloc feature is on.
#[cfg(all(
    test,
    target_os = "none",
    feature = "test-alloc",
    any(
        feature = "alloc-slab",
        feature = "alloc-freelist",
        feature = "alloc-bump",
        feature = "alloc-buddy",
        feature = "alloc-kalloc"
    )
))]
mod bench;

// Note: each allocator (freelist, slab, bump) is initialised from
// main.rs via `init_global_allocator()`, since the global static and
// linker symbols live in the binary crate.

#[allow(dead_code)]
fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}
