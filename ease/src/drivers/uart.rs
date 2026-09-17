//! QEMU UART Implementation
//!
//! Using QEMU virt board's 16550A UART
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

use crate::arch::{hart_id, interrupts, mmio};
use crate::board::uart;
use crate::kernel::collection::{StackVec, spsc};
use crate::kernel::sync::SpinLock;

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
/// Line Status Register - TX is done as both THR and TSR are Empty
const LSR_TEMT: u8 = 1 << 6;
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
/// Interrupt Identification Register ids - mask
const IIR_ID_MASK: u8 = 0x0E;
/// IIR no interrupt is pending (active-low "Interrupt Pending" bit)
const IIR_NO_INT_PENDING: u8 = 1 << 0;
const IIR_THR_EMPTY: u8 = 0b0010;
const IIR_RX_DATA_AVAILABLE: u8 = 0b0100;
const IIR_RX_LINE_STATUS: u8 = 0b0110;
const IIR_RX_TIMEOUT: u8 = 0b1100;
/// We construct the SPSC queues in this module
///
/// For transmission the producer is exclusively held in the [UartWriter] struct. Given that `UartWriter`
/// is protected by a spin lock, this serialises multiple threads from both HARTs to ensure only one queue
/// user at a time.
/// Equally, for transmission the consumer is held in a [TryLock]. Generally the lock is held by the ISR
/// but it can be picked up by the panic handler to flush the queue.
///
/// For the receiving queue the producer is purely the ISR so no locking is required. There can be multiple
/// threads consuming this queue so the consumer is protected by a spin lock.
mod queue {
    use crate::kernel::collection::spsc;
    use crate::kernel::sync::{SpinLock, TryLock, WaitQueue};

    /// Byte queue length for receiving. Small, so we don't support bursts e.g. paste into console
    pub(super) const RX_LEN: usize = 64;
    /// Ease queue size for transmission. Set to 1KB to deal with bursts.
    pub(super) const TX_LEN: usize = 1024;

    /// Byte queue for receiveing
    pub(super) static RX: spsc::Queue<u8, RX_LEN> = spsc::Queue::new();
    /// Receiving byte producer is a static because it is used by the ISR after trap.
    /// It is exclusively used by the ISR on HART 0. We use `TryLock` as a lightweight
    /// way to bring interior mutability into the static.
    // Safety: Only create the consumer once right here
    pub(super) static RX_PRODUCER: TryLock<spsc::Producer<'static, u8, RX_LEN>> =
        unsafe { TryLock::new(RX.producer_unchecked()) };
    /// Receiving byte consumer is a static where multiple input (keyboard) readers can contend.
    pub(super) static RX_CONSUMER: SpinLock<spsc::Consumer<'static, u8, RX_LEN>> =
        SpinLock::new(unsafe { RX.consumer_unchecked() });

    /// Transmission queue
    pub(super) static TX: spsc::Queue<u8, TX_LEN> = spsc::Queue::new();
    /// Transmission byte consumer is a static because it is used by the ISR after trap. It can also
    /// be used by the flush routine so needs a lightweight lock
    // Safety: Only created here and try_lock ensures only one user at a time
    pub(super) static TX_CONSUMER: TryLock<spsc::Consumer<'static, u8, TX_LEN>> =
        unsafe { TryLock::new(TX.consumer_unchecked()) };
    /// Wait queue on TX being empty
    pub(super) static TX_DRAINED: WaitQueue = WaitQueue::new();
}

/// Main writer
/// Note that this can only be used used with interrupts enabled
static UART_WRITER: SpinLock<UartWriter> = SpinLock::new(UartWriter::new()); // Private - only access with `with_uart_writer`

/// Direct write to MMIO - skips queue
/// Busy waits on the LSR before writing the byte
///
/// Use for panic and sensitive lock-free printing, may well be interleaved
/// Can be used with interrupts disabled
/// Does not require UART initialisation
pub(crate) fn direct_write_byte(byte: u8) {
    while !LSR_tx_ready() {
        core::hint::spin_loop();
    }
    mmio::write8(uart::BASE, THR, byte);

    #[cfg(all(test, feature = "test-io"))]
    crate::io::test_io::capture(byte);
}

