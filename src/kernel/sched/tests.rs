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
//
// QEMU TIMER JITTER NOTE — bounds on the "under_partner_load" tests
// (sleep10_wakes_promptly_under_partner_load,
// completion_wait_wakes_promptly_under_partner_load,
// mutex_holder_sleep_parks_contender,
// mutex_holder_parks_contender_with_completion) are deliberately loose
// (15-50 ms of slack) because QEMU's timer interrupt delivery has up
// to ~10-15 ms of jitter relative to mtimecmp under our partner +
// wfi pattern. Investigation (May 2026) traced this in two parts:
//
//   1. WITHOUT -icount: ~7 ms of host CFS scheduler latency between
//      mtime crossing mtimecmp and the trap handler running.
//   2. WITH -icount sleep=on: still 10-13 ms of *virtual* time
//      between mtime crossing mtimecmp and trap_entry, dominated by
//      the bootstrap idle thread's wfi loop not waking promptly even
//      though sleep=on should jump vt to the next event.
//
// The kernel wake path itself takes ~60 µs from trap entry to the
// sleeper resuming, regardless. So these bounds catch egregious
// regressions (>40-50 ms of wake delay would indicate a real bug)
// without flaking on every-run QEMU jitter. The diagnostic
// infrastructure used in the investigation (per-stage mtime
// timestamps in preempt/reschedule, mtimecmp call ring buffer) is
// not committed — see git history for context if it needs to be
// rebuilt. Real RP2350 hardware will have prompt interrupt delivery
// and the bounds can be tightened then.

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
        crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .spawn(partner_thread);
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
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .with_qos(Qos::Low)
        .spawn(marker_then_exit);
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
        crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .with_qos(Qos::Low)
            .spawn(long_leeway_sleeper);
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
        crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .with_qos(Qos::Low)
            .spawn(huge_leeway_sleeper);
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
        crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .with_qos(Qos::Low)
            .spawn(t3_spammer);
        crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .with_qos(Qos::Low)
            .spawn(t3_hog);
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
        crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .with_qos(Qos::Low)
            .spawn(t4_low_neighbor);
    }
    // Brief settle so the Low neighbor reaches its sleep before we
    // spawn the measurer; otherwise it's just main vs measurer.
    crate::kernel::sched::sleep(2);

    crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(t4_high_measurer);

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
    let handle = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(child_thread)
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
        let id = crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .spawn(worker);
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
        let id = crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .spawn(worker);
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
        let id = crate::kernel::sched::Builder::new()
            .with_stack_class(StackClass::KB2)
            .with_qos(Qos::Low)
            .spawn(worker);
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
/// release path's unpark wakes them.
///
/// Records timestamps from BOTH sides (holder records its own sleep
/// duration and release time; contender records its own start and
/// acquire time) so a failure panic shows exactly where the missing
/// milliseconds went. The historical failure mode is "contender acquired
/// in 70-79 ms instead of >=80 ms"; the diagnostic breakdown
/// distinguishes "holder's sleep was short" from "contender's start
/// was recorded late" from "everything looks fine and the bound is
/// wrong".
///
/// All times reported in the panic are relative to scenario T0 (just
/// before holder spawn) so the spacing is human-readable.
#[test_case]
fn mutex_holder_sleep_parks_contender() {
    const HOLD_MS: u64 = 100;
    static M: crate::kernel::sync::Mutex<u32> = crate::kernel::sync::Mutex::new(0);
    static T0: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_LOCKED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_SLEEP_STARTED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_SLEEP_ENDED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_RELEASED_AT: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_STARTED_AT: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_ACQUIRED_AT: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_DONE: AtomicUsize = AtomicUsize::new(0);
    static MAIN_AFTER_SLEEP10_AT: AtomicUsize = AtomicUsize::new(0);
    static MAIN_AFTER_SPAWN_CONTENDER_AT: AtomicUsize = AtomicUsize::new(0);

    *M.lock() = 0;
    for s in [
        &T0,
        &HOLDER_LOCKED_AT,
        &HOLDER_SLEEP_STARTED_AT,
        &HOLDER_SLEEP_ENDED_AT,
        &HOLDER_RELEASED_AT,
        &CONTENDER_STARTED_AT,
        &CONTENDER_ACQUIRED_AT,
        &CONTENDER_DONE,
        &MAIN_AFTER_SLEEP10_AT,
        &MAIN_AFTER_SPAWN_CONTENDER_AT,
    ] {
        s.store(0, Ordering::Relaxed);
    }

    fn holder() {
        let mut g = M.lock();
        HOLDER_LOCKED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        *g = 1;
        let sleep_start = crate::kernel::timer::elapsed_ms();
        HOLDER_SLEEP_STARTED_AT.store(sleep_start as usize, Ordering::Relaxed);
        crate::kernel::sched::sleep(HOLD_MS);
        HOLDER_SLEEP_ENDED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        *g = 2;
        HOLDER_RELEASED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        // Drop the guard; the contender should be parked and get woken.
    }

    fn contender() {
        let start = crate::kernel::timer::elapsed_ms();
        CONTENDER_STARTED_AT.store(start as usize, Ordering::Relaxed);
        let g = M.lock();
        let acquired = crate::kernel::timer::elapsed_ms();
        CONTENDER_ACQUIRED_AT.store(acquired as usize, Ordering::Relaxed);
        // We should see value == 2 (set by holder before release).
        assert_eq!(*g, 2, "contender saw stale value (memory ordering)");
        CONTENDER_DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    T0.store(
        crate::kernel::timer::elapsed_ms() as usize,
        Ordering::Relaxed,
    );
    let id1 = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(holder);
    assert!(id1.is_some(), "holder spawn failed");
    // Brief delay so holder definitely grabs the lock first.
    crate::kernel::sched::sleep(10);
    MAIN_AFTER_SLEEP10_AT.store(
        crate::kernel::timer::elapsed_ms() as usize,
        Ordering::Relaxed,
    );
    let id2 = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(contender);
    assert!(id2.is_some(), "contender spawn failed");
    MAIN_AFTER_SPAWN_CONTENDER_AT.store(
        crate::kernel::timer::elapsed_ms() as usize,
        Ordering::Relaxed,
    );

    // Wait for contender to complete.
    let wait_start = crate::kernel::timer::elapsed_ms();
    while CONTENDER_DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - wait_start > 1000 {
            panic!("contender did not acquire lock");
        }
    }

    let t0 = T0.load(Ordering::Relaxed);
    let hlocked = HOLDER_LOCKED_AT.load(Ordering::Relaxed);
    let hsleep_start = HOLDER_SLEEP_STARTED_AT.load(Ordering::Relaxed);
    let hsleep_end = HOLDER_SLEEP_ENDED_AT.load(Ordering::Relaxed);
    let hreleased = HOLDER_RELEASED_AT.load(Ordering::Relaxed);
    let cstarted = CONTENDER_STARTED_AT.load(Ordering::Relaxed);
    let cacquired = CONTENDER_ACQUIRED_AT.load(Ordering::Relaxed);
    let main_after_sleep10 = MAIN_AFTER_SLEEP10_AT.load(Ordering::Relaxed);
    let main_after_spawn = MAIN_AFTER_SPAWN_CONTENDER_AT.load(Ordering::Relaxed);

    let latency = cacquired - cstarted;
    let holder_sleep_duration = hsleep_end - hsleep_start;
    // Holder sleeps HOLD_MS while holding. Contender should wait at least
    // most of that. The HOLD_MS - 50 lower bound accommodates the
    // "sleep(10) as barrier" heuristic stretching to ~25-30 ms under
    // QEMU timer jitter (see QEMU TIMER JITTER NOTE at top of file)
    // plus the few ms it takes for main to spawn the contender after
    // its sleep(10) wakes. Anything tighter flakes on most CI runs.
    assert!(
        latency >= HOLD_MS as usize - 50,
        "contender acquired too quickly ({} ms < {} ms) — likely spinning instead of parking.\n  \
         T0={} ms (reference). All times below are ms-since-T0.\n  \
         holder_locked          @ +{} ms\n  \
         holder_sleep_started   @ +{} ms\n  \
         holder_sleep_ended     @ +{} ms  (sleep_duration={} ms, requested={} ms)\n  \
         holder_released        @ +{} ms\n  \
         main_after_sleep10     @ +{} ms  (requested 10 ms)\n  \
         main_after_spawn_contender @ +{} ms\n  \
         contender_started      @ +{} ms\n  \
         contender_acquired     @ +{} ms",
        latency,
        HOLD_MS,
        t0,
        hlocked - t0,
        hsleep_start - t0,
        hsleep_end - t0,
        holder_sleep_duration,
        HOLD_MS,
        hreleased - t0,
        main_after_sleep10 - t0,
        main_after_spawn - t0,
        cstarted - t0,
        cacquired - t0,
    );
}

