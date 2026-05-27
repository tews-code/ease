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
 *             | .user_text         |   Must be NAPOT for PMP
 *             +--------------------+
 *             | .text              |
 *             | .rodata / .srodata |
 *             | .data / .sdata     |   LMA for .data
 * 0x2100_0000 +--------------------+
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
 *             |  idle stack hart0  |   2KB
 *             +--------------------+
 *             |  idle stack hart1  |   2KB
 *             +--------------------+
 *             |    -- 128KB --     |
 *             | kernel heap -->    |   128KB and grows up
 *             |                    |
 *             |                    |
 * 0x8008_0000 +--------------------+   Also in Power Domain 1
 *             |SRAM8: HART0 scratch|   Split into text, IRQ stack, per-cpu data
 * 0x80081000  +--------------------+   Also in Power Domain 1
 *             |SRAM9: HART1 scratch|   Split into text, IRQ stack, per-cpu data
 * 0x80082000  +--------------------+   End of declared SRAM (520KiB)
 *             :                    :
 *             :   (unused gap)     :   Backed by QEMU RAM, but unused
 *             :                    :
 * 0x81000000  +--------------------+   Represents Adafruit Metro PSRAM (8MB)
 *             |  user heap ->      |
 *             |                    |
 *             |     -- 4MB --      |   End of user heap
 *             |  buffers           |
 *             |                    |
 * 0x816d4000  |  640x480x4 fb      |   Framebuffer configured via QEMU ramfb
 * 0x81800000  +--------------------+   End of declared PSRAM
 *
 *
 *  Scratch RAM is split into
 *
 *             +--------------------+
 *             |   .text    ~2KB+   |   .text is switch_to, preempt trampoline, mark_for_preempt
 *             +--------------------+
 *             |   IRQ stack 1.5KB  |
 *             +--------------------+
 *             |    per cpu ~64B    |
 *             +--------------------+
 */

MEMORY {
    FLASH (rx)      : ORIGIN = 0x20000000, LENGTH = 0x01000000 /* 16 MB */
    SRAM_PD0(rw)    : ORIGIN = 0x80000000, LENGTH = 0x00040000 /* SRAM0-3 - 256KB */
    SRAM_PD1 (rwx)  : ORIGIN = 0x80040000, LENGTH = 0x00040000 /* SRAM4-7 - 256KB */
    SRAM8 (rwx)     : ORIGIN = 0x80080000, LENGTH = 0x00001000 /* HART0 4KB scratch RAM */
    SRAM9 (rwx)     : ORIGIN = 0x80081000, LENGTH = 0x00001000 /* HART1 4KB scratch RAM */
    PSRAM (rw)      : ORIGIN = 0x81000000, LENGTH = 0x00800000 /* 8MB */
}

__user_text_size        = 4K;
__idle_stack_size       = 2K;
__irq_stack_size        = 1K + 512;
__kernel_heap_size      = 128K;
__user_heap_sram_size   = 256K;
__user_heap_psram_size  = 4M;
__fb_width = 640; __fb_height = 480; __fb_bpp = 4; /* 640  x 480 x 4 bytes = 1.2MiB */


__sram_pd1_end  = ORIGIN(SRAM_PD1) + LENGTH(SRAM_PD1);
__psram_end     = ORIGIN(PSRAM) + LENGTH(PSRAM);
__fb_size       = __fb_width * __fb_height * __fb_bpp;
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

    .user_text : ALIGN(__user_text_size) {
        __user_text_start = .;
        *(.user_text .user_text.*)
        . = __user_text_start + __user_text_size;
        __user_text_end = .;
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

    .hart0_idle_stack (NOLOAD) : ALIGN(16) {
        __hart0_idle_stack_base  = .;
        . = . + __idle_stack_size;
        __hart0_idle_stack_top = .;
    } > SRAM_PD1

    .hart1_idle_stack (NOLOAD) : ALIGN(16) {
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
    .sram8_text : ALIGN(4) {
        __sram8_text_start = .;
        *(.sram8_text .sram8_text.*)
        . = ALIGN(4);
        __sram8_text_end = .;
    } > SRAM8 AT > FLASH
    __sram8_text_lma = LOADADDR(.sram8_text);

    .sram8_irq_stack (NOLOAD) : ALIGN(16) {
        __hart0_irq_stack_base = .;
        . = . + __irq_stack_size;
        __hart0_irq_stack_top = .;
    } > SRAM8

    .sram8_percpu (NOLOAD) : ALIGN(8) {
        __hart0_percpu_start = .;
        *(.sram8_percpu .sram8_percpu.*)
        __hart0_percpu_end = .;
    } > SRAM8

    /* SRAM9 */

    /* SRAM9 is the dedicated HART1 scratch RAM */
    .sram9_text : ALIGN(4) {
        __sram9_text_start = .;
        *(.sram9_text .sram9_text.*)
        . = ALIGN(4);
        __sram9_text_end = .;
    } > SRAM9 AT > FLASH
    __sram9_text_lma = LOADADDR(.sram9_text);

    .sram9_irq_stack (NOLOAD) : ALIGN(16) {
        __hart1_irq_stack_base = .;
        . = . + __irq_stack_size;
        __hart1_irq_stack_top = .;
    } > SRAM9

    .sram9_percpu (NOLOAD) : ALIGN(8) {
        __hart1_percpu_start = .;
        *(.sram9_percpu .sram9_percpu.*)
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

    /* Remaining PSRAM up to the frame buffer is a region for buffers etc. */
    .psram_buf (NOLOAD) : ALIGN(4) {
        __psram_buf_start = .;
        *(.psram_buf .psram_buf.*)
        __psram_buf_end = .;
    } > PSRAM

    /* Framebuffer */
    .framebuffer __fb_addr (NOLOAD) : ALIGN(4) {
        *(.fb_buf .fb_buf.*)
    } > PSRAM

    /* Discard .eh_frame as we are using panic = "abort" */
    /DISCARD/ : { *(.comment) *(.eh_frame_hdr) *(.eh_frame)} /* Discard comment strings to keep binary small */
}

ASSERT(__pd1_buf_end <= __hart0_idle_stack_base, "PD1 .data .bss and buffers overflow into HART0 idle stack")
ASSERT(__hart1_idle_stack_top <= __heap_pd1_start, "Idle stack overflows into kernel heap")
ASSERT(__heap_pd1_end == __sram_pd1_end, "kernel heap doesn't end at SRAM_PD1 boundary")
ASSERT(__hart0_percpu_end <= ORIGIN(SRAM8) + LENGTH(SRAM8), "SRAM8 overflow")
ASSERT(__hart1_percpu_end <= ORIGIN(SRAM9) + LENGTH(SRAM9), "SRAM9 overflow")
ASSERT(__psram_buf_end <= __fb_addr, "psram_buf overflows in the framebuffer")
ASSERT(__fb_addr >= ORIGIN(PSRAM), "framebuffer below PSRAM")
ASSERT(__fb_addr + __fb_size <= ORIGIN(PSRAM) + LENGTH(PSRAM), "framebuffer exceeds PSRAM")
