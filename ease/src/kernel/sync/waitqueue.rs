//! Wait Queue
//!
//! When multiple threads are interested in a state of some part of the OS, they can
//! register on a wait queue and go to sleep instead of busy-waiting. For example,
//! one thread may be waiting for a queue to empty so that it can enable low-power mode,
//! while another thread is waiting on that same queue for when a slot is free. Both
//! threads sleep until the chosen state is reached, at which point all the registered threads
//! are woken (we do not support wake-one) and evaluate the state to determine if it is the
//! right state for them. If it is not, then they go back to sleep.
//!
//! In order to support threads registering on a wait queues there is a wait queue which
//! holds an array of `Option<ThreadHandle>`. In EASE there are only `THREADS_MAX` threads so a simple array
//! is used for the queue. For a thread to register its interest it puts its thread handle into the array.
//! To deregister it clears that same slot. We use thread handles (rather than thread TCB index) to
//! avoid stale slots triggering against new threads.
//!
//! When a thread is woken from the queue, it is scheduled and immediately loops on a closure. The
//! closure is that thread's specific requirement on the event (e.g. is empty, or has one slot free).
//! If the closure evalutes true it breaks the loop and continues, otherwise it goes back to sleep.
//! The closure also takes a mut ref to the part of the OS that is being waited on, to ensure the
//! closure is run with the appropriate locks held.
//!
//! User threads may need to be interrupted (due to a fault), so an interruptible version of the wait
//! method is also provided, which returns an `Err(Interrupted)` if interrupted.
//!
//! The wait queue is one array indexed by thread, one inner lock, usable directly as a static.
//!
//! The closure with the resource lock held and must not block, sleep, or take the scheduler lock.

use super::Interrupted;
use crate::kernel::sched::{self, THREADS_MAX, ThreadHandle};
use crate::kernel::sync::{IrqSpinLock, SpinLock, SpinLockGuard};

struct WaitQueueInner([Option<ThreadHandle>; THREADS_MAX]);

impl WaitQueueInner {
    /// New is const
    const fn new() -> Self {
        Self([const { None }; THREADS_MAX])
    }
    /// Register a thread in the wait queue. This is idempotent and does not check existing state
    ///
    /// # Panics #
    /// Panics if the thread handle is invalid
    fn register(&mut self, handle: ThreadHandle) {
        assert!(handle.idx < THREADS_MAX);
        self.0[handle.idx] = Some(handle);
    }
    /// Deregister a thread from the wait queue. This is idempotent and does not check existing state
    ///
    /// # Panics #
    /// Panics if the thread handle is invalid
    fn deregister(&mut self, handle: ThreadHandle) {
        assert!(handle.idx < THREADS_MAX);
        self.0[handle.idx] = None;
    }
    /// Drain the queue
    fn drain(&mut self) -> [Option<ThreadHandle>; THREADS_MAX] {
        core::mem::replace(&mut self.0, [const { None }; THREADS_MAX])
    }
}

pub(crate) struct WaitQueue {
    inner: IrqSpinLock<WaitQueueInner>,
}

