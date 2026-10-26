//! Processes
//!
//! Ease uses kernel scheduling for threads within a process. Each process supports
//! multiple threads using the same user memory map.

use super::Deadline;
use super::stride::{SchedInner, Scheduler};
use super::thread;
use super::usermem;
use crate::kernel::collection::{Arena, StackVec};
use crate::kernel::fd;
use crate::kernel::ipi;
use crate::kernel::percpu;
use crate::kernel::sched;
use crate::kernel::sched::usermem::{UserBuf, ValidatedUserBuf};
use crate::kernel::sync::IrqSpinLockGuard;
use core::marker::PhantomData;
use core::ptr::NonNull;

pub(crate) const MAX_COUNT: usize = thread::MAX_COUNT - 2; // Two threads are for idle. All other processes could be single-thread
const MAX_THREADS_PER_PROC: usize = 6;

/// A token to ensure the current thread's process is in Alive state at the
/// start of a syscall.
pub(crate) struct SyscallContext {
    fds: fd::Handle,
    _marker: PhantomData<*const ()>, // !Sync and !Send
}

impl SyscallContext {
    /// Get a new SyscallContext token for the current thread which can be used
    /// to guarantee the process was Alive at the start of the syscall and the fds
    /// are valid for as long as any code holding the token can still run. That's
    /// because the thread stays a resource holder until it exits, and exiting never
    /// returns to that code.
    ///
    /// Takes the scheduler lock.
    ///
    /// # Errors
    /// Returns [Error::ProcessNotInAliveState] if the process is not in Alive state.
    ///
    /// # Panics
    /// Panics if called on a kernel thread
    pub(crate) fn current() -> Result<Self, Error> {
        let fds = sched::SCHEDULER.current_process_fds()?;
        Ok(Self {
            fds,
            _marker: PhantomData,
        })
    }

    /// Validate a U-Mode created `UserBuf` against the current thread's process memory map and
    /// thread stack, returning an `Option<ValidatedUserBuf>`, with `None` indicating that the buffer
    /// did not pass validation or the process is not in Alive state.
    ///
    /// Takes the scheduler lock.
    ///
    /// # Panics
    /// Panics if called by a kernel thread
    pub(crate) fn validate_buf<'a>(&'a self, user_buf: UserBuf) -> Option<ValidatedUserBuf<'a>> {
        let validated_buf =
            sched::SCHEDULER.validate_user_buf(percpu::current_thread(), user_buf)?;
        Some(ValidatedUserBuf::new(
            validated_buf,
            user_buf.direction,
            self,
        ))
    }

    /// Get the SyscallContext process file descriptor table handle
    pub(crate) fn fds(&self) -> fd::Handle {
        self.fds
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    /// Thread creation errors
    Thread(thread::Error),
    /// There are not enough process slots - see [MAX_COUNT]
    NotEnoughProcessSlots,
    /// The given process handle is stale
    ProcessHandleIsStale,
    /// The process is not in Starting state
    ProcessNotInStartingState,
    /// The process is not in Alive state
    ProcessNotInAliveState,
    /// The process is not in Alive or Teardown state
    ProcessNotInAliveOrTeardownState,
    /// A new thread cannot be added to the process as it has its full allotment - see [MAX_THREADS_PER_PROC]
    TooManyThreadsInProcess,
    /// A given thread is not part of this process
    ThreadNotInProcess,
}

impl From<thread::Error> for Error {
    fn from(value: thread::Error) -> Self {
        Error::Thread(value)
    }
}

/// The process is alive and can have threads added; it holds file descriptors as a resource
struct AliveState {
    /// File descriptors are in separate static arena with its own lock so they can be used
    /// without taking the scheduler lock
    fds: fd::Handle,
    /// Forged entry return address for new user threads in this process
    entry_ra: usize,
}

/// The process is being torn down by the `teardown_thread` and will release the file descriptors
struct TeardownState {
    /// File descriptors are still owned by the process at this point
    fds: fd::Handle,
    /// Handle of the thread that performs the resource release for the entire process
    teardown_thread: thread::Handle,
}

