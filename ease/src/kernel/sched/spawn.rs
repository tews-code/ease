//! Spawn processes and threads

use crate::arch::{context, umode};
use crate::kernel::alloc::{self, MemRegion, Order};
use crate::kernel::fd;
use crate::kernel::ipi;
use crate::kernel::percpu;
use crate::kernel::sync::IrqSpinLockGuard;
use crate::kernel::timer;

use super::Qos;
use super::process;
use super::stride::{SLICE, SchedInner, Scheduler};
use super::thread;
use super::userloader;

/// Process spawn errors
#[derive(Debug)]
pub(crate) enum Error {
    Alloc(alloc::Error),
    Thread(thread::Error),
    Process(process::Error),
    UserLoader(userloader::Error),
    NotFound,
}

impl From<alloc::Error> for Error {
    fn from(value: alloc::Error) -> Self {
        Error::Alloc(value)
    }
}

impl From<thread::Error> for Error {
    fn from(value: thread::Error) -> Self {
        Error::Thread(value)
    }
}

impl From<process::Error> for Error {
    fn from(value: process::Error) -> Self {
        Error::Process(value)
    }
}

impl From<userloader::Error> for Error {
    fn from(value: userloader::Error) -> Self {
        Error::UserLoader(value)
    }
}

impl Scheduler {
    // Helper function to complete the spawn
    fn finish_spawn(&self, mut sched: IrqSpinLockGuard<SchedInner>, affinity: Option<u8>) {
        // Set the timer
        self.wake_sleeping_threads(&mut sched);
        timer::set_next_deadline(sched.threads.next_timer_deadline(SLICE, timer::elapsed()));
        // If the spawned thread has affinity for the other hart, send an IPI
        drop(sched);
        if let Some(h) = affinity
            && h as usize != crate::arch::hart_id()
        {
            ipi::send(ipi::RESCHEDULE);
        } else {
            percpu::set_needs_reschedule();
        }
    }
    /// Spawn a new kernel (M-mode) thread to run the closure provided in entry.
    /// The closure is run exactly once and then exit is automatically called.
    ///
    /// Each spawn acquires a new thread control block with a forged context
    /// to allow for the existing scheduler `switch_to` to switch into the spawned
    /// thread seamlessly.
    ///
    /// The closure is stored on the heap by the spawner, so that it can be picked
    /// up by the newly spawned thread and run.
    ///
    /// On success a new ThreadHandle is given, on failure returns a [SpawnError].
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_kernel_thread_with<F: FnOnce() + Send + 'static>(
        &self,
        entry: F,
        priority: u8,
        stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Result<thread::Handle, Error> {
        // Thread stack is taken from the kernel heap
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, stack_order)?;
        // Now take the scheduler lock
        let mut sched = self.sched.lock();
        // Acquire a valid initialised thread control block slot with sp
        // pointing to a forged stack.
        let thread = sched.threads.acquire(
            |kernel_stack| context::forge_kernel_thread_stack(kernel_stack, entry),
            thread::ControlBlockSpec {
                kernel_stack,
                qos,
                priority,
                affinity,
                user: None,
            },
        )?;
        // Make sure threads don't launch with stale flags
        self.clear_thread_flags(thread);
        self.finish_spawn(sched, affinity);
        Ok(thread)
    }

    /// Add an additional user thread to a process
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_user_thread(
        &self,
        process: process::Handle,
        entry: userloader::UserEntry,
        priority: u8,
        kernel_stack_order: Order,
        user_stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Result<thread::Handle, Error> {
        // Allocate stacks before locking
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)?;
        // Now lock the scheduler
        let mut sched = self.sched.lock();
        // See if this process can accept another thread - in which case use the given entry return address
        let user_exit = sched.new_thread_entry_ra(process)?;
        let user_stack_top = user_stack.top();
        let user_stack_base = user_stack.base();
        let thread = sched.threads.acquire(
            |kernel_stack| unsafe {
                umode::init_stack_for_user_thread(
                    kernel_stack,
                    user_stack_base,
                    user_stack_top,
                    entry,
                    user_exit,
                )
            },
            thread::ControlBlockSpec {
                kernel_stack,
                qos,
                priority,
                affinity,
                user: Some(thread::UserContext {
                    stack: user_stack,
                    entry,
                    process,
                }),
            },
        )?;
        // Make sure threads don't launch with stale flags
        self.clear_thread_flags(thread);
        self.finish_spawn(sched, affinity);
        Ok(thread)
    }
    /// Spawn a new user process
    ///
    /// The process starts with one user thread.
    /// Note that additional threads added later will share the same `UserMemMap`.
    ///
    /// Takes a lock on the file descriptor table while holding the scheduler lock.
    /// Lock order: (1) SCHEDULER (2) FILE_DESCRIPTORS
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_process(
        &self,
        name: &'static str,
        priority: u8,
        kernel_stack_order: Order,
        user_stack_order: Order,
        loaded_image: userloader::LoadedImage,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Result<process::Handle, Error> {
        // Load the program before locking; wasteful but safe if there aren't sufficient process resources
        let userloader::LoadedImage {
            user_mem_map,
            entry,
            entry_ra,
        } = loaded_image;
        // Allocate stacks before locking; this is wasteful if there aren't enough other resources
        // but we don't want to do this under the scheduler lock
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)?;
        // Take the lock
        let mut sched = self.sched.lock();
        // Start with the process control block as we need the handle for the thread
        let process = sched.processes.acquire(name, user_mem_map)?;
        // Create the first thread for this process
        let user_stack_base = user_stack.base();
        let user_stack_top = user_stack.top();
        let thread = match sched.threads.acquire(
            |kernel_stack| unsafe {
                umode::init_stack_for_user_thread(
                    kernel_stack,
                    user_stack_base,
                    user_stack_top,
                    entry,
                    entry_ra,
                )
            },
            thread::ControlBlockSpec {
                kernel_stack,
                qos,
                priority,
                affinity,
                user: Some(thread::UserContext {
                    stack: user_stack,
                    entry,
                    process,
                }),
            },
        ) {
            Ok(thread) => thread,
            Err(e) => {
                // Release the PCB - there are no threads successfully created so safe to exit
                sched.processes.release(process);
                return Err(Error::Thread(e));
            }
        };
        // Clear all stale flags
        self.clear_thread_flags(thread);
        // Add the file descriptors just before launching the user thread - this is the "commit point" after which we don't revert
        // Get a handle to a file descriptor table for this process
        let fds = fd::acquire();
        fd::add_standard(fds);
        sched.commit_alive(process, fds, entry_ra);
        self.finish_spawn(sched, affinity);
        Ok(process)
    }
}