impl WaitQueue {
    /// New in `const` to support static
    pub(crate) const fn new() -> Self {
        Self {
            inner: IrqSpinLock::new(WaitQueueInner::new()),
        }
    }
    /// Use for kernel threads that are never exited.
    /// For user threads that may fault use [Self::wait_with_interruptible].
    ///
    /// The first argument is a spinlock on the resource of interest.
    /// The second argument is a closure what examines the resource state
    /// and returns `true` if the state matches the wait criteria, or `false`
    /// if they do not match and the thread can go back to sleep.
    ///
    /// Lock order is (1) Resource (2) WaitQueue (3) Scheduler
    pub(crate) fn wait_with<'a, F, R>(
        &self,
        resource: &'a SpinLock<R>,
        f: F,
    ) -> SpinLockGuard<'a, R>
    where
        F: FnMut(&mut R) -> bool,
    {
        self.wait_with_interruptible(resource, f)
            .expect("kernel thread should never be marked for exit")
    }
    /// Adds the current thread to the wait queue with a closure to evalute the resource state
    /// on wakeup. Can be used with `static` wait queues.
    ///
    /// This form should be used for user threads, which may need to be torn down. For kernel
    /// threads use [Self::wait_with].
    ///
    /// Lock order is (1) Resource (2) WaitQueue (3) Scheduler
    pub(crate) fn wait_with_interruptible<'a, F, R>(
        &self,
        resource: &'a SpinLock<R>,
        mut f: F,
    ) -> Result<SpinLockGuard<'a, R>, Interrupted>
    where
        F: FnMut(&mut R) -> bool,
    {
        loop {
            let current_handle = sched::current_thread();
            // Take the wait queue lock to ensure next steps are never split by an interrupt
            let mut wait_queue = self.inner.lock();
            // First register this thread
            wait_queue.register(current_handle);
            // Mark self as blocked. Note - if the thread is already condemned it will not block
            // but instead be kept in Running state
            sched::set_self_blocked();
            // Drop wait queue lock in case another running thread is starting the
            // same process and takes the resource lock and then the wait queue lock
            // while we still hold them in reverse order
            drop(wait_queue);
            // Evaluate the closure to see if the criteria match
            //
            let mut guard = resource.lock();
            if f(&mut *guard) {
                // We meet the criteria so revert to Running to execute immediately
                self.inner.lock().deregister(current_handle);
                sched::set_self_running();
                // Return _with the machinery lock still held_
                return Ok(guard);
            }
            // We can sleep
            drop(guard);
            sched::park_if_blocked();
            // Check if we have been interrupted, in which case exit with error
            if sched::current_user_thread_needs_exit() {
                // Clear the slot
                self.inner.lock().deregister(current_handle);
                return Err(Interrupted);
            }
        }
    }
    /// Wake all threads that are sleeping on this resource
    ///
    /// Lock order is (1) WaitQueue (2) Scheduler
    pub(crate) fn wake_all(&self) {
        let current_queue = self.inner.lock().drain();
        for handle in current_queue.iter().flatten() {
            sched::unpark(handle);
        }
    }
}

// QEMU tests: the wait queue is pure scheduler interaction (park, unpark,
// state transitions), so there is no host-runnable subset. Gated on the
// scheduler feature because every case spawns kernel threads. Each test
// declares its own statics so state never leaks between cases.
#[cfg(all(test, target_os = "none", feature = "test-sched"))]
mod tests {
    use super::*;
    use crate::kernel::alloc::Order;
    use crate::kernel::sched::{Builder, sleep};
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Poll `cond` every 10 ms for up to `limit_ms`. Returns whether it
    /// became true.
    fn wait_until(limit_ms: u64, cond: impl Fn() -> bool) -> bool {
        let mut waited = 0;
        while !cond() {
            if waited >= limit_ms {
                return false;
            }
            sleep(10);
            waited += 10;
        }
        true
    }

    fn spawn(entry: fn()) {
        assert!(
            Builder::new()
                .with_stack_class(Order::KB2)
                .spawn(entry)
                .is_some(),
            "spawn failed (no free TCB?)"
        );
    }