/// Process state lifecycle
/// The enum is private - the only source is from the PCB
enum State {
    /// The process is starting. It does not yet have file descriptors
    Starting,
    /// The process is alive. It can add and remove threads, check user buffer memory and work with file descriptors
    Alive(AliveState),
    /// The process is in a teardown state and is releasing its resources. Threads can be released but not added, and user buffers cannot be validated
    Teardown(TeardownState),
    /// The process is exiting. It no longer has any file descriptors and any remaining threads are still being exited
    Exiting,
    /// Used purely to transition between states, this should never be observed.
    Invalid,
}

impl State {
    /// Transition from Starting to Alive
    ///
    /// # Errors
    /// Returns a [Error::ProcessNotInStartingState] error if the process is not in Starting state
    fn begin_alive(&mut self, fds: fd::Handle, entry_ra: usize) -> Result<(), Error> {
        // Safe to replace with the Invalid placeholder, as we hold &mut and restore at the end of the method, so no one else sees Invalid
        match core::mem::replace(self, Self::Invalid) {
            Self::Starting => {
                *self = State::Alive(AliveState { fds, entry_ra });
                Ok(())
            }
            other_state => {
                *self = other_state;
                Err(Error::ProcessNotInStartingState)
            }
        }
    }

    /// Transition from Alive to Teardown
    ///
    /// # Errors
    /// Returns [Error::ProcessNotInAliveState] if the process is not Alive
    fn begin_teardown(&mut self, teardown_thread: thread::Handle) -> Result<(), Error> {
        // Safe to replace with `Invalid` placeholder, as we hold &mut and restore at the end of the method, so no one else sees Invalid
        match core::mem::replace(self, Self::Invalid) {
            Self::Alive(alive_state) => {
                *self = Self::Teardown(TeardownState {
                    fds: alive_state.fds,
                    teardown_thread,
                });
                Ok(())
            }
            other_state => {
                *self = other_state;
                Err(Error::ProcessNotInAliveState)
            }
        }
    }

    /// Transition from Alive or Teardown to Exiting
    ///
    /// # Errors
    /// Returns [Error::ProcessNotInAliveOrTeardownState] if the process is not in one of the two valid states
    ///
    /// # Panics
    /// Panics if called on Starting state
    fn begin_exiting(&mut self) -> Result<fd::Handle, Error> {
        match core::mem::replace(self, Self::Invalid) {
            // Safe to do so, as we hold &mut and restore at the end of the method, so no one else sees Invalid
            Self::Starting => unreachable!(),
            Self::Alive(alive_state) => {
                let fds = alive_state.fds;
                *self = Self::Exiting;
                Ok(fds)
            }
            Self::Teardown(teardown_state) => {
                let fds = teardown_state.fds;
                *self = Self::Exiting;
                Ok(fds)
            }
            Self::Exiting => {
                *self = Self::Exiting;
                Err(Error::ProcessNotInAliveOrTeardownState)
            }
            Self::Invalid => unreachable!(),
        }
    }

    /// On user thread release, return the teardown thread handle if in that state,
    /// otherwise if Alive or Exiting it returns `None`
    fn teardown_thread(&self) -> Option<thread::Handle> {
        match self {
            Self::Starting => unreachable!(),
            Self::Alive(_) => None,
            Self::Teardown(teardown_state) => Some(teardown_state.teardown_thread),
            Self::Exiting => None,
            Self::Invalid => unreachable!(),
        }
    }
}

/// The process control block
pub(crate) struct ControlBlock {
    _name: &'static str,
    state: State,
    /// Process memory map used by all threads (although the threads have individual stacks)
    mem_map: usermem::Map,
}

