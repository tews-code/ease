//! Test case - user process spins forever

#![no_std]
#![no_main]

use ease_ulib as _; // linked for its lang items and entry point, not its names, hence "as _"

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    loop {
        core::hint::spin_loop();
    }
}
