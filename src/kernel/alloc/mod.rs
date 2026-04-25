//! Allocators

// Each allocator compiles unconditionally in host builds so its
// `host_tests` module runs under every `cargo test --lib` and Miri
// invocation regardless of which allocator is active in the kernel
// binary. In kernel binary builds each is gated behind its own feature,
// so only one provides the `#[global_allocator]` static in main.rs.
pub mod buddy;
pub mod bump;
pub mod freelist;
pub mod slab;
pub mod tier;

// Allocator-agnostic QEMU benchmark suite. Compiles only when one of the
// global allocators is active and the test-alloc feature is on.
#[cfg(all(
    test,
    target_os = "none",
    feature = "test-alloc",
    any(
        feature = "alloc-slab",
        feature = "alloc-freelist",
        feature = "alloc-bump",
        feature = "alloc-buddy"
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
