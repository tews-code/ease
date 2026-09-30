//! Spawn processes and threads

use crate::arch::{context, umode};
use crate::kernel::alloc::{MemRegion, Order};
use crate::kernel::ipi;
use crate::kernel::percpu;
use crate::kernel::sched::process::SpawnError;
use crate::kernel::sync::IrqSpinLockGuard;
use crate::kernel::timer;

use super::Qos;
use super::process;
use super::stride::{SLICE, SchedInner, Scheduler};
use super::thread;
use super::userloader;

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
    /// On success a new ThreadHandle is given, on failure returns None.
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_kernel_thread_with<F: FnOnce() + Send + 'static>(
        &self,
        entry: F,
        priority: u8,
        stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Option<thread::Handle> {
        // Thread stack is taken from the kernel heap
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, stack_order)?;
        // Now take the scheduler lock
        let mut sched = self.sched.lock();
        // Acquire a valid initialised thread control block slot with sp
        // pointing to a forged stack.
        let handle = sched.threads.acquire(
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
        self.clear_thread_flags(handle);
        self.finish_spawn(sched, affinity);
        Some(handle)
    }

    // Add an additional user thread to a process
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
    ) -> Option<thread::Handle> {
        // Allocate stacks before locking
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)?;
        // Now lock the scheduler
        let mut sched = self.sched.lock();
        // We should have a process control block already set up
        // and it should not already be in the process of being torn down
        let Some(pcb) = sched
            .processes
            .pcbs
            .get(process)
            .filter(|pcb| pcb.teardown_thread.is_none())
        else {
            drop(sched);
            return None;
        };
        let user_stack_top = user_stack.top();
        let user_stack_base = user_stack.base();
        let user_exit = pcb.entry_ra;
        let thread_handle = sched.threads.acquire(
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
        // Increment this process's thread count
        if sched
            .processes
            .pcbs
            .get_mut(process)
            .expect("still holding the lock and verified this is Some above")
            .add_thread_count()
            .is_err()
        {
            // Too many threads requested, need to exit
            sched.threads.release(thread_handle);
            drop(sched);
            return None;
        }
        // Make sure threads don't launch with stale flags
        self.clear_thread_flags(thread_handle);
        self.finish_spawn(sched, affinity);
        Some(thread_handle)
    }
    /// Spawn a new user process
    ///
    /// The process starts with one user thread.
    /// Note that additional threads added later will share the same `UserMemMap`.
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
    ) -> Result<process::Handle, process::SpawnError> {
        // Load the program before locking; wasteful but safe if there aren't sufficient process resources
        let userloader::LoadedImage {
            user_mem_map,
            entry,
            entry_ra,
        } = loaded_image;
        // Allocate stacks before locking; this is wasteful if there aren't enough other resources
        // but we don't want to do this under the scheduler lock
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)
                .ok_or(process::SpawnError::NotEnoughMemory)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)
                .ok_or(process::SpawnError::NotEnoughMemory)?;
        // Take the lock
        let mut sched = self.sched.lock();
        // Start with the process control block as we need the handle for the thread
        let process = sched
            .processes
            .pcbs
            .add(process::ControlBlock::new(name, user_mem_map, entry_ra))
            .ok_or(SpawnError::NotEnoughProcessSlots)?;
        // Create the thread
        let user_stack_base = user_stack.base();
        let user_stack_top = user_stack.top();
        let Some(thread) = sched.threads.acquire(
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
        ) else {
            // Not enough thread slots, need to remove the process and return
            sched.processes.pcbs.take(process);
            return Err(SpawnError::NotEnoughThreadSlots);
        };
        // Clear all stale flags
        self.clear_thread_flags(thread);
        // Update the process control block with the thread count and file descriptor
        let pcb = sched
            .processes
            .pcbs
            .get_mut(process)
            .expect("just installed and still hold the lock");
        pcb.add_thread_count()
            .expect("adding the first thread is always valid");
        pcb.fds.new_process();
        self.finish_spawn(sched, affinity);
        Ok(process)
    }
}
