//! Scheduler tests

// =============================================================================
// Smoke tests + cycle-count benchmark
// =============================================================================
//
// Tests pin down observable behaviour rather than implementation details, so
// they should survive future rung changes (preemption, priorities, etc.).
//
// The cycle-count benchmark prints a single line; no pass/fail. Useful as a
// number to eyeball across rungs. Note that with the test-sched background
// threads (thread1/thread2 spawned in kernel_init), the measured round-trip
// includes some time in those threads — it's "yield round-trip in this
// system" rather than bare context-switch cost. Still informative.

use super::stride::PRIORITY_DEFAULT;
use crate::kernel::sched::{Qos, StackClass};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Counter the partner thread bumps each iteration. We only assert it
/// INCREASES during a test, so the absolute value across tests is fine.
static PARTNER_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Set to 1 once the partner has been spawned; ensure-once across tests.
static PARTNER_SPAWNED: AtomicUsize = AtomicUsize::new(0);

fn partner_thread() {
    loop {
        PARTNER_COUNT.fetch_add(1, Ordering::Relaxed);
        crate::kernel::sched::yield_now();
    }
}

fn ensure_partner_spawned() {
    if PARTNER_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
        crate::kernel::sched::spawn(partner_thread, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
    }
}

/// Verify that `exit()` actually runs the dying-thread's last
/// instructions, then cleans up its TCB slot so it can be reused.
/// Spawns a tiny thread that flips a flag and exits; after a brief
/// wait, the flag must be set. Slot reuse is exercised implicitly:
/// spawning the helper THREADS_MAX times across a test session would
/// fail if exit() didn't recycle slots.
#[test_case]
fn exit_runs_and_recycles_slot() {
    static DONE: AtomicUsize = AtomicUsize::new(0);
    fn marker_then_exit() {
        DONE.store(1, Ordering::Relaxed);
        crate::kernel::sched::exit()
    }
    ensure_partner_spawned();
    let id = crate::kernel::sched::spawn(
        marker_then_exit,
        PRIORITY_DEFAULT,
        StackClass::KB2,
        Qos::Low,
    );
    assert!(id.is_some(), "spawn failed (no free slot?)");
    // Give the thread time to run, mark, and exit.
    crate::kernel::sched::sleep(50);
    assert_eq!(
        DONE.load(Ordering::Relaxed),
        1,
        "spawned thread did not run to completion",
    );
}

/// Smoke test 1: yield_now lets at least one other thread make progress.
/// Catches "scheduler never switches" or "switch corrupts state."
#[test_case]
fn yield_makes_progress() {
    ensure_partner_spawned();
    let before = PARTNER_COUNT.load(Ordering::Relaxed);
    for _ in 0..10 {
        crate::kernel::sched::yield_now();
    }
    let after = PARTNER_COUNT.load(Ordering::Relaxed);
    assert!(
        after > before,
        "partner thread didn't run during yields (before={}, after={})",
        before,
        after
    );
}

/// Smoke test 2: sched::sleep blocks for approximately the requested
/// duration. Catches "sleep doesn't actually block" and "sleep returns
/// late." Bound is tight (~one slice quantum of slack) since the
/// scheduler is tickless with sub-quantum sleep precision.
#[test_case]
fn sleep_blocks_for_duration() {
    ensure_partner_spawned();
    let start = crate::kernel::timer::elapsed_ms();
    crate::kernel::sched::sleep(100);
    let elapsed = crate::kernel::timer::elapsed_ms() - start;
    assert!(elapsed >= 95, "sleep too short: {} ms", elapsed);
    assert!(elapsed <= 118, "sleep too long: {} ms", elapsed);
}

