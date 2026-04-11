//! Allocators

// Bump compiles unconditionally in host builds so its `host_tests`
// module can run under `cargo test --lib` and Miri. In the kernel
// binary build it stays gated behind `feature = "alloc-bump"`.
#[cfg(any(feature = "alloc-bump", not(target_os = "none")))]
pub mod bump;
#[cfg(feature = "alloc-freelist")]
pub mod freelist;
#[cfg(feature = "alloc-slab")]
pub mod slab;

// Allocator-agnostic QEMU benchmark suite. Compiles only when one of the
// global allocators is active and the test-alloc feature is on.
#[cfg(all(
    test,
    target_os = "none",
    feature = "test-alloc",
    any(
        feature = "alloc-slab",
        feature = "alloc-freelist",
        feature = "alloc-bump"
    )
))]
mod bench;

// Note: each allocator (freelist, slab, bump) is initialised from
// main.rs via `init_global_allocator()`, since the global static and
// linker symbols live in the binary crate.

#[allow(dead_code)]
fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}
