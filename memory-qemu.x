ENTRY(_start) /* For ELF metadata e.g. debugger */

/*
 * Memory layout for QEMU `virt` machine
 *
 * We are modeling an AdaFruit Metro RP2350 with 8MB PSRAM.
 * This board contians a RP2350 microcontroller with 520KB SRAM and 8MB PSRAM,
 * some of which is assigned to a framebuffer. QEMU is configured to provide
 * 32MB of physical RAM starting at 0x80000000.
 * We declare only 520KiB as our working SRAM region. PSRAM is placed
 * at a separate address (0x81000000) to model a separate PSRAM region,
 * similar to real hardware
 * Flash memory is used to replicate XIP.
 *
 * 0x2000_0000 +--------------------+   Represents Adafruit Metro 16 MB flash which supports XIP
 *             | .text              |
 *             | .rodata / .srodata |
 * 0x2100_0000 +--------------------+
 * 0x8000_0000 +--------------------+   Represents RP2350 520KiB SRAM
 *             |                    |
 *             | .data / .sdata     |   Data segment should be zero sized - no static mut
 *             |                    |   or non-zero initialised statics.
 *             | .bss / .sbss       |
 *             | heap -->           |  Grows up (bounded by __heap_end)
 *             |                    |
 *             |                    |
 *             |                    |
 * 0x80080000  +--------------------+
 *             | SRAM4 - HART0 stack|
 * 0x80081000  +--------------------+
 *             | SRAM5 - HART1 stack|
 * 0x80082000  +--------------------+  End of declared SRAM (520KiB)
 *             :   (unused gap)     :
 * 0x81000000  +--------------------+   Represents Adafruit Metro PSRAM (8MB)
 *             | PSRAM (8MB)        |
 *             |                    |
 * 0x816d4000  | 640x480x4 fb       | Framebuffer configured via QEMU ramfb
 * 0x81800000  +--------------------+ End of declared PSRAM
 *
 * The heap cannot grow past __heap_end (enforced by the global allocator).
 * The gap between SRAM and PSRAM is backed by QEMU's physical RAM but is
 * not used.
 */

MEMORY {
    FLASH : ORIGIN = 0x20000000, LENGTH = 0x01000000 /* 16 MB  on Adafruit Metro RP2350 */
    SRAM : ORIGIN = 0x80000000, LENGTH = 0x00082000 /* 520 KB SRAM on RP2350, not power 2 */
    PSRAM : ORIGIN = 0x81000000, LENGTH = 0x00800000 /* 8MB PSRAM */
}

__psram_start = 0x81000000;
__psram_end = 0x81800000;
__fb_size   = 640 * 480 * 4;   /* 640  x 480 x 4 bytes = 1.2MiB */
__fb_addr   = 0x81800000 - __fb_size;

SECTIONS {
    .text : {
        *(.text.init)
        *(.text .text.*)
    } > FLASH

    .rodata : { *(.rodata .rodata.* .srodata .srodata.*) } > FLASH

    /* Note: No gp use for LLVM for RISC-V so do not PROVIDE */
    .data : {
        *(.data .data.*)
         *(.sdata .sdata.*)
    } > SRAM

    .bss : { 
        . = ALIGN(4);
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.*)
        . = ALIGN(4);
        __bss_end = .;
    } > SRAM

    .heap (NOLOAD) : ALIGN(4096) { /* For buddy allocator need heap to be aligned to largest alloc size */
        __heap_start = .;
        . = . + 256K;
        __heap_end = .;
    } > SRAM

    /* Add dedicated SRAM4 for HART0 stack */
    .sram4 0x80080000 (NOLOAD) : {
        __hart0_stack_start = .;
        . = . + 4K;
        __hart0_stack_top = .;
    } > SRAM

    /* Add dedicated SRAM5 for HART1 stack */
    .sram5 0x80081000 (NOLOAD) : {
        __hart1_stack_start = .;
        . = . + 4K;
        __hart1_stack_top = .;
    } > SRAM

    /DISCARD/ : { *(.comment) *(.eh_frame)} /* Discard comment strings to keep binary small */
}

ASSERT(SIZEOF(.data) == 0, "non-empty .data not yet supported - add LMA copy logic")
ASSERT(__fb_addr >= ORIGIN(PSRAM), "framebuffer below PSRAM")
ASSERT(__fb_addr + __fb_size <= ORIGIN(PSRAM) + LENGTH(PSRAM), "framebuffer exceeds PSRAM")
ASSERT(__heap_end <= __hart0_stack_start, "heap overflows dedicated stacks")
