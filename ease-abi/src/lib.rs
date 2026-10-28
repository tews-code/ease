//! System Calls
//!
//! Shared system call definitions and syscall error codes.
//!
//! We take inspiration from the RISCV SBI approach on syscall responses:
//!  `a0` holds 0 on success or a syscall error code on failure
//!  `a1` holds the value on success or zero on error
//!  `a2` for some calls this holds an additional return value on success or zero on error.
//!
//! Also holds
//! - Error codes
//! - File descriptor common values
//! - the maximum heap size
//! - common ASCII codes

#![no_std]

pub mod syscall {
    /// Exit the user process, exiting all threads and releasing all resources
    pub const EXIT: usize = 0;
    /// Read from the given file descriptor
    pub const READ: usize = 1;
    /// Write to the given file descriptor
    pub const WRITE: usize = 2;
    /// Request memory from the kernel for the entire process; must only be called once before the first allocation.
    /// The kernel allocator will round up to a suitable region and return the base address in `a1` and the region size in `a2`.
    pub const ALLOC: usize = 3;

    // Test-only syscalls live at 100+ so real syscall growth never
    // collides. The kernel only wires them up in test builds; a
    // production program issuing one gets the unknown-syscall panic.
    pub const TEST_MUTEX_BLOCK: usize = 100;
}

pub mod fds {
    pub const STDIN: usize = 0;
    pub const STDOUT: usize = 1;
    pub const STDERR: usize = 2;
}

/// Syscall error codes
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    // Skip 0 as it indicates success
    /// Item not found
    NotFound = 1,
    /// Buffer is invalid, for example outside of the process memory range
    BadBuffer = 2,
    /// File descriptor is invalid
    BadFd = 3,
    /// Attempt to write but zero bytes written; Produced by `ulib` not kernel.
    WriteZero = 4,
    /// Syscall refused
    Interrupted = 5,
    /// The process already has a heap (second call)
    HeapAlreadyAllocated = 6,
    /// The requested size is zero or over the maximum
    HeapSizeNotSupported = 7,
    /// Not enough memory to satisfy the heap request
    NotEnoughHeapMemory = 8,
}
/// Convert an error number into the Error enum.
///
/// If the error is successfully mapped, returns `Ok` holding the error variant.
/// If the error is not mapped, returns `Err` holding the unmapped value.
impl TryFrom<usize> for Error {
    type Error = usize; // when decoding fails, hand back the raw number so it can be printed
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            v if v == Error::NotFound as usize => Ok(Error::NotFound),
            v if v == Error::BadBuffer as usize => Ok(Error::BadBuffer),
            v if v == Error::BadFd as usize => Ok(Error::BadFd),
            v if v == Error::WriteZero as usize => Ok(Error::WriteZero),
            v if v == Error::Interrupted as usize => Ok(Error::Interrupted),
            v if v == Error::HeapAlreadyAllocated as usize => Ok(Error::HeapAlreadyAllocated),
            v if v == Error::HeapSizeNotSupported as usize => Ok(Error::HeapSizeNotSupported),
            v if v == Error::NotEnoughHeapMemory as usize => Ok(Error::NotEnoughHeapMemory),
            _ => Err(value),
        }
    }
}
/// Convert an Error into a number.
impl From<Error> for usize {
    fn from(value: Error) -> Self {
        value as usize
    }
}

/// A user program can request at most 4MiB of heap
pub const MAX_HEAP_SIZE: usize = 4 * 1024 * 1024;

// ASCII chars that are used for console and serial control
pub mod ascii {
    pub const BELL: u8 = 0x07;
    pub const BS: u8 = 0x08;
    pub const TAB: u8 = 0x09;
    pub const LF: u8 = 0x0A;
    pub const FF: u8 = 0x0C;
    pub const CR: u8 = 0x0D;
    pub const DEL: u8 = 0x7F;
}
