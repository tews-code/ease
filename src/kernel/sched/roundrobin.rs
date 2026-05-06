//! Preemptive multitasking with round robin scheduling

use core::alloc::Layout;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::STACK_CANARY;
use crate::arch::context::swap_to;
use crate::arch::trap::TrapFrame;
use crate::kernel::collection::StackVec;
use crate::kernel::sync::{CounterU64, IrqSpinLock};

const THREADS_MAX: usize = 32;

// Hart boot thread slots added to the end of the threads array
const HART0_TCB_SLOT: usize = THREADS_MAX - 2;
#[expect(dead_code)]
const HART1_TCB_SLOT: usize = THREADS_MAX - 1;

pub const PRIORITY_DEFAULT: u8 = u8::MAX / 2;
pub const PRIORITY_MIN: u8 = u8::MAX - 1; // Highest priority is 0

static SCHEDULER: Scheduler = Scheduler::new();
static SWITCH_CYCLE_COUNT: CounterU64 = CounterU64::new();

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

    // Initialises a heap-based thread stack
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
        let tf_ptr = unsafe {
            stack_base
                .as_ptr()
                .add(class.size() - core::mem::size_of::<TrapFrame>()) as *mut TrapFrame
        };
        // Safety: tf_ptr is derived from stack_base, and
        // aligned because sizeof(TF) is a multiple of align(TF).
        let mstatus = crate::arch::csr::mstatus::MPIE | crate::arch::csr::mstatus::MPP;
        unsafe {
            core::ptr::write_bytes(tf_ptr, 0, 1); // writes 0 across one TrapFrame's worth of bytes
            (*tf_ptr).mepc = entry as usize;
            (*tf_ptr).mstatus = mstatus;
        }
        tf_ptr as *mut u8
    }

    // Check canary
    // Safety: sp must point inside a stack region that was set up
    // class must equal the StackClass used to set up this region
    // and has not been deallocated.
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
enum State {
    Avail,
    Ready,
    Running,
    Sleeping(u64), // Milliseconds of sleep time
}

struct ThreadControlBlock {
    id: u32,
    state: State,
    sp: *mut u8,
    stack_class: Option<StackClass>,
    stack_owned: bool, // If true then stack is on the heap
    priority: u8,
    pass: u64,
    run_cycles: CounterU64,
}

impl ThreadControlBlock {
    const fn new() -> Self {
        Self {
            id: 0,
            state: State::Avail,
            sp: core::ptr::null_mut(),
            stack_class: None,
            stack_owned: false,
            priority: 0,
            pass: 0,
            run_cycles: CounterU64::new(),
        }
    }

    // Add cycles to the accummulator
    // Safety: Caller must ensure only one writer at a time
    unsafe fn add_run_cycles(&mut self, n: u64) {
        unsafe { self.run_cycles.add(n) };
    }

    // Stride forward
    fn stride(&mut self) {
        // We add the priority value itself as the stride
        // Note that threads at priority 0 do not share CPU with other threads
        self.pass += self.priority as u64;
    }

    // Deallocate the stack if heap-based
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

struct ThreadsInner {
    control_blocks: [ThreadControlBlock; THREADS_MAX],
}

impl ThreadsInner {
    // Returns the currently running thread
    fn get_current(&self) -> &ThreadControlBlock {
        self.control_blocks
            .iter()
            .find(|tcb| tcb.state == State::Running)
            .expect("should always have exactly one running thread")
    }

    // Wakes any threads past their deadlines
    fn wake_threads(&mut self) {
        self.control_blocks.iter_mut().for_each(|tcb| {
            if let State::Sleeping(d) = tcb.state
                && d <= crate::kernel::timer::ticks_ms()
            {
                tcb.state = State::Ready;
            }
        });
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
    ) -> Option<(&mut ThreadControlBlock, &mut ThreadControlBlock)> {
        // Find current index
        let curr_idx = self
            .control_blocks
            .iter()
            .position(|tcb| tcb.state == State::Running)
            .expect("only one thread running at a time");
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
        Some((curr, next))
    }
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for ThreadsInner {}

struct Scheduler {
    threads: IrqSpinLock<ThreadsInner>,
    thread_id_counter: AtomicU32,
    last_switch_cycles: CounterU64,
}

impl Scheduler {
    // Create new array
    const fn new() -> Self {
        Self {
            threads: IrqSpinLock::new(ThreadsInner {
                control_blocks: [const { ThreadControlBlock::new() }; THREADS_MAX],
            }),
            thread_id_counter: AtomicU32::new(0),
            last_switch_cycles: CounterU64::new(),
        }
    }

    // Update the cpu cycle count for a thread
    fn credit_cycles_to(&self, tcb: &mut ThreadControlBlock) {
        let current_cycles = crate::arch::csr::rdcycles();
        // Safety: Scheduler only credits cycles to one thread at a time - single writer
        unsafe {
            let cycle_increment = current_cycles - self.last_switch_cycles.set(current_cycles);
            tcb.add_run_cycles(cycle_increment);
        }
    }

