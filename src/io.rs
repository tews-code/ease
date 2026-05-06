//! I/O abstractions for EASE
//!
//! Provides traits and implementations for byte-level I/O.
//! In test mode, output is captured to a buffer for verification.

/// Direct UART writer that bypasses TX buffer.
///
/// Safe to use from interrupt handlers and panic handler.
pub struct DirectWriter;

impl core::fmt::Write for DirectWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            crate::drivers::uart::direct_write_byte(b);
        }
        Ok(())
    }
}

/// Print to UART only
///
/// Prints formatted string to UART without locking.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        use $crate::drivers::uart::with_uart_writer;
        let _ = with_uart_writer(|w| write!(w, $($arg)*));
    }}
}

/// Print to UART only with newline
///
/// Prints formatted string to UART with trailing newline.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! println {
    () => { {
        $crate::print!("\n");
    }};
    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*));
    }}
}

// =============================================================================
// Test I/O Capture
// =============================================================================

/// Test I/O capture functionality
///
/// Captures all output bytes to a static buffer for test verification.
/// Only available in test builds.

#[cfg(all(test, feature = "test-io"))]
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

// Tests
#[cfg(all(test, feature = "test-io"))]
mod test {
    use crate::io::test_io;

    #[test_case]
    fn test_println_output() {
        test_io::clear();
        println!("hello");
        assert!(test_io::equals("hello\n"));
    }

    #[test_case]
    fn test_print_no_newline() {
        test_io::clear();
        print!("abc");
        assert!(test_io::equals("abc"));
    }

    #[test_case]
    fn test_println_formatted() {
        test_io::clear();
        println!("count: {}", 42);
        assert!(test_io::equals("count: 42\n"));
    }

    #[test_case]
    fn test_multiple_prints() {
        test_io::clear();
        print!("one ");
        print!("two ");
        println!("three");
        assert!(test_io::equals("one two three\n"));
    }

    #[test_case]
    fn test_output_contains() {
        test_io::clear();
        println!("the quick brown fox");
        assert!(test_io::contains("quick"));
        assert!(test_io::contains("brown"));
        assert!(!test_io::contains("lazy"));
    }
}