impl ControlBlock {
    /// Get a borrow on the AliveState for this process. Note that this reborrows
    /// all the way up to the Scheduler lock.  Returns a borrow on the `AliveState` on success.
    ///
    /// # Errors
    /// Returns [Error::ProcessNotInAliveState] on failure.
    fn get_alive(&self) -> Result<&AliveState, Error> {
        if let State::Alive(state) = &self.state {
            Ok(state)
        } else {
            Err(Error::ProcessNotInAliveState)
        }
    }
}

/// Arena that holds the process control blocks
pub(super) struct Processes(Arena<ControlBlock, MAX_COUNT>);

impl Processes {
    /// Create a new Processes arena
    pub(super) const fn new() -> Self {
        Self(Arena::new())
    }

    /// Borrows the process's control block.
    ///
    /// # Panics
    /// Panics if the process handle is stale.
    pub(super) fn pcb(&self, process: Handle) -> &ControlBlock {
        self.0
            .get(process)
            .expect("process handle should not be stale")
    }

    /// Borrows the process's control block.
    ///
    /// # Errors
    /// Returns [Error::ProcessHandleIsStale] if the process handle is stale.
    pub(super) fn get_pcb(&self, process: Handle) -> Result<&ControlBlock, Error> {
        self.0.get(process).ok_or(Error::ProcessHandleIsStale)
    }

    /// Test-only liveness probe: whether the handle still names a PCB.
    #[cfg(test)]
    pub(super) fn contains(&self, process: Handle) -> bool {
        self.0.get(process).is_some()
    }

    /// Exclusively borrows the process's control block.
    ///
    /// # Panics
    /// Panics if the process handle is stale.
    pub(super) fn pcb_mut(&mut self, process: Handle) -> &mut ControlBlock {
        self.0
            .get_mut(process)
            .expect("process handle should not be stale")
    }

    /// Acquire new process control block.
    /// Returns a handle to the process control block on success.
    ///
    /// # Errors
    /// Returns [Error::NotEnoughProcessSlots] if the Processes arena is full.
    pub(super) fn acquire(
        &mut self,
        _name: &'static str,
        mem_map: usermem::Map,
    ) -> Result<Handle, Error> {
        self.0
            .add(ControlBlock {
                _name,
                state: State::Starting,
                mem_map,
            })
            .ok_or(Error::NotEnoughProcessSlots)
    }

    /// Release a process control block only if the process is in Starting or Exiting state.
    ///
    /// The caller guarantees that there are _no_ threads left in the process.
    ///
    /// # Panics
    /// - If the given process handle is stale
    /// - If the process is not in Starting or Exiting state
    pub(super) fn release(&mut self, process: Handle) {
        assert!(
            matches!(self.pcb(process).state, State::Starting | State::Exiting),
            "process must be in Starting or Exiting state"
        );
        self.0.take(process);
    }

    /// Get the memory map for the given process
    ///
    /// # Panics
    /// Panics if the given process handle is stale
    pub(super) fn mem_map(&self, process: Handle) -> &usermem::Map {
        &self.pcb(process).mem_map
    }

    /// Returns the file descriptor handle of the given process if the process is Alive.
    ///
    /// # Errors
    /// - Returns [Error::ProcessHandleIsStale] if the process handle is stale
    /// - Returns [Error::ProcessNotInAliveState] if called on a process that is not Alive
    pub(super) fn get_fds_if_alive(&self, process: Handle) -> Result<fd::Handle, Error> {
        Ok(self.get_pcb(process)?.get_alive()?.fds)
    }
}

/// Alias to simplify the process handle definition
pub(crate) type Handle = crate::kernel::collection::Handle<ControlBlock>;

impl SchedInner {
    /// Checks if a process can accept a new thread, and returns the entry return address if acceptable.
    ///
    /// # Errors
    /// - Returns [Error::TooManyThreadsInProcess] if the process already has [MAX_THREADS_PER_PROC]
    /// - Returns [Error::ProcessNotInAliveState] if the process is not in [State::Alive]
    /// - Returns [Error::ProcessHandleIsStale] if the process handle is stale
    pub(super) fn new_thread_entry_ra(&self, process: Handle) -> Result<usize, Error> {
        let entry_ra = self.processes.get_pcb(process)?.get_alive()?.entry_ra;
        if self.threads.thread_count_of(process) >= MAX_THREADS_PER_PROC {
            Err(Error::TooManyThreadsInProcess)
        } else {
            Ok(entry_ra)
        }
    }