/// Direct sleep-precision test: a single dedicated thread records its OWN
/// sleep duration while the partner yield-loop runs in the background.
/// This isolates "is sleep itself short?" from contender measurement
/// noise. If `mutex_holder_sleep_parks_contender` fails with a short
/// `holder_sleep_duration`, this test should fail too.
///
/// Bound is tighter than `sleep_blocks_for_duration` (which runs from
/// the main test runner thread) because this thread has no other
/// responsibilities — its measured elapsed should be dominated by the
/// sleep itself, not by per-test setup/teardown noise.
#[test_case]
fn sleep_precision_dedicated_thread() {
    const SLEEP_MS: u64 = 100;
    static MEASURED_MS: AtomicUsize = AtomicUsize::new(0);
    static DONE: AtomicUsize = AtomicUsize::new(0);

    MEASURED_MS.store(0, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);

    fn sleeper() {
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep(SLEEP_MS);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        MEASURED_MS.store(elapsed as usize, Ordering::Relaxed);
        DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(sleeper);
    assert!(id.is_some(), "sleeper spawn failed");

    let wait_start = crate::kernel::timer::elapsed_ms();
    while DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(20);
        if crate::kernel::timer::elapsed_ms() - wait_start > 1000 {
            panic!("sleeper did not finish");
        }
    }

    let measured = MEASURED_MS.load(Ordering::Relaxed) as u64;
    assert!(
        measured >= SLEEP_MS - 5,
        "dedicated-thread sleep too short: {} ms (requested {} ms)",
        measured,
        SLEEP_MS,
    );
    assert!(
        measured <= SLEEP_MS + 30,
        "dedicated-thread sleep too long: {} ms (requested {} ms)",
        measured,
        SLEEP_MS,
    );
}

