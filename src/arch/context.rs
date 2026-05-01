//! Context swap for co-operative scheduler

use core::arch::global_asm;

use crate::arch::csr::mstatus::{MPIE, MPP};

global_asm!(
    r#"
    .section .text
    .global swap_to
    .align 4
    swap_to:
        # Save all relevant registers to the stack
        addi sp, sp, -4 * 32
        sw ra,  4 *  0(sp)
        sw gp,  4 *  1(sp)
        sw tp,  4 *  2(sp)
        sw t0,  4 *  3(sp)
        sw t1,  4 *  4(sp)
        sw t2,  4 *  5(sp)
        sw t3,  4 *  6(sp)
        sw t4,  4 *  7(sp)
        sw t5,  4 *  8(sp)
        sw t6,  4 *  9(sp)
        sw a0,  4 * 10(sp)
        sw a1,  4 * 11(sp)
        sw a2,  4 * 12(sp)
        sw a3,  4 * 13(sp)
        sw a4,  4 * 14(sp)
        sw a5,  4 * 15(sp)
        sw a6,  4 * 16(sp)
        sw a7,  4 * 17(sp)
        sw s0,  4 * 18(sp)
        sw s1,  4 * 19(sp)
        sw s2,  4 * 20(sp)
        sw s3,  4 * 21(sp)
        sw s4,  4 * 22(sp)
        sw s5,  4 * 23(sp)
        sw s6,  4 * 24(sp)
        sw s7,  4 * 25(sp)
        sw s8,  4 * 26(sp)
        sw s9,  4 * 27(sp)
        sw s10, 4 * 28(sp)
        sw s11, 4 * 29(sp)

        sw ra, 4 * 30(sp)       #(mepc slot = ra)
        li t0, {init_mstatus}
        sw t0, 4 * 31(sp)       #(mstatus slot)

        # Swap sp (a0 holds pointer to prev, a1 holds pointer to next)
        sw sp, 0(a0)
        lw sp, 0(a1)

        lw t0,  4 * 30(sp)
        csrw mepc, t0
        lw t0,  4 * 31(sp)
        csrw mstatus, t0

        # Restore the callee-saved registers
        lw ra,  4 *  0(sp)
        lw gp,  4 *  1(sp)
        lw tp,  4 *  2(sp)
        lw t0,  4 *  3(sp)
        lw t1,  4 *  4(sp)
        lw t2,  4 *  5(sp)
        lw t3,  4 *  6(sp)
        lw t4,  4 *  7(sp)
        lw t5,  4 *  8(sp)
        lw t6,  4 *  9(sp)
        lw a0,  4 * 10(sp)
        lw a1,  4 * 11(sp)
        lw a2,  4 * 12(sp)
        lw a3,  4 * 13(sp)
        lw a4,  4 * 14(sp)
        lw a5,  4 * 15(sp)
        lw a6,  4 * 16(sp)
        lw a7,  4 * 17(sp)
        lw s0,  4 * 18(sp)
        lw s1,  4 * 19(sp)
        lw s2,  4 * 20(sp)
        lw s3,  4 * 21(sp)
        lw s4,  4 * 22(sp)
        lw s5,  4 * 23(sp)
        lw s6,  4 * 24(sp)
        lw s7,  4 * 25(sp)
        lw s8,  4 * 26(sp)
        lw s9,  4 * 27(sp)
        lw s10, 4 * 28(sp)
        lw s11, 4 * 29(sp)

        addi sp, sp, 4 * 32

        mret
    "#,
    init_mstatus = const (MPIE | MPP),
);

unsafe extern "C" {
    // Safety: caller must ensure prev points to a writable slot owned by the current
    // thread; next points to a slot containing a saved sp produced by a prior swap_to call or
    // by spawn's stack forging; calling with interrupts disabled is undefined;
    pub(crate) fn swap_to(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
}
