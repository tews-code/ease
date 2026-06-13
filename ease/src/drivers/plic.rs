//! PLIC driver
//!
//! All interrupts are serviced by HART0

use crate::arch::csr::mie;
use crate::arch::mmio;
use crate::board::plic;

// Any IRQ with priority > threshold will be delivered
const THRESHOLD_LEVEL: u32 = 0;
const PRIORITY_LEVEL: u32 = 1;

/// Initialise PLIC for HART0 by setting the interrupt threshold
/// as well as Machine External Interrupt Enable (MEIE) on `mie`
pub fn init() {
    mmio::write32(plic::BASE, plic::HART0_THRESHOLD, THRESHOLD_LEVEL);
    mie::enable_bits(mie::MEIE);
}

/// Enable external interrupts by IRQ number
/// All interrupts are set to priority level 1 (threshold is 0)
pub fn enable(irq: u32) {
    // Set the priority
    mmio::write32(
        plic::BASE,
        plic::PRIORITY + irq as usize * 4,
        PRIORITY_LEVEL,
    );
    // Enable the irq source
    let offset = plic::HART0_ENABLE + (irq as usize / 32) * 4;
    let current = mmio::read32(plic::BASE, offset);
    mmio::write32(plic::BASE, offset, current | (1 << (irq % 32)));
}

/// Claim the interrupt for processing
pub fn claim() -> u32 {
    mmio::read32(plic::BASE, plic::HART0_CLAIM_COMPLETE)
}

/// Mark an interrupt process as complete
pub fn complete(irq: u32) {
    mmio::write32(plic::BASE, plic::HART0_CLAIM_COMPLETE, irq);
}
