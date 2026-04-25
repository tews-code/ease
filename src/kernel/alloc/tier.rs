//! Tier allocator using buddy and slab family

use core::alloc::GlobalAlloc;

use crate::kernel::alloc::buddy::Buddy;
use crate::kernel::alloc::slab::Pool;

const BASE_SIZE: usize = 4096;

pub struct Tier {
    pool_16: Pool<16, { BASE_SIZE / 16 }>,
    pool_32: Pool<32, { BASE_SIZE / 32 }>,
    pool_64: Pool<64, { BASE_SIZE / 64 }>,
    pool_128: Pool<128, { BASE_SIZE / 128 }>,
    pool_256: Pool<256, { BASE_SIZE / 256 }>,
    buddy: Buddy,
}

impl Tier {
    pub const fn new() -> Self {
        Self {
            pool_16: Pool::new(),
            pool_32: Pool::new(),
            pool_64: Pool::new(),
            pool_128: Pool::new(),
            pool_256: Pool::new(),
            buddy: Buddy::new(),
        }
    }

    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        unsafe { self.buddy.init(start, size) };
    }
}

unsafe impl GlobalAlloc for Tier {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        unsafe { self.buddy.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        unsafe { self.buddy.dealloc(ptr, layout) }
    }
}
