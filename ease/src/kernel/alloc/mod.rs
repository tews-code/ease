//! Allocators

// Each allocator module compiles unconditionally so its `host_tests`
// module runs under `cargo test --lib` and Miri. The kernel binary
// uses KAlloc (slab + buddy tier) as its sole `#[global_allocator]`.
// `bump` and `freelist` are retained for their host_tests as reference
// algorithms — they're not wired into the kernel.

#[cfg(target_os = "none")]
use core::alloc::GlobalAlloc;
use core::alloc::Layout;
use core::ptr::NonNull;

mod buddy;
#[cfg(all(test, not(target_os = "none")))]
mod bump;
#[cfg(all(test, not(target_os = "none")))]
mod freelist;
mod kalloc;
mod region;
mod slab;

use crate::kernel::alloc::buddy::{BuddyPd0, BuddyPsram};
use crate::kernel::alloc::kalloc::KAlloc;
// Needs to be used by bin compile but not lib compile
#[allow(unused_imports)]
pub(crate) use region::{MemRegion, Order};

#[cfg_attr(target_os = "none", global_allocator)]
static KALLOC_PD1: KAlloc = KAlloc::new();
static BUDDY_PD0: BuddyPd0 = BuddyPd0::new();
static BUDDY_PSRAM: BuddyPsram = BuddyPsram::new();

#[cfg(target_os = "none")]
mod bare_metal_alloc {
    use super::*;

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
    #[cfg(all(test, feature = "bench"))]
    pub(crate) fn heap_pd1_start_addr() -> usize {
        &raw const __heap_pd1_start as usize
    }

    /// Initialise the global allocator from the linker-defined heap region.
    /// Must be called exactly once during boot, before any allocations.
    pub fn init_global_allocator() {
        use core::sync::atomic::{AtomicBool, Ordering};
        static INIT: AtomicBool = AtomicBool::new(false);

        // Return early if we've already been initialised
        if INIT.swap(true, Ordering::Relaxed) {
            return;
        }

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

#[derive(Clone, Copy, Debug)]
pub(crate) enum Pool {
    UserPd0,
    KernelPd1,
    Psram,
}

/// Requests allocation from the specified pool
///
/// Safety: Caller must ensure allocators are initialised
#[cfg(target_os = "none")]
fn alloc_in(pool: Pool, layout: Layout) -> Option<NonNull<u8>> {
    // Safety: Caller ensures allocators have been initialised
    NonNull::new(unsafe {
        match pool {
            Pool::UserPd0 => BUDDY_PD0.alloc(layout),
            Pool::KernelPd1 => KALLOC_PD1.alloc(layout),
            Pool::Psram => BUDDY_PSRAM.alloc(layout),
        }
    })
}

///  Deallocates from the specified pool
///
/// Safety: Caller must ensure the region base pointer and layout are from a valid allocation
#[cfg(target_os = "none")]
fn dealloc_in(pool: Pool, base: NonNull<u8>, layout: Layout) {
    unsafe {
        match pool {
            Pool::UserPd0 => BUDDY_PD0.dealloc(base.as_ptr(), layout),
            Pool::KernelPd1 => KALLOC_PD1.dealloc(base.as_ptr(), layout),
            Pool::Psram => BUDDY_PSRAM.dealloc(base.as_ptr(), layout),
        }
    };
}

/// Requests allocation from the specified pool - required for host testing
#[cfg(not(target_os = "none"))]
fn alloc_in(_pool: Pool, layout: Layout) -> Option<NonNull<u8>> {
    NonNull::new(unsafe { alloc::alloc::alloc(layout) })
}

///  Deallocates from the specified pool - required for host testing
#[cfg(not(target_os = "none"))]
fn dealloc_in(_pool: Pool, base: NonNull<u8>, layout: Layout) {
    unsafe {
        alloc::alloc::dealloc(base.as_ptr(), layout);
    }
}

#[cfg(all(test, target_os = "none", feature = "bench"))]
pub(crate) use bare_metal_alloc::heap_pd1_start_addr;
// main.rs consumes this but isn't part of the lib check
#[cfg(target_os = "none")]
#[allow(unused_imports)]
// `init_global_allocator()` is called from main.rs during early boot.
pub use bare_metal_alloc::init_global_allocator;

// QEMU benchmark suite for KAlloc. The `bench` feature is opt-in (not
// part of test-all) so regression benches run separately from functional
// tests via `cargo test --features bench`.
#[cfg(all(test, target_os = "none", feature = "bench"))]
mod bench;

#[cfg(not(target_os = "none"))]
pub(super) fn align_up(addr: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    addr.checked_add(align - 1).map(|a| a & !(align - 1))
}
