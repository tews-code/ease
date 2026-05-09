//! Benchmarking utilities
//!
//! Each measurement records two cycle counts:
//!  - `cpu`: per-thread CPU cycles (excludes blocked / preempted time)
//!  - `wall`: raw mhart cycles elapsed (includes everything, observer-visible)
//!
//! Regression baselines gate on `cpu` only — wall-clock varies with
//! interrupt scheduling and is reported as informational.
//!
//! Only available in test builds.
#![cfg(test)]

use crate::arch::csr::rdcycles;
use crate::kernel::sched::get_current_cycles;

/// Cycle counts captured for a single measurement window.
#[derive(Copy, Clone)]
pub struct Cycles {
    /// Per-thread CPU cycles consumed by the closure.
    pub cpu: u64,
    /// Wall-clock cycles elapsed (includes blocking, preemption, IRQs).
    pub wall: u64,
}

/// Measure CPU and wall cycles taken by a closure.
///
/// Wall is the outer bracket (cheap `rdcycles` reads); cpu is the inner
/// bracket (heavier `get_current_cycles` reads). This guarantees
/// wall >= cpu, which would otherwise flip for very short operations
/// because reading `get_current_cycles` itself is non-trivial.
pub fn measure<F: FnOnce()>(f: F) -> Cycles {
    let wall_start = rdcycles();
    let cpu_start = get_current_cycles(0);
    f();
    let cpu_end = get_current_cycles(0);
    let wall_end = rdcycles();
    Cycles {
        cpu: cpu_end.saturating_sub(cpu_start),
        wall: wall_end.saturating_sub(wall_start),
    }
}

/// Run a benchmark and print results.
#[allow(dead_code)]
pub fn run<F: FnOnce()>(name: &str, f: F) {
    let c = measure(f);
    crate::println!("  {}: cpu={} wall={} cycles", name, c.cpu, c.wall);
}

/// Run a benchmark multiple times and print averages of cpu and wall.
#[allow(dead_code)]
pub fn run_avg<F: FnMut()>(name: &str, iterations: u32, mut f: F) {
    // Warm-up run (not counted)
    f();

    let mut cpu_total: u64 = 0;
    let mut wall_total: u64 = 0;
    for _ in 0..iterations {
        let c = measure(|| f());
        cpu_total += c.cpu;
        wall_total += c.wall;
    }
    let cpu_avg = cpu_total / iterations as u64;
    let wall_avg = wall_total / iterations as u64;
    crate::println!(
        "  {}: cpu={} wall={} cycles (avg of {})",
        name,
        cpu_avg,
        wall_avg,
        iterations
    );
}

/// Default tolerance for regression detection (20%)
#[allow(dead_code)]
pub const DEFAULT_TOLERANCE_PERCENT: u64 = 20;

/// Check for performance regression on CPU cycles.
///
/// Wall cycles are reported alongside but do not affect the assertion —
/// they reflect device latency, interrupt scheduling, and host noise,
/// which are not reproducible enough to gate CI on.
///
/// Panics if `cpu_avg > baseline * (100 + tolerance_percent) / 100`.
pub fn check_regression<F: FnMut()>(
    name: &str,
    baseline: u64,
    tolerance_percent: u64,
    iterations: u32,
    mut f: F,
) {
    // Warm-up run
    f();

    let mut cpu_total: u64 = 0;
    let mut wall_total: u64 = 0;
    for _ in 0..iterations {
        let c = measure(&mut f);
        cpu_total += c.cpu;
        wall_total += c.wall;
    }
    let cpu_avg = cpu_total / iterations as u64;
    let wall_avg = wall_total / iterations as u64;

    let max_allowed = baseline + (baseline * tolerance_percent / 100);

    if cpu_avg > max_allowed {
        let regression_pct = (cpu_avg - baseline) * 100 / baseline;
        crate::print!(
            "  REGRESSION {}: cpu={} wall={} cycles (baseline cpu: {}, +{}%)",
            name,
            cpu_avg,
            wall_avg,
            baseline,
            regression_pct
        );
        panic!("Performance regression detected");
    } else if cpu_avg > baseline {
        let over_pct = (cpu_avg - baseline) * 100 / baseline;
        crate::print!(
            "  OK {}: cpu={} wall={} cycles (baseline cpu: {}, +{}%)",
            name,
            cpu_avg,
            wall_avg,
            baseline,
            over_pct
        );
    } else {
        let under_pct = (baseline - cpu_avg) * 100 / baseline;
        crate::print!(
            "  OK {}: cpu={} wall={} cycles (baseline cpu: {}, -{}%)",
            name,
            cpu_avg,
            wall_avg,
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
