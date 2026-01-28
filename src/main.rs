//! EASE - OS for RP2350
//!
//! A hobby OS for Raspberry Pi Pico 2 in RISC-V mode.
//!
//! # Boot Process (QEMU virt)
//!
//! 1. QEMU loads the kernel binary at `0x80000000` (RAM base, set in `memory-qemu.x`)
//! 2. CPU begins execution at the `ENTRY` symbol: [`_start`]
//! 3. Currently just loops forever — no stack, BSS, or hardware init yet
//!
//! # Memory Layout
//!
//! Defined in `memory-qemu.x` for QEMU's `virt` machine:
//!
//! | Section   | Location | Contents                    |
//! |-----------|----------|-----------------------------|
//! | `.text`   | RAM      | Executable code             |
//! | `.rodata` | RAM      | Read-only data (strings)    |
//! | `.data`   | RAM      | Initialized mutable data    |
//! | `.bss`    | RAM      | Zero-initialized data       |
//!
//! RAM spans `0x80000000` to `0x88000000` (128 MB on QEMU virt).

#![no_std]
#![no_main]
#![warn(missing_docs)]

use core::arch::naked_asm;
use core::ptr::write_volatile;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

fn uart_putc(byte: u8) {
    const UART_ADDRESS: usize = 0x10000000;
    unsafe { write_volatile(UART_ADDRESS as *mut u8, byte); }
}

#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    uart_putc(b'H');
    loop {}
}

#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    naked_asm!(
        "li sp, 0x80100000",
        "j main",
        "unimp",
    );
}
