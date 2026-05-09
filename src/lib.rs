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

// fs/volume.rs uses `alloc::vec::Vec`. The kernel binary gets `alloc`
// via main.rs's `extern crate alloc;`; the lib crate needs the same
// declaration so the path resolves whether we're in test mode (where
// std re-exports alloc) or no_std mode (where alloc must be brought
// in explicitly).
extern crate alloc;

// The real `print!`/`println!` macros live in main.rs and write to UART.
// Shared code in `kernel/alloc/` may want to emit debug prints via
// `crate::print!`/`crate::println!`; those calls resolve against whichever
// crate's root is currently being compiled. In the binary crate they hit
// the UART macros; in this lib crate they need something at the root to
// resolve to. These no-op stubs discard their arguments silently, so
// shared code can use `crate::print!`/`crate::println!` without breaking
// host / Miri builds.
/// No-op stub for `print!` in the lib crate. See module docs above.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {};
}

/// No-op stub for `println!` in the lib crate. See module docs above.
#[macro_export]
macro_rules! println {
    () => {};
    ($($arg:tt)*) => {};
}

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

// `fs/mod.rs` and `fs/volume.rs` reference `crate::drivers::virtio`
// for the BlkError type and the `read_block`/`write_block` block I/O
// functions. The lib crate doesn't include the real virtio driver, so
// here is a tiny stub: the BlkError variants match the real type for
// source-level compatibility, and the block I/O functions panic if
// actually called. Lib-crate tests never exercise volume.rs's disk
// paths (they're gated to kernel-only), so the stubs are pure
// type-system glue.
#[allow(dead_code)]
mod drivers {
    pub mod virtio {
        #[derive(Debug)]
        pub enum BlkError {
            SectorOutOfRange,
            DeviceError(u8),
            Timeout,
        }

        // Must match crate::hal::BLOCK_SIZE in the kernel binary.
        pub fn read_block(_block: u32, _buf: &mut [u8; 512]) -> Result<(), BlkError> {
            panic!("virtio::read_block stub: must not be called from the lib crate")
        }

        pub fn write_block(_block: u32, _buf: &[u8; 512]) -> Result<(), BlkError> {
            panic!("virtio::write_block stub: must not be called from the lib crate")
        }
    }

    #[allow(dead_code)]
    pub mod uart {
        pub fn direct_write_byte(_byte: u8) {
            panic!("uart::direct_write_byte stub: must not be called from the lib crate")
        }
    }
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

// Pull the fs module tree in directly via fs/mod.rs (which loads bpb,
// dir_entry, and volume). bpb.rs and dir_entry.rs are pure logic and
// run as host tests; volume.rs's production code compiles fine against
// the drivers stub above, but its tests are kernel-only and gated on
// `target_os = "none"` so they don't fire here.
#[allow(missing_docs, dead_code)]
mod fs;

#[cfg(test)]
mod tests {
    #[test]
    fn test_lib_compiles() {
        // Placeholder test to verify lib crate is testable on host
        assert_eq!(2 + 2, 4);
    }
}