    /// Change the given process from Starting to Alive with given file descriptor and entry return address.
    /// From this point on releasing the process must first entail releasing the process resources.
    ///
    /// # Panics
    /// - If the given process handle is stale
    /// - If the process is not in Starting state
    pub(super) fn commit_alive(&mut self, process: Handle, fds: fd::Handle, entry_ra: usize) {
        // Transition to Alive state
        self.processes
            .pcb_mut(process)
            .state
            .begin_alive(fds, entry_ra)
            .expect("process must be in Starting state")
    }

    /// Change the given process to teardown state using the given thread as the teardown thread that will be
    /// last and release all shared resources.
    ///
    /// Note that the given thread cannot yet have been marked for exit, as it will be marking siblings for exit but
    /// no other thread will do so.
    ///
    /// # Errors
    /// - Returns [Error::ThreadNotInProcess] if the given thread is not part of the given process
    /// - Returns [Error::ProcessNotInAliveState] if called on a process that isn't Alive
    ///
    /// # Panics
    /// Panics if the given process handle is stale
    pub(super) fn claim_teardown_role(
        &mut self,
        process: Handle,
        teardown_thread: thread::Handle,
    ) -> Result<(), Error> {
        // Check if the given thread is part of the process
        if self.threads.process_handle_of(teardown_thread) != Some(process) {
            return Err(Error::ThreadNotInProcess);
        }
        // Transition to teardown state
        self.processes
            .pcb_mut(process)
            .state
            .begin_teardown(teardown_thread)
    }

    /// If this is the last thread still using the process's resources, move the process to Exiting, and return its
    /// fd table so that the files can be closed after dropping the scheduler lock.
    ///
    /// Must be called _after_ [thread::Threads::mark_resources_released].
    ///
    /// Lock order: (1) SCHEDULER (2) FILE_DESCRIPTORS
    ///
    /// # Panics
    /// - If the process handle is stale
    /// - The process is already in Exiting
    pub(super) fn take_fds_if_last_resource_holder(
        &mut self,
        process: Handle,
    ) -> Option<fd::Table> {
        if self.threads.any_resource_holders(process) {
            return None;
        }
        // There are no resource holders, we can release resources
        let fds = self
            .processes
            .pcb_mut(process)
            .state
            .begin_exiting()
            .expect("process must be in Alive or Teardown state");
        let fd_table = fd::take(fds);
        Some(fd_table)
    }

    /// Called whenever a user thread is released.
    ///
    /// Clears the thread wakeup flag. Checks whether the given thread is the teardown thread, and
    /// if not, the teardown thread has its wakeup flag set.
    ///
    /// # Panics
    /// Panics if the given process handle is stale
    pub(super) fn release_user_thread(&mut self, process: Handle, thread: thread::Handle) {
        // For processes that are being torn down, if this is not the teardown thread then set the wakeup flag
        if let Some(teardown_thread) = self.processes.pcb(process).state.teardown_thread()
            && teardown_thread != thread
        {
            sched::set_needs_wakeup(teardown_thread);
            // NOTE - we do not call percpu::set_needs_reschedule as we can happily wait on the next switch
        }
        sched::clear_wakeup_signal(thread);
    }

    /// Attempts to release a process.
    ///
    /// The process is released if no threads remain.
    ///
    /// # Panics
    /// - If the given process handle is stale
    /// - If there are zero threads left in the process but the process is not in Starting or Exiting state
    pub(super) fn release_process_if_empty(&mut self, process: Handle) {
        if self.threads.thread_count_of(process) == 0 {
            // This process is done
            self.processes.release(process);
        }
    }
}

