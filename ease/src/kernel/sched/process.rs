//! U-mode processes
//!
//! Ease uses kernel scheduling for threads within a process

use super::Deadline;
use super::stride::{SchedInner, Scheduler};
use super::thread;
use super::userloader;
use super::usermem;
use crate::kernel::collection::{Arena, StackVec};
use crate::kernel::fd;
use crate::kernel::ipi;
use crate::kernel::percpu;
use crate::kernel::sched;
use crate::kernel::sync::IrqSpinLockGuard;

pub(crate) const MAX_COUNT: usize = thread::MAX_COUNT - 2; // Two threads are for idle. All other processes could be single-thread
const MAX_THREADS_PER_PROC: u8 = 6;

/// Process spawn errors
#[derive(Debug)]
pub(crate) enum SpawnError {
    Load(userloader::Error),
    NotEnoughMemory,
    NotEnoughThreadSlots,
    NotEnoughProcessSlots,
    NotFound,
}

impl From<userloader::Error> for SpawnError {
    fn from(value: userloader::Error) -> Self {
        SpawnError::Load(value)
    }
}
/// The process control block
pub(crate) struct ControlBlock {
    _name: &'static str,
    pub(super) mem_map: usermem::Map,
    pub(super) fds: fd::Table,
    thread_count: u8,
    pub(super) entry_ra: usize, // Inserted into the 'ra' register slot in the forged trap return
    pub(super) teardown_thread: Option<thread::Handle>, // Handle of the thread that performs the resource release for the entire process
}

impl ControlBlock {
    pub(super) fn new(_name: &'static str, mem_map: usermem::Map, entry_ra: usize) -> Self {
        Self {
            _name,
            mem_map,
            fds: fd::Table::new(),
            thread_count: 0,
            entry_ra,
            teardown_thread: None,
        }
    }
    /// Get the thread count of this process.
    pub(super) fn thread_count(&self) -> u8 {
        self.thread_count
    }
    /// Adds to the thread count and returns the new value if not above the cap
    ///
    /// Errors if the process can't have any more threads
    pub(super) fn add_thread_count(&mut self) -> Result<u8, SpawnError> {
        if self.thread_count < MAX_THREADS_PER_PROC {
            self.thread_count += 1;
            Ok(self.thread_count)
        } else {
            Err(SpawnError::NotEnoughThreadSlots)
        }
    }
    /// Decrements the process thread count, returns new count.
    ///
    /// # Panics
    /// Panics if there are zero threads left
    fn dec_thread_count(&mut self) -> u8 {
        assert!(
            self.thread_count > 0,
            "releasing a thread from a process with zero threads"
        );
        self.thread_count -= 1;
        self.thread_count
    }
    /// Sets the process teardown thread. This thread takes responsbility for
    /// releasing any shared process resource. There can be only one.
    ///
    /// Returns `true` on success or `false` if the teardown thread is already set
    pub(super) fn set_teardown_thread(&mut self, handle: thread::Handle) -> bool {
        if self.teardown_thread.is_none() {
            self.teardown_thread = Some(handle);
            true
        } else {
            false
        }
    }
}

pub(super) struct Processes {
    pub(super) pcbs: Arena<ControlBlock, MAX_COUNT>,
}

/// Alias to simplify the process handle definition
pub(crate) type Handle = crate::kernel::collection::Handle<ControlBlock>;

