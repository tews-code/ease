//! U-mode processes
//!
//! Ease uses kernel scheduling for threads within a process

use core::sync::atomic::{AtomicU32, Ordering};

use super::stride::{SchedInner, Scheduler};
use super::threads::PostSwitch;
use super::userloader;
use super::usermem;
use super::{ExitReason, State, THREADS_MAX, clear_wakeup_signal, set_needs_wakeup};
use crate::arch::hart_id;
use crate::kernel::fd;
use crate::kernel::ipi;
use crate::kernel::percpu;
use crate::kernel::sync::IrqSpinLockGuard;

pub(crate) const MAX: usize = THREADS_MAX - 2; // Two threads are for idle. All other processes could be single-thread
const THREADS_PER_PROC_MAX: u8 = 6;

static PID_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Process spawn errors
#[derive(Debug)]
pub(crate) enum SpawnError {
    Load(userloader::Error),
    NotEnoughMemory,
    NotEnoughThreads,
    NotFound,
    TooManyProcesses,
}

impl From<userloader::Error> for SpawnError {
    fn from(value: userloader::Error) -> Self {
        SpawnError::Load(value)
    }
}

pub(crate) struct Handle {
    pub(super) pid: u32,
    pub(super) idx: usize,
}

pub(crate) struct ControlBlock {
    pub(super) pid: u32,
    _name: &'static str,
    pub(super) mem_map: usermem::Map,
    pub(crate) fds: fd::Table,
    thread_count: u8,
    pub(super) entry_ra: usize, // Inserted into the 'ra' register slot in the forged trap return
    teardown_thread: Option<usize>, // Index of the thread that performs the resource release for the entire process
}

impl ControlBlock {
    pub(super) fn new(_name: &'static str, mem_map: usermem::Map, entry_ra: usize) -> Self {
        Self {
            pid: PID_COUNTER.fetch_add(1, Ordering::Relaxed),
            _name,
            mem_map,
            fds: fd::Table::new(),
            thread_count: 0,
            entry_ra,
            teardown_thread: None,
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

    // Sets the teardown thread
    //
    // Returns `true` on success or `false` if the teardown thread is already set
    pub(super) fn set_teardown_thread(&mut self, thread_idx: usize) -> bool {
        if self.teardown_thread.is_none() {
            self.teardown_thread = Some(thread_idx);
            true
        } else {
            false
        }
    }
}

pub(super) struct Procs(pub(super) [Option<ControlBlock>; MAX]);

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
        pcb: ControlBlock,
    ) -> Handle {
        let pid = pcb.pid;
        assert!(self.0[idx].is_none());
        self.0[idx] = Some(pcb);
        Handle { pid, idx }
    }
}

impl SchedInner {
    /// Claim the file descriptor table of a process from the last (cleanup) thread
    ///
    /// Returns None if another thread has not yet released its resources.
    /// # Panics #
    /// Panics if called on a kernel thread
    pub(super) fn claim_fds(
        &mut self,
        process_idx: u8,
        thread_idx: usize,
    ) -> Option<[Option<fd::Kind>; fd::MAX]> {
        // Set this thread to no longer use resources
        self.thread_blocks.0[thread_idx]
            .as_mut()
            .expect("current thread must have a valid TCB set up")
            .resources_released = true;
        if !self.thread_blocks.any_resource_holders(process_idx) {
            Some(
                self.process_blocks.0[process_idx as usize]
                    .as_mut()
                    .unwrap()
                    .fds
                    .take_all(),
            )
        } else {
            None
        }
    }
    /// Attempt to claim the resource freeing role as the first exiting thread in a faulting process.
    /// This method does not check the validitiy of the given thread index
    ///
    /// Returns `true` if the role was claimed, otherwise `false` if the role has already been claimed
    /// by another thread.
    ///
    /// # Panics #
    /// Panics if:
    /// - the given process_idx >= MAX
    /// - the process_idx doesn't correspond to a valid PCB
    pub(super) fn claim_teardown_role(&mut self, process_idx: u8, thread_idx: usize) -> bool {
        self.process_blocks.0[process_idx as usize]
            .as_mut()
            .expect("should be a valid process")
            .set_teardown_thread(thread_idx)
    }
    /// Get the current thread count of the given process index
    ///
    /// # Panics #
    /// Panics if:
    /// - the given process_idx >= MAX
    /// - the process_idx doesn't correspond to a valid PCB
    pub(super) fn thread_count(&self, process_idx: u8) -> u8 {
        self.process_blocks.0[process_idx as usize]
            .as_ref()
            .expect("process index must index a valid PCB slot")
            .thread_count()
    }
}

