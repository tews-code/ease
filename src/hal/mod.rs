//! Hardware Abstraction Layer
//!
//! Defines HAL traits. Hardware-specific implementations are in submodules

pub mod qemu_virt;

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
