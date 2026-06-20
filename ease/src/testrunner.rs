//! Test Runner framework for running no_std tests

// =============================================================================
// Test Framework
// =============================================================================

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
        let name = core::any::type_name::<T>();
        // Report live threads entering this test: a leftover from a prior
        // test shows as an elevated runnable count here.
        #[cfg(feature = "trace")]
        crate::kernel::sched::trace::report_live(name);
        print!("{name}...\t");
        // Clear the scheduler trace so a panic dump reflects only this test.
        #[cfg(feature = "trace")]
        crate::kernel::sched::trace::reset();
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
    #[cfg(feature = "paint-stack")]
    crate::kernel::stack::print_irq_idle_stacks();
    crate::qemu::exit_success();
}
