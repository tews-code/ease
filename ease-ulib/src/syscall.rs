//! System calls

use core::arch::asm;
use core::num::NonZeroUsize;
use ease_abi::{Error, fds, syscall};

/// Print a single u8 byte to standard out
pub fn put_char(ch: u8) -> Result<(), Error> {
    let buf = [ch; 1];
    ecall3(syscall::WRITE, fds::STDOUT, buf.as_ptr().addr(), 1).map(|_| ())
}

/// Fill a user slice buffer from the file descriptor `fd`.
/// Returns the number of bytes received.
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize, Error> {
    ecall3(syscall::READ, fd, buf.as_mut_ptr().addr(), buf.len())
}

/// Write a buffer from a user slice to file descriptor `fd`.
///
/// Writes as many bytes as permitted by the syscall.
/// Returns the number of bytes written on success
/// or holds an `Error` on failure.
pub fn write(fd: usize, buf: &[u8]) -> Result<usize, Error> {
    ecall3(syscall::WRITE, fd, buf.as_ptr().addr(), buf.len())
}

/// Writes the complete buffer in a user slice to file descriptor `fd`.
/// Loops over [write] until the buffer is empty.
///
/// Returns `Ok(())` on success or `Err(Error)` on failure.
pub fn write_all(fd: usize, buf: &[u8]) -> Result<(), Error> {
    let mut idx = 0;
    while idx < buf.len() {
        let bytes_written = write(fd, &buf[idx..])?;
        if bytes_written == 0 {
            return Err(Error::WriteZero);
        }; // We aren't making any progress
        idx += bytes_written;
        if idx > buf.len() {
            panic!("written more bytes than buffer holds");
        }
    }
    Ok(())
}

/// Exit a process
pub fn exit() -> ! {
    let _ = ecall0(syscall::EXIT);
    unsafe { asm!("unimp", options(noreturn)) };
}

/// Memory region
pub struct Region {
    base: NonZeroUsize,
    size: NonZeroUsize,
}

/// Request heap memory from the kernel
fn request_heap(size: usize) -> Result<Region, Error> {
    let (base, size) = ecall1_ret2(syscall::ALLOC, size)?;
    Ok(Region {
        base: NonZeroUsize::try_from(base)
            .expect("kernel should not give a zero base address for allocation"),
        size: NonZeroUsize::try_from(size)
            .expect("kernel should not give a zero size for the allocated region"),
    })
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

/// Common syscall asm and error decoded for one argument syscalls.
/// Returns an error code in a0, or if a0 is zero returns `val1` in a1 and `val2` in `a2`
fn ecall1_ret2(syscall: usize, arg: usize) -> Result<(usize, usize), Error> {
    let error: usize;
    let val1: usize;
    let val2: usize;
    unsafe {
        asm!(
            "ecall",
             clobber_abi("C"),
             inout("a0") arg => error,
             out("a1") val1,
             out("a2") val2,
             in("a7") syscall,
        );
    }
    if error == 0 {
        Ok((val1, val2))
    } else {
        Err(Error::try_from(error).expect("kernel returned an unknown error code"))
    }
}

/// Common syscall asm and error decoded for three-argument syscalls
///
/// Returns `Ok(usize)` on success or an `Error` on failure.
fn ecall3(syscall: usize, arg0: usize, arg1: usize, arg2: usize) -> Result<usize, Error> {
    let error: usize;
    let value: usize;
    unsafe {
        asm!(
            "ecall",
             clobber_abi("C"),
             inout("a0") arg0 => error,
             inout("a1") arg1 => value,
             in("a2") arg2,
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
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start() -> ! {
    unsafe { main() };
    exit()
}

/// Struct that prints via syscall::WRITE to STDOUT
pub struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_all(fds::STDOUT, s.as_bytes()).map_err(|_| core::fmt::Error)?;
        Ok(())
    }
}
