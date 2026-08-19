ENTRY(_start) /* For ELF metadata e.g. debugger */

/*
 * Memory layout for QEMU `virt` machine
 *
 * We are modeling an AdaFruit Metro RP2350 with 8MB PSRAM and 16MB flash.
 * This board contains a RP2350 microcontroller with 520KB SRAM and 8MB PSRAM,
 * QEMU is configured to provide 32MB of physical RAM starting at 0x80000000, but
 * we declare only 520KiB as our working SRAM region. In order to model a separate
 * PSRAM region, it is placed at a separate address 0x81000000
 * Flash memory is used to replicate XIP.
 *
 * 0x2000_0000 +--------------------+   Represents Adafruit Metro 16 MB flash which supports XIP
 *             | .text              |
 *             | .rodata / .srodata |
 *             | .data / .sdata     |   LMA for .data
 * 0x2100_0000 +--------------------+
 *             :                    :
 *             :    (unused gap)    :   QEMU virt requires 32MB but this is unused
 *             :                    :
 * 0x2200_0000 +--------------------+
 *
 * 0x8000_0000 +--------------------+   Represents RP2350 256KiB SRAM0-3 - Power Domain 0
 *             | user heap -->      |
 *             |                    |
 *             +--------------------+
 * 0x8004_0000 +--------------------+   Represents RP2350 256KiB SRAM4-7 - Power Domain 1
 *             | .data / .sdata     |   VMA for .data
 *             | .bss / .sbss       |
 *             |  buffers           |
 *             +--------------------+
 *             |  idle stack hart0  |   2KB; doubles as boot stack
 *             +--------------------+
 *             |  idle stack hart1  |   2KB; doubles as boot stack
 *             +--------------------+
 *             |    -- 128KB --     |
 *             | kernel heap -->    |   128KB and grows up
 *             |                    |
 *             |                    |
 * 0x8008_0000 +--------------------+
 *             |SRAM8: HART0 scratch|   Also in Power Domain 1
 * 0x80081000  +--------------------+
 *             |SRAM9: HART1 scratch|   Also in Power Domain 1
 * 0x80082000  +--------------------+   End of declared SRAM (520KiB)
 *             :                    :
 *             :   (unused gap)     :   Backed by QEMU RAM, but unused (QEMU virt only supports one RAM region)
 *             :                    :
 * 0x81000000  +--------------------+   Represents Adafruit Metro PSRAM (8MB)
 *             |  PSRAM heap ->     |
 *             |    (4MB)           |
 *             +--------------------+   End of heap
 *             | .user_text (4KB)   |   Temporary for user processes set at compile time; Must be NAPOT for PMP
 *             +--------------------+
 *             | .user_data/bss(4KB)|   Temporary for user processes set at compile time; Must be NAPOT for PMP
 *             +--------------------+
 *             |  buffers           |
 *             +--------------------+
 * 0x816d4000  |  640x480x4 fb      |   Framebuffer configured via QEMU ramfb
 * 0x81800000  +--------------------+   End of declared PSRAM
 *
 *
 *  Scratch RAM is split into
 *
 *             +--------------------+
 *             |   .text      2KB   |   .text is switch_to, preempt trampoline, mark_for_preempt
 *             +--------------------+   It is NAPOT 2KB to allow PMP M-mode to protect from writes
 *             |   IRQ stack 1.5KB  |
 *             +--------------------+
 *             |    per cpu ~64B    |
 *             +--------------------+
 */

MEMORY {
    FLASH       : ORIGIN = 0x20000000, LENGTH = 0x01000000 /* 16 MB */
    SRAM_PD0    : ORIGIN = 0x80000000, LENGTH = 0x00040000 /* SRAM0-3 - 256KB */
    SRAM_PD1    : ORIGIN = 0x80040000, LENGTH = 0x00040000 /* SRAM4-7 - 256KB */
    SRAM8       : ORIGIN = 0x80080000, LENGTH = 0x00001000 /* HART0 4KB scratch RAM */
    SRAM9       : ORIGIN = 0x80081000, LENGTH = 0x00001000 /* HART1 4KB scratch RAM */
    PSRAM       : ORIGIN = 0x81000000, LENGTH = 0x00800000 /* 8MB */
}

