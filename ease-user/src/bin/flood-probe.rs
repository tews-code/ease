//! Test case - floods standard output well past the kernel's 1 KB UART
//! transmit queue, then reports whether every write succeeded.
//!
//! Writes FLOOD_LINES numbered 64-byte lines through `write_all`. Once the
//! queue fills, WRITE must wait for space rather than return 0, otherwise
//! `write_all` fails with WriteZero and the verdict is FLOOD FAIL.

#![no_std]
#![no_main]

use ease_ulib::{println, write_all};

/// Standard output (ease-ulib does not re-export ease_abi::fds)
const STDOUT: usize = 1;
/// 32 lines x 64 bytes = 2 KB: twice the transmit queue, and small enough
/// to fit the kernel test's 4 KB output capture
const FLOOD_LINES: usize = 32;
const LINE_LEN: usize = 64;

/// All user programs _must_ export "main"
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    let mut failed = None;
    for i in 0..FLOOD_LINES {
        // "FLOOD NN " then dots, then a newline: the number lets the test
        // check order, the fixed length makes the expected text easy to build
        let mut line = [b'.'; LINE_LEN];
        line[..6].copy_from_slice(b"FLOOD ");
        line[6] = b'0' + (i / 10) as u8;
        line[7] = b'0' + (i % 10) as u8;
        line[8] = b' ';
        line[LINE_LEN - 1] = b'\n';
        if let Err(e) = write_all(STDOUT, &line) {
            failed = Some(e);
            break;
        }
    }
    match failed {
        None => println!("FLOOD OK"),
        Some(e) => println!("FLOOD FAIL {:?}", e),
    }
}