    // Two waiters with different conditions on one resource. One wake_all
    // must wake both; the one whose condition is now true proceeds, the
    // other must re-evaluate and go back to sleep, then proceed on the
    // second wake. This is the "wake all, each re-checks" contract.
    #[test_case]
    fn waitqueue_wake_all_wakes_every_waiter_each_rechecks() {
        static WQ: WaitQueue = WaitQueue::new();
        static R: SpinLock<u32> = SpinLock::new(0);
        static A_DONE: AtomicUsize = AtomicUsize::new(0);
        static B_DONE: AtomicUsize = AtomicUsize::new(0);
        static B_EVALS: AtomicUsize = AtomicUsize::new(0);

        fn waiter_a() {
            let g = WQ.wait_with(&R, |r| *r >= 1);
            drop(g);
            A_DONE.store(1, Ordering::Release);
        }
        fn waiter_b() {
            let g = WQ.wait_with(&R, |r| {
                B_EVALS.fetch_add(1, Ordering::AcqRel);
                *r >= 2
            });
            drop(g);
            B_DONE.store(1, Ordering::Release);
        }

        spawn(waiter_a);
        spawn(waiter_b);
        // Both evaluate once (false) and park.
        assert!(
            wait_until(500, || B_EVALS.load(Ordering::Acquire) == 1),
            "waiter B never evaluated its condition"
        );
        sleep(30);
        assert_eq!(A_DONE.load(Ordering::Acquire), 0, "A ran before any wake");

        *R.lock() = 1;
        WQ.wake_all();
        assert!(
            wait_until(500, || A_DONE.load(Ordering::Acquire) == 1),
            "A not woken"
        );
        // B was woken too: it must have re-evaluated, found false, re-parked.
        assert!(
            wait_until(500, || B_EVALS.load(Ordering::Acquire) == 2),
            "B was not woken by wake_all"
        );
        sleep(30);
        assert_eq!(
            B_DONE.load(Ordering::Acquire),
            0,
            "B proceeded on a false condition"
        );

        *R.lock() = 2;
        WQ.wake_all();
        assert!(
            wait_until(500, || B_DONE.load(Ordering::Acquire) == 1),
            "B not woken"
        );
        assert_eq!(B_EVALS.load(Ordering::Acquire), 3);
    }

    // Condition already true on entry: wait_with must return without
    // parking, and without needing any wake_all.
    #[test_case]
    fn waitqueue_fast_path_when_condition_already_holds() {
        static WQ: WaitQueue = WaitQueue::new();
        static R: SpinLock<u32> = SpinLock::new(7);
        static DONE: AtomicUsize = AtomicUsize::new(0);

        fn waiter() {
            let g = WQ.wait_with(&R, |r| *r == 7);
            drop(g);
            DONE.store(1, Ordering::Release);
        }

        spawn(waiter);
        assert!(
            wait_until(200, || DONE.load(Ordering::Acquire) == 1),
            "fast path parked instead of returning"
        );
        // Nothing should be left registered: a wake_all now is a no-op
        // and must not disturb anything.
        WQ.wake_all();
    }

    // The guard handed back is the live resource lock: a write through it
    // is visible to the waker afterwards, and the lock is released when
    // the guard drops (so the parent can take it again).
    #[test_case]
    fn waitqueue_returns_resource_lock_held() {
        static WQ: WaitQueue = WaitQueue::new();
        static R: SpinLock<u32> = SpinLock::new(0);
        static DONE: AtomicUsize = AtomicUsize::new(0);

        fn waiter() {
            let mut g = WQ.wait_with(&R, |r| *r == 1);
            *g = 42;
            drop(g);
            DONE.store(1, Ordering::Release);
        }

        spawn(waiter);
        sleep(30);
        *R.lock() = 1;
        WQ.wake_all();
        assert!(
            wait_until(500, || DONE.load(Ordering::Acquire) == 1),
            "waiter not woken"
        );
        assert_eq!(*R.lock(), 42, "write through the returned guard was lost");
    }

    // wake_all on a queue with no waiters must be harmless, and the queue
    // must still work for a waiter that arrives afterwards.
    #[test_case]
    fn waitqueue_wake_all_on_empty_queue_is_noop() {
        static WQ: WaitQueue = WaitQueue::new();
        static R: SpinLock<u32> = SpinLock::new(0);
        static DONE: AtomicUsize = AtomicUsize::new(0);

        WQ.wake_all();
        WQ.wake_all();

        fn waiter() {
            let g = WQ.wait_with(&R, |r| *r == 1);
            drop(g);
            DONE.store(1, Ordering::Release);
        }
        spawn(waiter);
        sleep(30);
        assert_eq!(
            DONE.load(Ordering::Acquire),
            0,
            "stale wake let the waiter through"
        );
        *R.lock() = 1;
        WQ.wake_all();
        assert!(
            wait_until(500, || DONE.load(Ordering::Acquire) == 1),
            "waiter not woken"
        );
    }

