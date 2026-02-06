//! QEMU Implementation
//!
//! Using QEMU virt board

use core::ptr::{read_volatile, write_volatile};

/// UART base address in QEMU virt board (16550 compatible)
const UART_ADDRESS: usize = 0x10000000;

/// UART writer for QEMU
pub struct UartWriter;

impl super::Writer for UartWriter {
    fn write_byte(&self, byte: u8) {
        unsafe {
            write_volatile(UART_ADDRESS as *mut u8, byte);
        }

        #[cfg(test)]
        crate::io::test_io::capture(byte);
    }
}

impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        super::Writer::write_str(self, s);
        Ok(())
    }
}

/// UART reader for QEMU
pub struct UartReader;

impl super::Reader for UartReader {
    fn read_byte(&self) -> Option<u8> {
        const LSR: usize = 5;
        const BYTE_READY: u8 = 1;
        // First check LSR byte
        if unsafe { read_volatile((UART_ADDRESS + LSR) as *const u8) } & BYTE_READY == 0 {
            None
        } else {
            // Read the byte as ready
            Some(unsafe { read_volatile(UART_ADDRESS as *const u8) })
        }
    }
}
