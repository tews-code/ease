//! User library for EASE processes

#![no_std]

use core::arch::asm;

use ease_abi::syscall;
use ease_abi::Error;
pub use ease_abi::ascii;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Attempt to print the info
    println!("USER PANIC: {}", info.message());
    exit();
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

unsafe extern "C" {
    /// Common "main" exported by all user programs
    /// This is picked up by _start and run once, followed by exit.
    fn main();
}
/// Start function ensures we run the user program then exit cleanly
///
/// Linker script ensures that _start is at the beginning of .text
#[unsafe(no_mangle)]
#[unsafe(link_section=".text._start")]
pub extern "C" fn _start() -> ! {
    unsafe { main() };
    exit()
}

// PRINT MACROS

/// Struct that prints via syscall::PUT_CHAR
pub struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            let _ = put_char(b);
        }
        Ok(())
    }
}

/// Print formatted string .
/// Printing may be interleaved
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        use $crate::Writer;
        let _ = write!(Writer, $($arg)*);
    }}
}

/// Print with newline
/// Printing may be interleaved
#[macro_export]
macro_rules! println {
    () => { {
        $crate::print!("\n");
    }};
    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*));
    }}
}

