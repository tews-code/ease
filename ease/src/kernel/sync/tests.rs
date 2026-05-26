// Host-runnable tests for SpinLock. Run with
//     cargo test --lib --target $HOST_TARGET
// and verified clean under Miri:
//     cargo +nightly miri test --lib --target $HOST_TARGET
//
// The headline test (`spinlock_concurrent_counter`) spawns multiple
// threads that each take the lock and increment a shared counter. Miri
// verifies that the Acquire/Release ordering on the inner AtomicBool
// correctly synchronises the data access — if the orderings were too
// weak (e.g. Relaxed in place of Acquire on the load), Miri would
// report a data race on the counter.
//
// IrqSpinLock is not tested here because its `disable_interrupts` /
// `restore_interrupts` calls are stubbed out in the lib crate's
// `mod arch` (they're no-ops on the host). The interesting algorithmic
// behaviour — the AtomicBool spin loop, the guard semantics — is
// shared with SpinLock, so testing SpinLock here covers it.
//
// Gated on `not(target_os = "none")` so the kernel build is unaffected.
mod host_tests {
    use std::sync::Arc;
    use std::thread;
    use std::vec::Vec;

    use super::super::{CounterU64, SpinLock};

    #[test]
    fn spinlock_basic_lock_unlock() {
        let lock = SpinLock::new(42u32);
        {
            let guard = lock.lock();
            assert_eq!(*guard, 42);
        }
        // After the first guard drops, the lock can be acquired again.
        let guard2 = lock.lock();
        assert_eq!(*guard2, 42);
    }

    #[test]
    fn spinlock_mutation_through_deref_mut() {
        let lock = SpinLock::new(0u32);
        {
            let mut guard = lock.lock();
            *guard = 100;
            *guard += 1;
        }
        let guard = lock.lock();
        assert_eq!(*guard, 101);
    }

    #[test]
    fn spinlock_try_lock_when_locked_returns_none() {
        let lock = SpinLock::new(7u32);
        let _guard = lock.lock();
        // While `_guard` is alive, try_lock must fail. The CAS sees
        // `locked = true` and returns Err — no spurious-failure window
        // because the actual stored value is `true`, not `false`.
        assert!(
            lock.try_lock().is_none(),
            "try_lock should fail while another guard is held"
        );
    }

    #[test]
    fn spinlock_drop_releases_lock() {
        let lock = SpinLock::new(0u32);
        {
            let _guard = lock.lock();
            assert!(
                lock.try_lock().is_none(),
                "lock should be held inside scope"
            );
        }
        // Once the guard's scope ends, the lock is released and a new
        // lock() call must succeed. We use lock() here rather than
        // try_lock() because compare_exchange_weak inside try_lock can
        // spuriously fail; lock() retries until success so it's a
        // reliable post-condition.
        let guard = lock.lock();
        assert_eq!(*guard, 0);
    }

