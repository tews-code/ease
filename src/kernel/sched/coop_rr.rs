//! Cooperative multitasking with round robin scheduling

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::arch::context::swap_to;
use crate::arch::trap::TrapFrame;
use crate::kernel::sync::{CounterU64, IrqSpinLock};

const THREADS_MAX: usize = 8;
const IDLE_SLOT: usize = THREADS_MAX - 1;
const THREAD_STACK_SIZE: usize = 4096;
const STACK_CANARY: usize = 0xDEADBEEF;

const TF_SIZE: usize = core::mem::size_of::<TrapFrame>();

static SCHEDULER: Scheduler = Scheduler::new();
static THREAD_ID: AtomicUsize = AtomicUsize::new(0);
static SWITCH_CYCLE_COUNT: CounterU64 = CounterU64::new();

#[derive(Copy, Clone)]
#[repr(align(16))]
struct ThreadStack([u8; THREAD_STACK_SIZE]);

impl ThreadStack {
    pub const fn new() -> Self {
        Self([0u8; THREAD_STACK_SIZE])
    }
}

#[derive(Copy, Clone, PartialEq)]
enum State {
    Avail,
    Ready,
    Running,
    Sleeping(u64), // Milliseconds of sleep time
}

struct ThreadControlBlock {
    sp: *mut u8,
    state: State,
    run_cycles: CounterU64,
    id: usize,
}

impl ThreadControlBlock {
    pub const fn new() -> Self {
        Self {
            sp: core::ptr::null_mut(),
            state: State::Avail,
            run_cycles: CounterU64::new(),
            id: 0,
        }
    }
}

struct ThreadsInner {
    control_blocks: [ThreadControlBlock; THREADS_MAX],
    stacks: [ThreadStack; THREADS_MAX],
}

// Safety: All access to the TCB array elements is via a spin lock that disables interrupts
unsafe impl Send for ThreadsInner {}

struct Scheduler {
    threads: IrqSpinLock<ThreadsInner>,
}

impl Scheduler {
    // Create new array
    const fn new() -> Self {
        Self {
            threads: IrqSpinLock::new(ThreadsInner {
                control_blocks: [const { ThreadControlBlock::new() }; THREADS_MAX],
                stacks: [ThreadStack::new(); THREADS_MAX],
            }),
        }
    }

    // Find a free thread slot
    fn free_slot(&self) -> Option<usize> {
        let threads = self.threads.lock();
        threads
            .control_blocks
            .iter()
            .enumerate()
            .position(|(i, tcb)| i != IDLE_SLOT && tcb.state == State::Avail)
    }

    // Set up the boot thread
    fn bootstrap(&self) {
        // Make sure bootstrap is only called once
        assert!(THREAD_ID.fetch_add(1, Ordering::Relaxed) == 0);
        let i = self.free_slot().unwrap(); // If there is no place for boot thread just panic
        let mut threads = self.threads.lock();

        // Set up idle thread
        // Store idle_thread return address in mepc slot
        threads.stacks[IDLE_SLOT].0[THREAD_STACK_SIZE - 8..THREAD_STACK_SIZE - 4]
            .copy_from_slice(&usize::to_ne_bytes(idle_thread as *const () as usize));
        // Set up TCB
        let mstatus = crate::arch::csr::mstatus::MPIE | crate::arch::csr::mstatus::MPP;
        threads.stacks[IDLE_SLOT].0[THREAD_STACK_SIZE - 4..THREAD_STACK_SIZE]
            .copy_from_slice(&usize::to_ne_bytes(mstatus));
        threads.stacks[IDLE_SLOT].0[0..4].copy_from_slice(&usize::to_ne_bytes(STACK_CANARY));
        threads.control_blocks[IDLE_SLOT].sp = (&raw mut threads.stacks[IDLE_SLOT] as *mut u8)
            .wrapping_add(THREAD_STACK_SIZE - TF_SIZE);
        threads.control_blocks[IDLE_SLOT].id = usize::MAX;
        threads.control_blocks[IDLE_SLOT].state = State::Ready;

        // Set up boot thread - note already running
        threads.stacks[i].0[0..4].copy_from_slice(&usize::to_ne_bytes(STACK_CANARY));
        threads.control_blocks[i].state = State::Running;
        threads.control_blocks[i].id = 0;
    }