impl Scheduler {
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
                sched.release_user_thread(process, *thread);
            }
        }
    }

    /// Exits the current user thread. If the process is faulting the thread will attempt to take the teardown
    /// role which is responsible for releasing process resources. If it is a normal exit the last thread in
    /// the process will do the same.
    ///
    /// Resources are dropped outside of the scheduler lock.
    ///
    /// # Panics
    /// - If called on a kernel thread
    /// - If the sibling teardown takes too long - see `CLAIM_ROLE_TIMEOUT_MS`
    pub(super) fn exit_current_user_thread(&self, reason: thread::ExitReason) -> ! {
        // Set multiple 100's of milliseconds as some tests show a long tail (over 200ms) of threads waiting to be scheduled under heavy load
        // Also set to < 2_000 which is the individual test timeout
        const CLAIM_ROLE_TIMEOUT_MS: u64 = 1_500;

        let current_thread = percpu::current_thread();
        let mut sched = self.sched.lock();
        let process = sched
            .threads
            .process_handle_of(current_thread)
            .expect("the current thread must be a user thread");
        // Set this thread to be the teardown thread for the entire process, if that role isn't already taken
        let claimed_role = reason == thread::ExitReason::Fault
            && sched.claim_teardown_role(process, current_thread).is_ok();
        if claimed_role {
            self.evict_sibling_threads(&mut sched, reason, process, current_thread);
        }
        drop(sched);
        // If I am the teardown thread, firstly loop waiting for other threads to finish their exits
        if claimed_role {
            // Set a wait timeout in case the user thread hangs; No leeway on this wakeup
            let deadline = Deadline::after_ms(CLAIM_ROLE_TIMEOUT_MS, 0);
            // Loop until all the other threads are done (or panic if its taking too long)
            loop {
                let mut sched = self.sched.lock();
                if sched.threads.thread_count_of(process) == 1 {
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
        // Drop the resources
        let mut sched = self.sched.lock();
        sched.threads.mark_resources_released(current_thread);
        let fd_table = sched.take_fds_if_last_resource_holder(process);
        drop(sched);
        // Drop the file descriptors for this process.
        // Where this is not the last thread, we are dropping `None` which leaves the files in
        // place until the last threads makes the call on `Some(fd_table)` and actually closes the
        // files.
        drop(fd_table);
        // Exit this thread
        self.exit(reason);
    }

    /// Get the current thread's process file descriptor table handle
    /// Returns an error if the current process is not in Alive state.
    ///
    /// # Errors
    /// - Returns [Error::ProcessNotInAliveState] if called on a process that is not Alive.
    ///
    /// # Panics
    /// Panics if the thread is not a user thread
    pub(super) fn current_process_fds(&self) -> Result<fd::Handle, Error> {
        let sched = self.sched.lock();
        let process = sched
            .threads
            .process_handle_of(percpu::current_thread())
            .expect("current thread should be a user thread");
        sched.processes.get_fds_if_alive(process)
    }

    /// Validates a given user buffer against the given user thread and its process memory map.
    ///
    /// Returns `None` if the buffer is not validated or if the process is not in Alive state.
    ///
    /// Returns `Some(NonNull<[u8]>)` if the user buffer is valid.
    ///
    /// # Panics
    /// Panics if:
    /// - the given thread handle is stale
    /// - the thread is not a user thread.
    pub(super) fn validate_user_buf(
        &self,
        thread: thread::Handle,
        user_buf: UserBuf,
    ) -> Option<NonNull<[u8]>> {
        let sched = self.sched.lock();
        // Get the given thread's user context
        let user_context = sched
            .threads
            .tcbs
            .get(thread)
            .expect("thread handle should not be stale")
            .user
            .as_ref()
            .expect("should not run on a kernel thread");
        // Get the process memory map if it is in Alive state
        let pcb = sched.processes.pcb(user_context.process);
        if !matches!(pcb.state, State::Alive(_)) {
            return None;
        }
        pcb.mem_map.validate(&user_context.stack, user_buf)
    }
}
