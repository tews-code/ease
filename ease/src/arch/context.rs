//! Context switch for co-operative scheduling
// On RP2350 the RISCV cores do not perform hardware caller register
// so keep a lightweight yield function

use core::arch::naked_asm;
use core::ptr::NonNull;

use super::usermode::user_first_run;
use crate::arch::trap::TrapFrame;
use crate::kernel::alloc::MemRegion;
use crate::kernel::sched::userloader;
#[cfg(feature = "paint-stack")]
use crate::kernel::stack::paint_stack;
use crate::kernel::stack::set_canary;

// The RISC-V psABI requires 16 byte alignment, and it splits regs
// into callee-saved and caller-saved.
// The compiler has already preserved any caller-saved regs across
// our extern "C" call.
#[repr(C, align(16))]
#[derive(Default)]
pub struct Context {
    ra: usize,
    pub s0: usize,
    s1: usize,
    s2: usize,
    s3: usize,
    s4: usize,
    s5: usize,
    s6: usize,
    s7: usize,
    s8: usize,
    s9: usize,
    s10: usize,
    s11: usize,
}

const _: () =
    assert!(core::mem::size_of::<Context>().is_multiple_of(core::mem::align_of::<Context>()));
// ra is always at the top
const _: () = assert!(core::mem::offset_of!(Context, ra) == 0);

impl Context {
    // Forges a thread Context for a kernel thread
    // The thread entry function is stored in s0
    // Returns the stack pointer
    // Safety: stack_base must be class.size()-aligned and point to
    // writeable memory of at least class.size() bytes
    pub unsafe fn init_kernel_stack(
        stack: &mut MemRegion,
        closure_run: extern "C" fn(*mut u8) -> !,
        closure_ptr: *mut u8,
    ) -> NonNull<u8> {
        debug_assert!(
            stack.size() > core::mem::size_of::<Context>(),
            "stack memory region too small for context switch"
        );
        // Safety: Caller has ensured base and top addresses are aligned and valid for writes
        unsafe {
            #[cfg(feature = "paint-stack")]
            paint_stack(stack.base_addr(), stack.top().addr().into());
            set_canary(stack.base_addr());
        }
        let context_ptr = unsafe {
            stack
                .base()
                .add(stack.size() - core::mem::size_of::<Context>())
        };
        // Safety: context_ptr is a multiple of Context's align
        unsafe {
            context_ptr
                .cast::<Context>()
                .as_ptr()
                .write(Context::init_for_kernel_entry(closure_run, closure_ptr));
        }
        context_ptr
    }

    // Forges a thread Context for a user thread
    // Returns the stack pointer
    // Safety:
    // - stack_base must be class.size()-aligned and point to
    // writeable memory of at least class.size() bytes.
    // - user stack top must be the top of a live, U-mode-accessible region
    pub unsafe fn init_user_stack(
        kernel_stack: &mut MemRegion,
        entry: userloader::UserEntry,
        user_stack_top: NonNull<u8>,
        user_exit: usize,
    ) -> NonNull<u8> {
        debug_assert!(
            kernel_stack.size()
                > core::mem::size_of::<Context>() + core::mem::size_of::<TrapFrame>(),
            "kernel stack memory region too small for context switch and trap return"
        );
        // Safety: kernel stack has aligned addresses and region is valid for writes
        unsafe {
            #[cfg(feature = "paint-stack")]
            paint_stack(kernel_stack.base_addr(), kernel_stack.top().addr().into());
            set_canary(kernel_stack.base_addr());
        }
        // Safety: trap_frame_ptr is derived from stack_base and aligned
        unsafe {
            // Set up a trap frame so trap return arrives in U-mode
            let trap_frame_ptr = kernel_stack
                .base()
                .as_ptr()
                .add(kernel_stack.size() - core::mem::size_of::<TrapFrame>())
                as *mut TrapFrame;
            core::ptr::write(
                trap_frame_ptr,
                TrapFrame::init_for_user_entry(entry, user_stack_top, user_exit),
            );
        }
        // Set up a switch context
        // Safety: context_ptr is derived from stack_base, and
        // aligned because sizeof(TrapFrame) + sizeof(Context) is a multiple of align(Context).
        let context_ptr = unsafe {
            kernel_stack
                .base()
                .add(
                    kernel_stack.size()
                        - core::mem::size_of::<TrapFrame>()
                        - core::mem::size_of::<Context>(),
                )
                .cast()
        };
        unsafe {
            context_ptr.write(Context::init_for_user_entry());
        }
        context_ptr.cast::<u8>()
    }

    pub fn init_for_kernel_entry(
        run_closure_thread: extern "C" fn(*mut u8) -> !,
        closure_ptr: *mut u8,
    ) -> Self {
        Self {
            ra: kernel_first_run_shim as *const () as usize,
            s0: run_closure_thread as usize,
            s1: closure_ptr as usize,
            ..Self::default()
        }
    }

    pub fn init_for_user_entry() -> Self {
        Self {
            ra: user_first_run as *const () as usize, // switch_to's ret lands in user_first_run; the trap frame above it holds the U-mode state"
            ..Self::default()
        }
    }
}

/// ABI shim: `Context::init_for_kernel_entry.` placed `trampoline_ptr` in `s0`
/// and `closure_ptr` in `s1` so they survived `switch_to`'s callee-saved
/// restore. Move them to `a0`/`a1` and tail-call `kernel_thread_first_run`.
// Safety: Reachable only via `switch_to` returning into a `Context` forged by `Context::init_for_kernel_entry.`
#[unsafe(naked)]
unsafe extern "C" fn kernel_first_run_shim() {
    naked_asm!(
        "mv a0, s0",
        "mv a1, s1",
        "tail {trampoline}",    // Does not set unwanted return address before call as we do not want to return to `kernel_first_run_shim`.
        trampoline = sym kernel_thread_first_run,
    );
}

unsafe extern "C" fn kernel_thread_first_run(
    trampoline_ptr: extern "C" fn(*mut u8) -> !,
    closure_ptr: *mut u8,
) -> ! {
    crate::kernel::sched::post_switch_cleanup();
    crate::arch::interrupts::enable();
    trampoline_ptr(closure_ptr);
}

crate::arch::percore_text::naked_asm_function!(
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
