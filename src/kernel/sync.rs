//! Synchronisation primitives

use core::cell::UnsafeCell;
#[cfg(target_os = "none")]
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[cfg(target_os = "none")]
use crate::arch::{disable_interrupts, restore_interrupts};
#[cfg(target_os = "none")]
use crate::kernel::sched::ThreadHandle;

//-------------------------------------------------------------------------
//
//  IrqSpinLock
//
//-------------------------------------------------------------------------

/// Lock used by kernel-wide singletons such as the global allocator.
///
/// On the kernel target this is the interrupt-disabling spinlock
/// (`IrqSpinLock`) so that critical sections can run safely from inside
/// trap handlers. On a host build (e.g. running unit tests under Miri)
/// it is a plain `SpinLock`, which has the same `lock()` interface and
/// guard semantics, so the surrounding code is identical.
#[cfg(target_os = "none")]
#[allow(dead_code)]
pub type AllocatorLock<T> = IrqSpinLock<T>;
#[cfg(not(target_os = "none"))]
#[allow(dead_code)]
pub type AllocatorLock<T> = SpinLock<T>;

// IrqSpinLock and the interrupt-disabling primitives are only meaningful on
// the kernel target. They are gated out of host builds because they depend
// on RISC-V inline assembly via `arch::disable_interrupts`. SpinLock (below)
// is target-independent and provides the host substitute via `AllocatorLock`.

/// Zero-sized proof that interrupts are disabled.
/// Private constructor — only `with_interrupts_disabled` can create one.
#[cfg(target_os = "none")]
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct CriticalSection<'cs> {
    _lifetime: PhantomData<&'cs ()>, // lifetime linked to struct existence
}

#[cfg(target_os = "none")]
impl<'cs> CriticalSection<'cs> {
    // # Safety
    // Interrupts must be disabled for the duration of 'cs.
    unsafe fn new() -> Self {
        Self {
            _lifetime: PhantomData,
        }
    }
}

/// Runs the closure with interrupts disabled, providing a `CriticalSection` token
/// as proof. Interrupts are restored to their previous state when the closure returns.
#[cfg(target_os = "none")]
pub fn with_interrupts_disabled<F, R>(f: F) -> R
where
    F: FnOnce(CriticalSection<'_>) -> R,
{
    let prev = disable_interrupts();
    let result = f(unsafe { CriticalSection::new() });
    restore_interrupts(prev);

    result
}

/// SpinLock which disables interrupts and restores on exit
#[cfg(target_os = "none")]
pub struct IrqSpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

#[cfg(target_os = "none")]
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}
#[cfg(target_os = "none")]
unsafe impl<T: Send> Send for IrqSpinLock<T> {}

#[cfg(target_os = "none")]
impl<T> IrqSpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let mut prev_mstatus: usize;
        loop {
            // Spin with cheap relaxed loads while locked
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }

            // Disable interrupts before taking the lock
            prev_mstatus = disable_interrupts();
            // Only attempt CAS when we see it's free
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            restore_interrupts(prev_mstatus);
        }
        IrqSpinLockGuard {
            lock: self,
            prev_interrupt_status: prev_mstatus,
        }
    }

    #[expect(dead_code)]
    pub fn try_lock(&self) -> Option<IrqSpinLockGuard<'_, T>> {
        // Disable interrupts before taking the lock
        let prev_mstatus = disable_interrupts();
        // Only attempt CAS
        if self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(IrqSpinLockGuard {
                lock: self,
                prev_interrupt_status: prev_mstatus,
            })
        } else {
            restore_interrupts(prev_mstatus);
            None
        }
    }
}

#[cfg(target_os = "none")]
#[must_use = "if unused, the lock is released immediately"]
pub struct IrqSpinLockGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    prev_interrupt_status: usize,
}

#[cfg(target_os = "none")]
impl<'a, T> Deref for IrqSpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

#[cfg(target_os = "none")]
impl<'a, T> DerefMut for IrqSpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}

#[cfg(target_os = "none")]
impl<'a, T> Drop for IrqSpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        // Enable interrupts if previously enabled
        restore_interrupts(self.prev_interrupt_status);
    }
}

//-------------------------------------------------------------------------
//
//  SpinLock
//
//-------------------------------------------------------------------------

