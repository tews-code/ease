//! OS standard memory regions
//!
//! Region sizes must be power-of-two and aligned to their own size (NAPOT)
//! This is true for buddy allocator needs, used for stacks and PMP

use core::alloc::Layout;
use core::ptr::NonNull;

use super::{Pool, alloc_in, dealloc_in};

#[derive(Debug)]
pub(crate) enum Error {
    /// Not enough memory for the allocation to succeed
    NotEnoughMemory,
    /// The requested heap size is not supported
    HeapSizeNotSupported,
}

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
    pub(crate) const KB256: Self = Self(18);
    pub(crate) const KB512: Self = Self(19);
    pub(crate) const KB1024: Self = Self(20);
    pub(crate) const KB2048: Self = Self(21);
    pub(crate) const KB4096: Self = Self(22);

    pub(crate) const ALL: [Self; 14] = [
        Self::B512,
        Self::KB1,
        Self::KB2,
        Self::KB4,
        Self::KB8,
        Self::KB16,
        Self::KB32,
        Self::KB64,
        Self::KB128,
        Self::KB256,
        Self::KB512,
        Self::KB1024,
        Self::KB2048,
        Self::KB4096,
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

impl TryFrom<usize> for Order {
    type Error = Error;
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        const MIN: usize = Order::B512.size();
        const MAX: usize = Order::KB4096.size();
        match value {
            0 => Err(Error::HeapSizeNotSupported),
            1..MIN => Ok(Order::B512),
            MIN..=MAX => Ok(Order(usize::trailing_zeros(value.next_power_of_two()) as u8)),
            _ => Err(Error::HeapSizeNotSupported),
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
    /// # Errors
    /// Returns [Error::NotEnoughMemory] if the allocation fails.
    pub(crate) fn from_heap(pool: Pool, order: Order) -> Result<Self, Error> {
        let base = alloc_in(pool, order.layout()).ok_or(Error::NotEnoughMemory)?;
        // The allocator promised us `layout.align()` alignment; assert it in debug builds.
        debug_assert!(
            base.addr().get() % order.align() == 0,
            "MemRegion base not aligned for base={:p}, align={:x} from pool {:?}",
            base.as_ptr(),
            order.align(),
            pool
        );
        Ok(Self {
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
    /// Confirms whether a memory extent is contained within the region
    pub(crate) fn contains(&self, base_addr: usize, len: usize) -> bool {
        base_addr >= self.base_addr()
            && base_addr
                .checked_add(len)
                .is_some_and(|a| a <= self.top().addr().get())
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

// Byte count -> Order is pure arithmetic on an untrusted size (it comes
// from a user's ALLOC request in a0), so it runs on the host and under
// Miri. Order has no Debug, so results are compared by size.
#[cfg(all(test, not(target_os = "none"), feature = "test-alloc"))]
mod host_tests {
    use super::{Error, Order};

    fn size_for(bytes: usize) -> usize {
        Order::try_from(bytes)
            .expect("size should be supported")
            .size()
    }

    #[test]
    fn zero_is_refused() {
        assert!(matches!(
            Order::try_from(0),
            Err(Error::HeapSizeNotSupported)
        ));
    }

    #[test]
    fn below_minimum_rounds_up_to_b512() {
        assert_eq!(size_for(1), 512);
        assert_eq!(size_for(100), 512);
        assert_eq!(size_for(511), 512);
    }

    #[test]
    fn exact_powers_of_two_are_kept() {
        for order in Order::ALL {
            assert_eq!(size_for(order.size()), order.size());
        }
    }

    #[test]
    fn one_byte_over_a_power_of_two_doubles() {
        assert_eq!(size_for(513), 1024);
        assert_eq!(size_for(1025), 2048);
        assert_eq!(size_for(Order::KB2048.size() + 1), Order::KB4096.size());
    }

    #[test]
    fn maximum_is_inclusive() {
        assert_eq!(size_for(ease_abi::MAX_HEAP_SIZE), ease_abi::MAX_HEAP_SIZE);
    }

    #[test]
    fn over_maximum_is_refused() {
        // usize::MAX checks the size is bounded before rounding:
        // next_power_of_two would overflow on it.
        for bytes in [ease_abi::MAX_HEAP_SIZE + 1, usize::MAX] {
            assert!(matches!(
                Order::try_from(bytes),
                Err(Error::HeapSizeNotSupported)
            ));
        }
    }
}
