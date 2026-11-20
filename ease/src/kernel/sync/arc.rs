//! Atomic reference count
//!
//! Note this is hand-written using Mara Bos's approach [https://mara.nl]. A
//! perfectly good implementation can be taken from the alloc crate.

use alloc::boxed::Box;
use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering, fence};

/// Holds the data and a reference count
/// The struct is deliberately private as
/// it is internal to the [Arc] itself.
#[expect(dead_code)]
struct ArcData<T> {
    ref_count: AtomicUsize,
    data: T,
}

/// The atomic reference counted object wrapper
///
/// Each Arc is a handle to the ArcData, and can be cloned and shared between threads.
/// Dropping the last Arc will also drop the ArcData.
#[expect(dead_code)]
pub struct Arc<T> {
    ptr: NonNull<ArcData<T>>, // Raw pointer as handle lifetime is separate to object lifetime
}

// Safety: If an Arc<T> is sent from one thread to another,
// then T is shared between the threads, requiring T to be Sync.
// The last thread drops its Arc also drops T, which requires T to be Send
// as this could be a different thread to the one that created the Arc.
// If T is Send + Sync, then the Arc can be Send as it is a raw pointer
// to an ArcData object which holds just the atomic and T.
unsafe impl<T: Send + Sync> Send for Arc<T> {}
// Safety: If an Arc<T> is sent from one thread to another,
// then T is shared between the threads, requiring T to be Sync.
// The last thread drops its Arc also drops T, which requires T to be Send
// as this could be a different thread to the one that created the Arc.
// If T is Send + Sync, then the Arc can be Sync as it is a raw pointer
// to an ArcData object which holds just the atomic and T, and multiple
// threads can hold a reference to an Arc<T> and Clone it.
unsafe impl<T: Send + Sync> Sync for Arc<T> {}

#[expect(dead_code)]
impl<T> Arc<T> {
    /// A new Arc<T>
    ///
    /// Note that the entire T is added to the thread stack before
    /// it is allocated. To avoid this memory restrictions wrap T in
    /// a Box before using the Arc wrapper.
    /// The reference count starts at `1`.
    pub fn new(data: T) -> Arc<T> {
        Arc {
            ptr: NonNull::from(
                // Create a raw pointer to the leaked object
                Box::leak(
                    // Leak this to remove ownership
                    Box::new(
                        // Create an allocation with a ref count of 1
                        ArcData {
                            ref_count: AtomicUsize::new(1),
                            data,
                        },
                    ),
                ),
            ),
        }
    }

    /// Get a reference to the data from the Arc object
    fn data(&self) -> &ArcData<T> {
        // Safety: The data that is pointed to is only dropped when the
        // reference count has become zero, ensuring that the pointer is
        // always valid
        unsafe { self.ptr.as_ref() }
    }

    /// Get a mutable reference to the object.
    ///
    /// Only returns Some mutable reference if there is just one reference
    /// holder. If there is more than one reference holder, then `None` is returned.
    ///
    /// This method does not take &mut self to avoid Deref ambiguity, call
    /// using `Arc::get_mut(&mut a)`.
    pub fn get_mut(arc: &mut Self) -> Option<&mut T> {
        if arc.data().ref_count.load(Ordering::Relaxed) == 1 {
            fence(Ordering::Acquire);
            // Safety: Since we only have one reference holder, we have
            // exclusive access, and the &mut reference ensures no other thread
            // can get access.
            Some(unsafe { &mut arc.ptr.as_mut().data })
        } else {
            None
        }
    }