///SpinLock that leaves interrupts enabled
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        loop {
            // Spin with cheap relaxed loads while locked
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
            // Only attempt CAS when we see it's free
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
        SpinLockGuard { lock: self }
    }

    #[allow(dead_code)]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        // Only attempt CAS
        if self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinLockGuard { lock: self })
        } else {
            None
        }
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<'a, T> Deref for SpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

impl<'a, T> DerefMut for SpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<'a, T> Drop for SpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

//-------------------------------------------------------------------------
//
//  Mutex
//
//-------------------------------------------------------------------------

#[cfg(target_os = "none")]
const FREE: u8 = MutexState::Free as u8;
#[cfg(target_os = "none")]
const LOCKED: u8 = MutexState::Locked as u8;
#[cfg(target_os = "none")]
const LOCKED_WITH_WAITERS: u8 = MutexState::LockedWithWaiters as u8;

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
#[cfg(target_os = "none")]
enum MutexState {
    Free = 0,
    Locked = 1,
    LockedWithWaiters = 2,
}

#[cfg(target_os = "none")]
pub struct Mutex<T> {
    state: AtomicU8, // 0 - unlocked; 1 - locked no waiters; 2 - locked with waiter
    data: UnsafeCell<T>,
    waiters: SpinLock<Option<ThreadHandle>>,
}

#[cfg(target_os = "none")]
impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicU8::new(0),
            data: UnsafeCell::new(data),
            waiters: SpinLock::new(None),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        const SPIN_MAX: usize = 100;
        // CAS, spin a bit, then block
        let mut count = 0;
        loop {
            if self
                .state
                .compare_exchange_weak(
                    MutexState::Free as u8,
                    MutexState::Locked as u8,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                // We got the lock, no need to block
                return MutexGuard { lock: self };
            } else {
                count += 1;
                if count == SPIN_MAX {
                    break;
                }
            }
            core::hint::spin_loop();
        }
        // No luck, time to block
        let mut head = self.waiters.lock();
        // Check state
        loop {
            match self.state.load(Ordering::Relaxed) {
                FREE => {
                    // Let's try to take this lock
                    if self
                        .state
                        .compare_exchange_weak(
                            MutexState::Free as u8,
                            MutexState::Locked as u8,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        // Successfully caught the lock this time, unwind and return the mutex guard
                        drop(head);
                        return MutexGuard { lock: self };
                    }
                }
                LOCKED => {
                    // Let's add a waiter
                    if self
                        .state
                        .compare_exchange_weak(
                            MutexState::Locked as u8,
                            MutexState::LockedWithWaiters as u8,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        // Successfully changed state to having waiters
                        break;
                    }
                }
                LOCKED_WITH_WAITERS => {
                    // We need to block the thread
                    break;
                }
                _ => {
                    unreachable!("should not reach this lock state");
                }
            }
            core::hint::spin_loop();
        }
        // Now block
        // Enqueue self at the head of the waiter list
        let curr_handle = crate::kernel::sched::current_thread();
        crate::kernel::sched::set_next_waiter(&curr_handle, *head);
        *head = Some(curr_handle);

        // Set the state to Blocked - in case a thread has called Drop in the mean time
        crate::kernel::sched::set_self_blocked();
        drop(head);
        crate::kernel::sched::park_if_blocked();
        // Add an Acquire fence (matched by Release fence in Drop)
        core::sync::atomic::fence(Ordering::Acquire);
        MutexGuard { lock: self }
    }
}

#[cfg(target_os = "none")]
unsafe impl<T: Send> Send for Mutex<T> {}
#[cfg(target_os = "none")]
unsafe impl<T: Send> Sync for Mutex<T> {}

#[must_use = "if unused, the lock is released immediately"]
#[cfg(target_os = "none")]
pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
}

#[cfg(target_os = "none")]
impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.lock.data.get()) }
    }
}

#[cfg(target_os = "none")]
impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut (*self.lock.data.get()) }
    }
}

#[cfg(target_os = "none")]
impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        if self
            .lock
            .state
            .compare_exchange(
                MutexState::Locked as u8,
                MutexState::Free as u8,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return;
        }
        // Pop first thread from the list
        let mut head = self.lock.waiters.lock();
        let popped = head
            .take()
            .expect("there should be waiters in the blocked list");
        let next_waiter_in_list = crate::kernel::sched::get_next_waiter(&popped);
        crate::kernel::sched::set_next_waiter(&popped, None);
        *head = next_waiter_in_list;
        // Change the state
        if next_waiter_in_list.is_none() {
            self.lock
                .state
                .store(MutexState::Locked as u8, Ordering::Release);
        }
        core::sync::atomic::fence(Ordering::Release);
        drop(head);
        crate::kernel::sched::unpark(&popped);
    }
}

