//! OS standard memory regions
//!
//! Region sizes must be power-of-two and aligned to their own size (NAPOT)
//! This is true for buddy allocator needs, used for stacks and PMP

use core::alloc::Layout;
use core::ptr::NonNull;

use super::{Pool, alloc_in, dealloc_in};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Order(u8);

#[allow(dead_code)]
impl Order {
    pub(crate) const B512: Self = Self(9);
    pub(crate) const KB1: Self = Self(10);
    pub(crate) const KB2: Self = Self(11);
    pub(crate) const KB4: Self = Self(12);
    pub(crate) const KB8: Self = Self(13);
    pub(crate) const KB16: Self = Self(14);
    pub(crate) const KB32: Self = Self(15);
    pub(crate) const KB64: Self = Self(16);
    pub(crate) const KB128: Self = Self(17);

    const ALL: [Self; 9] = [
        Self::B512,
        Self::KB1,
        Self::KB2,
        Self::KB4,
        Self::KB8,
        Self::KB16,
        Self::KB32,
        Self::KB64,
        Self::KB128,
    ];
}

impl Order {
    pub(crate) const fn size(self) -> usize {
        1usize << self.0
    }

    pub(crate) const fn align(self) -> usize {
        self.size()
    }

    pub(crate) const fn layout(self) -> Layout {
        match Layout::from_size_align(self.size(), self.align()) {
            Ok(layout) => layout,
            Err(_) => panic!("Order size and align are equal powers of two within isize::MAX"),
        }
    }
}

pub(crate) struct MemRegion {
    base: NonNull<u8>,
    order: Order,
    dealloc_pool: Option<Pool>,
}

impl MemRegion {
    /// Allocates a new MemRegion.
    ///
    /// If allocation fails returns None
    pub(crate) fn from_heap(pool: Pool, order: Order) -> Option<Self> {
        let base = alloc_in(pool, order.layout())?;
        // The allocator promised us `layout.align()` alignment; assert it in debug builds.
        debug_assert!(
            base.addr().get() % order.align() == 0,
            "MemRegion base not aligned for base={:p}, align={:x} from pool {:?}",
            base.as_ptr(),
            order.align(),
            pool
        );
        Some(Self {
            base,
            order,
            dealloc_pool: Some(pool),
        })
    }

    /// Sets a new MemRegion over a fixed NAPOT region.
    ///
    /// Panics if region is not NAPOT
    pub(crate) fn from_fixed(base: NonNull<u8>, order: Order) -> Self {
        assert!(
            base.addr().get().is_multiple_of(order.align()),
            "MemRegion base not aligned base={:p}, align={}",
            base.as_ptr(),
            order.align()
        );
        Self {
            base,
            order,
            dealloc_pool: None,
        }
    }

    pub(crate) fn base(&self) -> NonNull<u8> {
        self.base
    }

    pub(crate) fn top(&self) -> NonNull<u8> {
        // Safety: Base is aligned and size is less than isize::MAX
        // Result is one-past-the-end of the region's own allocation, which is permitted.
        unsafe { self.base.add(self.order.size()) }
    }

    pub(crate) fn base_addr(&self) -> usize {
        self.base.addr().into()
    }

    pub(crate) fn size(&self) -> usize {
        self.order.size()
    }
}

impl Drop for MemRegion {
    /// Deallocates the region from a pool if it was allocated
    fn drop(&mut self) {
        if let Some(pool) = self.dealloc_pool {
            dealloc_in(pool, self.base, self.order.layout());
        }
    }
}
