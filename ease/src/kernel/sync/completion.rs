//! Completion

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::kernel::percpu;
use crate::kernel::sched::{
    self, ThreadHandle, current_thread, park_if_blocked, set_needs_wakeup, set_self_blocked,
};
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

    // Sets a flag on the scheduler that this thread needs to be woken
    // and either requests a reschedule or rings doorbell on other hart to reschedule
    // to pick up the woken thread
    #[cfg_attr(feature = "profile", profile)]
    pub fn signal(&self) {
        let mut inner = self.inner.lock();
        inner.pending = true;
        if let Some(handle) = inner.waiter.take() {
            drop(inner);
            set_needs_wakeup(handle.idx);
            // Notify the scheduler that work needs to be done
            percpu::set_needs_reschedule();
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
