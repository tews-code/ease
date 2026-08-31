//! System Calls
//!
//! Shared system call definitions and
//! Syscall error codes.
//!
//! We take inspiration from the RISCV SBI approach on syscall responses:
//!  `a0` holds 0 on success or a syscall error code on failure
//!  `a1` holds the value on success or zero on error
//!
//! Also holds
//! - a module of decoded special keys for console
//! - common ASCII codes

#![no_std]

pub mod syscall {
    pub const EXIT: usize = 0;
    pub const PUT_CHAR: usize = 1;
    pub const GET_CHAR: usize = 2;

    // Test-only syscalls live at 100+ so real syscall growth never
    // collides. The kernel only wires them up in test builds; a
    // production program issuing one gets the unknown-syscall panic.
    pub const TEST_MUTEX_BLOCK: usize = 100;
}

/// Syscall error codes
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    // Skip 0 as it indicates success
    NotFound  = 1,
}

impl TryFrom<usize> for Error {
    type Error = usize; // when decoding fails, hand back the raw number so it can be printed
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            v if v == Error::NotFound as usize => Ok(Error::NotFound),
            _ => Err(value),
        }
    }
}

pub mod special_key {
    // All special keys are placed above 0xFF
    const BASE: usize = 0x100;
    pub const UNKNOWN: usize = BASE;
    pub const UP_ARROW: usize = BASE + 1;
}

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
