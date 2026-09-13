//! Arena
//!
//! An arena is a single array of fixed size where each
//! slot optionally holds the same `T`. It provides handles to slots
//! and protects the handle from stale matches using generational
//! counters.
//!
//! In order to support `static` arenas, the initialisation is const.

use core::{fmt, marker::PhantomData};

/// A handle is a simple struct holding the slot index and generation.
/// It is only created by using [Arena::add]
pub(crate) struct Handle<T> {
    index: usize,
    generation: u32,
    _marker: PhantomData<T>,
}

/// Implement rather than derive as we do not want to apply to T itself
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Handle<T> {}
impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.index == other.index
    }
}
impl<T> Eq for Handle<T> {}
impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Each slot holds an (optional) `T` and a counter. u32 only wraps
/// after 2^32 uses of the slot so stale handles will not match.
/// Internal fields are private: all access via Arena methods.
struct Slot<T> {
    entry: Option<T>,
    generation: u32,
}

impl<T> Slot<T> {
    /// Returns a reference to the entry contents if the handle is valid
    fn entry(&self, handle: Handle<T>) -> Option<&T> {
        if self.generation != handle.generation {
            return None;
        }
        self.entry.as_ref()
    }
    /// Returns a mutable reference to the entry contents if the handle is valid
    fn entry_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        if self.generation != handle.generation {
            return None;
        }
        self.entry.as_mut()
    }
}

/// The arena struct with an array of size `N` each holding an Entry
pub(crate) struct Arena<T, const N: usize> {
    array: [Slot<T>; N],
}

impl<T, const N: usize> Arena<T, N> {
    /// New under const to support statics
    pub(crate) const fn new() -> Self {
        Self {
            array: [const {
                Slot {
                    entry: None,
                    generation: 0,
                }
            }; N],
        }
    }
    /// Add a new entry.
    /// Returns `Some(Handle)` on success or `None` if no free slots.
    pub(crate) fn add(&mut self, val: T) -> Option<Handle<T>> {
        for (index, slot) in self.array.iter_mut().enumerate() {
            if slot.entry.is_none() {
                slot.entry = Some(val);
                return Some(Handle {
                    index,
                    generation: slot.generation,
                    _marker: PhantomData,
                });
            }
        }
        None
    }
    /// Get a reference to a `T` from a `Handle`.
    /// Returns `None` if the handle is stale or invalid
    pub(crate) fn get(&self, handle: Handle<T>) -> Option<&T> {
        let slot = self.array.get(handle.index)?;
        slot.entry(handle)
    }
    /// Get a mutable reference to a `T` from a `Handle`.
    /// Returns `None` if the handle is stale or invalid
    pub(crate) fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        let slot = self.array.get_mut(handle.index)?;
        slot.entry_mut(handle)
    }
    /// Get two disjoint mutable references to `T` from two `Handle`s.
    /// Returns `None` if either handle is stale or invalid or they
    /// are handles to the same slot
    pub(crate) fn get_disjoint_mut(
        &mut self,
        handle_a: Handle<T>,
        handle_b: Handle<T>,
    ) -> Option<(&mut T, &mut T)> {
        // array.get_disjoint_mut will check indices are in bound and not equal
        if let Ok([slot_a, slot_b]) = self
            .array
            .get_disjoint_mut([handle_a.index, handle_b.index])
        {
            let entry_a = slot_a.entry_mut(handle_a)?;
            let entry_b = slot_b.entry_mut(handle_b)?;
            return Some((entry_a, entry_b));
        }
        None
    }
    /// Take a slot. This returns the slot's contents by value or `None` if already empty
    /// or if the handle is stale or invalid
    pub(crate) fn take(&mut self, handle: Handle<T>) -> Option<T> {
        let slot = self.array.get_mut(handle.index)?;
        if slot.generation != handle.generation {
            return None;
        }
        let entry = slot.entry.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        Some(entry)
    }
    /// Provides an iterator over the values present
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.array.iter().filter_map(|slot| slot.entry.as_ref())
    }
}

