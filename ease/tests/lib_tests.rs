//! Host-side integration tests for EASE
//!
//! These tests run on the development machine (with std), not in QEMU.
//! Use `cargo test --test lib_tests --target <host>` to run.
//!
//! # Purpose
//!
//! Test pure logic that doesn't depend on hardware:
//! - Parsers (shell commands, FAT16 structures)
//! - Data structures (gap buffer, ring buffer)
//! - Algorithms (formatting, conversion)
//!
//! Hardware-dependent tests go in `src/main.rs` and run via QEMU.
//!
//! # Limitation
//!
//! Currently, running `cargo test --tests` also compiles the binary which
//! contains RISC-V assembly. Until the binary is made target-conditional,
//! integration tests must be self-contained and not import from `ease::*`.
//!
//! Once `ease::parser`, `ease::gap_buffer`, etc. are added to lib.rs,
//! tests can import and test them here.

/// Verify the test infrastructure works.
#[test]
fn test_infrastructure() {
    assert_eq!(2 + 2, 4);
}

/// Placeholder for future parser tests.
///
/// When `ease::parser` is implemented, tests like these will verify:
/// - Command parsing: `ls -l` -> Command { name: "ls", args: ["-l"] }
/// - Edge cases: empty input, quotes, escapes
mod parser_tests {
    #[test]
    fn test_parser_placeholder() {
        // TODO: Import ease::parser and test when implemented
        // Example:
        // use ease::parser::parse;
        // let cmd = parse("echo hello").unwrap();
        // assert_eq!(cmd.name, "echo");
    }
}

/// Placeholder for future data structure tests.
///
/// When `ease::gap_buffer` is implemented:
/// - Insert at cursor
/// - Delete at cursor
/// - Move cursor
/// - Buffer growth
mod data_structure_tests {
    #[test]
    fn test_gap_buffer_placeholder() {
        // TODO: Import ease::gap_buffer and test when implemented
        // Example:
        // use ease::gap_buffer::GapBuffer;
        // let mut buf = GapBuffer::new();
        // buf.insert('a');
    }
}

/// Tests that can use `#[should_panic]` - not available in QEMU tests.
///
/// QEMU tests use a custom test framework that doesn't support `#[should_panic]`.
/// Put panic tests here instead.
mod panic_tests {
    #[test]
    #[should_panic(expected = "deliberate")]
    fn test_should_panic_works() {
        panic!("deliberate panic for testing");
    }
}
