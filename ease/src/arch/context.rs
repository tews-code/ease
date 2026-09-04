//! Context switch
//!
//! EASE supports preemptive scheduling but all threads use the same cooperative switch_to
//! mechanism. On RP2350 the RISCV cores do not perform hardware caller register saves
//! so need to keep a lightweight yield function so the C ABI performs this automatically.
//!

//! KERNEL THREAD SPAWN
//!
//! 1. An existing running thread is needed to spawn a new thread. See [sched::spawn::spawn_kernel_thread_with]. The new thread
//!    body is provided as a closure and is run in a function that ensures it is run once followed by a clean exit.
//!    See [kernel_thread_closure_runner].
//!
//! 2. New threads are created with a forged context to match the expected context for `switch_to` (see macro below). These
//!    are callee saved registers and `ra`
//!
//! 3. Acquring a new thread control block is performed as a single function call that guarantees the TCB is configured, the
//!    stack is forged and the TCB state is Ready - allowing the scheduler to immediately pick up and run the thread. See
//!    [sched::threads::acquire].
//!
//! 4. Since the context switch is callee saved registers only, it is necessary to use pointers in the `s` registers rather than
//!    passing directly.  See [init_for_kernel_entry]. The converse is that the values need to be moved to `a` registers (to appear
//!    as function arguments before the thread can run), which is the function of the shim. See [kernel_thread_first_run_shim].
//!
//! 5. Once the thread is running, it is expected to perform the same post-switch cleanup on the prior thread as any other thread.
//!    It performs this work, then reenables interrupts, and finally runs the closure in through the runner function. See
//!    [kernel_thread_first_run].
//!
//! Since the closure pointer needs to survive the switch into the new thread, it is placed on the heap and
//! then orphaned to avoid a drop, keeping only the raw pointer. See [forge_kernel_thread_stack]. The inverse of
//! turning this back into a function to call is performed by [run_closure].
//!
//! The closure must be FnOnce() + Send + 'static:
//!  - FnOnce() - call_once(self) — takes self by value. The call is a move so the closure is consumed (and can't run again).
//!  - Send - the closure must be Send as it may be run on the other HART.
//!  - 'static - only static context can be taken from the original thread that has spawned the new thread, no dangling references.

use alloc::boxed::Box;
use core::arch::naked_asm;
use core::ptr::NonNull;

use crate::arch::{interrupts, per_hart};
use crate::kernel::alloc::MemRegion;
use crate::kernel::sched::{self, ExitReason};
use crate::kernel::stack;

// Context switch asm
//
// Note: Creating two copies of .text for each HART's hot path
//
// Save callee-saved registers to stack. Swap sp and restore.
//
// # Invariants #
// Must be run with interrupts disabled
per_hart::naked_asm_function!(
    /* No visibility */,
    ".sram8_text",
    switch_to_h0,
    ".sram9_text",
    switch_to_h1,
    (
        // Save callee-saved registers to the stack
        "addi sp, sp, -4 * 16", // Need 16 byte alignment even for 13 regs
        "sw ra,  4 *  0(sp)",
        "sw s0,  4 *  1(sp)",
        "sw s1,  4 *  2(sp)",
        "sw s2,  4 *  3(sp)",
        "sw s3,  4 *  4(sp)",
        "sw s4,  4 *  5(sp)",
        "sw s5,  4 *  6(sp)",
        "sw s6,  4 *  7(sp)",
        "sw s7,  4 *  8(sp)",
        "sw s8,  4 *  9(sp)",
        "sw s9,  4 * 10(sp)",
        "sw s10, 4 * 11(sp)",
        "sw s11, 4 * 12(sp)",
        // Perform switch
        "sw sp, 0(a0)",
        "lw sp, 0(a1)",
        // Restore callee saved registers
        "lw ra,  4 *  0(sp)",
        "lw s0,  4 *  1(sp)",
        "lw s1,  4 *  2(sp)",
        "lw s2,  4 *  3(sp)",
        "lw s3,  4 *  4(sp)",
        "lw s4,  4 *  5(sp)",
        "lw s5,  4 *  6(sp)",
        "lw s6,  4 *  7(sp)",
        "lw s7,  4 *  8(sp)",
        "lw s8,  4 *  9(sp)",
        "lw s9,  4 * 10(sp)",
        "lw s10, 4 * 11(sp)",
        "lw s11, 4 * 12(sp)",
        "addi sp, sp, 4 * 16",
        "ret",
    )
);

// Expect this many callee-saved registers
const SWITCH_TO_FRAME_SLOTS: usize = 16;

// The RISC-V psABI requires 16 byte alignment, and it splits regs
// into callee-saved and caller-saved.
// The compiler has already preserved any caller-saved regs across
// our extern "C" call.
#[repr(C, align(16))]
#[derive(Default)]
pub(crate) struct Frame {
    pub(crate) ra: usize,
    pub(crate) s0: usize,
    pub(crate) s1: usize,
    pub(crate) s2: usize,
    pub(crate) s3: usize,
    pub(crate) s4: usize,
    pub(crate) s5: usize,
    pub(crate) s6: usize,
    pub(crate) s7: usize,
    pub(crate) s8: usize,
    pub(crate) s9: usize,
    pub(crate) s10: usize,
    pub(crate) s11: usize,
}

