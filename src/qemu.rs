//! QEMU-specific utilities
//!
//! Provides mechanisms for interacting with QEMU's virt machine,
//! including exit codes for test automation.
//!
//! The SiFive test device accepts a 32-bit value at address 0x100000:
//! - Lower 16 bits: status (PASS=0x5555, FAIL=0x3333, RESET=0x7777)
//! - Upper 16 bits: exit code passed to QEMU process

#[cfg(test)]
mod inner {
    /// Address of the SiFive test device on QEMU virt machine
    const SIFIVE_TEST_ADDR: usize = 0x100000;

    /// Status code for successful shutdown
    const FINISHER_PASS: u32 = 0x5555;

    /// Status code for failure/panic shutdown
    const FINISHER_FAIL: u32 = 0x3333;

    /// Exit QEMU with success status (exit code 0)
    ///
    /// Writes to the SiFive test device to terminate QEMU cleanly.
    /// Used by test framework when all tests pass.
    pub fn exit_success() -> ! {
        // FINISHER_PASS with exit code 0 in upper 16 bits
        unsafe {
            core::ptr::write_volatile(SIFIVE_TEST_ADDR as *mut u32, FINISHER_PASS);
        }
        loop {}
    }

    /// Exit QEMU with failure status (exit code 1)
    ///
    /// Writes to the SiFive test device to terminate QEMU with error.
    /// Used by test framework when a test fails.
    pub fn exit_failure() -> ! {
        // FINISHER_FAIL with exit code 1 in upper 16 bits
        // Format: (exit_code << 16) | status
        unsafe {
            core::ptr::write_volatile(SIFIVE_TEST_ADDR as *mut u32, (1 << 16) | FINISHER_FAIL);
        }
        loop {}
    }
}

#[cfg(test)]
pub use inner::*;
