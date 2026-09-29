//! Threads

use core::fmt::Debug;
use core::ptr::NonNull;

use super::{Qos, deadline::Deadline, process, stride::PRIORITY_MIN, userloader};
use crate::kernel::alloc::MemRegion;
use crate::kernel::collection::Arena;
#[cfg(feature = "paint-stack")]
use crate::kernel::stack::print_watermark;
use crate::kernel::timer;

/// Alias to clearly simplify the thread handle definition
pub(crate) type Handle = crate::kernel::collection::Handle<ControlBlock>;

/// Maximum number of simultaneous threads. Includes two slots used for idle threads
pub(crate) const MAX_COUNT: usize = 16;
/// Thread exit reason
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ExitReason {
    Exit,
    Fault,
}
const _: () = assert!(ExitReason::Exit as u8 == 0); // Must be zero as used in assembly
const _: () = assert!(ExitReason::Fault as u8 == 1); // Must be one as used in assembly
/// The state the thread will be put into in post-switch cleanup
#[derive(PartialEq, Debug, Copy, Clone)]
pub(crate) enum PostSwitch {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Sleeping(Deadline),
    Dead(ExitReason),
}
/// The current thread state
#[derive(PartialEq, Debug, Copy, Clone)]
pub(crate) enum State {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(Deadline),
}
/// Outcome of [Threads::make_blocked_ready]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnblockedResult {
    Unparked(Option<u8>), // Holds the affinity
    Deferred,             // The thread is mid-switch
    NotBlocked,           // No action to be taken
}
/// If the thread is a user thread this context holds the user stack,
/// entry point and the process it to which it belongs.
pub(super) struct UserContext {
    pub(super) stack: MemRegion,
    pub(super) entry: userloader::UserEntry,
    pub(super) process_idx: u8,
}
/// Key data structure: the thread control block holds the running thread
/// details
pub(crate) struct ControlBlock {
    pub(super) state: State,
    pub(super) kernel_stack: MemRegion,
    pub(super) sp: NonNull<u8>,
    pub(super) qos: Qos,
    pub(super) priority: u8,                // Lower number is higher priority
    pub(super) affinity: Option<u8>,        // Affinity to a particular HART
    pub(super) user: Option<UserContext>,   // If is Some then this TCB is supporting a user thread
    pub(super) pass: u64,                   // The next ready thread with lowest pass wins
    pub(super) last_started_cycles: u64,    // Cycle stamp from last switch
    pub(super) next_waiter: Option<Handle>, // Handle of next thread waiting on blocked resource
    pub(super) resources_released: bool, // If set then thread no longer uses any resources (e.g. file descriptors) except for cleanup
    #[cfg(feature = "trace")]
    pub(super) ready_since: u64, // Cycle stamp of the last transition into Ready (for wake-latency tracing)
}
/// Specification struct for clean creation of new TCBs
pub(super) struct ControlBlockSpec {
    pub(super) kernel_stack: MemRegion,
    pub(super) qos: Qos,
    pub(super) priority: u8,              // Lower number is higher priority
    pub(super) affinity: Option<u8>,      // Affinity to a particular HART
    pub(super) user: Option<UserContext>, // If is Some then this TCB is supporting a user thread
}

impl ControlBlock {
    // Stride forward by ran_cycles weighted by priority.
    //
    // `ran_cycles` is in CLINT cycles (mtime units) so `pass` is the same
    // unit across all threads. No `/ SLICE` normalisation is applied — that
    // would round sub-millisecond runs to zero stride, which left
    // yield-loopers' pass stagnant and starved sleepers (see Phase 5
    // notes). The trade-off is `pass` values grow larger in absolute
    // terms (~10⁷ per ms at PRIORITY_DEFAULT), but they stay well under
    // u64 saturation for any realistic uptime.
    //
    // Threads at priority 0 do not stride/age (stride is 0 for any ran)
    // and so always have the lowest pass — they win every pick_next.
    pub(super) fn stride_forward(&mut self, ran_cycles: u64) {
        self.pass = self
            .pass
            .saturating_add(ran_cycles.saturating_mul(self.priority as u64));
    }

