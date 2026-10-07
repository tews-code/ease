//! Test case - prints through ease-ulib's `println!`, then exits cleanly.
//!
//! Exercises the whole user print path: core::fmt, Writer::write_str,
//! write_all and the WRITE syscall. The long line is longer than the
//! kernel's per-call WRITE buffer, so write_all must loop over short
//! writes for it to arrive intact.

#![no_std]
#![no_main]

use ease_ulib::println;

/// 200 bytes: longer than one WRITE call will take
const LONG: &str = concat!(
    "0123456789012345678901234567890123456789",
    "0123456789012345678901234567890123456789",
    "0123456789012345678901234567890123456789",
    "0123456789012345678901234567890123456789",
    "0123456789012345678901234567890123456789",
);

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    println!("ULIB PRINT {} OK", 42);
    println!("{}", LONG);
}
