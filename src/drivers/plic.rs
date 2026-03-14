//! PLIC driver

#![allow(dead_code)]

use crate::arch::mmio;
use crate::board::plic;

// Sets the priority threshold for interrupts
pub fn set_priority(source: u32, priority: u32) {
    mmio::write32(plic::BASE, source as usize * 4, priority);
}

// Set the interrupt threshold for all external interrupts
pub fn set_threshold(threshold: u32) {
    mmio::write32(plic::BASE, plic::THRESHOLD, threshold);
}

// Enable external interrupts by IRQ number
pub fn enable(source: u32) {
    let offset = plic::ENABLE + (source as usize / 32) * 4;
    let current = mmio::read32(plic::BASE, offset);
    mmio::write32(plic::BASE, offset, current | (1 << (source % 32)));
}

// Claim the interrupt for processing
pub fn claim() -> u32 {
    mmio::read32(plic::BASE, plic::CLAIM_COMPLETE)
}

// Mark an interrupt process as complete
pub fn complete(source: u32) {
    mmio::write32(plic::BASE, plic::CLAIM_COMPLETE, source);
}
