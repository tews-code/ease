//! Threads

#![allow(dead_code)]

use alloc::fmt::Debug;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU16, Ordering};

use crate::kernel::alloc::MemRegion;
use crate::kernel::sched::Qos;
use crate::kernel::sched::stride::PRIORITY_MIN;
use crate::kernel::stack;
use crate::kernel::timer;

use super::deadline::Deadline;

pub(crate) const THREADS_MAX: usize = 16;

static THREAD_ID_COUNTER: AtomicU16 = AtomicU16::new(0); // Wraps at 65535 but 0 isn't special

#[derive(Clone, Copy, Debug)]
pub(crate) struct ThreadHandle {
    pub(super) idx: usize,
    pub(crate) id: u16,
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ExitReason {
    Exit,
    Fault,
}
const _: () = assert!(ExitReason::Exit as u8 == 0);
const _: () = assert!(ExitReason::Fault as u8 == 1);

#[derive(PartialEq, Debug, Copy, Clone)]
pub(crate) enum PostSwitch {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Sleeping(Deadline),
    Dead(ExitReason),
}

#[derive(PartialEq, Debug, Copy, Clone)]
pub(crate) enum State {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(Deadline),
}

// user_entry is read from assembly
pub(super) struct UserContext {
    pub(super) user_stack: MemRegion,
    pub(super) user_entry: extern "C" fn(),
    pub(super) process_idx: u8,
}

pub(super) struct ThreadControlBlock {
    pub(super) id: u16,
    pub(super) state: State,
    pub(super) kernel_stack: MemRegion,
    pub(super) sp: NonNull<u8>,
    pub(super) qos: Qos,
    pub(super) priority: u8,              // Lower number is higher priority
    pub(super) affinity: Option<u8>,      // Affinity to a particular HART
    pub(super) user: Option<UserContext>, // If is Some then this TCB is supporting a user thread
    pub(super) pass: u64,                 // The next ready thread with lowest pass wins
    pub(super) last_started_cycles: u64,  // Cycle stamp from last switch
    pub(super) next_waiter: Option<ThreadHandle>, // Handle of next thread waiting on blocked resource
    pub(super) marked_for_exit: bool, // If set then thread will be forced to exit on next schedule
    #[cfg(feature = "trace")]
    pub(super) ready_since: u64, // Cycle stamp of the last transition into Ready (for wake-latency tracing)
}

pub(super) struct ThreadControlBlockSpec {
    pub(super) kernel_stack: MemRegion,
    pub(super) qos: Qos,
    pub(super) priority: u8,              // Lower number is higher priority
    pub(super) affinity: Option<u8>,      // Affinity to a particular HART
    pub(super) user: Option<UserContext>, // If is Some then this TCB is supporting a user thread
}

impl ThreadControlBlock {
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
            | State::Switching(PostSwitch::BlockedUntil(deadline)) => {
                let leeway = deadline.leeway(&self.qos);
                Some(deadline.at_cycles.saturating_add(leeway))
            }
            _ => None,
        }
    }

    #[inline(never)]
    fn check_canary(&self) {
        let base_addr = self.kernel_stack.base_addr();
        // Safety: base address is aligned and valid for reads either from linker script or buddy allocation
        if let Err(val) = unsafe { stack::check_canary(base_addr) } {
            panic!(
                "kernel stack canary corrupted in thread {}: sp={:?}, base={:#x}, read={:#x}, expected={:#x}",
                self.id,
                self.sp,
                base_addr,
                val,
                stack::STACK_CANARY
            )
        };
    }
}

impl Debug for ThreadControlBlock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "id: {}", self.id)?;
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
        writeln!(f, "marked_for_exit: {}", self.marked_for_exit)
    }
}

#[derive(Debug)]
pub(super) struct Threads(pub(super) [Option<ThreadControlBlock>; THREADS_MAX]);

