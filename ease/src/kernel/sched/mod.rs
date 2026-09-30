//! Scheduler

#[cfg(all(test, feature = "bench"))]
mod bench;
mod deadline;
mod process;
mod spawn;
mod stride;
#[cfg(all(test, any(feature = "test-sched", feature = "bench")))]
pub(crate) mod test_support;
#[cfg(all(test, feature = "test-sched"))]
mod tests;
pub(crate) mod thread;
#[cfg(feature = "trace")]
pub mod trace;
pub(crate) mod userloader;
pub(crate) mod usermem;

use crate::board::HARTS_MAX;
use crate::kernel::alloc::{MemRegion, Order};
use crate::kernel::fd;
use crate::kernel::percpu;
use crate::kernel::sync::with_interrupts_disabled;
use crate::user;
pub(crate) use deadline::Deadline;
use stride::SCHEDULER;
#[expect(unused_imports)]
pub(crate) use stride::{PRIORITY_DEFAULT, PRIORITY_MIN, SchedInner};

#[derive(Debug, Copy, Clone)]
pub enum Qos {
    High,
    Low,
}

#[must_use = "Builder must be terminated with .spawn() to actually create a thread"]
pub struct Builder {
    stack: Order,         // Must be from stack class
    qos: Qos,             // Low priority for background
    priority: u8,         // Lower number is higher priority
    affinity: Option<u8>, // Affinity to a particular HART
}

#[allow(dead_code)]
impl Builder {
    pub const fn new() -> Self {
        Self {
            stack: Order::KB4,
            qos: Qos::High,
            priority: PRIORITY_DEFAULT,
            affinity: None,
        }
    }

    pub fn with_stack_class(mut self, order: Order) -> Self {
        self.stack = order;
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

    pub fn spawn<F: FnOnce() + Send + 'static>(self, entry: F) -> Option<thread::Handle> {
        SCHEDULER.spawn_kernel_thread_with(
            entry,
            self.priority,
            self.stack,
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
                crate::arch::interrupts::wait_for_interrupt();
            }
        });
        schedule();
    }
}

/// Spawn a new kernel thread with the provided closure
#[allow(dead_code)]
pub fn spawn<F: FnOnce() + Send + 'static>(entry: F) -> Option<thread::Handle> {
    Builder::new().spawn(entry)
}

/// Set up the boot thread
pub fn bootstrap(hartid: usize) {
    SCHEDULER.bootstrap(hartid)
}

/// Voluntarily yield the current thread
pub fn yield_now() {
    SCHEDULER.yield_now();
}
/// Get cycles of running thread
pub fn get_current_cycles(tcb_idx: usize) -> u64 {
    SCHEDULER.get_current_cycles(tcb_idx)
}
/// Clean up switched status threads
pub fn post_switch_cleanup() {
    SCHEDULER.post_switch_cleanup();
}

// SLEEP

/// Blocks the current thread until the timer has passed the absolute deadline.
/// Time is measured in milliseconds
/// Uses the system default leeway (which is based on the thread's QoS)
pub fn sleep_until(deadline_ms: u64) {
    let qos = percpu::current_qos();
    let deadline = Deadline::from_ms_with_system_leeway(deadline_ms, qos);
    SCHEDULER.sleep_until(deadline);
}
/// Blocks for `duration`
/// Time is measured in milliseconds
/// Uses the system default leeway (which is based on the thread's QoS)
pub fn sleep(duration_ms: u64) {
    let qos = percpu::current_qos();
    let deadline = Deadline::after_ms_with_system_leeway(duration_ms, qos);
    SCHEDULER.sleep_until(deadline);
}
/// Blocks for `duration` in milliseconds given a leeway in milliseconds
#[cfg(test)]
pub fn sleep_with_leeway_ms(duration_ms: u64, leeway_ms: u64) {
    let deadline = Deadline::after_ms(duration_ms, leeway_ms);
    SCHEDULER.sleep_until(deadline);
}

// THREAD EXIT

