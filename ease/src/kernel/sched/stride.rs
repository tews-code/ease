//! Preemptive multitasking with stride scheduling

use alloc::boxed::Box;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use super::types::{
    Deadline, PostSwitch, Qos, State, THREADS_MAX, ThreadControlBlock, ThreadHandle, ThreadsInner,
    UserContext,
};
use crate::arch::{cpu_id, csr};
use crate::board::HARTS_MAX;
use crate::kernel::alloc::Order;
use crate::kernel::sched::process::PROCS_MAX;
use crate::kernel::sched::stack;
use crate::kernel::sched::{MemRegion, STACK_CANARY};
use crate::kernel::sync::{CounterU64, IrqSpinLock, with_interrupts_disabled};
use crate::kernel::{percpu, timer};

#[cfg(feature = "profile")]
use ease_macros::profile;

unsafe extern "C" {
    // Safety: caller must ensure prev points to a writable slot owned by the current
    // thread; next points to a slot containing a saved sp produced by a prior swap_to call or
    // by spawn's stack forging; calling with interrupts disabled is undefined;
    pub(crate) fn switch_to_h0(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
    pub(crate) fn switch_to_h1(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
    static __hart0_irq_stack_top: u8;
    static __hart1_irq_stack_top: u8;
    static __idle_stack_size: u8;
}

// Helper function to determine the HART boot threads in the threads array
// To match hardware - HART0 using SRAM4 and HART1 using SRAM5
// Note: all other threads have their stack in the heap
const fn slot_for_boot(hartid: usize) -> usize {
    THREADS_MAX - HARTS_MAX + hartid
}
// Priority is 0 (highest, does not stride/age) to 255 (idle)
pub const PRIORITY_DEFAULT: u8 = u8::MAX / 2;
pub const PRIORITY_MIN: u8 = u8::MAX - 1;

// Under contention use this time slice per thread
const SLICE_US: u64 = 16_000;
const SLICE: u64 = SLICE_US * timer::CYCLES_PER_US;
// Default slack period
const LEEWAY_BASE_US: u64 = 100;
const LEEWAY_BASE: u64 = LEEWAY_BASE_US * timer::CYCLES_PER_US;
// Maximum slack period - used to cap the maximum requested leeway to sensible values
const LEEWAY_MAX_US: u64 = 1_000_000;
const LEEWAY_MAX: u64 = LEEWAY_MAX_US * timer::CYCLES_PER_US;

// User thread constants
#[allow(dead_code)]
const USER_MEM_REGION_8KB: u8 = 13; // Order is log2(N)
#[allow(dead_code)]
const USER_MEM_REGION_16KB: u8 = 14;

pub(super) static SCHEDULER: Scheduler = Scheduler::new();

impl Deadline {
    // Helper function - returns the leeway in clint cycles
    fn leeway(&self, qos: &Qos) -> u64 {
        if let Some(l) = self.fixed_leeway {
            l
        } else {
            let now = crate::kernel::timer::elapsed();
            let remaining = self.min.saturating_sub(now);
            match qos {
                Qos::High => (remaining >> 16).min(LEEWAY_BASE),
                Qos::Low => (remaining >> 3).min(LEEWAY_MAX),
            }
        }
    }
}

impl ThreadControlBlock {
    pub(super) const fn new() -> Self {
        Self {
            id: u32::MAX,
            state: State::Avail,
            sp: None,
            kernel_stack: None,
            qos: Qos::Low,
            priority: PRIORITY_MIN,
            pass: 0,
            last_started_cycles: 0,
            next_waiter: None,
            affinity: None,
            user: None,
        }
    }

    // Stride forward by ran_cycles weighted by priority.
    //
    // `ran_cycles` is in CLINT cycles (mtime units) so `pass` is the same
    // unit across all threads. No `/ SLICE` normalisation is applied — that
    // would round sub-millisecond runs to zero stride, which left
    // yield-loopers' pass stagnant and starved sleepers (see Phase 5
    // notes). The trade-off is `pass` values grow larger in absolute
    // terms (~10⁷ per ms at PRIORITY_DEFAULT), but they stay well under
    // u64 saturation for any realistic uptime.
    //
    // Threads at priority 0 do not stride/age (stride is 0 for any ran)
    // and so always have the lowest pass — they win every pick_next.
    fn stride(&mut self, ran_cycles: u64) {
        self.pass = self
            .pass
            .saturating_add(ran_cycles.saturating_mul(self.priority as u64));
    }

    // Calculates the latest wake up for a thread including leeway
    // Returns None if thread is not sleeping
    fn wakeup_deadline(&self) -> Option<u64> {
        match self.state {
            State::Sleeping(deadline)
            | State::Switching(PostSwitch::Sleeping(deadline))
            | State::BlockedUntil(deadline)
            | State::Switching(PostSwitch::BlockedUntil(deadline)) => {
                let leeway = deadline.leeway(&self.qos);
                Some(deadline.min.saturating_add(leeway))
            }
            _ => None,
        }
    }
}

impl ThreadsInner {
    // Find a free thread slot
    fn free_slot(&mut self) -> Option<(usize, &mut ThreadControlBlock)> {
        self.thread_blocks
            .iter_mut()
            .enumerate()
            .find(|(_, tcb)| tcb.state == State::Avail)
    }

    // Find the minimum current pass value among active, non-idle threads.
    //
    // PRI_MIN threads (the idle bootstrap on non-main harts) accumulate
    // very little stride — they mostly WFI and never switch out — so
    // including them in the baseline calculation would give every newly
    // spawned thread a pass of 0, letting it dominate pick_next until its
    // pass naturally catches up to the rest of the system.
    fn pass_baseline(&self) -> u64 {
        let mut best: Option<u64> = None;
        for tcb in &self.thread_blocks {
            let eligible = (tcb.state == State::Ready || tcb.state == State::Running)
                && tcb.priority != PRIORITY_MIN;
            if eligible {
                best = Some(best.map_or(tcb.pass, |b| b.min(tcb.pass)));
            }
        }
        best.unwrap_or(0)
    }

    // Wakes any threads past their deadlines
    //
    // Returns a count of the ready threads
    fn wake_sleeping_threads(&mut self) -> usize {
        let now = timer::elapsed();
        let running_idx = percpu::current_thread_idx();
        let mut ready_count: usize = 0;
        for (idx, tcb) in self.thread_blocks.iter_mut().enumerate() {
            if idx == running_idx {
                continue;
            }
            match tcb.state {
                State::Sleeping(Deadline {
                    min,
                    fixed_leeway: _,
                })
                | State::BlockedUntil(Deadline {
                    min,
                    fixed_leeway: _,
                }) if min <= now => {
                    // If we are waking threads ignore slack and wake any passed min deadline
                    tcb.state = State::Ready;
                    ready_count += 1;
                }
                State::Ready => ready_count += 1,
                _ => {} // Ignore switching, even if passed deadline
            }
        }
        ready_count
    }

    // Gets the first wake up deadline including leeway (including threads busy switching)
    // Returns None if no threads are sleeping
    fn earliest_deadline(&self) -> Option<u64> {
        self.thread_blocks
            .iter()
            .filter_map(|tcb| tcb.wakeup_deadline())
            .min()
    }

    /// Set the timer to the earliest deadline or next slice under contention
    fn set_next_timer(&mut self, earliest_deadline: Option<u64>, ready_count: usize) {
        let now = timer::elapsed();
        let slice_end = if ready_count >= 1 {
            SLICE + now
        } else {
            u64::MAX
        };

        let wake = earliest_deadline.inspect(|&b| {
            // For every sleeper whose tolerance window includes `b`, pin it
            // to wake at `b` (the coalesce point) and zero its leeway so
            // subsequent calls treat it as a hard deadline. For sleepers
            // whose window doesn't include `b`, memoise the computed
            // leeway into `fixed_leeway` so future visits in this set of
            // calls see a stable value.
            for tcb in &mut self.thread_blocks {
                if let State::Sleeping(d) | State::Switching(PostSwitch::Sleeping(d)) =
                    &mut tcb.state
                {
                    let leeway = d.leeway(&tcb.qos);
                    if b >= d.min && b <= d.min.saturating_add(leeway) {
                        d.min = b;
                        d.fixed_leeway = Some(0);
                    } else if d.fixed_leeway.is_none() {
                        d.fixed_leeway = Some(leeway);
                    }
                }
            }
        });

        crate::kernel::timer::set_next_deadline(slice_end.min(wake.unwrap_or(u64::MAX)));
    }

    // Get disjoint mutable TCBs for current and next
    // Given there is always an idle thread in Ready
    // Panics if there is no idle thread, or idle calls reschedule
    #[inline(never)]
    fn pick_next_ready_mut(
        &mut self,
    ) -> Option<(
        &mut ThreadControlBlock,
        usize,
        &mut ThreadControlBlock,
        usize,
    )> {
        // Find current index
        let curr_idx = percpu::current_thread_idx();
        let this_hart = crate::arch::cpu_id() as u8;
        let mut best_idx = None;
        let mut best_pass = u64::MAX;
        for (idx, tcb) in self.thread_blocks.iter().enumerate() {
            let candidate = tcb.state == State::Ready;
            let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
            let pri_ok = tcb.priority != PRIORITY_MIN;
            if candidate && affinity_ok && pri_ok && tcb.pass < best_pass {
                best_pass = tcb.pass;
                best_idx = Some(idx);
            }
        }
        let next_idx = best_idx.unwrap_or_else(percpu::idle_thread_idx);
        if curr_idx == next_idx {
            None
        } else {
            let [curr, next] = self
                .thread_blocks
                .get_disjoint_mut([curr_idx, next_idx])
                .expect("indices have been selected as disjoint");
            Some((curr, curr_idx, next, next_idx))
        }
    }

    // Get disjoint mutable TCBs for current and next
    #[inline(never)]
    fn pick_next_if_fairer_mut(
        &mut self,
    ) -> Option<(
        &mut ThreadControlBlock,
        usize,
        &mut ThreadControlBlock,
        usize,
    )> {
        // Find current index
        let curr_idx = percpu::current_thread_idx();
        let this_hart = crate::arch::cpu_id() as u8;
        let mut best_idx = None;
        let mut best_pass = u64::MAX;
        for (idx, tcb) in self.thread_blocks.iter().enumerate() {
            let candidate = tcb.state == State::Ready || idx == curr_idx;
            let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
            let pri_ok = tcb.priority != PRIORITY_MIN;
            if candidate && affinity_ok && pri_ok && tcb.pass < best_pass {
                best_pass = tcb.pass;
                best_idx = Some(idx);
            }
        }
        let next_idx = best_idx.unwrap_or_else(percpu::idle_thread_idx);
        if curr_idx == next_idx {
            None
        } else {
            let [curr, next] = self
                .thread_blocks
                .get_disjoint_mut([curr_idx, next_idx])
                .expect("indices have been selected as disjoint");
            Some((curr, curr_idx, next, next_idx))
        }
    }

    #[inline(never)]
    fn check_curr_canary(&self) {
        let curr = &self.thread_blocks[percpu::current_thread_idx()];
        let base_addr = match &curr.kernel_stack {
            Some(memregion) => memregion.base_addr(),
            None =>
            // Idle thread stack (fixed by linker script)
            {
                &raw const __hart0_idle_stack_base as usize
            }
        };
        let val = unsafe { core::ptr::read(base_addr as *const usize) };
        assert!(
            val == STACK_CANARY,
            "kernel stack canary corrupted in thread {}: sp={:?}, base={}, read={:#x}, expected={:#x}",
            curr.id,
            curr.sp,
            base_addr,
            val,
            STACK_CANARY,
        );
    }

    // Set the PerCpu info for a running thread on this HART
    // and set mscratch to top IRQ stack for M-mode or kernel stack for U-mode
    fn activate_thread(
        &mut self,
        current_thread_idx: usize,
        switching_from_thread_idx: Option<usize>,
    ) {
        // Set PerCpu
        let stack = self.thread_blocks[current_thread_idx]
            .kernel_stack
            .as_ref()
            .expect("should not be setting percpu for thread without a stack");
        percpu::set_current_thread_idx(current_thread_idx);
        percpu::set_switching_from_thread_idx(switching_from_thread_idx);
        percpu::set_current_stack_base(stack.base().as_ptr());

        // Set mscratch
        if let Some(user_context) = &self.thread_blocks[current_thread_idx].user {
            // User thread has kernel stack top in mscratch
            unsafe {
                csr::mscratch::write(stack.top().addr().into());
            }

            // For user thread merge the thread's stack and set pmp
            let pmp_config = self.process_blocks[user_context.process_idx as usize]
                .as_ref()
                .expect("process should be configured before this thread is scheduled")
                .mem_map
                .to_pmp(&user_context.user_stack);
            pmp_config.activate();
        } else {
            // Kernel thread has IRQ stack top in mscratch
            if cpu_id() == 0 {
                unsafe {
                    csr::mscratch::write(&raw const __hart0_irq_stack_top as usize);
                }
            } else {
                unsafe {
                    csr::mscratch::write(&raw const __hart1_irq_stack_top as usize);
                }
            }
        }
    }
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for ThreadsInner {}

pub(super) struct Scheduler {
    threads: IrqSpinLock<ThreadsInner>,
    run_cycles: [CounterU64; THREADS_MAX], // Outside of threads for lock-free read
}

unsafe extern "C" {
    static __hart0_idle_stack_base: u8;
    static __hart1_idle_stack_base: u8;
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            threads: IrqSpinLock::new(ThreadsInner {
                thread_blocks: [const { ThreadControlBlock::new() }; THREADS_MAX],
                process_blocks: [const { None }; PROCS_MAX],
            }),
            run_cycles: [const { CounterU64::new(0) }; THREADS_MAX],
        }
    }

    // Set up the boot thread for each hart
    pub(super) fn bootstrap(&self, hartid: usize) {
        let id = next_thread_id();
        let idx = slot_for_boot(hartid);
        let mut threads = self.threads.lock();
        assert_eq!(
            &raw const __idle_stack_size as usize,
            Order::KB2.size(),
            "idle stack size disagrees with linker"
        );
        threads.thread_blocks[idx] = ThreadControlBlock {
            id,
            state: State::Running,
            sp: NonNull::new(crate::arch::csr::regs::sp() as *mut u8),
            kernel_stack: Some(MemRegion::from_fixed(
                NonNull::new(match hartid {
                    0 => &raw const __hart0_idle_stack_base as *mut u8,
                    1 => &raw const __hart1_idle_stack_base as *mut u8,
                    _ => unreachable!("only running two harts"),
                })
                .unwrap(),
                Order::KB2,
            )),
            last_started_cycles: timer::elapsed(),
            ..ThreadControlBlock::new()
        };
        percpu::set_idle_thread_idx(idx);
        threads.activate_thread(idx, None);
    }

    /// Helper function to clean up post switch threads
    #[cfg_attr(feature = "profile", profile)]
    pub(super) fn post_switch_cleanup(&self) {
        // After switch_to returns (on this thread's eventual resume),
        let mut threads = self.threads.lock();
        let switched_from_idx = percpu::take_switching_from_thread_idx()
            .expect("should have a Switching thread to set back to Ready");
        let new_state = {
            match threads.thread_blocks[switched_from_idx].state {
                State::Switching(PostSwitch::Ready) => State::Ready,
                State::Switching(PostSwitch::Sleeping(d)) => State::Sleeping(d),
                State::Switching(PostSwitch::Blocked) => State::Blocked,
                State::Switching(PostSwitch::BlockedUntil(d)) => State::BlockedUntil(d),
                State::Switching(PostSwitch::Dead) => {
                    // Release user memory (if U-mode thread)
                    threads.thread_blocks[switched_from_idx].user = None;
                    // Release kernel stack
                    threads.thread_blocks[switched_from_idx].kernel_stack = None;
                    threads.thread_blocks[switched_from_idx].sp = None;
                    threads.thread_blocks[switched_from_idx] = ThreadControlBlock::new();
                    // Mark TCB slot avaialble for use
                    State::Avail
                }
                _ => panic!("Post switch but not in switching state"),
            }
        };
        threads.thread_blocks[switched_from_idx].state = new_state;
    }

    // Set up kernel thread initial thread block and stack for a new thread
    pub(super) fn spawn<F: FnOnce() + Send + 'static>(
        &self,
        entry: F,
        priority: u8,
        stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Option<ThreadHandle> {
        // Create pointer to closure
        let b = Box::new(entry);
        let closure_ptr = Box::into_raw(b) as *mut u8;
        // Thread stack is taken from the kernel heap
        let mut stack_region =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, stack_order)?;
        let sp = unsafe {
            stack::init_for_kernel_entry(&mut stack_region, run_closure_thread::<F>, closure_ptr)
        };
        // Now lock the scheduler
        let mut threads = self.threads.lock();
        // Get the current pass baselines so we don't schedule ahead of other threads
        let baseline = threads.pass_baseline();
        // Find a free TCB slot
        let Some((idx, tcb)) = threads.free_slot() else {
            drop(threads);
            return None;
        };
        // Initialise the TCB
        *tcb = ThreadControlBlock {
            id: next_thread_id(),
            state: State::Ready,
            sp,
            kernel_stack: Some(stack_region),
            qos,
            priority,
            pass: baseline,
            affinity,
            ..ThreadControlBlock::new()
        };
        // Local variables to drop threads
        let handle = ThreadHandle { id: tcb.id, idx };
        // Set the timer
        let ready_count = threads.wake_sleeping_threads();
        let earliest_deadline = threads.earliest_deadline();
        threads.set_next_timer(earliest_deadline, ready_count);
        // If the spawned thread has affinity for the other hart, send an IPI
        drop(threads);
        if let Some(h) = affinity
            && h as usize != crate::arch::cpu_id()
        {
            crate::kernel::ipi::send(h as usize);
        } else {
            percpu::set_needs_reschedule();
        }
        Some(handle)
    }

    // Set up initial thread control block and stack for a new user thread
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_user(
        &self,
        process_idx: usize,
        user_entry: extern "C" fn(),
        priority: u8,
        kernel_stack_order: Order,
        user_stack_order: Order,
        qos: Qos,
        affinity: Option<u8>,
    ) -> Option<ThreadHandle> {
        // Allocate stacks before locking
        let mut kernel_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::KernelPd1, kernel_stack_order)?;
        let user_stack =
            MemRegion::from_heap(crate::kernel::alloc::Pool::UserPd0, user_stack_order)?;
        let sp =
            unsafe { stack::init_for_user_entry(&mut kernel_stack, user_entry, user_stack.top()) };
        // Now lock the scheduler
        let mut threads = self.threads.lock();
        // We should have a process control block already set up
        if threads.process_blocks[process_idx].is_none() {
            drop(threads);
            return None;
        }
        // Get the current pass baselines so we don't schedule ahead of other threads
        let baseline = threads.pass_baseline();
        // Find a free TCB slot
        let Some((idx, tcb)) = threads.free_slot() else {
            drop(threads);
            return None;
        };

        // Initialise the TCB
        *tcb = ThreadControlBlock {
            id: next_thread_id(),
            sp,
            state: State::Ready,
            qos,
            priority,
            kernel_stack: Some(kernel_stack),
            pass: baseline,
            affinity,
            user: Some(UserContext {
                user_stack,
                user_entry,
                process_idx: process_idx as u8,
            }),
            ..ThreadControlBlock::new()
        };
        // Local variables to allow dropping the threads lock
        let handle = ThreadHandle { id: tcb.id, idx };
        // Set the timer
        let ready_count = threads.wake_sleeping_threads();
        let earliest_deadline = threads.earliest_deadline();
        threads.set_next_timer(earliest_deadline, ready_count);
        // If the spawned thread has affinity for the other hart, send an IPI
        drop(threads);
        if let Some(h) = affinity
            && h as usize != crate::arch::cpu_id()
        {
            crate::kernel::ipi::send(h as usize);
        } else {
            percpu::set_needs_reschedule();
        }
        Some(handle)
    }

    // Set current thread state to `new_state` and next thread to `Ready` in round robin
    // If `currrent_state` is Some(State), reschedule only takes place if the
    // current thread state matches; if `current_state` is None then reschedule
    // unconditionally
    pub(super) fn reschedule(&self, current_state: Option<State>, new_state: PostSwitch) {
        // Note - the closure body will contain switch_to, which is unusual
        // It's a non-local control transfer wearing the disguise of a function call.
        // It works correctly because the closure frame is preserved on the suspended thread's stack
        with_interrupts_disabled(|_cs| {
            // Clear any reschedule flag as we are rescheduling
            let _ = percpu::take_needs_reschedule();
            // We manually take and release the lock before the
            // context switch — it's held in this
            // thread's stack frame, so switch_to would otherwise carry the lock
            // across the switch and block other harts/threads from rescheduling.
            let mut threads = self.threads.lock();
            threads.check_curr_canary();
            // First check if current state matches pre-condition
            if let Some(state) = current_state {
                let idx = percpu::current_thread_idx();
                if threads.thread_blocks[idx].state != state {
                    return;
                }
            }
            // Check if any threads have reached or passed their deadline
            let ready_count = threads.wake_sleeping_threads();
            // Get current and next TCBs
            let Some((curr, curr_idx, next, next_idx)) = threads.pick_next_ready_mut() else {
                let earliest_deadline = threads.earliest_deadline();
                threads.set_next_timer(earliest_deadline, ready_count);
                drop(threads);
                return;
            };
            // Ready to switch
            // Set current thread to the new state
            curr.state = State::Switching(new_state);
            // Safety: Only writing to current within locked threads array - single writer
            let now_cycles = timer::elapsed();
            let ran = now_cycles - curr.last_started_cycles;
            unsafe { self.run_cycles[curr_idx].add(ran) };
            curr.stride(ran);

            next.state = State::Running;
            next.last_started_cycles = now_cycles;

            // Create local variables before dropping the lock
            let prev_sp_ptr = &raw mut curr.sp;
            let next_sp_ptr = &raw mut next.sp;

            threads.activate_thread(next_idx, Some(curr_idx));
            let earliest_deadline = threads.earliest_deadline(); // Re-run after setting up sleeper
            threads.set_next_timer(earliest_deadline, ready_count);
            drop(threads);

            if cpu_id() == 0 {
                //Safety: Option<NonNull<u8>> is bit for bit identical to *mut u8
                //  - Same size, same alignment (both one pointer-word).
                // - None is represented by the all-zeroes / null bit pattern.
                // - Some(NonNull(p)) is represented by exactly p's bits.
                unsafe {
                    switch_to_h0(prev_sp_ptr as *mut *mut u8, next_sp_ptr as *mut *mut u8);
                }
            } else {
                unsafe {
                    switch_to_h1(prev_sp_ptr as *mut *mut u8, next_sp_ptr as *mut *mut u8);
                }
            }

            self.post_switch_cleanup();
            //mret has restored MIE via the thread trampoline
        });
    }

    /// Perform switch accounting and set reschedule flag
    pub(super) fn mark_for_preempt(&self) {
        let mut threads = self.threads.lock();
        threads.check_curr_canary();
        let ready_count = threads.wake_sleeping_threads();
        let earliest_deadline = threads.earliest_deadline();

        // Apply current's stride upfront so pass comparisons in
        // pick_next_if_fairer_mut see a fresh value —
        // otherwise a long-running
        // thread keeps appearing to have its old (low) pass and
        // never loses a comparison.
        let now_cycles = timer::elapsed();
        let curr_idx = percpu::current_thread_idx();
        {
            let curr = &mut threads.thread_blocks[curr_idx];
            let ran = now_cycles - curr.last_started_cycles;
            curr.last_started_cycles = now_cycles;
            unsafe { self.run_cycles[curr_idx].add(ran) };
            curr.stride(ran);
        }

        let Some((_curr, _curr_idx, _next, _next_idx)) = threads.pick_next_if_fairer_mut() else {
            // Same thread is running uncontended, increase slice deadline
            threads.set_next_timer(earliest_deadline, ready_count);
            return;
        };
        threads.set_next_timer(earliest_deadline, ready_count);
        percpu::set_needs_reschedule();

        let earliest_deadline = threads.earliest_deadline();
        threads.set_next_timer(earliest_deadline, ready_count);
        percpu::set_needs_reschedule();
    }

    pub(super) fn schedule(&self) {
        if percpu::take_needs_reschedule() {
            with_interrupts_disabled(|_cs| {
                let mut threads = self.threads.lock();
                threads.check_curr_canary();
                let pick = threads.pick_next_if_fairer_mut();
                let Some((curr, curr_idx, next, next_idx)) = pick else {
                    return;
                };

                // Perform switch
                curr.state = State::Switching(PostSwitch::Ready);
                next.state = State::Running;
                let now_cycles = timer::elapsed();
                next.last_started_cycles = now_cycles;
                // Create local variables before dropping the lock
                let prev_sp_ptr = &raw mut curr.sp;
                let next_sp_ptr = &raw mut next.sp;
                threads.activate_thread(next_idx, Some(curr_idx));
                drop(threads);

                if cpu_id() == 0 {
                    unsafe {
                        switch_to_h0(prev_sp_ptr as *mut *mut u8, next_sp_ptr as *mut *mut u8);
                    }
                } else {
                    unsafe {
                        switch_to_h1(prev_sp_ptr as *mut *mut u8, next_sp_ptr as *mut *mut u8);
                    }
                }

                // After switch_to returns (on this thread's eventual resume),
                self.post_switch_cleanup();
            });
        }
    }

    /// Yields current thread
    pub(super) fn yield_now(&self) {
        self.reschedule(None, PostSwitch::Ready);
    }

    /// Blocks until the timer has passed the deadline
    ///
    /// Time is measured in milliseconds
    pub(super) fn sleep_until(&self, deadline_ms: u64, fixed_leeway_ms: Option<u64>) {
        let deadline = Deadline {
            min: deadline_ms.saturating_mul(timer::CYCLES_PER_MS),
            fixed_leeway: fixed_leeway_ms.map(|l| l.saturating_mul(timer::CYCLES_PER_MS)),
        };
        self.reschedule(None, PostSwitch::Sleeping(deadline));
    }

    /// Blocks for `deadline` milliseconds
    #[allow(dead_code)]
    pub(super) fn sleep(&self, deadline_ms: u64) {
        self.sleep_until(
            crate::kernel::timer::elapsed_ms().saturating_add(deadline_ms),
            None,
        );
    }

    /// Blocks for `deadline` milliseconds
    #[allow(dead_code)]
    pub(super) fn sleep_with_leeway(&self, deadline_ms: u64, leeway_ms: u64) {
        let now_ms = crate::kernel::timer::elapsed_ms();
        self.sleep_until(now_ms.saturating_add(deadline_ms), Some(leeway_ms));
    }

    /// Get currently running thread total cpu cycles
    pub fn get_current_cycles(&self, tcb_idx: usize) -> u64 {
        self.run_cycles[tcb_idx].get()
    }

    pub(super) fn exit(&self) -> ! {
        self.reschedule(None, PostSwitch::Dead);
        // reschedule switches away. If we get here, no other thread was
        // available to switch to, which means this thread is the only one
        // alive on this hart and we can't actually die. Panic — it's a
        // programmer error to call exit() on the last thread.
        unreachable!("exit() called but no other thread to switch to");
    }

    // Park the current thread
    #[allow(dead_code)]
    pub(super) fn park(&self) {
        self.reschedule(None, PostSwitch::Blocked);
    }

    // Park the current thread if it is in blocked state
    pub(super) fn park_if_blocked(&self) {
        self.reschedule(Some(State::Blocked), PostSwitch::Blocked);
    }

    // Park the current thread if it is in blocked until deadline state
    pub(super) fn park_if_blocked_until(&self, deadline_ms: u64) {
        let deadline = Deadline {
            min: deadline_ms * timer::CYCLES_PER_MS,
            fixed_leeway: None,
        };
        self.reschedule(
            Some(State::BlockedUntil(deadline)),
            PostSwitch::BlockedUntil(deadline),
        );
    }

    // Unpark the thread at index
    pub(super) fn unpark(&self, handle: &ThreadHandle) {
        let mut threads = self.threads.lock();
        let mut did_unpark: bool = false;
        let affinity = if matches!(
            threads.thread_blocks[handle.idx].state,
            State::Blocked | State::BlockedUntil(_)
        ) && threads.thread_blocks[handle.idx].id == handle.id
        {
            did_unpark = true;
            // Note - does not deal with lost wakeup yet
            threads.thread_blocks[handle.idx].state = State::Ready;
            threads.thread_blocks[handle.idx].affinity
        } else {
            None
        };
        drop(threads);
        if did_unpark {
            // If the unparked thread has affinity for the other hart, send an IPI
            if let Some(h) = affinity
                && h as usize != crate::arch::cpu_id()
            {
                crate::kernel::ipi::send(h as usize);
            } else {
                // In order to avoid waiting a time slice, set the preempt flag
                percpu::set_needs_reschedule();
            }
        }
    }

    /// Get the current thread handle
    pub fn current_thread(&self) -> ThreadHandle {
        let threads = self.threads.lock();
        let idx = percpu::current_thread_idx();
        ThreadHandle {
            id: threads.thread_blocks[idx].id,
            idx,
        }
    }

    /// Sets the TCB's next_waiter for the thread the handle points to.
    pub fn set_next_waiter(&self, handle: &ThreadHandle, next: Option<ThreadHandle>) {
        let mut threads = self.threads.lock();
        if threads.thread_blocks[handle.idx].id == handle.id {
            threads.thread_blocks[handle.idx].next_waiter = next;
        }
    }

    /// Get waiter tcb index
    pub fn get_next_waiter(&self, handle: &ThreadHandle) -> Option<ThreadHandle> {
        let threads = self.threads.lock();
        if threads.thread_blocks[handle.idx].id == handle.id {
            threads.thread_blocks[handle.idx].next_waiter
        } else {
            None
        }
    }

    /// Unpark a thread by TCB index
    #[allow(dead_code)]
    pub fn unpark_by_index(&self, idx: usize) {
        let mut threads = self.threads.lock();
        if threads.thread_blocks[idx].state == State::Blocked {
            threads.thread_blocks[idx].state = State::Ready;
            // Ask for immediate reschedule to avoid waiting for a time slice
            percpu::set_needs_reschedule();
        }
    }

    /// Set this thread to blocked state without rescheduling
    pub fn set_self_blocked(&self) {
        let current = self.current_thread();
        let mut threads = self.threads.lock();
        threads.thread_blocks[current.idx].state = State::Blocked;
    }

    /// Set this thread to blocked state without rescheduling with a wake up deadline
    pub fn set_self_blocked_until(&self, deadline_ms: u64) {
        let deadline = Deadline {
            min: deadline_ms * timer::CYCLES_PER_MS,
            fixed_leeway: None,
        };
        let current = self.current_thread();
        let mut threads = self.threads.lock();
        threads.thread_blocks[current.idx].state = State::BlockedUntil(deadline);
    }
}

/// Generate a new thread id
fn next_thread_id() -> u32 {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Call exit at the end of a spawned closure
extern "C" fn run_closure_thread<F: FnOnce() + Send + 'static>(entry_ptr: *mut u8) -> ! {
    let e = unsafe { Box::from_raw(entry_ptr as *mut F) };
    e(); // runs the closure exactly once and consumes both the closure and the Box.
    SCHEDULER.exit()
}
