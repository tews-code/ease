//! Synchronisation for Ease

#[cfg(target_os = "none")]
use core::marker::PhantomData;

#[cfg(target_os = "none")]
use crate::arch::interrupts;

#[cfg(target_os = "none")]
mod completion;
#[cfg(not(target_has_atomic = "64"))]
mod counteru64;
#[cfg(target_os = "none")]
mod mutex;
mod spinlock;
#[cfg(all(test, not(target_os = "none")))]
pub mod tests;

#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use completion::{Completion, TimedOut};
#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use counteru64::CounterU64;
#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use mutex::Mutex;
#[allow(unused_imports)]
pub use spinlock::{IrqSpinLock, IrqSpinLockGuard, SpinLock};

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
#[cfg_attr(feature = "irqsoff", track_caller)] // irqsoff attributes the section to our caller
pub fn with_interrupts_disabled<F, R>(f: F) -> R
where
    F: FnOnce(CriticalSection<'_>) -> R,
{
    let prev = interrupts::disable();
    let result = f(unsafe { CriticalSection::new() });
    interrupts::restore(prev);

    result
}

/// Thin wrapper for host tests
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::{AtomicU64, Ordering};

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