/// Voluntarily terminate the current kernel thread. Doesn't return.
pub(crate) fn exit_kernel_thread(reason: thread::ExitReason) -> ! {
    SCHEDULER.exit(reason);
}
/// Voluntarily terminate the current user thread. Doesn't return
pub(crate) fn exit_current_user_thread(reason: thread::ExitReason) -> ! {
    SCHEDULER.exit_current_user_thread(reason);
}
/// Lock-free check: has the current user thread been condemned by a
/// process kill? (Reads the `needs_user_exit` bitmap.)
///
/// This is the query flavour of the interruptible-wait discipline: a
/// blocking wait that HOLDS LOCKS must use this, convert a `true` into
/// an interrupted-error, and propagate it up through normal returns —
/// releasing everything it holds on the way — dying only once the
/// stack has unwound to the syscall boundary.
pub(crate) fn current_user_thread_needs_exit() -> bool {
    SCHEDULER
        .needs_user_exit
        .get(percpu::current_thread().idx())
}
/// Exit the current user thread now if it has been condemned;
/// otherwise return normally. Like `park_if_blocked`, the `if` in the
/// name warns that this call sometimes never returns.
///
/// Every wakeup inside a blocking syscall's wait loop must make this
/// check — a condemned thread that re-parks unaware stalls its
/// process's teardown. This exit-on-the-spot flavour is legal ONLY
/// for waits that hold nothing: `exit_user_thread` diverges, so Drop
/// never runs and anything held (a MutexGuard, a claimed fd) would be
/// orphaned. If your wait holds locks, use
/// [`current_user_thread_needs_exit`] and propagate an error instead.
///
/// The reason is hardcoded to `Fault` because the sole producer of the
/// condemned bit today is fault eviction. If a non-fault producer ever
/// appears (e.g. a kill syscall), the reason belongs in the PCB —
/// process-scoped, like the marking itself — not in this bitmap.
pub(crate) fn exit_user_thread_if_needs_exit() {
    if current_user_thread_needs_exit() {
        SCHEDULER.exit_current_user_thread(thread::ExitReason::Fault);
    }
}

// PARK

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
pub fn unpark(handle: thread::Handle) {
    SCHEDULER.unpark(handle);
}
/// Set the waiter tcb index
pub fn set_next_waiter(handle: thread::Handle, next: Option<thread::Handle>) {
    SCHEDULER.set_next_waiter(handle, next);
}

/// Get waiter tcb index
pub fn get_next_waiter(handle: thread::Handle) -> Option<thread::Handle> {
    SCHEDULER.get_next_waiter(handle)
}

/// Unpark using TCB index instead of thread handle
#[allow(dead_code)]
pub fn unpark_by_handle(handle: thread::Handle) {
    SCHEDULER.unpark_by_handle(handle);
}

/// Set this thread to blocked state without rescheduling
pub fn set_self_blocked() {
    SCHEDULER.set_self_blocked()
}

/// Set this thread to blocked state, with a wakeup `Deadline`
pub fn set_self_blocked_until(deadline: Deadline) {
    SCHEDULER.set_self_blocked_until(deadline);
}
/// Park this thread in blocked state with wake up `Deadline`
pub fn park_if_blocked_until(deadline: Deadline) {
    SCHEDULER.park_if_blocked_until(deadline);
}

/// Set this thread to Running state without rescheduling
pub fn set_self_running() {
    SCHEDULER.set_self_running()
}

/// Flat this thread as ready to be preemptively rescheduled
pub fn mark_for_preempt() {
    SCHEDULER.mark_for_preempt();
}

/// Schedule the next thread
pub fn schedule() {
    SCHEDULER.schedule();
}

/// Cycles the current thread's last timer wake overshot its deadline.
#[cfg(feature = "trace")]
#[allow(dead_code)] // used only by test probes
pub fn current_wake_overshoot() -> u64 {
    SCHEDULER.current_wake_overshoot()
}

//
// SPAWN USER
//

/// Spawn a user process
pub fn spawn_process(name: &'static str) -> Result<process::Handle, process::SpawnError> {
    // Look up this program in the table
    let image = user::PROGRAMS
        .find(name)
        .ok_or(process::SpawnError::NotFound)?;
    // Load the image into memory
    let loaded_image = userloader::load_user_image(image)?;
    // Spawn the process
    SCHEDULER.spawn_process(
        name,
        PRIORITY_DEFAULT,
        Order::KB4,
        Order::KB4,
        loaded_image,
        Qos::High,
        None,
    )
}

/// Spawn a user thread
#[allow(dead_code)]
pub fn spawn_user(
    process: process::Handle,
    entry: userloader::UserEntry,
) -> Option<thread::Handle> {
    SCHEDULER.spawn_user_thread(
        process,
        entry,
        PRIORITY_DEFAULT,
        Order::KB4,
        Order::KB4,
        Qos::High,
        None,
    )
}

//
// PER-THREAD FLAGS
//

/// Set the wake up flag for a thread by index
pub fn set_needs_wakeup(handle: thread::Handle) {
    SCHEDULER.set_wakeup_flag(handle);
}
/// Clear the wake up flag for a thread by index
pub fn clear_wakeup_signal(handle: thread::Handle) {
    SCHEDULER.clear_wakeup_flag(handle);
}

/// Prints the running thread kernel stack high watermarks
#[cfg(feature = "paint-stack")]
pub fn stacks() {
    SCHEDULER.stacks();
}

//
//  FILE DESCRIPTORS
//

fn open_file_descriptor(fd_kind: fd::Kind) -> Result<usize, fd::Error> {
    SCHEDULER.open_fd(fd_kind)
}

fn close_file_descriptor(fd: usize) -> Result<fd::Kind, fd::Error> {
    SCHEDULER.close_fd(fd)
}

fn new_process_fds() {
    SCHEDULER.new_process_fds();
}