/// Initialise UART by enabling FIFOs and the receiving interrupt;
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

/// Prints to UART
///
/// Runs a closure with exclusive access to the UART_WRITER struct.
pub(crate) fn with_uart_writer<F, R>(f: F) -> R
where
    F: FnOnce(&mut UartWriter) -> R,
{
    assert!(
        interrupts::enabled(),
        "print macros must only be used with interrupts enabled"
    );
    let mut uart_writer = UART_WRITER.lock();
    f(&mut uart_writer)
}

/// Pop bytes from the TX queue and write them into the FIFO
/// with at most [FIFO_BYTES] written.
/// Mask THRE if the queue is empty (with a recheck in case of late push)
///
/// Takes a exclusive borrow of the Consumer to ensure only one caller at a time
fn tx_fill_fifo(consumer: &mut spsc::Consumer<'static, u8, { queue::TX_LEN }>) {
    if !LSR_tx_ready() {
        return; // We don't wait for the FIFO buffer to clear, return and wait for another interrupt
    }
    let mut sent = 0;
    while sent < uart::FIFO_SIZE
        && let Some(byte) = consumer.pop()
    {
        mmio::write8(uart::BASE, THR, byte);
        sent += 1;
    }
    // Check for empty queue, in which case we want to stop the interrupt
    if queue::TX.is_empty() {
        THRE_interrupt_disable();
        mmio::fence_mmio_write_to_mem_read();
        // Recheck in case of last push
        if !queue::TX.is_empty() {
            // OK, more bytes so reenable
            THRE_interrupt_enable();
        }
    }
}
/// Flush the TX queue
///
/// This has three cases:
/// 1. This is a blocking call if interrupts are enabled
/// 2. if interrupts are disabled on HART1, spin while waiting on the ISR to do the flushing for us
/// 3. if interrupts are disabled on HART0, busy wait polling on the transmission queue being empty.
///
/// Only call from kernel threads as it cannot be interrupted for thread teardown.
///
/// Note that full flush can take milliseconds so avoid using in critical sections.
#[allow(dead_code)]
pub(crate) fn flush() {
    if !queue::TX.is_empty() {
        // We know that there are bytes left so preemptively enable the THRE interrupt
        THRE_interrupt_enable();
        if interrupts::enabled() {
            // Park until the TX queue is empty
            let tx_guard = queue::TX_DRAINED.wait_with(
                &UART_WRITER,
                |_uart_writer| queue::TX.is_empty(), // Don't actually need the uart_writer resource, can answer directly from the queue's atomics
            );
            // Drop the guard right away on wake
            drop(tx_guard);
        } else if hart_id() == 0 {
            // Interrupts are disabled and we are HART0 (which would have received any THRE interrupts)
            // Drain synchronously under lock.
            if let Some(mut consumer) = queue::TX_CONSUMER.try_lock() {
                // Now pop all the bytes in the queue
                while !queue::TX.is_empty() {
                    // We need to busy wait on each FIFO_SIZE to not overflow the FIFO buffer
                    while !LSR_tx_ready() {
                        core::hint::spin_loop();
                    }
                    tx_fill_fifo(&mut consumer);
                }
            }
            // We couldn't get the lock - so probably a panic while the ISR
            // is holding the lock, so assume we need to flush what has made
            // it to the queue by polling on LSR's THRE (as interrupts can't be used).
            // We can't get hold of the TX consumer, so we can't pop the queue
            // So all that is left is what was in the FIFO
        } else {
            // We are HART1 with interrupts disabled - just spin while HART0 deals with the queue for us
            while !queue::TX.is_empty() {
                core::hint::spin_loop();
            }
        }
    }
    // Busy wait until sure the all bytes are out of the TSR too
    // Note that this can take milliseconds
    while !LSR_tx_done() {
        core::hint::spin_loop();
    }
}

