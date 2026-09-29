//! Vector on the stack

use core::fmt::Write;
use core::mem::MaybeUninit;

/// Collection of N elements
///
/// The elements must be Copy.
/// All elements are held on the stack.
#[derive(Clone, Copy, Debug)]
pub struct StackVec<T: Copy, const N: usize> {
    buf: [MaybeUninit<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> StackVec<T, N> {
    /// Create a new collection of maximum N elements of type T
    ///
    /// Use turbofish syntax to create
    /// `let mut line: Vec<u8, 256> = Vec::new();`
    pub const fn new() -> Self {
        Self {
            len: 0,
            buf: [MaybeUninit::uninit(); N],
        }
    }

    /// Append to end of collection
    ///
    /// Will return the element on failure
    pub fn push(&mut self, element: T) -> Result<(), T> {
        if self.len < N {
            self.buf[self.len].write(element);
            self.len += 1;
            Ok(())
        } else {
            Err(element)
        }
    }

    /// Pop the last element off the collection
    pub fn pop(&mut self) -> Option<T> {
        if self.len > 0 {
            self.len -= 1;
            Some(
                // Safety: We've just checked via len that this is valid data
                unsafe { self.buf[self.len].assume_init_read() },
            )
        } else {
            None
        }
    }

    /// Insert an element at position
    ///
    /// Shifts the remaining elements right (if any)
    /// Returns Err(element) if there is insufficient space
    /// `let mut vec: Vec<usize, 5> = Vec::new();`
    /// vec is [1, 3, 7, , ];
    /// Let's insert 5 at the 3rd element (index 2)
    /// `vec.insert(2, 5);`
    /// vec is [1, 3, 5, 7, ];
    pub fn insert(&mut self, index: usize, element: T) -> Result<(), T> {
        if index > self.len {
            return Err(element);
        };
        if self.len < N {
            // First shift elements right to make a space
            self.buf.copy_within(index..self.len, index + 1);
            self.buf[index].write(element);
            self.len += 1;
            Ok(())
        } else {
            Err(element)
        }
    }

    /// Remove element at given index
    ///
    /// Returns None if index is out of bounds, or
    /// Some(element) of the removed item
    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index >= self.len {
            return None;
        };
        let element = unsafe {
            // Safety: index < length of collection so this is initialised data
            self.buf[index].assume_init_read()
        };
        self.buf.copy_within(index + 1..self.len, index);
        self.len -= 1;
        Some(element)
    }

    /// Collection is full
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    /// Clear the collection
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns a slice of the collection
    pub fn as_slice(&self) -> &[T] {
        // Safety: data is non-null and valid for reads of len * size_of::<T>() bytes.
        // Data is aligned as created through safe Rust. There are len number of
        // initialised `T` and data is len consecutive initialised values of type `T`.
        // The total size len * size_of::<T>() is less than isize::MAX because
        // the compiler bounds the size of [MaybeUninit<T>; N] and len is never bigger
        // than N.
        // Takes &self, so no other user can mutate in parallel.
        unsafe { core::slice::from_raw_parts(self.buf.as_ptr() as *const T, self.len) }
    }

    /// Returns a mutable slice of the collection
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // Safety: data is non-null and valid for reads and writes
        // of len * size_of::<T>() bytes.
        // Data is aligned as created through safe Rust. There are len number of
        // initialised `T` and data is len consecutive initialised values of type `T`.
        // The total size len * size_of::<T>() is less than isize::MAX because
        // the compiler bounds the size of [MaybeUninit<T>; N] and len is never bigger
        // than N.
        // Takes &mut self, so only one owner ensures no other mutation.
        unsafe { core::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut T, self.len) }
    }
}

impl<const N: usize> StackVec<u8, N> {
    /// Returns a string slice of the collection
    ///
    /// Elements must bytes and valid for UTF-8.
    pub fn as_str(&self) -> Result<&str, ()> {
        core::str::from_utf8(self.as_slice()).map_err(|_| ())
    }

    /// Fills Vec<u8; N> with values from &str
    pub fn copy_from_str(&mut self, s: &str) {
        let bytes = s.as_bytes();
        self.len = bytes.len().min(N);
        for (i, &b) in bytes.iter().enumerate().take(self.len) {
            self.buf[i].write(b);
        }
    }
}
/// Deref the StackVec into a slice. This allows us to inherit all the slice
/// methods.
impl<T: Copy, const N: usize> core::ops::Deref for StackVec<T, N> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}
/// Deref the StackVec into a mut slice. This allows us to inherit all the slice
/// methods.
impl<T: Copy, const N: usize> core::ops::DerefMut for StackVec<T, N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<const N: usize> Write for StackVec<u8, N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            if self.push(byte).is_err() {
                return Err(core::fmt::Error);
            }
        }
        Ok(())
    }
}

