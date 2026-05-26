//! Boot code for QEMU virt machine
//!
//! Contains early startup: stack init, BSS zeroing, jump to main.

use core::arch::{global_asm, naked_asm};

use crate::arch::stack::{STACK_CANARY, STACK_PAINT_PATTERN};

// The extern block and _start function go here
// # Safety
// Symbols are defined in the linker script and mark aligned addresses
unsafe extern "C" {
    static __data_start: u8;
    static __data_end: u8;
    static __data_lma: u8;
    static __bss_start: u8;
    static __bss_end: u8;

    static __sram8_text_start: u8;
    static __sram8_text_end: u8;
    static __sram8_text_lma: u8;

    static __hart0_irq_stack_base: u8;
    static __hart0_irq_stack_top: u8;
    static __hart0_idle_stack_base: u8;
    static __hart0_idle_stack_top: u8;
    static __hart0_percpu_start: u8;
    static __hart0_percpu_end: u8;

    static __sram9_text_start: u8;
    static __sram9_text_end: u8;
    static __sram9_text_lma: u8;

    static __hart1_irq_stack_base: u8;
    static __hart1_irq_stack_top: u8;
    static __hart1_idle_stack_base: u8;
    static __hart1_idle_stack_top: u8;
    static __hart1_percpu_start: u8;
    static __hart1_percpu_end: u8;
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

global_asm!(
    r#"
    .section .text
    .global _fill_section
    .align 4
    _fill_section:
        # Fill a section with the same word
        # a0 = word, a1 = start, a2 = end
        # Clobbers a1
        1:
        bge a1, a2, 2f
        sw a0, 0(a1)
        addi a1, a1, 4
        j 1b
        2:
        ret
        "#
);

global_asm!(
    r#"
    .section .text
    .global _paint_stack
    .align 4
    _paint_stack:
        # Paint a stack section with canary at base and pattern
        # a0 = canary, a1 = pattern, a2 = section start, a3 = section end
        # Clobbers a2
        bge a2, a3, 2f
        sw a0, 0(a2)
        addi a2, a2, 4
        1:
        bge a2, a3, 2f
        sw a1, 0(a2)
        addi a2, a2, 4
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
        // Set the idle stack into SRAM8
        "la sp, {hart0_idle_stack_top}",
        // Paint the idle stack
        "li a0, {canary}",
        "li a1, {pattern}",
        "la a2, {hart0_idle_stack_base}",
        "la a3, {hart0_idle_stack_top}",
        "jal _paint_stack",

        // Paint the IRQ stack
        "li a0, {canary}",
        "li a1, {pattern}",
        "la a2, {hart0_irq_stack_base}",
        "la a3, {hart0_irq_stack_top}",
        "jal _paint_stack",

        // Copy .data from LMA to VMA
        "la a0, {data_lma}",
        "la a1, {data_start}",
        "la a2, {data_end}",
        "jal _copy_section",

        // Zero BSS segment
        "li a0, 0",
        "la a1, {bss_start}",
        "la a2, {bss_end}",
        "jal _fill_section",

        // Copy .sram8_text from LMA to VMA
        "la a0, {sram8_text_lma}",
        "la a1, {sram8_text_start}",
        "la a2, {sram8_text_end}",
        "jal _copy_section",

        // Zero PerCpu for HART0
        "li a0, 0",
        "la a1, {hart0_percpu_start}",
        "la a2, {hart0_percpu_end}",
        "jal _fill_section",

        // Set trap vector for HART0
        "la t0, _trap_vector_h0",
        "csrw mtvec, t0",

        // Store the IRQ stack top in mscratch
        "la t0, {hart0_irq_stack_top}",
        "csrw mscratch, t0",

        "j main",

        // Set up HART1
        "hart1:",
        // Set idle stack in SRAM9
        "la sp, {hart1_idle_stack_top}",

        // Paint the idle stack
        "li a0, {canary}",
        "li a1, {pattern}",
        "la a2, {hart1_idle_stack_base}",
        "la a3, {hart1_idle_stack_top}",
        "jal _paint_stack",

        // Paint the IRQ stack
        "li a0, {canary}",
        "li a1, {pattern}",
        "la a2, {hart1_irq_stack_base}",
        "la a3, {hart1_irq_stack_top}",
        "jal _paint_stack",

        // Copy .sram9_text from LMA to VMA
        "la a0, {sram9_text_lma}",
        "la a1, {sram9_text_start}",
        "la a2, {sram9_text_end}",
        "jal _copy_section",

        // Zero PerCpu for HART1
        "li a0, 0",
        "la a1, {hart1_percpu_start}",
        "la a2, {hart1_percpu_end}",
        "jal _fill_section",

        // Set trap vector for HART1
        "la t0, _trap_vector_h1",
        "csrw mtvec, t0",

        // Store the IRQ stack top in mscratch
        "la t0, {hart1_irq_stack_top}",
        "csrw mscratch, t0",

        "j secondary_main",

        "unimp",
        sram8_text_lma = sym __sram8_text_lma,
        sram8_text_start = sym __sram8_text_start,
        sram8_text_end = sym __sram8_text_end,

        hart0_irq_stack_base = sym __hart0_irq_stack_base,
        hart0_irq_stack_top = sym __hart0_irq_stack_top,
        hart0_idle_stack_base = sym __hart0_idle_stack_base,
        hart0_idle_stack_top = sym __hart0_idle_stack_top,
        hart0_percpu_start = sym __hart0_percpu_start,
        hart0_percpu_end = sym __hart0_percpu_end,

        hart1_irq_stack_base = sym __hart1_irq_stack_base,
        hart1_irq_stack_top = sym __hart1_irq_stack_top,
        hart1_idle_stack_base = sym __hart1_idle_stack_base,
        hart1_idle_stack_top = sym __hart1_idle_stack_top,
        hart1_percpu_start = sym __hart1_percpu_start,
        hart1_percpu_end = sym __hart1_percpu_end,

        sram9_text_lma = sym __sram9_text_lma,
        sram9_text_start = sym __sram9_text_start,
        sram9_text_end = sym __sram9_text_end,

        data_start = sym __data_start,
        data_end = sym __data_end,
        data_lma = sym __data_lma,

        bss_start = sym __bss_start,
        bss_end = sym __bss_end,

        canary = const STACK_CANARY,
        pattern = const STACK_PAINT_PATTERN,
    );
}
