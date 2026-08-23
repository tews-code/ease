//! Entry for EASE traps
//!
//! Saves context and calls handler, returns with `mret`.

use core::ptr::NonNull;

use crate::sched::userloader;

#[repr(C, align(16))]
#[derive(Default)]
pub(crate) struct TrapFrame {
    pub(crate) ra: usize,
    gp: usize,
    tp: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
    pub(crate) a0: usize,
    pub(crate) a1: usize,
    pub(crate) a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    s0: usize,
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
    pub(crate) mepc: usize,
    pub(crate) mstatus: usize,
    pub(crate) user_sp: usize,
    _pad: [usize; 3],
}

impl TrapFrame {
    pub(crate) fn syscall(&self) -> usize {
        self.a7
    }

    pub(crate) fn is_from_user(&self) -> bool {
        (self.mstatus & crate::arch::csr::mstatus::MPP) == 0
    }

    pub(crate) fn init_for_user_entry(
        entry: userloader::UserEntry,
        user_stack_top: NonNull<u8>,
        user_exit: usize,
    ) -> Self {
        Self {
            ra: user_exit,
            mepc: entry.addr(),
            mstatus: 0, //  MPP=U, MPIE=0. later step will enable interrupts in U-mode
            user_sp: user_stack_top.addr().into(),
            ..Default::default()
        }
    }
}

pub(crate) const NUM_SLOTS: usize = 36;
const _: () = assert!(core::mem::size_of::<TrapFrame>() == NUM_SLOTS * 4);
// ra is always at the top
const _: () = assert!(core::mem::offset_of!(TrapFrame, ra) == 0);
const _: () = assert!(core::mem::offset_of!(TrapFrame, mepc) == (NUM_SLOTS - 6) * 4);
const _: () = assert!(core::mem::offset_of!(TrapFrame, mstatus) == (NUM_SLOTS - 5) * 4);
const _: () = assert!(core::mem::offset_of!(TrapFrame, user_sp) == (NUM_SLOTS - 4) * 4);
const _: () = assert!(
    core::mem::size_of::<TrapFrame>().is_multiple_of(core::mem::align_of::<TrapFrame>()),
    "trap frame size must be a multiple of its alignment so it lands aligned at top of a stack"
);

use crate::kernel::trap::{trap_handler_h0, trap_handler_h1};

crate::arch::percore_text::per_hart_trap_vector!(
    ".sram8_text",
    _trap_vector_h0,
    trap_handler_h0,
    ".sram9_text",
    _trap_vector_h1,
    trap_handler_h1,
    NUM_SLOTS,
    crate::arch::csr::mstatus::MPP,
    r#"
    # Swap sp with IRQ stack top in mscratch
    csrrw sp, mscratch, sp
    # Save registers to stack
    addi sp, sp, -4 * {num_slots}
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
    csrr t0, mepc
    sw t0,  4 * 30(sp)
    csrr t0, mstatus
    sw t0,  4 * 31(sp)
    # Keep a copy of the stack pointer before we entered (needed if user sp)
    csrr t0, mscratch
    sw t0,  4 * 32(sp)

    mv a0, sp
    call {handler}

    lw t0,  4 * 30(sp)
    csrw mepc, t0
    lw t0,  4 * 31(sp)
    csrw mstatus, t0

    # Set up user stack pointer if returning to U-mode
    # Check for U-mode
    li t1, {mstatus_MPP}
    and t1, t0, t1
    bnez t1, 2f
    lw t0, 4 * 32(sp)
    csrw mscratch, t0
    2:

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

    addi sp, sp, 4 * {num_slots}

    # Swap sp back into in mscratch
    csrrw sp, mscratch, sp

    mret
    "#
);

crate::arch::percore_text::naked_asm_function!(
    ".sram8_text",
    preempt_trampoline_h0,
    ".sram9_text",
    preempt_trampoline_h1,

    (
    "addi sp, sp, -4 * 20",  // 20 x 4 = 80 to keep 16 byte aligned even though we only store caller-saved registers
    "sw ra,  4 *  0(sp)",
    "sw gp,  4 *  1(sp)",
    "sw tp,  4 *  2(sp)",
    "sw t0,  4 *  3(sp)",
    "sw t1,  4 *  4(sp)",
    "sw t2,  4 *  5(sp)",
    "sw t3,  4 *  6(sp)",
    "sw t4,  4 *  7(sp)",
    "sw t5,  4 *  8(sp)",
    "sw t6,  4 *  9(sp)",
    "sw a0,  4 * 10(sp)",
    "sw a1,  4 * 11(sp)",
    "sw a2,  4 * 12(sp)",
    "sw a3,  4 * 13(sp)",
    "sw a4,  4 * 14(sp)",
    "sw a5,  4 * 15(sp)",
    "sw a6,  4 * 16(sp)",
    "sw a7,  4 * 17(sp)",

    // Get stored mepc and mstatus and stash
    "call {preempt_mepc}",
    "sw a0,  4 * 18(sp)",
    "call {preempt_mstatus}",
    "sw a0,  4 * 19(sp)",

    // Call the scheduler
    "call {schedule}",

    // Return
    "lw a0,  4 * 19(sp)",
    "csrw mstatus, a0",
    "lw a0,  4 * 18(sp)",
    "csrw mepc, a0",

    "lw ra,  4 *  0(sp)",
    "lw gp,  4 *  1(sp)",
    "lw tp,  4 *  2(sp)",
    "lw t0,  4 *  3(sp)",
    "lw t1,  4 *  4(sp)",
    "lw t2,  4 *  5(sp)",
    "lw t3,  4 *  6(sp)",
    "lw t4,  4 *  7(sp)",
    "lw t5,  4 *  8(sp)",
    "lw t6,  4 *  9(sp)",
    "lw a0,  4 * 10(sp)",
    "lw a1,  4 * 11(sp)",
    "lw a2,  4 * 12(sp)",
    "lw a3,  4 * 13(sp)",
    "lw a4,  4 * 14(sp)",
    "lw a5,  4 * 15(sp)",
    "lw a6,  4 * 16(sp)",
    "lw a7,  4 * 17(sp)",

    "addi sp, sp, +4 * 20",

    "mret",
    preempt_mepc = sym crate::kernel::percpu::preempt_mepc,
    preempt_mstatus = sym crate::kernel::percpu::preempt_mstatus,
    schedule = sym crate::kernel::sched::schedule,
    )
);
