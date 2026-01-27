//! EASE - OS for RP2350
//!
//! A hobby OS for Raspberry Pi Pico 2 in RISC-V mode

#![no_std]
#![no_main]
#![warn(missing_docs)]

#[unsafe(no_mangle)]
extern "C" fn _start() {
    loop {}
}
