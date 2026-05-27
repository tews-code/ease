//! User mode

use core::arch::naked_asm;

use crate::arch::csr::mstatus;

unsafe extern "C" {
    static __heap_pd0_end: u8;
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub(crate) extern "C" fn user_entry(entry: extern "C" fn()) {
    naked_asm!(
        // Disable interrupts
        "li t0, {mstatus_MIE}",
        "csrc mstatus, t0",
        // Store callee-saved registers
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
        "mv s0, a0",    // Keep a copy of a0
        "mv a0, sp",
        "call {set_kernel_resume_sp}",

        "csrw mepc, s0",
        "li t0, {mstatus_MPP}",
        "csrc mstatus, t0",
        "li t0, {mstatus_MPIE}",
        "csrc mstatus, t0",
        "la t0, {user_heap}",
        "mv sp, t0",
        "la t0, {user_exit}",
        "mv ra, t0",
        "mret",
        mstatus_MIE = const mstatus::MIE,
        set_kernel_resume_sp = sym crate::kernel::percpu::set_kernel_resume_sp,
        mstatus_MPP = const mstatus::MPP,
        mstatus_MPIE = const mstatus::MPIE,
        user_heap = sym __heap_pd0_end,
        user_exit = sym crate::user::user_exit,
    );
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub(crate) extern "C" fn resume_kernel() {
    naked_asm!(
        "call {kernel_resume_sp}",
        "mv sp, a0",
        // Restore callee-saved registers
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
        kernel_resume_sp = sym crate::kernel::percpu::kernel_resume_sp,
    );
}

// `arch` is bin-only (stubbed out of the host lib crate), so these run on
// the kernel target in QEMU (`cargo test --bin ease`) via `#[test_case]`.
// Each drives a full U-mode excursion through `user_entry`/`resume_kernel`
// and checks how the kernel regained control via the per-hart exit reason.
#[cfg(all(test, feature = "test-user"))]
mod test {
    use super::user_entry;
    use crate::kernel::percpu::{ExitReason, user_exit_reason};
    use crate::user::{user_fault_test, user_return_test, user_test};

    // A user thread that leaves via the `EXIT` syscall returns cleanly.
    #[test_case]
    fn clean_exit_reports_exit() {
        user_entry(user_test);
        // Read immediately, before any yield: the reason is per-hart state.
        assert_eq!(user_exit_reason(), ExitReason::Exit);
    }

    // A user thread that simply returns (no explicit `ecall`) exits cleanly
    // via the `user_exit` shim that `user_entry` installs in `ra`.
    #[test_case]
    fn normal_return_reports_exit() {
        user_entry(user_return_test);
        assert_eq!(user_exit_reason(), ExitReason::Exit);
    }

    // A user thread that reads kernel memory takes a PMP access fault, which
    // the kernel recovers from (returns) rather than panicking.
    #[test_case]
    fn kernel_access_reports_fault() {
        user_entry(user_fault_test);
        assert_eq!(user_exit_reason(), ExitReason::Fault);
    }
}