/// Helper function to check the LSR to see if transmission register (THR) is empty
#[allow(non_snake_case)]
fn LSR_tx_ready() -> bool {
    let lsr = mmio::read8(uart::BASE, LSR);
    lsr & LSR_THRE != 0
}
/// Helper function to check the LSR to see if transmitter is empty (THR and TSR are both empty)
#[allow(non_snake_case)]
fn LSR_tx_done() -> bool {
    let lsr = mmio::read8(uart::BASE, LSR);
    lsr & LSR_TEMT != 0
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

/// UART Writer to hold the write_str trait
///
/// Owns the producer side of the TX queue.
/// Must be used as a static
/// It is the only producer for the transmission queue
pub(crate) struct UartWriter {
    producer: spsc::Producer<'static, u8, { queue::TX_LEN }>,
    lost: usize,
}

impl UartWriter {
    /// New UartWriter
    const fn new() -> Self {
        Self {
            // Safety: We never call split() and unchecked on the same queue
            producer: unsafe { queue::TX.producer_unchecked() },
            lost: 0,
        }
    }
    /// Push a single byte into the transmission queue
    /// If the push fails it is counted and a later write
    /// attempt will show the lost byte count
    fn write_byte(&mut self, byte: u8) -> Result<(), ()> {
        if self.producer.push(byte).is_ok() {
            #[cfg(all(test, feature = "test-io"))]
            crate::io::test_io::capture(byte);
            Ok(())
        } else {
            Err(())
        }
    }
}
/// Implement Write trait on sealed struct - only use via [with_uart_writer]
impl core::fmt::Write for UartWriter {
    /// Writes all the bytes in the string and
    /// enables the THRE interrupt when done
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.lost > 0 {
            let mut marker: StackVec<u8, 48> = StackVec::new();
            let _ = write!(marker, "\nLost bytes: {}\n", self.lost);
            if queue::TX.remaining() < marker.len() {
                // Still no room to report the gap: this write joins it.
                self.lost += s.len();
                return Ok(());
            }
            for &byte in marker.as_slice() {
                let _ = self.write_byte(byte);
            }
            self.lost = 0;
        }
        let dropped = s.bytes().filter(|&b| self.write_byte(b).is_err()).count();
        self.lost += dropped;
        mmio::fence_mem_write_to_mmio_write();
        THRE_interrupt_enable();
        Ok(())
    }
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
                let mut producer = queue::RX_PRODUCER
                    .try_lock()
                    .expect("must be able to take the lock's borrow as the only user");
                // The interrupt could be spurious so check if LSR agrees that a byte is ready
                while LSR_rx_ready() {
                    // RX data ready — read RBR to clear interrupt
                    let byte = mmio::read8(uart::BASE, RBR);
                    // We can safely push to this SPSC ring buffer because we set up
                    // the PLIC to only use one HART to handle interrupts
                    // so there is no need to serialise
                    let _ = producer.push(byte); // drop if full
                }
            }
            IIR_THR_EMPTY => {
                let mut consumer = queue::TX_CONSUMER.try_lock()
                .expect("flush should not be holding the consumer, as flush only takes the consumer if interrupts are disabled");
                tx_fill_fifo(&mut consumer);
                // Wake any queue threads waiting on TX being drained
                if queue::TX.is_empty() {
                    queue::TX_DRAINED.wake_all();
                }
                // After 16 bytes the FIFO is full and the trap needs to return, otherwise the loop will just continue
                // So we return, but the PLIC will keep the interrupt raised and we'll return to the trap within microseconds
                // Also, the RX interrupts are a higher level than TX so will be handled first
                return;
            }
            IIR_RX_LINE_STATUS => {
                // Unwanted line status interrupt — read LSR to clear
                let _ = mmio::read8(uart::BASE, LSR);
            }
            _ => {} // No interrupt pending or modem status — ignore
        }
    }
}

