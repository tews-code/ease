//! Preemptive multitasking with stride scheduling

use core::ptr::NonNull;

use super::Qos;
use crate::arch::{csr, hart_id};
use crate::kernel::alloc::Order;
use crate::kernel::collection::{AtomicBitmap, bitmap_words_for};
use crate::kernel::sched::MemRegion;
use crate::kernel::sched::THREADS_MAX;
use crate::kernel::sched::deadline::Deadline;
use crate::kernel::sched::process::{PROCS_MAX, Procs};
use crate::kernel::sched::threads::{
    ExitReason, PostSwitch, State, ThreadControlBlock, ThreadControlBlockSpec, ThreadHandle,
    Threads,
};
#[cfg(feature = "paint-stack")]
use crate::kernel::stack::print_stack_watermark;
use crate::kernel::stack::{STACK_CANARY, check_canary};
use crate::kernel::sync::{CounterU64, IrqSpinLock, IrqSpinLockGuard, with_interrupts_disabled};
use crate::kernel::{ipi, percpu, timer};

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

// Priority is 0 (highest, does not stride/age) to 255 (idle)
pub const PRIORITY_DEFAULT: u8 = u8::MAX / 2;
pub const PRIORITY_MIN: u8 = u8::MAX - 1;

// Under contention use this time slice per thread
const SLICE_US: u64 = 16_000;
pub(super) const SLICE: u64 = SLICE_US * timer::CYCLES_PER_US;
pub(super) const BONUS: u64 = 500 * timer::CYCLES_PER_US;

// A Ready thread waiting longer than this is anomalous (longer than a full
// slice means it lost to something it should have beaten); the trace records
// a `ready-stall` snapshot when it happens.
#[cfg(feature = "trace")]
const READY_STALL: u64 = 10 * timer::CYCLES_PER_MS;

// User thread constants
#[allow(dead_code)]
const USER_MEM_REGION_8KB: u8 = 13; // Order is log2(N)
#[allow(dead_code)]
const USER_MEM_REGION_16KB: u8 = 14;

pub(super) static SCHEDULER: Scheduler = Scheduler::new();

pub(super) struct SchedInner {
    pub(super) thread_blocks: super::threads::Threads,
    pub(super) process_blocks: super::process::Procs, // Two thread control blocks are taken up by idle so can't be used for a process
    #[cfg(feature = "trace")]
    pub(super) wake_overshoot: [u64; THREADS_MAX],
}

impl SchedInner {
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
        let this_hart = crate::arch::hart_id() as u8;
        let mut best_idx = None;
        let mut best_pass = u64::MAX;
        for (idx, slot) in self.thread_blocks.0.iter().enumerate() {
            if let Some(tcb) = slot {
                let candidate = tcb.state == State::Ready;
                let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
                // Don't pick the thread the OTHER hart is currently running (that
                // would run it on two harts → corruption). Only applies when the
                // other hart is online: during boot it isn't, and its (zeroed)
                // current_thread_idx would otherwise wrongly exclude slot 0.
                let not_stealing =
                    !percpu::other_online() || idx != percpu::other_current_thread_idx();
                let pri_ok = tcb.priority != PRIORITY_MIN;
                if candidate && not_stealing && affinity_ok && pri_ok && tcb.pass < best_pass {
                    best_pass = tcb.pass;
                    best_idx = Some(idx);
                }
            }
        }
        let next_idx = best_idx.unwrap_or_else(percpu::idle_thread_idx);
        if curr_idx == next_idx {
            None
        } else {
            let [curr, next] = self
                .thread_blocks
                .0
                .get_disjoint_mut([curr_idx, next_idx])
                .expect("indices have been selected as disjoint");
            Some((
                curr.as_mut().unwrap(),
                curr_idx,
                next.as_mut().unwrap(),
                next_idx,
            ))
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
        let this_hart = crate::arch::hart_id() as u8;
        let mut best_idx = None;
        let mut best_pass = u64::MAX;
        for (idx, slot) in self.thread_blocks.0.iter().enumerate() {
            if let Some(tcb) = slot {
                let candidate = tcb.state == State::Ready || idx == curr_idx;
                let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
                // Don't pick the thread the OTHER hart is currently running (that
                // would run it on two harts → corruption). Only applies when the
                // other hart is online: during boot it isn't, and its (zeroed)
                // current_thread_idx would otherwise wrongly exclude slot 0.
                let not_stealing =
                    !percpu::other_online() || idx != percpu::other_current_thread_idx();
                let pri_ok = tcb.priority != PRIORITY_MIN;
                if candidate && not_stealing && affinity_ok && pri_ok && tcb.pass < best_pass {
                    best_pass = tcb.pass;
                    best_idx = Some(idx);
                }
            }
        }
        let next_idx = best_idx.unwrap_or_else(percpu::idle_thread_idx);
        if curr_idx == next_idx {
            None
        } else {
            let [curr, next] = self
                .thread_blocks
                .0
                .get_disjoint_mut([curr_idx, next_idx])
                .expect("indices have been selected as disjoint");
            Some((
                curr.as_mut().unwrap(),
                curr_idx,
                next.as_mut().unwrap(),
                next_idx,
            ))
        }
    }