    /// Consumes the Arc and returns the data if this is the only reference
    /// holder returns the data, or None if not the only reference holder.
    ///
    /// Only takes the object if there is exactly one reference holder in which case
    /// the data is taken and returned, otherwise returns `None`. Regardless, this Arc is dropped.
    ///
    /// Takes arc: Self to avoid clashing with other Deref, call using
    /// `Arc::into_inner()`.
    pub fn into_inner(arc: Self) -> Option<T> {
        // Prevent Drop from running
        let arc = core::mem::ManuallyDrop::new(arc);
        // Normal decrements only need Release ordering until we are dropping
        // the actual object
        if arc.data().ref_count.fetch_sub(1, Ordering::Release) == 1 {
            // Now that we are taking the actual object, need Acquire
            // as we take ownership of the object in order to take it
            fence(Ordering::Acquire);
            // Safety: The Box was created once on calling new, and is
            // only dropped by a single Arc reference holder
            let data = unsafe { Box::from_raw(arc.ptr.as_ptr()) };
            Some(data.data)
        } else {
            None
        }
    }
}

/// Auto deref an Arc<T> to T
/// Note - no DerefMut as this is a shared object
impl<T> Deref for Arc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data().data
    }
}

/// Clone the Arc, which must increment the reference count
impl<T> Clone for Arc<T> {
    fn clone(&self) -> Self {
        if self.data().ref_count.fetch_add(1, Ordering::Relaxed) > usize::MAX / 2 {
            panic!("Arc is out of reference counts");
        };
        Self { ptr: self.ptr }
    }
}

/// Drop the Arc, which must decrement the reference count.
/// Only if the reference count hits zero should the object
/// itself be dropped as well
impl<T> Drop for Arc<T> {
    fn drop(&mut self) {
        // Normal decrements only need Release ordering until we are dropping
        // the actual object
        if self.data().ref_count.fetch_sub(1, Ordering::Release) == 1 {
            // Now that we are dropping the actual object, need Acquire
            // as we take ownership of the object in order to drop it
            fence(Ordering::Acquire);
            drop(
                // Safety: The Box was created once on calling new, and is
                // only dropped here.
                unsafe { Box::from_raw(self.ptr.as_ptr()) },
            )
        }
    }
}

#[cfg(all(test, not(target_os = "none"), feature = "test-collections"))]
mod tests {
    use super::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::spawn;

