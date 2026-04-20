ENTRY(_start) /* For ELF metadata e.g. debugger */

/*
 * Memory layout for QEMU `virt` machine
 *
 * We are modeling an RP2350 microcontroller with 520KB SRAM and 8MB PSRAM,
 * some of which is assigned to a framebuffer. QEMU is configured to provide
 * 32MB of physical RAM starting at 0x80000000.
 * We declare only 520KB as our working SRAM region. PSRAM is placed
 * at a separate address (0x81000000) to model a separate PSRAM region,
 * similar to real hardware (see e.g. Adafruit Metro RP2350 with PSRAM).
 *
 * 0x80000000  +--------------------+
 *             | .text              |
 *             | .rodata / .srodata |
 *             | .data / .sdata     |
 *             | .bss / .sbss       |
 *             | heap -->           |  Grows up (bump allocator, bounded by __heap_end)
 *             |                    |
 *             |        <-- stack   |  Grows down from __stack_top (64KB reserved)
 * 0x80082000  +--------------------+  End of declared SRAM (520KB)
 *             :   (unused gap)     :
 * 0x81000000  +--------------------+
 *             | PSRAM (8MB)        |
 *             |                    |
 * 0x816d4000  | 640x480x4 fb       | Framebuffer configured via QEMU ramfb
 * 0x81800000  +--------------------+ End of declared PSRAM
 *
 * The heap cannot grow past __heap_end (enforced by the bump allocator).
 * The gap between SRAM and PSRAM is backed by QEMU's physical RAM but is
 * not used.
 */

MEMORY {
    SRAM : ORIGIN = 0x80000000, LENGTH = 0x00082000 /* 520 KB SRAM on RP2350, not power 2 */
    PSRAM : ORIGIN = 0x81000000, LENGTH = 0x00800000 /* 8MB PSRAM */
}

__stack_top = 0x80082000; /* Must be 16-byte aligned (needed for RISC-V function entry) */
__psram_start = 0x81000000;
__psram_end = 0x81800000;
__fb_size   = 640 * 480 * 4;   /* 640  x 480 x 4 bytes = 1.2MiB */
__fb_addr   = 0x81800000 - __fb_size;


SECTIONS {
    .text : {
        *(.text.init)
        *(.text .text.*)
    } > SRAM

    .rodata : { *(.rodata .rodata.* .srodata .srodata.*) } > SRAM

    .data : {
        *(.data .data.*)
         PROVIDE(__global_pointer$ = . + 0x800); /* Small variable gp pointer address at + 2KB */
         *(.sdata .sdata.*)
    } > SRAM

    .bss : { 
        . = ALIGN(4);
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.*)
        . = ALIGN(4);
        __bss_end = .;
    } > SRAM

    .heap (NOLOAD) : ALIGN(4096) { /* For buddy allocator need heap to be aligned to laegest alloc size */
        __heap_start = .;
        . = . + 256K;
        __heap_end = .;
    } > SRAM

    /* Add stack guard with 4 bytes reserved */
    .stack_guard (NOLOAD) : {
        __stack_guard = .;
        . = . + 4;
    } > SRAM

    /DISCARD/ : { *(.comment) } /* Discard comment strings to keep binary small */
    /* Note - .eh_frame is not emitted by LLVM for target riscv32imac-unknown-none-elf (panic=abort) */
}

ASSERT(__fb_addr >= ORIGIN(PSRAM), "framebuffer below PSRAM")
ASSERT(__fb_addr + __fb_size <= ORIGIN(PSRAM) + LENGTH(PSRAM), "framebuffer exceeds PSRAM")
ASSERT(__heap_end <= __stack_top, "heap overlaps stack region")
ASSERT(__heap_end <= __psram_start, "heap overlaps psram region")
ASSERT(__stack_guard + 4 <= __stack_top, "SRAM sections overflow into stack")
