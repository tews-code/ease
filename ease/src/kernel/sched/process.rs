//! U-mode processes

// Ease uses kernel scheduling for threads within a process

use core::sync::atomic::{AtomicU32, Ordering};

use super::stride::SchedInner;
use crate::kernel::sched::{THREADS_MAX, usermemmap::UserMemMap};

pub(super) const PROCS_MAX: usize = THREADS_MAX - 2; // Two threads are for idle. All other processes could be single-thread
const THREADS_PER_PROC_MAX: u8 = 6;

static PID_COUNTER: AtomicU32 = AtomicU32::new(0);

pub(crate) struct ProcessHandle {
    pub(super) pid: u32,
    pub(super) idx: usize,
}

#[allow(dead_code)]
pub(super) struct ProcessControlBlock {
    pub(super) pid: u32,
    name: &'static str,
    pub(super) mem_map: UserMemMap,
    thread_count: u8,
}

impl ProcessControlBlock {
    pub(super) fn new(name: &'static str, mem_map: UserMemMap) -> Self {
        Self {
            pid: PID_COUNTER.fetch_add(1, Ordering::Relaxed),
            name,
            mem_map,
            thread_count: 0,
        }
    }

    // Adds to the thread count and returns the new value if not above the cap
    //
    // Errors if the process can't have any more threads
    pub(super) fn add_thread_count(&mut self) -> Result<u8, ()> {
        if self.thread_count < THREADS_PER_PROC_MAX {
            self.thread_count += 1;
            Ok(self.thread_count)
        } else {
            Err(())
        }
    }

    // Decrements the process thread count, returns new count.
    fn dec_thread_count(&mut self) -> u8 {
        assert!(
            self.thread_count > 0,
            "process has zero threads so cannot remove another"
        );
        self.thread_count -= 1;
        self.thread_count
    }
}

pub(super) struct Procs(pub(super) [Option<ProcessControlBlock>; PROCS_MAX]);

impl SchedInner {
    pub(super) fn find_process_slot(&self) -> Option<usize> {
        self.process_blocks.0.iter().position(|pcb| pcb.is_none())
    }

    pub(super) fn install_process_control_block(
        &mut self,
        idx: usize,
        pcb: ProcessControlBlock,
    ) -> ProcessHandle {
        let pid = pcb.pid;
        self.process_blocks.0[idx] = Some(pcb);
        ProcessHandle { pid, idx }
    }

    // Decrements the process thread count and releases the process control
    // block if the thread count reaches zero.
    pub(super) fn release_process_thread(&mut self, process_idx: u8) {
        let thread_count = self.process_blocks.0[process_idx as usize]
            .as_mut()
            .expect("should only be decrementing thread count on a valid process control block")
            .dec_thread_count();
        if thread_count == 0 {
            self.process_blocks.0[process_idx as usize] = None;
        }
    }
}
