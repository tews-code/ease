//! Test case - user process exits on fault

#![no_std]
#![no_main]

use ease_ulib as _; // linked for its lang items and entry point, not its names, hence "as _"

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    unsafe {
        core::ptr::read_volatile(4 as *const u32);  // Triggers PMP protection near address 0
    }
    loop {
        core::hint::spin_loop();    // Keep spinning to allow test case to detect failure
    }
}
