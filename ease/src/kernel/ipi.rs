//! Inter processor interrupts

use crate::arch::csr::mie;
use crate::drivers::clint::with_clint;

pub fn init() {
    mie::enable_bits(mie::MSIE);
}

pub fn send(hart_id: usize) {
    with_clint(|c| {
        c.set_msip(hart_id);
    })
}

pub fn clear_self() {
    with_clint(|c| c.clear_msip())
}
