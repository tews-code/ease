//! EASE - OS for RP2350
//!
//! A hobby OS for Raspberry Pi Pico 2 in RISC-V mode.
//!
//! # Boot Process (QEMU virt)
//!
//! 1. QEMU loads the kernel binary at `0x80000000` (RAM base, set in `memory-qemu.x`)
//! 2. CPU begins execution at the `ENTRY` symbol: [`_start`]
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
//! RAM spans `0x80000000` to `0x81800000` (32 MB on QEMU virt).

#![no_std]
#![no_main]
#![warn(missing_docs)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

mod arch;
mod bench;
mod board;
mod drivers;
mod fs;
mod hal;
mod input;
mod io;
mod kernel;
mod qemu;
mod shell;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    printdln!("PANIC! {info}");
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    printdln!("\x1b[31mfailed\x1b[0m");
    printdln!("Error: {}", info);
    qemu::exit_failure();
}

// =============================================================================
// Entry Points
// =============================================================================

fn kernel_init() {
    kernel::stack_guard::init();
    kernel::alloc::init();
    kernel::timer::init();

    // Configure PLIC
    drivers::plic::set_threshold(0);
    drivers::plic::set_priority(board::plic::UART0_IRQ, 1);
    drivers::plic::enable(board::plic::UART0_IRQ);
    drivers::plic::set_priority(board::plic::VIRTIO0_IRQ, 1);
    drivers::plic::enable(board::plic::VIRTIO0_IRQ);
    arch::csr::mie::enable_bits(arch::csr::mie::MEIE);

    drivers::uart::enable_rx_interrupt();

    arch::enable_interrupts();

    drivers::virtio::virtio_blk_init();
    let fb = drivers::ramfb::FrameBuffer::init();
    let fbr = drivers::render::FrameBufferRenderer::new(
        fb,
        drivers::ramfb::Colour::WHITE,
        drivers::ramfb::Colour::BLACK,
    );
    drivers::DISPLAY.lock().init(fbr);
}

#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    kernel_init();

    // Start the shell
    let _shell = shell::Shell::new();

    test_main();
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    kernel_init();

    println!("Hello from EASE!");
    // Start the shell
    let mut shell = shell::Shell::new();
    shell.run(); // Never returns
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
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[used]
    static BSS_TEST: AtomicUsize = AtomicUsize::new(0);

    #[test_case]
    fn test_bss_zeroed() {
        assert_eq!(BSS_TEST.load(Ordering::Relaxed), 0);
    }

    /// Verify UART output works while DISPLAY lock is held.
    /// Before the fix, this scenario would deadlock in the panic handler
    /// (and any printdln! while DISPLAY was locked would also deadlock
    /// if it had used println! instead).
    #[test_case]
    fn test_printdln_while_display_locked() {
        let _guard = crate::drivers::DISPLAY.lock();
        crate::printdln!("UART works while DISPLAY is locked");
        // If we reach here, no deadlock occurred
    }
}
