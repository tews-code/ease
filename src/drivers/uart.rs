//! QEMU UART Implementation
//!
//! Using QEMU virt board

#![allow(dead_code)]

use crate::arch::mmio;
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
        mmio::write8(uart::BASE, THR, byte);

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

        if mmio::read8(uart::BASE, LSR) & LSR_BYTE_READY == 0 {
            None
        } else {
            // Read the byte as ready
            Some(mmio::read8(uart::BASE, RBR))
        }
    }
}

// Interrupt handler for UART
//
// Called by trap handler - interrupts are disabled
// Reads IIR to determine interrupt type and clears the source.
pub fn handle_interrupt() {
    let iir = mmio::read8(uart::BASE, IIR) & 0x0E;
    match iir {
        0b0100 | 0b1100 => {
            // RX data ready — drain RBR to clear interrupt
            // TODO: Step 6 will push into SpscRingBuf instead
            let _ = mmio::read8(uart::BASE, RBR);
        }
        0b0010 => {
            // THRE (TX ready) — nothing to do yet
        }
        0b0110 => {
            // Line status — read LSR to clear
            let _ = mmio::read8(uart::BASE, LSR);
        }
        _ => {} // No interrupt pending or modem status — ignore
    }
}

/// Enables UART RX interrupts
pub fn enable_rx_interrupt() {
    mmio::write8(uart::BASE, IER, 1);
}
