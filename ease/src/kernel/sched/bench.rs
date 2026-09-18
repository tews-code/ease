//! Scheduler benchmarks (behind the `bench` feature).
//!
//! Units: on QEMU `mcycle` is host time, 1000 cycles = 1 us. Under
//! `-icount shift=0` (pass via `EASE_QEMU_ARGS`) it counts guest
//! instructions instead, summed over both harts, so the same lines then
//! read as instruction counts; keep the other hart idle for clean numbers.
//! Compare min-of-run numbers across designs, not averages.

use crate::arch::csr::rdcycles;
use crate::bench;
use crate::kernel::alloc::Order;
use crate::kernel::sched::{self, Builder};
use crate::kernel::sync::with_interrupts_disabled;
use crate::kernel::timer;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Wall cycles per call of `f`, best of `runs` batches of `n` calls.
/// Min-of-batches discards host stalls (tb_flush, timer jitter).
fn per_call<F: FnMut()>(runs: u32, n: u32, mut f: F) -> u64 {
    let mut best = u64::MAX;
    for _ in 0..runs {
        let c = bench::measure(|| {
            for _ in 0..n {
                f();
            }
        });
        best = best.min(c.wall / n as u64);
    }
    best
}

/// riscv32 has no 64-bit atomics; anything over u32::MAX saturates.
fn clamp(v: u64) -> u32 {
    v.min(u32::MAX as u64) as u32
}

const RUNS: u32 = 5;
const M: u32 = 10_000;
const N: u32 = 1000;

/// The unit costs the switch path is built from, so the round trip can
/// be partitioned:
///  - `rdcycles`: the measurement itself (a CSR read)
///  - `timer::elapsed`: three CLINT MMIO reads; reschedule reads it once
///    at the top and reuses that `now` throughout
///  - `set_next_deadline`: three CLINT MMIO writes to mtimecmp (same MMIO
///    class); reschedule programs it once, before releasing the lock
///  - interrupts off/on: the mstatus CSR dance around the critical section
///  - sched lock: uncontended IrqSpinLock acquire + release
fn unit_costs() {
    let rd = per_call(RUNS, M, || {
        core::hint::black_box(rdcycles());
    });
    println!("  rdcycles:                    wall min={rd:>6} cycles/call");

    let el = per_call(RUNS, M, || {
        core::hint::black_box(timer::elapsed());
    });
    println!(
        "  timer::elapsed:              wall min={el:>6} cycles/call (~{} per MMIO read)",
        el / 3
    );

    let irq = per_call(RUNS, M, || {
        with_interrupts_disabled(|_cs| core::hint::black_box(()));
    });
    println!("  interrupts off/on:           wall min={irq:>6} cycles/call");

    let lk = per_call(RUNS, M, || {
        drop(super::SCHEDULER.sched.lock());
    });
    println!("  sched lock (uncontended):    wall min={lk:>6} cycles/call");

    // The mtimecmp reprogram every reschedule ends with: three CLINT MMIO
    // writes (hi=MAX, lo, hi), and QEMU re-arms a host timer behind them.
    // Far-future deadline so no tick lands inside the bracket; alternate
    // two values so a same-value write can't be short-circuited. The yield
    // afterwards hands the timer back to the scheduler's real deadline.
    let far = timer::elapsed() + 1_000 * timer::CYCLES_PER_MS;
    let mut flip = 0;
    let sd = per_call(RUNS, M, || {
        flip ^= 1;
        with_interrupts_disabled(|_cs| timer::set_next_deadline(far + flip));
    });
    sched::yield_now();
    println!(
        "  set_next_deadline (mtimecmp): wall min={sd:>6} cycles/call (3 MMIO writes, IRQs off/on)"
    );
}

/// The scans reschedule runs under the lock, each timed alone. The pick
/// itself is private to the scheduler and is not separable from here;
/// it is the remainder once these are subtracted.
fn reschedule_parts() {
    let idx = crate::kernel::percpu::current_thread_idx();

    let se = per_call(RUNS, M, || {
        let mut sched = super::SCHEDULER.sched.lock();
        let now = timer::elapsed();
        let tcb = sched.thread_blocks.0[idx].as_mut().unwrap();
        core::hint::black_box(tcb.slice_ended(now));
    });
    println!("  lock + elapsed + slice_ended: wall min={se:>6} cycles/call");

    let ws = per_call(RUNS, M, || {
        let mut sched = super::SCHEDULER.sched.lock();
        core::hint::black_box(super::SCHEDULER.wake_sleeping_threads(&mut sched));
    });
    println!("  lock + wake_sleeping_threads: wall min={ws:>6} cycles/call");

    let nd = per_call(RUNS, M, || {
        let mut sched = super::SCHEDULER.sched.lock();
        let now = timer::elapsed();
        core::hint::black_box(
            sched
                .thread_blocks
                .next_timer_deadline(super::stride::SLICE, now),
        );
    });
    println!("  lock + elapsed + next_timer_deadline: wall min={nd:>6} cycles/call");

    // Everything reschedule does BEFORE the pick: interrupts off, flag
    // take, lock, elapsed, slice_ended, wake_sleeping_threads, then the
    // precondition check fails (we are Running, not Blocked) and it returns.
    let pre = per_call(RUNS, M, || {
        super::SCHEDULER.reschedule(
            Some(super::State::Blocked),
            super::threads::PostSwitch::Ready,
        );
    });
    println!("  reschedule up to the pick (precondition bail): wall min={pre:>6} cycles/call");

    // A copy of pick_next_ready_mut's scan (the real one is private to
    // the scheduler), so its cost can be seen alone.
    let pk = per_call(RUNS, M, || {
        let sched = super::SCHEDULER.sched.lock();
        let this_hart = crate::arch::hart_id() as u8;
        let mut best_idx = None;
        let mut best_pass = u64::MAX;
        for (i, slot) in sched.thread_blocks.0.iter().enumerate() {
            if let Some(tcb) = slot {
                let candidate = tcb.state == super::State::Ready;
                let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
                let not_stealing = !crate::kernel::percpu::other_scheduler_online()
                    || i != crate::kernel::percpu::other_current_thread_idx();
                let pri_ok = tcb.priority != super::stride::PRIORITY_MIN;
                if candidate && not_stealing && affinity_ok && pri_ok && tcb.pass < best_pass {
                    best_pass = tcb.pass;
                    best_idx = Some(i);
                }
            }
        }
        core::hint::black_box(best_idx);
    });
    println!("  lock + pick scan (copy):     wall min={pk:>6} cycles/call");
}

