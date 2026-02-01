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

extern crate alloc;

use core::sync::atomic::AtomicUsize;

mod arch;
mod bench;
mod hal;
mod io;
mod kernel;
mod qemu;

#[used]
static BSS_TEST: AtomicUsize = AtomicUsize::new(0);

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("⚠️ PANIC! {info}");
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
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
        println!("[\x1b[32mok\x1b[0m]");
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
    kernel::alloc::init();
    test_main();
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    kernel::alloc::init();
    print!("Hello ");
    println!("from EASE!");
    unsafe {
        core::arch::asm!("unimp");
    }
    loop {
        core::hint::spin_loop();
    }
}

// =============================================================================
// Tests (QEMU)
// =============================================================================

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    use crate::{BSS_TEST, io::test_io};

    #[test_case]
    fn test_bss_zeroed() {
        assert_eq!(BSS_TEST.load(Ordering::Relaxed), 0);
    }

    #[test_case]
    fn test_vec_allocation() {
        use alloc::vec::Vec;
        let mut v = Vec::new();
        v.push(1);
        v.push(2);
        v.push(3);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], 1);
    }

    #[test_case]
    fn test_string_allocation() {
        use alloc::string::String;
        let s = String::from("hello heap!");
        assert!(s.contains("heap"));
    }

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
    pub const PRINT_HELLO: u64 = 38_000; // Measured: ~31,000
    pub const PRINTLN_HELLO: u64 = 32_000; // Measured: ~25,000
    pub const PRINTLN_FORMATTED: u64 = 36_000; // Measured: ~30,000
    pub const PRINTLN_LONG: u64 = 165_000; // Measured: ~138,000
    pub const BOX_NEW_U64: u64 = 18_000; // Measured: ~15,000
    pub const VEC_PUSH_100_ITEMS: u64 = 90_000; // Measured: ~53,000
    pub const STRING_FROM_SHORT: u64 = 20_000; // Measured: ~16,000
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

    #[test_case]
    fn bench_small_allocation() {
        use alloc::vec::Vec;
        use core::hint::black_box;
        test_io::clear();
        bench::check(
            "Vec::push 100 items",
            baselines::VEC_PUSH_100_ITEMS,
            ITERATIONS,
            || {
                let mut v: Vec<u32> = Vec::new();
                for i in 0..100 {
                    v.push(black_box(i));
                }
            },
        );
    }

    #[test_case]
    fn bench_string_allocation() {
        use alloc::string::String;
        test_io::clear();
        bench::check(
            "String::from short",
            baselines::STRING_FROM_SHORT,
            ITERATIONS,
            || {
                let _ = String::from("hello");
            },
        );
    }

    #[test_case]
    fn bench_box_allocation() {
        use alloc::boxed::Box;
        use core::hint::black_box;
        test_io::clear();
        bench::check("Box::new u64", baselines::BOX_NEW_U64, ITERATIONS, || {
            let _ = Box::new(black_box(42u64));
        });
    }
}
