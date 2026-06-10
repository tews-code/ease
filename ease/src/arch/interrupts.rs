//! Interrupt control for each Hart

use super::csr;

/// Enables machine-wide interrupts for this hart
pub(crate) fn enable() {
    unsafe {
        // Write mstatus to set MIE
        core::arch::asm!("csrw mstatus, {}", in(reg) csr::mstatus::MIE);
    }
}

/// Disables interrupts
///
/// - Returns prior machine status
pub(crate) fn disable() -> usize {
    let mstatus: usize;
    unsafe {
        // Use csrrc to atomically read mstatus and clear MIE (bit 3)
        core::arch::asm!("csrrc {}, mstatus, {}", out(reg) mstatus, const csr::mstatus::MIE);
    }
    mstatus
}

/// Enables interrupts if previously enabled
///
/// - `prev` is the previous machine status register
///
/// This is a no-op if interrupts were already disabled (handles nested locks correctly).
pub(crate) fn restore(prev: usize) {
    if prev & csr::mstatus::MIE != 0 {
        unsafe {
            core::arch::asm!("csrsi mstatus, {}", const csr::mstatus::MIE);
        }
    }
}

/// Checks if interrupts are enabled
pub(crate) fn enabled() -> bool {
    let mstatus: usize;
    unsafe { core::arch::asm!("csrr {}, mstatus", out(reg) mstatus) };
    mstatus & csr::mstatus::MIE != 0
}

/// Wait for interrupts
pub(crate) fn wait_for_interrupt() {
    unsafe { core::arch::asm!("wfi") };
}
