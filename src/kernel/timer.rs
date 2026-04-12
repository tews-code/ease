//! Timer support for QEMU virt platform

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::csr;
use crate::drivers::clint::{Clint, with_clint};
use crate::kernel::stack_guard;

/// Timer interval (QEMU runs at 10MHz, so 10_000 = 1ms)
const TIMER_INTERVAL: u64 = crate::board::clint::TIMER_FREQ_HZ / 1_000; // 1ms

/// Global tick counter (incremented by timer interrupt)
/// Note will wrap at 49 days (as 32-bit)
static TICKS: AtomicUsize = AtomicUsize::new(0);

/// Initialise the timer
pub fn init() {
    with_clint(|c| c.set_mtimecmp(Clint::mtime() + TIMER_INTERVAL));
    csr::mie::enable_bits(csr::mie::MTIE);
}

/// Handle interrupt called by trap vector
pub fn handle_interrupt() {
    if !stack_guard::check() {
        panic!("Stack has grown into heap");
    }
    TICKS.fetch_add(1, Ordering::Relaxed);
    with_clint(|c| {
        let next = c.get_mtimecmp() + TIMER_INTERVAL;
        c.set_mtimecmp(next);
    })
}

/// Get current tick count (TIMER_INTERVAL is 1ms)
pub fn ticks_ms() -> usize {
    TICKS.load(Ordering::Relaxed)
}

/// Sleep for given ms
#[allow(dead_code)]
pub fn sleep_ms(ms: usize) {
    let start = ticks_ms();
    debug_assert!(
        crate::arch::interrupts_enabled(),
        "sleep_ms called with interrupts disabled"
    );
    while ticks_ms().wrapping_sub(start) < ms {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

/// Busy wait for given ms
#[allow(dead_code)]
pub fn busy_wait_ms(ms: usize) {
    let count_start = Clint::mtime();
    while Clint::mtime() - count_start < ms as u64 * TIMER_INTERVAL {
        core::hint::spin_loop();
    }
}

#[cfg(all(test, feature = "test-timer"))]
mod tests {
    use super::*;

    #[test_case]
    fn test_ticks_incrementing() {
        let t1 = ticks_ms();
        for _ in 0..10000 {
            core::hint::spin_loop();
        }
        let t2 = ticks_ms();
        assert!(t2 >= t1, "ticks should not go backwards");
    }

    #[test_case]
    fn test_sleep_ms() {
        let start = ticks_ms();
        sleep_ms(100);
        let elapsed = ticks_ms() - start;
        assert!(elapsed >= 90, "sleep too short: {}ms", elapsed);
        assert!(elapsed <= 150, "sleep too long: {}ms", elapsed);
    }

    #[test_case]
    fn test_sleep_zero() {
        let start = ticks_ms();
        sleep_ms(0);
        let elapsed = ticks_ms() - start;
        assert!(elapsed <= 5, "sleep(0) took too long: {}ms", elapsed);
    }

    #[test_case]
    fn test_busy_wait_ms() {
        let start = Clint::mtime();
        busy_wait_ms(100);
        let elapsed = (Clint::mtime() - start) / TIMER_INTERVAL;
        assert!(elapsed >= 90, "busy_wait too short: {}ms", elapsed);
        assert!(elapsed <= 150, "busy_wait too long: {}ms", elapsed);
    }
}
