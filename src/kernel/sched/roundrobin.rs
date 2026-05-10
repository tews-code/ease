//! Preemptive multitasking with round robin scheduling

use core::alloc::Layout;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::context::{Context, switch_to};
use crate::arch::trap::TrapFrame;
use crate::arch::{STACK_CANARY, cpu_id};
use crate::board::HARTS_MAX;
use crate::kernel::collection::StackVec;
use crate::kernel::sync::{CounterU64, IrqSpinLock, with_interrupts_disabled};
use crate::kernel::timer::elapsed_ms;

const THREADS_MAX: usize = 32;
// Helper function to determine the HART boot threads in the threads array
const fn slot_for_boot(hartid: usize) -> usize {
    THREADS_MAX - HARTS_MAX + hartid
}
pub const PRIORITY_DEFAULT: u8 = u8::MAX / 2;
pub const PRIORITY_MIN: u8 = u8::MAX - 1; // Highest priority is 0

const SLICE_MS: u64 = 10; // Under contention use this time slice per thread

static SCHEDULER: Scheduler = Scheduler::new();

const _: () = assert!(
    core::mem::size_of::<TrapFrame>().is_multiple_of(core::mem::align_of::<TrapFrame>()),
    "trap frame size must be a multiple of its alignment so it lands aligned at top of stack"
);

// Stack sizes must be power-of-two and aligned to their own size
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StackClass(u8);

