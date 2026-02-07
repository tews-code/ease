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

// Tests
#[cfg(test)]
mod test {
    use crate::io::test_io;
    use crate::{print, println};

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
    // Original UART-only baselines (Phase 1-4):
    //   PRINT_HELLO:      180,000  (measured ~31,000)
    //   PRINTLN_HELLO:    240,000  (measured ~25,000)
    //   PRINTLN_FORMATTED:320,000  (measured ~30,000)
    //   PRINTLN_LONG:     750,000  (measured ~138,000)
    // Updated: print! now writes to UART + framebuffer console (Phase 5-6)
    // Console reset before benchmarks to avoid scroll cost
    // Each char draws 8x16 glyph + cursor hide/show = ~384 pixel writes
    // Baselines set with wide margin for QEMU timing variance
    pub const PRINT_HELLO: u64 = 1_000_000;
    pub const PRINTLN_HELLO: u64 = 900_000;
    pub const PRINTLN_FORMATTED: u64 = 3_000_000;
    pub const PRINTLN_LONG: u64 = 3_500_000;
}

#[cfg(test)]
mod profile {
    use crate::bench;
    use crate::drivers::console::CONSOLE;
    use crate::hal::ascii;
    use crate::println;
    use core::fmt::Write;

    const ITER_LARGE: u32 = 100;
    const ITER_SMALL: u32 = 10;

    #[test_case]
    fn profile_console_print() {
        use crate::drivers::font::Font;
        use crate::drivers::ramfb::{Colour, set_pixels};

        println!("\n=== Console Print Path Profile ===");

        let mut c = CONSOLE.lock();
        c.clear();
        bench::run_avg("set_pixels", ITER_LARGE, || {
            set_pixels(
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
            )
        });
        c.clear();
        bench::run_avg("Font::draw_char", ITER_LARGE, || {
            Font::draw_char(0, 0, b'X', Colour::WHITE, Colour::BLUE)
        });
        c.clear();
        bench::run_avg("Console::write_char(ch)", ITER_SMALL, || c.write_char(b'X'));
        c.clear();
        bench::run_avg("Console::write_char(LF)", ITER_SMALL, || {
            c.write_char(ascii::LF)
        });
        c.clear();
        bench::run_avg("Console::write_str(\"hello\\n\")", ITER_SMALL, || {
            let _ = c.write_str("hello\n");
        });
        println!("==================================");
    }

    #[test_case]
    fn profile_console_scroll() {
        println!("\n=== Console Scroll Path Profile ===");
        let mut c = CONSOLE.lock();
        c.clear();
        bench::run_avg("Console::scroll()", ITER_LARGE, || {
            c.scroll();
        });
        println!("====================================");
    }
}

#[cfg(test)]
mod benchmarks {
    use super::baselines;
    use crate::bench;
    use crate::drivers::console::CONSOLE;
    use crate::io::test_io;
    use crate::{print, println};

    /// Number of iterations for averaging (reduces noise)
    const ITERATIONS: u32 = 10;

    /// Reset console before benchmarks to avoid scroll cost dominating measurements
    fn reset_console() {
        CONSOLE.lock().clear();
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
