//! Console input
//!
//! Supports reading input pushed into source rings

use crate::drivers::uart::UartReader;
use crate::kernel::sync::{Interrupted, SpinLock, WaitQueue};

/// Main reader
/// Note that this can only be used with interrupts enabled
static READER: SpinLock<UartReader> = SpinLock::new(
    // Safety: This is the only place we create the UartReader.
    unsafe { UartReader::new() },
);

/// Wait queue for threads blocked on input
static AVAIL: WaitQueue = WaitQueue::new();

/// Called by input sources when they have pushed keys onto the queue
///
/// Wakes any threads sleeping on the [AVAIL] queue
pub(crate) fn notify() {
    AVAIL.wake_all();
}

/// Provides user thread access to the [AVAIL] wait queue.
///
/// Returns the number of bytes received on success.
///
/// # Errors
/// Returns [Interrupted] if the user thread's process is being torn down while this thread
/// was sleeping
pub(crate) fn wait_and_read_interruptible(buf: &mut [u8]) -> Result<usize, Interrupted> {
    if buf.is_empty() {
        return Ok(0);
    }
    let mut bytes_read: usize = 0;
    // Block until the RX queue has bytes available
    AVAIL.wait_with_interruptible(&READER, |reader| {
        bytes_read = reader.read_bytes(buf);
        bytes_read > 0
    })?;
    Ok(bytes_read)
}
