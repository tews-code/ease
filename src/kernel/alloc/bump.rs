//! Bump Allocator

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

use core::alloc::{GlobalAlloc, Layout};
#[cfg(test)]
use core::sync::atomic::AtomicU32;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::kernel::alloc::align_up;

unsafe extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

#[global_allocator]
static BUMP_ALLOCATOR: Allocator = Allocator {
    next: AtomicUsize::new(0),
};

#[cfg(test)]
static ALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static DEALLOCATED_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static ALLOC_COUNT: AtomicU32 = AtomicU32::new(0);

struct Allocator {
    next: AtomicUsize,
}

impl Allocator {
    fn init(&self) {
        let current = self.next.load(Ordering::Relaxed);
        debug_assert!(current == 0);
        // Needs initialisation
        let start = &raw const __heap_start as usize;
        self.next.store(start, Ordering::Relaxed);
    }

    #[cfg(all(test, feature = "test-alloc"))]
    pub unsafe fn reset(&self) {
        self.next
            .store(&raw const __heap_start as usize, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut current = self.next.load(Ordering::Relaxed);
        debug_assert!(current != 0);
        loop {
            let next = align_up(current, layout.align());
            // OOM check
            let new = next.saturating_add(layout.size());
            if new > &raw const __heap_end as usize {
                return core::ptr::null_mut();
            }
            match self.next.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    #[cfg(test)]
                    let _ = ALLOCATED_BYTES.fetch_add(layout.size() as u32, Ordering::Relaxed);
                    #[cfg(test)]
                    let _ = ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);

                    return next as *mut u8;
                }
                Err(actual) => current = actual,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        #[cfg(test)]
        let _ = DEALLOCATED_BYTES.fetch_add(_layout.size() as u32, Ordering::Relaxed);
    }
}

pub fn init() {
    BUMP_ALLOCATOR.init();
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
        pub(super) const ONE_BYTE_ALLOC: u64 = 500;
        pub(super) const ONE_BYTE_ALLOC_ITERS: u32 = 100_000;
        pub(super) const AWKWARD_ALLOC: u64 = 20_000;
        pub(super) const AWKWARD_ALLOC_ITERS: u32 = 20;
        pub(super) const CORE_SYNC_BASE: u64 = 300;
        pub(super) const CORE_SYNC_ITERS: u32 = 200_000;
    }

    const TOLERANCE_PERC: u64 = 50;

    fn allocate_one_byte() {
        let layout = Layout::new::<u8>();
        let _ = black_box(unsafe { BUMP_ALLOCATOR.alloc(layout) });
    }

    fn allocate_deallocate_awkward() {
        let _ = black_box(Box::new([0x5u128; 13]));
        let _ = black_box(Box::new([0x3u16; 1_001]));
        let _ = black_box(Box::new(true));
        let _ = black_box(Box::new([0x7u64; 513]));
        let _ = black_box(Box::new(false));
        let _ = black_box(Box::new([0x9u32; 257]));
        let _ = black_box(Box::new([0xbu8; 2_017]));
    }

    fn bare_sync_timing() {
        let current = BUMP_ALLOCATOR.next.load(Ordering::Relaxed);
        let _ = BUMP_ALLOCATOR.next.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
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
        //Safety: No live pointers between benchmarks
        unsafe { BUMP_ALLOCATOR.reset() };
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        ALLOC_COUNT.store(0, Ordering::Relaxed);

        crate::bench::check_regression(
            "alloc_awkward",
            baseline::AWKWARD_ALLOC,
            TOLERANCE_PERC,
            baseline::AWKWARD_ALLOC_ITERS,
            || allocate_deallocate_awkward(),
        );

        let total_heap_used =
            BUMP_ALLOCATOR.next.load(Ordering::Relaxed) - &raw const __heap_start as usize;
        let padding = total_heap_used - ALLOCATED_BYTES.load(Ordering::Relaxed) as usize;
        println!();
        println!("  Total padding: {}", padding);

        //Safety: No live pointers between benchmarks
        unsafe { BUMP_ALLOCATOR.reset() };

        crate::bench::check_regression(
            "core_sync_timing",
            baseline::CORE_SYNC_BASE,
            TOLERANCE_PERC,
            baseline::CORE_SYNC_ITERS,
            || bare_sync_timing(),
        );

        //Safety: No live pointers between benchmarks
        unsafe { BUMP_ALLOCATOR.reset() };
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        DEALLOCATED_BYTES.store(0, Ordering::Relaxed);
        ALLOC_COUNT.store(0, Ordering::Relaxed);

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
        let heap_used =
            BUMP_ALLOCATOR.next.load(Ordering::Relaxed) - &raw const __heap_start as usize;
        println!("  Heap used: {} bytes", heap_used);

        // Comment out to continue CI
        //Safety: No live pointers between benchmarks
        // unsafe { BUMP_ALLOCATOR.reset() };
        // ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        // DEALLOCATED_BYTES.store(0, Ordering::Relaxed);
        // ALLOC_COUNT.store(0, Ordering::Relaxed);
        // allocate_or_bust();

        println!();
        println!("===================== ");
        println!();
    }
}
