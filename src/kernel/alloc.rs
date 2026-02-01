//! Bump Allocator
//!
//! Simple bump allocator

use core::alloc::GlobalAlloc;

use super::sync::SpinLock;

/// Bump allocator
#[global_allocator]
pub static BUMP_ALLOCATOR: BumpAllocator = BumpAllocator::new();

pub struct BumpAllocator {
    inner: SpinLock<BumpAllocatorInner>,
}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(BumpAllocatorInner {
                heap_start: 0,
                heap_end: 0,
                next: 0,
            }),
        }
    }

    fn init(&self) {
        unsafe extern "C" {
            static __heap_start: u8;
            static __heap_end: u8;
        }

        let mut allocator = self.inner.lock();
        allocator.heap_start = &raw const __heap_start as usize;
        allocator.heap_end = &raw const __heap_end as usize;
        allocator.next = &raw const __heap_start as usize;
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let mut allocator = self.inner.lock();
        let start = align_up(allocator.next, layout.align());
        let end = start + layout.size();

        if end > allocator.heap_end {
            return core::ptr::null_mut();
        }

        allocator.next = end;
        start as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}

struct BumpAllocatorInner {
    heap_start: usize,
    heap_end: usize,
    next: usize,
}

/// Initialize the global allocator. Call once at boot.
pub fn init() {
    BUMP_ALLOCATOR.init();
}

fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}
