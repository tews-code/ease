//! Context swap for co-operative scheduler

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text
    .global swap_to
    .align 4
    swap_to:
        # Save callee-saved registers to stack (others have been spilled by compiler)
        addi sp, sp, -4 * 16 # We only use the first 13 but want alignment
        sw s0,  4 *  0(sp)
        sw s1,  4 *  1(sp)
        sw s2,  4 *  2(sp)
        sw s3,  4 *  3(sp)
        sw s4,  4 *  4(sp)
        sw s5,  4 *  5(sp)
        sw s6,  4 *  6(sp)
        sw s7,  4 *  7(sp)
        sw s8,  4 *  8(sp)
        sw s9,  4 *  9(sp)
        sw s10, 4 * 10(sp)
        sw s11, 4 * 11(sp)
        sw ra,  4 * 12(sp)

        # Swap sp (a0 holds pointer to prev, a1 holds pointer to next)
        sw sp, 0(a0)
        lw sp, 0(a1)

        # Restore the callee-saved registers
        lw s0,  4 *  0(sp)
        lw s1,  4 *  1(sp)
        lw s2,  4 *  2(sp)
        lw s3,  4 *  3(sp)
        lw s4,  4 *  4(sp)
        lw s5,  4 *  5(sp)
        lw s6,  4 *  6(sp)
        lw s7,  4 *  7(sp)
        lw s8,  4 *  8(sp)
        lw s9,  4 *  9(sp)
        lw s10, 4 * 10(sp)
        lw s11, 4 * 11(sp)
        lw ra,  4 * 12(sp)
        addi sp, sp, 4 * 16

        ret
    "#
);

unsafe extern "C" {
    // Safety: caller must ensure prev points to a writable slot owned by the current
    // thread; next points to a slot containing a saved sp produced by a prior swap_to call or
    // by spawn's stack forging; calling with interrupts disabled is undefined;
    pub(crate) fn swap_to(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
}
