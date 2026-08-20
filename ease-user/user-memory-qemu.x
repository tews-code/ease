/* User memory definitions are shared between EASE and
 * user programs.
 */

ENTRY(_start)

MEMORY {
    SRAM_PD0    : ORIGIN = 0x80000000, LENGTH = 0x00040000 /* SRAM0-3 - 256KB */
    PSRAM       : ORIGIN = 0x81000000, LENGTH = 0x00800000 /* 8MB */
}

INCLUDE memory-shared-qemu.x

SECTIONS {

    /* POWER DOMAIN 0 */

    /* Power Domain 0 used as a single memory region across SRAM0-3 */
    /* Used for user heap which needs NAPOT given PMP requirements */
    .heap (NOLOAD) : {
        __user_heap_start = .;
        . = . + __user_heap_sram_size;
        __user_heap_end = .;
    } > SRAM_PD0

    /* PSRAM */
    /* Temporarily put all user threads .text in 4KB window in PSRAM */
    .text __user_text_origin : {
        __user_text_start = .;
        *(.text._start)
        *(.text .text.* .rodata .rodata.* .srodata .srodata.*)
        . = __user_text_start + __user_text_size;
        __user_text_end = .;
    } > PSRAM

    /* Temporarily put all user threads .data and .bss in 4KB window in PSRAM */
    .data __user_data_origin : {
        __user_data_start = .;
        *(.data .data.* .sdata .sdata.*)
        . = __user_data_start + __user_data_size;
        __user_data_end = .;
    } > PSRAM

    .bss __user_bss_origin (NOLOAD) : {
        __user_bss_start = .;
        *(.bss .bss.* .sbss .sbss.*)
        . = __user_bss_start + __user_bss_size;
        __user_bss_end = .;
    } > PSRAM

    /* Discard .eh_frame as we are using panic = "abort" */
    /DISCARD/ : { *(.comment) *(.eh_frame_hdr) *(.eh_frame)} /* Discard comment strings to keep binary small */
}
