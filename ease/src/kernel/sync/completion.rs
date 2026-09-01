//! Completion

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::kernel::sched::{
    self, Deadline, ThreadHandle, current_thread, park_if_blocked, set_self_blocked,
};
use crate::kernel::sync::IrqSpinLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted;

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
    /// Set a thread to block until the completion is signalled
    pub fn wait(&self) {
        loop {
            // Wrong-variant tripwire: a condemned user thread parked here would
            // re-park forever once the switch-time kill conversions are deleted.
            // Condemned threads must be on wait_interruptible.
            assert!(
                !sched::current_user_thread_needs_exit(),
                "condemned user thread must use wait_interruptible, not wait"
            );
            let mut inner = self.inner.lock();
            if inner.pending {
                // Signal already fired
                inner.pending = false;
                drop(inner);
                return; // Early
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
    /// Set a thread to wait until the completion is signalled, but with a timeout.
    pub fn wait_with_deadline(&self, deadline: Deadline) -> Result<(), TimedOut> {
        loop {
            let mut inner = self.inner.lock();
            if inner.pending {
                inner.pending = false;
                inner.waiter = None;
                return Ok(());
            } else {
                // Check if we have passed the deadline - ignoring any leeway
                if deadline.has_passed() {
                    return Err(TimedOut);
                } else {
                    inner.waiter = Some(current_thread());
                    sched::set_self_blocked_until(deadline);
                    drop(inner);
                    sched::park_if_blocked_until(deadline);
                    let mut inner = self.inner.lock();
                    inner.waiter = None;
                }
            }
        }
    }
    /// Set a thread to wait until the completion is signalled.
    /// The thread can be interrupted, in which case it will return
    /// Err(Interrupted)
    pub(crate) fn wait_interruptible(&self) -> Result<(), Interrupted> {
        loop {
            let mut inner = self.inner.lock();
            // Check for signal at the start of the loop in case we have a quick return
            // Otherwise we check after each wake
            if inner.pending {
                // Signal has been passed to this completion
                // Clear the flag and return immediately
                inner.pending = false;
                drop(inner);
                return Ok(());
            }
            // We are (re)setting up the completion - it needs to know which thread is waiting
            inner.waiter = Some(current_thread());
            sched::set_self_blocked(); // Set the thread status to blocked (takes sched lock while holding the completion lock)
            drop(inner);
            sched::park_if_blocked(); // Reschedule the current thread to reach it's Blocked state. Takes sched lock, hence dropping inner first.
            // If we reach this point we've been unblocked - but this could be spurious, so clear state and re-loop
            let mut inner = self.inner.lock();
            inner.waiter = None;
            // Check if we have been interrupted, in which case exit with error
            if sched::current_user_thread_needs_exit() {
                return Err(Interrupted);
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
