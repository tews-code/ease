ENTRY(_start)

MEMORY {
    RAM : ORIGIN = 0x80000000, LENGTH = 0x8000000
}

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