/// Wake-after-spawn latency test: measures how long it takes a freshly
/// spawned thread to actually start running, from the spawner's point
/// of view. If this is consistently >0 ms, the stride scheduler is
/// delaying the new thread's first instruction by some scheduling
/// quantum.
///
/// Pattern mirrors how `mutex_holder_sleep_parks_contender` spawns the
/// contender: spawn from a sleep-waking thread, then immediately yield
/// via sleep(0). The new thread should run before the spawner resumes.
#[test_case]
fn spawn_to_first_instruction_latency() {
    static SPAWN_TIME: AtomicUsize = AtomicUsize::new(0);
    static CHILD_FIRST_INSTRUCTION: AtomicUsize = AtomicUsize::new(0);
    static CHILD_DONE: AtomicUsize = AtomicUsize::new(0);

    SPAWN_TIME.store(0, Ordering::Relaxed);
    CHILD_FIRST_INSTRUCTION.store(0, Ordering::Relaxed);
    CHILD_DONE.store(0, Ordering::Relaxed);

    fn child() {
        CHILD_FIRST_INSTRUCTION.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        CHILD_DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    // First sleep so we're in the "just-woken" state, mirroring the
    // mutex_holder test's flow (main spawns contender right after its
    // sleep(10) wakes).
    crate::kernel::sched::sleep(10);
    let spawn_at = crate::kernel::timer::elapsed_ms();
    SPAWN_TIME.store(spawn_at as usize, Ordering::Relaxed);
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(child);
    assert!(id.is_some(), "child spawn failed");

    // Wait for child to record its first instruction.
    let wait_start = crate::kernel::timer::elapsed_ms();
    while CHILD_DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - wait_start > 500 {
            panic!("child did not run");
        }
    }

    let spawn = SPAWN_TIME.load(Ordering::Relaxed);
    let first = CHILD_FIRST_INSTRUCTION.load(Ordering::Relaxed);
    let latency = first.saturating_sub(spawn);
    // The child should start running within one slice quantum (16 ms)
    // of being spawned. A larger latency means the spawner kept running
    // (or some other thread won the pick) for longer than expected.
    assert!(
        latency <= 16,
        "child took {} ms to start running after spawn (spawn @ {} ms, first instruction @ {} ms)",
        latency,
        spawn,
        first,
    );
}