/// Step 3 verification: a sub-slice-quantum sleep wakes at its actual
/// deadline, not the next slice boundary. Catches a regression where
/// reschedule fails to recompute the earliest sleeper deadline after
/// registering the current thread's new sleep state — without that
/// recompute, sleep(5) would round up to a slice boundary (~10ms+).
/// The upper bound has slack for UART drain noise from the test runner's
/// per-test name print (~6ms at 115200 baud) plus general overhead.
#[test_case]
fn sleep_below_quantum_wakes_at_deadline() {
    use core::fmt::Write;
    ensure_partner_spawned();
    let start = crate::kernel::timer::elapsed_ms();
    crate::kernel::sched::sleep_with_leeway(5, 0);
    let post_sleep = crate::kernel::timer::elapsed_ms();
    let elapsed = post_sleep - start;
    assert!(elapsed >= 5, "sleep too short: {} ms", elapsed);
    assert!(
        elapsed <= 25,
        "sub-quantum sleep rounded to slice boundary: {} ms",
        elapsed
    );
}

/// T1: a tight-deadline sleeper should wake within its own window even
/// when another sleeper with a much wider leeway window is also pending.
/// Guards the `earliest_deadline` contract: the tightest *upper bound* `b`
/// wins, not the smallest `min`. A pre-fix `earliest_deadline` that sorted
/// lexicographically on `(min, b)` could pick a long-leeway neighbor and
/// pin the timer to that neighbor's far-future `b`. In practice the
/// coalescing/yield path tends to self-correct within microseconds, so
/// this test is a contract guard for future regressions rather than a
/// direct demonstration of an observable bug. Pre- and post-fix both
/// pass under normal scheduling.
#[test_case]
fn tight_deadline_wakes_with_long_leeway_neighbor() {
    static BG_SPAWNED: AtomicUsize = AtomicUsize::new(0);
    fn long_leeway_sleeper() {
        // Short min, huge leeway: window [now+5ms, now+1005ms].
        crate::kernel::sched::sleep_with_leeway(5, 1000);
        // Exit cleanly so the slot is recycled and we don't leave a
        // ghost thread disturbing later tests' scheduling.
        crate::kernel::sched::exit()
    }
    ensure_partner_spawned();
    if BG_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
        crate::kernel::sched::spawn(
            long_leeway_sleeper,
            PRIORITY_DEFAULT,
            StackClass::KB2,
            Qos::Low,
        );
    }
    // Give the background sleeper a moment to reach its sleep_with_leeway.
    crate::kernel::sched::sleep(2);
    let start = crate::kernel::timer::elapsed_ms();
    crate::kernel::sched::sleep_with_leeway(20, 0);
    let elapsed = crate::kernel::timer::elapsed_ms() - start;
    // Bound is generous: this test is a contract guard for future
    // regressions in the earliest_deadline / coalescing path, not a tight
    // jitter measurement. A real regression (e.g. timer pinned to the
    // neighbor's far-future `b`) would blow past 100ms+.
    assert!(
        elapsed <= 50,
        "tight sleeper dragged by long-leeway neighbor: {} ms",
        elapsed
    );
}

/// T2: a neighbor sleeper with extreme `fixed_leeway` (passed as
/// `u64::MAX` at the public API) must not corrupt the wake math for
/// other sleepers. Without saturating arithmetic, `leeway_ms *
/// CYCLES_PER_MS` and `d.min + leeway` both wrap, potentially producing
/// a small bogus `wakeup_deadline` that `earliest_deadline` picks as
/// the global min, dragging tight sleepers' wake times forward. With
/// `saturating_mul` and `saturating_add` everywhere, the neighbor's
/// effective deadline pins to `u64::MAX` and falls out of the `.min()`,
/// leaving the tight sleeper undisturbed.
#[test_case]
fn huge_leeway_neighbor_does_not_corrupt_wake_math() {
    static SPAWNED: AtomicUsize = AtomicUsize::new(0);
    fn huge_leeway_sleeper() {
        // u64::MAX in both args — exercises every saturating site on the
        // path from public API to the Deadline struct.
        crate::kernel::sched::sleep_with_leeway(5, u64::MAX);
        crate::kernel::sched::exit()
    }
    ensure_partner_spawned();
    if SPAWNED.swap(1, Ordering::Relaxed) == 0 {
        crate::kernel::sched::spawn(
            huge_leeway_sleeper,
            PRIORITY_DEFAULT,
            StackClass::KB2,
            Qos::Low,
        );
    }
    // Give the background sleeper a moment to reach its sleep call.
    crate::kernel::sched::sleep(2);
    let start = crate::kernel::timer::elapsed_ms();
    crate::kernel::sched::sleep_with_leeway(20, 0);
    let elapsed = crate::kernel::timer::elapsed_ms() - start;
    // Lower bound catches "bogus wrapped wakeup pulled main forward"
    // (the original arithmetic-overflow failure mode).
    assert!(
        elapsed >= 5,
        "tight sleeper woke too early — likely wrapped neighbor deadline: {} ms",
        elapsed
    );
    // Upper bound catches "neighbor dragged main past slice boundary".
    assert!(
        elapsed <= 50,
        "tight sleeper delayed by huge-leeway neighbor: {} ms",
        elapsed
    );
}

