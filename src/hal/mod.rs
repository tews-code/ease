//! Hardware Abstraction Layer
//!
//! Defines HAL traits. Hardware-specific implementations are in submodules

#![allow(dead_code)]

pub const BLOCK_SIZE: usize = 512;
pub const PAGE_SIZE: usize = 4096;

/// Puts the CPU into low-power wait state until the next interrupt fires.
pub fn wait_for_interrupt() {
    unsafe {
        // Safety: "wfi" is safe to call
        core::arch::asm!("wfi");
    }
}

/// Trait for byte-level output
///
/// Implemenation can write to UART or capture output for testing
pub trait Writer {
    fn write_byte(&self, byte: u8);

    fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

/// Trait for byte-level input
///
/// Implementation can read from UART or capture input for testing
pub trait Reader {
    fn read_byte(&self) -> Option<u8>;
}
