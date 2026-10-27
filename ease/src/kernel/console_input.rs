//! Console input
//!
//! Supports reading input pushed into source rings

use crate::drivers::keyboard;
use crate::drivers::uart;
use crate::kernel::sync::{Interrupted, SpinLock, WaitQueue};

/// Main reader
/// Note that this can only be used with interrupts enabled
static READER: SpinLock<Reader> = SpinLock::new(
    // Safety: This is the only place we create the readers.
    unsafe { Reader::new() },
);

/// Wait queue for threads blocked on input
static AVAIL: WaitQueue = WaitQueue::new();

/// Reader which holds the sources
struct Reader {
    keyboard: keyboard::Reader,
    uart: uart::Reader,
}

impl Reader {
    /// Create a new [Reader] which holds the consumer side of all source SPSC queues
    ///
    /// # Safety
    /// The caller must ensure that there is only one Reader as it holds all the (unique)
    /// source consumers.
    pub(crate) const unsafe fn new() -> Self {
        // Safety: We only create the source queue consumers here (once)
        Self {
            keyboard: unsafe { keyboard::Reader::new() },
            uart: unsafe { uart::Reader::new() },
        }
    }
}

/// Called by input sources when they have pushed keys onto the queue
///
/// Wakes any threads sleeping on the [AVAIL] queue
pub(crate) fn notify() {
    AVAIL.wake_all();
}

/// Provides user thread access to a wait queue on the input sources.
/// The sources are always drained in the order:
/// 1. UART first until fully drained
/// 2. Keyboard second until fully drained.
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
    // Block until any of the READER sources queues have bytes available
    AVAIL.wait_with_interruptible(&READER, |reader| {
        // We try the UART and drain all
        bytes_read = reader.uart.read_bytes(buf);
        // Now try the console keyboard and drain all
        bytes_read += reader.keyboard.read_keys(&mut buf[bytes_read..]);
        // Confirm we got some bytes
        bytes_read > 0
    })?;
    Ok(bytes_read)
}
