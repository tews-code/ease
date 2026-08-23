//! Test case - user process exits cleanly

#![no_std]
#![no_main]

use ease_ulib as lib;

/// Immediate clean exit
pub extern "C" fn main() {
}

#[unsafe(no_mangle)]
extern "C" fn _start() {
    main();
    lib::exit();
}
