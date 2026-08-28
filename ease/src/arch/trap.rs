//! Entry for EASE traps
//!
//! EASE uses an IRQ stack per HART to use the hot path SRAM (SRAM8 for HART0, SRAM9 for HART1).
//! As a consequence, the IRQ stack is only used for the initial trap. Unless there is an immediate
//! return, the trap handler needs to use a trampoline to move off the IRQ stack and back onto the
//! interrupted thread's stack (while it is suspended) in order to proceed outside of the trap
//! handler itself.
//! A preempt trampoline (per-HART) is available to save caller-saved registers to forge a Rust function
//! call, allowing for further function calls, e.g. taking the scheduler lock and rescheduling.

/*
                    THREAD PREEMPT TRAP PATH

                              Thread
                          M-Mode or U-Mode (target design)
                        (Interrupts enabled)
        User Stack or
       Kernel Stack             o
     (for this thread)          |
        +--top--+               |
        |-------|               |                               Interrupt trap automatically sets:
  sp -> |-------|               |                                 - mepc <- interrupted instruction (which has not yet run)
        |-------|               |     / --------------            - pc <- mtvec (set to per-HART trap vector at boot)
        |-------|               |    /                            - mcause <- high bit = 1 (interrupt), low bits = 7 (timer)
        |-------|               |   /      Timer                  - mstatus.MPIE <- mstatus.MIE (current interrupt status = enabled)
        |-------|         pc -> | -/       Interrupt              - mstatus.MIE <- 0 (interrupts disabled)
        |-------|               |   \      runs                   - mstatus.MPP <- 11 (came from M-mode) or 00 (came from U-mode)
        +-base--+               |    \     here
                                |     \
                                |      \--------------
                                |
                                |
                                |
                                v


                        Timer Interrupt                     All traps land on the IRQ stack, but only one trap is allowed at a time.
                              M-Mode                        We store the interrupted thread's state, then look at the trap reason.
                     (Interrupts Disabled)                  If the trap reason means further work, we need to get off the IRQ stack.

                    pc (from mtvec) ->  o  arch::trap::per_hart_trap_vector
                                        |  - swap interrupted sp with mscratch - to switch to IRQ stack while remembering original sp
               IRQ Stack                |  - Store the trap frame inc interrupted thread's mepc and mstatus
             (for this HART)            |
        +-> +--top--+ <- Set in         +-> o  kernel::trap::trap_handler_impl
        |   |mstatus|   mscratch            |  - Examine mcause for trap reason
     stack  |--mepc-|   at boot             |
     frame  |--tp---|                       +-> o  sched::mark_for_preempt
        |   |--gp---|                           +-> o  timer::set_next_deadline(timer::elapsed() + SLICE)
  sp -> +-> |--ra --|                           +-> o  percpu::set_needs_reschedule();
            |-------|                       o <-o
            |-------|                       |  - If needs reschedule (which we just set!) then proceed down this path. Otherwise mret back to the original thread directly.
            |-------|                       |  - Stash mepc and mstatus in percpu  - these are the original thread's details
            +-base--+                       +-> o  arch::trap::set_up_for_divert_to_kernel
                                                |  - store trampoline address in frame's mepc
                                                |  - set frame's mstatus to previous M-mode with interrupts disabled
                                        o <-----o
                                        |  - Restore trap frame (with mepc altered to point to preempt trampoline, mstatus set to ret to M-mode interrupts disabled)
                                        |  - Swap sp back with mscratch - sp now goes back to interrupted thread's stack
                                        o  - mret
                                                                                                                    mret automatically sets:
                                                                                                                    - pc <- mepc
                                                                                                                    - HART mode <- mstatus.MPP (11 - M-mode)
                                                                                                                    - mstatus.MIE <- mstatus.MPIE (disable interrupts)
                    Preempt Trampoline     If we need to make Rust calls, we can't be on the IRQ stack.             - mstatus.MPIE <- 1
                        M-Mode             So we move to the interrupted thread's kernel stack and use that space.  - mstatus.MPP <- 00 (always least privileged U mode)
                 (Interrupts Disabled)     Then we forge a Rust function call by saving the caller-saved regs.


             pc (from mepc) ->  o  arch::trap::preempt_trampoline_h0 - Needs to forge a caller frame so that we can make a Rust call (the thread didn't ask for it)
                                |  - store caller frame
                                |  - fetch and store original thread's mepc and mstatus from percpu stash
         Kernel Stack           |
       (for this thread)        +-> o  sched::schedule
            +--top--+               |  - takes scheduler lock
            |-------|               |  - performs reschedule
        +-> |mstatus|                   ...
        |   |--mepc-|                     Scheduler may switch, or keep current thread scheduled. On switch, interrupts may be enabled.
     caller |--a0---|                   ...
     frame  |--t0---|               |  - scheduler re-schedules this thread
        |   |--gp---|               +-> o  sched::post_switch_cleanup
  sp -> +-> |--ra---|           + <-+
            |-------|          |  - restore mstatus (first, to keep interrupts off as MIE is 0)
            |-------|          |  - Now safely restore mepc - which is the original thread's interrupted instruction
            |-------|          |  - restore caller frame
            +-base--+          o  - mret back to the original thread






*/
use core::ptr::NonNull;

