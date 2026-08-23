//! Test case - user process spins forever

#![no_std]
#![no_main]

use ease_ulib as lib;

/// Spin forever
pub extern "C" fn main() {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start() {
    main();
    lib::exit();
}
