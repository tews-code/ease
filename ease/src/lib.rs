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

/// No-op stub for `dprintln!` in the lib crate. See module docs above.
#[macro_export]
macro_rules! dprintln {
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
    pub mod mmio {
        pub fn read32(_base: usize, _offset: usize) -> usize {
            0
        }
        // Stub
        pub fn write32(_base: usize, _offset: usize, _bits: u32) {}
    }

    pub mod regs {
        pub fn sp() -> usize {
            0
        }
    }
    pub mod interrupts {
        #[inline]
        pub fn disable() -> usize {
            0
        }
        #[inline]
        pub fn restore(_prev: usize) {}
        #[inline]
        pub fn enabled() -> bool {
            false
        }
    }
    pub mod csr {
        #[inline]
        pub fn rdcycles() -> u64 {
            0
        }
        pub mod mie {
            pub const MSIE: usize = 0;
            #[inline]
            pub fn enable_bits(_bits: usize) {}
        }
    }

    // Stubs for `kernel::profile` so the lib crate can compile profiled
    // functions in shared modules (e.g. `kernel::collection::spsc`). The
    // profile module itself is never exercised from host tests; these
    // stubs just satisfy the type checker.
    #[inline]
    pub fn hart_id() -> usize {
        0
    }
}

/// Stub
pub mod board {
    /// Stub
    pub mod clint {
        /// Stub
        pub const BASE: usize = 0;
    }
    /// Stub
    pub mod virtio {
        /// Stub
        pub mod blk {
            /// Stub
            pub const BLOCK_SIZE: usize = 512;
        }
    }
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
        pub mod blk {
            #[derive(Debug, PartialEq, Eq)]
            pub enum BlkError {
                SectorOutOfRange,
                DeviceError(u8),
                Timeout,
            }

            // Must match crate::board::virtio_blk::BLOCK_SIZE in the kernel binary.
            pub fn read_block(_block: u32, _buf: &mut [u8; 512]) -> Result<(), BlkError> {
                panic!("virtio::read_block stub: must not be called from the lib crate")
            }

            pub fn write_block(_block: u32, _buf: &[u8; 512]) -> Result<(), BlkError> {
                panic!("virtio::write_block stub: must not be called from the lib crate")
            }
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
    pub mod fd;
    pub mod ipi {}
    pub mod percpu {
        pub fn set_needs_reschedule() {}
    }
    pub mod profile;
    pub mod sync;
    pub mod timer {
        // Stub for `kernel::profile`. Real value lives in the binary
        // crate's timer module; this satisfies the type checker for the
        // lib build.
        pub const CYCLES_PER_US: u64 = 1;

        pub fn elapsed_ms() -> u64 {
            panic!("timer stub: must not be called from the lib crate")
        }

        pub fn elapsed() -> u64 {
            panic!("timer stub: must not be called from the lib crate")
        }
    }

    #[allow(dead_code)]
    pub mod sched {

        pub const THREADS_MAX: usize = 16;

        #[derive(Clone, Copy)]
        pub struct ThreadHandle {
            pub id: u32,
            pub idx: usize,
        }

        impl ThreadHandle {
            pub fn idx(&self) -> usize {
                self.idx
            }
        }

        pub fn current_thread() -> ThreadHandle {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn set_next_waiter(_handle: &ThreadHandle, _next: Option<ThreadHandle>) {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn get_next_waiter(_thread: &ThreadHandle) -> Option<ThreadHandle> {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn park() {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn park_if_blocked() {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn unpark(_handle: &ThreadHandle) {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn unpark_by_index(_idx: usize) {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn set_self_blocked() {
            panic!("sched stub: must not be called from the lib crate");
        }

        pub fn set_self_blocked_until(_deadline_ms: u64) {
            panic!("sched stub: must not be called from the lib crate");
        }
        /// Park this thread in blocked state with wakeup deadline
        pub fn park_if_blocked_until(_deadline_ms: u64) {
            panic!("sched stub: must not be called from the lib crate");
        }

        /// Park this thread in blocked state with wakeup deadline
        pub fn set_needs_wakeup(_idx: usize) {
            panic!("sched stub: must not be called from the lib crate");
        }

        /// Checks if the current thread has the flag indicating the user thread must exit
        pub(crate) fn current_user_thread_needs_exit() -> bool {
            panic!("sched stub: must not be called from the lib crate");
        }
    }
}

#[allow(dead_code)]
mod io {
    pub struct DirectWriter;
    impl core::fmt::Write for DirectWriter {
        fn write_str(&mut self, _: &str) -> core::fmt::Result {
            Ok(())
        }
    }
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