    // Lost-wakeup stress: a waker bumps the counter and wakes, over and
    // over with no coordination, while the waiter waits for each value in
    // turn. If a wake could slip between the waiter's check and its park,
    // the waiter would hang on some round and the bound below would fire.
    #[test_case]
    fn waitqueue_no_lost_wakeup_under_rapid_wakes() {
        const ROUNDS: u32 = 200;
        static WQ: WaitQueue = WaitQueue::new();
        static R: SpinLock<u32> = SpinLock::new(0);
        static DONE: AtomicUsize = AtomicUsize::new(0);

        fn waiter() {
            for target in 1..=ROUNDS {
                let g = WQ.wait_with(&R, |r| *r >= target);
                drop(g);
            }
            DONE.store(1, Ordering::Release);
        }
        fn waker() {
            for _ in 0..ROUNDS {
                *R.lock() += 1;
                WQ.wake_all();
                // Yield so the waiter gets a chance to park between wakes
                // on a single hart; on two harts this interleaves freely.
                crate::kernel::sched::yield_now();
            }
        }

        spawn(waiter);
        spawn(waker);
        let finished = wait_until(1500, || DONE.load(Ordering::Acquire) == 1);
        assert!(
            finished,
            "waiter hung: a wakeup was lost (counter = {})",
            *R.lock()
        );
    }
}

// Benchmark: the cost of waking a full wait queue. wake_all is a plain
// start-to-end walk of the array with one unpark per registered thread,
// so with every spare TCB slot parked on one queue this measures the
// unpark loop itself, and separately the serial chain that follows on
// one hart: each woken waiter runs, evaluates its closure, finds it false
// and parks again, until the LAST one to run finds the round complete.
// Everything is pinned to hart 0 so no IPIs or cross-hart wakeups are
// inside the bracket. Waiters exit at the end so their slots go back.
#[cfg(all(test, feature = "bench"))]
mod bench {
    use super::*;
    use crate::arch::csr::rdcycles;
    use crate::kernel::alloc::Order;
    use crate::kernel::sched::{Builder, sleep, yield_now};
    use crate::kernel::timer;
    use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    const RUNS: usize = 20;

    struct Round {
        armed: bool,
        unarmed_evals: usize, // closure runs before the first round: one per waiter parking
        evals: usize,         // closure runs this round: the n-th one is the last waiter to run
        end: u64,             // rdcycles at that n-th evaluation
        quit: bool,
    }
    static ROUND: SpinLock<Round> = SpinLock::new(Round {
        armed: false,
        unarmed_evals: 0,
        evals: 0,
        end: 0,
        quit: false,
    });
    static QUEUE: WaitQueue = WaitQueue::new();
    static WAITERS: AtomicUsize = AtomicUsize::new(0);
    static START: AtomicUsize = AtomicUsize::new(0);
    static ROUND_DONE: AtomicUsize = AtomicUsize::new(0);
    static EXITED: AtomicUsize = AtomicUsize::new(0);
    static WAKER_DONE: AtomicUsize = AtomicUsize::new(0);
    static WAKE_ALL_MIN: AtomicU32 = AtomicU32::new(u32::MAX);
    static CHAIN_MIN: AtomicU32 = AtomicU32::new(u32::MAX);

    /// riscv32 has no 64-bit atomics; anything over u32::MAX saturates.
    fn clamp(v: u64) -> u32 {
        v.min(u32::MAX as u64) as u32
    }