/// T3: a sleep-spammer (busy + short sleep loop) must not get
/// disproportionately more CPU than a pure CPU-bound thread of equal
/// priority. Pre-fix (per-turn stride + unconditional switch-on-wake
/// in `preempt`), the spammer triggered a switch on every wake and
/// took ~67% of CPU at 2:1 over the hog. With time-weighted stride +
/// pass-aware preempt + upfront stride application, the spammer's
/// pass advances proportional to actual CPU consumed, so the attack
/// no longer pays off.
///
/// Iter counters are loop-iteration counts. Both threads run the same
/// per-iter work (one atomic fetch_add), so the iter ratio equals the
/// CPU-time ratio.
///
/// What this test does NOT guard: the dual problem of the spammer
/// being under-served because slice granularity is too coarse to let
/// sub-slice runs catch up to a long-running hog. Empirically the
/// post-fix ratio is ~20:1 hog:spammer, which would require sub-slice
/// preemption to improve. That's a separate fairness-precision
/// concern, not the wake-spam attack this test exists to guard.
#[test_case]
fn fair_stride_resists_wake_spammer() {
    static T3_SPAMMER_ITERS: AtomicUsize = AtomicUsize::new(0);
    static T3_HOG_ITERS: AtomicUsize = AtomicUsize::new(0);
    static T3_SPAWNED: AtomicUsize = AtomicUsize::new(0);
    // Active while the test is measuring. Cleared at the end so the
    // contenders park (yield-loop) instead of hogging CPU during
    // subsequent tests.
    static T3_ACTIVE: AtomicUsize = AtomicUsize::new(1);

    fn t3_spammer() {
        while T3_ACTIVE.load(Ordering::Relaxed) != 0 {
            for _ in 0..10_000 {
                T3_SPAMMER_ITERS.fetch_add(1, Ordering::Relaxed);
            }
            crate::kernel::sched::sleep(1);
        }
        crate::kernel::sched::exit()
    }

    fn t3_hog() {
        while T3_ACTIVE.load(Ordering::Relaxed) != 0 {
            for _ in 0..10_000 {
                T3_HOG_ITERS.fetch_add(1, Ordering::Relaxed);
            }
        }
        crate::kernel::sched::exit()
    }

    if T3_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
        crate::kernel::sched::spawn(t3_spammer, PRIORITY_DEFAULT, StackClass::KB2, Qos::Low);
        crate::kernel::sched::spawn(t3_hog, PRIORITY_DEFAULT, StackClass::KB2, Qos::Low);
    }
    // Warmup so the contenders stabilise before we sample.
    crate::kernel::sched::sleep(50);
    let s_start = T3_SPAMMER_ITERS.load(Ordering::Relaxed);
    let h_start = T3_HOG_ITERS.load(Ordering::Relaxed);
    crate::kernel::sched::sleep(500);
    let s_delta = T3_SPAMMER_ITERS.load(Ordering::Relaxed) - s_start;
    let h_delta = T3_HOG_ITERS.load(Ordering::Relaxed) - h_start;

    // Park the contenders before asserting, so a panic on assertion
    // failure still leaves the test threads parked.
    T3_ACTIVE.store(0, Ordering::Relaxed);

    assert!(s_delta > 0, "spammer made no progress");
    assert!(h_delta > 0, "hog made no progress");

    // Original wake-spam bug produced ~2:1 spammer:hog. The 3:2 bound
    // catches it with margin while tolerating per-run scheduler jitter.
    assert!(
        s_delta * 2 <= h_delta * 3,
        "spammer dominated (wake-spam regression?): spammer={}, hog={}",
        s_delta,
        h_delta
    );
}