// Host tests: the arena is plain safe Rust with no target-specific code,
// so everything about handles, generations and slot reuse can be
// checked natively. `T` is not required to be `Copy`, so one test uses
// a `Drop`-counting type to confirm `take` moves the value out rather
// than dropping it in place.
//
// Gated on `not(target_os = "none")` so the kernel build (and the
// existing QEMU `#[test_case]` framework) is unaffected.
#[cfg(all(test, not(target_os = "none"), feature = "test-collections"))]
mod host_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn arena_add_then_get() {
        let mut a: Arena<u32, 4> = Arena::new();
        let h = a.add(7).expect("empty arena must accept an entry");
        assert_eq!(a.get(h), Some(&7));
        assert_eq!(a.get_mut(h), Some(&mut 7));
    }

    #[test]
    fn arena_full_at_exactly_n() {
        let mut a: Arena<u32, 3> = Arena::new();
        let hs: [Handle<u32>; 3] = core::array::from_fn(|i| a.add(i as u32).unwrap());
        assert!(a.add(99).is_none(), "fourth add into N=3 must be refused");
        for (i, h) in hs.iter().enumerate() {
            assert_eq!(a.get(*h), Some(&(i as u32)));
        }
    }

    #[test]
    fn arena_handles_are_distinct() {
        let mut a: Arena<u32, 4> = Arena::new();
        let h0 = a.add(0).unwrap();
        let h1 = a.add(1).unwrap();
        assert_ne!(h0, h1);
        assert_eq!(a.get(h0), Some(&0));
        assert_eq!(a.get(h1), Some(&1));
    }

    #[test]
    fn arena_take_returns_value_and_empties_slot() {
        let mut a: Arena<u32, 2> = Arena::new();
        let h = a.add(42).unwrap();
        assert_eq!(a.take(h), Some(42));
        assert_eq!(a.get(h), None, "handle must be stale after take");
        assert_eq!(a.take(h), None, "second take must find nothing");
    }

    #[test]
    fn arena_stale_handle_after_reuse() {
        // The core generational guarantee: a slot freed and refilled
        // must reject the old handle and accept the new one.
        let mut a: Arena<u32, 1> = Arena::new();
        let old = a.add(1).unwrap();
        assert_eq!(a.take(old), Some(1));
        let new = a.add(2).unwrap();
        assert_ne!(old, new, "reuse must hand out a different generation");
        assert_eq!(a.get(old), None);
        assert_eq!(a.get_mut(old), None);
        assert_eq!(
            a.take(old),
            None,
            "stale take must not disturb the live entry"
        );
        assert_eq!(a.get(new), Some(&2));
    }

    #[test]
    fn arena_stale_take_does_not_bump_generation() {
        // If a stale take bumped the generation, it could invalidate the
        // live handle that currently owns the slot.
        let mut a: Arena<u32, 1> = Arena::new();
        let old = a.add(1).unwrap();
        a.take(old);
        let new = a.add(2).unwrap();
        for _ in 0..3 {
            assert_eq!(a.take(old), None);
        }
        assert_eq!(a.get(new), Some(&2), "live handle must survive stale takes");
    }

    #[test]
    fn arena_add_reuses_lowest_free_slot() {
        let mut a: Arena<u32, 3> = Arena::new();
        let h0 = a.add(0).unwrap();
        let h1 = a.add(1).unwrap();
        let _h2 = a.add(2).unwrap();
        a.take(h0);
        let h3 = a.add(3).unwrap();
        // Same index as h0, but a fresh generation.
        assert_eq!(h3.index, h0.index);
        assert_ne!(h3, h0);
        assert_eq!(a.get(h1), Some(&1), "neighbouring slot untouched");
    }

    #[test]
    fn arena_foreign_handle_out_of_range() {
        // A handle minted by a larger arena must be rejected, not panic.
        let mut big: Arena<u32, 4> = Arena::new();
        let mut small: Arena<u32, 2> = Arena::new();
        for i in 0..3 {
            big.add(i).unwrap();
        }
        let h3 = big.add(3).unwrap();
        assert_eq!(small.get(h3), None);
        assert_eq!(small.get_mut(h3), None);
        assert_eq!(small.take(h3), None);
        let h0 = small.add(0).unwrap();
        assert!(small.get_disjoint_mut(h0, h3).is_none());
    }

    #[test]
    fn arena_get_mut_writes_through() {
        let mut a: Arena<u32, 2> = Arena::new();
        let h = a.add(1).unwrap();
        *a.get_mut(h).unwrap() += 10;
        assert_eq!(a.get(h), Some(&11));
    }

    #[test]
    fn arena_disjoint_two_live_entries() {
        let mut a: Arena<u32, 4> = Arena::new();
        let ha = a.add(10).unwrap();
        let hb = a.add(20).unwrap();
        let (ea, eb) = a.get_disjoint_mut(ha, hb).expect("two live handles");
        core::mem::swap(ea, eb);
        assert_eq!(a.get(ha), Some(&20));
        assert_eq!(a.get(hb), Some(&10));
    }

    #[test]
    fn arena_disjoint_refuses_same_slot() {
        let mut a: Arena<u32, 4> = Arena::new();
        let h = a.add(1).unwrap();
        assert!(a.get_disjoint_mut(h, h).is_none());
        // Same index, different generation: still overlapping.
        a.take(h);
        let h2 = a.add(2).unwrap();
        assert_eq!(h.index, h2.index);
        assert!(a.get_disjoint_mut(h, h2).is_none());
        assert!(a.get_disjoint_mut(h2, h).is_none());
    }

    #[test]
    fn arena_disjoint_refuses_stale_or_empty() {
        let mut a: Arena<u32, 4> = Arena::new();
        let ha = a.add(1).unwrap();
        let hb = a.add(2).unwrap();
        a.take(hb);
        assert!(a.get_disjoint_mut(ha, hb).is_none(), "stale second handle");
        assert!(a.get_disjoint_mut(hb, ha).is_none(), "stale first handle");
        let hc = a.add(3).unwrap();
        assert_ne!(hb, hc);
        assert!(
            a.get_disjoint_mut(ha, hb).is_none(),
            "old handle to reused slot"
        );
        assert!(
            a.get_disjoint_mut(ha, hc).is_some(),
            "new handle to reused slot"
        );
    }

    #[test]
    fn arena_generation_wraps_without_panic() {
        // Force the counter to the top and confirm the bump wraps
        // rather than overflowing.
        let mut a: Arena<u32, 1> = Arena::new();
        a.array[0].generation = u32::MAX;
        let h = a.add(1).unwrap();
        assert_eq!(h.generation, u32::MAX);
        assert_eq!(a.take(h), Some(1));
        assert_eq!(a.array[0].generation, 0);
        assert_eq!(a.get(h), None);
    }

    // `T` with a Drop impl: `take` must move the value out (no drop
    // inside the arena), and dropping the arena must drop what is
    // still held.
    struct Counted<'a>(&'a Cell<u32>);
    impl Drop for Counted<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn arena_take_moves_out_without_dropping() {
        let drops = Cell::new(0);
        let mut a: Arena<Counted<'_>, 2> = Arena::new();
        let h = a.add(Counted(&drops)).unwrap();
        let v = a.take(h).expect("live handle");
        assert_eq!(drops.get(), 0, "take must not drop inside the arena");
        drop(v);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn arena_drop_releases_held_entries() {
        let drops = Cell::new(0);
        {
            let mut a: Arena<Counted<'_>, 3> = Arena::new();
            a.add(Counted(&drops)).unwrap();
            let h = a.add(Counted(&drops)).unwrap();
            a.add(Counted(&drops)).unwrap();
            drop(a.take(h));
            assert_eq!(drops.get(), 1);
        }
        assert_eq!(drops.get(), 3, "remaining entries dropped with the arena");
    }

    #[test]
    fn arena_const_new_in_static_context() {
        // `new` must be usable in a const initialiser, which is how the
        // kernel will declare its arenas.
        static A: Arena<u32, 4> = Arena::new();
        assert!(
            A.get(Handle {
                index: 0,
                generation: 0,
                _marker: PhantomData
            })
            .is_none()
        );
    }
}

