//! Architecture-specific code for RISC-V

#![allow(dead_code)]

pub mod boot;
pub mod timer;
pub mod trap;

pub const MSTATUS_MIE: usize = 1 << 3; // Machine mode enable all interrupts

/// Disables interrupts
///
/// - Returns prior machine status
pub fn disable_interrupts() -> usize {
    let mstatus: usize;
    unsafe {
        // Use csrrc to atomically read mstatus and clear MIE (bit 3)
        core::arch::asm!("csrrc {}, mstatus, {}", out(reg) mstatus, const MSTATUS_MIE);
    }
    mstatus
}

/// Enables interrupts if previously enabled
///
/// - `prev` is the previous machine status register
///
/// This is a no-op if interrupts were already disabled (handles nested locks correctly).
pub fn restore_interrupts(prev: usize) {
    if prev & MSTATUS_MIE != 0 {
        unsafe {
            core::arch::asm!("csrsi mstatus, {}", const MSTATUS_MIE);
        }
    }
}
