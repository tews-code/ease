//! Preemptive multitasking with round robin scheduling

use core::alloc::Layout;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::context::{Context, switch_to};
use crate::arch::trap::TrapFrame;
use crate::arch::{STACK_CANARY, cpu_id};
use crate::board::HARTS_MAX;
use crate::kernel::sync::{CounterU64, IrqSpinLock, with_interrupts_disabled};
use crate::kernel::timer;

const THREADS_MAX: usize = 32;
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

static SCHEDULER: Scheduler = Scheduler::new();

// Stack sizes must be power-of-two and aligned to their own size
// This means that the stack base address is `size` aligned and can be found using a bitmask
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StackClass(u8);

#[allow(dead_code)]
impl StackClass {
    pub const KB1: Self = Self(10);
    pub const KB2: Self = Self(11);
    pub const KB4: Self = Self(12);
    pub const KB8: Self = Self(13);
    pub const KB16: Self = Self(14);

    const _MIN_CLASS_SIZE_CHECK: () =
        assert!(StackClass::KB1.size() >= core::mem::size_of::<TrapFrame>());

    const fn size(self) -> usize {
        1usize << self.0
    }

    const fn mask(self) -> usize {
        self.size() - 1
    }

    const fn align(self) -> usize {
        self.size()
    }

    const fn layout(self) -> Layout {
        match Layout::from_size_align(self.size(), self.align()) {
            Ok(layout) => layout,
            Err(_) => panic!("invalid layout"),
        }
    }
}

struct ThreadStack;

impl ThreadStack {
    // Allocates a thread stack from the heap and returns a pointer
    // to the stack base
    // Returns None if allocation fails
    pub fn allocate(class: StackClass) -> Option<NonNull<u8>> {
        NonNull::new(unsafe { alloc::alloc::alloc(class.layout()) })
    }

    // Forges a heap-based thread Context
    // The thread entry function is stored in s0
    // Returns the stack pointer
    // Safety: stack_base must be class.size()-aligned and point to
    // writeable memory of at least class.size() bytes
    pub unsafe fn init_for_entry(
        stack_base: NonNull<u8>,
        class: StackClass,
        entry: fn() -> !,
    ) -> *mut u8 {
        // Safety: Caller has provided a valid stack base pointer
        unsafe {
            core::ptr::write(stack_base.as_ptr() as *mut usize, STACK_CANARY);
        }
        let context_ptr = unsafe {
            stack_base
                .as_ptr()
                .add(class.size() - core::mem::size_of::<Context>()) as *mut Context
        };
        // Safety: context_ptr is derived from stack_base, and
        // aligned because sizeof(Context) is a multiple of align(Context).
        unsafe {
            core::ptr::write_bytes(context_ptr, 0, 1); // writes 0 across one Context's worth of bytes
            *context_ptr = Context::for_entry(entry);
        }
        context_ptr as *mut u8
    }

    // Check canary
    // Safety: sp must point inside a stack region that was set up
    // class must equal the StackClass used to set up this region
    // and has not been deallocated.
    #[expect(dead_code)]
    unsafe fn canary_ok(class: StackClass, sp: *mut u8) -> bool {
        unsafe { *(sp.with_addr(sp.addr() & !class.mask()) as *const usize) == STACK_CANARY }
    }

