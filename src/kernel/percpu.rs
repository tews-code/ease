//! Per HART struct

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

#[repr(C, align(8))]
#[allow(dead_code)]
struct PerCpu {
    idle_thread_idx: UnsafeCell<usize>,
    current_thread_idx: UnsafeCell<usize>,
    current_stack_base: UnsafeCell<*mut u8>,
    switching_thread_idx: UnsafeCell<Option<usize>>,
    needs_reschedule: AtomicBool,
}

// Safety: Each HART only accesses its own per-cpu data
unsafe impl Sync for PerCpu {}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            idle_thread_idx: UnsafeCell::new(usize::MAX),
            current_thread_idx: UnsafeCell::new(usize::MAX),
            current_stack_base: UnsafeCell::new(core::ptr::null_mut()),
            switching_thread_idx: UnsafeCell::new(None),
            needs_reschedule: AtomicBool::new(false),
        }
    }
}

#[unsafe(link_section = ".sram8_percpu")]
static PERCPU_HART0: PerCpu = PerCpu::new();
#[unsafe(link_section = ".sram9_percpu")]
static PERCPU_HART1: PerCpu = PerCpu::new();

fn this_cpu() -> &'static PerCpu {
    match crate::arch::cpu_id() {
        0 => &PERCPU_HART0,
        1 => &PERCPU_HART1,
        _ => unreachable!("only have two HARTs"),
    }
}

/// Get the idle thread TCB index.
pub fn idle_thread_idx() -> usize {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.idle_thread_idx.get() }
}

/// Set the idle thread TCB index.
pub fn set_idle_thread_idx(thread_idx: usize) {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.idle_thread_idx.get() = thread_idx }
}

/// Get the current TCB index.
pub fn current_thread_idx() -> usize {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.current_thread_idx.get() }
}

/// Set the current TCB index.
pub fn set_current_thread_idx(thread_idx: usize) {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.current_thread_idx.get() = thread_idx }
}

/// Get the current thread stack base
#[allow(dead_code)]
pub fn current_stack_base() -> *mut u8 {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.current_stack_base.get() }
}

/// Set the current thread stack base
pub fn set_current_stack_base(stack_base: *mut u8) {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.current_stack_base.get() = stack_base };
}

/// Get the switching thread index
#[allow(dead_code)]
pub fn switching_thread_idx() -> Option<usize> {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.switching_thread_idx.get() }
}

/// Set the switching thread index
pub fn set_switching_thread_idx(thread_idx: Option<usize>) {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *hart.switching_thread_idx.get() = thread_idx };
}

/// Take the switching thread index
pub fn take_switching_thread_idx() -> Option<usize> {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    let thread_idx: Option<usize>;
    unsafe {
        thread_idx = *hart.switching_thread_idx.get();
        *hart.switching_thread_idx.get() = None;
    }
    thread_idx
}

/// Check the needs_reschedule state of this thread
pub fn needs_reschedule() -> bool {
    let hart = this_cpu();
    hart.needs_reschedule.load(Ordering::Acquire)
}

/// Check if this thread needs to be rescheduled
/// Sets the reschedule flag to false on read
pub fn take_needs_reschedule() -> bool {
    let hart = this_cpu();
    hart.needs_reschedule.swap(false, Ordering::Acquire)
}

/// Set the reschedule request flag
pub fn set_needs_reschedule() {
    let hart = this_cpu();
    hart.needs_reschedule.store(true, Ordering::Release);
}
