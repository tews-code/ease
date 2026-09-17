//! Test case - user process blocks on a key, then exits cleanly.
//!
//! Gives a kernel test a thread whose lifetime it controls: the thread
//! parks in GET_CHAR and only exits once the test injects a key.

#![no_std]
#![no_main]

use ease_ulib as _; // linked for its lang items and entry point, not its names

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    let _ = ease_ulib::get_key();
}
