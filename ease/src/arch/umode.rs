//! User mode

use core::arch::naked_asm;

use crate::drivers::keyboard;
use crate::kernel::sched::ExitReason;
use crate::kernel::sched::{self, post_switch_cleanup};
use ease_abi::syscall;

/// User threads that exit via this function are faulting or voluntary exit.
/// Threads that are exited in `post_switch_cleanup` do not pass through this function.
/// It is possible for multiple threads in the same process running on different HARTs to
/// arrive in this function simultaneously.
///
/// It is entered via trap return from `exit_from_user`; `a0` carries the ExitReason.
///
/// # Panics #
/// Panics if the exit reason is unknown
pub(crate) extern "C" fn user_thread_exit(reason: usize) -> ! {
    let exit_reason = match reason {
        0 => ExitReason::Exit,
        1 => ExitReason::Fault,
        _ => panic!("unknown user thread exit reason"),
    };
    sched::exit_user_thread(exit_reason);
}
/// Handles blocking system calls for user threads
///
/// # Panics #
/// Panics if the system call number is unknown
pub(crate) extern "C" fn user_thread_block(
    return_address: usize,
    user_sp: usize,
    syscall: usize,
) -> ! {
    match syscall {
        syscall::GET_CHAR => {
            loop {
                if let Some(b) = keyboard::read_key() {
                    resume_user(0, b, return_address, user_sp);
                } else {
                    // Block using a completion on the key press
                    keyboard::KEY_PENDING.wait();
                }
            }
        }
        _ => panic!("unexpected blocking syscall: {}", syscall),
    }
}
/// Sets up user thread for first run
#[unsafe(naked)]
pub extern "C" fn user_first_run() -> ! {
    naked_asm!(
        "call {post_switch_cleanup}",
        // Set up mepc and mstatus
        "lw t0,  4 * 30(sp)",
        "csrw mepc, t0",
        "lw t0,  4 * 31(sp)",
        "csrw mstatus, t0",
        // Set up mscratch to the user sp
        // "lw t0,  4 * 32(sp)",
        // "csrw mscratch, t0",

        // Load GP registers from forged trap frame in thread's kernel stack
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
        "lw s0,  4 * 18(sp)",
        "lw s1,  4 * 19(sp)",
        "lw s2,  4 * 20(sp)",
        "lw s3,  4 * 21(sp)",
        "lw s4,  4 * 22(sp)",
        "lw s5,  4 * 23(sp)",
        "lw s6,  4 * 24(sp)",
        "lw s7,  4 * 25(sp)",
        "lw s8,  4 * 26(sp)",
        "lw s9,  4 * 27(sp)",
        "lw s10, 4 * 28(sp)",
        "lw s11, 4 * 29(sp)",

        // Set up stack pointer
        //"addi sp, sp, 4 * {num_slots}",

        // Swap kernel sp with mscratch (user sp)
        // "csrrw sp, mscratch, sp",

        // Load sp from frame
        "lw sp, 4 * 32(sp)",

        // Ensure .text is ready for execution
        "fence.i",

        "mret",
        post_switch_cleanup = sym post_switch_cleanup,
        // num_slots = const crate::arch::trap::NUM_SLOTS,
    );
}
/// Return to user thread from M mode
///
/// `error` is returned in `a0` with 0 indicating success
/// `value` is returned in `a1`
#[unsafe(naked)]
pub extern "C" fn resume_user(
    error: usize,
    value: usize,
    resume_address: usize,
    user_sp: usize,
) -> ! {
    naked_asm!(
        // Note that `error` is already in a0 and value is already in a1 as they are the first function arguments
        // Set up stack pointer
        "mv sp, a3",
        //Ensure interrupts are enabled in user mode
        "li t0, {mstatus_MIE}",
        "csrc mstatus, t0",
        "li t0, {mstatus_MPP}",
        "csrc mstatus, t0",
        "li t0, {mstatus_MPIE}",
        "csrc mstatus, t0",
        // Set the return address to the user thread
        "csrw mepc, a2",
        "mret",
        mstatus_MIE = const crate::arch::csr::mstatus::MIE,
        mstatus_MPP = const crate::arch::csr::mstatus::MPP,
        mstatus_MPIE = const crate::arch::csr::mstatus::MPIE
    );
}
