//! Hardware Abstraction Layer
//!
//! Defines HAL traits. Hardware-specific implementations are in submodules

#![allow(dead_code)]

pub const BLOCK_SIZE: usize = 512;
pub const PAGE_SIZE: usize = 4096;

/// Puts the CPU into low-power wait state until the next interrupt fires.
pub fn wait_for_interrupt() {
    unsafe {
        // Safety: "wfi" is safe to call
        core::arch::asm!("wfi");
    }
}
