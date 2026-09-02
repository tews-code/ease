//! QEMU UART Implementation
//!
//! Using QEMU virt board's 16550 UART
//!
//! ## UART 16550 ##
//!
//! # Status #
//! The UART uses the LSR (Line Status Register) to indicate the status of the UART.
//! - For transmission: we check the LSR_THRE bit (Transmit Hold Register is Empty) - if that register is empty we can write the next byte, which will automatically be teed up as the next transmission
//! - For receiving: we check the LSR_DATA_READY bit, which tells us if there is a byte to be read from the RBR register.
//!
//! # Interrupts #
//! In order to avoid polling the LSR, interrupts can be enabled by writing to the IER (Interrupt Enable Register). We enable interrupts for RX and TX:
//! 1. Transmission: The ETBEI (Enable Transmitter Holding Register Empty Interrupt) bit enables interrupts. The interrupt is triggered each time the THR register is empty (and triggers immediately if enabled while the register is empty)
//! 2. Receiving: The ERBFI (Enable Received Data Available Interrupt) bit enables an interrupt each time the RBR holds a byte.
//!
//! When an interrupt is received, it can be decoded by looking at the value in the IIR (Interrupt Identification Register).
//!
//! # Transmission #
//! The THR (Transmitter Holding Register) is the staging slot for a byte to be transmitted. When you write a byte to the UART, it lands here and is ready for actual transmission.
//!
//! # Receiving #
//! The RBR register holds the latest received byte.
//!
//! In our driver, we have two SPSC ring buffers, one for RX and one for TX.

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::arch::mmio;
use crate::board::uart;
use crate::kernel::collection::SpscRingBuf;
use crate::kernel::sync::IrqSpinLock;

// Standard 16550 UART offsets
/// Receive Buffer Register
const RBR: usize = 0; // offset +0: (read)
/// Transmit Holding Register
const THR: usize = 0; // offset +0: (write)
/// Interrupt Enable Register
const IER: usize = 1;
/// Interrupt Identification Register (read)
const IIR: usize = 2;
/// FIFO Constrol Register (write)
const FCR: usize = 2;
/// Line Status Register
const LSR: usize = 5;
// Standard 16550 flags
/// Line Status Register - TX is ready as THR is Empty
const LSR_THRE: u8 = 1 << 5;
/// Line Status Register - RX is ready (data in RBR)
const LSR_DATA_READY: u8 = 1 << 0;
/// Enable Received Data Available Interrupt
const ERBFI: u8 = 1 << 0;
/// Enable Transmitter Holding Register Empty Interrupt
const ETBEI: u8 = 1 << 1;
// FIFO control register flags
const FCR_FIFO_ENABLE: u8 = 1 << 0;
const FCR_RX_FIFO_RESET: u8 = 1 << 1;
const FCR_TX_FIFO_RESET: u8 = 1 << 2;
const FCR_TRIGGER_8: u8 = 0b10 << 6; // RX interrupt at 8 bytes
// Interrupt Identification Register ids
/// IIR no interrupt is pending (active-low "Interrupt Pending" bit)
const IIR_NO_INT_PENDING: u8 = 1 << 0;
const IIR_ID_MASK: u8 = 0x0E;
const IIR_THR_EMPTY: u8 = 0b0010;
const IIR_RX_DATA_AVAILABLE: u8 = 0b0100;
const IIR_RX_LINE_STATUS: u8 = 0b0110;
const IIR_RX_TIMEOUT: u8 = 0b1100;

/// Ease SPSC ring buffer size for receiving. Small so we don't support bursts e.g. paste into console
const RX_BUF_SIZE: usize = 64;
/// Ease SPSC ring buffer size for transmission. Set to 1KB to deal with bursts.
const TX_BUF_SIZE: usize = 1024;
static RX_BUF: SpscRingBuf<u8, RX_BUF_SIZE> = SpscRingBuf::new();
static TX_BUF: SpscRingBuf<u8, TX_BUF_SIZE> = SpscRingBuf::new();
/// We use lock-free SPSC buffers but need to
/// serialise the TX users by using a spinlock
/// to keep the single produce single consumer contract
/// Lock ordering - first UART_WRITER then TX_DRAIN_LOCK
static TX_DRAIN_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());
/// Main writer
static UART_WRITER: IrqSpinLock<UartWriter> = IrqSpinLock::new(UartWriter(())); // Private - only access with `with_uart_writer`

/// Initialise by enabling FIFOs and the receiving interrupt;
/// All other functionality is taken from QEMU virt default settings
pub(crate) fn init() {
    // Enable FIFOs
    mmio::write8(
        uart::BASE,
        FCR,
        FCR_FIFO_ENABLE | FCR_RX_FIFO_RESET | FCR_TX_FIFO_RESET | FCR_TRIGGER_8,
    );
    RBR_interrupt_enable();
}

