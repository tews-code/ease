//! Inter processor interrupts
//!
//! All IPIs are bare wakeups
//! Memory ordering is not enforced by the IPI and
//! assumes that shared structures (e.g. TCBs) are
//! protected by an IRQ lock

use crate::arch::csr::mie;
use crate::drivers::clint;

pub fn init() {
    mie::enable_bits(mie::MSIE);
}

pub fn send(hart: usize) {
    clint::set_msip(hart);
}

pub fn clear_self() {
    clint::clear_msip()
}
