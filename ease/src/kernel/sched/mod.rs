//! Scheduler

mod stride;
#[cfg(all(test, feature = "test-sched"))]
mod tests;
mod types;

use stride::SCHEDULER;

#[allow(unused_imports)]
pub use stride::{PRIORITY_DEFAULT, PRIORITY_MIN};
pub use types::{Qos, StackClass, ThreadHandle};

use crate::{
    board::HARTS_MAX,
    kernel::{percpu, sync::with_interrupts_disabled},
};

#[must_use = "Builder must be terminated with .spawn() to actually create a thread"]
pub struct Builder {
    stack_class: StackClass, // Must be from stack class
    qos: Qos,                // Low priority for background
    priority: u8,            // Lower number is higher priority
    affinity: Option<u8>,    // Affinity to a particular HART
}

#[allow(dead_code)]
impl Builder {
    pub const fn new() -> Self {
        Self {
            stack_class: StackClass::KB4,
            qos: Qos::High,
            priority: PRIORITY_DEFAULT,
            affinity: None,
        }
    }

    pub fn with_stack_class(mut self, stack_class: StackClass) -> Self {
        self.stack_class = stack_class;
        self
    }

    pub fn with_qos(mut self, qos: Qos) -> Self {
        self.qos = qos;
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_affinity(mut self, affinity: u8) -> Self {
        if affinity < HARTS_MAX as u8 {
            self.affinity = Some(affinity);
        }
        self
    }

    pub fn spawn<F: FnOnce() + Send + 'static>(self, entry: F) -> Option<ThreadHandle> {
        SCHEDULER.spawn(
            entry,
            self.priority,
            self.stack_class,
            self.qos,
            self.affinity,
        )
    }
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new()
    }
}

pub fn idle_thread() -> ! {
    loop {
        with_interrupts_disabled(|_cs| {
            if !percpu::needs_reschedule() {
                crate::arch::wait_for_interrupt();
            }
        });
        schedule();
    }
}

/// Spawn a new thread
pub fn spawn<F: FnOnce() + Send + 'static>(entry: F) -> Option<ThreadHandle> {
    Builder::new().spawn(entry)
}

/// Set up the boot thread
pub fn bootstrap(hartid: usize) {
    SCHEDULER.bootstrap(hartid);
}

/// Voluntarily yield the current thread
pub fn yield_now() {
    SCHEDULER.yield_now();
}

/// Blocks until the timer has passed the deadline
/// Time is measured in milliseconds
#[allow(dead_code)]
pub fn sleep_until(deadline_ms: u64) {
    SCHEDULER.sleep_until(deadline_ms, None);
}

/// Blocks for `deadline` millseconds
#[allow(dead_code)]
pub fn sleep(deadline_ms: u64) {
    SCHEDULER.sleep(deadline_ms);
}

/// Blocks for `deadline` millseconds
#[allow(dead_code)]
pub fn sleep_with_leeway(deadline_ms: u64, fixed_leeway_ms: u64) {
    SCHEDULER.sleep_with_leeway(deadline_ms, fixed_leeway_ms);
}

/// Get cycles of running thread
#[allow(dead_code)]
pub fn get_current_cycles(tcb_idx: usize) -> u64 {
    SCHEDULER.get_current_cycles(tcb_idx)
}

/// Clean up switched status threads
pub fn post_switch_cleanup() {
    SCHEDULER.post_switch_cleanup();
}

/// Voluntarily terminate the current thread. Doesn't return.
#[allow(dead_code)] // currently only used from test helpers
pub fn exit() -> ! {
    SCHEDULER.exit();
}

/// Park the current thread
///
/// Note this can race. For race-free use `park_if_blocked`
#[allow(dead_code)]
pub fn park() {
    SCHEDULER.park();
}

/// Park the current thread if it is in Blocked state
pub fn park_if_blocked() {
    SCHEDULER.park_if_blocked();
}

/// Unpark the current thread by thread handle
pub fn unpark(handle: &ThreadHandle) {
    SCHEDULER.unpark(handle);
}

/// Get a handle to the thread
pub fn current_thread() -> ThreadHandle {
    SCHEDULER.current_thread()
}

/// Set the waiter tcb index
pub fn set_next_waiter(handle: &ThreadHandle, next: Option<ThreadHandle>) {
    SCHEDULER.set_next_waiter(handle, next);
}

/// Get waiter tcb index
pub fn get_next_waiter(handle: &ThreadHandle) -> Option<ThreadHandle> {
    SCHEDULER.get_next_waiter(handle)
}

/// Unpark using TCB index instead of thread handle
#[allow(dead_code)]
pub fn unpark_by_index(idx: usize) {
    SCHEDULER.unpark_by_index(idx);
}

/// Set this thread to blocked state without rescheduling
pub fn set_self_blocked() {
    SCHEDULER.set_self_blocked()
}

/// Set this thread to blocked state, with a wakeup deadline
pub fn set_self_blocked_until(deadline_ms: u64) {
    SCHEDULER.set_self_blocked_until(deadline_ms);
}
/// Park this thread in blocked state with wakeup deadline
pub fn park_if_blocked_until(deadline_ms: u64) {
    SCHEDULER.park_if_blocked_until(deadline_ms);
}

/// Flat this thread as ready to be preemptively rescheduled
pub fn mark_for_preempt() {
    SCHEDULER.mark_for_preempt();
}
/// Schedule the next thread
pub fn schedule() {
    SCHEDULER.schedule();
}
