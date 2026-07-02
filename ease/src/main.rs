//! EASE - OS for RP2350
//!
//! A hobby OS for Adafruit Metro RP2350 with 8MB PSRAM in RISC-V mode.
//! Developed on the QEMU `virt` board.

#![no_std]
#![no_main]
#![warn(missing_docs)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::testrunner::test_runner))]
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

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

// Bring the print/println macros in first so other modules can benefit
#[macro_use]
mod io;

mod arch;
mod bench;
mod board;
mod drivers;
mod fs;
mod kernel;
mod qemu;
mod shell;
mod syscall;
#[cfg(test)]
mod testrunner;
mod user;

use drivers::ramfb::FrameBuffer;
use drivers::uart::UartInitToken;
use kernel::alloc::Order;
use kernel::percpu;

use crate::board::uart;
use crate::board::virtio;
use crate::kernel::sched;

pub(crate) static INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

// =============================================================================
// Entry Points
// =============================================================================

// Not spawned in test builds (see kernel_init), so it would be dead there.
#[cfg(not(test))]
fn shell_main(fb: FrameBuffer) {
    let console = shell::console::Console::new(fb);
    let mut shell = shell::Shell::new(console);
    shell.run();
}

fn interrupts_init_hart0(_uart_token: UartInitToken) {
    arch::interrupts::enable();
}

fn interrupts_init_hart1() {
    arch::interrupts::enable();
}

fn kernel_init() {
    // Initialise just the basics to keep stack use light
    // The initialisation functions panic or succeed
    #[cfg(feature = "profile")]
    kernel::profile::init();
    percpu::set_online();
    kernel::alloc::init_global_allocator();
    kernel::timer::init();
    drivers::plic::init();
    drivers::plic::enable(uart::IRQ);
    let uart_init_token = drivers::uart::init();
    sched::bootstrap(0);
    kernel::ipi::init();
    interrupts_init_hart0(uart_init_token);

    // Spawn the trace sampler on HART0 BEFORE the init thread, so it is
    // already sampling while the init thread runs (and exits) on this hart.
    // #[cfg(feature = "trace")]
    // sched::Builder::new()
    //     .with_stack_class(Order::KB2)
    //     .spawn(|| {
    //         use crate::kernel::sched::trace;
    //         loop {
    //             crate::sched::sleep(1);
    //             trace::take_snapshot("timed sample");
    //         }
    //     })
    //     .expect("spawn trace thread");

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
    sched::Builder::new()
        .with_stack_class(Order::KB16)
        .spawn(|| {
            drivers::virtio::blk::virtio_blk_init();
            drivers::plic::enable(virtio::blk::IRQ);
            drivers::virtio::input::virtio_keyboard_init();
            // drivers::plic::enable(virtio::keyboard::IRQ);
            fs::volume::fat16_init();
            let fb = FrameBuffer::init();
            // The interactive shell is a permanent runnable thread (its idle
            // loop WFIs while still the Running thread on its hart). In test
            // builds it's pure background contention — the runner never feeds
            // it input — so it skews the latency-sensitive scheduler tests.
            // Spawn it only outside test builds.
            #[cfg(not(test))]
            sched::Builder::new()
                .with_stack_class(Order::KB16)
                .spawn(move || shell_main(fb))
                .expect("spawn shell");
            #[cfg(test)]
            let _ = fb; // framebuffer still initialised; shell just not spawned
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
    percpu::set_online();
    // HART1 does not service external (PLIC) or driver interrupts; only timer and IPI
    interrupts_init_hart1();
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
        .spawn(crate::testrunner::test_runner_thread)
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
// Tests
// =============================================================================

#[cfg(all(test, feature = "test-boot-data"))]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    #[used]
    static BSS_TEST: AtomicU32 = AtomicU32::new(0);

    #[test_case]
    fn test_bss_zeroed() {
        core::hint::black_box(&BSS_TEST);
        assert_eq!(BSS_TEST.load(Ordering::Relaxed), 0);
    }

    // AtomicU32 with a non-zero initial value forces this static into .data
    // (interior mutability rules out .rodata). #[used] keeps it from being
    // dead-code-eliminated. If the boot LMA->VMA copy doesn't run, the read
    // returns whatever happens to be in SRAM at boot (zero on QEMU).
    #[used]
    static DATA_TEST: AtomicU32 = AtomicU32::new(0xCAFEBABE);

    #[test_case]
    fn test_data_initialized() {
        core::hint::black_box(&DATA_TEST);
        assert_eq!(DATA_TEST.load(Ordering::Relaxed), 0xCAFEBABE);
    }
}
