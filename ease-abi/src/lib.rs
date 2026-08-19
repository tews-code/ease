//! System Calls
//!
//! Shared system call definitions.
//! Also holds
//! - a module of decoded special keys for console
//! - common ASCII codes

#![no_std]

pub const EXIT: usize = 0;
pub const PUT_CHAR: usize = 1;
pub const GET_CHAR: usize = 2;

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