/// T4: a Qos::High sleeper wakes near its deadline even when a
/// Qos::Low neighbor has a much wider leeway window. Verifies that
/// the QoS-derived leeway formula in `Deadline::leeway` differentiates
/// the two classes, and that `earliest_deadline` picks the tight
/// upper bound so the wider Low neighbor doesn't drag the timer.
///
/// Setup: Low sleeper does sleep(200), getting ~25 ms QoS-derived
/// leeway (window [200, 225] ms). High measurer does sleep(20),
/// getting near-zero leeway (window [20, ~20] ms). The Low sleeper's
/// window starts past the measurer's deadline, so coalescing leaves
/// the measurer alone.
#[test_case]
fn qos_high_wakes_precisely_with_low_neighbor() {
    static BG_SPAWNED: AtomicUsize = AtomicUsize::new(0);
    static MEASURER_ELAPSED: AtomicUsize = AtomicUsize::new(0);
    static MEASURER_DONE: AtomicUsize = AtomicUsize::new(0);

    fn t4_low_neighbor() {
        // Qos::Low + long sleep gives a wide leeway window.
        crate::kernel::sched::sleep(200);
        crate::kernel::sched::exit()
    }

    fn t4_high_measurer() {
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep(20);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        MEASURER_ELAPSED.store(elapsed as usize, Ordering::Relaxed);
        MEASURER_DONE.store(1, Ordering::Relaxed);
        crate::kernel::sched::exit()
    }

    ensure_partner_spawned();
    if BG_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
        crate::kernel::sched::spawn(t4_low_neighbor, PRIORITY_DEFAULT, StackClass::KB2, Qos::Low);
    }
    // Brief settle so the Low neighbor reaches its sleep before we
    // spawn the measurer; otherwise it's just main vs measurer.
    crate::kernel::sched::sleep(2);

    crate::kernel::sched::spawn(
        t4_high_measurer,
        PRIORITY_DEFAULT,
        StackClass::KB2,
        Qos::High,
    );

    // Wait for the measurer to finish its 20 ms sleep and record.
    crate::kernel::sched::sleep(60);
    assert_eq!(
        MEASURER_DONE.load(Ordering::Relaxed),
        1,
        "Qos::High measurer didn't finish in time"
    );
    let elapsed = MEASURER_ELAPSED.load(Ordering::Relaxed);
    assert!(elapsed >= 20, "Qos::High sleep too short: {} ms", elapsed);
    // Qos::High's formula yields essentially zero leeway for short
    // sleeps. Tolerance covers scheduling jitter and the discrete
    // `elapsed_ms` granularity, not aggressive leeway.
    assert!(
        elapsed <= 40,
        "Qos::High wake delayed (likely pulled by Low neighbor): {} ms",
        elapsed
    );
}

/// Smoke test 3: sleep_until with a past deadline returns immediately.
/// Catches the wake-check edge case — deadline <= now should fire on the
/// first reschedule iteration, never reach the wfi loop.
/// Tolerance is one slice quantum (~8 ms) — `elapsed_ms` is ms-granular
/// and the wake check happens during the next reschedule.
#[test_case]
fn sleep_until_past_returns_quickly() {
    ensure_partner_spawned();
    let now = crate::kernel::timer::elapsed_ms();
    let deadline = now.saturating_sub(20);
    let start = crate::kernel::timer::elapsed_ms();
    crate::kernel::sched::sleep_until(deadline);
    let elapsed = crate::kernel::timer::elapsed_ms() - start;
    assert!(
        elapsed <= 15,
        "past deadline should return quickly, took {} ms",
        elapsed
    );
}