// QEMU tests: gated on target_os = "none" so the kernel-only `#[test_case]`
// custom test framework does not collide with libtest when this file is
// compiled as part of the lib crate's host tests.
#[cfg(all(test, target_os = "none", feature = "test-collections"))]
mod tests {
    use super::*;

    #[test_case]
    fn test_arena_add_get_take() {
        let mut a: Arena<u32, 4> = Arena::new();
        let h = a.add(7).unwrap();
        assert_eq!(a.get(h), Some(&7));
        assert_eq!(a.take(h), Some(7));
        assert_eq!(a.get(h), None);
    }

    #[test_case]
    fn test_arena_stale_after_reuse() {
        let mut a: Arena<u32, 1> = Arena::new();
        let old = a.add(1).unwrap();
        a.take(old);
        let new = a.add(2).unwrap();
        assert_eq!(a.get(old), None);
        assert_eq!(a.get(new), Some(&2));
    }

    #[test_case]
    fn test_arena_full_and_disjoint() {
        let mut a: Arena<u32, 2> = Arena::new();
        let ha = a.add(1).unwrap();
        let hb = a.add(2).unwrap();
        assert!(a.add(3).is_none());
        assert!(a.get_disjoint_mut(ha, ha).is_none());
        let (ea, eb) = a.get_disjoint_mut(ha, hb).unwrap();
        core::mem::swap(ea, eb);
        assert_eq!(a.get(ha), Some(&2));
    }
}
