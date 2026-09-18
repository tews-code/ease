//! Benchmarking utilities
//!
//! Each measurement records wall cycles: raw `rdcycles` elapsed across
//! the closure, including any blocking, preemption and interrupts.
//! (There is no per-thread CPU figure: the scheduler's run counter is
//! only charged at a switch and is kept in CLINT ticks, so it cannot
//! bracket a closure. A gated closure either never blocks, or blocks on
//! the device whose latency is the thing being gated.)
//!
//! Regression baselines gate on the MINIMUM of the iterations, not the
//! average: on QEMU a whole-VM stall (tb_flush, host scheduling) can land
//! inside any one iteration, and the minimum is the run it did not land
//! in. Averages are reported alongside as informational.
//!
//! Only available in test builds.
#![cfg(test)]

use crate::arch::csr::rdcycles;

/// Cycle counts captured for a single measurement window.
#[derive(Copy, Clone)]
pub struct Cycles {
    /// Wall-clock cycles elapsed (includes blocking, preemption, IRQs).
    pub wall: u64,
}

/// Measure wall cycles taken by a closure.
pub fn measure<F: FnOnce()>(f: F) -> Cycles {
    let wall_start = rdcycles();
    f();
    let wall_end = rdcycles();
    Cycles {
        wall: wall_end.saturating_sub(wall_start),
    }
}

/// Run a benchmark and print results.
#[allow(dead_code)]
pub fn run<F: FnOnce()>(name: &str, f: F) {
    let c = measure(f);
    crate::println!("  {}: wall={} cycles", name, c.wall);
}

/// Minimum and average wall cycles over `iterations` runs of `f`, after
/// one uncounted warm-up run.
fn min_avg<F: FnMut()>(iterations: u32, mut f: F) -> (u64, u64) {
    f();
    let mut min: u64 = u64::MAX;
    let mut total: u64 = 0;
    for _ in 0..iterations {
        let c = measure(&mut f);
        min = min.min(c.wall);
        total += c.wall;
    }
    (min, total / iterations as u64)
}

/// Run a benchmark multiple times and print min and average wall cycles.
#[allow(dead_code)]
pub fn run_avg<F: FnMut()>(name: &str, iterations: u32, f: F) {
    let (min, avg) = min_avg(iterations, f);
    crate::println!(
        "  {}: wall min={} avg={} cycles ({} runs)",
        name,
        min,
        avg,
        iterations
    );
}

/// Default tolerance for regression detection (20%)
#[allow(dead_code)]
pub const DEFAULT_TOLERANCE_PERCENT: u64 = 20;

/// Check for performance regression on the minimum wall cycles.
///
/// Panics if `wall_min > baseline * (100 + tolerance_percent) / 100`.
pub fn check_regression<F: FnMut()>(
    name: &str,
    baseline: u64,
    tolerance_percent: u64,
    iterations: u32,
    f: F,
) {
    let (min, avg) = min_avg(iterations, f);
    let max_allowed = baseline + (baseline * tolerance_percent / 100);

    if min > max_allowed {
        let regression_pct = (min - baseline) * 100 / baseline;
        crate::print!(
            "  REGRESSION {}: wall min={} avg={} cycles (baseline: {}, +{}%)",
            name,
            min,
            avg,
            baseline,
            regression_pct
        );
        panic!("Performance regression detected");
    } else if min > baseline {
        let over_pct = (min - baseline) * 100 / baseline;
        crate::print!(
            "  OK {}: wall min={} avg={} cycles (baseline: {}, +{}%)",
            name,
            min,
            avg,
            baseline,
            over_pct
        );
    } else {
        let under_pct = (baseline - min) * 100 / baseline;
        crate::print!(
            "  OK {}: wall min={} avg={} cycles (baseline: {}, -{}%)",
            name,
            min,
            avg,
            baseline,
            under_pct
        );
    }
}

/// Check for regression with default tolerance (20%)
#[allow(dead_code)]
pub fn check<F: FnMut()>(name: &str, baseline: u64, iterations: u32, f: F) {
    check_regression(name, baseline, DEFAULT_TOLERANCE_PERCENT, iterations, f);
}