// Ping-pong on ONE hart: worker and partner both pinned to hart 0, so
// every worker yield_now switches to the partner and the partner's
// yield_now switches straight back. One worker call = two real context
// switches through reschedule + switch_to + post_switch_cleanup. The
// other hart sits in wfi (halted under icount) apart from its ticks.
static PINGPONG_DONE: AtomicUsize = AtomicUsize::new(0);
static WORKER_DONE: AtomicUsize = AtomicUsize::new(0);
static ROUND_TRIP: AtomicU32 = AtomicU32::new(0);
static LONE_YIELD: AtomicU32 = AtomicU32::new(0);
static IPIS_PER_ROUND_TRIP_X100: AtomicU32 = AtomicU32::new(0);

fn partner() {
    while PINGPONG_DONE.load(Ordering::Relaxed) == 0 {
        sched::yield_now();
    }
    // Park rather than exit-by-yield: see feedback on ghost threads.
    sched::sleep_until(u64::MAX);
}

fn worker() {
    // Let the partner reach its yield loop first
    sched::sleep(5);
    let ipi_before = crate::kernel::ipi::SENT.load(Ordering::Relaxed);
    let rt = per_call(RUNS, N, sched::yield_now);
    let ipi_during = crate::kernel::ipi::SENT.load(Ordering::Relaxed) - ipi_before;
    ROUND_TRIP.store(clamp(rt), Ordering::Relaxed);
    IPIS_PER_ROUND_TRIP_X100.store(
        clamp(ipi_during as u64 * 100 / (RUNS * N) as u64),
        Ordering::Relaxed,
    );
    PINGPONG_DONE.store(1, Ordering::Relaxed);
    // Give the partner a chance to see the flag and park
    sched::sleep(5);
    // Now no Ready peer on this hart: yield_now takes the bookkeeping-only
    // path (lock, scans, mtimecmp write) and keeps running.
    let ly = per_call(RUNS, N, sched::yield_now);
    LONE_YIELD.store(clamp(ly), Ordering::Relaxed);
    WORKER_DONE.store(1, Ordering::Release);
    sched::sleep_until(u64::MAX);
}

fn pingpong() {
    PINGPONG_DONE.store(0, Ordering::Relaxed);
    WORKER_DONE.store(0, Ordering::Relaxed);
    let p = Builder::new()
        .with_stack_class(Order::KB2)
        .with_affinity(0)
        .spawn(partner);
    assert!(p.is_some(), "partner spawn failed");
    let w = Builder::new()
        .with_stack_class(Order::KB8)
        .with_affinity(0)
        .spawn(worker);
    assert!(w.is_some(), "worker spawn failed");
    let start = timer::elapsed_ms();
    while WORKER_DONE.load(Ordering::Acquire) == 0 {
        sched::sleep(10);
        assert!(
            timer::elapsed_ms() - start < 20_000,
            "pingpong worker did not finish in 20 s"
        );
    }
    println!(
        "  yield_now, no Ready peer:    wall min={:>6} cycles/call (bookkeeping only, no switch)",
        LONE_YIELD.load(Ordering::Relaxed)
    );
    println!(
        "  yield_now ping-pong, 1 hart: wall min={:>6} cycles/call (= 2 context switches)",
        ROUND_TRIP.load(Ordering::Relaxed)
    );
    let x = IPIS_PER_ROUND_TRIP_X100.load(Ordering::Relaxed);
    println!(
        "  IPIs sent per ping-pong call: {}.{:02} (either hart, whole window)",
        x / 100,
        x % 100
    );
}

/// No assertion; numbers are informational.
#[test_case]
fn sched_benchmarks() {
    println!();
    println!("====== SCHEDULER ====== ");
    println!("  ({M} calls x {RUNS} runs for unit costs, {N} x {RUNS} for yields)");
    unit_costs();
    reschedule_parts();
    pingpong();
    println!("===================== ");
    println!();
}