const _: () = assert!(core::mem::size_of::<Frame>().is_multiple_of(core::mem::align_of::<Frame>()));
const _: () = assert!(core::mem::size_of::<Frame>() == SWITCH_TO_FRAME_SLOTS * 4);
// ra is always at the top
const _: () = assert!(core::mem::offset_of!(Frame, ra) == 0);

impl Frame {
    // Forges a thread Context for a kernel thread
    // The thread entry function is stored in s0
    // Returns the stack pointer
    // Safety: stack_base must be class.size()-aligned and point to
    // writeable memory of at least class.size() bytes
    pub(super) unsafe fn init_stack_for_kernel_thread(
        stack: &mut MemRegion,
        closure_run: extern "C" fn(*mut u8) -> !,
        closure_ptr: *mut u8,
    ) -> NonNull<u8> {
        debug_assert!(
            stack.size() > core::mem::size_of::<Frame>(),
            "stack memory region too small for context switch"
        );
        // Safety: Caller has ensured base and top addresses are aligned and valid for writes
        unsafe {
            #[cfg(feature = "paint-stack")]
            stack::paint(stack.base_addr(), stack.top().addr().into());
            stack::set_canary(stack.base_addr());
        }
        let context_ptr = unsafe {
            stack
                .base()
                .add(stack.size() - core::mem::size_of::<Frame>())
        };
        // Safety: context_ptr is a multiple of Context's align
        unsafe {
            context_ptr
                .cast::<Frame>()
                .as_ptr()
                .write(Frame::forge_for_kernel_entry(closure_run, closure_ptr));
        }
        context_ptr
    }
    /// Forge a stack context for a kernel thread
    ///
    /// `ra`: set to a small shim that switches arguments from the temporary s-registers to expected a-registers
    /// `s0`: points to the function that safely runs the closure once and exits cleanly
    /// `s1`: points to the closure to be run
    fn forge_for_kernel_entry(
        kernel_thread_closure_runner: extern "C" fn(*mut u8) -> !,
        closure_ptr: *mut u8,
    ) -> Self {
        Self {
            ra: kernel_thread_first_run_shim as *const () as usize,
            s0: kernel_thread_closure_runner as usize,
            s1: closure_ptr as usize,
            ..Self::default()
        }
    }
}
/// ABI shim: `Context::init_for_kernel_entry.` placed `trampoline_ptr` in `s0`
/// and `closure_ptr` in `s1` so they survived `switch_to`'s callee-saved
/// restore. Move them to `a0`/`a1` and tail-call `kernel_thread_first_run`.
///
/// # Safety #
/// Reachable only via `switch_to` returning into a `Context` forged by `Context::init_for_kernel_entry.`
#[unsafe(naked)]
unsafe extern "C" fn kernel_thread_first_run_shim() {
    naked_asm!(
        "mv a0, s0",
        "mv a1, s1",
        "tail {trampoline}",    // Does not set unwanted return address before call as we do not want to return to this shim.
        trampoline = sym kernel_thread_first_run,
    );
}
/// Run cleanup and enable interrupts then run the closure for a new kernel thread that has just spawned.
///
/// A new thread is still expected to perform cleanup on the previously run thread
/// through [sched::post_switch_cleanup], before enabling interrupts and running
/// closure within the confines of [kernel_thread_closure_runner].
///
/// Expects the arguments in "C" format (arguments in `a0` and `a1`) so [kernel_thread_first_run_shim] must
/// be run first.
unsafe extern "C" fn kernel_thread_first_run(
    kernel_thread_closure_runner: extern "C" fn(*mut u8) -> !,
    closure_ptr: *mut u8,
) -> ! {
    sched::post_switch_cleanup(); // All threads are expected to perform clean up on previous thread when scheduled
    interrupts::enable(); // Thread was spawned with mstatus set to interrupts disabled. We are finally ready to reenable.
    kernel_thread_closure_runner(closure_ptr); // Now run the closure
}
/// Forge a kernel thread stack for first run
///
/// Takes a memory region and the entry closure
/// and returns a `NonNull<u8>` pointer to the stack position post-forging.
pub(crate) fn forge_kernel_thread_stack<F>(region: &mut MemRegion, entry: F) -> NonNull<u8>
where
    F: FnOnce() + Send + 'static,
{
    // Box the entry closure and immediately get a raw pointer to the start of the closure to put into the forged stack
    let closure_ptr = Box::into_raw(Box::new(entry)) as *mut u8;
    // Initialise the stack with run_closure (which handles a single run and clean exit) and our new closure pointer
    unsafe {
        Frame::init_stack_for_kernel_thread(region, kernel_thread_closure_runner::<F>, closure_ptr)
    }
}
/// Unbox a heap-stored closure, run it once and call the scheduler exit
extern "C" fn kernel_thread_closure_runner<F: FnOnce() + Send + 'static>(entry_ptr: *mut u8) -> ! {
    // Unbox the closure body and run it
    let body = unsafe { Box::from_raw(entry_ptr as *mut F) };
    body(); // runs the closure exactly once and consumes both the closure and the Box.
    // Call the scheduler for a clean exit
    sched::exit_kernel_thread(ExitReason::Exit);
}
