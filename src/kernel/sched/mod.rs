//! Scheduler

mod stride;
#[cfg(all(test, feature = "test-sched"))]
mod tests;
mod types;

use stride::SCHEDULER;

#[allow(unused_imports)]
pub use stride::{PRIORITY_DEFAULT, PRIORITY_MIN};
pub use types::{Qos, StackClass, ThreadHandle};

pub fn idle_thread() -> ! {
    loop {
        crate::arch::wait_for_interrupt();
    }
}

/// Spawn a new thread
pub fn spawn<F: FnOnce() + Send + 'static>(
    entry: F,
    priority: u8,
    class: StackClass,
    qos: Qos,
) -> Option<ThreadHandle> {
    SCHEDULER.spawn(entry, priority, class, qos)
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

/// Preempts thread
pub fn preempt() {
    SCHEDULER.preempt();
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
pub fn unpark_by_index(idx: usize) {
    SCHEDULER.unpark_by_index(idx);
}

/// Set this thread to blocked state without rescheduling
pub fn set_self_blocked() {
    SCHEDULER.set_self_blocked()
}