impl Scheduler {
    /// Decrements the process thread count and releases the process control
    /// block if the thread count reaches zero.
    /// Also removes any wakeup flag in the scheduler associated with the thread
    ///
    /// # Panics #
    /// Panics if
    /// - the process id >= MAX
    /// - process id does not correspond to a valid PCB
    /// - decrementing the number of processes when there are none left
    /// - the file descriptor table is not empty
    pub(super) fn release_process_thread(
        &self,
        sched: &mut IrqSpinLockGuard<SchedInner>,
        process_idx: u8,
        thread_idx: usize,
    ) {
        clear_wakeup_signal(thread_idx);
        let thread_count = sched.process_blocks.0[process_idx as usize]
            .as_mut()
            .expect("should only be decrementing thread count on a valid process control block")
            .dec_thread_count();
        // Check if there is a declared teardown claimant and wake them in case they need to get going - but skip ourselves
        if let Some(teardown_thread_idx) = sched.process_blocks.0[process_idx as usize]
            .as_ref()
            .and_then(|pcb| pcb.teardown_thread)
            && teardown_thread_idx != thread_idx
        {
            set_needs_wakeup(teardown_thread_idx);
            // NOTE - we do not call percpu::set_needs_reschedule as we can happily wait on the next switch
        }
        if thread_count == 0 {
            // Make sure the file descriptors have been cleaned up before emptying the slot
            assert!(
                sched.process_blocks.0[process_idx as usize]
                    .as_mut()
                    .unwrap()
                    .fds
                    .is_empty(),
                "process control block has no threads but still has open file descriptors"
            );
            sched.process_blocks.0[process_idx as usize] = None;
        }
    }
    /// Evict sibling threads for a faulting multi-thread user process
    pub(super) fn evict_sibling_threads(
        &self,
        sched: &mut IrqSpinLockGuard<SchedInner>,
        exit_reason: ExitReason,
        process_idx: u8,
        surviving_thread_idx: usize,
    ) {
        for idx in 0..THREADS_MAX {
            let mut release = false;
            if idx == surviving_thread_idx {
                continue;
            }
            if sched.thread_blocks.process_idx_of(idx) == Some(process_idx) {
                let tcb = sched.thread_blocks.0[idx].as_mut().unwrap();
                match tcb.state {
                    State::Blocked | State::BlockedUntil(_) => {
                        // Set marked for exit
                        tcb.marked_for_exit = true;
                        // Now set to Ready
                        let (did_unpark, affinity) = sched.thread_blocks.make_blocked_ready(idx);
                        if !did_unpark {
                            panic!("did not unpark blocked user thread");
                        }
                        if let Some(hart) = affinity
                            && hart as usize != crate::arch::hart_id()
                        {
                            // This is for the other HART
                            ipi::send(hart as usize);
                        } else {
                            // This is for us
                            percpu::set_needs_reschedule();
                        }
                    }
                    State::Ready | State::Sleeping(_) => release = true,
                    State::Running => {
                        // If it is running, it must be on the other HART so send an IPI
                        tcb.marked_for_exit = true;
                        ipi::send(hart_id() ^ 1);
                    }
                    State::Switching(_) => {
                        tcb.state = State::Switching(PostSwitch::Dead(exit_reason))
                    }
                }
            }
            if release {
                sched.thread_blocks.0[idx] = None;
                self.release_process_thread(sched, process_idx, idx)
            }
        }
    }
    /// Exits a user thread. If the thread is faulting it will attempt to take responsibility
    /// for releasing process resources. If it is a normal exit the last thread in the process will
    /// do the same.
    ///
    /// # Panics #
    /// Panics if
    /// - called on a kernel thread
    pub(super) fn exit_user_thread(&self, reason: ExitReason) -> ! {
        let current_thread_idx = percpu::current_thread_idx();
        let mut sched = self.sched.lock();
        let process_idx = sched
            .thread_blocks
            .process_idx_of(current_thread_idx)
            .expect("the current thread must be a user thread which is part of a process");
        // Set this thread to be the teardown thread for the entire process, if that role isn't already taken
        let claimed_role = reason == ExitReason::Fault
            && sched.claim_teardown_role(process_idx, current_thread_idx);
        if claimed_role {
            self.evict_sibling_threads(&mut sched, reason, process_idx, current_thread_idx);
        }
        drop(sched);
        // If I am the teardown thread, firstly loop waiting for other threads to finish their exits
        if claimed_role {
            loop {
                let mut sched = self.sched.lock();
                if sched.thread_count(process_idx) == 1 {
                    break;
                }
                sched.thread_blocks.0[current_thread_idx]
                    .as_mut()
                    .expect("current thread must have a valid TCB")
                    .state = State::Blocked;
                drop(sched);
                self.park_if_blocked();
            }
        }
        // Mark this thread as no longer using resources
        let fds = self.sched.lock().claim_fds(process_idx, current_thread_idx);
        if let Some(fds) = fds {
            fd::Table::close_all(fds);
        }
        // Exit this thread
        self.exit(reason);
    }
}
