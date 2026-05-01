// QEMU benchmark suite for the active global allocator. Runs as part of
// `cargo test --bin ease`. Allocator-agnostic: explicit alloc/dealloc
// calls go through `alloc::alloc::alloc`/`dealloc`, which dispatch via
// the registered `#[global_allocator]`. Per-allocator counters are
// pulled in from whichever allocator's module is currently compiled.
//
// Bump cannot free, so the bench harness calls `reset_bump()` between
// every measurement phase. For non-bump builds it is a no-op.

use alloc::alloc::{alloc, dealloc};
use alloc::boxed::Box;
use core::alloc::Layout;
use core::hint::black_box;
use core::sync::atomic::Ordering;

use crate::kernel::alloc::heap_start_addr;
use crate::println;

#[cfg(feature = "alloc-slab")]
use crate::kernel::alloc::slab::{
    ALLOC_COUNT, ALLOCATED_BYTES, DEALLOCATED_BYTES, HEAP_TOP, PADDING_BYTES,
};

#[cfg(feature = "alloc-freelist")]
use crate::kernel::alloc::freelist::{
    ALLOC_COUNT, ALLOCATED_BYTES, DEALLOCATED_BYTES, HEAP_TOP, PADDING_BYTES,
};

#[cfg(feature = "alloc-bump")]
use crate::kernel::alloc::bump::{
    ALLOC_COUNT, ALLOCATED_BYTES, DEALLOCATED_BYTES, HEAP_TOP, PADDING_BYTES,
};

#[cfg(feature = "alloc-buddy")]
use crate::kernel::alloc::buddy::{
    ALLOC_COUNT, ALLOCATED_BYTES, DEALLOCATED_BYTES, HEAP_TOP, PADDING_BYTES,
};

// kalloc dispatches small allocs to slab and large allocs to buddy. The
// bench workloads (alloc_one_byte, alloc_small_mix) are all ≤ 64 bytes,
// so they all route through slab. Slab's counters are therefore the
// representative ones for these workloads. If you add a workload that
// exercises sizes > 256 bytes, you'd see those allocations in buddy's
// counters and they wouldn't show up here.
#[cfg(feature = "alloc-kalloc")]
use crate::kernel::alloc::slab::{
    ALLOC_COUNT, ALLOCATED_BYTES, DEALLOCATED_BYTES, HEAP_TOP, PADDING_BYTES,
};

#[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
use alloc::string::ToString;
#[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
use alloc::vec::Vec;

mod baseline {
    #[cfg(feature = "alloc-freelist")]
    pub(super) const ONE_BYTE_ALLOC: u64 = 1_200;
    #[cfg(feature = "alloc-bump")]
    pub(super) const ONE_BYTE_ALLOC: u64 = 600;
    #[cfg(feature = "alloc-buddy")]
    pub(super) const ONE_BYTE_ALLOC: u64 = 4_000;
    #[cfg(feature = "alloc-slab")]
    pub(super) const ONE_BYTE_ALLOC: u64 = 1_500;
    #[cfg(feature = "alloc-kalloc")]
    pub(super) const ONE_BYTE_ALLOC: u64 = 2_500;

    pub(super) const ONE_BYTE_ALLOC_ITERS: u32 = 1;

    #[cfg(feature = "alloc-freelist")]
    pub(super) const SMALL_MIX_ALLOC: u64 = 4_000;
    #[cfg(feature = "alloc-bump")]
    pub(super) const SMALL_MIX_ALLOC: u64 = 1_000;
    #[cfg(feature = "alloc-buddy")]
    pub(super) const SMALL_MIX_ALLOC: u64 = 6_000;
    #[cfg(feature = "alloc-slab")]
    pub(super) const SMALL_MIX_ALLOC: u64 = 3_000;
    #[cfg(feature = "alloc-kalloc")]
    pub(super) const SMALL_MIX_ALLOC: u64 = 10_000;
    // Bump can't free, so its sidecar of live small_mix allocations
    // grows with each iteration. Cap iters so the cumulative footprint
    // stays comfortably inside the 256 KiB heap.
    #[cfg(not(feature = "alloc-bump"))]
    pub(super) const SMALL_MIX_ALLOC_ITERS: u32 = 10_000;
    #[cfg(feature = "alloc-bump")]
    pub(super) const SMALL_MIX_ALLOC_ITERS: u32 = 500;

    #[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
    pub(super) const AWKWARD_ALLOC: u64 = 11_000;
    #[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
    pub(super) const AWKWARD_ALLOC_ITERS: u32 = 20;
}

const TOLERANCE_PERC: u64 = 50;

/// Reset bump's heap pointer and counters between bench phases. No-op
/// for slab and freelist (they free, so no accumulation between phases).
///
/// # Safety
/// Caller must guarantee that no allocations made before this call are
/// still live. Always called between completed `check_regression`
/// invocations, so any locals from the previous phase are gone.
unsafe fn reset_bump() {
    #[cfg(feature = "alloc-bump")]
    unsafe {
        crate::BUMP.reset_for_bench();
    }
}

