ENTRY(_start) /* For ELF metadata e.g. debugger */

/*
 * Memory layout for QEMU `virt` machine
 *
 * We are modeling an AdaFruit Metro RP2350 with 8MB PSRAM.
 * This board contians a RP2350 microcontroller with 520KB SRAM and 8MB PSRAM,
 * QEMU is configured to provide 32MB of physical RAM starting at 0x80000000, but
 * we declare only 520KiB as our working SRAM region. PSRAM is placed
 * at a separate address (0x81000000) to model a separate PSRAM region,
 * similar to real hardware.
 * Flash memory is used to replicate XIP.
 *
 * 0x2000_0000 +--------------------+   Represents Adafruit Metro 16 MB flash which supports XIP
 *             | .text              |
 *             | .rodata / .srodata |
 *             | .data / .sdata     |   LMA for .data
 * 0x2100_0000 +--------------------+
 *
 * 0x8000_0000 +--------------------+   Represents RP2350 256KiB SRAM0-3 - Power Domain 0
 *             | user heap -->      |
 *             |                    |
 * 0x8004_0000 +--------------------+   Represents RP2350 256KiB SRAM4-7 - Power Domain 1
 *             | .data / .sdata     |   VMA for .data
 *             | .bss / .sbss       |
 *             |  buffers           |
 *             |    -- 128KB --     |
 *             | kernel heap -->    |   128KB and grows up bounded by __heap_end
 *             |                    |
 *             |                    |
 * 0x8008_0000 +--------------------+   Also in Power Domain 1
 *             |SRAM8: HART0 scratch|
 * 0x80081000  +--------------------+   Also in Power Domain 1
 *             |SRAM9: HART1 scratch|
 * 0x80082000  +--------------------+  End of declared SRAM (520KiB)
 *             :   (unused gap)     :
 * 0x81000000  +--------------------+   Represents Adafruit Metro PSRAM (8MB)
 *             |user heap ->        |
 *             |                    |
 *             |     -- 4MB --      |   End of user heap
 *             |  buffers           |
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
    SRAM_PD0 : ORIGIN = 0x80000000, LENGTH = 0x00040000 /* 256KB */
    SRAM_PD1 : ORIGIN = 0x80040000, LENGTH = 0x00042000 /* 256KB + 2 per HART 4KB scratch RAM */
    PSRAM : ORIGIN = 0x81000000, LENGTH = 0x00800000 /* 8MB PSRAM */
}

__psram_start = 0x81000000;
__psram_end = 0x81800000;
__fb_size   = 640 * 480 * 4;   /* 640  x 480 x 4 bytes = 1.2MiB */
__fb_addr   = 0x81800000 - __fb_size;

SECTIONS {
    .text : { *(.text.init) *(.text .text.*) } > FLASH

    .rodata : {
        *(.rodata .rodata.* .srodata .srodata.*)
        . = ALIGN(4);               /* Padding .rodata so that .data start is aligned */
    } > FLASH

    /* Note: No gp use for LLVM for RISC-V so do not PROVIDE */
    .data : {
        . = ALIGN(4);
        __data_start = .;
        *(.data .data.*) *(.sdata .sdata.*)
        . = ALIGN(4);
        __data_end = .;
    } > SRAM_PD1 AT > FLASH
    __data_lma = LOADADDR(.data);

    .bss : { 
        . = ALIGN(4);
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.*)
        . = ALIGN(4);
        __bss_end = .;
    } > SRAM_PD1

    .heap_pd0 (NOLOAD) : ALIGN(256K) {
        __heap_pd0_start = .;
        . = . + 256K;
        __heap_pd0_end = .;
    } > SRAM_PD0

    .heap_pd1 (NOLOAD) : ALIGN(128K) { /* For buddy allocator need heap to be aligned to largest alloc size */
        __heap_pd1_start = .;
        . = . + 128K;               /* For buddy allcoator must be power of two */
        __heap_pd1_end = .;
    } > SRAM_PD1

    .heap_psram (NOLOAD) : ALIGN(4M) {
        __heap_psram_start = .;
        . = . + 4M;
        __heap_psram_end = .;
    } > PSRAM

    /* Add dedicated SRAM8 for HART0 scratch ram */
    .scratch_hart0 0x80080000 (NOLOAD) : {
        __hart0_stack_start = .;
        . = . + 4K;
        __hart0_stack_top = .;
    } > SRAM_PD1

    /* Add dedicated SRAM9 for HART1 scratch ram */
    .scratch_hart1 0x80081000 (NOLOAD) : {
        __hart1_stack_start = .;
        . = . + 4K;
        __hart1_stack_top = .;
    } > SRAM_PD1

    /DISCARD/ : { *(.comment) *(.eh_frame)} /* Discard comment strings to keep binary small */
}

ASSERT(__fb_addr >= ORIGIN(PSRAM), "framebuffer below PSRAM")
ASSERT(__fb_addr + __fb_size <= ORIGIN(PSRAM) + LENGTH(PSRAM), "framebuffer exceeds PSRAM")
ASSERT(__heap_pd1_end <= __hart0_stack_start, "PD1 heap overflows scratch RAM")