    #[test]
    fn check_arc_drops() {
        static NUM_DROPS: AtomicUsize = AtomicUsize::new(0);

        struct DetectDrop;

        impl Drop for DetectDrop {
            fn drop(&mut self) {
                NUM_DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Create two Arcs sharing an object containing a string
        // and a DetectDrop, to detect when it's dropped.
        let x = Arc::new(("hello", DetectDrop));
        let y = x.clone();

        // Send x to another thread, and use it there.
        let t = spawn(move || {
            assert_eq!(x.0, "hello");
        });

        // In parallel, y should still be usable here.
        assert_eq!(y.0, "hello");

        // Wait for the thread to finish.
        t.join().unwrap();

        // One Arc, x, should be dropped by now.
        // We still have y, so the object shouldn't have been dropped yet.
        assert_eq!(NUM_DROPS.load(Ordering::Relaxed), 0);

        // Drop the remaining `Arc`.
        drop(y);

        // Now that `y` is dropped too,
        // the object should've been dropped.
        assert_eq!(NUM_DROPS.load(Ordering::Relaxed), 1);
    }

    /// Counts drops of the value inside an Arc. Each test owns its counter
    /// so the tests can run in parallel.
    struct CountDrop<'a>(&'a AtomicUsize);

    impl Drop for CountDrop<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn deref_reaches_the_value() {
        let a = Arc::new(41u32);
        assert_eq!(*a, 41);
        let b = a.clone();
        assert_eq!(*a + *b, 82);
        // Both point at the same allocation
        assert!(core::ptr::eq(&*a, &*b));
    }

    #[test]
    fn value_is_dropped_once_with_the_last_arc() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let a = Arc::new(CountDrop(&DROPS));
        let clones: Vec<_> = (0..10).map(|_| a.clone()).collect();
        drop(a);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        for c in clones {
            assert_eq!(DROPS.load(Ordering::Relaxed), 0);
            drop(c);
        }
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn ref_count_follows_clone_and_drop() {
        let a = Arc::new(());
        assert_eq!(a.data().ref_count.load(Ordering::Relaxed), 1);
        let b = a.clone();
        let c = b.clone();
        assert_eq!(a.data().ref_count.load(Ordering::Relaxed), 3);
        drop(b);
        assert_eq!(a.data().ref_count.load(Ordering::Relaxed), 2);
        drop(c);
        assert_eq!(a.data().ref_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn last_drop_on_another_thread_drops_the_value_there() {
        // The creating thread lets go first, so the value is destroyed by
        // the spawned thread: the reason Arc<T> needs T: Send.
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let a = Arc::new(CountDrop(&DROPS));
        let b = a.clone();
        drop(a);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        spawn(move || drop(b)).join().unwrap();
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrent_clone_and_drop_keeps_the_count() {
        // Several threads clone and drop the same Arc at once. A lost
        // update on the count shows up as a wrong final count, an early
        // free or a leak, and Miri reports each of those.
        const THREADS: usize = 4;
        const ROUNDS: usize = 50;
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let a = Arc::new(CountDrop(&DROPS));
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let mine = a.clone();
                spawn(move || {
                    for _ in 0..ROUNDS {
                        let extra = mine.clone();
                        let more = extra.clone();
                        drop(extra);
                        drop(more);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(a.data().ref_count.load(Ordering::Relaxed), 1);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        drop(a);
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn threads_share_the_value_through_a_reference() {
        // &Arc<T> crosses threads (Arc: Sync), and each thread clones
        // from the shared reference and uses the value.
        let total = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let mine = total.clone();
                    for _ in 0..25 {
                        mine.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(total.load(Ordering::Relaxed), 100);
        assert_eq!(total.data().ref_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn value_with_its_own_heap_allocation_is_freed() {
        // The Box workaround from the doc comment on new: Miri reports a
        // leak if the inner allocation is not dropped with the Arc.
        let a = Arc::new(Box::new([7u8; 256]));
        let b = a.clone();
        assert_eq!(b[255], 7);
        drop(a);
        assert_eq!(b[0], 7);
    }

    #[test]
    fn clone_panics_past_half_of_usize_max() {
        // The count cannot be reached by cloning, so set it directly.
        // The clone that finds the count at exactly half is still
        // allowed; every clone after it panics.
        let a = Arc::new(());
        a.data().ref_count.store(usize::MAX / 2, Ordering::Relaxed);
        let b = a.clone();
        for _ in 0..3 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.clone()));
            assert!(result.is_err(), "clone past the limit must panic");
        }
        // Put the true count back (a and b) so both drop cleanly.
        a.data().ref_count.store(2, Ordering::Relaxed);
        drop(b);
        drop(a);
    }

    #[test]
    fn arc_is_send_and_sync_for_send_sync_values() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<u32>>();
        assert_send_sync::<Arc<AtomicUsize>>();
    }

    #[test]
    fn get_mut_on_a_sole_arc_gives_the_value() {
        let mut a = Arc::new(1u32);
        *Arc::get_mut(&mut a).expect("only one holder") += 41;
        assert_eq!(*a, 42);
        // The count is untouched by get_mut
        assert_eq!(a.data().ref_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn get_mut_is_refused_while_shared_and_returns_after() {
        let mut a = Arc::new(5u32);
        let mut b = a.clone();
        // Neither holder is alone
        assert!(Arc::get_mut(&mut a).is_none());
        assert!(Arc::get_mut(&mut b).is_none());
        assert_eq!(*a, 5);
        drop(b);
        // Alone again
        *Arc::get_mut(&mut a).expect("clone was dropped") = 6;
        assert_eq!(*a, 6);
    }

    #[test]
    fn get_mut_after_another_thread_drops_its_clone() {
        // The other thread reads through its clone and drops it. Once it
        // has, this thread is the sole holder and may write. Miri checks
        // that the write does not race with the other thread's read.
        let mut a = Arc::new(Box::new(10u32));
        let b = a.clone();
        spawn(move || {
            assert_eq!(**b, 10);
        })
        .join()
        .unwrap();
        **Arc::get_mut(&mut a).expect("other thread has finished") = 11;
        assert_eq!(**a, 11);
    }

    #[test]
    fn get_mut_races_with_a_dropping_thread_safely() {
        // No join before get_mut: the answer may be Some or None
        // depending on timing, and both must be sound.
        for _ in 0..20 {
            let mut a = Arc::new(0u32);
            let b = a.clone();
            let t = spawn(move || {
                assert!(*b <= 1);
            });
            if let Some(value) = Arc::get_mut(&mut a) {
                *value = 1;
            }
            t.join().unwrap();
            assert!(Arc::get_mut(&mut a).is_some());
        }
    }

    #[test]
    fn value_changed_through_get_mut_is_still_dropped_once() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let mut a = Arc::new((0u32, CountDrop(&DROPS)));
        Arc::get_mut(&mut a).expect("only one holder").0 = 9;
        let b = a.clone();
        assert_eq!(b.0, 9);
        drop(a);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        drop(b);
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn into_inner_on_a_sole_arc_returns_the_value_undropped() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let a = Arc::new((7u32, CountDrop(&DROPS)));
        let value = Arc::into_inner(a).expect("only one holder");
        // The value moved out; freeing the allocation did not drop it
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        assert_eq!(value.0, 7);
        drop(value);
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn into_inner_while_shared_gives_none_and_leaves_the_value() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let a = Arc::new((3u32, CountDrop(&DROPS)));
        let b = a.clone();
        // Not the last holder: the reference is given up, nothing else
        assert!(Arc::into_inner(a).is_none());
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        assert_eq!(b.0, 3);
        assert_eq!(b.data().ref_count.load(Ordering::Relaxed), 1);
        // The remaining holder is now the last and gets the value
        let value = Arc::into_inner(b).expect("last holder");
        assert_eq!(value.0, 3);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        drop(value);
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn into_inner_moves_out_a_value_that_owns_heap_memory() {
        // No Default, no Copy: the String's buffer must arrive intact and
        // be freed exactly once (Miri reports a double free or a leak).
        let a = Arc::new(String::from("hello"));
        let mut text = Arc::into_inner(a).expect("only one holder");
        text.push_str(", world");
        assert_eq!(text, "hello, world");
    }

    #[test]
    fn into_inner_hands_the_value_to_exactly_one_of_two_racing_threads() {
        // The last two holders give up at once. A load-then-act version
        // can answer None to both and destroy the value unseen; with
        // fetch_sub exactly one of them is last.
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        const ROUNDS: usize = 20;
        for round in 0..ROUNDS {
            let a = Arc::new(CountDrop(&DROPS));
            let b = a.clone();
            let t1 = spawn(move || Arc::into_inner(a).is_some());
            let t2 = spawn(move || Arc::into_inner(b).is_some());
            let got1 = t1.join().unwrap();
            let got2 = t2.join().unwrap();
            assert!(got1 ^ got2, "exactly one thread should get the value");
            assert_eq!(DROPS.load(Ordering::Relaxed), round + 1);
        }
    }

    #[test]
    fn into_inner_after_another_thread_used_the_value() {
        // The other thread writes through the shared value and drops its
        // clone; the value taken out here must show that write.
        let a = Arc::new(AtomicUsize::new(0));
        let b = a.clone();
        spawn(move || {
            b.store(99, Ordering::Relaxed);
        })
        .join()
        .unwrap();
        let value = Arc::into_inner(a).expect("other thread has finished");
        assert_eq!(value.into_inner(), 99);
    }
}
