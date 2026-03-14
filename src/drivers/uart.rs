//! QEMU UART Implementation
//!
//! Using QEMU virt board

use core::ptr::{read_volatile, write_volatile};

use crate::board::uart;

/// UART writer for QEMU
pub struct UartWriter;

impl crate::hal::Writer for UartWriter {
    fn write_byte(&self, byte: u8) {
        unsafe {
            write_volatile(uart::BASE as *mut u8, byte);
        }

        #[cfg(test)]
        crate::io::test_io::capture(byte);
    }
}

impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        crate::hal::Writer::write_str(self, s);
        Ok(())
    }
}

/// UART reader for QEMU
pub struct UartReader;

impl crate::hal::Reader for UartReader {
    fn read_byte(&self) -> Option<u8> {
        // First check LSR byte
        if unsafe { read_volatile((uart::BASE + uart::LSR) as *const u8) } & uart::LSR_BYTE_READY
            == 0
        {
            None
        } else {
            // Read the byte as ready
            Some(unsafe { read_volatile(uart::BASE as *const u8) })
        }
    }
}
