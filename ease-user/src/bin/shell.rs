//! Built in shell for EASE

#![no_std]
#![no_main]

use ease_ulib as lib;
use lib::ascii;

/// Proto shell
///
/// Echos read stdin characters and starts a new line on Enter
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    const LINE_LEN: usize = 80;
    const READ_LEN: usize = 4;
    let mut read_buf = [0u8; READ_LEN];
    let mut line_buf = [0u8; LINE_LEN];
    let mut pos = 0;
    loop {
        // Blocks until at least one byte is available
        let Ok(bytes_read) = lib::read(lib::fds::STDIN, &mut read_buf) else {
            continue;
        };
        for &key in &read_buf[..bytes_read] {
            if key == ascii::CR {
                let _ = lib::put_char(ascii::CR);
                let _ = lib::put_char(ascii::LF);
                pos = 0;
            } else if (key.is_ascii_graphic() || key == b' ') && pos < line_buf.len() {
                line_buf[pos] = key;
                pos += 1;
                let _ = lib::put_char(key);
            }
            // Anything else (control bytes, or a full line) is dropped
        }
    }
}
