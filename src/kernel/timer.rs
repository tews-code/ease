//! Timer support

use crate::arch::csr;
use crate::board::clint::TIMER_FREQ_HZ;
use crate::drivers::clint::{Clint, with_clint};

/// Conversion factor (QEMU runs at 10MHz, so 10_000 = 1ms)
const TICKS_PER_MS: u64 = TIMER_FREQ_HZ / 1000;

/// Initialise the timer for a HART
pub fn init() {
    with_clint(|c| c.set_mtimecmp(u64::MAX));
    csr::mie::enable_bits(csr::mie::MTIE);
}

/// Set the next timer interrupt deadline
pub fn set_next_deadline_ms(deadline_ms: u64) {
    with_clint(|c| {
        c.set_mtimecmp(deadline_ms.saturating_mul(TICKS_PER_MS));
    })
}

/// Get elapsed time since boot
pub fn elapsed_ms() -> u64 {
    Clint::mtime() / TICKS_PER_MS
}

#[cfg(all(test, feature = "test-timer"))]
mod tests {
    use super::*;

    #[test_case]
    fn timer_smoke_test() {
        let then = elapsed_ms();
        let now = elapsed_ms();
        assert!(now >= then);
    }
}
