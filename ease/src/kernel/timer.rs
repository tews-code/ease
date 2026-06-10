//! Timer support

use crate::arch::csr;
use crate::board::clint::TIMER_FREQ_HZ;
use crate::drivers::clint::{Clint, with_clint};

/// Cycle count is derived from the clint frequency
pub const CYCLES_PER_MS: u64 = TIMER_FREQ_HZ / 1_000;
pub const CYCLES_PER_US: u64 = TIMER_FREQ_HZ / 1_000_000;

pub struct TimerInitToken(());

/// Initialise the timer for a HART
pub fn init() -> TimerInitToken {
    with_clint(|c| c.set_mtimecmp(u64::MAX));
    csr::mie::enable_bits(csr::mie::MTIE);
    TimerInitToken(())
}

/// Set the next timer interrupt deadline in clint cycles
pub fn set_next_deadline(deadline: u64) {
    with_clint(|c| {
        c.set_mtimecmp(deadline);
    })
}

/// Set the next timer interrupt deadline in milliseconds
#[allow(dead_code)]
pub fn set_next_deadline_ms(deadline_ms: u64) {
    let deadline = deadline_ms.saturating_mul(CYCLES_PER_MS);
    set_next_deadline(deadline);
}

/// Get elapsed time since boot in milliseconds
#[allow(dead_code)]
pub fn elapsed_ms() -> u64 {
    Clint::mtime() / CYCLES_PER_MS
}

/// Get elapsed time since boot in microseconds
#[allow(dead_code)]
pub fn elapsed_us() -> u64 {
    Clint::mtime() / CYCLES_PER_US
}

/// Get elapsed clint cycles since boot
pub fn elapsed() -> u64 {
    Clint::mtime()
}

#[cfg(all(test, feature = "test-timer"))]
mod tests {
    use super::*;

    #[test_case]
    fn timer_smoke_test() {
        let then = elapsed();
        let now = elapsed();
        assert!(now >= then);
    }
}
