//! Timer support for QEMU virt platform

use core::sync::atomic::{AtomicUsize, Ordering};

const CLINT_BASE: usize = 0x2000000;
const MTIME: usize = CLINT_BASE + 0xBFF8;
const MTIMECMP: usize = CLINT_BASE + 0x4000;

/// Timer interval (QEMU runs at 10MHz, so 10_000 = 1ms)
const TIMER_INTERVAL: u64 = 10_000;

/// Global tick counter (incremented by timer interrupt)
/// Note will wrap at 49 days (as 32-bit)
static TICKS: AtomicUsize = AtomicUsize::new(0);

/// Read the current mtime counter value
pub fn get_mtime() -> u64 {
    loop {
        let hi1 = unsafe { core::ptr::read_volatile((MTIME + 4) as *const u32) };
        let lo = unsafe { core::ptr::read_volatile(MTIME as *const u32) };
        let hi2 = unsafe { core::ptr::read_volatile((MTIME + 4) as *const u32) };
        if hi1 == hi2 {
            return ((hi1 as u64) << 32) | (lo as u64);
        }
    }
}

/// Set the timer comparison `mtimecmp` to trigger interrupt at given ticks count
pub fn set_mtimecmp(trigger_tick_count: u64) {
    unsafe {
        // Write max to low word first to avoid spurious interrupts
        core::ptr::write_volatile(MTIMECMP as *mut u32, u32::MAX);
        core::ptr::write_volatile(
            (MTIMECMP + 4) as *mut u32,
            (trigger_tick_count >> 32) as u32,
        );
        core::ptr::write_volatile(MTIMECMP as *mut u32, trigger_tick_count as u32);
    }
}

/// Initialise the timer
pub fn init() {
    const MIE_MTIE: u32 = 1 << 7;
    set_mtimecmp(get_mtime() + TIMER_INTERVAL);
    unsafe {
        core::arch::asm!("csrs mie, {}", in(reg) MIE_MTIE);
    }
}

/// Handle interrupt called by trap vector
pub fn handle_interrupt() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    set_mtimecmp(get_mtime() + TIMER_INTERVAL);
}

/// Get current ticks (ms)
pub fn ticks_ms() -> usize {
    TICKS.load(Ordering::Relaxed)
}

/// Sleep for given ms
pub fn sleep_ms(ms: usize) {
    let start = ticks_ms();
    while ticks_ms().wrapping_sub(start) < ms {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[cfg(test)]
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
}
