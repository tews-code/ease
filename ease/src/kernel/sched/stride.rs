//! Preemptive multitasking with stride scheduling

use core::ptr::NonNull;

use super::Qos;
use crate::arch::hart_id;
use crate::kernel::alloc::Order;
use crate::kernel::collection::{Arena, AtomicBitmap, bitmap_words_for};
use crate::kernel::fd;
use crate::kernel::ipi;
use crate::kernel::sched::Deadline;
use crate::kernel::sched::MemRegion;
use crate::kernel::sched::process;
use crate::kernel::sched::thread;
#[cfg(feature = "paint-stack")]
use crate::kernel::stack::print_watermark;
use crate::kernel::sync::{CounterU64, IrqSpinLock, IrqSpinLockGuard};
use crate::kernel::{interrupts, percpu, timer};

#[cfg(feature = "profile")]
use ease_macros::profile;

unsafe extern "C" {
    // Safety: caller must ensure prev points to a writable slot owned by the current
    // thread; next points to a slot containing a saved sp produced by a prior swap_to call or
    // by spawn's stack forging; calling with interrupts disabled is undefined;
    pub(crate) fn switch_to_h0(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
    pub(crate) fn switch_to_h1(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
    static __idle_stack_size: u8;
}

// Priority is 0 (highest, does not stride/age) to 255 (idle)
pub const PRIORITY_DEFAULT: u8 = u8::MAX / 2;
pub const PRIORITY_MIN: u8 = u8::MAX - 1;

/// Under contention use this time slice per thread
const SLICE_MS: u64 = 16;
pub(super) const SLICE: u64 = SLICE_MS * timer::CYCLES_PER_MS;
/// A woken thread is given this bonus to its pass to encourage low latency
pub(super) const WAKE_BONUS: u64 = 500 * timer::CYCLES_PER_US;
/// Any switch must have more than this number of cycles of pass to be switched
pub(super) const HYSTERESIS_CYCLES: u64 = WAKE_BONUS;

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

pub(crate) struct SchedInner {
    pub(super) threads: thread::Threads,
    pub(super) processes: super::process::Processes, // Two thread control blocks are taken up by idle so can't be used for a process
    #[cfg(feature = "trace")]
    pub(super) wake_overshoot: [u64; thread::MAX_COUNT],
    _private: (), // We use &SchedInner as a token to prove holding the Scheduler IrqSpinLock, so we use this private field to prevent it be created other than in stride.
}

impl SchedInner {
    /// Get disjoint mutable TCBs and their indices for current and next thread.
    /// Note there is always an idle thread in Ready if no other thread is available.
    ///
    /// # Panics #
    /// Panics if there is no idle thread, or idle calls reschedule
    #[inline(never)]
    fn pick_next_ready_mut(
        &mut self,
    ) -> Option<(
        &mut thread::ControlBlock,
        thread::Handle,
        &mut thread::ControlBlock,
        thread::Handle,
    )> {
        // Get current handle
        let curr_handle = percpu::current_thread();
        let this_hart = crate::arch::hart_id() as u8;
        let mut best_handle = None;
        let mut best_pass = u64::MAX;
        for (handle, tcb) in self.threads.tcbs.iter_with_handles() {
            let candidate = tcb.state == thread::State::Ready;
            let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
            // Don't pick the thread the OTHER hart is currently running (that
            // would run it on two harts → corruption). Only applies when the
            // other hart is online: during boot it isn't, and it is `None`.
            let not_stealing = percpu::try_other_current_thread(self).is_none_or(|h| h != handle);
            let pri_ok = tcb.priority != PRIORITY_MIN;
            if candidate && not_stealing && affinity_ok && pri_ok && tcb.pass < best_pass {
                best_pass = tcb.pass;
                best_handle = Some(handle);
            }
        }
        let next_handle = best_handle
            .unwrap_or_else(|| percpu::idle_thread().expect("idle thread should be installed"));
        if curr_handle == next_handle {
            None
        } else {
            let (curr, next) = self
                .threads
                .tcbs
                .get_disjoint_mut(curr_handle, next_handle)
                .expect("indices have been selected as disjoint");
            Some((curr, curr_handle, next, next_handle))
        }
    }

    // Get disjoint mutable TCBs for current and next
    #[inline(never)]
    fn pick_next_if_fairer_mut(
        &mut self,
    ) -> Option<(
        &mut thread::ControlBlock,
        thread::Handle,
        &mut thread::ControlBlock,
        thread::Handle,
    )> {
        let curr_handle = percpu::current_thread();
        let this_hart = hart_id() as u8;
        let mut best_handle = None;
        let mut best_pass = u64::MAX;
        let current_pass = self
            .threads
            .tcbs
            .get(curr_handle)
            .as_ref()
            .expect("current thread must have a valid TCB")
            .pass;
        for (handle, tcb) in self.threads.tcbs.iter_with_handles() {
            let candidate = tcb.state == thread::State::Ready || handle == curr_handle;
            let affinity_ok = tcb.affinity.is_none_or(|h| h == this_hart);
            // Don't pick the thread the OTHER hart is currently running (that
            // would run it on two harts → corruption). Only applies when the
            // other hart is online: during boot it isn't, and it is `None`
            let not_stealing = percpu::try_other_current_thread(self).is_none_or(|h| h != handle);
            let pri_ok = tcb.priority != PRIORITY_MIN;
            let pass_gap_greater_than_hysteresis = (handle == curr_handle)
                || (tcb.pass + (HYSTERESIS_CYCLES * tcb.priority as u64) < current_pass);
            if candidate
                && not_stealing
                && affinity_ok
                && pri_ok
                && pass_gap_greater_than_hysteresis
                && tcb.pass < best_pass
            {
                best_pass = tcb.pass;
                best_handle = Some(handle);
            }
        }
        let next_handle = best_handle
            .unwrap_or_else(|| percpu::idle_thread().expect("idle thread should be installed"));
        if curr_handle == next_handle {
            None
        } else {
            let (curr, next) = self
                .threads
                .tcbs
                .get_disjoint_mut(curr_handle, next_handle)
                .expect("indices have been selected as disjoint");
            Some((curr, curr_handle, next, next_handle))
        }
    }
    /// Set the PerCpu info for a running thread on this HART
    ///
    /// Takes a handle of the current thread as well as an `Option` on the handle
    /// of a thread that is switching out and will need post-switch cleanup
    fn activate_thread(
        &mut self,
        current_thread: thread::Handle,
        switching_from_thread: Option<thread::Handle>,
    ) {
        // Set PerCpu
        let tcb = self
            .threads
            .tcbs
            .get(current_thread)
            .expect("current thread must be installed");
        let kernel_stack = &tcb.kernel_stack;
        let user_stack_base = tcb.user.as_ref().map(|u| u.stack.base());
        percpu::set_current_thread(
            self,
            current_thread,
            kernel_stack.base(),
            kernel_stack.top(),
            user_stack_base,
            tcb.qos,
        );
        percpu::set_switching_from_thread(self, switching_from_thread);
        if let Some(user_context) = &tcb.user {
            // For user thread merge the thread's stack and set pmp
            let pmp_config = self
                .processes
                .pcbs
                .get(user_context.process)
                .expect("process should be configured before this thread is scheduled")
                .mem_map
                .to_pmp(&user_context.stack);
            pmp_config.activate();
        }
    }
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for SchedInner {}

pub(super) struct Scheduler {
    pub(super) sched: IrqSpinLock<SchedInner>,
    // Outside of sched inner for lock-free read access
    // Must be cleared at thread acquire
    // Flag for each thread - if it must be unblocked at next reschedule
    // Users should .take() the flag to atomically clear it
    // Even so, users of the flag should ensure the thread state is valid before acting on the flag
    pub(super) needs_unblocking:
        AtomicBitmap<{ thread::MAX_COUNT }, { bitmap_words_for(thread::MAX_COUNT) }>,
    // Flag for each (user) thread - if it must be exited at next U mode trap
    // Only set once one-way within a user thread's lifetime so lock-free reading is safe
    // Note that the users of the flag should check that the indexed thread is indeed a user thread
    pub(super) needs_user_exit:
        AtomicBitmap<{ thread::MAX_COUNT }, { bitmap_words_for(thread::MAX_COUNT) }>,
    run_cycles: [CounterU64; thread::MAX_COUNT],
}

unsafe extern "C" {
    static __hart0_idle_stack_base: u8;
    static __hart0_idle_stack_top: u8;
    static __hart1_idle_stack_base: u8;
    static __hart1_idle_stack_top: u8;
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            sched: IrqSpinLock::new(SchedInner {
                threads: thread::Threads { tcbs: Arena::new() },
                processes: process::Processes { pcbs: Arena::new() },
                #[cfg(feature = "trace")]
                wake_overshoot: [0; thread::MAX_COUNT],
                _private: (),
            }),
            needs_unblocking: AtomicBitmap::new(),
            needs_user_exit: AtomicBitmap::new(),
            run_cycles: [const { CounterU64::new(0) }; thread::MAX_COUNT],
        }
    }

    // Set up the boot thread for each hart
    pub(super) fn bootstrap(&self, hartid: usize) {
        let mut sched = self.sched.lock();
        let kernel_stack_base = match hartid {
            0 => NonNull::new(&raw const __hart0_idle_stack_base as *mut u8)
                .expect("linker should not set a null idle stack base"),
            1 => NonNull::new(&raw const __hart1_idle_stack_base as *mut u8)
                .expect("linker should not set a null idle stack base"),
            _ => unreachable!("only running two harts"),
        };
        let kernel_stack_top = match hartid {
            0 => NonNull::new(&raw const __hart0_idle_stack_top as *mut u8)
                .expect("linker should not set a null idle stack top"),
            1 => NonNull::new(&raw const __hart1_idle_stack_top as *mut u8)
                .expect("linker should not set a null idle stack top"),
            _ => unreachable!("only running two harts"),
        };
        assert_eq!(
            &raw const __idle_stack_size as usize,
            Order::KB2.size(),
            "idle stack size disagrees with linker"
        );
        let idle_thread = sched
            .threads
            .acquire(
                |stack_region| stack_region.top(),
                thread::ControlBlockSpec {
                    kernel_stack: MemRegion::from_fixed(kernel_stack_base, Order::KB2),
                    priority: PRIORITY_MIN,
                    qos: Qos::Low,
                    affinity: None,
                    user: None,
                },
            )
            .expect("boot strap thread must succeed to start system");
        percpu::set_idle_thread(&sched, idle_thread);
        percpu::set_current_thread(
            &sched,
            idle_thread,
            kernel_stack_base,
            kernel_stack_top,
            None,
            Qos::Low,
        );
        // Must clear the flags so that the new thread doesn't inherit prior thread's historic flags
        self.clear_thread_flags(idle_thread);
        sched.activate_thread(idle_thread, None);
    }
    /// Wakes any threads past their deadlines or which have a wake flag set
    /// Sets their pass to pass_baseline so they do not monopolise their Hart as their pass
    /// catches up with threads that were running
    ///
    /// Returns a count of the ready threads
    pub(super) fn wake_sleeping_threads(&self, sched: &mut IrqSpinLockGuard<SchedInner>) -> bool {
        let now = timer::elapsed();
        let current_thread = percpu::current_thread();
        let other_current_thread = percpu::try_other_current_thread(sched); // Note - could be `None` if other hart not yet installed
        let mut pass_baseline: Option<u64> = None; // This will be calculated by the Threads wake_if_due method if needed
        let threads_mut = &mut sched.threads;
        for idx in 0..thread::MAX_COUNT {
            if let Some(thread) = threads_mut.tcbs.handle_of(idx) {
                if thread == current_thread {
                    continue;
                }
                if let Some((pass, _at_cycles, affinity)) =
                    threads_mut.wake_if_due(thread, now, &mut pass_baseline, WAKE_BONUS)
                {
                    // Stash the timer overshoot
                    #[cfg(feature = "trace")]
                    {
                        // self.wake_overshoot[idx] = now.saturating_sub(_at_cycles);
                    }
                    // Interrupt other hart if appropriate
                    if let Some(other) = other_current_thread
                        && let Some(other_tcb) = threads_mut.tcbs.get(other)
                        && other_tcb.pass >= pass
                        && affinity.is_none_or(|affinity| affinity != hart_id() as u8)
                    {
                        ipi::send(ipi::RESCHEDULE);
                    }
                }
                if self.needs_unblocking.take(thread.idx()) {
                    self.wake_by_handle(threads_mut, thread);
                }
            }
        }
        sched.threads.is_under_contention()
    }
    /// Clean up a thread post switch
    /// Called by _every_ thread, even spawned threads (or
    /// threads returning from a forged trap).
    pub(super) fn post_switch_cleanup(&self) {
        let mut sched = self.sched.lock();
        let switched_from_thread = percpu::take_switching_from_thread(&sched)
            .expect("should have a Switching thread in post switch cleanup");
        debug_assert!(
            switched_from_thread != percpu::current_thread(),
            "post_switch_cleanup marking working on the live thread control block: idx={switched_from_thread:?}"
        );
        // If the thread is dead then clean up and end the routine
        // `state` is Copy so lift this out of the Threads array
        let state = sched
            .threads
            .tcbs
            .get(switched_from_thread)
            .map(|tcb| tcb.state);
        // Now deconstruct the state for dead threads
        if let Some(thread::State::Switching(thread::PostSwitch::Dead(_))) = state {
            // If we have been painting the stack then display high watermark on exit
            #[cfg(feature = "paint-stack")]
            {
                if let Some(tcb) = sched.threads.tcbs.get(switched_from_thread) {
                    let kernel_stack = &tcb.kernel_stack;
                    // Safety: kernel stack is aligned and valid for reads
                    unsafe {
                        print_watermark(
                            "Thread",
                            switched_from_thread.id(),
                            "Kernel",
                            kernel_stack.base_addr(),
                            kernel_stack.top().addr().into(),
                        );
                    }
                    // Print the user stack if this is a user thread
                    if let Some(uc) = &tcb.user {
                        // Safety: user stack is aligned and valid for reads
                        unsafe {
                            print_watermark(
                                "Thread",
                                switched_from_thread.id(),
                                "User",
                                uc.stack.base_addr(),
                                uc.stack.top().addr().into(),
                            );
                        }
                    }
                }
            }
            // For user processes we have already evicted all but the last thread in usermode::user_thread_exit
            // Set the dead thread to None and return early
            if let Some(process) = sched.threads.process_handle_of(switched_from_thread) {
                self.release_process_thread(&mut sched, process, switched_from_thread);
            }
            sched.threads.tcbs.take(switched_from_thread);
            self.clear_thread_flags(switched_from_thread); // Make sure we also clear the flags of the TCB that has been removed
            return;
        }
        // All other post switch cleanup for living thread
        let tcb = sched
            .threads
            .tcbs
            .get_mut(switched_from_thread)
            .expect("must be post switch in a valid thread");
        let new_state = match tcb.state {
            thread::State::Switching(thread::PostSwitch::Blocked) => thread::State::Blocked,
            thread::State::Switching(thread::PostSwitch::BlockedUntil(d)) => {
                thread::State::BlockedUntil(d)
            }
            thread::State::Switching(thread::PostSwitch::Ready) => thread::State::Ready,
            thread::State::Switching(thread::PostSwitch::Sleeping(d)) => thread::State::Sleeping(d),
            _ => panic!(
                "post switch switched_from={switched_from_thread:?} current={:?} state={:?}",
                percpu::current_thread(),
                tcb.state
            ),
        };
        tcb.state = new_state;
        // But if the thread we just switched has already been flagged to wake, call wake_by_index
        if self.needs_unblocking.take(switched_from_thread.idx()) {
            self.wake_by_handle(&mut sched.threads, switched_from_thread);
        }
        // If the thread is now Ready we need to check if this should run on the other hart
        if new_state == thread::State::Ready {
            #[cfg(feature = "trace")]
            {
                // Move the priority out of the thread array
                let priority = if let Some(tcb) = sched.threads.tcbs.get_mut(switched_from_thread) {
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
            if percpu::other_ipi_online()
                && let Some(tcb) = sched.threads.tcbs.get(switched_from_thread)
                && tcb
                    .affinity
                    .is_none_or(|hart| hart as usize != crate::arch::hart_id())
            {
                let other_handle = percpu::try_other_current_thread(&sched)
                    .expect("other hart should have current installed");
                let other = sched
                    .threads
                    .tcbs
                    .get(other_handle)
                    .expect("current thread handle should not be stale");
                let now = timer::elapsed();
                let other_effective_pass = other.pass.saturating_add(
                    now.saturating_sub(other.last_started_cycles)
                        .saturating_mul(other.priority as u64),
                );
                if tcb.pass < other_effective_pass {
                    ipi::send(ipi::RESCHEDULE);
                }
            }
        }
    }
    /// Set current thread state to `new_state` and next thread to `Ready` in round robin
    /// If `currrent_state` is Some(State), reschedule only takes place if the
    /// current thread state matches; if `current_state` is None then reschedule
    /// unconditionally
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn reschedule(
        &self,
        current_state: Option<thread::State>,
        new_state: thread::PostSwitch,
    ) {
        // Note - the closure body will contain switch_to, which is unusual
        // It's a non-local control transfer wearing the disguise of a function call.
        // It works correctly because the closure frame is preserved on the suspended thread's stack
        interrupts::with_interrupts_disabled(|_cs| {
            // Clear any reschedule flag as we are rescheduling
            let _ = percpu::take_needs_reschedule();
            // We manually take and release the lock before the
            // context switch — it's held in this
            // thread's stack frame, so switch_to would otherwise carry the lock
            // across the switch and block other harts/threads from rescheduling.
            let mut sched = self.sched.lock();
            let now_cycles = timer::elapsed();
            let current_thread = percpu::current_thread();
            {
                let curr_tcb = sched
                    .threads
                    .tcbs
                    .get_mut(current_thread)
                    .expect("the current thread must have a valid TCB");
                let ran = curr_tcb.slice_ended(now_cycles);
                unsafe { self.run_cycles[current_thread.idx()].add(ran) };
            }
            // Check if any threads have reached or passed their deadline
            // Note: we do this after the cycle bookkeeping above to avoid favouring waking threads
            self.wake_sleeping_threads(&mut sched);
            // Now reborrow the current thread tcb
            let curr_tcb = sched
                .threads
                .tcbs
                .get_mut(current_thread)
                .expect("the current thread must have a valid TCB");
            // First check if current state matches pre-condition
            if let Some(state) = current_state
                && curr_tcb.state != state
            {
                // If a racing unpark flips the thread to Ready just before the lock,
                // the precondition fails and you bail — but the thread is left marked Ready
                // while it's actually executing on this hart.
                // The other hart could then pick_next it and switch to it → the same thread running on two harts →
                // stack corruption.
                // Forcing it back to Running on the abort path is correct (the current thread always continues running here).
                curr_tcb.state = thread::State::Running;
                return;
            }
            // If the current thread is marked for exit then it can't be allowed to change state if it's trying to block or sleep.
            // It needs to keep running until it hits a U-mode trap or its wait loop's interruption check
            // which will cleanly exit it
            if self.needs_user_exit.get(current_thread.idx())
                && curr_tcb.user.is_some()
                && matches!(
                    new_state,
                    thread::PostSwitch::Blocked
                        | thread::PostSwitch::BlockedUntil(_)
                        | thread::PostSwitch::Sleeping(_)
                )
            {
                curr_tcb.state = thread::State::Running;
                return;
            }
            // A yield hands off to a Ready peer but never idles: if the pick
            // fell back to idle (no ready peer), keep running curr instead.
            // However, for Sleep/Block/Exit, curr is leaving, so the idle fallback is correct;
            let idle_handle = percpu::idle_thread().expect("idle thread should be installed");
            let is_yield = new_state == thread::PostSwitch::Ready;
            let mut disjoint_threads = sched.pick_next_ready_mut();
            // If we are due to yield but the next thread is the idle thread, stop and return early
            if is_yield && matches!(&disjoint_threads, Some((.., n)) if *n == idle_handle) {
                disjoint_threads = None;
            }
            let Some((curr, curr_handle, next, next_handle)) = disjoint_threads else {
                timer::set_next_deadline(sched.threads.next_timer_deadline(SLICE, now_cycles));
                drop(sched);
                return;
            };
            // Ready to switch
            curr.state = thread::State::Switching(new_state);
            next.state = thread::State::Running;
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
            if next_handle == percpu::idle_thread().expect("idle thread should be installed")
                && new_state == thread::PostSwitch::Ready
            {
                sched.snapshot_raw("idle-pick");
            }

            sched.activate_thread(next_handle, Some(curr_handle));
            timer::set_next_deadline(sched.threads.next_timer_deadline(SLICE, now_cycles));
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
    /// Preemptive involuntary reschedule
    ///
    /// The current thread is compared to [pick_next_if_fairer_mut] and
    /// can be switched out at that point.
    ///
    /// Note: we don't force needs_user_exit threads to exit here - they must complete preempt
    pub(super) fn schedule(&self) {
        if percpu::take_needs_reschedule() {
            interrupts::with_interrupts_disabled(|_cs| {
                let mut sched = self.sched.lock();
                let now_cycles = timer::elapsed();
                let curr_handle = percpu::current_thread();
                let curr_tcb = sched
                    .threads
                    .tcbs
                    .get_mut(curr_handle)
                    .expect("the current thread must have a valid TCB");
                let ran = curr_tcb.slice_ended(now_cycles);
                unsafe { self.run_cycles[curr_handle.idx()].add(ran) };
                // Wake any sleeping threads before we pick the next (if fairer)
                self.wake_sleeping_threads(&mut sched);
                // Pick the next thread to run (or keep running if has lowest pass)
                let pick = sched.pick_next_if_fairer_mut();
                let Some((curr, curr_handle, next, next_handle)) = pick else {
                    timer::set_next_deadline(sched.threads.next_timer_deadline(SLICE, now_cycles));
                    return;
                };
                // Perform switch
                curr.state = thread::State::Switching(thread::PostSwitch::Ready);
                next.state = thread::State::Running;
                next.last_started_cycles = now_cycles;
                // Create local variables before dropping the lock
                let prev_sp_ptr = &raw mut curr.sp;
                let next_sp_ptr = &raw mut next.sp;
                sched.activate_thread(next_handle, Some(curr_handle));
                timer::set_next_deadline(sched.threads.next_timer_deadline(SLICE, now_cycles));
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
            for (handle, tcb) in sched.threads.tcbs.iter_with_handles() {
                if tcb.state == thread::State::Ready
                    && tcb.priority != PRIORITY_MIN
                    && now.saturating_sub(tcb.ready_since) > READY_STALL
                {
                    sched.snapshot_ready_stall(handle);
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
        self.reschedule(None, thread::PostSwitch::Ready);
    }
    /// Blocks until the timer has passed the `Deadline`
    pub(super) fn sleep_until(&self, deadline: Deadline) {
        self.reschedule(None, thread::PostSwitch::Sleeping(deadline));
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
        sched.wake_overshoot[percpu::current_thread().idx()]
    }

    // THREAD EXIT

    pub(super) fn exit(&self, reason: thread::ExitReason) -> ! {
        self.reschedule(None, thread::PostSwitch::Dead(reason));
        // reschedule switches away. If we get here, no other thread was
        // available to switch to, which means this thread is the only one
        // alive on this hart and we can't actually die. Panic — it's a
        // programmer error to call exit() on the last thread.
        unreachable!(
            "exit() called but no other thread to switch to on hart {}
            percpu::current_thread_handle is {:?}
            percpu::idle_thread_handle is {:?}
            {:?}",
            hart_id(),
            percpu::current_thread(),
            percpu::idle_thread().expect("idle thread should be installed"),
            self.sched.lock().threads
        );
    }

    // PARK

    // Park the current thread
    #[allow(dead_code)]
    pub(super) fn park(&self) {
        self.reschedule(None, thread::PostSwitch::Blocked);
    }

    // Park the current thread if it is in blocked state
    pub(super) fn park_if_blocked(&self) {
        self.reschedule(Some(thread::State::Blocked), thread::PostSwitch::Blocked);
    }
    /// Park the current thread if it is in Blocked statue until the given `Deadline`
    /// at which point it will be woken.
    pub(super) fn park_if_blocked_until(&self, deadline: Deadline) {
        self.reschedule(
            Some(thread::State::BlockedUntil(deadline)),
            thread::PostSwitch::BlockedUntil(deadline),
        );
    }

    // FLAGS

    /// Set a wakeup flag for a particular thread
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn set_wakeup_flag(&self, handle: thread::Handle) {
        self.needs_unblocking.set(handle.idx());
    }
    /// Clear the wakeup flag for a particular thread
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn clear_wakeup_flag(&self, handle: thread::Handle) {
        self.needs_unblocking.clear(handle.idx());
    }
    /// Clear all flags for a thread
    pub(crate) fn clear_thread_flags(&self, handle: thread::Handle) {
        if handle.idx() < thread::MAX_COUNT {
            self.needs_unblocking.clear(handle.idx());
            self.needs_user_exit.clear(handle.idx());
        }
    }

    // Helper function used by unpark and wake_sleeping_threads
    pub(super) fn wake_by_handle(&self, threads: &mut thread::Threads, handle: thread::Handle) {
        match threads.make_blocked_ready(handle) {
            thread::UnblockedResult::Unparked(affinity) => {
                // If the unparked thread has affinity for the other hart, send an IPI
                if let Some(h) = affinity
                    && h as usize != crate::arch::hart_id()
                {
                    ipi::send(ipi::RESCHEDULE);
                } else {
                    // In order to avoid waiting a time slice, set the preempt flag
                    percpu::set_needs_reschedule();
                }
            }
            thread::UnblockedResult::Deferred => {
                // We need a wake up
                self.needs_unblocking.set(handle.idx());
            }
            thread::UnblockedResult::NotBlocked => {}
        }
    }

    // Unpark the thread at index
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub(super) fn unpark(&self, handle: thread::Handle) {
        let mut sched = self.sched.lock();
        if sched.threads.tcbs.contains(handle) {
            self.wake_by_handle(&mut sched.threads, handle);
            // Post-wake state under the same lock: the woken thread should
            // now be Ready ("unpark" rows from the #[trace] macro are entry,
            // pre-wake).
            #[cfg(feature = "trace")]
            sched.snapshot_raw("unparked");
        }
    }
    /// Sets the TCB's next_waiter for the thread the handle points to.
    pub fn set_next_waiter(&self, handle: thread::Handle, next: Option<thread::Handle>) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.threads.tcbs.get_mut(handle) {
            tcb.next_waiter = next;
        }
    }
    /// Get the next waiter's handle from the given thread handle.
    ///
    /// Returns `None` if there are no waiters, or `Some(thread::Handle)` if a waiter is found
    ///
    /// # Panics
    /// Panics if the handle is stale.
    pub fn get_next_waiter(&self, handle: thread::Handle) -> Option<thread::Handle> {
        self.sched
            .lock()
            .threads
            .tcbs
            .get(handle)
            .expect("get next waiter should not be given a stale handle")
            .next_waiter
    }
    /// Unpark a thread by handle
    #[cfg_attr(feature = "trace", ease_macros::trace)]
    pub fn unpark_by_handle(&self, handle: thread::Handle) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.threads.tcbs.get_mut(handle)
            && tcb.state == thread::State::Blocked
        {
            tcb.state = thread::State::Ready;
            #[cfg(feature = "trace")]
            {
                tcb.ready_since = timer::elapsed(); // stamp Ready entry
            }
            // Ask for immediate reschedule to avoid waiting for a time slice
            percpu::set_needs_reschedule();
        }
    }
    /// Set current thread to blocked state without rescheduling
    pub fn set_self_blocked(&self) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.threads.tcbs.get_mut(percpu::current_thread()) {
            tcb.state = thread::State::Blocked;
        }
    }
    /// Set current thread to blocked state without rescheduling with a wake up deadline
    pub fn set_self_blocked_until(&self, deadline: Deadline) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.threads.tcbs.get_mut(percpu::current_thread()) {
            tcb.state = thread::State::BlockedUntil(deadline);
        }
    }
    /// Set current thread to Running state without rescheduling
    pub fn set_self_running(&self) {
        let mut sched = self.sched.lock();
        if let Some(tcb) = sched.threads.tcbs.get_mut(percpu::current_thread()) {
            tcb.state = thread::State::Running;
        }
    }
    /// Print the painted stack high watermarks
    #[cfg(feature = "paint-stack")]
    pub fn stacks(&self) {
        let sched = self.sched.lock();
        sched.threads.stacks();
    }

    // FILE DESCRIPTORS

    /// Run a closure on the current process's file descriptors
    ///
    /// # Panics #
    /// Panics if the thread is not a user thread or holds a stale process handle
    fn with_current_process_fds<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut fd::Table) -> R,
    {
        let mut sched = self.sched.lock();
        let process = sched
            .threads
            .process_handle_of(percpu::current_thread())
            .expect("the current thread must be a user thread with a valid PCB handle");
        f(&mut sched
            .processes
            .pcbs
            .get_mut(process)
            .expect("the current thread's PCB should not be stale")
            .fds)
    }

    pub(super) fn open_fd(&self, fd_kind: fd::Kind) -> Result<usize, fd::Error> {
        self.with_current_process_fds(|fds| fds.open(fd_kind))
    }

    pub(super) fn close_fd(&self, fd: usize) -> Result<fd::Kind, fd::Error> {
        self.with_current_process_fds(|fds| fds.close(fd))
    }

    pub(super) fn new_process_fds(&self) {
        self.with_current_process_fds(|fds| fds.new_process());
    }
}
