//! Architecture-specific code for RISC-V

#![allow(dead_code)]

pub mod boot;
pub mod context;
pub mod csr;
pub mod mmio;
pub mod trap;

/// Sentinel placed at the bottom word of each thread's stack.
/// Checked by the scheduler / panic path to detect stack overflow.
pub const STACK_CANARY: usize = 0xDEAD_BEEF;

/// Get HART id that this thread is running on
pub fn cpu_id() -> usize {
    crate::arch::csr::mhartid::read()
}

/// Enables machine-wide interrupts
///
pub fn enable_interrupts() {
    unsafe {
        // Write mstatus to set MIE
        core::arch::asm!("csrw mstatus, {}", in(reg) csr::mstatus::MIE);
    }
}

/// Disables interrupts
///
/// - Returns prior machine status
pub fn disable_interrupts() -> usize {
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
pub fn restore_interrupts(prev: usize) {
    if prev & csr::mstatus::MIE != 0 {
        unsafe {
            core::arch::asm!("csrsi mstatus, {}", const csr::mstatus::MIE);
        }
    }
}

/// Checks if interrupts are enabled
pub fn interrupts_enabled() -> bool {
    let mstatus: usize;
    unsafe { core::arch::asm!("csrr {}, mstatus", out(reg) mstatus) };
    mstatus & csr::mstatus::MIE != 0
}

/// Wait for interrupts
pub fn wait_for_interrupt() {
    unsafe { core::arch::asm!("wfi") };
}
