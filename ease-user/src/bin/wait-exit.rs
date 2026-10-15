//! Test case - user process blocks on a key, then exits cleanly.
//!
//! Gives a kernel test a thread whose lifetime it controls: the thread
//! parks in GET_CHAR and only exits once the test injects a key.

#![no_std]
#![no_main]

use ease_ulib::fds; // linked for its lang items and entry point, not its names

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    let mut buf = [0u8; 1];
    let _ = ease_ulib::read(fds::STDIN, &mut buf);
}