use crate::arch::{csr, per_hart};
use crate::kernel::percpu;
use crate::kernel::trap::{trap_handler_h0, trap_handler_h1};
use crate::sched::{self, userloader};

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
    pub(crate) sp: usize,
    _pad: [usize; 3],
}

impl TrapFrame {
    pub(crate) fn syscall(&self) -> usize {
        self.a7
    }
    /// Divert mret to a kernel function given in `mepc`
    ///
    /// Disables interrupts and sets to run in M-mode after `mret`
    pub(crate) fn set_up_for_divert_to_kernel(&mut self, mepc: usize) {
        // Set up frame for trampoline
        self.mepc = mepc;
        self.mstatus &= !csr::mstatus::MPIE; // Ensure trampoline executes with interrupts disabled
        self.mstatus |= csr::mstatus::MPP; // Run the trampoline in M-mode
    }
    /// Check if a user thread had the trap
    pub(crate) fn is_from_user(&self) -> bool {
        (self.mstatus & csr::mstatus::MPP) == 0
    }
    /// Initialise a frame for the user entry trampoline
    pub(crate) fn init_for_user_entry(
        entry: userloader::UserEntry,
        user_stack_top: NonNull<u8>,
        user_exit: usize,
    ) -> Self {
        Self {
            ra: user_exit,
            mepc: entry.addr(),
            mstatus: 0, //  MPP=U, MPIE=0. later step will enable interrupts in U-mode
            sp: user_stack_top.addr().into(),
            ..Default::default()
        }
    }
}

pub(crate) const NUM_SLOTS: usize = 36;
const _: () = assert!(core::mem::size_of::<TrapFrame>() == NUM_SLOTS * 4);
const _: () = assert!(core::mem::offset_of!(TrapFrame, ra) == 0);
const _: () = assert!(core::mem::offset_of!(TrapFrame, mepc) == (NUM_SLOTS - 6) * 4);
const _: () = assert!(core::mem::offset_of!(TrapFrame, mstatus) == (NUM_SLOTS - 5) * 4);
const _: () = assert!(core::mem::offset_of!(TrapFrame, sp) == (NUM_SLOTS - 4) * 4);
const _: () = assert!(
    core::mem::size_of::<TrapFrame>().is_multiple_of(core::mem::align_of::<TrapFrame>()),
    "trap frame size must be a multiple of its alignment so it lands aligned at top of a stack"
);

/// Caller-saved registers
///
/// Also holds `mepc` and `mstatus` for returning from Rust functions in trap handler deferred execution
#[repr(C, align(16))]
#[derive(Default)]
pub(crate) struct CallerSavedFrame {
    ra: usize,
    gp: usize,
    tp: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    mepc: usize,
    mstatus: usize,
}

pub(crate) const CALLER_SAVED_SLOTS: usize = 20;
// Compile-time checks: if a field or a `sw` in the shim moves, the build fails
// here instead of the dispatcher silently reading the wrong register.
const _: () = assert!(core::mem::offset_of!(CallerSavedFrame, ra) == 0);
const _: () = assert!(core::mem::offset_of!(CallerSavedFrame, a0) == 4 * 10);
const _: () = assert!(core::mem::offset_of!(CallerSavedFrame, a1) == 4 * 11);
const _: () = assert!(core::mem::offset_of!(CallerSavedFrame, mepc) == 4 * 18);
const _: () = assert!(core::mem::offset_of!(CallerSavedFrame, mstatus) == 4 * 19);
const _: () = assert!(core::mem::size_of::<CallerSavedFrame>() == 4 * CALLER_SAVED_SLOTS);

per_hart::trap_vector!(
    ".sram8_text",
    _trap_vector_h0,
    trap_handler_h0,
    ".sram9_text",
    _trap_vector_h1,
    trap_handler_h1,
    NUM_SLOTS,
    csr::mstatus::MPP,
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

per_hart::naked_asm_function!(
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
    preempt_mepc = sym percpu::resume_mepc,
    preempt_mstatus = sym percpu::resume_mstatus,
    schedule = sym sched::schedule,
    )
);
