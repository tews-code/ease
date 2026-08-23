//! Test case - user process exits on fault

#![no_std]
#![no_main]

use ease_ulib as lib;

/// Fault exit
pub extern "C" fn main() {
    unsafe {
        core::ptr::read_volatile(4 as *const u32);  // Triggers PMP protection near address 0
    }
    loop {
        core::hint::spin_loop();    // Keep spinning to allow test case to detect failure
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start() {
    main();
    lib::exit();
}
