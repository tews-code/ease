//! Scheduler benchmarks (behind the `bench` feature).

use super::test_support::ensure_partner_spawned;

/// Benchmark: average yield_now round-trip cycles. No assertion.
/// Reports cpu (this thread only) and wall (includes partner thread).
#[test_case]
fn sched_benchmarks() {
    use crate::bench;
    use crate::println;

    println!();
    println!("====== SCHEDULER ====== ");
    println!();

    ensure_partner_spawned();
    const N: u32 = 1000;
    let c = bench::measure(|| {
        for _ in 0..N {
            crate::kernel::sched::yield_now();
        }
    });
    println!(
        "  yield_now round-trip: cpu={} wall={} cycles/call ({} calls)",
        c.cpu / N as u64,
        c.wall / N as u64,
        N
    );

    println!();
    println!("===================== ");
    println!();
}
