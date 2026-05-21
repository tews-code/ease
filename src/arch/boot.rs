//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing, jump to main.

use core::arch::{global_asm, naked_asm};

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

    static __sram8_text_start: u8;
    static __sram8_text_end: u8;
    static __sram8_text_lma: u8;

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

global_asm!(
    r#"
    .section .text
    .global _copy_section
    .align 4
    _copy_section:
        # Copy word by word from LMA to VMA
        # a0 = LMA, a1 = VMA start, a2 = VMA end
        # Clobbers a0, a1, a3
        1:
        bge a1, a2, 2f
        lw a3, 0(a0)
        sw a3, 0(a1)
        addi a0, a0, 4
        addi a1, a1, 4
        j 1b
        2:
        ret
        "#
);

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
        "la a0, {data_lma}",
        "la a1, {data_start}",
        "la a2, {data_end}",
        "jal _copy_section",

        // Copy .sram8_text from LMA to VMA
        "la a0, {sram8_text_lma}",
        "la a1, {sram8_text_start}",
        "la a2, {sram8_text_end}",
        "jal _copy_section",

        // Copy PerCpu from LMA to VMA - HART0
        "la a0, {hart0_percpu_lma}",
        "la a1, {hart0_percpu_start}",
        "la a2, {hart0_percpu_end}",
        "jal _copy_section",

        // Copy PerCpu from LMA to VMA - HART1
        "la a0, {hart1_percpu_lma}",
        "la a1, {hart1_percpu_start}",
        "la a2, {hart1_percpu_end}",
        "jal _copy_section",

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
        sram8_text_lma = sym __sram8_text_lma,
        sram8_text_start = sym __sram8_text_start,
        sram8_text_end = sym __sram8_text_end,
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