/// Mutex hold-across-sleep WITHOUT the heuristic 10ms barrier: holder
/// signals a Completion once it has acquired the lock and started its
/// sleep, so the contender can be spawned with certainty rather than
/// hoping 10 ms is enough.
///
/// This is the same scenario as `mutex_holder_sleep_parks_contender`
/// but with explicit synchronization. If THIS test passes reliably
/// while the original flakes, the flake is in the "sleep(10) as
/// barrier" heuristic, not in the mutex itself.
///
/// Diagnostic timestamps mirror the sleep-variant test: on failure
/// the panic message shows every interesting transition so we can see
/// whether the latency was eaten by main's wake-from-wait, by the spawn
/// call, or by the contender's own first-instruction delay.
#[test_case]
fn mutex_holder_parks_contender_with_completion() {
    const HOLD_MS: u64 = 100;
    static M: crate::kernel::sync::Mutex<u32> = crate::kernel::sync::Mutex::new(0);
    static HOLDER_LOCKED: crate::kernel::sync::Completion = crate::kernel::sync::Completion::new();
    static T0: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_LOCKED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_SIGNALED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_SLEEP_STARTED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_SLEEP_ENDED_AT: AtomicUsize = AtomicUsize::new(0);
    static HOLDER_RELEASED_AT: AtomicUsize = AtomicUsize::new(0);
    static MAIN_WOKE_FROM_WAIT_AT: AtomicUsize = AtomicUsize::new(0);
    static MAIN_AFTER_SPAWN_AT: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_STARTED_AT: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_ACQUIRED_AT: AtomicUsize = AtomicUsize::new(0);
    static CONTENDER_DONE: AtomicUsize = AtomicUsize::new(0);

    *M.lock() = 0;
    for s in [
        &T0,
        &HOLDER_LOCKED_AT,
        &HOLDER_SIGNALED_AT,
        &HOLDER_SLEEP_STARTED_AT,
        &HOLDER_SLEEP_ENDED_AT,
        &HOLDER_RELEASED_AT,
        &MAIN_WOKE_FROM_WAIT_AT,
        &MAIN_AFTER_SPAWN_AT,
        &CONTENDER_STARTED_AT,
        &CONTENDER_ACQUIRED_AT,
        &CONTENDER_DONE,
    ] {
        s.store(0, Ordering::Relaxed);
    }
    // Safe: this test owns the completion, no waiter is parked at this point.
    unsafe { HOLDER_LOCKED.reset() };

    fn holder() {
        let mut g = M.lock();
        HOLDER_LOCKED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        *g = 1;
        HOLDER_LOCKED.signal();
        HOLDER_SIGNALED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        let sleep_start = crate::kernel::timer::elapsed_ms();
        HOLDER_SLEEP_STARTED_AT.store(sleep_start as usize, Ordering::Relaxed);
        crate::kernel::sched::sleep(HOLD_MS);
        HOLDER_SLEEP_ENDED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        *g = 2;
        HOLDER_RELEASED_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
    }

    fn contender() {
        let start = crate::kernel::timer::elapsed_ms();
        CONTENDER_STARTED_AT.store(start as usize, Ordering::Relaxed);
        let g = M.lock();
        let acquired = crate::kernel::timer::elapsed_ms();
        CONTENDER_ACQUIRED_AT.store(acquired as usize, Ordering::Relaxed);
        assert_eq!(*g, 2, "contender saw stale value");
        CONTENDER_DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    T0.store(
        crate::kernel::timer::elapsed_ms() as usize,
        Ordering::Relaxed,
    );
    let id1 = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(holder);
    assert!(id1.is_some(), "holder spawn failed");
    // Wait until the holder confirms it has the lock and is about to
    // sleep. No 10-ms heuristic — the completion is a hard barrier.
    HOLDER_LOCKED.wait();
    MAIN_WOKE_FROM_WAIT_AT.store(
        crate::kernel::timer::elapsed_ms() as usize,
        Ordering::Relaxed,
    );
    let id2 = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(contender);
    assert!(id2.is_some(), "contender spawn failed");
    MAIN_AFTER_SPAWN_AT.store(
        crate::kernel::timer::elapsed_ms() as usize,
        Ordering::Relaxed,
    );

    let wait_start = crate::kernel::timer::elapsed_ms();
    while CONTENDER_DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - wait_start > 1000 {
            panic!("contender did not acquire lock");
        }
    }

    let t0 = T0.load(Ordering::Relaxed);
    let hlocked = HOLDER_LOCKED_AT.load(Ordering::Relaxed);
    let hsig = HOLDER_SIGNALED_AT.load(Ordering::Relaxed);
    let hsleep_start = HOLDER_SLEEP_STARTED_AT.load(Ordering::Relaxed);
    let hsleep_end = HOLDER_SLEEP_ENDED_AT.load(Ordering::Relaxed);
    let hreleased = HOLDER_RELEASED_AT.load(Ordering::Relaxed);
    let mwoke = MAIN_WOKE_FROM_WAIT_AT.load(Ordering::Relaxed);
    let mspawn = MAIN_AFTER_SPAWN_AT.load(Ordering::Relaxed);
    let cstart = CONTENDER_STARTED_AT.load(Ordering::Relaxed);
    let cacq = CONTENDER_ACQUIRED_AT.load(Ordering::Relaxed);
    let latency = cacq - cstart;
    let holder_sleep = hsleep_end - hsleep_start;

    // With explicit sync, the contender was spawned WHILE the holder
    // already held the lock and was about to sleep for HOLD_MS. The
    // HOLD_MS - 25 lower bound accommodates main's wake-from-wait
    // being delayed up to ~15 ms by QEMU timer jitter (see QEMU
    // TIMER JITTER NOTE at top of file), plus a few ms for the
    // spawn call itself. Without that allowance, the contender's
    // "start" lands too late and the apparent latency drops below
    // the bound even though the mutex park/unpark is working
    // correctly.
    assert!(
        latency >= HOLD_MS as usize - 25,
        "contender acquired too quickly ({} ms < {} ms) — likely spinning instead of parking.\n  \
         T0={} ms (reference). All times below are ms-since-T0.\n  \
         holder_locked          @ +{} ms\n  \
         holder_signaled        @ +{} ms\n  \
         holder_sleep_started   @ +{} ms\n  \
         holder_sleep_ended     @ +{} ms  (sleep_duration={} ms, requested={} ms)\n  \
         holder_released        @ +{} ms\n  \
         main_woke_from_wait    @ +{} ms\n  \
         main_after_spawn       @ +{} ms\n  \
         contender_started      @ +{} ms\n  \
         contender_acquired     @ +{} ms",
        latency,
        HOLD_MS,
        t0,
        hlocked - t0,
        hsig - t0,
        hsleep_start - t0,
        hsleep_end - t0,
        holder_sleep,
        HOLD_MS,
        hreleased - t0,
        mwoke - t0,
        mspawn - t0,
        cstart - t0,
        cacq - t0,
    );
}

