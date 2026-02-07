//! Benchmarking utilities
//!
//! Uses the RISC-V cycle counter for precise timing measurements.
//! Only available in test builds.

#![cfg(test)]

use core::arch::asm;

/// Read the RISC-V cycle counter (64-bit)
///
/// Returns the number of clock cycles since reset.
/// On RV32, this reads both `cycleh` and `cycle` CSRs.
fn cycles() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!(
            "rdcycleh {hi}",
            "rdcycle {lo}",
            hi = out(reg) hi,
            lo = out(reg) lo,
            options(nomem, nostack),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Measure the number of cycles taken by a closure
///
/// Returns the cycle count for the operation.
pub fn measure<F: FnOnce()>(f: F) -> u64 {
    let start = cycles();
    f();
    let end = cycles();
    end.saturating_sub(start)
}

/// Run a benchmark and print results
///
/// Runs the closure and prints the cycle count.
#[expect(dead_code)]
pub fn run<F: FnOnce()>(name: &str, f: F) {
    let elapsed = measure(f);
    crate::println!("  {}: {} cycles", name, elapsed);
}

/// Run a benchmark multiple times and print average
///
/// Useful for reducing noise in measurements.
#[expect(dead_code)]
pub fn run_avg<F: FnMut()>(name: &str, iterations: u32, mut f: F) {
    // Warm-up run (not counted)
    f();

    let mut total: u64 = 0;
    for _ in 0..iterations {
        total += measure(|| f());
    }
    let avg = total / iterations as u64;
    crate::println!("  {}: {} cycles (avg of {})", name, avg, iterations);
}

/// Default tolerance for regression detection (20%)
pub const DEFAULT_TOLERANCE_PERCENT: u64 = 20;

/// Check for performance regression
///
/// Runs the closure multiple times, computes average, and compares against baseline.
/// Panics if the measured cycles exceed `baseline * (100 + tolerance_percent) / 100`.
///
/// # Arguments
/// * `name` - Name of the benchmark (for reporting)
/// * `baseline` - Expected cycle count (update when intentionally changing performance)
/// * `tolerance_percent` - Allowed percentage above baseline (e.g., 20 = 20%)
/// * `iterations` - Number of iterations to average
/// * `f` - The operation to benchmark
///
/// # Panics
/// Panics if measured cycles exceed baseline + tolerance, failing the test.
pub fn check_regression<F: Fn()>(
    name: &str,
    baseline: u64,
    tolerance_percent: u64,
    iterations: u32,
    f: F,
) {
    // Warm-up run
    f();

    let mut total: u64 = 0;
    for _ in 0..iterations {
        total += measure(&f);
    }
    let avg = total / iterations as u64;

    let max_allowed = baseline + (baseline * tolerance_percent / 100);

    if avg > max_allowed {
        let regression_pct = (avg - baseline) * 100 / baseline;
        crate::print!(
            "  REGRESSION {}: {} cycles (baseline: {}, +{}%)",
            name,
            avg,
            baseline,
            regression_pct
        );
        panic!("Performance regression detected");
    } else if avg > baseline {
        let over_pct = (avg - baseline) * 100 / baseline;
        crate::print!(
            "  OK {}: {} cycles (baseline: {}, +{}%)",
            name,
            avg,
            baseline,
            over_pct
        );
    } else {
        let under_pct = (baseline - avg) * 100 / baseline;
        crate::print!(
            "  OK {}: {} cycles (baseline: {}, -{}%)",
            name,
            avg,
            baseline,
            under_pct
        );
    }
}

/// Check for regression with default tolerance (20%)
pub fn check<F: Fn()>(name: &str, baseline: u64, iterations: u32, f: F) {
    check_regression(name, baseline, DEFAULT_TOLERANCE_PERCENT, iterations, f);
}
