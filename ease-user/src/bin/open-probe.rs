//! Test case - calls `open` through ease-ulib and prints one line per case
//! with what came back, for the kernel test to check.
//!
//! The kernel test creates the files first: `OPENME.TXT` and the directory
//! `OPENDIR` in the root, and `INNER.TXT` inside `OPENDIR`.
//!
//! Each line is `OPEN <case>: ok` or `OPEN <case>: <error>`. The probe never
//! closes anything (there is no CLOSE yet), so every successful open keeps a
//! slot in the file descriptor table. The last case opens until the table
//! refuses and reports how many opens succeeded over the whole run: the table
//! has 8 slots and 3 hold the standard descriptors, so that must be 5.

#![no_std]
#![no_main]

use core::fmt::Debug;
use ease_ulib::{Fd, FileAccess, open, println};

/// 129 bytes: one more than ease_abi::MAX_PATH_TEXT
const TOO_LONG: &str = concat!(
    "0123456789012345678901234567890123456789",
    "0123456789012345678901234567890123456789",
    "0123456789012345678901234567890123456789",
    "012345678",
);
const _: () = assert!(TOO_LONG.len() == 129);

/// More opens than the file descriptor table can ever hold
const FILL_ATTEMPTS: usize = 16;

/// Print the outcome of one case and return whether it opened a file.
///
/// Generic over the error so that the probe needs nothing from ulib but its
/// Debug text.
fn report<E: Debug>(case: &str, result: Result<Fd, E>) -> bool {
    match result {
        Ok(_) => {
            println!("OPEN {}: ok", case);
            true
        }
        Err(e) => {
            println!("OPEN {}: {:?}", case, e);
            false
        }
    }
}

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    let mut opened = 0;
    let cases = [
        ("plain", "OPENME.TXT", FileAccess::READ),
        ("missing", "NOSUCH.TXT", FileAccess::READ),
        ("empty", "", FileAccess::READ),
        ("long", TOO_LONG, FileAccess::READ),
        ("create-only", "OPENME.TXT", FileAccess::CREATE),
        ("write", "OPENME.TXT", FileAccess::WRITE),
        ("subdir", "OPENDIR/INNER.TXT", FileAccess::READ),
        ("directory", "OPENDIR", FileAccess::READ),
    ];
    for (case, path, access) in cases {
        if report(case, open(path, access)) {
            opened += 1;
        }
    }
    // Fill what is left of the table
    for _ in 0..FILL_ATTEMPTS {
        match open("OPENME.TXT", FileAccess::READ) {
            Ok(_) => opened += 1,
            Err(e) => {
                println!("OPEN full after {}: {:?}", opened, e);
                break;
            }
        }
    }
    println!("OPEN DONE");
}