    #[inline(never)]
    fn check_curr_canary(&self) {
        let curr = &self.thread_blocks.0[percpu::current_thread_idx()]
            .as_ref()
            .expect("current thread should be a valid TCB");
        // Safety: base address is aligned and valid for reads either from linker script or buddy allocation
        if let Err(val) = unsafe { check_canary(curr.kernel_stack.base_addr()) } {
            panic!(
                "kernel stack canary corrupted in thread {}: sp={:?}, base={:#x}, read={:#x}, expected={:#x}",
                curr.id,
                curr.sp,
                curr.kernel_stack.base_addr(),
                val,
                STACK_CANARY
            )
        };
    }

    // Set the PerCpu info for a running thread on this HART
    // and set mscratch to top IRQ stack for M-mode or kernel stack for U-mode
    fn activate_thread(
        &mut self,
        current_thread_idx: usize,
        switching_from_thread_idx: Option<usize>,
    ) {
        // Set PerCpu
        let stack: &MemRegion;
        if let Some(tcb) = &self.thread_blocks.0[current_thread_idx] {
            stack = &tcb.kernel_stack;
            percpu::set_current_thread_idx(current_thread_idx);
            percpu::set_switching_from_thread_idx(switching_from_thread_idx);
            percpu::set_current_stack_base(stack.base().as_ptr());

            if let Some(user_context) = &tcb.user {
                // User thread has kernel stack top in mscratch
                unsafe {
                    csr::mscratch::write(stack.top().addr().into());
                }

                // For user thread merge the thread's stack and set pmp
                let pmp_config = self.process_blocks.0[user_context.process_idx as usize]
                    .as_ref()
                    .expect("process should be configured before this thread is scheduled")
                    .mem_map
                    .to_pmp(&user_context.user_stack);
                pmp_config.activate();
            } else {
                // Kernel thread has IRQ stack top in mscratch
                if hart_id() == 0 {
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
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for SchedInner {}

pub(super) struct Scheduler {
    pub(super) sched: IrqSpinLock<SchedInner>,
    // Outside of sched inner for lock-free access
    pub(super) needs_wakeup: AtomicBitmap<THREADS_MAX, { bitmap_words_for(THREADS_MAX) }>,
    run_cycles: [CounterU64; THREADS_MAX],
}

unsafe extern "C" {
    static __hart0_idle_stack_base: u8;
    static __hart1_idle_stack_base: u8;
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            sched: IrqSpinLock::new(SchedInner {
                thread_blocks: Threads([const { None }; THREADS_MAX]),
                process_blocks: Procs([const { None }; PROCS_MAX]),
                #[cfg(feature = "trace")]
                wake_overshoot: [0; THREADS_MAX],
            }),
            needs_wakeup: AtomicBitmap::new(),
            run_cycles: [const { CounterU64::new(0) }; THREADS_MAX],
        }
    }

    // Set up the boot thread for each hart
    pub(super) fn bootstrap(&self, hartid: usize) {
        let mut sched = self.sched.lock();
        assert_eq!(
            &raw const __idle_stack_size as usize,
            Order::KB2.size(),
            "idle stack size disagrees with linker"
        );
        let thread_handle = sched
            .thread_blocks
            .acquire(
                |stack_region| stack_region.top(),
                ThreadControlBlockSpec {
                    kernel_stack: MemRegion::from_fixed(
                        NonNull::new(match hartid {
                            0 => &raw const __hart0_idle_stack_base as *mut u8,
                            1 => &raw const __hart1_idle_stack_base as *mut u8,
                            _ => unreachable!("only running two harts"),
                        })
                        .unwrap(),
                        Order::KB2,
                    ),
                    priority: PRIORITY_MIN,
                    qos: Qos::Low,
                    affinity: None,
                    user: None,
                },
            )
            .expect("boot strap thread must succeed to start system");
        percpu::set_idle_thread_idx(thread_handle.idx);
        self.needs_wakeup.clear(thread_handle.idx); // Best be certain that the idle thread isn't marked for wake ups
        sched.activate_thread(thread_handle.idx, None);
    }

    // Wakes any threads past their deadlines or which have a wake flag set
    // Sets their pass to pass_baseline so they do not monopolise their Hart as their pass
    // catches up with threads that were running
    //
    // Returns a count of the ready threads
    pub(super) fn wake_sleeping_threads(&self, sched: &mut IrqSpinLockGuard<SchedInner>) -> bool {
        let now = timer::elapsed();
        let other_curr_idx = percpu::other_current_thread_idx();
        let mut pass_baseline: Option<u64> = None; // This will be calculated by the Threads wake_if_due method if needed
        let tcbs = &mut sched.thread_blocks;
        for idx in 0..THREADS_MAX {
            if idx == percpu::current_thread_idx() {
                continue;
            }
            if let Some((pass, _at_cycles, affinity)) =
                tcbs.wake_if_due(idx, now, &mut pass_baseline, BONUS)
            {
                // Stash the timer overshoot
                #[cfg(feature = "trace")]
                {
                    // self.wake_overshoot[idx] = now.saturating_sub(_at_cycles);
                }
                // Interrupt other hart if appropriate
                if let Some(other_hart_tcb) = &tcbs.0[other_curr_idx]
                    && other_hart_tcb.pass >= pass
                    && affinity.is_none_or(|affinity| affinity != hart_id() as u8)
                {
                    crate::kernel::ipi::send(hart_id() ^ 1);
                }
            }
            if self.needs_wakeup.take(idx) {
                self.wake_by_index(tcbs, idx);
            }
        }
        tcbs.is_under_contention()
    }

    // Clean up a thread post switch
    pub(super) fn post_switch_cleanup(&self) {
        let mut sched = self.sched.lock();
        let switched_from_idx = percpu::take_switching_from_thread_idx()
            .expect("should have a Switching thread in post switch cleanup");
        debug_assert!(
            switched_from_idx != percpu::current_thread_idx(),
            "post_switch_cleanup marking working on the live thread control block: idx={switched_from_idx}"
        );
        // If the thread is dead then clean up and end the routine
        // `state` is Copy so lift this out of the Threads array
        let state = sched.thread_blocks.0[switched_from_idx]
            .as_ref()
            .map(|tcb| tcb.state);
        // Now deconstruct the state for dead threads
        if let Some(State::Switching(PostSwitch::Dead(exit_reason))) = state {
            // If we have been painting the stack then display high watermark on exit
            #[cfg(feature = "paint-stack")]
            {
                if let Some(tcb) = sched.thread_blocks.0[switched_from_idx].as_ref() {
                    let kernel_stack = &tcb.kernel_stack;
                    let id = tcb.id;
                    // Safety: kernel stack is aligned and valid for reads
                    unsafe {
                        print_stack_watermark(
                            "Thread",
                            id as usize,
                            kernel_stack.base_addr(),
                            kernel_stack.top().addr().into(),
                        );
                    }
                }
            }
            // For user processes we need to release all the threads in that process on fault.
            if exit_reason == ExitReason::Fault {
                // `process_idx` is u8, so Copy, hence lift it out of the threads struct
                let exit_process_idx = sched.thread_blocks.0[switched_from_idx]
                    .as_ref()
                    .and_then(|tcb| tcb.user.as_ref())
                    .map(|user_context| user_context.process_idx)
                    .expect("Exit reason `ExitReason::Fault` not supported on kernel threads");
                for idx in 0..THREADS_MAX {
                    let mut release = false;
                    if idx == switched_from_idx {
                        continue;
                    }
                    let Some(tcb) = sched.thread_blocks.0[idx].as_mut() else {
                        continue;
                    };
                    if tcb
                        .user
                        .as_ref()
                        .is_some_and(|uc| uc.process_idx == exit_process_idx)
                    {
                        match tcb.state {
                            State::Blocked | State::BlockedUntil(_) => {
                                panic!("do not yet support blocked user threads")
                            }
                            State::Ready | State::Sleeping(_) => release = true,
                            State::Running => {
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
                        sched.release_process_thread(exit_process_idx)
                    }
                }
            }
            // Set the dead thread to None and early return
            if let Some(process_idx) = sched.thread_blocks.0[switched_from_idx]
                .as_ref()
                .and_then(|tcb| tcb.user.as_ref())
                .map(|uc| uc.process_idx)
            {
                sched.release_process_thread(process_idx);
            }
            sched.thread_blocks.0[switched_from_idx] = None;
            return;
        }
        // All other post switch cleanup for living thread
        let tcb = sched.thread_blocks.0[switched_from_idx]
            .as_mut()
            .expect("must be post switch in a valid thread");
        let new_state = match tcb.state {
            State::Switching(PostSwitch::Blocked) => State::Blocked,
            State::Switching(PostSwitch::BlockedUntil(d)) => State::BlockedUntil(d),
            State::Switching(PostSwitch::Ready) => State::Ready,
            State::Switching(PostSwitch::Sleeping(d)) => State::Sleeping(d),
            _ => panic!(
                "post switch switched_from={switched_from_idx} current={} state={:?}",
                percpu::current_thread_idx(),
                tcb.state
            ),
        };
        tcb.state = new_state;
        // But if the thread we just switched has already been flagged to wake, call wake_by_index
        if self.needs_wakeup.take(switched_from_idx) {
            self.wake_by_index(&mut sched.thread_blocks, switched_from_idx);
        }
        // If the thread is now Ready we need to check if this should run on the other hart
        if new_state == State::Ready {
            #[cfg(feature = "trace")]
            {
                // Move the priority out of the thread array
                let priority = if let Some(tcb) = sched.thread_blocks.0[switched_from_idx].as_mut()
                {
                    tcb.ready_since = timer::elapsed(); // stamp Ready entry
                    tcb.priority
                } else {
                    return;
                };
                // Capture the instant a real (non-idle) thread becomes Ready via a
                // switch-out, so we can see what each hart is doing right when the
                // later-stalled thread enters the run queue. Skip idle (PRI_MIN) to
                // avoid flooding.
                if priority != PRIORITY_MIN {
                    sched.snapshot_raw("ps-ready");
                }
            }
            if percpu::other_online() {
                let other_idx = percpu::other_current_thread_idx();
                let other = sched.thread_blocks.0[other_idx].as_ref().unwrap();
                let now = timer::elapsed();
                let other_effective_pass = other.pass.saturating_add(
                    now.saturating_sub(other.last_started_cycles)
                        .saturating_mul(other.priority as u64),
                );
                if sched.thread_blocks.0[switched_from_idx]
                    .as_ref()
                    .is_some_and(|tcb| tcb.pass < other_effective_pass)
                {
                    crate::kernel::ipi::send(hart_id() ^ 1);
                }
            }
        }
    }

    // Helper function shared by `schedule` (preempt called from trap handler) and `reschedule` (voluntary) scheduler calls
    // Performs common bookkeeping
    fn slice_ended(
        &self,
        sched: &mut IrqSpinLockGuard<SchedInner>,
        curr_idx: usize,
        now_cycles: u64,
    ) {
        sched.check_curr_canary();
        let curr = &mut sched.thread_blocks.0[curr_idx]
            .as_mut()
            .expect("current thread should be running with valid TCB");
        let ran = now_cycles - curr.last_started_cycles;
        curr.last_started_cycles = now_cycles;
        unsafe { self.run_cycles[curr_idx].add(ran) };
        curr.stride_forward(ran);
    }

    // Set current thread state to `new_state` and next thread to `Ready` in round robin
    // If `currrent_state` is Some(State), reschedule only takes place if the
    // current thread state matches; if `current_state` is None then reschedule
    // unconditionally
    #[cfg_attr(feature = "trace", ease_macros::trace)]
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
            let mut sched = self.sched.lock();
            let now_cycles = timer::elapsed();
            let curr_idx = percpu::current_thread_idx();
            self.slice_ended(&mut sched, curr_idx, now_cycles);
            // First check if current state matches pre-condition
            if let Some(state) = current_state {
                let idx = percpu::current_thread_idx();
                if let Some(tcb) = sched.thread_blocks.0[idx].as_mut()
                    && tcb.state != state
                {
                    /*If a racing unpark
                    flips the thread to Ready just before the lock, the precondition fails and you bail — but without your line the thread is left marked Ready
                    while it's actually executing on this hart. The other hart could then pick_next it and switch to it → the same thread running on two harts →
                        stack corruption. Forcing it back to Running on the abort path is correct (the current thread always continues running here).*/
                    tcb.state = State::Running;
                    return;
                }
            }
            // Check if any threads have reached or passed their deadline
            self.wake_sleeping_threads(&mut sched);
            // A yield hands off to a ready peer but never idles: if the pick
            // fell back to idle (no ready peer), keep running curr instead. For
            // sleep/block/exit curr is leaving, so the idle fallback is correct;
            // marked_for_exit likewise must switch out (so the thread can die).
            // Capture these before the pick so reading marked_for_exit doesn't
            // alias the &mut refs pick_next_ready_mut hands back.
            let idle_idx = percpu::idle_thread_idx();
            let is_yield = new_state == PostSwitch::Ready
                && !sched.thread_blocks.0[curr_idx]
                    .as_ref()
                    .is_some_and(|tcb| tcb.marked_for_exit);
            let mut disjoint_threads = sched.pick_next_ready_mut();
            if is_yield && matches!(&disjoint_threads, Some((.., n)) if *n == idle_idx) {
                disjoint_threads = None;
            }
            let Some((curr, curr_idx, next, next_idx)) = disjoint_threads else {
                timer::set_next_deadline(
                    sched
                        .thread_blocks
                        .next_timer_deadline(SLICE, timer::elapsed()),
                );
                drop(sched);
                return;
            };
            // Ready to switch
            // Set current thread to the new state, unless it is marked for exit
            curr.state = if curr.marked_for_exit {
                State::Switching(PostSwitch::Dead(ExitReason::Fault))
            } else {
                State::Switching(new_state)
            };

            next.state = State::Running;
            next.last_started_cycles = now_cycles;

            // Create local variables before dropping the lock
            let prev_sp_ptr = &raw mut curr.sp;
            let next_sp_ptr = &raw mut next.sp;

            // Diagnostic: we're about to switch a still-runnable (yielding)
            // thread out to the IDLE thread while it stays Ready — the strand.
            // Detected here (not in the pick) because `new_state` is known: the
            // pick excludes the still-Running current thread, so a yield with no
            // other Ready thread falls to idle and the yielder lands Ready behind
            // an idle hart.
            #[cfg(feature = "trace")]
            if next_idx == percpu::idle_thread_idx() && new_state == PostSwitch::Ready {
                sched.snapshot_raw("idle-pick");
            }

            sched.activate_thread(next_idx, Some(curr_idx));
            timer::set_next_deadline(
                sched
                    .thread_blocks
                    .next_timer_deadline(SLICE, timer::elapsed()),
            );
            drop(sched);

            if hart_id() == 0 {
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
            // Diagnostic: the FIRST point a just-switched-in thread executes
            // after resuming here. Lets us timestamp when a picked thread really
            // starts running on its hart (vs merely being marked Running), to
            // catch a "current in bookkeeping but not executing" stall.
            #[cfg(feature = "trace")]
            self.sched.lock().snapshot_raw("post-resume");
        });
    }

    pub(super) fn schedule(&self) {
        if percpu::take_needs_reschedule() {
            with_interrupts_disabled(|_cs| {
                let mut sched = self.sched.lock();
                let now_cycles = timer::elapsed();
                let curr_idx = percpu::current_thread_idx();
                self.slice_ended(&mut sched, curr_idx, now_cycles);

                // Check if thread is marked for exit
                let marked_for_exit = sched.thread_blocks.0[percpu::current_thread_idx()]
                    .as_ref()
                    .is_some_and(|tcb| tcb.marked_for_exit);
                if marked_for_exit {
                    drop(sched);
                    self.exit(ExitReason::Fault);
                }

                // Wake any sleeping threads before we pick the next (if fairer)
                self.wake_sleeping_threads(&mut sched);
                // Pick the next thread to run (or keep running if has lowest pass)
                let pick = sched.pick_next_if_fairer_mut();
                let Some((curr, curr_idx, next, next_idx)) = pick else {
                    timer::set_next_deadline(
                        sched
                            .thread_blocks
                            .next_timer_deadline(SLICE, timer::elapsed()),
                    );
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
                sched.activate_thread(next_idx, Some(curr_idx));
                timer::set_next_deadline(
                    sched
                        .thread_blocks
                        .next_timer_deadline(SLICE, timer::elapsed()),
                );
                drop(sched);

                if hart_id() == 0 {
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

    /// Perform switch accounting and set reschedule flag
    #[cfg_attr(feature = "profile", profile)]
    pub(super) fn mark_for_preempt(&self) {
        // Flag any Ready thread that's been waiting longer than a slice — it
        // should have been scheduled by now. Snapshot why (which thread holds
        // the CPU it out-ranks).
        #[cfg(feature = "trace")]
        {
            let sched = self.sched.lock();
            let now = timer::elapsed();
            for idx in 0..THREADS_MAX {
                if let Some(tcb) = sched.thread_blocks.0[idx].as_ref()
                    && tcb.state == State::Ready
                    && tcb.priority != PRIORITY_MIN
                    && now.saturating_sub(tcb.ready_since) > READY_STALL
                {
                    sched.snapshot_ready_stall(idx);
                    break;
                }
            }
        }

        // To avoid a timer IRQ storm, set the timer deadline to now + SLICE
        // This will quickly be replaced by an accurate calculation in `schedule`.
        timer::set_next_deadline(timer::elapsed() + SLICE);
        // Flag that scheduling is needed
        percpu::set_needs_reschedule();
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
            at_cycles: deadline_ms.saturating_mul(timer::CYCLES_PER_MS),
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

    /// Cycles by which the current thread's last timer wake overshot its
    /// deadline (set in `wake_sleeping_threads`). Distinguishes a late timer
    /// wake from on-time-wake-but-late-to-run.
    #[cfg(feature = "trace")]
    #[allow(dead_code)] // used only by test probes
    pub(super) fn current_wake_overshoot(&self) -> u64 {
        let sched = self.sched.lock();
        sched.wake_overshoot[percpu::current_thread_idx()]
    }

    pub(super) fn exit(&self, reason: ExitReason) -> ! {
        self.reschedule(None, PostSwitch::Dead(reason));
        // reschedule switches away. If we get here, no other thread was
        // available to switch to, which means this thread is the only one
        // alive on this hart and we can't actually die. Panic — it's a
        // programmer error to call exit() on the last thread.
        unreachable!(
            "exit() called but no other thread to switch to on hart {}
            percpu::current_thread_idx is {}
            percpu::idle_thread_idx is {}
            {:?}",
            hart_id(),
            percpu::current_thread_idx(),
            percpu::idle_thread_idx(),
            self.sched.lock().thread_blocks
        );
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
            at_cycles: deadline_ms * timer::CYCLES_PER_MS,
            fixed_leeway: None,
        };
        self.reschedule(
            Some(State::BlockedUntil(deadline)),
            PostSwitch::BlockedUntil(deadline),
        );
    }

    // Set a wakeup flag for a particular thread
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn set_wakeup_flag(&self, idx: usize) {
        self.needs_wakeup.set(idx);
    }

    // Clear the wakeup flag for a particular thread
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn clear_wakeup_flag(&self, idx: usize) {
        self.needs_wakeup.clear(idx);
    }

    // Helper function used by unpark and wake_sleeping_threads
    pub(super) fn wake_by_index(&self, threads: &mut Threads, idx: usize) {
        let (did_unpark, affinity) = threads.make_unparked_ready(idx);
        if did_unpark {
            // If the unparked thread has affinity for the other hart, send an IPI
            if let Some(h) = affinity
                && h as usize != crate::arch::hart_id()
            {
                crate::kernel::ipi::send(h as usize);
            } else {
                // In order to avoid waiting a time slice, set the preempt flag
                percpu::set_needs_reschedule();
            }
        }
    }

    // Unpark the thread at index
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn unpark(&self, handle: &ThreadHandle) {
        let mut sched = self.sched.lock();
        let id_ok = sched.thread_blocks.0[handle.idx]
            .as_ref()
            .is_some_and(|tcb| tcb.id == handle.id);
        if id_ok {
            self.wake_by_index(&mut sched.thread_blocks, handle.idx);
            // Post-wake state under the same lock: the woken thread should
            // now be Ready ("unpark" rows from the #[trace] macro are entry,
            // pre-wake).
            #[cfg(feature = "trace")]
            sched.snapshot_raw("unparked");
        }
    }

    /// Get the current thread handle
    pub fn current_thread(&self) -> ThreadHandle {
        let sched = self.sched.lock();
        let idx = percpu::current_thread_idx();
        let id = sched.thread_blocks.0[idx]
            .as_ref()
            .expect("current thread must exist")
            .id;
        ThreadHandle { id, idx }
    }

    /// Sets the TCB's next_waiter for the thread the handle points to.
    pub fn set_next_waiter(&self, handle: &ThreadHandle, next: Option<ThreadHandle>) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.thread_blocks.0[handle.idx].as_mut()
            && tcb.id == handle.id
        {
            tcb.next_waiter = next;
        }
    }

    /// Get waiter tcb index
    pub fn get_next_waiter(&self, handle: &ThreadHandle) -> Option<ThreadHandle> {
        let sched = self.sched.lock();
        if let Some(tcb) = sched.thread_blocks.0[handle.idx].as_ref()
            && tcb.id == handle.id
        {
            tcb.next_waiter
        } else {
            None
        }
    }

    /// Unpark a thread by TCB index
    #[allow(dead_code)]
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub fn unpark_by_index(&self, idx: usize) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.thread_blocks.0[idx].as_mut()
            && tcb.state == State::Blocked
        {
            tcb.state = State::Ready;
            #[cfg(feature = "trace")]
            {
                tcb.ready_since = timer::elapsed(); // stamp Ready entry
            }
            // Ask for immediate reschedule to avoid waiting for a time slice
            percpu::set_needs_reschedule();
        }
    }

    /// Set this thread to blocked state without rescheduling
    pub fn set_self_blocked(&self) {
        let current = self.current_thread();
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.thread_blocks.0[current.idx].as_mut() {
            tcb.state = State::Blocked;
        }
    }

    /// Set this thread to blocked state without rescheduling with a wake up deadline
    pub fn set_self_blocked_until(&self, deadline_ms: u64) {
        let deadline = Deadline {
            at_cycles: deadline_ms * timer::CYCLES_PER_MS,
            fixed_leeway: None,
        };
        let current = self.current_thread();
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.thread_blocks.0[current.idx].as_mut() {
            tcb.state = State::BlockedUntil(deadline);
        }
    }

    /// Print the painted stack high watermarks
    #[cfg(feature = "paint-stack")]
    pub fn stacks(&self) {
        let sched = self.sched.lock();
        sched.thread_blocks.stacks();
    }
}
