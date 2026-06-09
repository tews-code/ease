//! EASE - OS for RP2350
//!
//! A hobby OS for Adafruit Metro RP2350 with 8MB PSRAM in RISC-V mode.
//! Developed on the QEMU `virt` board.

#![no_std]
#![no_main]
#![warn(missing_docs)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::test::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]
// The bench-only build (`cargo test --features bench`) compiles just the
// benchmark #[test_case]s — not the functional tests or production main()
// that exercise the process/spawn machinery — so that code reads as dead
// in this one configuration only. Relax dead-code checking here; the
// functional builds (test-all) keep full vigilance.
#![cfg_attr(
    all(test, feature = "bench", not(feature = "test-sched")),
    allow(dead_code)
)]

use core::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::ramfb::FrameBuffer;
use crate::kernel::alloc::Order;
use crate::kernel::sched;

extern crate alloc;

// Bring the print/println macros in first so other modules can benefit
#[macro_use]
mod io;

mod arch;
mod bench;
mod board;
mod drivers;
mod fs;
mod hal;
mod kernel;
mod qemu;
mod shell;
mod syscall;
mod user;

static INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

// =============================================================================
// Entry Points
// =============================================================================

fn shell_main(fb: FrameBuffer) {
    let console = shell::console::Console::new(fb);
    let mut shell = shell::Shell::new(console);
    shell.run();
}

fn kernel_init() {
    // Initialise just the basics to keep stack use light
    // The initialisation functions panic or succeed
    #[cfg(feature = "profile")]
    kernel::profile::init();
    kernel::timer::init();
    kernel::alloc::init_global_allocator();
    drivers::plic::init();
    drivers::uart::init();
    kernel::ipi::init();
    sched::bootstrap(0);
    arch::enable_interrupts();

    // Spawn a profiler thread early if we want to profile the initialisation
    #[cfg(feature = "profile")]
    sched::Builder::new()
        .with_stack_class(Order::KB16)
        .spawn(|| {
            loop {
                sched::sleep(1_000);
                crate::kernel::profile::dump();
            }
        })
        .expect("unable to spawn profiler thread");

    // Spawn a thread with a deeper stack to complete initialisation
    sched::spawn(|| {
        sched::usermemmap::init();
        drivers::virtio::virtio_blk_init();
        fs::volume::fat16_init();
        let fb = FrameBuffer::init();
        sched::Builder::new()
            .with_stack_class(Order::KB16)
            .spawn(move || shell_main(fb))
            .expect("spawn shell");
        // Set the flag to allow HART1 to progress
        INIT_COMPLETE.store(true, Ordering::Release);
    })
    .expect("could not spawn initialisation thread");
}

extern "C" fn secondary_main() -> ! {
    // Spin on initialisation completion
    while !INIT_COMPLETE.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    // Perform Hart-specific initialisation
    kernel::timer::init();
    sched::bootstrap(1);
    kernel::ipi::init();
    // HART1 does not service external (PLIC) or driver interrupts; only timer and IPI
    arch::enable_interrupts();
    // Drop into idle
    sched::idle_thread();
}

#[cfg(test)]
extern "C" fn main() -> ! {
    kernel_init();

    // Move the test workload off the 4 KiB bootstrap stack onto a dedicated
    // 16 KiB heap stack. The test framework (format machinery, ~211 result
    // prints across test-all, FAT-format buffers, virtio sector reads on
    // stack) accumulates a surprisingly deep peak
    sched::Builder::new()
        .with_stack_class(Order::KB16)
        .spawn(crate::test::test_runner_thread)
        .expect("could not spawn test runner thread");
    // Bootstrap drops into the idle thread for Hart0
    sched::idle_thread();
}

#[cfg(not(test))]
extern "C" fn main() -> ! {
    kernel_init();
    // Main drops into idle_thread
    sched::idle_thread();
}

// =============================================================================
// Test Framework
// =============================================================================

#[cfg(test)]
mod test {
    /// Wrap `test_main` so it can be spawned as a thread entry.
    #[cfg(test)]
    pub(super) fn test_runner_thread() {
        crate::test_main();
        // `test_main` calls `qemu::exit_success` once all tests pass, so under
        // normal circumstances this never returns. If it ever does, the
        // implicit `exit()` in the trampoline cleans up this thread.
    }

    /// Trait for test cases that can be run by the test framework
    pub(super) trait Testable {
        /// Run the test and print status
        fn run(&self);
    }

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
    pub(super) fn test_runner(tests: &[&dyn Testable]) {
        println!("Running {} tests", tests.len());
        for test in tests {
            test.run();
        }
        println!();
        println!("All tests passed!");

        // Display stack depth used
        #[cfg(feature = "paint-stack")]
        {
            unsafe extern "C" {
                static __hart0_irq_stack_base: u8;
                static __hart0_irq_stack_top: u8;
                static __hart1_irq_stack_base: u8;
                static __hart1_irq_stack_top: u8;
                static __hart0_idle_stack_base: u8;
                static __hart0_idle_stack_top: u8;
                static __hart1_idle_stack_base: u8;
                static __hart1_idle_stack_top: u8;
            }

            use crate::kernel::stack::print_stack_watermark;

            print_stack_watermark(
                "IRQ Hart",
                0,
                &raw const __hart0_irq_stack_base as usize,
                &raw const __hart0_irq_stack_top as usize,
            );
            print_stack_watermark(
                "IRQ Hart",
                1,
                &raw const __hart1_irq_stack_base as usize,
                &raw const __hart1_irq_stack_top as usize,
            );
            print_stack_watermark(
                "Idle Hart",
                0,
                &raw const __hart0_idle_stack_base as usize,
                &raw const __hart0_idle_stack_top as usize,
            );
            print_stack_watermark(
                "Idle Hart",
                1,
                &raw const __hart1_idle_stack_base as usize,
                &raw const __hart1_idle_stack_top as usize,
            );
        }
        crate::qemu::exit_success();
    }
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
