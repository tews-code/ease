//! Types for the scheduler

use core::ptr::NonNull;

use crate::kernel::alloc::MemRegion;
use crate::kernel::sched::process::{PROCS_MAX, ProcessControlBlock};

pub const THREADS_MAX: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Deadline {
    pub(super) min: u64,
    pub(super) fixed_leeway: Option<u64>, // None - use system default leeway
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ExitReason {
    Exit,
    Fault,
}
const _: () = assert!(ExitReason::Exit as u8 == 0);
const _: () = assert!(ExitReason::Fault as u8 == 1);

#[derive(PartialEq, Debug)]
pub(super) enum PostSwitch {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Sleeping(Deadline),
    Dead(ExitReason),
}

#[derive(PartialEq, Debug)]
pub(super) enum State {
    Avail,
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(Deadline),
}

pub enum Qos {
    High,
    Low,
}

#[expect(dead_code)] // user_entry is read from assembly
pub(super) struct UserContext {
    pub(super) user_stack: MemRegion,
    pub(super) user_entry: extern "C" fn(),
    pub(super) process_idx: u8,
}

pub(super) struct ThreadControlBlock {
    pub(super) id: u32,
    pub(super) state: State,
    pub(super) sp: Option<NonNull<u8>>,
    pub(super) kernel_stack: Option<MemRegion>,
    pub(super) qos: Qos,
    pub(super) priority: u8,             // Lower number is higher priority
    pub(super) pass: u64,                // The next ready thread with lowest pass wins
    pub(super) last_started_cycles: u64, // Cycle stamp from last switch
    pub(super) next_waiter: Option<ThreadHandle>, // Handle of next thread waiting on blocked resource
    pub(super) affinity: Option<u8>,              // Affinity to a particular HART
    pub(super) user: Option<UserContext>, // If is Some then this TCB is supporting a user thread
}

pub(super) struct ThreadsInner {
    pub(super) thread_blocks: [ThreadControlBlock; THREADS_MAX],
    pub(super) process_blocks: [Option<ProcessControlBlock>; PROCS_MAX], // Two thread control blocks are taken up by idle so can't be used for a process
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadHandle {
    pub(super) id: u32,
    pub(super) idx: usize,
}

#[allow(dead_code)]
impl ThreadHandle {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn idx(&self) -> usize {
        self.idx
    }
}
