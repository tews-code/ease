//! Types for the scheduler

use core::ptr::NonNull;

use crate::kernel::alloc::MemRegion;
use crate::kernel::sched::usermemmap::UserMemMap;

pub const THREADS_MAX: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Deadline {
    pub(super) min: u64,
    pub(super) fixed_leeway: Option<u64>, // None - use system default leeway
}

#[derive(PartialEq, Debug)]
pub(super) enum PostSwitch {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Sleeping(Deadline),
    Dead,
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

pub(super) struct UserContext {
    pub(super) user_mem_map: UserMemMap,
    pub(super) user_entry: extern "C" fn(),
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
    pub(super) control_blocks: [ThreadControlBlock; THREADS_MAX],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadHandle {
    pub(super) id: u32,
    pub(super) idx: usize,
}

impl ThreadHandle {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn idx(&self) -> usize {
        self.idx
    }
}