    // Deallocate the heap-backed thread stack.
    // Safety: sp must point inside a stack region that was set up
    // by init_for_entry and has not been deallocated.
    // The caller transfers ownership of the sp; further use is UB.
    unsafe fn deallocate(class: StackClass, sp: *mut u8) {
        let base = sp.with_addr(sp.addr() & !class.mask());
        unsafe { alloc::alloc::dealloc(base, class.layout()) };
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Deadline {
    min: u64,
    fixed_leeway: Option<u64>, // None - use system default leeway
}

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

#[derive(PartialEq, Debug)]
enum PostSwitch {
    Ready,
    Sleeping(Deadline),
}

#[derive(PartialEq, Debug)]
enum State {
    Avail,
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(Deadline),
}

enum Stack {
    Heap(StackClass), // Needs dealloc on thread exit
    Fixed,            // Set by hardware
}

pub enum Qos {
    High,
    Low,
}

struct ThreadControlBlock {
    id: u32,
    state: State,
    sp: *mut u8,
    stack: Option<Stack>,
    qos: Qos,
    priority: u8,             // Lower number is higher priority
    pass: u64,                // The next ready thread with lowest pass wins
    last_started_cycles: u64, // Cycle stamp from last switch
}

impl ThreadControlBlock {
    const fn new() -> Self {
        Self {
            id: 0,
            state: State::Avail,
            sp: core::ptr::null_mut(),
            stack: None,
            qos: Qos::Low,
            priority: PRIORITY_DEFAULT,
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
    #[expect(dead_code)]
    fn release_stack(&mut self) {
        let stack = self
            .stack
            .take()
            .expect("should not be calling release_stack on uninitialised TCB");
        if let Stack::Heap(class) = stack {
            unsafe {
                ThreadStack::deallocate(class, self.sp);
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

struct HartState {
    running_thread: usize,
    switching_thread: Option<usize>,
}

struct ThreadsInner {
    control_blocks: [ThreadControlBlock; THREADS_MAX],
    hart_state: [HartState; HARTS_MAX],
}

impl ThreadsInner {
    // Returns the hart_state for this thread
    fn this_hart(&self) -> &HartState {
        &self.hart_state[cpu_id()]
    }

    fn this_hart_mut(&mut self) -> &mut HartState {
        &mut self.hart_state[cpu_id()]
    }

    // Find a free thread slot
    fn free_slot(&mut self) -> Option<&mut ThreadControlBlock> {
        self.control_blocks
            .iter_mut()
            .find(|tcb| tcb.state == State::Avail)
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
    // Returns None if current and next are the same
    fn pick_next_ready_mut(
        &mut self,
    ) -> Option<(
        &mut ThreadControlBlock,
        usize,
        &mut ThreadControlBlock,
        usize,
    )> {
        // Find current index
        let curr_idx = self.this_hart().running_thread;
        // Find next index
        let next_idx = self
            .control_blocks
            .iter()
            .enumerate()
            .filter(|(_, tcb)| tcb.state == State::Ready)
            .min_by_key(|(_, tcb)| tcb.pass)
            .map(|(i, _)| i)?;
        if curr_idx == next_idx {
            return None;
        };
        // Get disjoint TCBs
        let [curr, next] = self
            .control_blocks
            .get_disjoint_mut([curr_idx, next_idx])
            .expect("indices have been selected as disjoint");
        Some((curr, curr_idx, next, next_idx))
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
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for ThreadsInner {}

struct Scheduler {
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
    //
    // Hart 0's bootstrap runs the kernel main loop / shell / tests, so it's
    // at PRIORITY_DEFAULT. Hart 1's bootstrap (and any further harts) run
    // `idle_thread` and so are at PRIORITY_MIN. With time-weighted stride,
    // pass advances proportional to `priority`, so having hart 0's bootstrap
    // at PRIORITY_MIN caused its pass to climb 2× faster than partner
    // threads, eventually starving the test runner.
    fn bootstrap(&self, hartid: usize) {
        let id = next_thread_id();
        if hartid == 0 {
            assert!(id == 0); // We want to make sure HART0 is bootstrapped first
        } else {
            assert!(id > 0);
        }
        let priority = if hartid == 0 {
            PRIORITY_DEFAULT
        } else {
            PRIORITY_MIN
        };
        let mut threads = self.threads.lock();
        threads.control_blocks[slot_for_boot(hartid)] = ThreadControlBlock {
            id,
            state: State::Running,
            priority,
            stack: Some(Stack::Fixed),
            last_started_cycles: timer::elapsed(),
            ..ThreadControlBlock::new()
        };
        threads.this_hart_mut().running_thread = slot_for_boot(hartid)
    }

    /// Helper function to clean up post switch threads
    fn post_switch_cleanup(&self) {
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
                _ => panic!("Post switch but not in Switched state"),
            }
        };
        threads.control_blocks[switched_idx].state = new_state;
    }

    // Set up initial thread block and stack for a new thread
    fn spawn(&self, entry: fn() -> !, priority: u8, class: StackClass, qos: Qos) -> Option<u32> {
        // First allocate before locking
        let base = ThreadStack::allocate(class)?;
        let sp = unsafe { ThreadStack::init_for_entry(base, class, entry) };
        // Now lock the scheduler
        let mut threads = self.threads.lock();
        // Get the current pass baselines so we don't schedule ahead of other threads
        let baseline = threads.pass_baseline();
        // Find a free TCB slot
        let Some(tcb) = threads.free_slot() else {
            drop(threads);
            unsafe {
                ThreadStack::deallocate(class, sp);
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
        let id = tcb.id;
        // Set the timer
        let ready_count = threads.wake_sleeping_threads();
        let earliest_deadline = threads.earliest_deadline();
        threads.set_next_timer(earliest_deadline, ready_count);
        Some(id)
    }

    // Set current thread state to `new_state` and next thread to `Ready` in round robin
    fn reschedule(&self, new_state: PostSwitch) {
        // Note - the closure body will contain switch_to, which is unusual
        // It's a non-local control transfer wearing the disguise of a function call.
        // It works correctly because the closure frame is preserved on the suspended thread's stack
        with_interrupts_disabled(|_cs| {
            // We manually take and release the lock before the
            // context switch — it's held in this
            // thread's stack frame, so switch_to would otherwise carry the lock
            // across the switch and block other harts/threads from rescheduling.
            let mut threads = self.threads.lock();
            // Check if any threads have reached or passed their deadline
            let ready_count = threads.wake_sleeping_threads();
            let earliest_deadline = threads.earliest_deadline();
            // Get current and next TCBs
            let Some((curr, curr_idx, next, next_idx)) = threads.pick_next_ready_mut() else {
                // Current and next are the same, return
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
    fn preempt(&self) {
        let mut threads = self.threads.lock();
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
    fn yield_now(&self) {
        self.reschedule(PostSwitch::Ready);
    }

    /// Blocks until the timer has passed the deadline
    ///
    /// Time is measured in milliseconds
    fn sleep_until(&self, deadline_ms: u64, fixed_leeway_ms: Option<u64>) {
        let deadline = Deadline {
            min: deadline_ms.saturating_mul(timer::CYCLES_PER_MS),
            fixed_leeway: fixed_leeway_ms.map(|l| l.saturating_mul(timer::CYCLES_PER_MS)),
        };
        self.reschedule(PostSwitch::Sleeping(deadline));
    }

    /// Blocks for `deadline` milliseconds
    #[allow(dead_code)]
    fn sleep(&self, deadline_ms: u64) {
        self.sleep_until(
            crate::kernel::timer::elapsed_ms().saturating_add(deadline_ms),
            None,
        );
    }

    /// Blocks for `deadline` milliseconds
    #[allow(dead_code)]
    fn sleep_with_leeway(&self, deadline_ms: u64, leeway_ms: u64) {
        let now_ms = crate::kernel::timer::elapsed_ms();
        self.sleep_until(now_ms.saturating_add(deadline_ms), Some(leeway_ms));
    }

    /// Get currently running thread total cpu cycles
    pub fn get_current_cycles(&self, tcb_idx: usize) -> u64 {
        self.run_cycles[tcb_idx].get()
    }
}

/// Generate a new thread id
fn next_thread_id() -> u32 {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub fn idle_thread() -> ! {
    loop {
        crate::arch::wait_for_interrupt();
    }
}

/// Spawn a new thread
pub fn spawn(entry: fn() -> !, priority: u8, class: StackClass, qos: Qos) -> Option<u32> {
    SCHEDULER.spawn(entry, priority, class, qos)
}

/// Set up the boot thread
pub fn bootstrap(hartid: usize) {
    SCHEDULER.bootstrap(hartid);
}

/// Voluntarily yield the current thread
pub fn yield_now() {
    SCHEDULER.yield_now();
}

/// Blocks until the timer has passed the deadline
/// Time is measured in milliseconds
#[allow(dead_code)]
pub fn sleep_until(deadline_ms: u64) {
    SCHEDULER.sleep_until(deadline_ms, None);
}

/// Blocks for `deadline` millseconds
#[allow(dead_code)]
pub fn sleep(deadline_ms: u64) {
    SCHEDULER.sleep(deadline_ms);
}

/// Blocks for `deadline` millseconds
#[allow(dead_code)]
pub fn sleep_with_leeway(deadline_ms: u64, fixed_leeway_ms: u64) {
    SCHEDULER.sleep_with_leeway(deadline_ms, fixed_leeway_ms);
}

/// Preempts thread
pub fn preempt() {
    SCHEDULER.preempt();
}

/// Get cycles of running thread
#[allow(dead_code)]
pub fn get_current_cycles(tcb_idx: usize) -> u64 {
    SCHEDULER.get_current_cycles(tcb_idx)
}

/// Clean up switched status threads
pub fn post_switch_cleanup() {
    SCHEDULER.post_switch_cleanup();
}

// #[allow(dead_code)]
// Check the stack canary is in place using lock-free check
// Lock free for use by panic
// pub fn stack_ok_panic() -> bool {
//     let sp = crate::arch::csr::regs::sp() as *const u8;
//
//     let stack_base = sp.with_addr(sp.addr() & !(STACK_SIZE - 1));
//     unsafe { core::ptr::read_volatile(stack_base as *const usize) == STACK_CANARY }
// }

// =============================================================================
// Smoke tests + cycle-count benchmark
// =============================================================================
//
// Tests pin down observable behaviour rather than implementation details, so
// they should survive future rung changes (preemption, priorities, etc.).
//
// The cycle-count benchmark prints a single line; no pass/fail. Useful as a
// number to eyeball across rungs. Note that with the test-sched background
// threads (thread1/thread2 spawned in kernel_init), the measured round-trip
// includes some time in those threads — it's "yield round-trip in this
// system" rather than bare context-switch cost. Still informative.

#[cfg(all(test, feature = "test-sched"))]
mod tests {
    use crate::kernel::sched::{Qos, StackClass};

    use super::PRIORITY_DEFAULT;
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Counter the partner thread bumps each iteration. We only assert it
    /// INCREASES during a test, so the absolute value across tests is fine.
    static PARTNER_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// Set to 1 once the partner has been spawned; ensure-once across tests.
    static PARTNER_SPAWNED: AtomicUsize = AtomicUsize::new(0);

    fn partner_thread() -> ! {
        loop {
            PARTNER_COUNT.fetch_add(1, Ordering::Relaxed);
            crate::kernel::sched::yield_now();
        }
    }

    fn ensure_partner_spawned() {
        if PARTNER_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
            crate::kernel::sched::spawn(
                partner_thread,
                PRIORITY_DEFAULT,
                StackClass::KB2,
                Qos::High,
            );
        }
    }

    /// Smoke test 1: yield_now lets at least one other thread make progress.
    /// Catches "scheduler never switches" or "switch corrupts state."
    #[test_case]
    fn yield_makes_progress() {
        ensure_partner_spawned();
        let before = PARTNER_COUNT.load(Ordering::Relaxed);
        for _ in 0..10 {
            crate::kernel::sched::yield_now();
        }
        let after = PARTNER_COUNT.load(Ordering::Relaxed);
        assert!(
            after > before,
            "partner thread didn't run during yields (before={}, after={})",
            before,
            after
        );
    }

    /// Smoke test 2: sched::sleep blocks for approximately the requested
    /// duration. Catches "sleep doesn't actually block" and "sleep returns
    /// late." Bound is tight (~one slice quantum of slack) since the
    /// scheduler is tickless with sub-quantum sleep precision.
    #[test_case]
    fn sleep_blocks_for_duration() {
        ensure_partner_spawned();
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep(100);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        assert!(elapsed >= 95, "sleep too short: {} ms", elapsed);
        assert!(elapsed <= 118, "sleep too long: {} ms", elapsed);
    }

    /// Step 3 verification: a sub-slice-quantum sleep wakes at its actual
    /// deadline, not the next slice boundary. Catches a regression where
    /// reschedule fails to recompute the earliest sleeper deadline after
    /// registering the current thread's new sleep state — without that
    /// recompute, sleep(5) would round up to a slice boundary (~10ms+).
    /// The upper bound has slack for UART drain noise from the test runner's
    /// per-test name print (~6ms at 115200 baud) plus general overhead.
    #[test_case]
    fn sleep_below_quantum_wakes_at_deadline() {
        use core::fmt::Write;
        ensure_partner_spawned();
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep_with_leeway(5, 0);
        let post_sleep = crate::kernel::timer::elapsed_ms();
        let elapsed = post_sleep - start;
        assert!(elapsed >= 5, "sleep too short: {} ms", elapsed);
        assert!(
            elapsed <= 15,
            "sub-quantum sleep rounded to slice boundary: {} ms",
            elapsed
        );
    }

    /// T1: a tight-deadline sleeper should wake within its own window even
    /// when another sleeper with a much wider leeway window is also pending.
    /// Guards the `earliest_deadline` contract: the tightest *upper bound* `b`
    /// wins, not the smallest `min`. A pre-fix `earliest_deadline` that sorted
    /// lexicographically on `(min, b)` could pick a long-leeway neighbor and
    /// pin the timer to that neighbor's far-future `b`. In practice the
    /// coalescing/yield path tends to self-correct within microseconds, so
    /// this test is a contract guard for future regressions rather than a
    /// direct demonstration of an observable bug. Pre- and post-fix both
    /// pass under normal scheduling.
    #[test_case]
    fn tight_deadline_wakes_with_long_leeway_neighbor() {
        static BG_SPAWNED: AtomicUsize = AtomicUsize::new(0);
        fn long_leeway_sleeper() -> ! {
            // Short min, huge leeway: window [now+5ms, now+1005ms].
            crate::kernel::sched::sleep_with_leeway(5, 1000);
            // Park in deep sleep — yield-loop would create a permanent
            // low-pass thread that disturbs scheduling in later tests.
            loop {
                crate::kernel::sched::sleep_until(u64::MAX);
            }
        }
        ensure_partner_spawned();
        if BG_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
            crate::kernel::sched::spawn(
                long_leeway_sleeper,
                PRIORITY_DEFAULT,
                StackClass::KB2,
                Qos::Low,
            );
        }
        // Give the background sleeper a moment to reach its sleep_with_leeway.
        crate::kernel::sched::sleep(2);
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep_with_leeway(20, 0);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        // Bound is generous: this test is a contract guard for future
        // regressions in the earliest_deadline / coalescing path, not a tight
        // jitter measurement. A real regression (e.g. timer pinned to the
        // neighbor's far-future `b`) would blow past 100ms+.
        assert!(
            elapsed <= 50,
            "tight sleeper dragged by long-leeway neighbor: {} ms",
            elapsed
        );
    }

    /// T2: a neighbor sleeper with extreme `fixed_leeway` (passed as
    /// `u64::MAX` at the public API) must not corrupt the wake math for
    /// other sleepers. Without saturating arithmetic, `leeway_ms *
    /// CYCLES_PER_MS` and `d.min + leeway` both wrap, potentially producing
    /// a small bogus `wakeup_deadline` that `earliest_deadline` picks as
    /// the global min, dragging tight sleepers' wake times forward. With
    /// `saturating_mul` and `saturating_add` everywhere, the neighbor's
    /// effective deadline pins to `u64::MAX` and falls out of the `.min()`,
    /// leaving the tight sleeper undisturbed.
    #[test_case]
    fn huge_leeway_neighbor_does_not_corrupt_wake_math() {
        static SPAWNED: AtomicUsize = AtomicUsize::new(0);
        fn huge_leeway_sleeper() -> ! {
            // u64::MAX in both args — exercises every saturating site on the
            // path from public API to the Deadline struct.
            crate::kernel::sched::sleep_with_leeway(5, u64::MAX);
            loop {
                crate::kernel::sched::yield_now();
            }
        }
        ensure_partner_spawned();
        if SPAWNED.swap(1, Ordering::Relaxed) == 0 {
            crate::kernel::sched::spawn(
                huge_leeway_sleeper,
                PRIORITY_DEFAULT,
                StackClass::KB2,
                Qos::Low,
            );
        }
        // Give the background sleeper a moment to reach its sleep call.
        crate::kernel::sched::sleep(2);
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep_with_leeway(20, 0);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        // Lower bound catches "bogus wrapped wakeup pulled main forward"
        // (the original arithmetic-overflow failure mode).
        assert!(
            elapsed >= 5,
            "tight sleeper woke too early — likely wrapped neighbor deadline: {} ms",
            elapsed
        );
        // Upper bound catches "neighbor dragged main past slice boundary".
        assert!(
            elapsed <= 50,
            "tight sleeper delayed by huge-leeway neighbor: {} ms",
            elapsed
        );
    }

    /// T3: a sleep-spammer (busy + short sleep loop) must not get
    /// disproportionately more CPU than a pure CPU-bound thread of equal
    /// priority. Pre-fix (per-turn stride + unconditional switch-on-wake
    /// in `preempt`), the spammer triggered a switch on every wake and
    /// took ~67% of CPU at 2:1 over the hog. With time-weighted stride +
    /// pass-aware preempt + upfront stride application, the spammer's
    /// pass advances proportional to actual CPU consumed, so the attack
    /// no longer pays off.
    ///
    /// Iter counters are loop-iteration counts. Both threads run the same
    /// per-iter work (one atomic fetch_add), so the iter ratio equals the
    /// CPU-time ratio.
    ///
    /// What this test does NOT guard: the dual problem of the spammer
    /// being under-served because slice granularity is too coarse to let
    /// sub-slice runs catch up to a long-running hog. Empirically the
    /// post-fix ratio is ~20:1 hog:spammer, which would require sub-slice
    /// preemption to improve. That's a separate fairness-precision
    /// concern, not the wake-spam attack this test exists to guard.
    #[test_case]
    fn fair_stride_resists_wake_spammer() {
        static T3_SPAMMER_ITERS: AtomicUsize = AtomicUsize::new(0);
        static T3_HOG_ITERS: AtomicUsize = AtomicUsize::new(0);
        static T3_SPAWNED: AtomicUsize = AtomicUsize::new(0);
        // Active while the test is measuring. Cleared at the end so the
        // contenders park (yield-loop) instead of hogging CPU during
        // subsequent tests.
        static T3_ACTIVE: AtomicUsize = AtomicUsize::new(1);

        fn t3_spammer() -> ! {
            while T3_ACTIVE.load(Ordering::Relaxed) != 0 {
                for _ in 0..10_000 {
                    T3_SPAMMER_ITERS.fetch_add(1, Ordering::Relaxed);
                }
                crate::kernel::sched::sleep(1);
            }
            loop {
                crate::kernel::sched::sleep_until(u64::MAX);
            }
        }

        fn t3_hog() -> ! {
            while T3_ACTIVE.load(Ordering::Relaxed) != 0 {
                for _ in 0..10_000 {
                    T3_HOG_ITERS.fetch_add(1, Ordering::Relaxed);
                }
            }
            loop {
                crate::kernel::sched::yield_now();
            }
        }

        if T3_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
            crate::kernel::sched::spawn(t3_spammer, PRIORITY_DEFAULT, StackClass::KB2, Qos::Low);
            crate::kernel::sched::spawn(t3_hog, PRIORITY_DEFAULT, StackClass::KB2, Qos::Low);
        }
        // Warmup so the contenders stabilise before we sample.
        crate::kernel::sched::sleep(50);
        let s_start = T3_SPAMMER_ITERS.load(Ordering::Relaxed);
        let h_start = T3_HOG_ITERS.load(Ordering::Relaxed);
        crate::kernel::sched::sleep(500);
        let s_delta = T3_SPAMMER_ITERS.load(Ordering::Relaxed) - s_start;
        let h_delta = T3_HOG_ITERS.load(Ordering::Relaxed) - h_start;

        // Park the contenders before asserting, so a panic on assertion
        // failure still leaves the test threads parked.
        T3_ACTIVE.store(0, Ordering::Relaxed);

        assert!(s_delta > 0, "spammer made no progress");
        assert!(h_delta > 0, "hog made no progress");

        // Original wake-spam bug produced ~2:1 spammer:hog. The 3:2 bound
        // catches it with margin while tolerating per-run scheduler jitter.
        assert!(
            s_delta * 2 <= h_delta * 3,
            "spammer dominated (wake-spam regression?): spammer={}, hog={}",
            s_delta,
            h_delta
        );
    }

    /// T4: a Qos::High sleeper wakes near its deadline even when a
    /// Qos::Low neighbor has a much wider leeway window. Verifies that
    /// the QoS-derived leeway formula in `Deadline::leeway` differentiates
    /// the two classes, and that `earliest_deadline` picks the tight
    /// upper bound so the wider Low neighbor doesn't drag the timer.
    ///
    /// Setup: Low sleeper does sleep(200), getting ~25 ms QoS-derived
    /// leeway (window [200, 225] ms). High measurer does sleep(20),
    /// getting near-zero leeway (window [20, ~20] ms). The Low sleeper's
    /// window starts past the measurer's deadline, so coalescing leaves
    /// the measurer alone.
    #[test_case]
    fn qos_high_wakes_precisely_with_low_neighbor() {
        static BG_SPAWNED: AtomicUsize = AtomicUsize::new(0);
        static MEASURER_ELAPSED: AtomicUsize = AtomicUsize::new(0);
        static MEASURER_DONE: AtomicUsize = AtomicUsize::new(0);

        fn t4_low_neighbor() -> ! {
            // Qos::Low + long sleep gives a wide leeway window.
            crate::kernel::sched::sleep(200);
            // Park in deep sleep — yield-loop would create a permanent
            // low-pass thread that disturbs scheduling in later tests.
            loop {
                crate::kernel::sched::sleep_until(u64::MAX);
            }
        }

        fn t4_high_measurer() -> ! {
            let start = crate::kernel::timer::elapsed_ms();
            crate::kernel::sched::sleep(20);
            let elapsed = crate::kernel::timer::elapsed_ms() - start;
            MEASURER_ELAPSED.store(elapsed as usize, Ordering::Relaxed);
            MEASURER_DONE.store(1, Ordering::Relaxed);
            loop {
                crate::kernel::sched::sleep_until(u64::MAX);
            }
        }

        ensure_partner_spawned();
        if BG_SPAWNED.swap(1, Ordering::Relaxed) == 0 {
            crate::kernel::sched::spawn(
                t4_low_neighbor,
                PRIORITY_DEFAULT,
                StackClass::KB2,
                Qos::Low,
            );
        }
        // Brief settle so the Low neighbor reaches its sleep before we
        // spawn the measurer; otherwise it's just main vs measurer.
        crate::kernel::sched::sleep(2);

        crate::kernel::sched::spawn(
            t4_high_measurer,
            PRIORITY_DEFAULT,
            StackClass::KB2,
            Qos::High,
        );

        // Wait for the measurer to finish its 20 ms sleep and record.
        crate::kernel::sched::sleep(60);
        assert_eq!(
            MEASURER_DONE.load(Ordering::Relaxed),
            1,
            "Qos::High measurer didn't finish in time"
        );
        let elapsed = MEASURER_ELAPSED.load(Ordering::Relaxed);
        assert!(elapsed >= 20, "Qos::High sleep too short: {} ms", elapsed);
        // Qos::High's formula yields essentially zero leeway for short
        // sleeps. Tolerance covers scheduling jitter and the discrete
        // `elapsed_ms` granularity, not aggressive leeway.
        assert!(
            elapsed <= 40,
            "Qos::High wake delayed (likely pulled by Low neighbor): {} ms",
            elapsed
        );
    }

    /// Smoke test 3: sleep_until with a past deadline returns immediately.
    /// Catches the wake-check edge case — deadline <= now should fire on the
    /// first reschedule iteration, never reach the wfi loop.
    /// Tolerance is one slice quantum (~8 ms) — `elapsed_ms` is ms-granular
    /// and the wake check happens during the next reschedule.
    #[test_case]
    fn sleep_until_past_returns_quickly() {
        ensure_partner_spawned();
        let now = crate::kernel::timer::elapsed_ms();
        let deadline = now.saturating_sub(20);
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep_until(deadline);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        assert!(
            elapsed <= 15,
            "past deadline should return quickly, took {} ms",
            elapsed
        );
    }

    /// Benchmark: average yield_now round-trip cycles. No assertion.
    /// Reports cpu (this thread only) and wall (includes partner thread).
    #[cfg(feature = "test-bench")]
    #[test_case]
    fn sched_benchmarks() {
        use crate::bench;
        use crate::println;

        println!();
        println!("====== SCHEDULER ====== ");
        println!();

        ensure_partner_spawned();
        const N: u32 = 1000;
        let c = bench::measure(|| {
            for _ in 0..N {
                crate::kernel::sched::yield_now();
            }
        });
        println!(
            "  yield_now round-trip: cpu={} wall={} cycles/call ({} calls)",
            c.cpu / N as u64,
            c.wall / N as u64,
            N
        );

        println!();
        println!("===================== ");
        println!();
    }
}
