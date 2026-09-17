//! Static cell
//!
//! Used for one-time initialisation of statics
//! The [StaticCell::init] method provides a mutable
//! borrow of the contents only once. A second call
//! will panic.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct StaticCell<T> {
    data: UnsafeCell<MaybeUninit<T>>,
    initialised: AtomicBool,
}

// Safety: In order to be allowed use in statics, we need StaticCell to be Sync.
// We can be confident it is Sync because the atomic flag is used to ensure only
// one thread has any reference (and then, only once).
// When we hand out a &mut T the receiving thread can make any changes to it, this
// is equivalent to T being Send - hence the additional bound to ensure we don't use
// this on a T which can't be used in a Send context.
unsafe impl<T: Send> Sync for StaticCell<T> {}

impl<T: Send> StaticCell<T> {
    /// Const new to support statics
    pub(crate) const fn new() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            initialised: AtomicBool::new(false),
        }
    }
    /// Get a 'static mutable reference to the value held in the static cell.
    /// A `T` with a destructor never has that destructor run.
    ///
    /// # Panics #
    /// Panics if called twice
    #[allow(clippy::mut_from_ref)]
    pub(crate) fn init(&'static self, value: T) -> &'static mut T {
        // We have Relaxed ordering on success as there is no memory ordering
        // against the atomic, it is purely a gate to initialisation and the loser
        // panics
        if self
            .initialised
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            panic!("StaticCell initialised twice");
        }
        // We are the first and only initialisation - write the value
        // Safety: The raw pointer to data is aligned and valid for write.
        // Since MaybeUninit::write takes &mut MaybeUninit<T>, we have to be
        // confident that this is exclusive - which we are since we are the
        // only post-CAS thread
        unsafe { (*self.data.get()).write(value) }
    }
}