/// Directly measure how long a thread is delayed after waking from
/// sleep. The thread records the elapsed time of its own short sleep
/// and we assert that elapsed is close to requested.
///
/// The "wake-up delay" we are probing is: when a sleeping thread's
/// deadline fires, how long until it actually runs again? Under the
/// current stride scheduler, a high-`pass` thread that wakes alongside
/// a low-`pass` competitor (like the always-running partner) can be
/// passed over on the post-wake `pick_next_*` call and forced to
/// re-queue. This shows up as `sleep(N)` returning at N+δ rather than
/// at N exactly.
///
/// We use sleep(10) deliberately: a short sleep magnifies the issue
/// since δ is roughly bounded by the scheduling delay, independent of
/// the sleep duration. Failure surfaces the wake-delay directly with
/// a precise number rather than as a knock-on effect in another test.
#[test_case]
fn sleep10_wakes_promptly_under_partner_load() {
    const N_SAMPLES: usize = 10;
    static SAMPLES: [AtomicUsize; N_SAMPLES] = [const { AtomicUsize::new(0) }; N_SAMPLES];
    static DONE: AtomicUsize = AtomicUsize::new(0);

    for s in SAMPLES.iter() {
        s.store(0, Ordering::Relaxed);
    }
    DONE.store(0, Ordering::Relaxed);

    fn sleeper() {
        for i in 0..N_SAMPLES {
            let start = crate::kernel::timer::elapsed_ms();
            crate::kernel::sched::sleep(10);
            let elapsed = crate::kernel::timer::elapsed_ms() - start;
            SAMPLES[i].store(elapsed as usize, Ordering::Relaxed);
        }
        DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(sleeper);
    assert!(id.is_some(), "sleeper spawn failed");

    let wait_start = crate::kernel::timer::elapsed_ms();
    while DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(50);
        if crate::kernel::timer::elapsed_ms() - wait_start > 1000 {
            panic!("sleeper did not finish");
        }
    }

    let mut worst: usize = 0;
    let mut sum: usize = 0;
    for s in SAMPLES.iter() {
        let v = s.load(Ordering::Relaxed);
        if v > worst {
            worst = v;
        }
        sum += v;
    }
    let avg = sum / N_SAMPLES;
    // Bound (35 ms = sleep + ~25 ms QEMU timer jitter) catches
    // egregious regressions while tolerating QEMU's wfi/timer
    // delivery jitter. See QEMU TIMER JITTER NOTE at top of file
    // for the investigation. On real RP2350 hardware this bound can
    // be tightened to ~15 ms (one slice quantum + slack).
    assert!(
        worst <= 35,
        "worst sleep(10) over {} samples was {} ms (avg {} ms) — wake-from-sleep is delayed beyond one slice quantum.\n  \
         samples (ms): [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}]",
        N_SAMPLES,
        worst,
        avg,
        SAMPLES[0].load(Ordering::Relaxed),
        SAMPLES[1].load(Ordering::Relaxed),
        SAMPLES[2].load(Ordering::Relaxed),
        SAMPLES[3].load(Ordering::Relaxed),
        SAMPLES[4].load(Ordering::Relaxed),
        SAMPLES[5].load(Ordering::Relaxed),
        SAMPLES[6].load(Ordering::Relaxed),
        SAMPLES[7].load(Ordering::Relaxed),
        SAMPLES[8].load(Ordering::Relaxed),
        SAMPLES[9].load(Ordering::Relaxed),
    );
}

