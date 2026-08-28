//! Spawn processes and threads

use crate::arch::context;
use crate::kernel::alloc::{MemRegion, Order};
use crate::kernel::percpu;
use crate::kernel::sync::IrqSpinLockGuard;
use crate::kernel::timer;

use super::Qos;
use super::process;
use super::stride::{SLICE, SchedInner, Scheduler};
use super::threads::{ThreadControlBlockSpec, ThreadHandle, UserContext};
use super::userloader;

impl Scheduler {
    // Helper function to complete the spawn
    fn finish_spawn(&self, mut sched: IrqSpinLockGuard<SchedInner>, affinity: Option<u8>) {
        // Set the timer
        self.wake_sleeping_threads(&mut sched);
        timer::set_next_deadline(
            sched
                .thread_blocks
                .next_timer_deadline(SLICE, timer::elapsed()),
        );
        // If the spawned thread has affinity for the other hart, send an IPI
        drop(sched);
        if let Some(h) = affinity
            && h as usize != crate::arch::hart_id()
        {
            crate::kernel::ipi::send(h as usize);
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
    ) -> Option<ThreadHandle> {
        // Thread stack is taken from the kernel heap
        let stack_region =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, stack_order)?;
        // Now take the scheduler lock
        let mut sched = self.sched.lock();
        // Acquire a valid initialised thread control block slot with sp
        // pointing to a forged stack.
        let handle = sched.thread_blocks.acquire(
            |region| context::forge_kernel_thread_stack(region, entry),
            ThreadControlBlockSpec {
                kernel_stack: stack_region,
                qos,
                priority,
                affinity,
                user: None,
            },
        )?;
        self.needs_wakeup.clear(handle.idx); // Make sure threads don't launch with stale wakeup
        self.finish_spawn(sched, affinity);
        Some(handle)
    }

    // Add an additional user thread to a process
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_user(
        &self,
        process: &process::Handle,
        entry: userloader::UserEntry,
        priority: u8,
        kernel_stack_order: Order,
        user_stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Option<ThreadHandle> {
        // Allocate stacks before locking
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)?;
        // Now lock the scheduler
        let mut sched = self.sched.lock();
        // We should have a process control block already set up
        let Some(pcb) = sched.process_blocks.0[process.idx]
            .as_ref()
            .filter(|pcb| pcb.pid == process.pid)
        else {
            drop(sched);
            return None;
        };
        let user_stack_top = user_stack.top();
        let user_exit = pcb.entry_ra;
        let thread_handle = sched.thread_blocks.acquire(
            |kernel_stack_region| unsafe {
                context::Frame::init_stack_for_user_thread(
                    kernel_stack_region,
                    entry,
                    user_stack_top,
                    user_exit,
                )
            },
            ThreadControlBlockSpec {
                kernel_stack,
                qos,
                priority,
                affinity,
                user: Some(UserContext {
                    stack: user_stack,
                    entry,
                    process_idx: process.idx as u8,
                }),
            },
        )?;
        // Increment this process's thread count
        if sched.process_blocks.0[process.idx]
            .as_mut()
            .expect("still holding the lock and verified this is Some above")
            .add_thread_count()
            .is_err()
        {
            sched.thread_blocks.release(&thread_handle);
            drop(sched);
            return None;
        }
        self.needs_wakeup.clear(thread_handle.idx); // Make sure threads don't launch with stale wakeup
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
        let userloader::LoadedImage {
            user_mem_map,
            entry,
            entry_ra,
        } = loaded_image;
        let mut pcb = process::ControlBlock::new(name, user_mem_map, entry_ra);
        // Allocate stacks before locking
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)
                .ok_or(process::SpawnError::NotEnoughMemory)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)
                .ok_or(process::SpawnError::NotEnoughMemory)?;
        // Take the lock
        let mut sched = self.sched.lock();
        // Get a process slot
        let pcb_idx = sched
            .process_blocks
            .find_process_slot()
            .ok_or(process::SpawnError::TooManyProcesses)?;
        let user_stack_top = user_stack.top();
        let thread_handle = sched
            .thread_blocks
            .acquire(
                |kernel_stack_region| unsafe {
                    context::Frame::init_stack_for_user_thread(
                        kernel_stack_region,
                        entry,
                        user_stack_top,
                        entry_ra,
                    )
                },
                ThreadControlBlockSpec {
                    kernel_stack,
                    qos,
                    priority,
                    affinity,
                    user: Some(UserContext {
                        stack: user_stack,
                        entry,
                        process_idx: pcb_idx as u8,
                    }),
                },
            )
            .ok_or(process::SpawnError::NotEnoughThreads)?;
        self.needs_wakeup.clear(thread_handle.idx); // Make sure threads don't launch with stale wakeup
        // Install
        pcb.add_thread_count()
            .expect("adding the first thread is always valid");
        // Set up file descriptor table
        pcb.fds.new_process();
        let process_handle = sched
            .process_blocks
            .install_process_control_block(pcb_idx, pcb);
        self.finish_spawn(sched, affinity);
        Ok(process_handle)
    }
}