    // Set up initial thread block for a new thread
    fn spawn(&self, entry: fn() -> !) {
        // Find a free slot
        let i = self.free_slot().expect("must have a free thread slot");
        // Initialise the TCB
        let mut threads = self.threads.lock();
        // Set up the stack
        threads.stacks[i].0[THREAD_STACK_SIZE - 8..THREAD_STACK_SIZE - 4]
            .copy_from_slice(&usize::to_ne_bytes(entry as *const () as usize));
        // Set up mstatus and stack canary
        let mstatus = crate::arch::csr::mstatus::MPIE | crate::arch::csr::mstatus::MPP;
        threads.stacks[i].0[THREAD_STACK_SIZE - 4..THREAD_STACK_SIZE]
            .copy_from_slice(&usize::to_ne_bytes(mstatus));
        threads.stacks[i].0[0..4].copy_from_slice(&usize::to_ne_bytes(STACK_CANARY));
        threads.control_blocks[i].sp =
            (&raw mut threads.stacks[i] as *mut u8).wrapping_add(THREAD_STACK_SIZE - TF_SIZE);
        threads.control_blocks[i].state = State::Ready;
        threads.control_blocks[i].id = THREAD_ID.fetch_add(1, Ordering::Relaxed); // Don't spawn 4 billion threads if you don't want to wrap into zero
        // Safety: Only one writer as scheduler only schedules one thread at a time per HART and never the same thread on multiple HARTs
        unsafe { threads.control_blocks[i].run_cycles.reset() };
    }

    // Set current thread state to `new_state` and next thread to `Ready` in round robin
    fn reschedule(&self, new_state: State) {
        let prev = crate::arch::disable_interrupts();

        // Take the lock, all actions need to be achieved atomically
        let mut threads = self.threads.lock();
        // There can be only one currently running thread (even if idle_thread)
        let mut tcbs_iter = threads
            .control_blocks
            .iter()
            .enumerate()
            .filter(|(_, tcb)| tcb.state == State::Running);
        let (curr, _) = tcbs_iter.next().expect("no running thread");
        assert!(tcbs_iter.next().is_none());
        // Check stack STACK_CANARY
        assert!(
            threads.stacks[curr].0[0..4] == usize::to_ne_bytes(STACK_CANARY),
            "thread stack corrupted"
        );
        // Set current thread to the new state
        threads.control_blocks[curr].state = new_state;
        // Check if any threads have reached or passed their deadline
        threads.control_blocks.iter_mut().for_each(|tcb| {
            if let State::Sleeping(d) = tcb.state
                && d <= crate::kernel::timer::ticks_ms()
            {
                tcb.state = State::Ready;
            }
        });

        // Find the next ready thread
        let next = (1..=THREADS_MAX)
            .map(|off| (curr + off) % THREADS_MAX)
            .find(|&i| threads.control_blocks[i].state == State::Ready)
            .unwrap_or(IDLE_SLOT);

        if next == curr {
            // No other threads to run, change state back to running and leave
            threads.control_blocks[curr].state = State::Running;
            crate::arch::restore_interrupts(prev);
            return;
        }
        // Ready to switch
        let current_cycles = crate::arch::csr::rdcycles();
        // Safety: Only one thread can be running on any particular HART at a time
        // The scheduler never starts the same thread on two HARTs simultaneously
        let last_cycle_count = unsafe { SWITCH_CYCLE_COUNT.set(current_cycles) };
        let cycle_increment = current_cycles - last_cycle_count;
        unsafe { threads.control_blocks[curr].run_cycles.add(cycle_increment) };
        threads.control_blocks[next].state = State::Running;
        // Create local variables before dropping the lock
        let prev_sp_ptr = &raw mut threads.control_blocks[curr].sp;
        // Create local variables before dropping the lock
        let next_sp_ptr = &raw mut threads.control_blocks[next].sp;
        drop(threads);
        unsafe { swap_to(prev_sp_ptr, next_sp_ptr) };
        // After swap_to returns (on this thread's eventual resume), mret has
        // restored MIE via the saved frame's MPIE=1. Restore caller's state.
        crate::arch::restore_interrupts(prev);
    }

