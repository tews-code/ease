//! EASE library - pure logic that can be tested on host
//!
//! This crate contains hardware-independent logic that can be tested
//! using standard `#[test]` on the development machine.
//!
//! # Testing Strategy
//!
//! - **Host tests (`cargo test --lib`)**: Pure logic, parsers, data structures
//! - **QEMU tests (`cargo test --bin ease`)**: Hardware-dependent code (UART, interrupts)
//!
//! Note: `cargo test --tests` (integration tests in `tests/` directory) is not
//! currently supported because it tries to compile `main.rs` which contains
//! RISC-V assembly that fails on the host.
//!
//! Future modules (parser, fat16, gap_buffer, etc.) will live here
//! with standard `#[cfg(test)] mod tests { ... }` blocks.

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

// Future modules will be added here as development progresses:
// pub mod parser;
// pub mod fat16;
// pub mod gap_buffer;

#[cfg(test)]
mod tests {
    #[test]
    fn test_lib_compiles() {
        // Placeholder test to verify lib crate is testable on host
        assert_eq!(2 + 2, 4);
    }
}
