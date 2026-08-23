//! User library for EASE processes

#![no_std]

use core::arch::asm;

use ease_abi::syscall;
use ease_abi::Error;
pub use ease_abi::ascii;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}

/// Print a single u8 ASCII character to console
pub fn put_char(ch: u8) -> Result<(), Error> {
    let error: usize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
            clobber_abi("C"),
            inout("a0") ch as usize => error,
            out("a1") value,
            in("a7") syscall::PUT_CHAR,
        );
    }
    if error == 0 {
        Ok(())
    } else {
        Err(Error::try_from(error).expect("kernel returned an unknown error code"))
    }
}
/// Get a key from the keyboard
///
/// Returns `None` if no key has been pressed
/// Keys are returned as `Some(usize)` to cater for
/// ASCII keys (below 0xFF) and special keys (above 0xFF)
/// See [ease_abi::special_key] for details
pub fn get_key() -> Result<Option<usize>, Error> {
    let error: usize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
            clobber_abi("C"),
            out("a0") error,
            out("a1") value,
            in("a7") syscall::GET_CHAR,
        );
    }
    if error == 0 {
        if value == 0 {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    } else {
        Err(Error::try_from(error).expect("kernel returned an unknown error code"))
    }
}
/// Exit a process
pub fn exit() -> ! {
    unsafe {
        asm!(
            "ecall",
            "unimp",
            in("a7") syscall::EXIT,
            options(noreturn),
        );
    }
}