#[cfg(all(test, feature = "bench"))]
mod benchmarks {
    //! UART TX benchmarks: how long the *producer* spends inside
    //! `print!`. On QEMU the UART drains instantly, so these numbers are
    //! lock, ring and interrupt overhead rather than serial time.
    //!
    //! Two shapes, each measured on a thread pinned to hart 0 and again
    //! on one pinned to hart 1. The PLIC delivers every external
    //! interrupt to hart 0, so on hart 0 the THRE drain runs inside the
    //! measured window and on hart 1 it runs concurrently on the other
    //! hart. Hart 1 is therefore "enqueue only"; hart 0 is "enqueue plus
    //! drain".
    //!  - one 64-byte line with the ring empty: the no-wait path
    //!    (writer lock + push + THRE unmask).
    //!  - a 4 KB burst: with a ring smaller than that the producer must
    //!    wait for the drain, whichever way the driver implements that
    //!    wait; with a larger ring it is enqueue cost only.
    use crate::bench;
    use crate::kernel::alloc::Order;
    use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    /// 63 filler characters plus a newline.
    const LINE: &str = "...............................................................\n";
    const LINE_ITERS: u32 = 10;
    const BURST_ITERS: u32 = 5;
    /// 64 lines x 64 bytes = 4 KB. A fixed size so runs stay comparable
    /// across ring sizes (the 2026-09-08 baseline used a 1 KB ring, so
    /// there the burst overfilled it four times over).
    const BURST_LINES: u32 = (4096 / LINE.len()) as u32;
    const BURST_BYTES: u64 = BURST_LINES as u64 * LINE.len() as u64;

    struct Stats {
        min: u64,
        total: u64,
        n: u64,
    }
    impl Stats {
        fn new() -> Self {
            Stats {
                min: u64::MAX,
                total: 0,
                n: 0,
            }
        }
        fn add(&mut self, wall: u64) {
            self.min = self.min.min(wall);
            self.total += wall;
            self.n += 1;
        }
        fn avg(&self) -> u64 {
            self.total / self.n
        }
    }

    /// Results handed back from the pinned worker thread. riscv32 has no
    /// 64-bit atomics; anything over u32::MAX cycles (~4 s) saturates.
    static LINE_MIN: AtomicU32 = AtomicU32::new(0);
    static LINE_AVG: AtomicU32 = AtomicU32::new(0);
    static BURST_MIN: AtomicU32 = AtomicU32::new(0);
    static BURST_AVG: AtomicU32 = AtomicU32::new(0);

    fn clamp(v: u64) -> u32 {
        v.min(u32::MAX as u64) as u32
    }
    static RAN_ON: AtomicUsize = AtomicUsize::new(usize::MAX);
    static DONE: AtomicUsize = AtomicUsize::new(0);

    /// Let the ISR finish draining whatever the previous output left in
    /// the ring, so every measurement starts from an empty ring.
    fn settle() {
        crate::kernel::sched::sleep(20);
    }

    /// Wall on QEMU includes host stalls (tb_flush, timer jitter), so
    /// the min is the stable number to compare across driver designs;
    /// the average shows the noise.
    fn worker() {
        let mut line = Stats::new();
        for _ in 0..LINE_ITERS {
            settle();
            line.add(bench::measure(|| print!("{}", LINE)).wall);
        }
        let mut burst = Stats::new();
        for _ in 0..BURST_ITERS {
            settle();
            burst.add(
                bench::measure(|| {
                    for _ in 0..BURST_LINES {
                        print!("{}", LINE);
                    }
                })
                .wall,
            );
        }
        LINE_MIN.store(clamp(line.min), Ordering::Relaxed);
        LINE_AVG.store(clamp(line.avg()), Ordering::Relaxed);
        BURST_MIN.store(clamp(burst.min), Ordering::Relaxed);
        BURST_AVG.store(clamp(burst.avg()), Ordering::Relaxed);
        RAN_ON.store(crate::arch::hart_id(), Ordering::Relaxed);
        DONE.store(1, Ordering::Release);
        // Park rather than exit-by-yield: see feedback on ghost threads.
        crate::kernel::sched::sleep_until(u64::MAX);
    }

