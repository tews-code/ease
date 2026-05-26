// QEMU benchmark suite for the kernel's global allocator (KAlloc).
// Runs as part of `cargo test --bin ease`. Allocations go through the
// registered `#[global_allocator]`; per-allocator counters are read
// from the slab tier (see comment on the counter imports below).

use alloc::alloc::{alloc, dealloc};
use alloc::boxed::Box;
use core::alloc::Layout;
use core::hint::black_box;
use core::sync::atomic::Ordering;

use crate::kernel::alloc::heap_start_addr;
use crate::println;

// kalloc dispatches small allocs to slab and large allocs to buddy. The
// bench workloads (alloc_one_byte, alloc_small_mix) are all ≤ 64 bytes,
// so they all route through slab. Slab's counters are therefore the
// representative ones for these workloads. If you add a workload that
// exercises sizes > 256 bytes, you'd see those allocations in buddy's
// counters and they wouldn't show up here.
use crate::kernel::alloc::slab::{
    ALLOC_COUNT, ALLOCATED_BYTES, DEALLOCATED_BYTES, HEAP_TOP, PADDING_BYTES,
};

mod baseline {
    // Rung-3 baseline: includes ~18k cycles/iter of preemption overhead
    // (trap save/restore + boot↔idle context switch on every 10ms tick).
    // Allocator code unchanged from rung 2 (which was 2_500); the bump
    // reflects the per-tick rescheduling cost.
    pub(super) const ONE_BYTE_ALLOC: u64 = 25_000;
    pub(super) const ONE_BYTE_ALLOC_ITERS: u32 = 100_000;

    // Rung-3 baseline: small_mix does 8 allocs+deallocs per iter, with
    // proportional preemption overhead. Measured ~53k after rung 3.
    pub(super) const SMALL_MIX_ALLOC: u64 = 55_000;
    pub(super) const SMALL_MIX_ALLOC_ITERS: u32 = 10_000;
}

const TOLERANCE_PERC: u64 = 50;

fn allocate_one_byte() {
    let layout = Layout::new::<u8>();
    let p = black_box(unsafe { alloc(layout) });
    unsafe { dealloc(p, layout) };
}

// Slab-friendly mixed-workload. Every allocation stays within the
// slab's contract (size <= 64 B, align <= 64). Sizes and alignments
// are deliberately varied to defeat any single-shape fast path.
fn allocate_deallocate_small_mix() {
    let _a = black_box(Box::new(0x5u8)); //  1 B / align  1
    let _b = black_box(Box::new([0x3u16; 8])); // 16 B / align  2
    let _c = black_box(Box::new([0x7u32; 4])); // 16 B / align  4
    let _d = black_box(Box::new([0x9u64; 4])); // 32 B / align  8
    let _e = black_box(Box::new([0xbu8; 17])); // 17 B / align  1
    let _f = black_box(Box::new([0xdu16; 21])); // 42 B / align  2
    let _g = black_box(Box::new(0xfu128)); // 16 B / align 16
    let _h = black_box(Box::new([0x1u8; 64])); // 64 B / align  1 (= SLOT_SIZE)
}

// Stress test: a million 1 KiB Box allocations, each freed immediately
// at end-of-scope. Exercises the alloc/dealloc hot path under sustained
// pressure.
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

    crate::bench::check_regression(
        "alloc_one_byte",
        baseline::ONE_BYTE_ALLOC,
        TOLERANCE_PERC,
        baseline::ONE_BYTE_ALLOC_ITERS,
        allocate_one_byte,
    );

    println!();

    crate::bench::check_regression(
        "alloc_small_mix",
        baseline::SMALL_MIX_ALLOC,
        TOLERANCE_PERC,
        baseline::SMALL_MIX_ALLOC_ITERS,
        allocate_deallocate_small_mix,
    );

    println!();
    println!("  Total padding: {}", PADDING_BYTES.load(Ordering::Relaxed));

    println!();
    println!(
        "  Allocation count: {}",
        ALLOC_COUNT.load(Ordering::Relaxed)
    );
    println!(
        "  Allocated: {} bytes",
        ALLOCATED_BYTES.load(Ordering::Relaxed) - DEALLOCATED_BYTES.load(Ordering::Relaxed)
    );
    let heap_used = HEAP_TOP.load(Ordering::Relaxed) - heap_start_addr();
    println!("  Heap used: {} bytes", heap_used);

    println!();
    println!("===================== ");
    println!();
}
