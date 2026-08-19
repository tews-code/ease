#![no_std]

use syscall;

#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub fn put_char(b: u8) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a0") b,
            in("a7") syscall::PUT_CHAR,
        );
    }
}

#[allow(dead_code)]
#[unsafe(link_section = ".user_text")]
pub fn get_key() -> Option<usize> {
    let mut key: usize = 0;
    unsafe {
        core::arch::asm!(
            "ecall",
            clobber_abi("C"),
            out("a0") key,
            in("a7") syscall::GET_CHAR,
        );
    }
    if key == 0 { None } else { Some(key) }
}
