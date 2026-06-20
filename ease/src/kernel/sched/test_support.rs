//! Shared scheduler test scaffolding.
//!
//! A long-lived "partner" thread that other threads can yield to, so
//! tests and benchmarks exercise real context switches rather than
//! yield-to-idle. Used by both the functional suite (`tests.rs`, behind
//! `test-sched`) and the benchmark suite (`bench.rs`, behind `bench`),
//! so it lives in its own module gated on either.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::kernel::alloc::Order;

/// Bumped each iteration by the partner thread; some sched tests read it
/// to confirm the partner is making progress.
pub(super) static PARTNER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Set to 1 once the partner has been spawned; ensure-once across tests.
static PARTNER_SPAWNED: AtomicUsize = AtomicUsize::new(0);

fn partner_thread() {
    // Background load for the "under partner load" tests. Two pitfalls to avoid:
    //  - a bare `yield_now()` loop now busy-spins through `reschedule` (a lone
    //    yielder correctly keeps the CPU), burning a hart and flooding the trace;
    //  - a pure busy loop *permanently pegs* a hart, which starves tests that
    //    don't expect a busy sibling (e.g. fault_kills_whole_process already has
    //    a spin-forever process saturating the other hart → the poll loop hangs).
    // So: do a little work, then RELINQUISH via a short sleep. This keeps a
    // runnable thread cycling for contention without saturating a core.
    loop {
        for _ in 0..10_000 {
            PARTNER_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        crate::kernel::sched::sleep(1);
    }
}

/// Spawn the partner thread exactly once, however many times this is called.
pub(super) fn ensure_partner_spawned() {
    if PARTNER_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
        crate::kernel::sched::Builder::new()
            .with_stack_class(Order::KB2)
            .spawn(partner_thread);
    }
}
