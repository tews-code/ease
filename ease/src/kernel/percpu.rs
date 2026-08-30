//! Per HART struct

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;
use crate::kernel::sched::THREADS_MAX;

enum DeferredWork {
    Preempt,
    // Syscall(usize),
    // Exit(ExitReason),
}

#[repr(C)]
struct PerCpu {
    online: UnsafeCell<bool>,
    idle_thread_idx: UnsafeCell<u8>,
    current_thread_idx: UnsafeCell<u8>,
    current_kernel_stack_base: UnsafeCell<*mut u8>,
    current_kernel_stack_top: UnsafeCell<*mut u8>,
    current_user_stack_base: UnsafeCell<Option<*mut u8>>,
    resume_sp: UnsafeCell<usize>,
    switching_from_thread_idx: UnsafeCell<Option<u8>>,
    needs_reschedule: AtomicBool,
    resume_work: UnsafeCell<DeferredWork>, // On trap return the thread's deferred work
    resume_mstatus: UnsafeCell<usize>,     // Deferred work resumes with this mstatus
    resume_mepc: UnsafeCell<usize>,        // Deferred work resumes with this mepc
}

// Safety: Each HART only accesses its own per-cpu data
unsafe impl Sync for PerCpu {}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            online: UnsafeCell::new(false),
            idle_thread_idx: UnsafeCell::new(0),
            current_thread_idx: UnsafeCell::new(0),
            current_kernel_stack_base: UnsafeCell::new(core::ptr::null_mut()),
            current_kernel_stack_top: UnsafeCell::new(core::ptr::null_mut()),
            current_user_stack_base: UnsafeCell::new(None),
            resume_sp: UnsafeCell::new(0),
            switching_from_thread_idx: UnsafeCell::new(None),
            needs_reschedule: AtomicBool::new(false),
            resume_work: UnsafeCell::new(DeferredWork::Preempt), // On trap return the thread's deferred work
            resume_mstatus: UnsafeCell::new(0),
            resume_mepc: UnsafeCell::new(0),
        }
    }
}

#[unsafe(link_section = ".sram8_percpu")]
static PERCPU_HART0: PerCpu = PerCpu::new();
#[unsafe(link_section = ".sram9_percpu")]
static PERCPU_HART1: PerCpu = PerCpu::new();

/// Get the PerCpu struct for the HART of the function
fn this_hart() -> &'static PerCpu {
    match arch::hart_id() {
        0 => &PERCPU_HART0,
        1 => &PERCPU_HART1,
        _ => unreachable!("only have two HARTs"),
    }
}
/// Get the PerCpu struct for the other HART
fn that_hart() -> &'static PerCpu {
    match arch::hart_id() {
        0 => &PERCPU_HART1,
        1 => &PERCPU_HART0,
        _ => unreachable!("only have two HARTs"),
    }
}
/// Get the other hart id
pub(super) fn that_hart_id() -> usize {
    arch::hart_id() ^ 1
}

/// Get this hart's online status
#[allow(dead_code)]
pub fn online() -> bool {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().online.get() }
}

// Get the other hart's online status
pub fn other_online() -> bool {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *that_hart().online.get() }
}

/// Set the online status
pub fn set_online() {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().online.get() = true }
}

/// Get the idle thread TCB index.
pub fn idle_thread_idx() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().idle_thread_idx.get() as usize }
}

// Tracing thread behaviour requires reading cross-Hart PerCpu details
#[allow(dead_code)]
pub fn other_idle_thread_idx() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *that_hart().idle_thread_idx.get() as usize }
}

/// Set the idle thread TCB index.
pub fn set_idle_thread_idx(thread_idx: usize) {
    assert!(
        thread_idx < THREADS_MAX,
        "setting idle thread index outside of THREADS_MAX"
    );
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().idle_thread_idx.get() = thread_idx as u8 }
}

/// Get the current TCB index.
pub fn current_thread_idx() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_thread_idx.get() as usize }
}

