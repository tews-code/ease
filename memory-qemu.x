ENTRY(_start)

MEMORY {
    RAM : ORIGIN = 0x80000000, LENGTH = 0x8000000
}

__stack_top = 0x80100000;
__fb_addr   = 0x80200000;
__fb_size   = 640 * 480 * 4;   /* 640  x 480 x 4 bytes = 1.2MiB */


SECTIONS {
    .text : {
        *(.text.init)
        *(.text .text.*)
    } > RAM

    .rodata : { *(.rodata .rodata.*) } > RAM

    .data : { *(.data .data.*) } > RAM

    .bss : { 
	__bss_start = .;
	*(.bss .bss.*) 
	__bss_end = .;
    } > RAM

    /* Heap: 64KB after BSS */
    . = ALIGN(16);
    __heap_start = .;
    . = . + 64K;
    __heap_end = .;
}

ASSERT(__heap_end <= __stack_top, "heap overlaps stack region")
ASSERT(__stack_top <= __fb_addr, "stack region overlaps framebuffer")
ASSERT(__fb_addr + __fb_size <= ORIGIN(RAM) + LENGTH(RAM), "framebuffer exceeds RAM")
