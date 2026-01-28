//! EASE - OS for RP2350
//!
//! A hobby OS for Raspberry Pi Pico 2 in RISC-V mode

#![no_std]
#![no_main]
#![warn(missing_docs)]

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
extern "C" fn _start() {
    loop {}
}