fn allocate_one_byte() {
    let layout = Layout::new::<u8>();
    let p = black_box(unsafe { alloc(layout) });
    unsafe { dealloc(p, layout) };
}

// fn allocate_one_byte() {
//     let layout = Layout::new::<u8>();
//     let s0 = crate::bench::cycles();
//     let p = unsafe { alloc(layout) };
//     let s1 = crate::bench::cycles();
//     unsafe { dealloc(p, layout) };
//     let s2 = crate::bench::cycles();
//     crate::println!("alloc: {}, dealloc: {}", s1 - s0, s2 - s1);
// }

// Slab-friendly mixed-workload sibling of `allocate_deallocate_awkward`.
// Every allocation stays within the slab's contract (size <= 64 B,
// align <= 64), so all three allocators can run it and produce
// comparable numbers. Sizes and alignments are deliberately varied to
// defeat any single-shape fast path.
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

#[cfg(feature = "alloc-freelist")]
#[repr(C, align(4096))]
struct BigAlloc {
    val: bool,
}

// Stress test for variable-size, high-alignment allocations. The
// freelist version includes a 4096-aligned `BigAlloc` to exercise the
// alignment-padding path. The bump version omits it because the
// resulting per-iteration footprint would exceed the 256 KiB heap.
#[cfg(feature = "alloc-freelist")]
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

#[cfg(feature = "alloc-bump")]
fn allocate_deallocate_awkward() {
    let _a = black_box(Box::new([0x5u128; 13]));
    let _b = black_box(Box::new([0x3u16; 1_001]));
    let _c = black_box(Box::new(true));
    let _d = black_box(Box::new([0x7u64; 513]));
    let _e = black_box(Box::new(false));
    let _f = black_box(Box::new([0x9u32; 257]));
    let _g = black_box(Box::new([0xbu8; 2_017]));
}

// Simulate allocations typical of shell activity. Uses
// `Box<[u8; 1024]>`, `String`, and growing `Vec`s — all of which
// exceed the slab's slot size — so it is gated to freelist and bump.
#[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
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

// Stress test: a million 1 KiB Box allocations, each freed immediately
// at end-of-scope. Exercises the alloc/dealloc hot path under sustained
// pressure. Skipped for bump because bump cannot free, so the heap
// fills after a few hundred iterations and the next alloc panics.
#[cfg(not(feature = "alloc-bump"))]
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
    // Disable interrupts for the duration of the benchmark so preemption
    // and timer ISRs don't inflate cycle counts. The allocator benches
    // are pure CPU work — no I/O — so blocking interrupts here is safe.
    // (Virtio benches DO need interrupts for I/O completion, so they
    // run with interrupts enabled — measure() doesn't disable globally.)
    let prev = crate::arch::disable_interrupts();

    println!();
    println!("====== ALLOCATOR ====== ");
    println!();

    // SAFETY: No live allocations from prior tests survive into this
    // bench function — the test harness runs each test_case in
    // sequence and locals are dropped between them.
    unsafe { reset_bump() };
    crate::bench::check_regression(
        "alloc_one_byte",
        baseline::ONE_BYTE_ALLOC,
        TOLERANCE_PERC,
        baseline::ONE_BYTE_ALLOC_ITERS,
        || allocate_one_byte(),
    );

    println!();

    // SAFETY: previous phase's locals have been dropped.
    unsafe { reset_bump() };
    crate::bench::check_regression(
        "alloc_small_mix",
        baseline::SMALL_MIX_ALLOC,
        TOLERANCE_PERC,
        baseline::SMALL_MIX_ALLOC_ITERS,
        || allocate_deallocate_small_mix(),
    );

    #[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
    {
        println!();
        // SAFETY: previous phase's locals have been dropped.
        unsafe { reset_bump() };
        crate::bench::check_regression(
            "alloc_awkward",
            baseline::AWKWARD_ALLOC,
            TOLERANCE_PERC,
            baseline::AWKWARD_ALLOC_ITERS,
            || allocate_deallocate_awkward(),
        );
    }

    println!();
    println!("  Total padding: {}", PADDING_BYTES.load(Ordering::Relaxed));

    #[cfg(any(feature = "alloc-freelist", feature = "alloc-bump"))]
    {
        println!();
        // SAFETY: previous phase's locals have been dropped.
        unsafe { reset_bump() };
        allocator_benchmark();
    }

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

    // Long-running stress test. Slab and freelist can sustain it (each
    // box drops at end-of-scope, so the allocator reclaims the slot
    // before the next iteration); bump cannot, and is gated out.
    #[cfg(not(feature = "alloc-bump"))]
    {
        //println!();
        //allocate_or_bust();
    }

    println!();
    println!("===================== ");
    println!();

    crate::arch::restore_interrupts(prev);
}
