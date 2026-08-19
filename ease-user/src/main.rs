#![no_std]

/// Proto shell
///
/// Fills a buffer with characters from the keyboard and reprints on Enter
#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub extern "C" fn line() {
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