    fn waiter() {
        loop {
            let guard = QUEUE.wait_with(&ROUND, |r| {
                if r.quit {
                    return true;
                }
                if !r.armed {
                    r.unarmed_evals += 1;
                    return false;
                }
                r.evals += 1;
                if r.evals == WAITERS.load(Ordering::Relaxed) {
                    r.end = rdcycles();
                    true
                } else {
                    false
                }
            });
            let quit = guard.quit;
            drop(guard);
            if quit {
                break;
            }
            ROUND_DONE.store(1, Ordering::Release);
        }
        EXITED.fetch_add(1, Ordering::Release);
    }

    fn waker() {
        while START.load(Ordering::Acquire) == 0 {
            sleep(1);
        }
        let n = WAITERS.load(Ordering::Relaxed);
        // Every waiter has run its closure once (unarmed) and is parking.
        while ROUND.lock().unarmed_evals < n {
            yield_now();
        }
        sleep(1);

        let mut best_wake_all = u64::MAX;
        let mut best_chain = u64::MAX;
        for _ in 0..RUNS {
            {
                let mut r = ROUND.lock();
                r.armed = true;
                r.evals = 0;
                r.end = 0;
            }
            ROUND_DONE.store(0, Ordering::Relaxed);
            let t0 = rdcycles();
            QUEUE.wake_all();
            let t1 = rdcycles();
            while ROUND_DONE.load(Ordering::Acquire) == 0 {
                yield_now();
            }
            let end = ROUND.lock().end;
            best_wake_all = best_wake_all.min(t1 - t0);
            best_chain = best_chain.min(end - t0);
            // The last waiter re-registers by evaluating once more (evals
            // goes past n); give it a moment to actually park before the
            // next round's wake_all.
            while ROUND.lock().evals <= n {
                yield_now();
            }
            sleep(1);
        }
        WAKE_ALL_MIN.store(clamp(best_wake_all), Ordering::Relaxed);
        CHAIN_MIN.store(clamp(best_chain), Ordering::Relaxed);

        // Release the waiters so their slots return to the pool.
        ROUND.lock().quit = true;
        QUEUE.wake_all();
        WAKER_DONE.store(1, Ordering::Release);
    }

    /// No assertion; numbers are informational.
    #[test_case]
    fn waitqueue_benchmarks() {
        // The waker takes a slot first, then parks on START while the
        // waiters fill every remaining slot.
        let w = Builder::new()
            .with_stack_class(Order::KB4)
            .with_affinity(0)
            .spawn(waker);
        assert!(w.is_some(), "waker spawn failed");
        let mut n = 0;
        while Builder::new()
            .with_stack_class(Order::KB2)
            .with_affinity(0)
            .spawn(waiter)
            .is_some()
        {
            n += 1;
        }
        assert!(n >= 2, "need at least two waiters, got {n}");
        WAITERS.store(n, Ordering::Relaxed);
        START.store(1, Ordering::Release);

        let start = timer::elapsed_ms();
        while WAKER_DONE.load(Ordering::Acquire) == 0 {
            sleep(10);
            assert!(
                timer::elapsed_ms() - start < 20_000,
                "wait queue bench did not finish in 20 s"
            );
        }
        while EXITED.load(Ordering::Acquire) < n {
            sleep(10);
            assert!(
                timer::elapsed_ms() - start < 20_000,
                "wait queue waiters did not all exit in 20 s"
            );
        }

        let wake_all = WAKE_ALL_MIN.load(Ordering::Relaxed);
        let chain = CHAIN_MIN.load(Ordering::Relaxed);
        println!();
        println!("====== WAIT QUEUE ====== ");
        println!("  ({n} waiters, all on hart 0, min of {RUNS} rounds)");
        println!(
            "  wake_all, {n} parked waiters: wall min={wake_all:>7} cycles ({:>6} per unpark)",
            wake_all / n as u32
        );
        println!(
            "  wake_all -> last waiter ran:  wall min={chain:>7} cycles ({:>6} per waiter: switch + closure + re-park)",
            chain / n as u32
        );
        println!("======================== ");
        println!();
    }
}
