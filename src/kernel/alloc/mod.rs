//! Allocators

#[cfg(feature = "alloc-bump")]
pub mod bump;
#[cfg(feature = "alloc-freelist")]
pub mod freelist;

pub fn init() {
    #[cfg(feature = "alloc-bump")]
    bump::init();
    #[cfg(feature = "alloc-freelist")]
    freelist::init();
}

fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}