/// Mirror of `sleep10_wakes_promptly_under_partner_load` but for the
/// blocked→ready transition: a thread parks on a Completion, another
/// thread signals it, and we measure how long the wait() call took. A
/// signal that happens "very soon after" the wait should return
/// quickly; if instead the woken thread is passed over in favor of the
/// low-`pass` partner, the wait returns several ms later than the
/// signal fired.
#[test_case]
fn completion_wait_wakes_promptly_under_partner_load() {
    static C: crate::kernel::sync::Completion = crate::kernel::sync::Completion::new();
    static SIGNAL_AT: AtomicUsize = AtomicUsize::new(0);
    static WAKE_AT: AtomicUsize = AtomicUsize::new(0);
    static DONE: AtomicUsize = AtomicUsize::new(0);

    SIGNAL_AT.store(0, Ordering::Relaxed);
    WAKE_AT.store(0, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);
    unsafe { C.reset() };

    fn waiter() {
        C.wait();
        WAKE_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        DONE.store(1, Ordering::Relaxed);
    }

    fn signaler() {
        // Sleep long enough for the waiter to definitely park.
        crate::kernel::sched::sleep(30);
        SIGNAL_AT.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        C.signal();
    }

    ensure_partner_spawned();
    let id1 = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(waiter);
    assert!(id1.is_some(), "waiter spawn failed");
    let id2 = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(signaler);
    assert!(id2.is_some(), "signaler spawn failed");

    let wait_start = crate::kernel::timer::elapsed_ms();
    while DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(20);
        if crate::kernel::timer::elapsed_ms() - wait_start > 1000 {
            panic!("waiter did not wake");
        }
    }

    let signal_at = SIGNAL_AT.load(Ordering::Relaxed);
    let wake_at = WAKE_AT.load(Ordering::Relaxed);
    let delay = wake_at.saturating_sub(signal_at);
    // The waiter parked first, signal fires later. The delay from
    // signal to the waiter actually resuming should be sub-millisecond
    // ideally; bound (40 ms) accommodates QEMU's wfi/timer-delivery
    // jitter (see QEMU TIMER JITTER NOTE at top of file). On real
    // RP2350 hardware this bound can be tightened to ~5-10 ms. A
    // larger delay here indicates either a real wake-path bug or
    // the woken thread being passed over by the scheduler.
    assert!(
        delay <= 40,
        "wake-from-completion delay was {} ms (signal @ {} ms, woken @ {} ms) — \
         scheduler is not promptly picking the just-unparked thread",
        delay,
        signal_at,
        wake_at,
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
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(child);
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
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(child);
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

    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .spawn(child_two_waits);
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

// =============================================================================
// Hart affinity tests
// =============================================================================
//
// These tests run after the IPI plumbing landed: a Builder with affinity
// targets a hart, spawn fires an IPI when that hart isn't current, the
// remote hart's trap handler clears MSIP and runs preempt(), and
// preempt's pick_next_*_mut filters TCBs by affinity. The end-to-end
// observable behaviour is: a thread spawned with `with_affinity(N)`
// records `cpu_id() == N` when it runs.
//
// Tests run on HART0 (test runner). Spawning an affinity-1 thread from
// here must wake HART1 (currently in idle_thread / wfi) via IPI for the
// test to terminate within the timeout — so a passing affinity-1 test
// transitively verifies the IPI delivery path.

/// Affinity 0: a thread pinned to HART0 should be picked by HART0's
/// scheduler and `cpu_id()` from inside it should return 0. Sanity
/// check for the affinity-filter path on the local hart (no IPI
/// involved here).
#[test_case]
fn affinity_hart0_runs_on_hart0() {
    static CPU_OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);
    static DONE: AtomicUsize = AtomicUsize::new(0);
    static START_MS: AtomicUsize = AtomicUsize::new(usize::MAX);

    CPU_OBSERVED.store(usize::MAX, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);
    START_MS.store(usize::MAX, Ordering::Relaxed);

    fn pinned_hart0() {
        START_MS.store(
            crate::kernel::timer::elapsed_ms() as usize,
            Ordering::Relaxed,
        );
        CPU_OBSERVED.store(crate::arch::cpu_id(), Ordering::Relaxed);
        DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let test_runner_hart_before = crate::arch::cpu_id();
    let spawn_ms = crate::kernel::timer::elapsed_ms();
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .with_affinity(0)
        .spawn(pinned_hart0);
    assert!(id.is_some(), "spawn failed");
    let test_runner_hart_after = crate::arch::cpu_id();

    let wait_start = crate::kernel::timer::elapsed_ms();
    while DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - wait_start > 500 {
            use crate::io::DirectWriter;
            use core::fmt::Write;
            let _ = writeln!(
                DirectWriter,
                "\n[affinity-0 timeout] spawn_ms={} wait_start_ms={} now_ms={} \
                 test_runner_hart_before_spawn={} test_runner_hart_after_spawn={} \
                 test_runner_hart_now={} pinned_start_ms={} \
                 pinned_cpu_observed={} DONE={}",
                spawn_ms,
                wait_start,
                crate::kernel::timer::elapsed_ms(),
                test_runner_hart_before,
                test_runner_hart_after,
                crate::arch::cpu_id(),
                START_MS.load(Ordering::Relaxed),
                CPU_OBSERVED.load(Ordering::Relaxed),
                DONE.load(Ordering::Relaxed),
            );
            panic!("affinity-0 thread did not complete within 500 ms");
        }
    }

    let observed = CPU_OBSERVED.load(Ordering::Relaxed);
    assert_eq!(
        observed, 0,
        "affinity-0 thread ran on hart {}, expected hart 0",
        observed
    );
}

