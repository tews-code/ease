//! Completion

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::kernel::sched::{self, ThreadHandle, current_thread, park_if_blocked, set_self_blocked};
use crate::kernel::sync::IrqSpinLock;
use crate::kernel::timer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

struct CompletionInner {
    pending: bool,                // Flag that gets set if signal() fires before any wait().
    waiter: Option<ThreadHandle>, // The single thread parked on this completion, if any.
}

pub struct Completion {
    inner: IrqSpinLock<CompletionInner>,
}

impl Completion {
    pub const fn new() -> Self {
        Self {
            inner: IrqSpinLock::new(CompletionInner {
                pending: false,
                waiter: None,
            }),
        }
    }

    // Wakes the registered waiter via `unpark`, whose handle id check makes
    // a stale registration a no-op: a waiter that was killed while parked
    // here (its slot recycled or emptied) must not have its slot index
    // woken blind. Sets `pending` first so a signal with no waiter is
    // consumed by the next `wait`.
    #[cfg_attr(feature = "profile", profile)]
    pub fn signal(&self) {
        let mut inner = self.inner.lock();
        inner.pending = true;
        let handle = inner.waiter.take();
        drop(inner);
        if let Some(handle) = handle {
            sched::unpark(&handle); // Checks for staleness before waking the thread
        }
    }

    #[allow(dead_code)]
    pub fn wait(&self) {
        loop {
            let mut inner = self.inner.lock();
            if inner.pending {
                // Signal already fired
                inner.pending = false;
                drop(inner);
                return;
            } else {
                inner.waiter = Some(current_thread());
                set_self_blocked();
                drop(inner);
                park_if_blocked();
                // Clear waiter in case of spurious wake
                let mut inner = self.inner.lock();
                inner.waiter = None;
            }
        }
    }

    pub fn wait_with_deadline(&self, deadline_ms: u64) -> Result<(), TimedOut> {
        let abs_deadline_ms = deadline_ms.saturating_add(timer::elapsed_ms());
        loop {
            let mut inner = self.inner.lock();
            if inner.pending {
                inner.pending = false;
                inner.waiter = None;
                return Ok(());
            } else {
                let now = timer::elapsed_ms();
                if now >= abs_deadline_ms {
                    return Err(TimedOut);
                } else {
                    inner.waiter = Some(current_thread());
                    sched::set_self_blocked_until(abs_deadline_ms);
                    drop(inner);
                    sched::park_if_blocked_until(abs_deadline_ms);
                    let mut inner = self.inner.lock();
                    inner.waiter = None;
                }
            }
        }
    }

    // Caller must ensure no waiter is parked.
    pub unsafe fn reset(&self) {
        let mut inner = self.inner.lock();
        assert!(
            inner.waiter.is_none(),
            "trying to reset but there is a parked waiter"
        );
        inner.pending = false;
    }
}
