//! Hardware Abstraction Layer
//!
//! Defines HAL traits. Hardware-specific implementations are in submodules

pub const BLOCK_SIZE: usize = 512;
pub const PAGE_SIZE: usize = 4096;

/// Puts the CPU into low-power wait state until the next interrupt fires.
pub fn wait_for_interrupt() {
    unsafe {
        // Safety: "wfi" is safe to call
        core::arch::asm!("wfi");
    }
}

// ASCII chars that are used for console and serial control
pub mod ascii {
    pub const BELL: u8 = 0x07;
    pub const BS: u8 = 0x08;
    pub const TAB: u8 = 0x09;
    pub const LF: u8 = 0x0A;
    pub const FF: u8 = 0x0C;
    pub const CR: u8 = 0x0D;
    pub const DEL: u8 = 0x7F;
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

/// Trait for block devices
#[allow(dead_code)]
pub trait BlockDevice {
    /// Block device errors
    type BlkError;

    fn read_block(&self, block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), Self::BlkError>;

    fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), Self::BlkError>;

    fn block_count(&self) -> Option<u32>;
}