/// Affinity 1: spawn from HART0 with affinity for HART1. Verifies the
/// full cross-hart wake-up chain: spawn fires an IPI; HART1 (in wfi)
/// takes a software-interrupt trap; trap handler clears MSIP and runs
/// preempt; affinity filter accepts the new thread on HART1; switch
/// happens. Inside the thread `cpu_id() == 1` proves it landed on the
/// right hart. If the thread never completes, the IPI delivery or
/// trap-handler arm is broken.
#[test_case]
fn affinity_hart1_runs_on_hart1() {
    static CPU_OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);
    static DONE: AtomicUsize = AtomicUsize::new(0);

    CPU_OBSERVED.store(usize::MAX, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);

    fn pinned_hart1() {
        CPU_OBSERVED.store(crate::arch::cpu_id(), Ordering::Relaxed);
        DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .with_affinity(1)
        .spawn(pinned_hart1);
    assert!(id.is_some(), "spawn failed");

    let wait_start = crate::kernel::timer::elapsed_ms();
    while DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - wait_start > 500 {
            panic!(
                "affinity-1 thread did not complete within 500 ms — \
                 likely IPI delivery (set_msip / SOFTWARE arm / clear_msip) is broken"
            );
        }
    }

    let observed = CPU_OBSERVED.load(Ordering::Relaxed);
    assert_eq!(
        observed, 1,
        "affinity-1 thread ran on hart {}, expected hart 1",
        observed
    );
}