    /// Preempts from current thread to next thread, returning a pointer
    /// to the next thread stack frame.
    ///
    /// Note: if only one thread is runnable it is returned
    fn preempt_into(&self, curr_sp: *mut TrapFrame) -> *mut TrapFrame {
        let mut threads = self.threads.lock();
        let curr = threads
            .control_blocks
            .iter()
            .position(|tcb| tcb.state == State::Running)
            .expect("always have exactly one running thread");

        // Check canary
        assert!(
            threads.stacks[curr].0[0..4] == usize::to_ne_bytes(STACK_CANARY),
            "thread stack corrupted"
        );

        threads.control_blocks[curr].sp = curr_sp as *mut u8;
        threads.control_blocks[curr].state = State::Ready;

        // Wake check
        threads.control_blocks.iter_mut().for_each(|tcb| {
            if let State::Sleeping(d) = tcb.state
                && d <= crate::kernel::timer::ticks_ms()
            {
                tcb.state = State::Ready;
            }
        });

        // Find next thread
        let next = (1..=THREADS_MAX)
            .map(|off| (curr + off) % THREADS_MAX)
            .find(|&i| threads.control_blocks[i].state == State::Ready)
            .unwrap_or(IDLE_SLOT);

        // If next == curr return
        if next == curr {
            threads.control_blocks[curr].state = State::Running;
            return curr_sp;
        }

        // Ready to switch
        let current_cycles = crate::arch::csr::rdcycles();
        // Safety: Only one thread can be running on any particular HART at a time
        // The scheduler never starts the same thread on two HARTs simultaneously
        let last_cycle_count = unsafe { SWITCH_CYCLE_COUNT.set(current_cycles) };
        let cycle_increment = current_cycles - last_cycle_count;
        unsafe { threads.control_blocks[curr].run_cycles.add(cycle_increment) };

        threads.control_blocks[next].state = State::Running;
        threads.control_blocks[next].sp as *mut TrapFrame
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

    /// Blocks for `deadline` millseconds
    pub fn sleep(&self, deadline_ms: u64) {
        self.sleep_until(crate::kernel::timer::ticks_ms() + deadline_ms);
    }

    /// Get currently running thread total cycles
    pub fn get_current_cycles(&self) -> u64 {
        let threads = self.threads.lock();
        // Find running thread - this is safe as there is always one running thread (even idle)
        let curr = threads
            .control_blocks
            .iter()
            .position(|tcb| tcb.state == State::Running)
            .unwrap();
        let committed = threads.control_blocks[curr].run_cycles.get();
        let in_progress = crate::arch::csr::rdcycles() - SWITCH_CYCLE_COUNT.get();
        committed + in_progress
    }
}

fn idle_thread() -> ! {
    loop {
        crate::arch::wait_for_interrupt();
    }
}

/// Spawn a new thread
pub fn spawn(entry: fn() -> !) {
    SCHEDULER.spawn(entry);
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
pub fn sleep_until(deadline_ms: u64) {
    SCHEDULER.sleep_until(deadline_ms);
}

/// Blocks for `deadline` millseconds
pub fn sleep(deadline_ms: u64) {
    SCHEDULER.sleep(deadline_ms);
}

/// Preempts thread
pub fn preempt_into(curr_sp: *mut TrapFrame) -> *mut TrapFrame {
    SCHEDULER.preempt_into(curr_sp)
}

/// Get cycles of running thread
pub fn get_current_cycles() -> u64 {
    SCHEDULER.get_current_cycles()
}

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
            crate::kernel::sched::spawn(partner_thread);
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
