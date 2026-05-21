//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing, jump to main.

use core::arch::naked_asm;

use crate::arch::STACK_CANARY;

// The extern block and _start function go here
// # Safety
// Symbols are defined in the linker script and mark aligned addresses
unsafe extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
    static __data_start: u8;
    static __data_end: u8;
    static __data_lma: u8;

    static __hart0_stack_start: u8;
    static __hart0_stack_top: u8;
    static __hart0_percpu_start: u8;
    static __hart0_percpu_end: u8;
    static __hart0_percpu_lma: u8;

    static __hart1_stack_start: u8;
    static __hart1_stack_top: u8;
    static __hart1_percpu_start: u8;
    static __hart1_percpu_end: u8;
    static __hart1_percpu_lma: u8;
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

        // Copy .data from LMA to VMA
        "la t0, {data_lma}",
        "la t1, {data_start}",
        "la t2, {data_end}",
        "1:",
        "bge t1, t2, 2f",
        "lw t3, 0(t0)",
        "sw t3, 0(t1)",
        "addi t0, t0, 4",
        "addi t1, t1, 4",
        "j 1b",
        "2:",

        // Copy PerCpu from LMA to VMA - HART0
        "la t0, {hart0_percpu_lma}",
        "la t1, {hart0_percpu_start}",
        "la t2, {hart0_percpu_end}",
        "3:",
        "bge t1, t2, 4f",
        "lw t3, 0(t0)",
        "sw t3, 0(t1)",
        "addi t0, t0, 4",
        "addi t1, t1, 4",
        "j 3b",
        "4:",

        // Copy PerCpu from LMA to VMA - HART1
        "la t0, {hart1_percpu_lma}",
        "la t1, {hart1_percpu_start}",
        "la t2, {hart1_percpu_end}",
        "5:",
        "bge t1, t2, 6f",
        "lw t3, 0(t0)",
        "sw t3, 0(t1)",
        "addi t0, t0, 4",
        "addi t1, t1, 4",
        "j 5b",
        "6:",

        // Zero BSS segment
        "la t0, {bss_start}",
        "la t1, {bss_end}",
        "7:",
        "bge t0, t1, 8f",
        "sw zero, 0(t0)",
        "addi t0, t0, 4",       // A word is 4 bytes
        "j 7b",                 // "b" means jump backward
        "8:",

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
        data_start = sym __data_start,
        data_end = sym __data_end,
        data_lma = sym __data_lma,
        hart0_percpu_start = sym __hart0_percpu_start,
        hart0_percpu_end = sym __hart0_percpu_end,
        hart0_percpu_lma = sym __hart0_percpu_lma,
        hart1_percpu_start = sym __hart1_percpu_start,
        hart1_percpu_end = sym __hart1_percpu_end,
        hart1_percpu_lma = sym __hart1_percpu_lma,
        bss_start = sym __bss_start,
        bss_end = sym __bss_end,
        canary = const STACK_CANARY,
    );
}
