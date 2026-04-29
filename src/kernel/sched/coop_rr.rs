//! Cooperative multitasking with round robin scheduling

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::context::swap_to;
use crate::kernel::sync::SpinLock;

const THREADS_MAX: usize = 8;
const THREAD_STACK_SIZE: usize = 1024;

static TCBS: ThreadArray = ThreadArray::new();
static THREAD_ID: AtomicUsize = AtomicUsize::new(0);
static THREAD_STACKS: SpinLock<[ThreadStack; THREADS_MAX]> =
    SpinLock::new([ThreadStack([0u8; THREAD_STACK_SIZE]); THREADS_MAX]);

#[derive(Copy, Clone)]
#[repr(align(16))]
struct ThreadStack([u8; THREAD_STACK_SIZE]);

#[derive(Copy, Clone, PartialEq)]
enum State {
    Avail,
    Ready,
    Running,
}

#[derive(Copy, Clone)]
struct ThreadControlBlock {
    sp: *mut u8,
    state: State,
    id: usize,
}

// Safety: All access to the TCB array elements is via a spin lock
unsafe impl Send for ThreadControlBlock {}

impl ThreadControlBlock {
    pub const fn new() -> Self {
        Self {
            sp: core::ptr::null_mut(),
            state: State::Avail,
            id: 0,
        }
    }
}

struct ThreadArray(SpinLock<[ThreadControlBlock; THREADS_MAX]>);

impl ThreadArray {
    // Create new array
    const fn new() -> Self {
        Self(SpinLock::new([ThreadControlBlock::new(); THREADS_MAX]))
    }

    // Find a thread slot by id
    #[allow(dead_code)]
    fn find_slot(&self, id: usize) -> Option<usize> {
        let tcbs = self.0.lock();
        tcbs.iter().position(|tcb| tcb.id == id)
    }

    // Find a free thread slot
    fn free_slot(&self) -> Option<usize> {
        let tcbs = self.0.lock();
        tcbs.iter().position(|tcb| tcb.state == State::Avail)
    }

    // Set up the boot thread
    fn bootstrap(&self) {
        // Make sure bootstrap is only called once
        assert!(THREAD_ID.fetch_add(1, Ordering::Relaxed) == 0);
        let i = self.free_slot().unwrap(); // If there is no place for boot thread just panic
        let mut tcbs = self.0.lock();
        tcbs[i].state = State::Running;
        tcbs[i].id = 0;
    }

    // Set up initial thread block for a new thread
    fn spawn(&self, entry: fn() -> !) {
        // Find a free slot
        let i = self.free_slot().expect("must have a free thread slot");
        // Set up the stack
        let mut stacks = THREAD_STACKS.lock();
        stacks[i].0[THREAD_STACK_SIZE - 16..THREAD_STACK_SIZE - 12]
            .copy_from_slice(&usize::to_ne_bytes(entry as usize));

        // Initialise the TCB
        let mut tcbs = self.0.lock();
        tcbs[i].sp = (&raw mut stacks[i] as *mut u8).wrapping_add(THREAD_STACK_SIZE - 64);
        tcbs[i].state = State::Ready;
        tcbs[i].id = THREAD_ID.fetch_add(1, Ordering::Relaxed); // Don't spawn 4 billion threads if you don't want to wrap into zero
    }

    // Yield current thread to next thread in round robin
    fn yield_now(&self) {
        let mut tcbs = self.0.lock();
        // There can be only one running thread
        let mut tcbs_iter = tcbs
            .iter()
            .enumerate()
            .filter(|(_, tcb)| tcb.state == State::Running);
        let (curr, _) = tcbs_iter.next().expect("no running thread");
        assert!(tcbs_iter.next().is_none());
        // Find the next ready thread
        let Some(next) = (1..THREADS_MAX)
            .map(|off| (curr + off) % THREADS_MAX)
            .find(|&i| tcbs[i].state == State::Ready)
        else {
            return;
        };
        // Ready to switch
        tcbs[curr].state = State::Ready;
        tcbs[next].state = State::Running;
        // Create local variables before dropping the lock
        let prev_sp_ptr = &raw mut tcbs[curr].sp;
        // Create local variables before dropping the lock
        let next_sp_ptr = &raw mut tcbs[next].sp;
        drop(tcbs);
        unsafe { swap_to(prev_sp_ptr, next_sp_ptr) };
    }
}

/// Spawn a new thread
pub fn spawn(entry: fn() -> !) {
    TCBS.spawn(entry);
}

/// Set up the boot thread
pub fn bootstrap() {
    TCBS.bootstrap();
}

/// Voluntarily yield the current thread
pub fn yield_now() {
    TCBS.yield_now();
}