//-------------------------------------------------------------------------
//
//  CounterU64
//
//-------------------------------------------------------------------------

#[cfg(not(target_has_atomic = "64"))]
use core::sync::atomic::AtomicU32;

/// Lock-free monotonic 64-bit counter
///
/// Only supports single writer.
///
/// Uses Acquire / Release ordering
#[cfg(not(target_has_atomic = "64"))]
pub struct CounterU64 {
    seq: AtomicU32,
    hi: AtomicU32,
    lo: AtomicU32,
}

#[cfg(not(target_has_atomic = "64"))]
impl CounterU64 {
    pub const fn new(v: u64) -> Self {
        Self {
            seq: AtomicU32::new(0),
            hi: AtomicU32::new((v >> 32) as u32),
            lo: AtomicU32::new(v as u32),
        }
    }

    /// Add to the counter
    ///
    /// Returns the previous counter value
    /// Safety: Caller must ensure only single writer (no concurrency)
    pub unsafe fn add(&self, v: u64) -> u64 {
        // Sequence lock to prevent readers from seeing tearing
        let s = self.seq.fetch_add(1, Ordering::Acquire); // Odd - write in progress
        assert!(s & 1 == 0, "multiple writers not allowed");
        let lo = self.lo.fetch_add(v as u32, Ordering::Relaxed);
        let hi = if lo.wrapping_add(v as u32) < lo {
            self.hi
                .fetch_add(((v >> 32) as u32).wrapping_add(1), Ordering::Relaxed)
        } else {
            self.hi.fetch_add((v >> 32) as u32, Ordering::Relaxed)
        };
        self.seq.fetch_add(1, Ordering::Release); // Even - write complete
        ((hi as u64) << 32) | (lo as u64)
    }

    /// Reads the current counter value
    #[allow(dead_code)]
    pub fn get(&self) -> u64 {
        loop {
            // Check if a write is in progress
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                // Odd - write in progress
                core::hint::spin_loop();
                continue;
            }
            let hi = self.hi.load(Ordering::Relaxed);
            let lo = self.lo.load(Ordering::Relaxed);
            let s2 = self.seq.load(Ordering::Acquire);
            if s1 != s2 {
                core::hint::spin_loop();
                continue; // Write happened during read, start again
            }
            return ((hi as u64) << 32) | (lo as u64);
        }
    }

    /// Reset the counter
    ///
    /// Safety: Caller must ensure only single reset caller (no concurrency)
    #[allow(dead_code)]
    pub unsafe fn reset(&self) {
        // Sequence lock to prevent readers from seeing tearing
        let s = self.seq.fetch_add(1, Ordering::Acquire);
        assert!(s & 1 == 0, "multiple writers not allowed");
        self.hi.store(0, Ordering::Relaxed);
        self.lo.store(0, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Set the counter to a u64 value
    ///
    /// Returns the previous counter value
    /// Safety: Caller must ensure only single writer (no concurrency)
    #[allow(dead_code)]
    pub unsafe fn set(&self, v: u64) -> u64 {
        // Sequence lock to prevent readers from seeing tearing
        let s = self.seq.fetch_add(1, Ordering::Acquire);
        assert!(s & 1 == 0, "multiple writers not allowed");
        let hi = self.hi.swap((v >> 32) as u32, Ordering::Relaxed);
        let lo = self.lo.swap(v as u32, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
        ((hi as u64) << 32) | (lo as u64)
    }
}

/// Thin wrapper for host tests
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;

#[cfg(target_has_atomic = "64")]
pub struct CounterU64 {
    counter: AtomicU64,
}

#[cfg(target_has_atomic = "64")]
impl CounterU64 {
    pub const fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    pub unsafe fn add(&self, v: u64) -> u64 {
        self.counter.fetch_add(v, Ordering::AcqRel)
    }

    pub fn get(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    pub unsafe fn reset(&self) {
        self.counter.store(0, Ordering::Release);
    }
}

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
#[cfg(all(test, not(target_os = "none")))]
mod host_tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::vec::Vec;

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