/// Helper function to check the LSR to see if transmission register (THR) is empty
#[allow(non_snake_case)]
fn LSR_tx_ready() -> bool {
    let lsr = mmio::read8(uart::BASE, LSR);
    lsr & LSR_THRE != 0
}
/// Helper function to check the LSR to see if a byte has been received and RBR is ready for reading
#[allow(non_snake_case)]
fn LSR_rx_ready() -> bool {
    mmio::read8(uart::BASE, LSR) & LSR_DATA_READY != 0
}
/// Helper function to enable THR empty interrupt
#[allow(non_snake_case)]
fn THRE_interrupt_enable() {
    let enabled_interrupts = mmio::read8(uart::BASE, IER);
    mmio::write8(uart::BASE, IER, enabled_interrupts | ETBEI);
}
/// Helper function to disable THR empty interrupt
#[allow(non_snake_case)]
fn THRE_interrupt_disable() {
    let enabled_interrupts = mmio::read8(uart::BASE, IER);
    mmio::write8(uart::BASE, IER, enabled_interrupts & !ETBEI);
}
/// Helper function to enable RBR holds a value interrupt
#[allow(non_snake_case)]
fn RBR_interrupt_enable() {
    let enabled_interrupts = mmio::read8(uart::BASE, IER);
    mmio::write8(uart::BASE, IER, enabled_interrupts | ERBFI);
}

/// Pop all available bytes off the TX_BUF
/// as long as THR stays empty
/// Takes an IRQ lock on TX_DRAIN_LOCK
///
/// If the TX_BUF is completely drained
/// the THRE interrupt is disabled
fn drain_tx_bytes_under_lock() {
    // Take a lock - this is to serialise the consumer side of the SPSC
    let _tx_lock_guard = TX_DRAIN_LOCK.lock();
    // First check if the transmit register is available
    while LSR_tx_ready() {
        // Ok to pop a byte
        if let Some(byte) = TX_BUF.pop() {
            // Register is free and byte to transmit - so write
            mmio::write8(uart::BASE, THR, byte);
        } else {
            // Buffer is empty - disable THRE and exit
            THRE_interrupt_disable();
            return;
        }
    }
    // LSR no longer agrees that bytes can be transmitted, so exit without knowing the buffer is empty
}
/// Interrupt handler for UART
///
/// Called by trap handler - interrupts are disabled
/// Reads IIR to determine interrupt type and clears the source.
#[cfg_attr(feature = "profile", profile)]
pub fn handle_interrupt() {
    // Loop because the interrupts are ordered by priority, so more than one may be waiting
    loop {
        let iir = mmio::read8(uart::BASE, IIR);
        if iir & IIR_NO_INT_PENDING != 0 {
            // No more interrupts, we are done
            break;
        }
        match iir & IIR_ID_MASK {
            IIR_RX_DATA_AVAILABLE | IIR_RX_TIMEOUT => {
                // The interrupt could be spurious so check if LSR agrees that a byte is ready
                while LSR_rx_ready() {
                    // RX data ready — read RBR to clear interrupt
                    let byte = mmio::read8(uart::BASE, RBR);
                    // We can safely push to this SPSC ring buffer because
                    // the PLIC only allows one HART to handle a UART interrupt
                    // at a time, so there is no need to serialise
                    let _ = RX_BUF.push(byte); // drop if full
                }
            }
            IIR_THR_EMPTY => {
                // THRE (The THR is empty so we are TX ready)
                // We dare to take an IRQ lock while still running in the trap handler
                // because the function performs quick mmio reads and writes, and we can't
                // have the same core interrupt as we are in the trap handled with interrupts disabled
                // and the other core will only hold the lock briefly.
                drain_tx_bytes_under_lock();
            }
            IIR_RX_LINE_STATUS => {
                // Unwanted line status interrupt — read LSR to clear
                let _ = mmio::read8(uart::BASE, LSR);
            }
            _ => {} // No interrupt pending or modem status — ignore
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
    /// This is safe because we serialise the consumer side with a lock.
    fn write_byte(&self, byte: u8) {
        // Try to push the byte. We don't reenable interrupts in this function
        // as that is left to `write_str` to avoid interrupting per byte.
        while TX_BUF.push(byte).is_err() {
            // The TX buffer is full. To avoid stalling (with interrupts
            // disabled) we briefly take the role of the consumer.
            // Note that this means we take an IRQ spin lock on the TX_DRAIN_LOCK
            // inside the (already held) UART_WRITER lock, because it contends with
            // the actual consumer which is serialised by also taking this lock.
            drain_tx_bytes_under_lock();
            core::hint::spin_loop(); // Note this is a spin lock with interrupts disabled; expected to be short
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
        // But take the lock first - let's not get in the way of a write_byte's
        // drain_tx_bytes_under_lock() if the buffer was full.
        let _tx_drain_guard = TX_DRAIN_LOCK.lock();
        THRE_interrupt_enable();
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
///
/// Use for panic and sensitive lock-free printing, maybe interleaved
pub(crate) fn direct_write_byte(byte: u8) {
    while mmio::read8(uart::BASE, LSR) & LSR_THRE == 0 {
        core::hint::spin_loop();
    }
    mmio::write8(uart::BASE, THR, byte);

    #[cfg(all(test, feature = "test-io"))]
    crate::io::test_io::capture(byte);
}
