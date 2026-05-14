//! Preemptive multitasking with stride scheduling

use alloc::boxed::Box;

use crate::arch::STACK_CANARY;
use crate::arch::context::switch_to;
use crate::board::HARTS_MAX;
use crate::kernel::sync::{CounterU64, IrqSpinLock, with_interrupts_disabled};
use crate::kernel::timer;
use core::sync::atomic::{AtomicU32, Ordering};

use super::types::{
    Deadline, HartState, HeapStack, PostSwitch, Qos, Stack, StackClass, State, THREADS_MAX,
    ThreadControlBlock, ThreadHandle, ThreadsInner,
};

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
            sp: core::ptr::null_mut(),
            stack: None,
            qos: Qos::Low,
            priority: PRIORITY_MIN,
            pass: 0,
            last_started_cycles: 0,
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

    // Deallocate the stack if heap-based
    fn release_stack(&mut self) {
        let stack = self
            .stack
            .take()
            .expect("should not be calling release_stack on uninitialised TCB");
        if let Stack::Heap(class) = stack {
            unsafe {
                HeapStack::deallocate(class, self.sp);
            }
        }
        self.sp = core::ptr::null_mut();
    }

    // Calculates the latest wake up for a thread including leeway
    // Returns None if thread is not sleeping
    fn wakeup_deadline(&self) -> Option<u64> {
        match self.state {
            State::Sleeping(deadline) | State::Switching(PostSwitch::Sleeping(deadline)) => {
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
        self.control_blocks
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
        self.control_blocks
            .iter()
            .filter(|tcb| {
                (tcb.state == State::Ready || tcb.state == State::Running)
                    && tcb.priority != PRIORITY_MIN
            })
            .map(|tcb| tcb.pass)
            .min()
            .unwrap_or(0)
    }

    // Wakes any threads past their deadlines
    //
    // Returns a count of the ready threads
    fn wake_sleeping_threads(&mut self) -> usize {
        let now = timer::elapsed();
        let mut ready_count: usize = 0;
        for tcb in &mut self.control_blocks {
            match tcb.state {
                State::Sleeping(Deadline {
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
        self.control_blocks
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
            for tcb in &mut self.control_blocks {
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
    fn pick_next_ready_mut(
        &mut self,
    ) -> (
        &mut ThreadControlBlock,
        usize,
        &mut ThreadControlBlock,
        usize,
    ) {
        // Find current index
        let curr_idx = self.this_hart().running_thread;
        // Find next index
        let next_idx = self
            .control_blocks
            .iter()
            .enumerate()
            .filter(|(_, tcb)| tcb.state == State::Ready)
            .min_by_key(|(_, tcb)| tcb.pass)
            .map(|(i, _)| i)
            .expect("there must be at least one Ready thread - idle is missing");
        // Get disjoint TCBs
        let [curr, next] = self
            .control_blocks
            .get_disjoint_mut([curr_idx, next_idx])
            .expect("indices have been selected as disjoint");
        (curr, curr_idx, next, next_idx)
    }

    // Get disjoint mutable TCBs for current and next
    fn pick_next_if_fairer_mut(
        &mut self,
    ) -> Option<(
        &mut ThreadControlBlock,
        usize,
        &mut ThreadControlBlock,
        usize,
    )> {
        // Find current index
        let curr_idx = self.this_hart().running_thread;
        let next_idx = self
            .control_blocks
            .iter()
            .enumerate()
            .filter(|(idx, tcb)| tcb.state == State::Ready || *idx == curr_idx)
            .min_by_key(|(_, tcb)| tcb.pass)
            .map(|(idx, _)| idx)?;
        if curr_idx == next_idx {
            None
        } else {
            let [curr, next] = self
                .control_blocks
                .get_disjoint_mut([curr_idx, next_idx])
                .expect("indices have been selected as disjoint");
            Some((curr, curr_idx, next, next_idx))
        }
    }

    fn check_curr_canary(&self) {
        let curr = &self.control_blocks[self.this_hart().running_thread];
        if let Some(stack) = &curr.stack {
            let class = stack.class();
            let base_addr = curr.sp.addr() & !class.mask();
            let val = unsafe { core::ptr::read(base_addr as *const usize) };
            assert!(
                val == STACK_CANARY,
                "stack canary corrupted in thread {}: sp={:p}, base={:#x}, read={:#x}, expected={:#x}",
                curr.id,
                curr.sp,
                base_addr,
                val,
                STACK_CANARY,
            );
        }
    }
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for ThreadsInner {}

pub(super) struct Scheduler {
    threads: IrqSpinLock<ThreadsInner>,
    run_cycles: [CounterU64; THREADS_MAX], // Outside of threads for lock-free read
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            threads: IrqSpinLock::new(ThreadsInner {
                control_blocks: [const { ThreadControlBlock::new() }; THREADS_MAX],
                hart_state: [const {
                    HartState {
                        running_thread: 0,
                        switching_thread: None,
                    }
                }; HARTS_MAX],
            }),
            run_cycles: [const { CounterU64::new(0) }; THREADS_MAX],
        }
    }

    // Set up the boot thread for each hart
    pub(super) fn bootstrap(&self, hartid: usize) {
        let id = next_thread_id();
        let mut threads = self.threads.lock();
        threads.control_blocks[slot_for_boot(hartid)] = ThreadControlBlock {
            id,
            state: State::Running,
            sp: crate::arch::csr::regs::sp() as *mut u8,
            stack: Some(Stack::Fixed),
            last_started_cycles: timer::elapsed(),
            ..ThreadControlBlock::new()
        };
        threads.this_hart_mut().running_thread = slot_for_boot(hartid)
    }

    /// Helper function to clean up post switch threads
    pub(super) fn post_switch_cleanup(&self) {
        // After switch_to returns (on this thread's eventual resume),
        let mut threads = self.threads.lock();
        let switched_idx = threads
            .this_hart_mut()
            .switching_thread
            .take()
            .expect("should have a Switching thread to set back to Ready");
        let new_state = {
            match threads.control_blocks[switched_idx].state {
                State::Switching(PostSwitch::Ready) => State::Ready,
                State::Switching(PostSwitch::Sleeping(d)) => State::Sleeping(d),
                State::Switching(PostSwitch::Blocked) => State::Blocked,
                State::Switching(PostSwitch::Dead) => {
                    threads.control_blocks[switched_idx].release_stack();
                    threads.control_blocks[switched_idx] = ThreadControlBlock::new();
                    State::Avail
                }
                _ => panic!("Post switch but not in not in switching state"),
            }
        };
        threads.control_blocks[switched_idx].state = new_state;
    }

    // Set up initial thread block and stack for a new thread
    pub(super) fn spawn<F: FnOnce() + Send + 'static>(
        &self,
        entry: F,
        priority: u8,
        class: StackClass,
        qos: Qos,
    ) -> Option<ThreadHandle> {
        // First allocate before locking
        let base = HeapStack::allocate(class)?;
        let b = Box::new(entry);
        let closure_ptr = Box::into_raw(b) as *mut u8;
        {
            use crate::io::DirectWriter;
            use core::fmt::Write;
            let _ = writeln!(
                DirectWriter,
                "spawn: base={:p} size_of::<F>()={} closure_ptr={:p}",
                base.as_ptr(),
                core::mem::size_of::<F>(),
                closure_ptr,
            );
        }

        let sp =
            unsafe { HeapStack::init_for_entry(base, class, closure_trampoline::<F>, closure_ptr) };
        // Now lock the scheduler
        let mut threads = self.threads.lock();
        // Get the current pass baselines so we don't schedule ahead of other threads
        let baseline = threads.pass_baseline();
        // Find a free TCB slot
        let Some((idx, tcb)) = threads.free_slot() else {
            drop(threads);
            unsafe {
                HeapStack::deallocate(class, sp);
            }
            return None;
        };
        // Initialise the TCB
        *tcb = ThreadControlBlock {
            id: next_thread_id(),
            sp,
            state: State::Ready,
            qos,
            priority,
            stack: Some(Stack::Heap(class)),
            pass: baseline,
            ..ThreadControlBlock::new()
        };
        // Local variables to drop threads
        let handle = ThreadHandle { id: tcb.id, idx };
        // Set the timer
        let ready_count = threads.wake_sleeping_threads();
        let earliest_deadline = threads.earliest_deadline();
        threads.set_next_timer(earliest_deadline, ready_count);
        Some(handle)
    }

    // Set current thread state to `new_state` and next thread to `Ready` in round robin
    pub(super) fn reschedule(&self, new_state: PostSwitch) {
        // Note - the closure body will contain switch_to, which is unusual
        // It's a non-local control transfer wearing the disguise of a function call.
        // It works correctly because the closure frame is preserved on the suspended thread's stack
        with_interrupts_disabled(|_cs| {
            // We manually take and release the lock before the
            // context switch — it's held in this
            // thread's stack frame, so switch_to would otherwise carry the lock
            // across the switch and block other harts/threads from rescheduling.
            let mut threads = self.threads.lock();
            threads.check_curr_canary();
            // Check if any threads have reached or passed their deadline
            let ready_count = threads.wake_sleeping_threads();
            // Get current and next TCBs
            let (curr, curr_idx, next, next_idx) = threads.pick_next_ready_mut();

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

            *threads.this_hart_mut() = HartState {
                running_thread: next_idx,
                switching_thread: Some(curr_idx),
            };
            let earliest_deadline = threads.earliest_deadline(); // Re-run after setting up sleeper
            threads.set_next_timer(earliest_deadline, ready_count);
            drop(threads);

            unsafe { switch_to(prev_sp_ptr, next_sp_ptr) };
            self.post_switch_cleanup();
            //mret has restored MIE via the thread trampoline
        });
    }

    /// Preempts from current thread to next thread
    ///
    /// Note: if only one thread is runnable returns early.
    pub(super) fn preempt(&self) {
        let mut threads = self.threads.lock();
        threads.check_curr_canary();
        // let sched = threads.wake_threads();
        let ready_count = threads.wake_sleeping_threads();
        let earliest_deadline = threads.earliest_deadline();

        // Apply current's stride upfront so pass comparisons in
        // pick_next_if_fairer_mut see a fresh value —
        // otherwise a long-running
        // thread keeps appearing to have its old (low) pass and
        // never loses a comparison.
        let now_cycles = timer::elapsed();
        let curr_idx = threads.this_hart().running_thread;
        {
            let curr = &mut threads.control_blocks[curr_idx];
            let ran = now_cycles - curr.last_started_cycles;
            curr.last_started_cycles = now_cycles;
            unsafe { self.run_cycles[curr_idx].add(ran) };
            curr.stride(ran);
        }

        let Some((curr, curr_idx, next, next_idx)) = threads.pick_next_if_fairer_mut() else {
            // Same thread is running uncontended, increase slice deadline
            threads.set_next_timer(earliest_deadline, ready_count);
            return;
        };
        // Perform switch
        curr.state = State::Switching(PostSwitch::Ready);
        next.state = State::Running;
        next.last_started_cycles = now_cycles;

        // Create local variables before dropping the lock
        let prev_sp_ptr = &raw mut curr.sp;
        let next_sp_ptr = &raw mut next.sp;
        *threads.this_hart_mut() = HartState {
            running_thread: next_idx,
            switching_thread: Some(curr_idx),
        };
        let earliest_deadline = threads.earliest_deadline();
        threads.set_next_timer(earliest_deadline, ready_count);
        drop(threads);

        unsafe { switch_to(prev_sp_ptr, next_sp_ptr) };

        // After switch_to returns (on this thread's eventual resume),
        self.post_switch_cleanup();
    }

    /// Yields current thread
    pub(super) fn yield_now(&self) {
        self.reschedule(PostSwitch::Ready);
    }

    /// Blocks until the timer has passed the deadline
    ///
    /// Time is measured in milliseconds
    pub(super) fn sleep_until(&self, deadline_ms: u64, fixed_leeway_ms: Option<u64>) {
        let deadline = Deadline {
            min: deadline_ms.saturating_mul(timer::CYCLES_PER_MS),
            fixed_leeway: fixed_leeway_ms.map(|l| l.saturating_mul(timer::CYCLES_PER_MS)),
        };
        self.reschedule(PostSwitch::Sleeping(deadline));
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
        self.reschedule(PostSwitch::Dead);
        // reschedule switches away. If we get here, no other thread was
        // available to switch to, which means this thread is the only one
        // alive on this hart and we can't actually die. Panic — it's a
        // programmer error to call exit() on the last thread.
        unreachable!("exit() called but no other thread to switch to");
    }

    // Park the current thread
    pub(super) fn park(&self) {
        self.reschedule(PostSwitch::Blocked)
    }

    // Unpark the thread at index
    pub(super) fn unpark(&self, handle: ThreadHandle) {
        let mut threads = self.threads.lock();
        if threads.control_blocks[handle.idx].state == State::Blocked
            && threads.control_blocks[handle.idx].id == handle.id
        {
            // Note - does not deal with lost wakeup yet
            threads.control_blocks[handle.idx].state = State::Ready;
        }
    }

    /// Get the current thread handle
    pub fn current_thread(&self) -> ThreadHandle {
        let threads = self.threads.lock();
        let idx = threads.this_hart().running_thread;
        ThreadHandle {
            id: threads.control_blocks[idx].id,
            idx,
        }
    }
}

/// Generate a new thread id
fn next_thread_id() -> u32 {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Call exit at the end of a spawned closure
extern "C" fn closure_trampoline<F: FnOnce() + Send + 'static>(entry_ptr: *mut u8) -> ! {
    let e = unsafe { Box::from_raw(entry_ptr as *mut F) };
    e(); // runs the closure exactly once and consumes both the closure and the Box.
    SCHEDULER.exit()
}
