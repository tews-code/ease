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
    loop {
        PARTNER_COUNT.fetch_add(1, Ordering::Relaxed);
        crate::kernel::sched::yield_now();
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
