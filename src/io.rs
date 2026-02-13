//! I/O abstractions for EASE
//!
//! Provides traits and implementations for byte-level I/O.
//! In test mode, output is captured to a buffer for verification.

pub use crate::drivers::uart::UartWriter;

/// Print to Console and UART
///
/// Prints formatted string to Console and UART.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::io::UartWriter, $($arg)*);
        // Also console if available.
        let mut c = $crate::drivers::DISPLAY.lock();
        let _ = write!(c, $($arg)*);
    }}
}

/// Print to Console and UART with newline
///
/// Prints formatted string to Console and UART with trailing newline.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! println {
    () => {{ $crate::print!("\n"); }};
    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*));
    }}
}

/// Print to UART only
///
/// Prints formatted string to Console and UART.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! printd {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::io::UartWriter, $($arg)*);
    }}
}

/// Print to UART only with newline
///
/// Prints formatted string to UART with trailing newline.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! printdln {
    () => { {
        $crate::printd!("\n");
    }};
    ($($arg:tt)*) => {{
        $crate::printd!("{}\n", format_args!($($arg)*));
    }}
}

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

// Tests
#[cfg(test)]
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

// =============================================================================
// Benchmarks (QEMU)
// =============================================================================

/// Baseline cycle counts for regression detection.
/// Update these when intentionally changing performance.
/// Run `cargo test --bin ease` to see current measurements.
/// Baselines set ~20% above measured values to allow for variance.
#[cfg(test)]
mod baselines {
    // Baselines measured with profile.test opt-level = 1
    // Set at ~2x measured values for QEMU timing variance
    pub const PRINT_HELLO: u64 = 300_000;
    pub const PRINTLN_HELLO: u64 = 2_500_000;
    pub const PRINTLN_FORMATTED: u64 = 500_000;
    pub const PRINTLN_LONG: u64 = 4_000_000;
}

#[cfg(test)]
mod profile {
    use crate::bench;
    use crate::drivers::DISPLAY;
    use crate::hal::ascii;
    use core::fmt::Write;

    const ITER_LARGE: u32 = 100;
    const ITER_SMALL: u32 = 10;

    #[test_case]
    fn profile_console_print() {
        use crate::drivers::font::Font;
        use crate::drivers::ramfb::Colour;

        printdln!("\n=== Console Print Path Profile ===");

        let mut d = DISPLAY.lock();
        d.put_char(ascii::FF);
        let mut renderer = d.release_to_app().unwrap();
        bench::run_avg("set_pixels", ITER_LARGE, || {
            renderer.fb.set_pixels(
                0,
                0,
                &[
                    Colour::RED.as_raw(),
                    Colour::BLUE.as_raw(),
                    Colour::RED.as_raw(),
                    Colour::BLUE.as_raw(),
                    Colour::RED.as_raw(),
                    Colour::BLUE.as_raw(),
                    Colour::RED.as_raw(),
                    Colour::BLUE.as_raw(),
                ],
            );
        });
        d.return_to_console(renderer);
        d.put_char(ascii::FF);
        let mut renderer = d.release_to_app().unwrap();
        bench::run_avg("Font::draw_char", ITER_LARGE, || {
            Font::draw_char(&mut renderer.fb, 0, 0, b'X', Colour::WHITE, Colour::BLUE)
        });
        d.return_to_console(renderer);
        d.put_char(ascii::FF);
        d.put_char(ascii::FF);
        bench::run_avg("Console::put_char(ch)", ITER_SMALL, || d.put_char(b'X'));
        d.put_char(ascii::FF);
        bench::run_avg("Console::show+hide_cursor", ITER_SMALL, || {
            d.show_cursor();
            d.hide_cursor();
        });
        d.put_char(ascii::FF);
        bench::run_avg("Console::write_char(ch)", ITER_SMALL, || {
            d.write_char(b'X' as char)
                .expect("should be able to write char")
        });
        d.put_char(ascii::FF);
        bench::run_avg("Console::write_char(LF)", ITER_SMALL, || {
            d.write_char(ascii::LF as char)
                .expect("should be able to write line feed")
        });
        d.put_char(ascii::FF);
        bench::run_avg("Console::write_str(\"hello\\n\")", ITER_SMALL, || {
            let _ = d.write_str("hello\n");
        });
        printdln!("==================================");
    }

    #[test_case]
    fn profile_console_scroll() {
        println!("\n=== Console Scroll Path Profile ===");
        let mut d = DISPLAY.lock();
        d.put_char(ascii::FF);
        // Position cursor at last row so each LF triggers a scroll
        for _ in 0..29 {
            d.put_char(ascii::LF);
        }
        bench::run_avg("Console::write_char(LF) with scroll", ITER_LARGE, || {
            d.write_char(ascii::LF as char).unwrap();
        });
        drop(d);
        println!("====================================");
    }
}

#[cfg(test)]
mod benchmarks {
    use super::baselines;
    use crate::bench;
    use crate::drivers::DISPLAY;
    use crate::hal::ascii;
    use crate::io::test_io;

    /// Number of iterations for averaging (reduces noise)
    const ITERATIONS: u32 = 10;

    /// Reset console before benchmarks to avoid scroll cost dominating measurements
    fn reset_console() {
        DISPLAY.lock().put_char(ascii::FF);
    }

    #[test_case]
    fn regression_print_hello() {
        println!();
        println!("=== Regression Checks ===");
        reset_console();
        test_io::clear();
        bench::check("print!(hello)", baselines::PRINT_HELLO, ITERATIONS, || {
            print!("hello");
        });
    }

    #[test_case]
    fn regression_println_hello() {
        reset_console();
        test_io::clear();
        bench::check(
            "println!(hello)",
            baselines::PRINTLN_HELLO,
            ITERATIONS,
            || {
                println!("hello");
            },
        );
    }

    #[test_case]
    fn regression_println_formatted() {
        reset_console();
        test_io::clear();
        bench::check(
            "println!(formatted)",
            baselines::PRINTLN_FORMATTED,
            ITERATIONS,
            || {
                println!("num: {}", 42);
            },
        );
    }

    #[test_case]
    fn regression_println_long() {
        reset_console();
        test_io::clear();
        bench::check(
            "println!(50 chars)",
            baselines::PRINTLN_LONG,
            ITERATIONS,
            || {
                println!("the quick brown fox jumps over the lazy dog!!");
            },
        );
    }
}
