//! Entry for EASE traps
//!
//! Saves context and calls handler, returns with `mret`.

use core::arch::asm;
use core::arch::global_asm;

global_asm!(
    r#"
    .section .text
    .global _trap_vector
    .align 4
    _trap_vector:
        # Save registers to stack
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

        call trap_handler

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
"#
);

const EXCEPTION_ILLEGAL_INSTRUCTION: usize = 2;
const INTERRUPT_TIMER: usize = 7;

#[unsafe(no_mangle)]
extern "C" fn trap_handler() {
    let mcause: usize;
    let mepc: usize;

    unsafe {
        asm!("csrr {}, mcause", out(reg) mcause);
        asm!("csrr {}, mepc", out(reg) mepc);
    }

    // Bit 31 - 1 means interrupt, 0 means exception
    let is_interrupt = (mcause >> 31) & 1 == 1;
    let code = mcause & 0x7FFFFFFF;

    if is_interrupt {
        match code {
            INTERRUPT_TIMER => crate::kernel::timer::handle_interrupt(),
            _ => crate::printdln!("Unknown interrupt {}", code),
        }
    } else {
        match code {
            EXCEPTION_ILLEGAL_INSTRUCTION => panic!("Illegal instruction at {:x}", mepc),
            _ => panic!("Unknown exception mcause {:x} mepc {:x}", mcause, mepc),
        }
    }
}