    #[test]
    fn spinlock_concurrent_counter() {
        // Headline Miri test: N threads each increment a shared counter
        // K times via the lock. The total must equal N*K. Miri checks
        // the Acquire/Release ordering on the inner AtomicBool against
        // the actual reads and writes of the counter through the guard.
        //
        // What this catches if the lock were buggy:
        //   - Wrong memory ordering (e.g. Relaxed on the CAS) → data race
        //     on the counter, reported by Miri
        //   - Forgetting to release on guard drop → deadlock under Miri
        //   - Releasing before the data write completes → stale-write race
        //
        // Thread/iteration counts are kept small so Miri finishes quickly.
        const THREADS: usize = 4;
        const ITERS: u32 = 50;
        let lock: Arc<SpinLock<u32>> = Arc::new(SpinLock::new(0));
        let mut handles: Vec<thread::JoinHandle<()>> = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let lock = Arc::clone(&lock);
            handles.push(thread::spawn(move || {
                for _ in 0..ITERS {
                    *lock.lock() += 1;
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }
        let final_value = *lock.lock();
        assert_eq!(final_value, (THREADS as u32) * ITERS);
    }

    // ---- CounterU64 tests ----------------------------------------------------
    //
    // These tests run against whichever CounterU64 impl the host has. On a
    // typical 64-bit host they exercise the AtomicU64-backed thin wrapper.
    // The kernel-target Lamport-pair / seqlock implementation can't be directly
    // tested from host tests (it's cfg-gated out on hosts with native 64-bit
    // atomics). The API contract is the same on both, so these tests still
    // serve as a behavioural specification.

    #[test]
    fn counter_new_is_zero() {
        let c = CounterU64::new();
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn counter_add_returns_old_value() {
        let c = CounterU64::new();
        let old = unsafe { c.add(10) };
        assert_eq!(old, 0, "first add: old should be 0");

        let old = unsafe { c.add(5) };
        assert_eq!(old, 10, "second add: old should be 10");

        assert_eq!(c.get(), 15, "final value should be 15");
    }

    #[test]
    fn counter_reset_clears_to_zero() {
        let c = CounterU64::new();
        unsafe { c.add(42) };
        assert_eq!(c.get(), 42);
        unsafe { c.reset() };
        assert_eq!(c.get(), 0, "reset should bring counter to 0");
    }

    #[test]
    fn counter_wraps_low_into_high_half() {
        // Verify the carry behaviour: when low overflows, high increments.
        let c = CounterU64::new();
        // First add: a value just below 2^32.
        unsafe { c.add(u32::MAX as u64 - 5) };
        assert_eq!(c.get(), u32::MAX as u64 - 5);
        // Second add: 10 cycles, which crosses the 2^32 boundary.
        unsafe { c.add(10) };
        // Expected: (u32::MAX - 5) + 10 = u32::MAX + 5 = (1 << 32) + 4
        assert_eq!(c.get(), (1u64 << 32) + 4);
    }

    #[test]
    fn counter_handles_large_increments() {
        // Large single increment that has both high and low components.
        let c = CounterU64::new();
        let v = (3u64 << 32) | 0x12345678;
        let old = unsafe { c.add(v) };
        assert_eq!(old, 0);
        assert_eq!(c.get(), v);
    }

    #[test]
    fn counter_concurrent_readers_observe_monotonic() {
        // Single writer, multiple readers. The CounterU64 contract is
        // single-writer (enforced by the assertion in the kernel impl). With
        // many readers, every read must observe a value >= the previous read
        // — i.e., no tearing into a smaller intermediate value, no torn
        // composition of high/low halves.
        //
        // Under Miri, this also validates the Acquire/Release ordering of
        // the seq increments (or the underlying AtomicU64 on this host).
        //
        // What this catches if the impl were buggy:
        //   - Wrong memory ordering on the seq atomic → a reader could see a
        //     partial state and report a non-monotonic value.
        //   - Lamport-pair without seqlock on multi-core → "old hi + new lo"
        //     produces a value 2^32 too large, breaking monotonicity in the
        //     opposite direction.
        //   - Forgetting to close the seqlock → readers loop forever.
        const READERS: usize = 4;
        const WRITES: u64 = 1000;

        let counter: Arc<CounterU64> = Arc::new(CounterU64::new());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut reader_handles: Vec<thread::JoinHandle<()>> = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            let counter = Arc::clone(&counter);
            let stop = Arc::clone(&stop);
            reader_handles.push(thread::spawn(move || {
                let mut last: u64 = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let now = counter.get();
                    assert!(
                        now >= last,
                        "non-monotonic read: now={} < last={}",
                        now,
                        last
                    );
                    last = now;
                }
            }));
        }

        // Single writer - increments by varying amounts to exercise both
        // halves of the counter and the carry path.
        for i in 0..WRITES {
            // Mix: some +1, some larger; occasional crossings of 2^32 are
            // unlikely but not relevant — the test covers monotonic behaviour
            // at all times, not specifically the wrap.
            unsafe { counter.add((i % 7) + 1) };
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for h in reader_handles {
            h.join().expect("reader thread panicked");
        }

        // Final value: sum of (i % 7) + 1 for i in 0..WRITES.
        let expected: u64 = (0..WRITES).map(|i| (i % 7) + 1).sum();
        assert_eq!(counter.get(), expected);
    }
}
