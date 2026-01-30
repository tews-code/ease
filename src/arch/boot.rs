//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing,   jump to main.

use core::arch::naked_asm;

// The extern block and _start function go here
// # Safety
// Symbols are always defined in the linker script and hence aligned
unsafe extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
}

#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    naked_asm!(
        "li sp, 0x80100000",
        // Zero BSS segment
        "la t0, {bss_start}",
        "la t1, {bss_end}",
        "1:",
        "bge t0, t1, 2f",       // If t0 >= t1, jump to 2
        "sw zero, 0(t0)",
        "addi t0, t0, 4",       // A word is 4 bytes
        "j 1b",                 // "b" means jump backward
        "2:",
        "j main",
        "unimp",
        bss_start = sym __bss_start,
        bss_end = sym __bss_end,
    );
}