    fn run_pinned(hart: u8) {
        DONE.store(0, Ordering::Relaxed);
        let spawned = crate::kernel::sched::Builder::new()
            .with_stack_class(Order::KB8)
            .with_affinity(hart)
            .spawn(worker);
        assert!(spawned.is_some(), "uart bench worker spawn failed");
        let start = crate::kernel::timer::elapsed_ms();
        while DONE.load(Ordering::Acquire) == 0 {
            crate::kernel::sched::sleep(10);
            assert!(
                crate::kernel::timer::elapsed_ms() - start < 10_000,
                "uart bench worker on hart {hart} did not finish in 10 s"
            );
        }
        let ran_on = RAN_ON.load(Ordering::Relaxed);
        assert_eq!(ran_on, hart as usize, "worker ran on the wrong hart");
        println!();
        println!(
            "  [hart {}] print!(64-byte line, ring empty): wall min={} avg={} cycles ({} runs)",
            ran_on,
            LINE_MIN.load(Ordering::Relaxed),
            LINE_AVG.load(Ordering::Relaxed),
            LINE_ITERS
        );
        println!(
            "  [hart {}] print! burst of {} bytes (ring {} bytes): wall min={} avg={} cycles ({} runs), min {} cycles/byte",
            ran_on,
            BURST_BYTES,
            super::queue::TX_LEN,
            BURST_MIN.load(Ordering::Relaxed),
            BURST_AVG.load(Ordering::Relaxed),
            BURST_ITERS,
            BURST_MIN.load(Ordering::Relaxed) as u64 / BURST_BYTES
        );
    }

    #[test_case]
    fn uart_benchmarks() {
        println!();
        println!("====== UART TX ====== ");
        println!("  (filler lines below are the benchmark's own output)");
        run_pinned(0);
        run_pinned(1);
        println!();
        println!("===================== ");
        println!();
    }
}

// QEMU tests for the TX path: producer pushes to the ring and unmasks
// THRE; the interrupt handler moves at most FIFO_SIZE bytes per trap and
// masks THRE when the ring is empty (rechecking for a push that raced the
// mask). Observable from a thread: the ring drains to empty and ETBEI ends
// up clear. `test_io` captures bytes accepted into the ring, so it can
// show what was pushed but not what reached the wire — the drain checks
// below are what prove the interrupt side did its job.
#[cfg(all(test, feature = "test-io"))]
mod tests {
    use super::*;
    use crate::io::test_io;
    use crate::kernel::alloc::Order;
    use crate::kernel::sched::{self, Builder};
    use core::sync::atomic::{AtomicUsize, Ordering};

    fn thre_masked() -> bool {
        mmio::read8(uart::BASE, IER) & ETBEI == 0
    }

    /// Wait until the ring is empty and THRE has been masked, or give up.
    fn wait_drained(limit_ms: u64) -> bool {
        let mut waited = 0;
        while !(queue::TX.is_empty() && thre_masked()) {
            if waited >= limit_ms {
                return false;
            }
            sched::sleep(1);
            waited += 1;
        }
        true
    }

    // A single print far longer than the FIFO must be accepted whole (no
    // drop marker) and then fully drained across many THRE passes, with
    // the interrupt masked again at the end.
    #[test_case]
    fn uart_burst_longer_than_fifo_drains_whole() {
        assert!(wait_drained(500), "ring never idle before test");
        test_io::clear();
        const LEN: usize = 12 * uart::FIFO_SIZE + 5; // not a FIFO multiple
        print!("{:>LEN$}\n", "B");
        assert!(
            !test_io::contains("Lost bytes"),
            "a {LEN}-byte print inside a {}-byte ring must not drop",
            queue::TX_LEN
        );
        assert_eq!(
            test_io::output().len(),
            LEN + 1,
            "every byte accepted into the ring"
        );
        assert!(
            wait_drained(500),
            "ring did not drain: {} bytes left, THRE masked = {}",
            queue::TX.len(),
            thre_masked()
        );
    }