// Host-runnable tests for the collection types. Run with
//     cargo test --lib --target $HOST_TARGET
// and verified clean under Miri:
//     cargo +nightly miri test --lib --target $HOST_TARGET
//
// The SpscRingBuf concurrency test is the headline value-add: Miri can
// simulate multi-threaded execution and check the atomic ordering on
// push()/pop() against the actual reads and writes of the underlying
// storage cells. The StackVec / RingBuf tests cover the MaybeUninit
// safety invariants — that no `assume_init_*` is ever called on a slot
// that wasn't written, and that `as_slice` only covers initialised
// elements. Both sets of types use `T: Copy`, so there are no Drop
// concerns to test.
//
// To stress-test more interleavings of the concurrent SpscRingBuf
// test, run with
//     MIRIFLAGS="-Zmiri-many-seeds=0..32" cargo +nightly miri test --lib ...
//
// Gated on `not(target_os = "none")` so the kernel build (and the
// existing QEMU `#[test_case]` framework) is unaffected.
#[cfg(all(test, not(target_os = "none"), feature = "test-collections"))]
mod host_tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────
    // StackVec
    //
    // The Miri-relevant invariant is MaybeUninit safety: every
    // `assume_init_*` call inside push/pop/insert/remove, and the
    // pointer casts in as_slice/as_mut_slice (which back Deref/DerefMut,
    // and so indexing and every slice method), must only cover slots
    // that were previously written. Each test below exercises one or
    // more of those unsafe paths and asserts the expected values; Miri
    // additionally verifies that no read sees uninit memory.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn stackvec_push_pop_lifo() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        assert!(v.is_empty());
        assert!(v.push(10).is_ok());
        assert!(v.push(20).is_ok());
        assert!(v.push(30).is_ok());
        assert_eq!(v.len(), 3);
        // pop in LIFO order
        assert_eq!(v.pop(), Some(30));
        assert_eq!(v.pop(), Some(20));
        assert_eq!(v.pop(), Some(10));
        assert_eq!(v.pop(), None);
        assert!(v.is_empty());
    }

    #[test]
    fn stackvec_push_full_returns_value() {
        let mut v: StackVec<u32, 2> = StackVec::new();
        assert!(v.push(1).is_ok());
        assert!(v.push(2).is_ok());
        assert!(v.is_full());
        // push to a full StackVec must hand the value back, not write
        // past the end of the buffer.
        assert_eq!(v.push(3), Err(3));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn stackvec_clear_then_reuse_does_not_read_stale() {
        // After clear() the length resets to 0 but the underlying slots
        // still hold the old values. This test makes sure that pushing
        // a fresh value and then popping it returns the *new* value,
        // not the stale bytes left over. Miri also verifies that a pop
        // after clear/push only reads the slot that was just written.
        let mut v: StackVec<u32, 4> = StackVec::new();
        let _ = v.push(0xAAAA_AAAA);
        let _ = v.push(0xBBBB_BBBB);
        v.clear();
        assert_eq!(v.len(), 0);
        let _ = v.push(0xDEAD);
        assert_eq!(v.pop(), Some(0xDEAD));
        assert_eq!(v.pop(), None);
    }

    #[test]
    fn stackvec_insert_shifts_right() {
        let mut v: StackVec<u32, 5> = StackVec::new();
        let _ = v.push(1);
        let _ = v.push(3);
        let _ = v.push(5);
        // insert 2 at index 1: [1, 3, 5] -> [1, 2, 3, 5]
        // exercises the copy_within shift inside insert().
        assert!(v.insert(1, 2).is_ok());
        assert_eq!(v.as_slice(), &[1, 2, 3, 5]);
        // insert 4 at index 3: [1, 2, 3, 5] -> [1, 2, 3, 4, 5]
        assert!(v.insert(3, 4).is_ok());
        assert_eq!(v.as_slice(), &[1, 2, 3, 4, 5]);
        assert!(v.is_full());
        // insert into a full StackVec must hand the value back.
        assert_eq!(v.insert(0, 99), Err(99));
        // insert past len must fail too.
        let mut small: StackVec<u32, 4> = StackVec::new();
        let _ = small.push(1);
        assert_eq!(small.insert(5, 99), Err(99));
    }

    #[test]
    fn stackvec_remove_shifts_left() {
        let mut v: StackVec<u32, 5> = StackVec::new();
        for i in [1, 2, 3, 4, 5] {
            let _ = v.push(i);
        }
        // remove the middle element: [1, 2, 3, 4, 5] -> [1, 2, 4, 5]
        // exercises both assume_init_read AND copy_within left-shift.
        assert_eq!(v.remove(2), Some(3));
        assert_eq!(v.as_slice(), &[1, 2, 4, 5]);
        // remove past len must return None, not read uninit.
        assert_eq!(v.remove(99), None);
    }

    #[test]
    fn stackvec_as_slice_only_covers_initialised() {
        // as_slice() casts the buffer pointer to *const T and creates
        // a slice of length self.len. Miri verifies that all `len`
        // elements of the resulting slice are within initialised memory.
        let mut v: StackVec<u32, 8> = StackVec::new();
        for i in 0..5 {
            let _ = v.push(i);
        }
        let s = v.as_slice();
        assert_eq!(s.len(), 5);
        // Materially read every element to make sure Miri actually
        // visits the initialised range.
        let sum: u32 = s.iter().sum();
        assert_eq!(sum, 0 + 1 + 2 + 3 + 4);
    }

    #[test]
    fn stackvec_indexing() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        let _ = v.push(10);
        let _ = v.push(20);
        let _ = v.push(30);
        // Indexing resolves through Deref to the slice's Index impl.
        assert_eq!(v[0], 10);
        assert_eq!(v[1], 20);
        assert_eq!(v[2], 30);
        // Range indexing is a slice feature Deref brings for free.
        assert_eq!(&v[1..3], &[20, 30]);
    }

    // Deref<Target = [T]>: slice methods and coercion arrive without
    // the type implementing any of them itself.
    #[test]
    fn stackvec_derefs_to_slice() {
        fn takes_slice(s: &[u32]) -> usize {
            s.len()
        }
        let mut v: StackVec<u32, 4> = StackVec::new();
        for i in [3, 1, 2] {
            let _ = v.push(i);
        }
        assert_eq!(takes_slice(&v), 3); // &StackVec coerces to &[T]
        assert_eq!(v.iter().sum::<u32>(), 6);
        assert!(v.contains(&1));
        assert_eq!(v.first(), Some(&3));
        assert_eq!(v.last(), Some(&2));
        // NB: `for x in &v` needs an explicit `IntoIterator for &StackVec`;
        // Deref only supplies methods and coercions, not trait impls.
        let mut seen = 0;
        for &x in v.iter() {
            seen += x;
        }
        assert_eq!(seen, 6);
    }

    // DerefMut: writes through the slice land in the buffer, so a
    // later pop() observes them. Miri checks the &mut slice only spans
    // initialised slots.
    #[test]
    fn stackvec_deref_mut_writes_through() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        for i in [1, 2, 3] {
            let _ = v.push(i);
        }
        v[1] = 20;
        for x in v.iter_mut() {
            *x += 100;
        }
        v.as_mut_slice().reverse();
        assert_eq!(v.as_slice(), &[103, 120, 101]);
        assert_eq!(v.pop(), Some(101));
        assert_eq!(v.len(), 2);
    }

    // Both slice views of an empty StackVec are empty: from_raw_parts
    // with len 0 over an all-uninit buffer must not be a read of any
    // slot.
    #[test]
    fn stackvec_empty_derefs_to_empty_slices() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        assert!(v.as_slice().is_empty());
        assert!(v.as_mut_slice().is_empty());
        assert_eq!(v.iter().count(), 0);
        for _ in v.iter_mut() {
            panic!("no elements to visit");
        }
    }
}

