//! EASE - OS for RP2350
//!
//! A hobby OS for Raspberry Pi Pico 2 in RISC-V mode.
//!
//! # Boot Process (QEMU virt)
//!
//! 1. QEMU loads the kernel binary at `0x80000000` (RAM base, set in `memory-qemu.x`)
//! 2. CPU begins execution at the `ENTRY` symbol: [`_start`]
//! 3. Currently just loops forever — no stack, BSS, or hardware init yet
//!
//! # Memory Layout
//!
//! Defined in `memory-qemu.x` for QEMU's `virt` machine:
//!
//! | Section   | Location | Contents                    |
//! |-----------|----------|-----------------------------|
//! | `.text`   | RAM      | Executable code             |
//! | `.rodata` | RAM      | Read-only data (strings)    |
//! | `.data`   | RAM      | Initialized mutable data    |
//! | `.bss`    | RAM      | Zero-initialized data       |
//!
//! RAM spans `0x80000000` to `0x88000000` (128 MB on QEMU virt).

#![no_std]
#![no_main]
#![warn(missing_docs)]

#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

use core::arch::naked_asm;

mod bench;
mod io;
mod qemu;

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[failed]");
    println!("Error: {}", info);
    qemu::exit_failure();
}

/// Print to UART
///
/// Prints formatted string to UART.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::io::UartWriter, $($arg)*);
    }}
}

/// Print to UART with newline
///
/// Prints formatted string to UART with trailing newline.
/// In test mode, output is also captured for verification.
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
}

// =============================================================================
// Test Framework
// =============================================================================

/// Trait for test cases that can be run by the test framework
#[cfg(test)]
trait Testable {
    /// Run the test and print status
    fn run(&self);
}

#[cfg(test)]
impl<T: Fn()> Testable for T {
    fn run(&self) {
        print!("{}...\t", core::any::type_name::<T>());
        self();
        println!("[ok]");
    }
}

/// Custom test runner for QEMU
///
/// Runs all test cases and exits QEMU with appropriate status code.
#[cfg(test)]
fn test_runner(tests: &[&dyn Testable]) {
    println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    println!();
    println!("All tests passed!");
    qemu::exit_success();
}

// =============================================================================
// Entry Points
// =============================================================================

#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    test_main();
    loop {}
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    print!("Hello ");
    println!("from EASE!");
    loop {}
}

#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    naked_asm!(
        "li sp, 0x80100000",
        "j main",
        "unimp",
    );
}

// =============================================================================
// Tests (QEMU)
// =============================================================================

#[cfg(test)]
mod tests {
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
    pub const PRINT_HELLO: u64 = 38_000;      // Measured: ~31,000
    pub const PRINTLN_HELLO: u64 = 32_000;    // Measured: ~25,000
    pub const PRINTLN_FORMATTED: u64 = 36_000; // Measured: ~30,000
    pub const PRINTLN_LONG: u64 = 165_000;    // Measured: ~138,000
}

#[cfg(test)]
mod benchmarks {
    use super::baselines;
    use crate::bench;
    use crate::io::test_io;

    /// Number of iterations for averaging (reduces noise)
    const ITERATIONS: u32 = 5;

    #[test_case]
    fn regression_print_hello() {
        println!();
        println!("=== Regression Checks ===");
        test_io::clear();
        bench::check("print!(hello)", baselines::PRINT_HELLO, ITERATIONS, || {
            print!("hello");
        });
    }

    #[test_case]
    fn regression_println_hello() {
        test_io::clear();
        bench::check("println!(hello)", baselines::PRINTLN_HELLO, ITERATIONS, || {
            println!("hello");
        });
    }

    #[test_case]
    fn regression_println_formatted() {
        test_io::clear();
        bench::check("println!(formatted)", baselines::PRINTLN_FORMATTED, ITERATIONS, || {
            println!("num: {}", 42);
        });
    }

    #[test_case]
    fn regression_println_long() {
        test_io::clear();
        bench::check("println!(50 chars)", baselines::PRINTLN_LONG, ITERATIONS, || {
            println!("the quick brown fox jumps over the lazy dog!!");
        });
    }
}