#[allow(dead_code)]
impl StackClass {
    pub const KB1: Self = Self(10);
    const _MIN_CLASS_SIZE_CHECK: () =
        assert!(StackClass::KB1.size() >= core::mem::size_of::<TrapFrame>());
    pub const KB2: Self = Self(11);
    pub const KB4: Self = Self(12);
    pub const KB8: Self = Self(13);
    pub const KB16: Self = Self(14);

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

#[derive(PartialEq)]
enum PostSwitch {
    Ready,
    Sleeping(u64),
}

#[derive(PartialEq)]
enum State {
    Avail,
    Ready,
    Running,
    Switching(PostSwitch),
    Sleeping(u64), // Milliseconds of sleep time
}

struct ThreadControlBlock {
    id: u32,
    state: State,
    sp: *mut u8,
    stack_class: Option<StackClass>,
    stack_owned: bool,        // If true then stack is on the heap
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
            stack_class: None,
            stack_owned: false,
            priority: PRIORITY_DEFAULT,
            pass: 0,
            last_started_cycles: 0,
        }
    }

    // Stride forward
    fn stride(&mut self) {
        // We add the priority value itself as the stride
        // Note that threads at priority 0 do not share CPU with other threads
        self.pass += self.priority as u64;
    }

    // Deallocate the stack if heap-based
    #[expect(dead_code)]
    fn release_stack(&mut self) {
        if self.stack_owned {
            if let Some(class) = self.stack_class {
                // Safety: base_ptr is derived from sp which was created using thread initialisation
                unsafe { ThreadStack::deallocate(class, self.sp) };

                self.sp = core::ptr::null_mut();
                self.stack_owned = false;
                self.stack_class = None;
            }
        } else {
            panic!("cannot release stack with class not configured");
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

    // Wakes any threads past their deadlines
    //
    // Returns a tuple of next upcoming deadline if any sleepers, else None;
    // and the number of runnable threads
    fn wake_threads(&mut self) -> (Option<u64>, usize) {
        let now = crate::kernel::timer::elapsed_ms();
        let mut earliest_deadline: Option<u64> = None;
        let mut ready: usize = 0;
        // Iterate through thread control blocks
        for tcb in &mut self.control_blocks {
            let deadline = match tcb.state {
                State::Sleeping(d) | State::Switching(PostSwitch::Sleeping(d)) => d,
                State::Ready => {
                    ready += 1;
                    continue;
                }
                _ => continue,
            };
            if deadline <= now {
                tcb.state = State::Ready;
                ready += 1;
            } else {
                earliest_deadline = Some(earliest_deadline.map_or(deadline, |d| d.min(deadline)));
            }
        }
        (earliest_deadline, ready)
    }

    // Find a free thread slot
    fn free_slot(&mut self) -> Option<&mut ThreadControlBlock> {
        self.control_blocks
            .iter_mut()
            .find(|tcb| tcb.state == State::Avail)
    }

    // Find the minimum current pass value
    fn pass_baseline(&self) -> u64 {
        self.control_blocks
            .iter()
            .filter(|tcb| tcb.state == State::Ready || tcb.state == State::Running)
            .map(|tcb| tcb.pass)
            .min()
            .unwrap_or(0)
    }

    // Get disjoint mutable TCBs for current and next
    // Returns None if current and next are the same
    fn get_current_and_next_mut(
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

    // - 2+ runnable threads → next event = min(now + SLICE_QUANTUM, earliest_sleeper).
    // - Otherwise → next event = earliest_sleeper (or u64::MAX).

    /// Set the timer to the earliest deadline or next slice
    fn set_next_timer(&mut self, sched: (Option<u64>, usize)) {
        let (earliest_deadline_ms, ready_count) = sched;
        // Two or more threads contending, compare default slice and earliest
        let deadline_ms = if let Some(next_deadline) = earliest_deadline_ms {
            let now = crate::kernel::timer::elapsed_ms();
            (SLICE_MS + now).min(next_deadline)
        } else {
            if ready_count >= 2 {
                let now = crate::kernel::timer::elapsed_ms();
                SLICE_MS + now
            } else {
                // Quiet system
                u64::MAX
            }
        };
        crate::kernel::timer::set_next_deadline_ms(deadline_ms);
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
    fn bootstrap(&self, hartid: usize) {
        let id = next_thread_id();
        if hartid == 0 {
            assert!(id == 0); // We want to make sure HART0 is bootstrapped first
        } else {
            assert!(id > 0);
        }
        let mut threads = self.threads.lock();
        threads.control_blocks[slot_for_boot(hartid)] = ThreadControlBlock {
            id,
            state: State::Running,
            priority: PRIORITY_MIN,
            stack_class: Some(StackClass::KB4),
            last_started_cycles: crate::arch::csr::rdcycles(),
            ..ThreadControlBlock::new()
        };
        threads.set_next_timer((None, 2));
        threads.this_hart_mut().running_thread = slot_for_boot(hartid)
    }

    // Set up initial thread block and stack for a new thread
    fn spawn(&self, entry: fn() -> !, priority: u8, class: StackClass) -> Option<u32> {
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
            priority,
            stack_class: Some(class),
            stack_owned: true,
            pass: baseline,
            ..ThreadControlBlock::new()
        };
        Some(tcb.id)
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
            let sched = threads.wake_threads();
            // Get current and next TCBs
            let Some((curr, curr_idx, next, next_idx)) = threads.get_current_and_next_mut() else {
                // Current and next are the same, return
                threads.set_next_timer(sched);
                drop(threads);
                // crate::arch::restore_interrupts(prev);
                return;
            };

            // Ready to switch
            // Set current thread to the new state
            curr.state = State::Switching(new_state);
            // Safety: Only writing to current within locked threads array - single writer
            let curr_cycles = crate::arch::csr::rdcycles();
            unsafe { self.run_cycles[curr_idx].add(curr_cycles - curr.last_started_cycles) };
            curr.stride();

            next.state = State::Running;
            next.last_started_cycles = curr_cycles;

            // Create local variables before dropping the lock
            let prev_sp_ptr = &raw mut curr.sp;
            let next_sp_ptr = &raw mut next.sp;

            *threads.this_hart_mut() = HartState {
                running_thread: next_idx,
                switching_thread: Some(curr_idx),
            };
            threads.set_next_timer(sched);
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
        let sched = threads.wake_threads();
        let Some((curr, curr_idx, next, next_idx)) = threads.get_current_and_next_mut() else {
            // Same thread is running uncontended, increase slice deadline
            threads.set_next_timer(sched);
            return;
        };
        // Ready to switch
        // Update book-keeping

        let curr_cycles = crate::arch::csr::rdcycles();
        unsafe { self.run_cycles[curr_idx].add(curr_cycles - curr.last_started_cycles) };
        // Perform switch
        curr.state = State::Switching(PostSwitch::Ready);
        curr.stride();
        next.state = State::Running;
        next.last_started_cycles = curr_cycles;

        // Create local variables before dropping the lock
        let prev_sp_ptr = &raw mut curr.sp;
        let next_sp_ptr = &raw mut next.sp;
        *threads.this_hart_mut() = HartState {
            running_thread: next_idx,
            switching_thread: Some(curr_idx),
        };
        threads.set_next_timer(sched);
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
    fn sleep_until(&self, deadline_ms: u64) {
        self.reschedule(PostSwitch::Sleeping(deadline_ms));
    }

    /// Blocks for `deadline` milliseconds
    fn sleep(&self, deadline_ms: u64) {
        self.sleep_until(crate::kernel::timer::elapsed_ms() + deadline_ms);
    }

    /// Get currently running thread total cpu cycles
    pub fn get_current_cycles(&self, tcb_idx: usize) -> u64 {
        self.run_cycles[tcb_idx].get()
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
pub fn spawn(entry: fn() -> !, priority: u8, class: StackClass) -> Option<u32> {
    SCHEDULER.spawn(entry, priority, class)
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
///
/// Time is measured in milliseconds
#[allow(dead_code)]
pub fn sleep_until(deadline_ms: u64) {
    SCHEDULER.sleep_until(deadline_ms);
}

/// Blocks for `deadline` millseconds
pub fn sleep(deadline_ms: u64) {
    SCHEDULER.sleep(deadline_ms);
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
    use crate::kernel::sched::StackClass;

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
            crate::kernel::sched::spawn(partner_thread, PRIORITY_DEFAULT, StackClass::KB2);
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
    /// late."
    #[test_case]
    fn sleep_blocks_for_duration() {
        ensure_partner_spawned();
        let start = crate::kernel::timer::elapsed_ms();
        crate::kernel::sched::sleep(100);
        let elapsed = crate::kernel::timer::elapsed_ms() - start;
        assert!(elapsed >= 95, "sleep too short: {} ms", elapsed);
        assert!(elapsed <= 200, "sleep too long: {} ms", elapsed);
    }

    /// Smoke test 3: sleep_until with a past deadline returns immediately.
    /// Catches the wake-check edge case — deadline <= now should fire on the
    /// first reschedule iteration, never reach the wfi loop.
    /// Tolerance is one tick (10 ms) since `ticks_ms()` has tick-granularity.
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
