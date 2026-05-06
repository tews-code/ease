//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing, jump to main.

use core::arch::naked_asm;

use crate::arch::STACK_CANARY;

// The extern block and _start function go here
// # Safety
// Symbols are defined in the linker script and mark aligned addresses
unsafe extern "C" {
    static __hart0_stack_start: u8; // HART0 stack
    static __hart0_stack_top: u8;
    static __hart1_stack_start: u8; // HART1 stack
    static __hart1_stack_top: u8;
    static __bss_start: u8;
    static __bss_end: u8;
}

#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    naked_asm!(
        "csrr t0, mhartid",             // Read HARTID
        "bnez t0, hart1",               // Set up other HARTS

        // HART0 setup
        // Set the stack and canary into SRAM4
        "la sp, {hart0_stack_top}",
        "la t0, {hart0_stack_start}",
        "li a0, {canary}",
        "sw a0, 0(t0)",

        // Zero BSS segment
        "la t0, {bss_start}",
        "la t1, {bss_end}",
        "1:",
        "bge t0, t1, 2f",       // If t0 >= t1, jump to 2
        "sw zero, 0(t0)",
        "addi t0, t0, 4",       // A word is 4 bytes
        "j 1b",                 // "b" means jump backward
        "2:",

        // Set trap vector for HART0
        "la t0, _trap_vector",
        "csrw mtvec, t0",

        "j main",

        // Set up HART1
        "hart1:",
        // Set up stack and canary
        "la sp, {hart1_stack_top}",
        "la t0, {hart1_stack_start}",
        "li a0, {canary}",
        "sw a0, 0(t0)",

        // Set trap vector for HART1
        "la t0, _trap_vector",
        "csrw mtvec, t0",
        "j secondary_main",

        "unimp",
        hart0_stack_start = sym __hart0_stack_start,
        hart0_stack_top = sym __hart0_stack_top,
        hart1_stack_start = sym __hart1_stack_start,
        hart1_stack_top = sym __hart1_stack_top,
        bss_start = sym __bss_start,
        bss_end = sym __bss_end,
        canary = const STACK_CANARY,
    );
}