// QEMU tests: gated on target_os = "none" so the kernel-only `#[test_case]`
// custom test framework does not collide with libtest when this file is
// compiled as part of the lib crate's host tests.
#[cfg(all(test, target_os = "none", feature = "test-collections"))]
mod tests {
    use super::*;

    #[test_case]
    fn test_push_and_index() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        assert!(v.push(10).is_ok());
        assert!(v.push(20).is_ok());
        assert!(v.push(30).is_ok());
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], 10);
        assert_eq!(v[1], 20);
        assert_eq!(v[2], 30);
    }

    #[test_case]
    fn test_push_full() {
        let mut v: StackVec<u8, 2> = StackVec::new();
        assert!(v.push(1).is_ok());
        assert!(v.push(2).is_ok());
        // Full — returns the element back
        assert_eq!(v.push(3).unwrap_err(), 3);
        assert_eq!(v.len(), 2);
        assert!(v.is_full());
    }

    #[test_case]
    fn test_pop() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        assert_eq!(v.pop(), Some(2));
        assert_eq!(v.pop(), Some(1));
        assert_eq!(v.pop(), None);
    }

    #[test_case]
    fn test_pop_empty() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        assert_eq!(v.pop(), None);
    }

    #[test_case]
    fn test_insert_at_beginning() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(2).unwrap();
        v.push(3).unwrap();
        assert!(v.insert(0, 1).is_ok());
        assert_eq!(v[0], 1);
        assert_eq!(v[1], 2);
        assert_eq!(v[2], 3);
        assert_eq!(v.len(), 3);
    }

    #[test_case]
    fn test_insert_at_middle() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(1).unwrap();
        v.push(3).unwrap();
        assert!(v.insert(1, 2).is_ok());
        assert_eq!(v[0], 1);
        assert_eq!(v[1], 2);
        assert_eq!(v[2], 3);
    }

    #[test_case]
    fn test_insert_at_end() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        // Insert at index == len is equivalent to push
        assert!(v.insert(2, 3).is_ok());
        assert_eq!(v[2], 3);
        assert_eq!(v.len(), 3);
    }

    #[test_case]
    fn test_insert_beyond_len() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(1).unwrap();
        // index 5 is way past len()==1
        assert_eq!(v.insert(5, 99).unwrap_err(), 99);
        assert_eq!(v.len(), 1);
    }

    #[test_case]
    fn test_insert_when_full() {
        let mut v: StackVec<u32, 2> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        assert_eq!(v.insert(0, 99).unwrap_err(), 99);
        assert_eq!(v.len(), 2);
    }

    #[test_case]
    fn test_remove_beginning() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        v.push(3).unwrap();
        assert_eq!(v.remove(0), Some(1));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], 2);
        assert_eq!(v[1], 3);
    }

    #[test_case]
    fn test_remove_middle() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        v.push(3).unwrap();
        assert_eq!(v.remove(1), Some(2));
        assert_eq!(v[0], 1);
        assert_eq!(v[1], 3);
    }

    #[test_case]
    fn test_remove_end() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        v.push(3).unwrap();
        assert_eq!(v.remove(2), Some(3));
        assert_eq!(v.len(), 2);
    }

    #[test_case]
    fn test_remove_out_of_bounds() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        v.push(1).unwrap();
        assert_eq!(v.remove(5), None);
        assert_eq!(v.remove(1), None);
        assert_eq!(v.len(), 1);
    }

    #[test_case]
    fn test_clear() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        v.clear();
        assert_eq!(v.len(), 0);
        assert!(v.is_empty());
    }

    #[test_case]
    fn test_is_empty_and_is_full() {
        let mut v: StackVec<u8, 2> = StackVec::new();
        assert!(v.is_empty());
        assert!(!v.is_full());
        v.push(1).unwrap();
        assert!(!v.is_empty());
        assert!(!v.is_full());
        v.push(2).unwrap();
        assert!(!v.is_empty());
        assert!(v.is_full());
    }

    #[test_case]
    fn test_as_slice() {
        let mut v: StackVec<u32, 8> = StackVec::new();
        v.push(10).unwrap();
        v.push(20).unwrap();
        v.push(30).unwrap();
        assert_eq!(v.as_slice(), &[10, 20, 30]);
    }

    #[test_case]
    fn test_as_str() {
        let mut v: StackVec<u8, 16> = StackVec::new();
        for &b in b"hello" {
            v.push(b).unwrap();
        }
        assert_eq!(v.as_str(), Ok("hello"));
    }

    #[test_case]
    fn test_copy_from_str() {
        let mut v: StackVec<u8, 16> = StackVec::new();
        v.copy_from_str("ease");
        assert_eq!(v.as_str(), Ok("ease"));
        assert_eq!(v.len(), 4);
    }

    #[test_case]
    fn test_copy_from_str_truncates() {
        let mut v: StackVec<u8, 4> = StackVec::new();
        v.copy_from_str("toolong");
        assert_eq!(v.len(), 4);
        assert_eq!(v.as_str(), Ok("tool"));
    }

    #[test_case]
    fn test_index_mut() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        v[1] = 99;
        assert_eq!(v[1], 99);
    }

    #[test_case]
    fn test_deref_slice_methods() {
        let mut v: StackVec<u32, 4> = StackVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        v.push(3).unwrap();
        assert!(v.contains(&2));
        assert_eq!(v.iter().sum::<u32>(), 6);
        assert_eq!(&v[1..], &[2, 3]);
        for x in v.iter_mut() {
            *x *= 2;
        }
        assert_eq!(v.as_slice(), &[2, 4, 6]);
    }
}