/// Cross-hart unpark via IPI: an affinity-1 thread parks on a
/// Completion. From HART0 we signal the completion, which calls
/// `unpark`, which (because the woken thread's affinity is 1) fires
/// an IPI to HART1. HART1's preempt then picks the now-Ready thread.
///
/// This exercises a different code path than `affinity_hart1_runs_on_hart1`
/// (which sends the IPI from `spawn`). Failure mode: the thread never
/// completes because unpark didn't IPI HART1.
#[test_case]
fn affinity_unpark_wakes_via_ipi() {
    use crate::kernel::sync::Completion;
    static C: Completion = Completion::new();
    static CPU_OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);
    static DONE: AtomicUsize = AtomicUsize::new(0);

    // Safe: this test owns the completion, no parked waiter yet.
    unsafe { C.reset() };
    CPU_OBSERVED.store(usize::MAX, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);

    fn waiter_on_hart1() {
        C.wait();
        CPU_OBSERVED.store(crate::arch::cpu_id(), Ordering::Relaxed);
        DONE.store(1, Ordering::Relaxed);
    }

    ensure_partner_spawned();
    let id = crate::kernel::sched::Builder::new()
        .with_stack_class(StackClass::KB2)
        .with_affinity(1)
        .spawn(waiter_on_hart1);
    assert!(id.is_some(), "spawn failed");

    // Give the waiter time to actually park before we signal. (If we
    // signal before it parks, the Completion's pending flag is set and
    // the wait returns without parking — IPI path not exercised.)
    crate::kernel::sched::sleep(50);
    assert_eq!(
        DONE.load(Ordering::Relaxed),
        0,
        "waiter completed before signal — did it actually park?"
    );

    C.signal();

    let wait_start = crate::kernel::timer::elapsed_ms();
    while DONE.load(Ordering::Relaxed) == 0 {
        crate::kernel::sched::sleep(10);
        if crate::kernel::timer::elapsed_ms() - wait_start > 500 {
            panic!(
                "affinity-1 waiter did not wake within 500 ms after signal — \
                 likely unpark didn't fire an IPI to HART1"
            );
        }
    }

    let observed = CPU_OBSERVED.load(Ordering::Relaxed);
    assert_eq!(
        observed, 1,
        "affinity-1 waiter ran on hart {} after wake, expected hart 1",
        observed
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
