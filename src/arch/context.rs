//! Context swap for co-operative scheduler
// On RP2350 the RISCV cores do not perform hardware caller register
// so keep a lightweight yield function

use core::arch::{global_asm, naked_asm};

// The RISC-V psABI splits regs into callee-saved and caller-saved,
// and the compiler has already preserved any caller-saved regs across
// our extern "C" call
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
    pub fn for_entry(trampoline_ptr: extern "C" fn(*mut u8) -> !, closure_ptr: *mut u8) -> Self {
        Self {
            ra: thread_entry as *const () as usize,
            s0: trampoline_ptr as usize,
            s1: closure_ptr as usize,
            ..Self::default()
        }
    }
}

#[unsafe(naked)]
// Safety: Reached via switch_to returning into a Context forged by
// spawn, with s0 = entry. Enables MIE then jumps to s0.
unsafe extern "C" fn thread_entry() {
    naked_asm!(
        "mv a0, s0",
        "mv a1, s1",
        "tail {trampoline}",
        trampoline = sym thread_first_run,
    );
}

#[unsafe(no_mangle)]
unsafe extern "C" fn thread_first_run(
    trampoline_ptr: extern "C" fn(*mut u8) -> !,
    closure_ptr: *mut u8,
) -> ! {
    crate::kernel::sched::post_switch_cleanup();
    crate::arch::enable_interrupts();
    trampoline_ptr(closure_ptr);
}

global_asm!(
    r#"
    .section .text
    .global switch_to
    .align 4
    switch_to:
        #Save callee-saved registers to the stack
        addi sp, sp, -4 * 16    # Need 16 byte alignment even for 13 regs
        sw ra,  4 *  0(sp)
        sw s0,  4 *  1(sp)
        sw s1,  4 *  2(sp)
        sw s2,  4 *  3(sp)
        sw s3,  4 *  4(sp)
        sw s4,  4 *  5(sp)
        sw s5,  4 *  6(sp)
        sw s6,  4 *  7(sp)
        sw s7,  4 *  8(sp)
        sw s8,  4 *  9(sp)
        sw s9,  4 * 10(sp)
        sw s10, 4 * 11(sp)
        sw s11, 4 * 12(sp)

        # Perform switch
        sw sp, 0(a0)
        lw sp, 0(a1)

        # Restore callee saved registers
        lw ra,  4 *  0(sp)
        lw s0,  4 *  1(sp)
        lw s1,  4 *  2(sp)
        lw s2,  4 *  3(sp)
        lw s3,  4 *  4(sp)
        lw s4,  4 *  5(sp)
        lw s5,  4 *  6(sp)
        lw s6,  4 *  7(sp)
        lw s7,  4 *  8(sp)
        lw s8,  4 *  9(sp)
        lw s9,  4 * 10(sp)
        lw s10, 4 * 11(sp)
        lw s11, 4 * 12(sp)
        addi sp, sp, 4 * 16

        ret
    "#
);

unsafe extern "C" {
    // Safety: caller must ensure prev points to a writable slot owned by the current
    // thread; next points to a slot containing a saved sp produced by a prior swap_to call or
    // by spawn's stack forging; calling with interrupts disabled is undefined;
    pub(crate) fn switch_to(prev_sp: *mut *mut u8, next_sp: *mut *mut u8);
}
