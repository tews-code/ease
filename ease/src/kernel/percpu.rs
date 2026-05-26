//! Per HART struct

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sched::THREADS_MAX;

#[repr(C, align(8))]
#[allow(dead_code)]
struct PerCpu {
    idle_thread_idx: UnsafeCell<u8>,
    current_thread_idx: UnsafeCell<u8>,
    current_stack_base: UnsafeCell<*mut u8>,
    switching_thread_idx: UnsafeCell<Option<u8>>,
    needs_reschedule: AtomicBool,
    preempt_mstatus: UnsafeCell<usize>,
    preempt_mepc: UnsafeCell<usize>,
}

// Safety: Each HART only accesses its own per-cpu data
unsafe impl Sync for PerCpu {}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            idle_thread_idx: UnsafeCell::new(0),
            current_thread_idx: UnsafeCell::new(0),
            current_stack_base: UnsafeCell::new(core::ptr::null_mut()),
            switching_thread_idx: UnsafeCell::new(None),
            needs_reschedule: AtomicBool::new(false),
            preempt_mstatus: UnsafeCell::new(0),
            preempt_mepc: UnsafeCell::new(0),
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
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().idle_thread_idx.get() as usize }
}

/// Set the idle thread TCB index.
pub fn set_idle_thread_idx(thread_idx: usize) {
    assert!(
        thread_idx < THREADS_MAX,
        "setting idle thread index outside of THREADS_MAX"
    );
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().idle_thread_idx.get() = thread_idx as u8 }
}

/// Get the current TCB index.
pub fn current_thread_idx() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().current_thread_idx.get() as usize }
}

/// Set the current TCB index.
pub fn set_current_thread_idx(thread_idx: usize) {
    assert!(
        thread_idx < THREADS_MAX,
        "setting current thread index outside of THREADS_MAX"
    );
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().current_thread_idx.get() = thread_idx as u8 }
}

/// Get the current thread stack base
#[allow(dead_code)]
pub fn current_stack_base() -> *mut u8 {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().current_stack_base.get() }
}

/// Set the current thread stack base
pub fn set_current_stack_base(stack_base: *mut u8) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().current_stack_base.get() = stack_base };
}

/// Get the switching thread index
#[allow(dead_code)]
pub fn switching_thread_idx() -> Option<usize> {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().switching_thread_idx.get() }.map(|i| i as usize)
}

/// Set the switching thread index
pub fn set_switching_thread_idx(thread_idx: Option<usize>) {
    let idx = thread_idx.map(|i| {
        assert!(
            i < THREADS_MAX,
            "Setting switching thread index outside of THREADS_MAX"
        );
        i as u8
    });
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().switching_thread_idx.get() = idx };
}

/// Take the switching thread index
pub fn take_switching_thread_idx() -> Option<usize> {
    let hart = this_cpu();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    let thread_idx: Option<u8>;
    unsafe {
        thread_idx = *hart.switching_thread_idx.get();
        *hart.switching_thread_idx.get() = None;
    }
    thread_idx.map(|i| i as usize)
}

/// Check the needs_reschedule state of this thread
pub fn needs_reschedule() -> bool {
    this_cpu().needs_reschedule.load(Ordering::Acquire)
}

/// Check if this thread needs to be rescheduled
/// Sets the reschedule flag to false on read
pub fn take_needs_reschedule() -> bool {
    this_cpu().needs_reschedule.swap(false, Ordering::Acquire)
}

/// Set the reschedule request flag
pub fn set_needs_reschedule() {
    this_cpu().needs_reschedule.store(true, Ordering::Release);
}

/// Get the preempt_mepc state of this thread
pub fn preempt_mepc() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().preempt_mepc.get() }
}

/// Set the preempt_mepc state of this thread
pub fn set_preempt_mepc(mepc: usize) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().preempt_mepc.get() = mepc }
}

/// Get the preempt_mstatus state of this thread
pub fn preempt_mstatus() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().preempt_mstatus.get() }
}

/// Set the preempt_mstatus state of this thread
pub fn set_preempt_mstatus(mstatus: usize) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_cpu().preempt_mstatus.get() = mstatus }
}
