//! SiFive CLINT Driver
//!
//! All set_mtimecmp callers have interrupts disabled
//! This means there is no need for any locking, as
//! the registers are per-Hart

use crate::arch::{hart_id, mmio};
use crate::board::clint;

const MSIP: usize = 0; // Machine Software Interrupt Pending
const MTIME: usize = 0xBFF8;
const MTIMECMP: usize = 0x4000;
const ENABLE: u32 = 1;
const CLEAR: u32 = 0;

/// Set the timer comparison `mtimecmp` to trigger interrupt at given ticks count
pub fn set_mtimecmp(ticks_trigger_value: u64) {
    debug_assert!(
        !crate::arch::interrupts::enabled(),
        "set mtimecmp must be run with interrupts disabled"
    );
    let hart = hart_id();
    let hi = (ticks_trigger_value >> 32) as u32;
    let lo = ticks_trigger_value as u32;
    // Step 1: Set high word to MAX so mtimecmp is impossibly large (no spurious interrupt)
    mmio::write32(clint::BASE, MTIMECMP + hart * 8 + 4, u32::MAX);
    // Step 2: Write the actual low word (mtimecmp still huge due to MAX high word)
    mmio::write32(clint::BASE, MTIMECMP + hart * 8, lo);
    // Step 3: Write the actual high word (mtimecmp is now the correct value)
    mmio::write32(clint::BASE, MTIMECMP + hart * 8 + 4, hi);
}

/// Read the current mtime counter tick value
#[inline(always)]
pub fn mtime() -> u64 {
    loop {
        let hi = mmio::read32(clint::BASE, MTIME + 4);
        let lo = mmio::read32(clint::BASE, MTIME);
        let hi_again = mmio::read32(clint::BASE, MTIME + 4);
        if hi == hi_again {
            return ((hi as u64) << 32) | (lo as u64);
        }
    }
}

/// Set the Machine Software Interrupt Pending to trigger a software interrupt
/// for the given HART id
pub fn set_msip(hart: usize) {
    mmio::write32(clint::BASE, MSIP + hart * 4, ENABLE);
}

/// Clear the Machine Software Interrupt Pending for the current HART
pub fn clear_msip() {
    mmio::write32(clint::BASE, MSIP + hart_id() * 4, CLEAR);
}

// Access CLINT software interrupt MMI0 address directly during boot
pub const fn clint_msip_addr(hart_id: usize) -> usize {
    clint::BASE + MSIP + 4 * hart_id
}

#[cfg(all(test, feature = "test-clint"))]
mod tests {
    use crate::drivers::clint;

    #[test_case]
    fn test_counter_incrementing() {
        let c1 = clint::mtime();
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        let c2 = clint::mtime();
        assert!(c2 > c1, "counter should increment");
    }
}