    // Set up the boot thread to become the idle thread
    fn setup_boot(&self, threads: &mut ThreadsInner) {
        threads.control_blocks[HART0_TCB_SLOT] = ThreadControlBlock {
            id: 0,
            state: State::Running,
            priority: PRIORITY_MIN,
            stack_class: Some(StackClass::KB4),
            ..ThreadControlBlock::new()
        };
    }

    // Set up the boot thread
    fn bootstrap(&self) {
        // Make sure bootstrap is only called once
        // We use up id counter zero - which we will set to the HART0_TCB_SLOT
        assert!(self.thread_id_counter.fetch_add(1, Ordering::Relaxed) == 0);

        let mut threads = self.threads.lock();
        self.setup_boot(&mut threads);
    }

    // Set up initial thread block for a new thread
    fn spawn(&self, entry: fn() -> !, priority: u8, class: StackClass) -> Option<u32> {
        // First allocate before locking
        let base = ThreadStack::allocate(class)?;
        let sp = unsafe { ThreadStack::init_for_entry(base, class, entry) };
        // Now lock the scheduler
        let mut threads = self.threads.lock();
        // Get the current pass baselines
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
            id: self.thread_id_counter.fetch_add(1, Ordering::Relaxed),
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
    fn reschedule(&self, new_state: State) {
        // We manually take and release the lock before the
        // context switch — it's held in this
        // thread's stack frame, so swap_to would otherwise carry the lock
        // across the switch and block other harts/threads from rescheduling.
        let prev = crate::arch::disable_interrupts();
        // Take the lock, all actions need to be achieved atomically
        let mut threads = self.threads.lock();
        // Check if any threads have reached or passed their deadline
        threads.wake_threads();
        // Get current and next TCBs
        let Some((curr, next)) = threads.get_current_and_next_mut() else {
            // Current and next are the same, return
            drop(threads);
            crate::arch::restore_interrupts(prev);
            return;
        };

        // Ready to switch
        // Set current thread to the new state
        curr.state = new_state;
        self.credit_cycles_to(curr);
        curr.stride();
        next.state = State::Running;

        // Create local variables before dropping the lock
        let prev_sp_ptr = &raw mut curr.sp;
        let next_sp_ptr = &raw mut next.sp;

        drop(threads);

        unsafe { swap_to(prev_sp_ptr, next_sp_ptr) };
        // After swap_to returns (on this thread's eventual resume), mret has
        // restored MIE via the saved frame's MPIE=1. Restore caller's state.
        crate::arch::restore_interrupts(prev);
    }

    /// Preempts from current thread to next thread, returning a pointer
    /// to the next thread stack frame.
    ///
    /// Note: if only one thread is runnable it is returned.
    /// Safety:
    /// - Caller must provide a valid stack pointer to the thread trap frame
    /// - Caller must use returned pointer to populate mepc
    unsafe fn preempt_into(&self, curr_sp: *mut TrapFrame) -> *mut TrapFrame {
        let mut threads = self.threads.lock();
        threads.wake_threads();
        let Some((curr, next)) = threads.get_current_and_next_mut() else {
            return curr_sp;
        };
        // Ready to switch
        // Update book-keeping
        self.credit_cycles_to(curr);
        curr.sp = curr_sp as *mut u8;
        curr.state = State::Ready;
        next.state = State::Running;
        next.sp as *mut TrapFrame
    }

    /// Yields current thread
    fn yield_now(&self) {
        self.reschedule(State::Ready);
    }

    /// Blocks until the timer has passed the deadline
    ///
    /// Time is measured in milliseconds
    fn sleep_until(&self, deadline_ms: u64) {
        self.reschedule(State::Sleeping(deadline_ms));
    }

    /// Blocks for `deadline` milliseconds
    fn sleep(&self, deadline_ms: u64) {
        self.sleep_until(crate::kernel::timer::ticks_ms() + deadline_ms);
    }

    /// Get currently running thread total cycles
    pub fn get_current_cycles(&self) -> u64 {
        let threads = self.threads.lock();
        // Find running thread - this is safe as there is always one running thread (even idle)
        let curr = threads.get_current();
        let committed = curr.run_cycles.get();
        let in_progress = crate::arch::csr::rdcycles() - SWITCH_CYCLE_COUNT.get();
        committed + in_progress
    }
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
pub fn bootstrap() {
    SCHEDULER.bootstrap();
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
/// Safety: Caller must ensure `curr_sp` is a valid pointer to a trap frame
pub unsafe fn preempt_into(curr_sp: *mut TrapFrame) -> *mut TrapFrame {
    unsafe { SCHEDULER.preempt_into(curr_sp) }
}

/// Get cycles of running thread
#[allow(dead_code)]
pub fn get_current_cycles() -> u64 {
    SCHEDULER.get_current_cycles()
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
        let start = crate::kernel::timer::ticks_ms();
        crate::kernel::sched::sleep(100);
        let elapsed = crate::kernel::timer::ticks_ms() - start;
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
        let now = crate::kernel::timer::ticks_ms();
        let deadline = now.saturating_sub(20);
        let start = crate::kernel::timer::ticks_ms();
        crate::kernel::sched::sleep_until(deadline);
        let elapsed = crate::kernel::timer::ticks_ms() - start;
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
