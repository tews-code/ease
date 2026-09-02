//! QEMU UART Implementation
//!
//! Using QEMU virt board's 16550 uart

//! UART 16550
//!
//! # Control #
//! The UART uses the LSR (Line Status Register) to indicate the status of the UART.
//!
//! # Transmission #
//! The UART has two registers for transmission:
//! 1. THR (Transmitter Holding Register) — the staging slot. When you write a byte to the UART, it lands here. This is all the CPU ever touches.
//! 2. TSR (Transmitter Shift Register) — the working register. The UART moves the byte from THR into TSR, then shifts it out onto the wire bit by bit at baud-rate speed.
//!
//! We use THRE (Transmitter Holding Register Empty) as our flag to feed the next
//! byte, even if the TSR is still working on transmitting the previous byte.

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::arch::mmio;
use crate::board::uart;
use crate::kernel::collection::SpscRingBuf;
use crate::kernel::sync::IrqSpinLock;

/// Standard 16550 UART offsets
const RBR: usize = 0; // offset +0: receive buffer register (read)
/// Transmit Holding Register
const THR: usize = 0; // offset +0: (write)
/// Interrupt Enable Register
const IER: usize = 1;
/// Interrupt Identification Register
const IIR: usize = 2;
/// Line Status Register
const LSR: usize = 5;
/// Standard 16550 flags
/// Line Status Register - TX is ready
const LSR_TX_READY: u8 = 0x20;
const LSR_BYTE_READY: u8 = 1;
/// Transmitter Holding Register Empty
const THRE_INTERRUPT: u8 = 1 << 1; // IER register bit 1
/// Interrupt Identification Register ids
const IIR_ID_MASK: u8 = 0x0E;
const IIR_THR_EMPTY: u8 = 0b0010;
const IIR_RX_DATA_AVAILABLE: u8 = 0b0100;
const IIR_RX_LINE_STATUS: u8 = 0b0110;
const IIR_RX_TIMEOUT: u8 = 0b1100;

/// Ease buffers
const RX_BUF_SIZE: usize = 64;
const TX_BUF_SIZE: usize = 1024;
/// We use lock-free SPSC buffers but need to
/// serialise the TX users by using a spinlock
/// to keep the single produce single consumer contract
static RX_BUF: SpscRingBuf<u8, RX_BUF_SIZE> = SpscRingBuf::new();
static TX_BUF: SpscRingBuf<u8, TX_BUF_SIZE> = SpscRingBuf::new();
// Lock ordering - first UART_WRITER then TX_DRAIN_LOCK
static TX_DRAIN_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());
/// Main writer
static UART_WRITER: IrqSpinLock<UartWriter> = IrqSpinLock::new(UartWriter(())); // Private - only access with `with_uart_writer`

/// Helper function to check if transmission (THR) is ready
#[allow(non_snake_case)]
fn THR_ready() -> bool {
    let lsr = mmio::read8(uart::BASE, LSR);
    lsr & LSR_TX_READY != 0
}
/// Helper function to enable THRE interrupt
#[allow(non_snake_case)]
fn THRE_enable() {
    let enabled_interrupts = mmio::read8(uart::BASE, IER);
    mmio::write8(uart::BASE, IER, enabled_interrupts | THRE_INTERRUPT);
}
/// Helper function to disable THRE interrupt
#[allow(non_snake_case)]
fn THRE_disable() {
    let enabled_interrupts = mmio::read8(uart::BASE, IER);
    mmio::write8(uart::BASE, IER, enabled_interrupts & !THRE_INTERRUPT);
}

/// Pop all available bytes off the TX_BUF
/// while the hardware has the register available
/// Takes an IRQ lock on TX_DRAIN_LOCK
fn pop_tx_bytes_under_lock() {
    let _tx_lock_guard = TX_DRAIN_LOCK.lock();
    // First check if the transmit register is available
    while THR_ready() {
        // Ok to pop a byte
        if let Some(byte) = TX_BUF.pop() {
            // Register is free and byte to transmit - so write
            mmio::write8(uart::BASE, THR, byte);
        } else {
            // Buffer is empty - disable THRE and exit
            THRE_disable();
            return;
        }
    }
}

/// UART writer for QEMU
pub struct UartWriter(()); // ZST has a private field to seal

impl UartWriter {
    /// Push a single byte into the TX_BUF SPSC queue
    ///
    /// If the push fails the queue is full. We (briefly!) take
    /// on the role of the consumer to read as many bytes as available
    /// before returning to the producer role.
    fn write_byte(&self, byte: u8) {
        // Try to push the byte, on success raise the interrupt
        while TX_BUF.push(byte).is_err() {
            // The TX buffer is full. To avoid stalling (with interrupts
            // disabled) we briefly take the role of the consumer.
            // Note that this means we take an IRQ spin lock on the TX_DRAIN_LOCK
            // inside the (already held) UART_WRITER lock. This contends with
            // the actual consumer which is serialised by also taking this lock.
            pop_tx_bytes_under_lock();
            core::hint::spin_loop();
        }
        #[cfg(all(test, feature = "test-io"))]
        crate::io::test_io::capture(byte);
    }
    /// Writes all the bytes in the string and
    /// enables the THRE interrupt when done
    fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        // Bytes have been pushed into the TX_BUF SPSC queue.
        // Enable THRE interrupt to signal the consumer to drain the TX buffer
        let _tx_drain_guard = TX_DRAIN_LOCK.lock();
        THRE_enable();
    }
}

impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        UartWriter::write_str(self, s);
        Ok(())
    }
}

/// Runs a closure with exclusive access to the UART_WRITER driver.
pub fn with_uart_writer<F, R>(f: F) -> R
where
    F: FnOnce(&mut UartWriter) -> R,
{
    let mut uart_writer = UART_WRITER.lock();
    f(&mut uart_writer)
}

/// Direct write to MMIO - skips queue
/// Busy waits on the LSR before writing the byte
pub fn direct_write_byte(byte: u8) {
    while mmio::read8(uart::BASE, LSR) & LSR_TX_READY == 0 {
        core::hint::spin_loop();
    }
    mmio::write8(uart::BASE, THR, byte);

    #[cfg(all(test, feature = "test-io"))]
    crate::io::test_io::capture(byte);
}

// Interrupt handler for UART
//
// Called by trap handler - interrupts are disabled
// Reads IIR to determine interrupt type and clears the source.
#[cfg_attr(feature = "profile", profile)]
pub fn handle_interrupt() {
    let interrupt_id = mmio::read8(uart::BASE, IIR) & IIR_ID_MASK;
    match interrupt_id {
        IIR_RX_DATA_AVAILABLE | IIR_RX_TIMEOUT => {
            // RX data ready — drain RBR to clear interrupt
            while mmio::read8(uart::BASE, LSR) & LSR_BYTE_READY != 0 {
                let byte = mmio::read8(uart::BASE, RBR);
                let _ = RX_BUF.push(byte); // drop if full
            }
        }
        IIR_THR_EMPTY => {
            // THRE (The THR is empty so we are TX ready)
            // We dare to take an IRQ lock while still running in the trap
            // handler
            pop_tx_bytes_under_lock();
        }
        IIR_RX_LINE_STATUS => {
            // Line status — read LSR to clear
            let _ = mmio::read8(uart::BASE, LSR);
        }
        _ => {} // No interrupt pending or modem status — ignore
    }
}