impl Threads {
    // Helper function providing an iterator for the index and reference to valid threads
    pub(super) fn iter_indexed(&self) -> impl Iterator<Item = (usize, &ThreadControlBlock)> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(idx, slot)| slot.as_ref().map(|tcb| (idx, tcb)))
    }

    // Helper function to get a reference to a TCB by index
    // Since the index comes from current_thread_idx/handle.idx and are
    // always valid — the Option here is about presence, not bounds.)
    pub(super) fn get(&self, idx: usize) -> Option<&ThreadControlBlock> {
        self.0[idx].as_ref()
    }

    // Helper function to get a mutable reference to a TCB by index
    pub(super) fn get_mut(&mut self, idx: usize) -> Option<&mut ThreadControlBlock> {
        self.0[idx].as_mut()
    }

    // (and _mut) — self.0.iter().enumerate().filter_map(|(i, slot)| slot.as_ref().map(|t| (i, t)))
    // Acquire a free slot and set up a valid thread control block
    pub(super) fn acquire(
        &mut self,
        forge_stack: impl FnOnce(&mut MemRegion) -> NonNull<u8>,
        spec: ThreadControlBlockSpec,
    ) -> Option<ThreadHandle> {
        let idx = self.0.iter().position(|tcb| tcb.is_none())?;
        let pass_baseline = self.pass_baseline();
        let id = THREAD_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ThreadControlBlockSpec {
            qos,
            priority,
            mut kernel_stack,
            affinity,
            user,
        } = spec;
        let sp = forge_stack(&mut kernel_stack);
        let now = timer::elapsed();
        self.0[idx] = Some(ThreadControlBlock {
            id,
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
            marked_for_exit: false,
            #[cfg(feature = "trace")]
            ready_since: now, // Cycle stamp of the last transition into Ready (for wake-latency tracing)
        });
        Some(ThreadHandle { idx, id })
    }

    pub(super) fn release(&mut self, thread: &ThreadHandle) {
        if self.0[thread.idx]
            .as_ref()
            .is_some_and(|tcb| tcb.id == thread.id)
        {
            self.0[thread.idx] = None;
        } else {
            panic!("could not release thread");
        }
    }

    // Find the minimum current pass value among active, non-idle threads.
    //
    // PRI_MIN threads (the idle bootstrap on non-main harts) accumulate
    // very little stride — they mostly WFI and never switch out — so
    // including them in the baseline calculation would give every newly
    // spawned thread a pass of 0, letting it dominate pick_next until its
    // pass naturally catches up to the rest of the system.
    pub(super) fn pass_baseline(&self) -> u64 {
        self.0
            .iter()
            .flatten()
            .filter(|tcb| tcb.state == State::Ready || tcb.state == State::Running)
            .filter(|tcb| tcb.priority != PRIORITY_MIN)
            .map(|tcb| tcb.pass)
            .min()
            .unwrap_or(0)
    }

    // Gets the soonest wake deadline (including leeway) including threads busy switching
    // Returns None if no threads are sleeping
    pub(super) fn next_wake_due(&self) -> Option<u64> {
        self.0
            .iter()
            .flatten()
            .filter_map(|tcb| tcb.must_wake_by())
            .min()
    }

    // Wakes a sleeping TCB and catches up its pass
    pub(super) fn wake_if_due(
        &mut self,
        idx: usize,
        now: u64,
        pass_baseline: &mut Option<u64>,
        bonus: u64,
    ) -> Option<(u64, u64, Option<u8>)> {
        // First take a immutable borrow to see whether we need to do any work here
        let at_cycles = if let Some(tcb) = &self.0[idx] {
            match tcb.state {
                State::Sleeping(Deadline {
                    at_cycles,
                    fixed_leeway: _,
                })
                | State::BlockedUntil(Deadline {
                    at_cycles,
                    fixed_leeway: _,
                }) => Some(at_cycles),
                _ => None,
            }
        } else {
            None
        }?;
        if at_cycles <= now {
            let baseline = *pass_baseline.get_or_insert_with(|| self.pass_baseline());
            let tcb = self.0[idx].as_mut().unwrap();
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

    // Returns whether there are contending threads (that will need a slice switch)
    pub(super) fn is_under_contention(&self) -> bool {
        self.0.iter().flatten().any(|tcb| tcb.state == State::Ready)
    }

    // Get the next earliest wakeup including any coalescing
    pub(super) fn next_timer_deadline(&mut self, slice: u64, now: u64) -> u64 {
        let slice_end = if self.is_under_contention() {
            slice + now
        } else {
            u64::MAX
        };
        let next_wake = self.next_wake_due();
        let wake = next_wake.inspect(|&b| {
            for slot in &mut self.0 {
                if let Some(tcb) = slot
                    && let State::Sleeping(deadline)
                    | State::Switching(PostSwitch::Sleeping(deadline)) = &mut tcb.state
                {
                    let leeway = deadline.leeway(&tcb.qos);
                    if b >= deadline.at_cycles && b <= deadline.at_cycles.saturating_add(leeway) {
                        deadline.at_cycles = b;
                        deadline.fixed_leeway = Some(0);
                    } else if deadline.fixed_leeway.is_none() {
                        deadline.fixed_leeway = Some(leeway);
                    }
                }
            }
        });
        slice_end.min(wake.unwrap_or(u64::MAX))
    }

    // Returns whether the thread belongs to a process `pid`
    pub(super) fn belongs_to_process(&self, idx: usize, pid: usize) -> bool {
        self.0[idx]
            .as_ref()
            .and_then(|tcb| tcb.user.as_ref())
            .is_some_and(|uc| uc.process_idx as usize == pid)
    }
}
