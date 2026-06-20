//! Threads

#![allow(dead_code)]

use alloc::fmt::Debug;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU16, Ordering};

use super::deadline::Deadline;
use crate::kernel::alloc::MemRegion;

const THREADS_MAX: usize = 16;

static THREAD_ID_COUNTER: AtomicU16 = AtomicU16::new(0); // Wraps at 65535 but 0 isn't special

#[derive(Debug)]
struct ThreadHandle {
    idx: usize,
    id: u16,
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Debug)]
enum ExitReason {
    Exit,
    Fault,
}
const _: () = assert!(ExitReason::Exit as u8 == 0);
const _: () = assert!(ExitReason::Fault as u8 == 1);

#[derive(PartialEq, Debug, Copy, Clone)]
enum PostSwitch {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Sleeping(Deadline),
    Dead(ExitReason),
}

#[derive(PartialEq, Debug, Copy, Clone)]
enum State {
    Blocked,
    BlockedUntil(Deadline),
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(Deadline),
}

#[derive(Debug, Copy, Clone)]
pub(super) enum Qos {
    High,
    Low,
}

#[expect(dead_code)] // user_entry is read from assembly
struct UserContext {
    user_stack: MemRegion,
    user_entry: extern "C" fn(),
    process_idx: u8,
}

struct ThreadControlBlock {
    id: u16,
    state: State,
    sp: NonNull<u8>,
    kernel_stack: MemRegion,
    qos: Qos,
    priority: u8,                      // Lower number is higher priority
    pass: u64,                         // The next ready thread with lowest pass wins
    last_started_cycles: u64,          // Cycle stamp from last switch
    next_waiter: Option<ThreadHandle>, // Handle of next thread waiting on blocked resource
    affinity: Option<u8>,              // Affinity to a particular HART
    user: Option<UserContext>,         // If is Some then this TCB is supporting a user thread
    marked_for_exit: bool,             // If set then thread will be forced to exit on next schedule
    ready_since: u64, // Cycle stamp of the last transition into Ready (for wake-latency tracing)
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
        writeln!(f, "marked_for_exit: {}", self.marked_for_exit)?;
        writeln!(f, "ready_since: {}", self.ready_since)
    }
}

struct Threads([Option<ThreadControlBlock>; THREADS_MAX]);

impl Threads {
    pub(super) fn acquire(
        &mut self,
        build_tcb: impl FnOnce(u16) -> ThreadControlBlock,
    ) -> Option<ThreadHandle> {
        let (idx, slot) = self
            .0
            .iter_mut()
            .enumerate()
            .find(|(_, tcb)| tcb.is_none())?;
        let id = THREAD_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        *slot = Some(build_tcb(id));
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
}
