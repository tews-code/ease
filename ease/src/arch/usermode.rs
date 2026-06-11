//! User mode

use core::arch::naked_asm;

use crate::arch::csr::mstatus;
use crate::kernel::sched::{self, post_switch_cleanup};
use crate::sched::ExitReason;

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

// Entered via trap return from exit_from_user; a0 carries the ExitReason discriminant
pub(crate) extern "C" fn user_thread_exit(reason: usize) -> ! {
    let exit_reason = match reason {
        0 => ExitReason::Exit,
        1 => ExitReason::Fault,
        _ => panic!("unknown user thread exit reason"),
    };
    sched::exit(exit_reason);
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub(crate) extern "C" fn resume_kernel() {
    naked_asm!(
        "call {take_kernel_resume_sp}",
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
        take_kernel_resume_sp = sym crate::kernel::percpu::take_kernel_resume_sp,
    );
}

// Sets up user thread for first run
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
        "lw t0,  4 * 32(sp)",
        "csrw mscratch, t0",

        // Set up GP registers
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
        "addi sp, sp, 4 * {num_slots}",

        // Swap kernel sp with mscratch (user sp)
        "csrrw sp, mscratch, sp",

        // Ensure .text is ready for execution
        "fence.i",

        "mret",
        post_switch_cleanup = sym post_switch_cleanup,
        num_slots = const crate::arch::trap::NUM_SLOTS,
    );
}

// `arch` is bin-only (stubbed out of the host lib crate), so these run on
// the kernel target in QEMU (`cargo test --bin ease`) via `#[test_case]`.
// Each drives a full U-mode excursion through `user_entry`/`resume_kernel`
// and checks how the kernel regained control via the per-hart exit reason.
#[cfg(all(test, feature = "test-user"))]
mod test {
    use super::user_entry;
    use crate::kernel::percpu::user_exit_reason;
    use crate::kernel::sched::ExitReason;
    use crate::user::{user_fault_test, user_return_test, user_test};

    // These tests drive `user_entry` synchronously rather than going through
    // the scheduler's `activate_thread`, so nothing installs a per-thread PMP
    // map for them. Set one up by hand, mapping exactly two regions:
    //   - `.user_text` execute-only (X, no R): user code can run but cannot
    //     read itself as data. `kernel_access_reports_fault` depends on this —
    //     its load of `__user_text_start` must fault.
    //   - the PD0 heap R+W: `user_entry` parks the user `sp` at `__heap_pd0_end`.
    // Nothing else is mapped, so any other U-mode access faults.
    //
    // NOTE: this maps `.user_text` X-only, but the scheduler's `Role::Text`
    // (usermemmap.rs) currently grants `R | X` — see the message accompanying
    // this change; the two should be reconciled when the user-thread memory
    // model is designed properly.
    fn setup_user_excursion_pmp() {
        unsafe extern "C" {
            static __user_text_start: u8;
            static __user_text_end: u8;
            static __heap_pd0_start: u8;
            static __heap_pd0_end: u8;
        }
        use crate::arch::csr::pmp::{NAPOT, R, W, X};
        use crate::arch::pmp::Pmp;

        // Initialise a user memory map - copy .text, .data and zero .bss
        crate::kernel::sched::usermemmap::UserMemMap::load_user_image();

        let text_base = &raw const __user_text_start as usize;
        let text_size = &raw const __user_text_end as usize - text_base;
        let heap_base = &raw const __heap_pd0_start as usize;
        let heap_size = &raw const __heap_pd0_end as usize - heap_base;

        // Regions 0 and 1 are reserved for the locked M-mode scratch-text
        // guards, so user regions start at 2 (matching `Role::addr_slot`).
        let mut pmp = Pmp::new();
        pmp.set_region(2, text_base, text_size, NAPOT, X);
        pmp.set_region(3, heap_base, heap_size, NAPOT, R | W);
        pmp.activate();
    }

    // A user thread that leaves via the `EXIT` syscall returns cleanly.
    #[test_case]
    fn clean_exit_reports_exit() {
        setup_user_excursion_pmp();
        user_entry(user_test);
        // Read immediately, before any yield: the reason is per-hart state.
        assert_eq!(user_exit_reason(), ExitReason::Exit);
    }

    // A user thread that simply returns (no explicit `ecall`) exits cleanly
    // via the `user_exit` shim that `user_entry` installs in `ra`.
    #[test_case]
    fn normal_return_reports_exit() {
        setup_user_excursion_pmp();
        user_entry(user_return_test);
        assert_eq!(user_exit_reason(), ExitReason::Exit);
    }

    // A user thread that reads kernel memory takes a PMP access fault, which
    // the kernel recovers from (returns) rather than panicking.
    #[test_case]
    fn kernel_access_reports_fault() {
        setup_user_excursion_pmp();
        user_entry(user_fault_test);
        assert_eq!(user_exit_reason(), ExitReason::Fault);
    }
}
