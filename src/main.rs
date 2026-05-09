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
use core::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::ramfb::FrameBuffer;
use crate::kernel::sched;
use crate::kernel::sync::IrqSpinLock;

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

static FB_HANDOFF: IrqSpinLock<Option<FrameBuffer>> = IrqSpinLock::new(None);
static INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

// =============================================================================
// Thread Test Function
// =============================================================================

#[cfg(all(feature = "test-sched", not(test)))]
mod test_sched {
    use crate::kernel::sched;
    use crate::print;
    pub fn thread1() -> ! {
        loop {
            print!("B");
            sched::sleep(500);
            sched::yield_now();
        }
    }
    pub fn thread2() -> ! {
        loop {
            print!("C");
            sched::sleep(300);
            sched::yield_now();
        }
    }
    pub fn thread3() -> ! {
        loop {
            print!("X");
            sched::sleep(300);
        }
    }
}

// =============================================================================
// Entry Points
// =============================================================================

#[allow(dead_code)]
fn shell_thread() -> ! {
    let fb = FB_HANDOFF.lock().take().expect("FB already handed off");
    let console = shell::console::Console::new(fb);
    let mut shell = shell::Shell::new(console);
    shell.run();
    #[allow(unreachable_code)]
    loop {
        crate::arch::wait_for_interrupt();
    }
}

fn secondary_init() {
    kernel::timer::init();
    sched::bootstrap(1);
    arch::enable_interrupts();
}

fn kernel_init() {
    kernel::timer::init();
    kernel::alloc::init_global_allocator();

    // Configure PLIC
    drivers::plic::set_threshold(0);
    drivers::plic::set_priority(board::plic::UART0_IRQ, 1);
    drivers::plic::enable(board::plic::UART0_IRQ);
    drivers::plic::set_priority(board::plic::VIRTIO0_IRQ, 1);
    drivers::plic::enable(board::plic::VIRTIO0_IRQ);
    arch::csr::mie::enable_bits(arch::csr::mie::MEIE);

    drivers::uart::enable_rx_interrupt();

    sched::bootstrap(0);
    arch::enable_interrupts();

    // Large-allocation init — skipped when the slab is the sole allocator,
    // because the slab cannot serve the virtq and FAT buffers these need.
    #[cfg(not(feature = "alloc-slab"))]
    {
        drivers::virtio::virtio_blk_init();
        fs::volume::fat16_init();
    }

    #[cfg(all(feature = "test-sched", not(test)))]
    {
        sched::spawn(
            test_sched::thread1,
            sched::PRIORITY_DEFAULT,
            sched::StackClass::KB4,
        );
        sched::spawn(
            test_sched::thread2,
            sched::PRIORITY_DEFAULT,
            sched::StackClass::KB4,
        );
        sched::spawn(
            test_sched::thread3,
            sched::PRIORITY_DEFAULT,
            sched::StackClass::KB4,
        );
    }

    let fb = drivers::ramfb::FrameBuffer::init();
    *FB_HANDOFF.lock() = Some(fb);

    // Set the flag
    INIT_COMPLETE.store(true, Ordering::Release);
}

#[unsafe(no_mangle)]
extern "C" fn secondary_main() -> ! {
    // Spin on initialisation completion
    while !INIT_COMPLETE.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    secondary_init();
    println!("Hello from HART{}!", crate::arch::cpu_id());
    sched::idle_thread();
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
    kernel_init();
    println!("Hello from EASE HART{}!", crate::arch::cpu_id());

    #[allow(clippy::diverging_sub_expression)]
    let Some(id) = sched::spawn(
        shell_thread(),
        #[allow(unreachable_code)]
        sched::PRIORITY_DEFAULT,
        sched::StackClass::KB8,
    ) else {
        println!("failed to lanuch shell");
    };

    #[cfg(feature = "test-sched")]
    {
        loop {
            print!("A");
            sched::sleep(700);
            sched::yield_now();
        }
    }
    // Start the shell
    #[allow(unreachable_code)]
    // Main drops into idle_thread
    sched::idle_thread();
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

#[cfg(all(test, feature = "test-data"))]
mod data_tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    // AtomicU32 with a non-zero initial value forces this static into .data
    // (interior mutability rules out .rodata). #[used] keeps it from being
    // dead-code-eliminated. If the boot LMA->VMA copy doesn't run, the read
    // returns whatever happens to be in SRAM at boot (zero on QEMU).
    #[used]
    static DATA_TEST: AtomicU32 = AtomicU32::new(0xCAFEBABE);

    #[test_case]
    fn test_data_initialized() {
        assert_eq!(DATA_TEST.load(Ordering::Relaxed), 0xCAFEBABE);
    }
}
