//! Test Runner framework for running no_std tests

// =============================================================================
// Test Framework
// =============================================================================

#[cfg(test)]
use crate::drivers::uart;

/// Wrap `test_main` so it can be spawned as a thread entry.
#[cfg(test)]
pub(super) fn test_runner_thread() {
    // Wait for the init thread (spawned by `kernel_init`) to finish bringing up
    // drivers/filesystem before running tests — otherwise virtio/fs tests can
    // race ahead of `virtio_blk_init`/`fat16_init` and see uninitialised state.
    // Sleep (don't busy-spin): until INIT_COMPLETE is set, HART1 is still parked
    // in its boot spin, so every thread runs on HART0 — busy-spinning here would
    // compete with the init thread on the same hart and only yield at slice
    // boundaries; sleeping lets the init thread run and set the flag promptly.
    while !crate::INIT_COMPLETE.load(core::sync::atomic::Ordering::Acquire) {
        crate::kernel::sched::sleep(1);
    }
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
        let name = core::any::type_name::<T>();
        // Report live threads entering this test: a leftover from a prior
        // test shows as an elevated runnable count here.
        #[cfg(feature = "trace")]
        crate::kernel::sched::trace::report_live(name);
        // Name the test that grew a hart's longest interrupts-off section.
        #[cfg(feature = "irqsoff")]
        crate::kernel::irqsoff::test_boundary(name);
        print!("{name}...\t");
        // Clear the scheduler trace so a panic dump reflects only this test.
        #[cfg(feature = "trace")]
        crate::kernel::sched::trace::reset();
        self();
        uart::flush();
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
    #[cfg(feature = "paint-stack")]
    crate::kernel::stack::print_irq_idle_stacks();
    #[cfg(feature = "irqsoff")]
    {
        crate::kernel::irqsoff::test_boundary("(end of run)");
        crate::kernel::irqsoff::print_report();
    }
    #[cfg(feature = "trace")]
    dump_trace_at_end_of_run();
    crate::qemu::exit_success();
}

/// Dump the scheduler trace at the end of every passing `--trace` run, so the
/// dump code runs on every such run instead of only on a failure (it had
/// rotted unseen: a light snapshot's unknown other-hart roster panicked it).
///
/// `dump_trace` must run single-threaded with no concurrent snapshot writers,
/// so this freezes the system the way the panic handler does: park the other
/// hart via STOP + IPI and keep this hart's interrupts off for the dump.
#[cfg(all(test, feature = "trace"))]
fn dump_trace_at_end_of_run() {
    use crate::kernel::panic::{PARKED, STOP};
    use core::sync::atomic::Ordering;
    // The dump writes straight to the UART; drain the buffered test output
    // first so the two don't interleave
    uart::flush();
    crate::kernel::interrupts::with_interrupts_disabled(|_cs| {
        STOP.store(true, Ordering::Relaxed);
        crate::kernel::ipi::send(crate::kernel::ipi::RESCHEDULE);
        let start = crate::kernel::timer::elapsed_ms();
        while !PARKED.load(Ordering::Acquire) {
            assert!(
                crate::kernel::timer::elapsed_ms() - start < 1000,
                "other hart never parked for the end-of-run trace dump"
            );
            core::hint::spin_loop();
        }
        // Light points are rare (only when a snapshot finds the sched lock
        // hot) and the runner resets the trace per test, so force one: the
        // dump's light-row rendering then runs on every trace run.
        crate::kernel::sched::trace::take_light_snapshot_for_test("end-of-run");
        crate::kernel::sched::trace::dump_trace();
    });
}
