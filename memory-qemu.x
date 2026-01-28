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
    .bss : { *(.bss .bss.*) } > RAM
}