    // Calculates the latest wake up for a thread including any leeway
    // Returns None if thread is not sleeping
    fn must_wake_by(&self) -> Option<u64> {
        match self.state {
            State::Sleeping(deadline)
            | State::Switching(PostSwitch::Sleeping(deadline))
            | State::BlockedUntil(deadline)
            | State::Switching(PostSwitch::BlockedUntil(deadline)) => Some(deadline.latest()),
            _ => None,
        }
    }
    /// Get the process index of this thread control block
    ///
    /// Returns the process index or None if not a user thread
    fn process_idx(&self) -> Option<u8> {
        self.user.as_ref().map(|uc| uc.process_idx)
    }
    /// Helper function shared by `schedule` (preempt called from trap handler) and `reschedule` (voluntary) scheduler calls
    /// Performs common cycle count bookkeeping and updates stride for the current thread.
    pub(super) fn slice_ended(&mut self, now_cycles: u64) -> u64 {
        let ran = now_cycles - self.last_started_cycles;
        self.last_started_cycles = now_cycles;
        self.stride_forward(ran);
        ran
    }
}

impl Debug for ControlBlock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "state: {:?}", self.state)?;
        writeln!(f, "QoS: {:?}", self.qos)?;
        writeln!(f, "priority: {}", self.priority)?;
        writeln!(f, "pass: {}", self.pass)?;
        writeln!(f, "last_started_cycles: {}", self.last_started_cycles)?;
        writeln!(f, "next_waiter: {:?}", self.next_waiter)?;
        writeln!(f, "affinity: {:?}", self.affinity)?;
        writeln!(f, "user thread? {}", self.user.is_some())?;
        #[cfg(feature = "trace")]
        writeln!(f, "ready_since: {}", self.ready_since)?;
        Ok(())
    }
}
/// The threads are held in an arena.
/// There is an additional `maybe_sleeping` bitmap for efficiency - allowing the scheduler to avoid
/// a full thread arena scan on sleeping threads.
#[derive(Debug)]
pub(super) struct Threads {
    pub(super) tcbs: Arena<ControlBlock, MAX_COUNT>,
}

