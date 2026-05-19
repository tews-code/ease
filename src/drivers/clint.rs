//! SiFive CLINT Driver

use crate::arch::{cpu_id, mmio};
use crate::board::clint;
use crate::kernel::sync::IrqSpinLock;

pub struct SiFiveClint(()); // Private field to prevent other modules creating this struct

pub type Clint = SiFiveClint;

impl SiFiveClint {
    const MSIP: usize = 0;
    const MTIME: usize = 0xBFF8;
    const MTIMECMP: usize = 0x4000;

    /// Get the timer comparison 'mtimecmp'
    #[expect(dead_code)]
    pub fn get_mtimecmp(&self) -> u64 {
        let cpu_id = cpu_id();
        loop {
            let hi = mmio::read32(clint::BASE, Self::MTIMECMP + cpu_id * 8 + 4);
            let lo = mmio::read32(clint::BASE, Self::MTIMECMP + cpu_id * 8);
            let hi_again = mmio::read32(clint::BASE, Self::MTIMECMP + cpu_id * 8 + 4);
            if hi == hi_again {
                return ((hi as u64) << 32) | (lo as u64);
            }
        }
    }

    /// Set the timer comparison `mtimecmp` to trigger interrupt at given ticks count
    pub fn set_mtimecmp(&mut self, ticks_trigger_value: u64) {
        let cpu_id = cpu_id();
        // Safety: CLINT MTIMECMP is a valid MMIO register at BASE + MTIMECMP.
        let hi = (ticks_trigger_value >> 32) as u32;
        let lo = ticks_trigger_value as u32;
        // Step 1: Set high word to MAX so mtimecmp is impossibly large (no spurious interrupt)
        mmio::write32(clint::BASE, Self::MTIMECMP + cpu_id * 8 + 4, u32::MAX);
        // Step 2: Write the actual low word (mtimecmp still huge due to MAX high word)
        mmio::write32(clint::BASE, Self::MTIMECMP + cpu_id * 8, lo);
        // Step 3: Write the actual high word (mtimecmp is now the correct value)
        mmio::write32(clint::BASE, Self::MTIMECMP + cpu_id * 8 + 4, hi);
    }

    /// Read the current mtime counter tick value
    ///
    /// Static method: mtime is a read-only hardware counter,
    /// no locking required.
    pub fn mtime() -> u64 {
        loop {
            // Safety: MMIO register at valid CLINT address, volatile access required
            let hi = mmio::read32(clint::BASE, Self::MTIME + 4);
            let lo = mmio::read32(clint::BASE, Self::MTIME);
            let hi_again = mmio::read32(clint::BASE, Self::MTIME + 4);
            if hi == hi_again {
                return ((hi as u64) << 32) | (lo as u64);
            }
        }
    }

    /// Set the Machine Software Interrupt Pending to trigger a software interrupt for the given HART id
    pub fn set_msip(&mut self, hart_id: usize) {
        // Safety: CLINT MSIP is valid for writes at BASE + MSIP
        mmio::write32(clint::BASE, Self::MSIP + hart_id * 4, 1);
    }

    /// Clear the Machine Software Interrupt Pending for the current HART
    pub fn clear_msip(&mut self) {
        // Safety: CLINT MSIP is valid for writes at BASE + MSIP
        mmio::write32(clint::BASE, Self::MSIP + cpu_id() * 4, 0);
    }
}

static CLINT: IrqSpinLock<SiFiveClint> = IrqSpinLock::new(SiFiveClint(())); // Private - only access with `with_clint`

/// Runs a closure with exclusive access to the CLINT driver.
pub fn with_clint<F, R>(f: F) -> R
where
    F: FnOnce(&mut SiFiveClint) -> R,
{
    let mut clint = CLINT.lock();
    f(&mut clint)
}

#[cfg(all(test, feature = "test-clint"))]
mod tests {
    use crate::drivers::clint::Clint;

    #[test_case]
    fn test_counter_incrementing() {
        let c1 = Clint::mtime();
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        let c2 = Clint::mtime();
        assert!(c2 > c1, "counter should increment");
    }
}
