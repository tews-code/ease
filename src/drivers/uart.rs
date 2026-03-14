//! QEMU UART Implementation
//!
//! Using QEMU virt board

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

use crate::board::uart;

const RBR: usize = 0; // offset +0: receive buffer register (read)
const THR: usize = 0; // offset +0: transmit holding register (write)
const IER: usize = 1; // offset +1: interrupt enable register
const IIR: usize = 2; // offset +2: Interrupt Identification Register
const LSR: usize = 5; // offset +5: line status register

const LSR_TX_READY: u8 = 0x20;
const LSR_BYTE_READY: u8 = 1;
const THRE_INTERRUPT: u8 = 1 << 1; // Transmitter Holding Register Empty - IER register bit 1

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
        if unsafe { read_volatile((uart::BASE + LSR) as *const u8) } & LSR_BYTE_READY == 0 {
            None
        } else {
            // Read the byte as ready
            Some(unsafe { read_volatile(uart::BASE as *const u8) })
        }
    }
}