impl Threads {
    // Acquire a free slot and set up a valid thread control block
    pub(super) fn acquire(
        &mut self,
        forge_stack: impl FnOnce(&mut MemRegion) -> NonNull<u8>,
        spec: ControlBlockSpec,
    ) -> Option<Handle> {
        if self.tcbs.is_full() {
            return None;
        }
        let pass_baseline = self.pass_baseline();
        let ControlBlockSpec {
            qos,
            priority,
            mut kernel_stack,
            affinity,
            user,
        } = spec;
        let sp = forge_stack(&mut kernel_stack);
        let now = timer::elapsed();
        self.tcbs.add(ControlBlock {
            state: State::Ready,
            kernel_stack,
            sp,
            qos,
            priority,
            affinity,
            user,
            pass: pass_baseline,
            last_started_cycles: now,
            next_waiter: None,
            resources_released: false,
            #[cfg(feature = "trace")]
            ready_since: now, // Cycle stamp of the last transition into Ready (for wake-latency tracing)
        })
    }
    /// Release a TCB slot based on the given thread handle
    ///
    /// # Panics #
    /// Panics if the slot is already released.
    pub(super) fn release(&mut self, handle: Handle) {
        self.tcbs.take(handle).expect("could not release thread");
    }
    /// Make a blocked thread ready, returning the `UnblockedResult`.
    /// Works for threads that are currently blocked or switching to blocked.
    /// If the thread is not blocked no action is taken and returns
    /// [UnblockedResult::NotBlocked].
    ///
    /// # Panics #
    /// Panics if the thread handle is stale
    pub(super) fn make_blocked_ready(&mut self, handle: Handle) -> UnblockedResult {
        let tcb = self.tcbs.get_mut(handle).expect("should be a valid handle");
        match tcb.state {
            State::Blocked | State::BlockedUntil(_) => {
                tcb.state = State::Ready;
                #[cfg(feature = "trace")]
                {
                    tcb.ready_since = timer::elapsed(); // stamp Ready entry to track how long it stays in this state
                }
                UnblockedResult::Unparked(tcb.affinity)
            }
            State::Switching(PostSwitch::Blocked)
            | State::Switching(PostSwitch::BlockedUntil(_)) => {
                // We were asked to wake up the thread too soon
                UnblockedResult::Deferred
            }
            _ => UnblockedResult::NotBlocked,
        }
    }
    /// Find the minimum current pass value among active, non-idle threads.
    ///
    /// PRI_MIN threads (the idle bootstrap on non-main harts) accummulate
    /// very little stride — they mostly WFI and never switch out — so
    /// including them in the baseline calculation would give every newly
    /// spawned thread a pass of 0, letting it dominate pick_next until its
    /// pass naturally catches up to the rest of the system.
    pub(super) fn pass_baseline(&self) -> u64 {
        self.tcbs
            .iter()
            .filter(|tcb| tcb.state == State::Ready || tcb.state == State::Running)
            .filter(|tcb| tcb.priority != PRIORITY_MIN)
            .map(|tcb| tcb.pass)
            .min()
            .unwrap_or(0)
    }
    /// Wakes a sleeping thread and catches up its pass.
    ///
    /// If the thread is not sleeping returns `None`, otherwise
    /// returns a tuple containing the TCB's
    /// (pass, deadline, affinity)
    pub(super) fn wake_if_due(
        &mut self,
        handle: Handle,
        now: u64,
        pass_baseline: &mut Option<u64>,
        bonus: u64,
    ) -> Option<(u64, u64, Option<u8>)> {
        // Borrows are sequenced, never overlapping: (1) shared borrow of the
        // slot to see whether any work is due, (2) shared borrow of the whole
        // array for the lazy pass baseline, (3) mutable borrow of the slot to
        // apply the wake.
        let at_cycles = match self.tcbs.get(handle)?.state {
            State::Sleeping(deadline) | State::BlockedUntil(deadline) => Some(deadline.at()),
            _ => None,
        }?;
        if at_cycles <= now {
            let baseline = *pass_baseline.get_or_insert_with(|| self.pass_baseline());
            let tcb = self.tcbs.get_mut(handle).unwrap();
            tcb.state = State::Ready;
            tcb.pass = tcb.pass.max(baseline.saturating_sub(bonus));
            #[cfg(feature = "trace")]
            {
                tcb.ready_since = now; // stamp Ready entry (wake-latency tracing)
            }
            Some((tcb.pass, at_cycles, tcb.affinity))
        } else {
            None
        }
    }
    /// Gets the soonest wake deadline (including leeway) including threads busy switching
    /// Returns None if no threads are sleeping
    pub(super) fn next_wake_due(&self) -> Option<u64> {
        self.tcbs.iter().filter_map(|tcb| tcb.must_wake_by()).min()
    }
    /// Returns whether there are contending threads (that will need a slice switch)
    pub(super) fn is_under_contention(&self) -> bool {
        self.tcbs.iter().any(|tcb| tcb.state == State::Ready)
    }
    /// Get the next earliest wakeup including any coalescing
    pub(super) fn next_timer_deadline(&mut self, slice: u64, now: u64) -> u64 {
        let slice_end = if self.is_under_contention() {
            slice + now
        } else {
            u64::MAX
        };
        let next_wake = self.next_wake_due();
        let wake =
            next_wake.inspect(|&coalesce_deadline_cycles| {
                for tcb in self.tcbs.iter_mut() {
                    if let State::Sleeping(deadline)
                    | State::Switching(PostSwitch::Sleeping(deadline)) = &mut tcb.state
                        && deadline.contains(coalesce_deadline_cycles)
                    {
                        deadline.coalesce_to(coalesce_deadline_cycles);
                    }
                }
            });
        slice_end.min(wake.unwrap_or(u64::MAX))
    }

    // Debug - print the painted stack depth for running threads
    #[cfg(feature = "paint-stack")]
    pub(super) fn stacks(&self) {
        for (handle, tcb) in self.tcbs.iter_with_handles() {
            // Print kernel stack
            unsafe {
                print_watermark(
                    "thread",
                    handle.id(),
                    "kernel",
                    tcb.kernel_stack.base_addr(),
                    tcb.kernel_stack.top().as_ptr().addr(),
                )
            }
            if let Some(uc) = &tcb.user {
                // Print user stack
                unsafe {
                    print_watermark(
                        "thread",
                        handle.id(),
                        "user",
                        uc.stack.base_addr(),
                        uc.stack.top().as_ptr().addr(),
                    )
                }
            }
        }
    }
    /// Get the process index of a given thread handle
    ///
    /// Returns `None` if:
    /// - not a user thread
    /// - handle is stale
    pub(super) fn process_idx_of(&self, handle: Handle) -> Option<u8> {
        self.tcbs.get(handle).and_then(|tcb| tcb.process_idx())
    }
    /// Determines if any threads are resource holders for process with `process_idx`
    ///
    /// # Panics #
    /// Panics if `process_idx >= PROCS_MAX`
    pub(super) fn any_resource_holders(&self, process_idx: u8) -> bool {
        assert!((process_idx as usize) < process::MAX);
        self.tcbs.iter().any(|tcb| {
            tcb.user
                .as_ref()
                .is_some_and(|uc| uc.process_idx == process_idx)
                && !tcb.resources_released
        })
    }
}
