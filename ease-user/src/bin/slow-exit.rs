//! Test case - clean exit after a short busy delay
//!
//! The delay gives kernel tests a window to attach a second thread at
//! `_start` before the first thread exits; it must stay short enough
//! that threads drain quickly once spawned.

#![no_std]
#![no_main]

use ease_ulib as _; // linked for its entry point and panic handler, not its names

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    let mut i = 0u32;
    while core::hint::black_box(i) < 2_000_000 {
        i += 1;
    }
}
