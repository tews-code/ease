//! QEMU UART Implementation
//!
//! Using QEMU virt board

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::arch::mmio;
use crate::board::{plic, uart};
use crate::drivers::plic::with_plic;
use crate::kernel::collection::SpscRingBuf;
use crate::kernel::sync::IrqSpinLock;

const RBR: usize = 0; // offset +0: receive buffer register (read)
const THR: usize = 0; // offset +0: transmit holding register (write)
const IER: usize = 1; // offset +1: interrupt enable register
const IIR: usize = 2; // offset +2: Interrupt Identification Register
const LSR: usize = 5; // offset +5: line status register

const LSR_TX_READY: u8 = 0x20;
const LSR_BYTE_READY: u8 = 1;
const THRE_INTERRUPT: u8 = 1 << 1; // Transmitter Holding Register Empty - IER register bit 1

static RX_BUF: SpscRingBuf<u8, 64> = SpscRingBuf::new();
static TX_BUF: SpscRingBuf<u8, 256> = SpscRingBuf::new();

/// UART writer for QEMU
pub struct UartWriter(()); // ZST has a private field to seal

impl UartWriter {
    fn write_byte(&self, byte: u8) {
        while TX_BUF.push(byte).is_err() {
            mmio::write8(
                uart::BASE,
                IER,
                mmio::read8(uart::BASE, IER) | THRE_INTERRUPT,
            );
            core::hint::spin_loop();
        }
        // Enable THRE interrupt to drain the buffer
        mmio::write8(
            uart::BASE,
            IER,
            mmio::read8(uart::BASE, IER) | THRE_INTERRUPT,
        );

        #[cfg(all(test, feature = "test-io"))]
        crate::io::test_io::capture(byte);
    }

    fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        UartWriter::write_str(self, s);
        Ok(())
    }
}

static UART_WRITER: IrqSpinLock<UartWriter> = IrqSpinLock::new(UartWriter(())); // Private - only access with `with_uart_writer`

/// Runs a closure with exclusive access to the UART_WRITER driver.
pub fn with_uart_writer<F, R>(f: F) -> R
where
    F: FnOnce(&mut UartWriter) -> R,
{
    let mut uart_writer = UART_WRITER.lock();
    f(&mut uart_writer)
}

/// Direct write to MMIO - skips queue
pub fn direct_write_byte(byte: u8) {
    while mmio::read8(uart::BASE, LSR) & LSR_TX_READY == 0 {
        core::hint::spin_loop();
    }
    mmio::write8(uart::BASE, THR, byte);

    #[cfg(all(test, feature = "test-io"))]
    crate::io::test_io::capture(byte);
}

/// UART reader for QEMU
pub struct UartReader;

impl UartReader {
    pub fn read_byte(&self) -> Option<u8> {
        RX_BUF.pop()
    }
}

// Interrupt handler for UART
//
// Called by trap handler - interrupts are disabled
// Reads IIR to determine interrupt type and clears the source.
#[cfg_attr(feature = "profile", profile)]
pub fn handle_interrupt() {
    let iir = mmio::read8(uart::BASE, IIR) & 0x0E;
    match iir {
        0b0100 | 0b1100 => {
            // RX data ready — drain RBR to clear interrupt
            while mmio::read8(uart::BASE, LSR) & LSR_BYTE_READY != 0 {
                let byte = mmio::read8(uart::BASE, RBR);
                let _ = RX_BUF.push(byte); // drop if full
            }
        }
        0b0010 => {
            // THRE (TX ready)
            if let Some(byte) = TX_BUF.pop() {
                mmio::write8(uart::BASE, THR, byte);
            } else {
                // Buffer empty — disable THRE interrupt
                mmio::write8(
                    uart::BASE,
                    IER,
                    mmio::read8(uart::BASE, IER) & !THRE_INTERRUPT,
                );
            }
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

/// Initialise the UART
pub fn init() {
    // Plic setup
    with_plic(|p| {
        p.set_priority(plic::UART0_IRQ, 1);
        p.enable(plic::UART0_IRQ);
    });
    // Uart enable
    enable_rx_interrupt();
}
