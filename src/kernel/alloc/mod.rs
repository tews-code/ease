//! Allocators

#[cfg(feature = "alloc-bump")]
pub mod bump;
#[cfg(feature = "alloc-freelist")]
pub mod freelist;

pub fn init() {
    // Note: the freelist allocator is initialised from main.rs via
    // `init_global_allocator()`, since the global static and linker
    // symbols live in the binary crate.
    #[cfg(feature = "alloc-bump")]
    bump::init();
}

fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}
