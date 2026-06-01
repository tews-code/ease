//! EASE - OS for RP2350
//!
//! A hobby OS for Adafruit Metro RP2350 with 8MB PSRAM in RISC-V mode.
//!

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
use crate::kernel::alloc::Order;
use crate::kernel::sched::{self, spawn};
use crate::kernel::sync::{Completion, IrqSpinLock};

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
mod syscall;
mod user;

static FB_HANDOFF: IrqSpinLock<Option<FrameBuffer>> = IrqSpinLock::new(None);
static FB_READY: Completion = Completion::new();
static INIT_COMPLETE: AtomicBool = AtomicBool::new(false);

// =============================================================================
// Entry Points
// =============================================================================

#[allow(dead_code)]
fn shell_thread() {
    FB_READY.wait();
    let fb = FB_HANDOFF
        .lock()
        .take()
        .expect("Framebuffer should be present after FB_READY signal");
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
    kernel::ipi::init();
    // HART1 does not service external (PLIC) or driver interrupts; only timer and IPI
    arch::enable_interrupts();
}

fn minimal_init() {
    // Initialise just the basics to keep stack use light
    #[cfg(feature = "profile")]
    kernel::profile::init();
    kernel::timer::init();
    kernel::alloc::init_global_allocator();
    drivers::plic::init();
    drivers::uart::init();
    kernel::ipi::init();

    sched::bootstrap(0);
    arch::enable_interrupts();
    // Set the flag to allow HART1 to progress
    INIT_COMPLETE.store(true, Ordering::Release);
}

fn kernel_init() {
    minimal_init();
    #[cfg(feature = "profile")]
    sched::Builder::new()
        .with_stack_class(sched::StackClass::KB16)
        .with_priority(sched::PRIORITY_DEFAULT)
        .spawn(|| {
            loop {
                sched::sleep(1_000);
                crate::kernel::profile::dump();
            }
        });

    // Spawn a thread with a deeper stack to complete initialisation
    spawn(|| {
        drivers::virtio::virtio_blk_init();
        fs::volume::fat16_init();

        let fb = drivers::ramfb::FrameBuffer::init();
        *FB_HANDOFF.lock() = Some(fb);
        FB_READY.signal();
    });
}

#[unsafe(no_mangle)]
extern "C" fn secondary_main() {
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
    use crate::kernel::alloc::Order;

    let _fb = kernel_init();

    // Move the test workload off the 4 KiB bootstrap stack onto a dedicated
    // 16 KiB heap stack. The test framework (format machinery, ~211 result
    // prints across test-all, FAT-format buffers, virtio sector reads on
    // stack) accumulates a surprisingly deep peak
    let id = sched::Builder::new()
        .with_stack_class(Order::KB16)
        .spawn(test_runner_thread);
    assert!(id.is_some(), "could not spawn test runner thread");

    // Bootstrap converts into the idle thread
    sched::idle_thread();
}

/// Wrap `test_main` so it can be spawned as a thread entry.
#[cfg(test)]
fn test_runner_thread() {
    test_main();
    // `test_main` calls `qemu::exit_success` once all tests pass, so under
    // normal circumstances this never returns. If it ever does, the
    // implicit `exit()` in the trampoline cleans up this thread.
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    kernel_init();

    #[allow(clippy::diverging_sub_expression)]
    let Some(_id) = sched::Builder::new()
        .with_stack_class(Order::KB16)
        .spawn(shell_thread)
    else {
        panic!("failed to launch shell");
    };

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

    {
        use crate::io::DirectWriter;
        use core::fmt::Write;
        {
            unsafe extern "C" {
                static __hart0_irq_stack_base: u8;
                static __hart0_irq_stack_top: u8;
            }

            use crate::kernel::sched::stack::stack_high_watermark;
            let start_addr = &raw const __hart0_irq_stack_base as usize;
            let end_addr = &raw const __hart0_irq_stack_top as usize;
            println!("==== IRQ Stack High Watermark Check ====");
            if let Some(addr) = stack_high_watermark(start_addr, end_addr) {
                println!("Start address: {start_addr:x}");
                println!("High watermark address: {addr:x}");
                println!("Top address: {end_addr:x}");
            } else {
                println!(" * STACK CORRUPT * ");
            };
            println!("==== IRQ Stack High Watermark Check ====");
        }
        {
            unsafe extern "C" {
                static __hart0_idle_stack_base: u8;
                static __hart0_idle_stack_top: u8;
            }
            use crate::kernel::sched::stack::stack_high_watermark;
            let start_addr = &raw const __hart0_idle_stack_base as usize;
            let end_addr = &raw const __hart0_idle_stack_top as usize;
            let _ = writeln!(DirectWriter, "==== Boot Stack High Watermark Check ====");
            if let Some(addr) = stack_high_watermark(start_addr, end_addr) {
                let _ = writeln!(DirectWriter, "Start address: {start_addr:x}");
                let _ = writeln!(DirectWriter, "High watermark address: {addr:x}");
                let _ = writeln!(DirectWriter, "Top address: {end_addr:x}");
            } else {
                let _ = writeln!(DirectWriter, " * STACK CORRUPT * ");
            };
            let _ = writeln!(DirectWriter, "==== Boot Stack High Watermark Check ====");
        }
    }
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
