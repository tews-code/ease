//! Context switch for co-operative scheduling
// On RP2350 the RISCV cores do not perform hardware caller register
// so keep a lightweight yield function

use core::arch::naked_asm;

use super::usermode::user_first_run;

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
