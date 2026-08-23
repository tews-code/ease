//! Built in shell for EASE

#![no_std]
#![no_main]

use ease_ulib as lib;
use lib::ascii;

/// Proto shell
///
/// Fills a buffer with characters from the keyboard and reprints on Enter
pub extern "C" fn main() {
    const LINE_LEN: usize = 64;
    let mut buf = [0u8; LINE_LEN];
    let mut pos = 0;
    loop {
        if let Some(key) = lib::get_key().unwrap() {
            let key = key as u8;
            if key == ascii::CR {
                let _ = lib::put_char(ascii::CR);
                let _ = lib::put_char(ascii::LF);
                for i in 0..pos {
                    let _ = lib::put_char(buf[i]);
                }
                pos = 0;
            } else {
                if key.is_ascii() {
                    buf[pos] = key;
                    pos += 1;
                }
            }
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start() {
    main();
    lib::exit();
}
