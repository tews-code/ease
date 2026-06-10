//! PLIC driver

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::arch::csr::mie;
use crate::arch::mmio;
use crate::board::plic;
use crate::kernel::sync::IrqSpinLock;

pub struct SiFivePlic(());
#[allow(dead_code)]
pub type Plic = SiFivePlic;

impl SiFivePlic {
    /// Sets the priority threshold for interrupts
    pub fn set_priority(&mut self, source: u32, priority: u32) {
        mmio::write32(plic::BASE, source as usize * 4, priority);
    }

    /// Set the interrupt threshold for all external interrupts
    pub fn set_threshold(&mut self, threshold: u32) {
        mmio::write32(plic::BASE, plic::THRESHOLD, threshold);
    }

    /// Enable external interrupts by IRQ number
    pub fn enable(&mut self, source: u32) {
        let offset = plic::ENABLE + (source as usize / 32) * 4;
        let current = mmio::read32(plic::BASE, offset);
        mmio::write32(plic::BASE, offset, current | (1 << (source % 32)));
    }

    /// Claim the interrupt for processing
    pub fn claim(&mut self) -> u32 {
        mmio::read32(plic::BASE, plic::CLAIM_COMPLETE)
    }

    /// Mark an interrupt process as complete
    pub fn complete(&mut self, source: u32) {
        mmio::write32(plic::BASE, plic::CLAIM_COMPLETE, source);
    }
}

static PLIC: IrqSpinLock<SiFivePlic> = IrqSpinLock::new(SiFivePlic(())); // Private - only access with `with_plic`

/// Runs a closure with exclusive access to the PLIC driver.
#[cfg_attr(feature = "profile", profile)]
pub fn with_plic<F, R>(f: F) -> R
where
    F: FnOnce(&mut SiFivePlic) -> R,
{
    let mut plic = PLIC.lock();
    f(&mut plic)
}

pub struct PlicInitToken(());

/// Initialise by enabling interrupts on this HART
pub fn init() -> PlicInitToken {
    with_plic(|p| p.set_threshold(0));
    mie::enable_bits(mie::MEIE);
    PlicInitToken(())
}
