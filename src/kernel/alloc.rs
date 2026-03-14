//! Bump Allocator
//!
//! Simple bump allocator

use core::alloc::GlobalAlloc;

use super::sync::IrqSpinLock;

/// Bump allocator
#[global_allocator]
pub static BUMP_ALLOCATOR: BumpAllocator = BumpAllocator::new();

pub struct BumpAllocator {
    inner: IrqSpinLock<BumpAllocatorInner>,
}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            inner: IrqSpinLock::new(BumpAllocatorInner {
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

#[cfg(test)]
mod tests {

    // TESTS

    #[test_case]
    fn test_vec_allocation() {
        use alloc::vec::Vec;
        let mut v = Vec::new();
        v.push(1);
        v.push(2);
        v.push(3);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], 1);
    }

    #[test_case]
    fn test_string_allocation() {
        use alloc::string::String;
        let s = String::from("hello heap!");
        assert!(s.contains("heap"));
    }

    // BENCHMARKS

    use crate::bench;

    const ITERATIONS: u32 = 10;

    mod baselines {
        pub const BOX_NEW_U64: u64 = 360_000;
        pub const VEC_PUSH_100_ITEMS: u64 = 2_000_000;
        pub const STRING_FROM_SHORT: u64 = 240_000;
    }

    #[test_case]
    fn bench_small_allocation() {
        use alloc::vec::Vec;
        use core::hint::black_box;
        bench::check(
            "Vec::push 100 items",
            baselines::VEC_PUSH_100_ITEMS,
            ITERATIONS,
            || {
                let mut v: Vec<u32> = Vec::new();
                for i in 0..100 {
                    v.push(black_box(i));
                }
            },
        );
    }

    #[test_case]
    fn bench_string_allocation() {
        use alloc::string::String;
        bench::check(
            "String::from short",
            baselines::STRING_FROM_SHORT,
            ITERATIONS,
            || {
                let _ = String::from("hello");
            },
        );
    }

    #[test_case]
    fn bench_box_allocation() {
        use alloc::boxed::Box;
        use core::hint::black_box;
        bench::check("Box::new u64", baselines::BOX_NEW_U64, ITERATIONS, || {
            let _ = Box::new(black_box(42u64));
        });
    }
}
