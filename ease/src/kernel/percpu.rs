//! Per HART struct
//!
//! Most fields are written and read by the same HART, avoiding concurrent
//! reads and writes which would be undefined behaviour.
//!
//! Some "other" accessors can only be safely read or written to under the scheduler
//! locks and hence  take `&SchedInner` as a token parameter, or are atomics.

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;
use crate::kernel::sched::{Qos, SchedInner, thread};
use crate::kernel::trap::Work;

/// Currently running thread details.
/// These fields must always be consistent with each other
// Derive Copy as when getting the fields we are taking a snapshot at the time
// and need to copy those fields out of the UnsafeCell.
#[derive(Clone, Copy)]
struct CurrentThread {
    handle: thread::Handle,
    kernel_stack_base: NonNull<u8>, // Required for lock-free canary check
    kernel_stack_top: NonNull<u8>,  // Required for lock-free mret divert to kernel mode
    user_stack_base: Option<NonNull<u8>>, // Required for lock-free canary check
    qos: Qos,                       // Required for lock-free deadline calculations
}
/// Deferred work resumes with this context
// No `Copy` as Resume should only be taken on resumption and not copied or reused
pub(crate) struct ResumeContext {
    pub(crate) work: Work,
    pub(crate) sp: usize,
    pub(crate) mstatus: usize,
    pub(crate) mepc: usize,
}
/// The per-HART struct
struct PerCpu {
    ipi_online: AtomicBool,
    current_thread: UnsafeCell<Option<CurrentThread>>,
    idle_thread: UnsafeCell<Option<thread::Handle>>,
    switching_from_thread: UnsafeCell<Option<thread::Handle>>,
    needs_reschedule: AtomicBool,
    resume_context: UnsafeCell<Option<ResumeContext>>,
}
// Safety: Each HART only accesses its own per-cpu data, with the exception of the
// "other" accessors, which can only be called with the scheduler lock held. As a
// result, the fields are either Sync as they are atomic, trivally never used by other
// threads, or because of the scheduler spinlock serialisation.
unsafe impl Sync for PerCpu {}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            ipi_online: AtomicBool::new(false),
            current_thread: UnsafeCell::new(None),
            idle_thread: UnsafeCell::new(None),
            switching_from_thread: UnsafeCell::new(None),
            needs_reschedule: AtomicBool::new(false),
            resume_context: UnsafeCell::new(None),
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
/// Get this hart's IPI availability status
pub(crate) fn ipi_online() -> bool {
    // We don't order memory against this atomic, it is purely a flag.
    // (The IPI mailbox handles ordering).
    this_hart().ipi_online.load(Ordering::Relaxed)
}
/// Get the other hart's IPI online status
pub(crate) fn other_ipi_online() -> bool {
    // We don't order memory against this atomic, it is purely a flag.
    // (The IPI mailbox handles ordering).
    that_hart().ipi_online.load(Ordering::Relaxed)
}
/// Set this HART's IPI online status
pub(crate) fn set_ipi_online() {
    // We don't order memory against this atomic, it is purely a flag.
    // (The IPI mailbox handles ordering).
    this_hart().ipi_online.store(true, Ordering::Relaxed)
}
/// Get the idle thread handle.
/// Returns `None` if called before this has been installed by the scheduler.
pub(crate) fn idle_thread() -> Option<thread::Handle> {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().idle_thread.get() }
}
/// The handle of the idle thread on the other HART. Returns `None` if this is not
/// yet installed. Tracing thread behaviour requires reading cross-Hart PerCpu details.
pub(crate) fn other_idle_thread(_sched: &SchedInner) -> Option<thread::Handle> {
    // Safety: the scheduler lock ensures no concurrent writes, so deferencing is safe
    unsafe { *that_hart().idle_thread.get() }
}
/// Set the idle thread handle.
/// Requres proof of holding a scheduler lock to prevent concurrent writes with reads.
/// Note also that the lock around SchedInner provides memory ordering.
pub(crate) fn set_idle_thread(_sched: &SchedInner, handle: thread::Handle) {
    // Safety: can only be called with the scheduler lock held so
    // no other hart reads it concurrently with this write, so no data race
    unsafe { *this_hart().idle_thread.get() = Some(handle) }
}
/// Get the current thread handle, or `None` before this hart's scheduler
/// bootstrap has installed one.
pub(crate) fn try_current_thread() -> Option<thread::Handle> {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().current_thread.get() }.map(|c| c.handle)
}
/// Get the current thread handle.
///
/// # Panics
/// Panics if the current thread has not yet been installed. Use [try_current_thread]
/// to access if uncertain whether the thread has been installed or not.
#[track_caller]
pub(crate) fn current_thread() -> thread::Handle {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().current_thread.get() }
        .map(|c| c.handle)
        .expect("current thread should be installed")
}
/// Returns the other HART's currently running thread handle if the scheduler has installed
/// a current thread for that HART, otherwise returns `None`.
///
/// Must only be called with the scheduler lock held to ensure no concurrent writes with the read
pub(crate) fn try_other_current_thread(_sched: &SchedInner) -> Option<thread::Handle> {
    // Safety: the scheduler lock ensures there are no concurrent writes
    unsafe { *that_hart().current_thread.get() }.map(|c| c.handle)
}
/// Set the current thread details.
///
/// Takes a SchedInner token to ensure scheduler lock is held.
pub(crate) fn set_current_thread(
    _sched: &SchedInner,
    handle: thread::Handle,
    kernel_stack_base: NonNull<u8>,
    kernel_stack_top: NonNull<u8>,
    user_stack_base: Option<NonNull<u8>>,
    qos: Qos,
) {
    let current_thread = CurrentThread {
        handle,
        kernel_stack_base,
        kernel_stack_top,
        user_stack_base,
        qos,
    };
    // Safety: The scheduler lock ensures that there are no concurrent reads or writers and also
    // ensures interrupts are off so the write cannot be interrupted to create a torn write
    unsafe { *this_hart().current_thread.get() = Some(current_thread) };
}
/// Set the current thread details.
///
/// Safety: Caller must ensure that there are no concurrent readers or writers
pub(crate) unsafe fn set_current_thread_unchecked(
    handle: thread::Handle,
    kernel_stack_base: NonNull<u8>,
    kernel_stack_top: NonNull<u8>,
    user_stack_base: Option<NonNull<u8>>,
    qos: Qos,
) {
    let current_thread = CurrentThread {
        handle,
        kernel_stack_base,
        kernel_stack_top,
        user_stack_base,
        qos,
    };
    // Safety: The scheduler lock ensures that there are no concurrent reads or writers and also
    // ensures interrupts are off so the write cannot be interrupted to create a torn write
    unsafe { *this_hart().current_thread.get() = Some(current_thread) };
}
/// Get the current thread kernel stack base.
///
/// # Panics
/// Panics if the current thread has not yet been installed
#[track_caller]
pub(crate) fn current_kernel_stack_base() -> NonNull<u8> {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().current_thread.get() }
        .map(|c| c.kernel_stack_base)
        .expect("current thread should be installed")
}
/// Get the current thread kernel stack top
///
/// # Panics
/// Panics if the current thread has not yet been installed
#[track_caller]
pub(crate) fn current_kernel_stack_top() -> NonNull<u8> {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().current_thread.get() }
        .map(|c| c.kernel_stack_top)
        .expect("current thread should be installed")
}
/// Get the current thread user stack base.
/// Returns `None` if the current thread is not a user thread.
///
/// # Panics
/// Panics if the current thread is not installed.
#[track_caller]
pub(crate) fn current_user_stack_base() -> Option<NonNull<u8>> {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().current_thread.get() }
        .expect("current thread should be installed")
        .user_stack_base
}
/// Get the current thread QoS
///
/// # Panics
/// Panics if the current thread is not installed.
#[track_caller]
pub(crate) fn current_qos() -> Qos {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().current_thread.get() }
        .map(|c| c.qos)
        .expect("current thread should be installed")
}
/// Take the resume context details
///
/// Returns `None` if the details were not present
pub(crate) fn take_resume_context() -> Option<ResumeContext> {
    // Safety: this is this hart's PerCpu instance; no other hart writes or reads it
    unsafe { (*this_hart().resume_context.get()).take() }
}
/// Set the resume context details
/// Overwrites existing resume context (if any)
pub(crate) fn set_resume_context(resume_context: ResumeContext) {
    // Safety: this is this hart's PerCpu instance; no other hart writes or reads it
    unsafe { *this_hart().resume_context.get() = Some(resume_context) }
}
/// Get the switching-from thread handle.
/// Returns `None` if there is no switched from thread.
pub fn switching_from_thread() -> Option<thread::Handle> {
    // Safety: this is this hart's PerCpu instance; no other hart writes it concurrently, so no data race
    unsafe { *this_hart().switching_from_thread.get() }
}
/// Returns the other hart's switching-from thread handle if there is
/// a switching thread, otherwise returns `None`. Called with the scheduler
/// lock held to ensure no concurrent write.
#[cfg(feature = "trace")]
pub(crate) fn other_switching_from_thread(_sched: &SchedInner) -> Option<thread::Handle> {
    // Safety: the scheduler lock ensures there are no concurrent writes
    unsafe { *that_hart().switching_from_thread.get() }
}
/// Set the switching thread handle
/// Requres proof of holding a scheduler lock to prevent concurrent writes with reads.
/// Note also that the lock around SchedInner provides memory ordering.
pub fn set_switching_from_thread(_sched: &SchedInner, handle: Option<thread::Handle>) {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently as
    // we are holding the scheduler lock, so no data race
    unsafe { *this_hart().switching_from_thread.get() = handle };
}
/// Take the switching-from thread handle.
/// Takes a `&SchedInner` token to ensure the scheduler lock is held.
pub fn take_switching_from_thread(_sched: &SchedInner) -> Option<thread::Handle> {
    // Safety: this is this hart's PerCpu instance; no other hart reads or writes it concurrently as
    // we are holding the scheduler lock, so no data race
    unsafe { (*this_hart().switching_from_thread.get()).take() }
}
/// Check the needs_reschedule state of this HART
pub fn needs_reschedule() -> bool {
    // Memory ordering: Relaxed as this is just a flag, the IPI mailbox handles actual ordering
    this_hart().needs_reschedule.load(Ordering::Relaxed)
}
/// Check if the other HART is flagged for needing a reschedule.
/// Tracing thread behaviour requires reading cross-Hart PerCpu details.
#[cfg(feature = "trace")]
pub fn other_needs_reschedule() -> bool {
    // Memory ordering: Relaxed as this is purely a flag; actual ordering handled by the
    // IPI mailbox.
    that_hart().needs_reschedule.load(Ordering::Relaxed)
}
/// Check if this HART needs to call the scheduler.
/// Sets the reschedule flag to false on read.
pub fn take_needs_reschedule() -> bool {
    // Memory ordering: Relaxed as this is purely a flag; actual ordering handled by the
    // IPI mailbox.
    this_hart().needs_reschedule.swap(false, Ordering::Relaxed)
}
/// Set the reschedule request flag
pub fn set_needs_reschedule() {
    // Memory ordering: Relaxed as this is purely a flag; actual ordering handled by the
    // IPI mailbox.
    this_hart().needs_reschedule.store(true, Ordering::Relaxed);
}