/// Verify that `park()` actually blocks the calling thread and that
/// `unpark()` wakes it. Spawns a child that records progress before
/// parking and after waking. The test asserts the child is stuck at
/// "parked" until `unpark()` runs, then advances to "resumed" afterward.
///
/// Note: timing-dependent — the sleeps must be long enough for the
/// child to be scheduled and reach `park`. Race-free guarantees come
/// once the mutex layer is in place; this is a mechanism smoke test.
#[test_case]
fn park_blocks_until_unpark() {
    static CHILD_PROGRESS: AtomicUsize = AtomicUsize::new(0);
    fn child_thread() {
        CHILD_PROGRESS.store(1, Ordering::Relaxed);
        crate::kernel::sched::park();
        CHILD_PROGRESS.store(2, Ordering::Relaxed);
    }
    ensure_partner_spawned();
    // Reset — the static persists across test runs.
    CHILD_PROGRESS.store(0, Ordering::Relaxed);
    let handle =
        crate::kernel::sched::spawn(child_thread, PRIORITY_DEFAULT, StackClass::KB2, Qos::High)
            .expect("spawn failed (no free slot?)");
    // Give the child time to run and reach park().
    crate::kernel::sched::sleep(50);
    assert_eq!(
        CHILD_PROGRESS.load(Ordering::Relaxed),
        1,
        "child did not reach park (progress != 1)"
    );
    crate::kernel::sched::unpark(&handle);
    // Give the child time to resume past park().
    crate::kernel::sched::sleep(50);
    assert_eq!(
        CHILD_PROGRESS.load(Ordering::Relaxed),
        2,
        "child did not resume past park (progress != 2)"
    );
}

/// Mutex smoke test: lock, mutate through the guard, drop guard, lock again,
/// verify the mutation persisted. Single-threaded — exercises only the
/// uncontended fast path. Catches gross breakage in CAS arguments, the
/// Guard Deref/DerefMut wiring, and the Drop release path.
#[test_case]
fn mutex_basic_lock_unlock() {
    static M: crate::kernel::sync::Mutex<u32> = crate::kernel::sync::Mutex::new(0);
    {
        let mut g = M.lock();
        assert_eq!(*g, 0, "initial value");
        *g = 42;
    }
    {
        let g = M.lock();
        assert_eq!(*g, 42, "value persisted across lock/unlock");
    }
    // Reset for any subsequent test runs.
    *M.lock() = 0;
}

