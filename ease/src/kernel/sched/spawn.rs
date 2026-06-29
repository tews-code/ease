//! Spawn processes and threads

use alloc::boxed::Box;

use crate::arch::context::Context;
use crate::kernel::alloc::{MemRegion, Order};
use crate::kernel::percpu;
use crate::kernel::sync::IrqSpinLockGuard;
use crate::kernel::timer;

use super::process::{ProcessControlBlock, ProcessHandle};
use super::stride::{SLICE, SchedInner, Scheduler};
use super::threads::{ThreadControlBlockSpec, ThreadHandle, UserContext};
use super::usermemmap::UserMemMap;
use super::{ExitReason, Qos, SCHEDULER};

impl Scheduler {
    // Helper function to complete the spawn
    fn finish_spawn(&self, mut sched: IrqSpinLockGuard<SchedInner>, affinity: Option<u8>) {
        // Set the timer
        sched.wake_sleeping_threads();
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

    // Set up kernel thread initial thread block and stack for a new thread
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn<F: FnOnce() + Send + 'static>(
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
        // Now take the schedler lock
        let mut sched = self.sched.lock();
        // Acquire a thread control block slot
        let handle = sched.thread_blocks.acquire(
            |region| {
                let closure_ptr = Box::into_raw(Box::new(entry)) as *mut u8;
                unsafe { Context::init_kernel_stack(region, run_closure_thread::<F>, closure_ptr) }
            },
            ThreadControlBlockSpec {
                kernel_stack: stack_region,
                qos,
                priority,
                affinity,
                user: None,
            },
        )?;
        self.finish_spawn(sched, affinity);
        Some(handle)
    }

    // Add an additional user thread to a process
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_user(
        &self,
        process: &ProcessHandle,
        user_entry: extern "C" fn(),
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
        if sched.process_blocks.0[process.idx]
            .as_ref()
            .is_none_or(|pcb| pcb.pid != process.pid)
        {
            drop(sched);
            return None;
        }
        let user_stack_top = user_stack.top();
        let user_exit = crate::user::user_exit as *const () as usize;
        let thread_handle = sched.thread_blocks.acquire(
            |kernel_stack_region| unsafe {
                Context::init_user_stack(kernel_stack_region, user_entry, user_stack_top, user_exit)
            },
            ThreadControlBlockSpec {
                kernel_stack,
                qos,
                priority,
                affinity,
                user: Some(UserContext {
                    user_stack,
                    user_entry,
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
        // let thread_handle = acquire_user_thread();
        self.finish_spawn(sched, affinity);
        Some(thread_handle)
    }

    // Spawn a new user process
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn spawn_process(
        &self,
        name: &'static str,
        user_entry: extern "C" fn(),
        priority: u8,
        kernel_stack_order: Order,
        user_stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Option<ProcessHandle> {
        let mem_map = UserMemMap::for_process().ok()?;
        let mut pcb = ProcessControlBlock::new(name, mem_map);
        // Allocate stacks before locking
        let kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)?;
        // Take the lock
        let mut sched = self.sched.lock();
        // Get a process slot
        let pcb_idx = sched.process_blocks.find_process_slot()?;
        let user_stack_top = user_stack.top();
        let user_exit = crate::user::user_exit as *const () as usize;
        let _ = sched.thread_blocks.acquire(
            |kernel_stack_region| unsafe {
                Context::init_user_stack(kernel_stack_region, user_entry, user_stack_top, user_exit)
            },
            ThreadControlBlockSpec {
                kernel_stack,
                qos,
                priority,
                affinity,
                user: Some(UserContext {
                    user_stack,
                    user_entry,
                    process_idx: pcb_idx as u8,
                }),
            },
        )?;
        // Install
        pcb.add_thread_count()
            .expect("adding the first thread is always valid");
        let process_handle = sched
            .process_blocks
            .install_process_control_block(pcb_idx, pcb);
        self.finish_spawn(sched, affinity);
        Some(process_handle)
    }
}

/// Call exit at the end of a spawned closure
extern "C" fn run_closure_thread<F: FnOnce() + Send + 'static>(entry_ptr: *mut u8) -> ! {
    let e = unsafe { Box::from_raw(entry_ptr as *mut F) };
    e(); // runs the closure exactly once and consumes both the closure and the Box.
    SCHEDULER.exit(ExitReason::Exit)
}