    // Lost-wakeup shape: let the ring go idle so the handler has masked
    // THRE, then print. The producer's unmask must bring the drain back.
    #[test_case]
    fn uart_line_after_idle_drains() {
        assert!(wait_drained(500), "ring never idle before test");
        sched::sleep(20);
        assert!(thre_masked(), "THRE should stay masked while idle");
        test_io::clear();
        println!("after idle");
        assert!(test_io::contains("after idle"));
        assert!(
            wait_drained(200),
            "line printed after idle was never drained (THRE unmask lost)"
        );
    }

    // flush() with interrupts on parks on the console wait queue until the
    // handler has drained the ring, then polls TEMT. On return nothing may
    // be left anywhere: ring empty, THRE masked, transmitter idle.
    #[test_case]
    fn uart_flush_blocks_until_wire_idle() {
        assert!(wait_drained(500), "ring never idle before test");
        print!("{:>500}\n", "F");
        // On hart 0 the handler may already have drained this before we
        // get here (QEMU re-raises THRE synchronously), so no precondition
        // on the ring: only the postconditions are guaranteed.
        flush();
        assert!(
            queue::TX.is_empty(),
            "flush returned with bytes still in the ring"
        );
        assert!(
            LSR_tx_done(),
            "flush returned before the transmitter went idle"
        );
    }

    // flush() with interrupts off cannot wait for the handler, so it must
    // drain the ring itself through the direct path. This is the panic /
    // exit-with-IRQs-masked shape.
    #[test_case]
    fn uart_flush_drains_synchronously_with_interrupts_off() {
        assert!(wait_drained(500), "ring never idle before test");
        print!("{:>300}\n", "S");
        crate::kernel::sync::with_interrupts_disabled(|_cs| {
            flush();
            assert!(
                queue::TX.is_empty(),
                "synchronous flush left bytes in the ring"
            );
            assert!(
                LSR_tx_done(),
                "synchronous flush returned before the transmitter went idle"
            );
        });
    }

    // Many short lines from a producer pinned to the other hart while the
    // handler on hart 0 drains and masks concurrently. Exercises the
    // mask-then-recheck path under real cross-hart timing. Completion plus
    // an empty ring at the end is the pass; a lost unmask leaves bytes
    // stranded and the drain wait fires.
    #[test_case]
    fn uart_cross_hart_producer_fully_drained() {
        const LINES: usize = 300;
        static DONE: AtomicUsize = AtomicUsize::new(0);
        DONE.store(0, Ordering::Relaxed);
        assert!(wait_drained(500), "ring never idle before test");
        test_io::clear();

        fn producer() {
            for i in 0..LINES {
                println!("x{i}");
                // Pace the producer so the ring never fills: on QEMU a push
                // costs nanoseconds and a drained byte ~30 us of host time,
                // so an unpaced loop overruns any ring and drops by design.
                // Paced, a drop can only mean the drain stalled.
                if i % 10 == 9 {
                    sched::sleep(1);
                }
            }
            DONE.store(1, Ordering::Release);
        }
        Builder::new()
            .with_stack_class(Order::KB2)
            .with_affinity(1)
            .spawn(producer)
            .expect("producer should spawn");

        let mut waited = 0;
        while DONE.load(Ordering::Acquire) == 0 {
            assert!(waited < 2000, "producer never finished");
            sched::sleep(10);
            waited += 10;
        }
        assert!(
            wait_drained(500),
            "cross-hart output not fully drained: {} bytes left, THRE masked = {}",
            queue::TX.len(),
            thre_masked()
        );
        assert!(
            !test_io::contains("Lost bytes"),
            "paced cross-hart producer dropped bytes: the drain stalled behind a masked THRE"
        );
    }
}