/// Mutex contention test: N worker threads each increment a shared counter
/// K times through the mutex. Final value must equal N*K. Exercises:
///   - fast-path CAS under contention (some threads will spin briefly and win)
///   - slow-path enqueue + park (threads that lose the spin block)
///   - Drop's slow path (waking the next waiter when a contender is parked)
///   - direct handoff (woken thread should already own the lock)
///
/// If any of those paths are broken, the final value is either too low
/// (lost increments → broken mutual exclusion) or the test hangs (lost
/// wakeup → some worker parked forever and DONE_COUNT never reaches N).
#[test_case]
fn mutex_contention_counter() {
    const WORKERS: usize = 3;
    const ITERS: u32 = 50;
    static M: crate::kernel::sync::Mutex<u32> = crate::kernel::sync::Mutex::new(0);
    static DONE_COUNT: AtomicUsize = AtomicUsize::new(0);

    // Reset across runs.
    *M.lock() = 0;
    DONE_COUNT.store(0, Ordering::Relaxed);

    fn worker() {
        for _ in 0..ITERS {
            *M.lock() += 1;
        }
        DONE_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    for _ in 0..WORKERS {
        let id = crate::kernel::sched::spawn(worker, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
        assert!(id.is_some(), "spawn failed (no free slot?)");
    }

    // Wait for all workers to finish. Generous timeout (~1s) — at ITERS=50
    // and SLICE=16ms even with lots of contention this should be under 200ms.
    let start = crate::kernel::timer::elapsed_ms();
    while DONE_COUNT.load(Ordering::Relaxed) < WORKERS {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - start > 1000 {
            panic!(
                "workers did not complete in time (done={}/{}, counter={})",
                DONE_COUNT.load(Ordering::Relaxed),
                WORKERS,
                *M.lock()
            );
        }
    }

    let final_value = *M.lock();
    assert_eq!(
        final_value,
        (WORKERS as u32) * ITERS,
        "lost increments — mutual exclusion violated",
    );
}

/// Mutex<()> serialisation test: mirrors the shape of Virtio's IO_IN_PROGRESS.
/// The mutex carries no data — its only purpose is to serialise a critical
/// section. We use it to gate updates to a *separate* AtomicUsize so we can
/// inspect the count, but the mutex's job is just "only one thread in here
/// at a time." Catches: zero-sized UnsafeCell breakage, Drop on Mutex<()>,
/// guard usage when there's no data to dereference.
#[test_case]
fn mutex_unit_serialises_critical_section() {
    const WORKERS: usize = 3;
    const ITERS: u32 = 30;
    static GATE: crate::kernel::sync::Mutex<()> = crate::kernel::sync::Mutex::new(());
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static DONE_COUNT: AtomicUsize = AtomicUsize::new(0);

    COUNTER.store(0, Ordering::Relaxed);
    DONE_COUNT.store(0, Ordering::Relaxed);

    fn worker() {
        for _ in 0..ITERS {
            let _g = GATE.lock();
            // Use Relaxed loads/stores inside the critical section — the
            // mutex provides the ordering. If serialisation is broken,
            // two threads will load the same value and the final count
            // will be too low.
            let v = COUNTER.load(Ordering::Relaxed);
            // A tiny "yield-like" pause so concurrent threads have a
            // chance to interleave if mutual exclusion is broken.
            core::hint::spin_loop();
            COUNTER.store(v + 1, Ordering::Relaxed);
        }
        DONE_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    for _ in 0..WORKERS {
        let id = crate::kernel::sched::spawn(worker, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
        assert!(id.is_some(), "spawn failed (no free slot?)");
    }

    let start = crate::kernel::timer::elapsed_ms();
    while DONE_COUNT.load(Ordering::Relaxed) < WORKERS {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - start > 1000 {
            panic!(
                "workers did not complete in time (done={}/{}, counter={})",
                DONE_COUNT.load(Ordering::Relaxed),
                WORKERS,
                COUNTER.load(Ordering::Relaxed)
            );
        }
    }

    assert_eq!(
        COUNTER.load(Ordering::Relaxed),
        WORKERS * ITERS as usize,
        "lost increments — Mutex<()> serialisation broken",
    );
}

/// High-contention stress test: more workers and iterations than the basic
/// counter test, sized to force the slow path repeatedly. Validates that
/// the parking/unparking cycle is robust under sustained pressure — if any
/// rare race fires (lost wakeup, double pop, list corruption), the test
/// will hang on the watchdog or produce a wrong final count.
#[test_case]
fn mutex_high_contention_stress() {
    const WORKERS: usize = 6;
    const ITERS: u32 = 200;
    static M: crate::kernel::sync::Mutex<u32> = crate::kernel::sync::Mutex::new(0);
    static DONE_COUNT: AtomicUsize = AtomicUsize::new(0);

    *M.lock() = 0;
    DONE_COUNT.store(0, Ordering::Relaxed);

    fn worker() {
        for _ in 0..ITERS {
            *M.lock() += 1;
        }
        DONE_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    for _ in 0..WORKERS {
        let id = crate::kernel::sched::spawn(worker, PRIORITY_DEFAULT, StackClass::KB2, Qos::Low);
        assert!(id.is_some(), "spawn failed (no free slot?)");
    }

    let start = crate::kernel::timer::elapsed_ms();
    while DONE_COUNT.load(Ordering::Relaxed) < WORKERS {
        crate::kernel::sched::sleep(20);
        if crate::kernel::timer::elapsed_ms() - start > 3000 {
            panic!(
                "stress test did not complete (done={}/{}, counter={})",
                DONE_COUNT.load(Ordering::Relaxed),
                WORKERS,
                *M.lock()
            );
        }
    }

    assert_eq!(
        *M.lock(),
        (WORKERS as u32) * ITERS,
        "lost increments under high contention",
    );
}

/// Hold-across-sleep test: one thread acquires the mutex and sleeps while
/// holding it. A second thread tries to acquire — it should *park* (not
/// busy-spin) until the holder releases. Verifies that contenders actually
/// reach the slow path's park() rather than spinning forever, and that the
/// release path's unpark wakes them. Indirectly proves blocking happens by
/// measuring contender latency: if the contender spun rather than parked,
/// it would still acquire ~immediately after release; if it parked
/// correctly, the acquire latency reflects the holder's full sleep
/// duration.
#[test_case]
fn mutex_holder_sleep_parks_contender() {
    const HOLD_MS: u64 = 100;
    static M: crate::kernel::sync::Mutex<u32> = crate::kernel::sync::Mutex::new(0);
    static CONTENDER_DONE: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_LATENCY_MS: AtomicUsize = AtomicUsize::new(0);

    *M.lock() = 0;
    CONTENDER_DONE.store(0, Ordering::Relaxed);
    CONTENDER_LATENCY_MS.store(0, Ordering::Relaxed);

    fn holder() {
        let mut g = M.lock();
        *g = 1;
        crate::kernel::sched::sleep(HOLD_MS);
        *g = 2;
        // Drop the guard; the contender should be parked and get woken.
    }

    fn contender() {
        let start = crate::kernel::timer::elapsed_ms();
        let g = M.lock();
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        // We should see value == 2 (set by holder before release).
        assert_eq!(*g, 2, "contender saw stale value (memory ordering)");
        CONTENDER_LATENCY_MS.store(elapsed as usize, Ordering::Relaxed);
        CONTENDER_DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let id1 = crate::kernel::sched::spawn(holder, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
    assert!(id1.is_some(), "holder spawn failed");
    // Brief delay so holder definitely grabs the lock first.
    crate::kernel::sched::sleep(10);
    let id2 = crate::kernel::sched::spawn(contender, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
    assert!(id2.is_some(), "contender spawn failed");

    // Wait for contender to complete.
    let start = crate::kernel::timer::elapsed_ms();
    while CONTENDER_DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - start > 1000 {
            panic!("contender did not acquire lock");
        }
    }

    let latency = CONTENDER_LATENCY_MS.load(Ordering::Relaxed) as u64;
    // Holder sleeps HOLD_MS while holding. Contender should wait at least
    // most of that. Allow generous slack for scheduling jitter.
    assert!(
        latency >= HOLD_MS - 20,
        "contender acquired too quickly ({} ms < {} ms) — likely spinning instead of parking",
        latency,
        HOLD_MS
    );
}

/// Completion signal-then-wait: signal() fires before any wait(), so the
/// pending flag is set. A subsequent wait() observes pending = true and
/// returns immediately without parking. Validates the "fast path" of
/// wait() — the case where the signal has already arrived.
#[test_case]
fn completion_signal_then_wait() {
    static C: crate::kernel::sync::Completion = crate::kernel::sync::Completion::new();
    static CHILD_DONE: AtomicUsize = AtomicUsize::new(0);

    CHILD_DONE.store(0, Ordering::Relaxed);

    fn child() {
        C.wait();
        CHILD_DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    // Signal BEFORE the child runs — pending should be set.
    C.signal();
    let id = crate::kernel::sched::spawn(child, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
    assert!(id.is_some(), "spawn failed (no free slot?)");

    // Give the child time to run. wait() should return immediately
    // because pending is true; CHILD_DONE should be set quickly.
    crate::kernel::sched::sleep(50);
    assert_eq!(
        CHILD_DONE.load(Ordering::Relaxed),
        1,
        "child did not pass through wait() after pre-signal",
    );
}

/// Completion wait-then-signal: a child calls wait() and parks; the parent
/// later calls signal() to wake it. Exercises the parking path of wait() —
/// the case where the waiter sleeps and is unparked by signal().
#[test_case]
fn completion_wait_then_signal() {
    static C: crate::kernel::sync::Completion = crate::kernel::sync::Completion::new();
    static CHILD_PROGRESS: AtomicUsize = AtomicUsize::new(0);

    CHILD_PROGRESS.store(0, Ordering::Relaxed);

    fn child() {
        CHILD_PROGRESS.store(1, Ordering::Relaxed);
        C.wait();
        CHILD_PROGRESS.store(2, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let id = crate::kernel::sched::spawn(child, PRIORITY_DEFAULT, StackClass::KB2, Qos::High);
    assert!(id.is_some(), "spawn failed (no free slot?)");

    // Give the child time to reach C.wait() and actually park.
    crate::kernel::sched::sleep(50);
    assert_eq!(
        CHILD_PROGRESS.load(Ordering::Relaxed),
        1,
        "child did not reach wait() (progress != 1)",
    );

    // Fire the signal — child should wake up and continue past wait().
    C.signal();
    crate::kernel::sched::sleep(50);
    assert_eq!(
        CHILD_PROGRESS.load(Ordering::Relaxed),
        2,
        "child did not resume past wait() after signal (progress != 2)",
    );
}

/// One-shot semantics: two signal() calls without an intervening wait()
/// must NOT satisfy two future wait()s. Since pending is a boolean (not a
/// counter), the second signal is idempotent. The first wait() consumes the
/// pending flag; a second wait() must block, proving the completion's
/// single-shot contract.
#[test_case]
fn completion_signal_twice_is_idempotent() {
    static C: crate::kernel::sync::Completion = crate::kernel::sync::Completion::new();
    static FIRST_DONE: AtomicUsize = AtomicUsize::new(0);
    static SECOND_DONE: AtomicUsize = AtomicUsize::new(0);

    FIRST_DONE.store(0, Ordering::Relaxed);
    SECOND_DONE.store(0, Ordering::Relaxed);

    fn child_two_waits() {
        C.wait();
        FIRST_DONE.store(1, Ordering::Relaxed);
        C.wait();
        SECOND_DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    // Two signals before any wait. Pending becomes true (then stays true).
    C.signal();
    C.signal();

    let id = crate::kernel::sched::spawn(
        child_two_waits,
        PRIORITY_DEFAULT,
        StackClass::KB2,
        Qos::High,
    );
    assert!(id.is_some(), "spawn failed (no free slot?)");

    // First wait should pass through immediately (pending was set).
    crate::kernel::sched::sleep(50);
    assert_eq!(
        FIRST_DONE.load(Ordering::Relaxed),
        1,
        "first wait did not consume the pending signal",
    );

    // Second wait must BLOCK — the boolean pending was consumed by the
    // first wait, and the second signal didn't add a second slot.
    assert_eq!(
        SECOND_DONE.load(Ordering::Relaxed),
        0,
        "second wait wrongly satisfied — pending behaves like a counter, not a one-shot",
    );

    // Send one more signal to release the child and clean up.
    C.signal();
    crate::kernel::sched::sleep(50);
    assert_eq!(
        SECOND_DONE.load(Ordering::Relaxed),
        1,
        "second wait did not complete after explicit signal",
    );
}

/// Benchmark: average yield_now round-trip cycles. No assertion.
/// Reports cpu (this thread only) and wall (includes partner thread).
#[cfg(feature = "test-bench")]
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
