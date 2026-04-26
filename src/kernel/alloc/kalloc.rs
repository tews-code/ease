//! Tier allocator using buddy and slab family

use core::alloc::GlobalAlloc;

use crate::kernel::alloc::buddy::Buddy;
use crate::kernel::alloc::slab::Slab;

const BASE_SIZE: usize = 4096;

pub struct KAlloc {
    pool_16: Slab<16>,
    pool_32: Slab<32>,
    pool_64: Slab<64>,
    pool_128: Slab<128>,
    pool_256: Slab<256>,
    buddy: Buddy,
}

impl KAlloc {
    pub const fn new() -> Self {
        Self {
            pool_16: Slab::new(),
            pool_32: Slab::new(),
            pool_64: Slab::new(),
            pool_128: Slab::new(),
            pool_256: Slab::new(),
            buddy: Buddy::new(),
        }
    }

    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        unsafe { self.buddy.init(start, size) };
    }
}

unsafe impl GlobalAlloc for KAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        unsafe { self.buddy.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        unsafe { self.buddy.dealloc(ptr, layout) }
    }
}
