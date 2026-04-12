//! Simple stack guard protection (stack into heap)

unsafe extern "C" {
    static __stack_guard: u8;
}

pub fn init() {
    unsafe { core::ptr::write_volatile(&raw const __stack_guard as *mut usize, 0xDEAD_BEEF) };
}

pub fn check() -> bool {
    unsafe { core::ptr::read_volatile(&raw const __stack_guard as *const usize) == 0xDEAD_BEEF }
}

#[cfg(all(test, feature = "test-stack-guard"))]
mod tests {
    use super::*;
    use crate::arch::{disable_interrupts, restore_interrupts};

    #[test_case]
    fn canary_intact_after_init() {
        assert!(check());
    }

    #[test_case]
    fn canary_detects_corruption() {
        // Disable interrupts so the timer handler doesn't see the corrupted canary
        let prev = disable_interrupts();
        let ptr = &raw const __stack_guard as *mut usize;
        unsafe { core::ptr::write_volatile(ptr, 0) };
        assert!(!check());
        // Restore canary before re-enabling interrupts
        unsafe { core::ptr::write_volatile(ptr, 0xDEAD_BEEF) };
        restore_interrupts(prev);
    }
}
