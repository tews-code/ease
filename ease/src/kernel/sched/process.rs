//! U-mode processes

// Ease uses kernel scheduling for threads within a process

use core::sync::atomic::{AtomicU32, Ordering};

use crate::kernel::fd;
use crate::kernel::sched::{THREADS_MAX, clear_wakeup_signal, usermemmap::UserMemMap};

use super::stride::SchedInner;

pub(crate) const PROCS_MAX: usize = THREADS_MAX - 2; // Two threads are for idle. All other processes could be single-thread
const THREADS_PER_PROC_MAX: u8 = 6;

static PID_COUNTER: AtomicU32 = AtomicU32::new(0);

pub(crate) struct ProcessHandle {
    pub(super) pid: u32,
    pub(super) idx: usize,
}

pub(crate) struct ProcessControlBlock {
    pub(super) pid: u32,
    _name: &'static str,
    pub(super) mem_map: UserMemMap,
    pub(crate) fds: fd::Table,
    thread_count: u8,
}

impl ProcessControlBlock {
    pub(super) fn new(_name: &'static str, mem_map: UserMemMap) -> Self {
        Self {
            pid: PID_COUNTER.fetch_add(1, Ordering::Relaxed),
            _name,
            mem_map,
            fds: fd::Table::new(),
            thread_count: 0,
        }
    }

    // Get the thread count
    pub(super) fn thread_count(&self) -> u8 {
        self.thread_count
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

impl Procs {
    pub(super) fn find_process_slot(&self) -> Option<usize> {
        self.0.iter().position(|pcb| pcb.is_none())
    }

    /// Installs a new process control block `pcb` into a slot at index `idx`
    ///
    /// The existing slot is always empty
    pub(super) fn install_process_control_block(
        &mut self,
        idx: usize,
        pcb: ProcessControlBlock,
    ) -> ProcessHandle {
        let pid = pcb.pid;
        assert!(self.0[idx].is_none());
        self.0[idx] = Some(pcb);
        ProcessHandle { pid, idx }
    }
}

impl SchedInner {
    // Decrements the process thread count and releases the process control
    // block if the thread count reaches zero.
    // Also removes any wakeup flag in the scheduler associated with the thread
    pub(super) fn release_process_thread(&mut self, process_idx: u8) {
        clear_wakeup_signal(process_idx as usize);
        let thread_count = self.process_blocks.0[process_idx as usize]
            .as_mut()
            .expect("should only be decrementing thread count on a valid process control block")
            .dec_thread_count();
        if thread_count == 0 {
            // Make sure the file descriptors have been cleaned up before emptying the slot
            // Known gap - if two threads of one user process voluntarily exit at the same time and see
            // the fds table at the same and don't remove the fds
            assert!(
                self.process_blocks.0[process_idx as usize]
                    .as_mut()
                    .unwrap()
                    .fds
                    .is_empty(),
                "process control block has no threads but still has open file descriptors"
            );
            self.process_blocks.0[process_idx as usize] = None;
        }
    }
}
