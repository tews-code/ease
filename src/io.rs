//! I/O abstractions for EASE
//!
//! Provides traits and implementations for byte-level I/O.
//! In test mode, output is captured to a buffer for verification.

pub use crate::hal::qemu_virt::UartWriter;

// =============================================================================
// Test I/O Capture
// =============================================================================

/// Test I/O capture functionality
///
/// Captures all output bytes to a static buffer for test verification.
/// Only available in test builds.
#[cfg(test)]
pub mod test_io {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicUsize, Ordering};

    const BUFFER_SIZE: usize = 4096;

    /// Static buffer for capturing test output
    struct CaptureBuffer {
        data: UnsafeCell<[u8; BUFFER_SIZE]>,
        len: AtomicUsize,
    }

    // Safety: Tests run single-threaded, and we use atomic for the length
    unsafe impl Sync for CaptureBuffer {}

    static BUFFER: CaptureBuffer = CaptureBuffer {
        data: UnsafeCell::new([0; BUFFER_SIZE]),
        len: AtomicUsize::new(0),
    };

    /// Capture a byte to the test buffer
    pub fn capture(byte: u8) {
        let idx = BUFFER.len.fetch_add(1, Ordering::Relaxed);
        if idx < BUFFER_SIZE {
            // Safety: Single-threaded test execution, index is unique per call
            unsafe {
                let data: &mut [u8; BUFFER_SIZE] = &mut *BUFFER.data.get();
                data[idx] = byte;
            }
        }
    }

    /// Clear the capture buffer
    ///
    /// Call this before the code under test to isolate its output.
    pub fn clear() {
        BUFFER.len.store(0, Ordering::Relaxed);
    }

    /// Get captured output as a string slice
    ///
    /// Returns all bytes captured since the last `clear()`.
    pub fn output() -> &'static str {
        let len = BUFFER.len.load(Ordering::Relaxed).min(BUFFER_SIZE);
        // Safety: We only write valid UTF-8 via print!/println!
        // The explicit reference silences dangerous_implicit_autorefs lint
        unsafe {
            let data: &[u8; BUFFER_SIZE] = &*BUFFER.data.get();
            core::str::from_utf8_unchecked(&data[..len])
        }
    }

    /// Check if captured output contains a substring
    pub fn contains(expected: &str) -> bool {
        output().contains(expected)
    }

    /// Check if captured output equals expected string exactly
    pub fn equals(expected: &str) -> bool {
        output() == expected
    }
}
