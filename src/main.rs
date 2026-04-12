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

#[allow(unused_imports)]
use core::fmt::Write;

use crate::drivers::ramfb::FrameBuffer;
#[cfg(feature = "alloc-bump")]
use crate::kernel::alloc::bump::Bump;
#[cfg(feature = "alloc-freelist")]
use crate::kernel::alloc::freelist::FreeBlockList;
#[cfg(feature = "alloc-slab")]
use crate::kernel::alloc::slab::Slab;

extern crate alloc;

mod arch;
mod bench;
mod board;
mod drivers;
mod fs;
mod hal;
mod io;
mod kernel;
mod qemu;
mod shell;

// =============================================================================
// Heap and Global Allocator
// =============================================================================
//
// The global allocator instance lives here in the binary crate (not in
// kernel/alloc/freelist.rs) so that the `#[global_allocator]` attribute and
// the linker-symbol references stay confined to code that is only ever
// compiled for the kernel target. This keeps the FreeBlockList type itself
// host-testable without polluting the lib crate.

// Safety: Symbols are created in the linker script with valid addresses
// and are FreeBlock-aligned (16-byte ALIGN in the .heap section).
unsafe extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

#[global_allocator]
#[cfg(feature = "alloc-freelist")]
pub(crate) static FREE_BLOCK_LIST: FreeBlockList = FreeBlockList::new();

#[global_allocator]
#[cfg(feature = "alloc-slab")]
pub(crate) static SLAB: Slab = Slab::new();

#[global_allocator]
#[cfg(feature = "alloc-bump")]
pub(crate) static BUMP: Bump = Bump::new();

/// Initialise the global allocator from the linker-defined heap region.
/// Must be called exactly once during boot, before any allocations.
fn init_global_allocator() {
    let start = &raw const __heap_start as *mut u8;
    let size = &raw const __heap_end as usize - start as usize;
    // Safety: The heap region is defined by the linker, exclusively owned
    // by the allocator, FreeBlock-aligned, and large enough to hold a
    // FreeBlock header.
    #[cfg(feature = "alloc-freelist")]
    unsafe {
        FREE_BLOCK_LIST.init(start, size)
    };
    #[cfg(feature = "alloc-slab")]
    unsafe {
        SLAB.init(start, size)
    };
    #[cfg(feature = "alloc-bump")]
    unsafe {
        BUMP.init(start, size)
    };
}

/// Returns the address of `__heap_start` for diagnostics (e.g. computing
/// heap-used in benchmarks). Only referenced from the `#[cfg(test)]`
/// allocator benchmarks.
#[cfg(all(test, feature = "test-alloc"))]
pub(crate) fn heap_start_addr() -> usize {
    &raw const __heap_start as usize
}

// =============================================================================
// Entry Points
// =============================================================================

fn kernel_init() -> FrameBuffer {
    kernel::stack_guard::init();
    kernel::timer::init();
    init_global_allocator();

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
    fs::volume::fat16_init();

    drivers::ramfb::FrameBuffer::init()
}

#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    let _fb = kernel_init();

    test_main();
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    let fb = kernel_init();
    let mut console = shell::console::Console::new(fb);
    let _ = writeln!(console, "Hello from EASE!");
    println!("Hello from EASE!");
    // Start the shell
    let mut shell = shell::Shell::new(console);
    shell.run();
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

#[cfg(all(test, feature = "test-bss"))]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[used]
    static BSS_TEST: AtomicUsize = AtomicUsize::new(0);

    #[test_case]
    fn test_bss_zeroed() {
        assert_eq!(BSS_TEST.load(Ordering::Relaxed), 0);
    }
}
