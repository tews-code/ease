//! EASE library - pure logic that can be tested on host
//!
//! This crate contains hardware-independent logic that can be tested
//! using standard `#[test]` on the development machine.
//!
//! # Testing Strategy
//!
//! - **Host tests (`cargo test --lib`)**: Pure logic, parsers, data structures,
//!   and any kernel modules whose code is target-independent (e.g. the
//!   freelist allocator). Run under Miri to verify soundness:
//!   `cargo +nightly miri test --lib --target $HOST_TARGET`
//! - **QEMU tests (`cargo test --bin ease`)**: Hardware-dependent code (UART,
//!   interrupts, virtio, the global allocator instance).
//!
//! Note: `cargo test --tests` (integration tests in `tests/` directory) is not
//! currently supported because it tries to compile `main.rs` which contains
//! RISC-V assembly that fails on the host.

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

// `kernel/sync.rs` references `crate::arch::{disable_interrupts,
// restore_interrupts}` when compiling for the kernel target. The lib
// crate does not pull in the real `arch` module (it depends on more
// linker symbols and inline assembly than we want to compile here), so
// we provide a tiny stub with matching signatures. The stub is never
// actually called from host code paths because `AllocatorLock` resolves
// to `SpinLock` on host targets, but it makes `cargo doc` /
// `cargo clippy` succeed when they build the lib crate for
// `riscv32imac-unknown-none-elf`.
#[allow(dead_code)]
mod arch {
    #[inline]
    pub fn disable_interrupts() -> usize {
        0
    }
    #[inline]
    pub fn restore_interrupts(_prev: usize) {}
}

// Pull just the host-compatible pieces of the kernel module tree into
// the lib crate so the freelist allocator, sync primitives and
// collections can be unit-tested. We do NOT load `kernel/mod.rs`
// because that pulls in `panic`, `stack_guard`, `timer` which depend
// on `crate::arch`/`crate::board`/`crate::drivers` — modules that only
// exist in the binary crate. Instead we declare an inline `kernel`
// module and use `#[path]` to point at the kernel directory so that
// `crate::kernel::sync` and friends still resolve identically inside
// freelist.rs whether it's compiled as part of the lib or the binary.
#[allow(missing_docs, dead_code)]
#[path = "kernel"]
mod kernel {
    pub mod alloc;
    pub mod collection;
    pub mod sync;
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_lib_compiles() {
        // Placeholder test to verify lib crate is testable on host
        assert_eq!(2 + 2, 4);
    }
}