/* Include the user memory definitions that are shared between user programs and the OS */
INCLUDE memory-shared-qemu.x

__idle_stack_size       = 2K;
__irq_stack_size        = 1K + 512;
__scratch_ram_text_size = 2K;
__kernel_heap_size      = 128K;
__fb_width = 640; __fb_height = 480; __fb_bytes_pp = 4; /* 640  x 480 x 4 bytes = ~1.2MiB */

__sram_pd1_end  = ORIGIN(SRAM_PD1) + LENGTH(SRAM_PD1);
__psram_end     = ORIGIN(PSRAM) + LENGTH(PSRAM);
__fb_size       = __fb_width * __fb_height * __fb_bytes_pp;
__fb_addr       = __psram_end - __fb_size;

SECTIONS {
    /* FLASH */

    .text : {
        KEEP(*(.text.init))
        *(.text .text.*)
    } > FLASH

    .rodata : {
        *(.rodata .rodata.* .srodata .srodata.*)
        . = ALIGN(4);   /* Padding .rodata so that .data start from VMA copy is aligned */
    } > FLASH

    /* POWER DOMAIN 0 */

    /* Power Domain 0 used as a single memory region across SRAM0-3 */
    /* Used for user heap which needs NAPOT given PMP requirements */
    .heap_pd0 (NOLOAD) : ALIGN(__user_heap_sram_size) {
        __heap_pd0_start = .;
        . = . + __user_heap_sram_size;
        __heap_pd0_end = .;
    } > SRAM_PD0

    /* POWER DOMAIN 1 */

    /* Power Domain 1 used for kernel memory */
    /* VMA .data .bss, buffers, idle stacks and kernel heap across SRAM4-7 */
    .data : ALIGN(4) {
        __data_start = .; /* Note: No gp use for LLVM for RISC-V so do not PROVIDE */
        *(.data .data.*) *(.sdata .sdata.*)
        . = ALIGN(4);   /* Padding .data so that .bss start is aligned */
        __data_end = .;
    } > SRAM_PD1 AT > FLASH
    __data_lma = LOADADDR(.data);

    .bss : {
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.*)
        . = ALIGN(4);
        __bss_end = .;
    } > SRAM_PD1

    .pd1_buf (NOLOAD) : {
        __pd1_buf_start = .;
        *(.pd1_buf .pd1_buf.*)
        __pd1_buf_end = .;
    } > SRAM_PD1

    .hart0_idle_stack (NOLOAD) : ALIGN(__idle_stack_size) {
        __hart0_idle_stack_base  = .;
        . = . + __idle_stack_size;
        __hart0_idle_stack_top = .;
    } > SRAM_PD1

    .hart1_idle_stack (NOLOAD) : ALIGN(__idle_stack_size) {
        __hart1_idle_stack_base = .;
        . = . + __idle_stack_size;
        __hart1_idle_stack_top = .;
    } > SRAM_PD1

    .heap_pd1 (NOLOAD) : ALIGN(__kernel_heap_size) { /* For buddy allocator need heap to be aligned to size and power-of-two*/
        __heap_pd1_start = .;
        . = . + __kernel_heap_size;   /* Size and alignment pushes kernel heap to be the last 128 KB of kernel SRAM */
        __heap_pd1_end = .;
    } > SRAM_PD1

    /* SRAM8 */

    /* SRAM8 is the dedicated HART0 scratch RAM */
    .sram8_text : ALIGN(__scratch_ram_text_size) {
        __sram8_text_start = .;
        *(.sram8_text .sram8_text.*)
        . = __sram8_text_start + __scratch_ram_text_size;
        __sram8_text_end = .;
    } > SRAM8 AT > FLASH
    __sram8_text_lma = LOADADDR(.sram8_text);

    .sram8_irq_stack (NOLOAD) : ALIGN(16) {
        __hart0_irq_stack_base = .;
        . = . + __irq_stack_size;
        __hart0_irq_stack_top = .;
    } > SRAM8

    .sram8_percpu (NOLOAD) : ALIGN(4) {
        __hart0_percpu_start = .;
        *(.sram8_percpu .sram8_percpu.*)
        . = ALIGN(4);   /* Zero region in boot assembly requires 4 byte alignment */
        __hart0_percpu_end = .;
    } > SRAM8

    /* SRAM9 */

    /* SRAM9 is the dedicated HART1 scratch RAM */
    .sram9_text : ALIGN(__scratch_ram_text_size) {
        __sram9_text_start = .;
        *(.sram9_text .sram9_text.*)
        . = __sram9_text_start + __scratch_ram_text_size;
        __sram9_text_end = .;
    } > SRAM9 AT > FLASH
    __sram9_text_lma = LOADADDR(.sram9_text);

    .sram9_irq_stack (NOLOAD) : ALIGN(16) {
        __hart1_irq_stack_base = .;
        . = . + __irq_stack_size;
        __hart1_irq_stack_top = .;
    } > SRAM9

    .sram9_percpu (NOLOAD) : ALIGN(4) {
        __hart1_percpu_start = .;
        *(.sram9_percpu .sram9_percpu.*)
        . = ALIGN(4);   /* Zero region in boot assembly requires 4 byte alignment */
        __hart1_percpu_end = .;
    } > SRAM9

    /* PSRAM */

    /* First part of PSRAM is used for a user heap */
    /* Note user heap needs to be NAPOT for RP2350 PMP requirements */
    .heap_psram (NOLOAD) : ALIGN(__user_heap_psram_size) {
        __heap_psram_start = .;
        . = . + __user_heap_psram_size;
        __heap_psram_end = .;
    } > PSRAM

    /* Temporarily put all user threads .text in 4KB window in PSRAM */
    .user_text __user_text_origin : {
        __user_text_start = .;
        *(.user_text .user_text.*)
        . = __user_text_start + __user_text_size;
        __user_text_end = .;
    } > PSRAM AT > FLASH
    __user_text_lma = LOADADDR(.user_text);

    /* Temporarily put all user threads .data and .bss in 4KB window in PSRAM */
    .user_data __user_data_origin : {
        __user_data_bss_start = .;
        __user_data_start = .;
        *(.user_data .user_data.*)
        . = __user_data_start + __user_data_size;
        __user_data_end = .;
    } > PSRAM AT > FLASH
    __user_data_lma = LOADADDR(.user_data);

    .user_bss __user_bss_origin (NOLOAD) : {
        __user_bss_start = .;
        *(.user_bss .user_bss.*)
        . = __user_bss_start + __user_bss_size;
        __user_bss_end = .;
        __user_data_bss_end = .;
    } > PSRAM

    /* Remaining PSRAM up to the frame buffer is a region for buffers etc. */
    .psram_buf (NOLOAD) : ALIGN(4) {
        __psram_buf_start = .;
        *(.psram_buf .psram_buf.*)
        __psram_buf_end = .;
    } > PSRAM

    /* Framebuffer */
    .framebuffer __fb_addr (NOLOAD) : {
        *(.fb_buf .fb_buf.*)
    } > PSRAM

    /* Discard .eh_frame as we are using panic = "abort" */
    /DISCARD/ : { *(.comment) *(.eh_frame_hdr) *(.eh_frame)} /* Discard comment strings to keep binary small */
}

ASSERT(__heap_psram_end == __user_text_origin, "user .text is should be immediately after the heap PSRAM allocation")
ASSERT(__scratch_ram_text_size % 4  == 0, "scratch text size must be word-multiple (copy_region)")
ASSERT(__irq_stack_size        % 16 == 0, "irq stack size must be 16-multiple (paint + ABI sp)")
ASSERT(__idle_stack_size       % 16 == 0, "idle stack size must be 16-multiple (paint + ABI sp)")
ASSERT(__heap_pd1_end == __sram_pd1_end, "kernel heap doesn't end at SRAM_PD1 boundary")
ASSERT(__hart0_percpu_end <= ORIGIN(SRAM8) + LENGTH(SRAM8), "SRAM8 overflow")
ASSERT(__hart1_percpu_end <= ORIGIN(SRAM9) + LENGTH(SRAM9), "SRAM9 overflow")
ASSERT(__psram_buf_end <= __fb_addr, "psram_buf overflows into the framebuffer")
ASSERT(__fb_addr >= ORIGIN(PSRAM), "framebuffer below PSRAM")
ASSERT(__fb_addr + __fb_size <= ORIGIN(PSRAM) + LENGTH(PSRAM), "framebuffer exceeds PSRAM")
