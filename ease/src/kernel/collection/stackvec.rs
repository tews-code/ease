//! Vector on the stack

use core::mem::MaybeUninit;
use core::ops::{Index, IndexMut};

/// Collection of N elements
///
/// The elements must be Copy.
/// All elements are held on the stack.
#[derive(Clone, Copy, Debug)]
pub struct StackVec<T: Copy, const N: usize> {
    len: usize,
    buf: [MaybeUninit<T>; N],
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
    #[allow(dead_code)]
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

    /// Length of data in the collection
    pub fn len(&self) -> usize {
        self.len
    }

    /// Collection is full
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    /// Collection is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Clear the collection
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns a slice of the collection
    pub fn as_slice(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.buf.as_ptr() as *const T, self.len) }
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

impl<T: Copy, const N: usize> Index<usize> for StackVec<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        assert!(index < self.len, "index out of bounds");
        unsafe { self.buf[index].assume_init_ref() }
    }
}

impl<T: Copy, const N: usize> IndexMut<usize> for StackVec<T, N> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        assert!(index < self.len, "index out of bounds");
        unsafe { self.buf[index].assume_init_mut() }
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
    // `assume_init_*` call inside push/pop/insert/remove/index/as_slice
    // must access a slot that was previously written. Each test below
    // exercises one or more of those unsafe paths and asserts the
    // expected values; Miri additionally verifies that no read sees
    // uninit memory.
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
        // Index calls assume_init_ref on each slot.
        assert_eq!(v[0], 10);
        assert_eq!(v[1], 20);
        assert_eq!(v[2], 30);
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
}
