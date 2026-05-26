//! Allocators

// Each allocator module compiles unconditionally so its `host_tests`
// module runs under `cargo test --lib` and Miri. The kernel binary
// uses KAlloc (slab + buddy tier) as its sole `#[global_allocator]`.
// `bump` and `freelist` are retained for their host_tests as reference
// algorithms — they're not wired into the kernel.
mod buddy;
mod bump;
mod freelist;
mod kalloc;
mod slab;

#[cfg(target_os = "none")]
use core::alloc::{GlobalAlloc, Layout};

#[cfg(target_os = "none")]
mod bare_metal_alloc {
    use crate::kernel::alloc::buddy::{BuddyPd0, BuddyPsram};
    use crate::kernel::alloc::kalloc::KAlloc;

    // =============================================================================
    // Heap and Global Allocator
    // =============================================================================
    //
    // The `#[global_allocator]` static and the linker-symbol references
    // live inside this `#[cfg(target_os = "none")]` mod so they only
    // compile for the kernel target. The allocator algorithm types stay
    // host-testable in their own modules.

    // Safety: Symbols are created in the linker script with valid addresses
    // and are aligned per each region's ALIGN() in the linker script.
    unsafe extern "C" {
        static __heap_pd0_start: u8;
        static __heap_pd0_end: u8;
        static __heap_pd1_start: u8;
        static __heap_pd1_end: u8;
        static __heap_psram_start: u8;
        static __heap_psram_end: u8;
    }

    /// Returns the address of `__heap_pd1_start` for diagnostics (e.g. computing
    /// heap-used in benchmarks). Only referenced from the `#[cfg(test)]`
    /// allocator benchmarks.
    #[cfg(all(test, feature = "test-alloc", feature = "test-bench"))]
    pub(crate) fn heap_start_addr() -> usize {
        &raw const __heap_pd1_start as usize
    }

    #[global_allocator]
    pub(crate) static KALLOC_PD1: KAlloc = KAlloc::new();
    pub(crate) static BUDDY_PD0: BuddyPd0 = BuddyPd0::new();
    pub(crate) static BUDDY_PSRAM: BuddyPsram = BuddyPsram::new();

    /// Initialise the global allocator from the linker-defined heap region.
    /// Must be called exactly once during boot, before any allocations.
    pub fn init_global_allocator() {
        let start_pd0 = &raw const __heap_pd0_start as *mut u8;
        let size_pd0 = &raw const __heap_pd0_end as usize - start_pd0 as usize;
        let start_pd1 = &raw const __heap_pd1_start as *mut u8;
        let size_pd1 = &raw const __heap_pd1_end as usize - start_pd1 as usize;
        let start_psram = &raw const __heap_psram_start as *mut u8;
        let size_psram = &raw const __heap_psram_end as usize - start_psram as usize;
        unsafe {
            BUDDY_PD0.init(start_pd0, size_pd0);
            KALLOC_PD1.init(start_pd1, size_pd1);
            BUDDY_PSRAM.init(start_psram, size_psram);
        };
    }
}

/// Allocate to SRAM0-3 in Power Domain 0 with NAPOT - intended for user allocation
#[cfg(target_os = "none")]
#[allow(dead_code)]
pub fn kalloc_pd0_napot(order: u8) -> *mut u8 {
    let size = 1usize << order;
    let layout = Layout::from_size_align(size, size).expect("for NAPOT must align to own size");
    // Safety: BUDDY_PD0 init has been called
    unsafe { bare_metal_alloc::BUDDY_PD0.alloc(layout) }
}

/// Allocate to PSRAM with NAPOT - intended for user allocation
/// PSRAM's MIN_BLOCK_SIZE is 4096, so meaningful order starts at 12 — smaller orders get rounded up to 4 KB by the buddy.
#[cfg(target_os = "none")]
#[allow(dead_code)]
pub fn kalloc_psram_napot(order: u8) -> *mut u8 {
    let size = 1usize << order;
    let layout = Layout::from_size_align(size, size).expect("for NAPOT must align to own size");
    // Safety: BUDDY_PSRAM init has been called
    unsafe { bare_metal_alloc::BUDDY_PSRAM.alloc(layout) }
}

/// Allocate to PSRAM without NAPOT - intended for kernel alloc of arbitrary regions
#[cfg(target_os = "none")]
#[allow(dead_code)]
pub fn kalloc_psram(layout: Layout) -> *mut u8 {
    // Safety: BUDDY_PSRAM init has been called
    unsafe { bare_metal_alloc::BUDDY_PSRAM.alloc(layout) }
}

#[cfg(target_os = "none")]
#[cfg(all(test, feature = "test-alloc", feature = "test-bench"))]
pub(crate) use bare_metal_alloc::heap_start_addr;
#[cfg(target_os = "none")]
#[allow(unused_imports)] // main.rs consumes this but isn't part of the lib check
pub use bare_metal_alloc::init_global_allocator;

// QEMU benchmark suite for KAlloc. test-bench is opt-in (not part of
// test-all) so regression benches can be run separately from functional
// tests.
#[cfg(all(
    test,
    target_os = "none",
    feature = "test-alloc",
    feature = "test-bench",
))]
mod bench;

// `init_global_allocator()` is called from main.rs during early boot.

#[allow(dead_code)]
fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}
