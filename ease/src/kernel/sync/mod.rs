//! Synchronisation for Ease
//!
//! IrqSpinLock and the interrupt-disabling primitives are only meaningful on
//! the kernel target. They are gated out of host builds because they depend
//! on RISC-V inline assembly via `arch::disable_interrupts`.SpinLock (below)
//! is target-independent and provides the host substitute via `AllocatorLock`.

#[cfg(target_os = "none")]
mod completion;
#[cfg(not(target_has_atomic = "64"))]
mod counteru64;
#[cfg(target_os = "none")]
mod mutex;
mod spinlock;
mod staticcell;
#[cfg(all(test, not(target_os = "none")))]
pub mod tests;
mod waitqueue;

#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use completion::Completion;
#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use counteru64::CounterU64;
#[cfg(target_os = "none")]
#[allow(unused_imports)]
pub use mutex::Mutex;
#[allow(unused_imports)]
pub use spinlock::{IrqSpinLock, IrqSpinLockGuard, SpinLock, SpinLockGuard, TryLock};
#[allow(unused_imports)]
pub(crate) use staticcell::StaticCell;
#[allow(unused_imports)]
pub(crate) use waitqueue::WaitQueue;

/// Return Err variants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted;

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
