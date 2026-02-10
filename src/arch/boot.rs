//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing,   jump to main.

use core::arch::naked_asm;

const MSTATUS_MIE: u32 = 0x8;

// The extern block and _start function go here
// # Safety
// Symbols are always defined in the linker script and hence aligned
unsafe extern "C" {
    static __stack_top: u8;
    static __bss_start: u8;
    static __bss_end: u8;
}

#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    naked_asm!(
        "la sp, {stack_top}",

        // Zero BSS segment
        "la t0, {bss_start}",
        "la t1, {bss_end}",
        "1:",
        "bge t0, t1, 2f",       // If t0 >= t1, jump to 2
        "sw zero, 0(t0)",
        "addi t0, t0, 4",       // A word is 4 bytes
        "j 1b",                 // "b" means jump backward
        "2:",

        // Set trap vector
        "la t0, _trap_vector",
        "csrw mtvec, t0",

        // Enable interrupts
        "csrsi mstatus, {mstatus_mie}",

        "j main",
        "unimp",
        stack_top = sym __stack_top,
        bss_start = sym __bss_start,
        bss_end = sym __bss_end,
        mstatus_mie = const MSTATUS_MIE,
    );
}
