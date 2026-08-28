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
    ecall1(syscall::PUT_CHAR, ch as usize).map(|_| () )
}
/// Get a key from the keyboard
///
/// Returns `None` if no key has been pressed
/// Keys are returned as `Some(usize)` to cater for
/// ASCII keys (below 0xFF) and special keys (above 0xFF)
/// See [ease_abi::special_key] for details
pub fn get_key() -> Result<Option<usize>, Error> {
    ecall0(syscall::GET_CHAR).map(|v| if v == 0 { None } else { Some(v) })
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
/// Common syscall asm and error decoded for zero argument syscalls
/// `error` in a0, `value` in a1, zero means success
fn ecall0(syscall: usize) -> Result<usize, Error> {
    let error: usize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
             clobber_abi("C"),
             out("a0") error,
             out("a1") value,
             in("a7") syscall,
        );
    }
    if error == 0 {
        Ok(value)
    } else {
        Err(Error::try_from(error).expect("kernel returned an unknown error code"))
    }
}
/// Common syscall asm and error decoded for one argument syscalls
/// `error` in a0, `value` in a1, zero means success
fn ecall1(syscall: usize, arg: usize) -> Result<usize, Error> {
    let error: usize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
             clobber_abi("C"),
             inout("a0") arg => error,
             out("a1") value,
             in("a7") syscall,
        );
    }
    if error == 0 {
        Ok(value)
    } else {
        Err(Error::try_from(error).expect("kernel returned an unknown error code"))
    }
}
