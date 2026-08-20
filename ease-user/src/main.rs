//! User binaries for EASE

#![no_std]
#![no_main]

use ease_ulib as lib;
use ease_ulib::ascii as ascii;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}

/// Proto shell
///
/// Fills a buffer with characters from the keyboard and reprints on Enter
pub extern "C" fn shell() -> ! {
    const LINE_LEN: usize = 64;
    let mut buf = [0u8; LINE_LEN];
    let mut pos = 0;
    loop {
        if let Some(key) = lib::get_key() {
            let key = key as u8;
            if key == ascii::CR {
                lib::put_char(ascii::CR);
                lib::put_char(ascii::LF);
                for i in 0..pos {
                    lib::put_char(buf[i]);
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
extern "C" fn _start() -> ! {
    shell();
}