impl SchedInner {
    /// Claim the file descriptor table of a process from the last (cleanup) thread
    ///
    /// Returns None if another thread has not yet released its resources.
    /// # Panics #
    /// Panics if the thread handle is stale.
    pub(super) fn claim_fds(
        &mut self,
        process: Handle,
        thread: thread::Handle,
    ) -> Option<[Option<fd::Kind>; fd::MAX]> {
        // Set this thread to no longer use resources
        self.threads
            .tcbs
            .get_mut(thread)
            .expect("current thread must have a valid TCB set up")
            .resources_released = true;
        // Take the file descriptors from this process
        if !self.threads.any_resource_holders(process) {
            Some(self.processes.pcbs.get_mut(process).unwrap().fds.take_all())
        } else {
            None
        }
    }
    /// Attempt to claim the resource freeing role as a thread in a faulting process.
    /// This method does not check the validitiy of the given thread handle.
    ///
    /// Returns `true` if the role was claimed, otherwise `false` if the role has already been claimed
    /// by another thread.
    ///
    /// # Panics #
    /// Panics if the process handle is stale or invalid.
    pub(super) fn claim_teardown_role(&mut self, process: Handle, thread: thread::Handle) -> bool {
        self.processes
            .pcbs
            .get_mut(process)
            .expect("should be a valid process")
            .set_teardown_thread(thread)
    }
    /// Get the current thread count of the given process index
    ///
    /// # Panics #
    /// Panics if the process handle is stale or invalid
    pub(super) fn thread_count(&self, process: Handle) -> u8 {
        self.processes
            .pcbs
            .get(process)
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
    /// - decrementing the number of threads when there are none left
    /// - the file descriptor table is not empty
    pub(super) fn release_process_thread(
        &self,
        sched: &mut IrqSpinLockGuard<SchedInner>,
        process: Handle,
        thread: thread::Handle,
    ) {
        sched::clear_wakeup_signal(thread);
        let process_control_block = sched
            .processes
            .pcbs
            .get_mut(process)
            .expect("PCB should be valid for this process handle");
        let new_thread_count = process_control_block.dec_thread_count();
        // Check if there is a declared teardown claimant and wake them in case they need to get going - but skip if it happens to be the given thread
        if let Some(teardown_thread) = process_control_block.teardown_thread
            && teardown_thread != thread
        {
            sched::set_needs_wakeup(teardown_thread);
            // NOTE - we do not call percpu::set_needs_reschedule as we can happily wait on the next switch
        }
        if new_thread_count == 0 {
            // Make sure the file descriptors have been cleaned up before emptying the slot
            assert!(
                process_control_block.fds.is_empty(),
                "process control block has no threads but still has open file descriptors"
            );
            let _ = sched.processes.pcbs.take(process);
        }
    }
    /// Evict sibling threads for a faulting multi-thread user process
    pub(super) fn evict_sibling_threads(
        &self,
        sched: &mut IrqSpinLockGuard<SchedInner>,
        exit_reason: thread::ExitReason,
        process: Handle,
        surviving_thread: thread::Handle,
    ) {
        let mut sibling_threads: StackVec<thread::Handle, { thread::MAX_COUNT - 3 }> =
            StackVec::new(); // Exclude 2 idle threads and ourselves = 3 threads
        for (thread, tcb) in sched.threads.tcbs.iter_with_handles() {
            if thread == surviving_thread {
                continue;
            }
            if tcb.user.as_ref().is_some_and(|uc| uc.process == process) {
                sibling_threads
                    .push(thread)
                    .expect("should be enough space on the StackVec");
            }
        }
        for thread in sibling_threads.iter() {
            let mut release = false;
            match sched.threads.tcbs.get(*thread).unwrap().state {
                thread::State::Blocked | thread::State::BlockedUntil(_) => {
                    // Set marked for exit
                    self.needs_user_exit.set(thread.idx());
                    // Now set to Ready
                    match sched.threads.make_blocked_ready(*thread) {
                        thread::UnblockedResult::NotBlocked | thread::UnblockedResult::Deferred => {
                            panic!("did not unpark blocked user thread")
                        }
                        thread::UnblockedResult::Unparked(affinity) => {
                            if let Some(hart) = affinity
                                && hart as usize != crate::arch::hart_id()
                            {
                                // This is for the other HART
                                ipi::send(ipi::RESCHEDULE);
                            } else {
                                // This is for us
                                percpu::set_needs_reschedule();
                            }
                        }
                    }
                }
                thread::State::Ready | thread::State::Sleeping(_) => release = true,
                thread::State::Running => {
                    // If it is running, it must be on the other HART so send an IPI
                    self.needs_user_exit.set(thread.idx());
                    ipi::send(ipi::RESCHEDULE);
                }
                thread::State::Switching(_) => {
                    sched.threads.tcbs.get_mut(*thread).unwrap().state =
                        thread::State::Switching(thread::PostSwitch::Dead(exit_reason))
                }
            }
            if release {
                sched.threads.release(*thread);
                self.release_process_thread(sched, process, *thread)
            }
        }
    }
    /// Exits the current user thread. If the thread is faulting it will attempt to take responsibility
    /// for releasing process resources. If it is a normal exit the last thread in the process will
    /// do the same.
    ///
    /// # Panics #
    /// Panics if called on a kernel thread
    pub(super) fn exit_current_user_thread(&self, reason: thread::ExitReason) -> ! {
        // Set multiple 100's of milliseconds as some tests show a long tail (over 200ms) of threads waiting to be scheduled under heavy load
        // Also set to < 2_000 which is the individual test timeout
        const CLAIM_ROLE_TIMEOUT_MS: u64 = 1_500;

        let current_thread = percpu::current_thread();
        let mut sched = self.sched.lock();
        let process = sched
            .threads
            .process_handle_of(current_thread)
            .expect("the current thread must be a user thread which is part of a process");
        // Set this thread to be the teardown thread for the entire process, if that role isn't already taken
        let claimed_role = reason == thread::ExitReason::Fault
            && sched.claim_teardown_role(process, current_thread);
        if claimed_role {
            self.evict_sibling_threads(&mut sched, reason, process, current_thread);
        }
        drop(sched);
        // If I am the teardown thread, firstly loop waiting for other threads to finish their exits
        if claimed_role {
            // Set a wait timeout in case the user thread hangs; No leeway on this wakeup
            let deadline = Deadline::after_ms(CLAIM_ROLE_TIMEOUT_MS, 0);
            loop {
                let mut sched = self.sched.lock();
                if sched.thread_count(process) == 1 {
                    break;
                }
                if deadline.has_passed() {
                    dprintln!(
                        "User thread {:?} timed out waiting for siblings to exit for process {:?}",
                        current_thread,
                        process
                    );
                    dprintln!("TCB Table:");
                    for (handle, tcb) in sched.threads.tcbs.iter_with_handles() {
                        dprintln!("--------------");
                        dprintln!("{:?} - {:?}\n", handle, tcb);
                    }
                    drop(sched);
                    panic!("Unable to exit faulting user process");
                }
                sched
                    .threads
                    .tcbs
                    .get_mut(current_thread)
                    .expect("current thread must have a valid TCB")
                    .state = thread::State::BlockedUntil(deadline);
                drop(sched);
                self.park_if_blocked_until(deadline);
            }
        }
        // Mark this thread as no longer using resources
        let fds = self.sched.lock().claim_fds(process, current_thread);
        if let Some(fds) = fds {
            fd::Table::close_all(fds);
        }
        // Exit this thread
        self.exit(reason);
    }
}
