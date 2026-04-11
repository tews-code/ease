//! Synchronisation primitives

use core::cell::UnsafeCell;
#[cfg(target_os = "none")]
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "none")]
use crate::arch::{disable_interrupts, restore_interrupts};

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
#[allow(dead_code)]
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
}