// Tracing thread behaviour requires reading cross-Hart PerCpu details
pub fn other_current_thread_idx() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *that_hart().current_thread_idx.get() as usize }
}

/// Set the current TCB index.
pub fn set_current_thread_idx(thread_idx: usize) {
    assert!(
        thread_idx < THREADS_MAX,
        "setting current thread index outside of THREADS_MAX"
    );
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_thread_idx.get() = thread_idx as u8 }
}

/// Get the current thread kernel stack base
pub fn current_kernel_stack_base() -> *mut u8 {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_kernel_stack_base.get() }
}

/// Set the current thread kernel stack base
pub fn set_current_kernel_stack_base(stack_base: *mut u8) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_kernel_stack_base.get() = stack_base };
}

/// Get the current thread kernel stack top
pub fn current_kernel_stack_top() -> *mut u8 {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_kernel_stack_top.get() }
}

/// Set the current thread kernel stack top
pub fn set_current_kernel_stack_top(stack_top: *mut u8) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_kernel_stack_top.get() = stack_top };
}

/// Get the current thread user stack base
pub fn current_user_stack_base() -> Option<*mut u8> {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_user_stack_base.get() }
}
/// Set the current thread user stack base
pub fn set_current_user_stack_base(stack_base: Option<*mut u8>) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().current_user_stack_base.get() = stack_base };
}

/// Get the resume stack pointer
pub fn resume_sp() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().resume_sp.get() }
}

/// Set the resume stack pointer
pub fn set_resume_sp(sp: usize) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().resume_sp.get() = sp };
}

/// Get the switching thread index
#[allow(dead_code)]
pub fn switching_from_thread_idx() -> Option<usize> {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().switching_from_thread_idx.get() }.map(|i| i as usize)
}

// Tracing thread behaviour requires reading cross-Hart PerCpu details
#[cfg(feature = "trace")]
pub fn other_switching_from_thread_idx() -> Option<usize> {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *that_hart().switching_from_thread_idx.get() }.map(|i| i as usize)
}

/// Set the switching thread index
pub fn set_switching_from_thread_idx(thread_idx: Option<usize>) {
    let idx = thread_idx.map(|i| {
        assert!(
            i < THREADS_MAX,
            "Setting switching thread index outside of THREADS_MAX"
        );
        i as u8
    });
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().switching_from_thread_idx.get() = idx };
}

/// Take the switching thread index
pub fn take_switching_from_thread_idx() -> Option<usize> {
    let hart = this_hart();
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    let thread_idx: Option<u8>;
    unsafe {
        thread_idx = *hart.switching_from_thread_idx.get();
        *hart.switching_from_thread_idx.get() = None;
    }
    thread_idx.map(|i| i as usize)
}

/// Check the needs_reschedule state of this thread
pub fn needs_reschedule() -> bool {
    this_hart().needs_reschedule.load(Ordering::Acquire)
}

// Tracing thread behaviour requires reading cross-Hart PerCpu details
#[cfg(feature = "trace")]
pub fn other_needs_reschedule() -> bool {
    that_hart().needs_reschedule.load(Ordering::Acquire)
}

/// Check if this thread needs to be rescheduled
/// Sets the reschedule flag to false on read
pub fn take_needs_reschedule() -> bool {
    this_hart().needs_reschedule.swap(false, Ordering::Acquire)
}

/// Set the reschedule request flag
pub fn set_needs_reschedule() {
    this_hart().needs_reschedule.store(true, Ordering::Release);
}

/// Get the resume_mepc state of this thread
pub fn resume_mepc() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().resume_mepc.get() }
}

/// Set the resume_mepc state of this thread
pub fn set_resume_mepc(mepc: usize) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().resume_mepc.get() = mepc }
}

/// Get the resume_mstatus state of this thread
pub fn resume_mstatus() -> usize {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().resume_mstatus.get() }
}

/// Set the resume_mstatus state of this thread
pub fn set_resume_mstatus(mstatus: usize) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently, so no data race
    unsafe { *this_hart().resume_mstatus.get() = mstatus }
}
