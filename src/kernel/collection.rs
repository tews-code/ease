//! Heapless Collections

#![allow(dead_code)]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::ops::{Index, IndexMut};
use core::sync::atomic::{AtomicUsize, Ordering};

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

/// Ring buffer using the stack
///
/// T is Copy
/// Note that this always allocates one more T than is used
pub struct RingBuf<T: Copy, const N: usize> {
    buf: [MaybeUninit<T>; N],
    head: usize,
    tail: usize,
}

impl<T: Copy, const N: usize> RingBuf<T, N> {
    /// Create a new ring buffer
    pub const fn new() -> Self {
        Self {
            buf: [MaybeUninit::uninit(); N],
            head: 0,
            tail: 0,
        }
    }
    /// Push a new value into the ring at the tail
    ///
    /// Overwites the oldest if full
    pub fn push(&mut self, value: T) {
        self.buf[self.head].write(value);
        self.head = (self.head + 1) % N;
        if self.head == self.tail {
            self.tail = (self.tail + 1) % N
        };
    }
    /// Length of data in the ring buffer
    pub fn len(&self) -> usize {
        if self.head >= self.tail {
            self.head - self.tail
        } else {
            N - self.tail + self.head
        }
    }
    /// Buffer is empty
    ///
    /// Note - stack resource is only released when buffer is dropped
    pub fn is_empty(&self) -> bool {
        self.tail == self.head
    }
    /// Buffer is full
    ///
    /// This does not prevent adding values (which will overwrite the oldest)
    pub fn is_full(&self) -> bool {
        (self.head + 1) % N == self.tail
    }
    /// Get a value by index
    ///
    /// Indexing is by value's age. 0 = oldest
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len() {
            None
        } else {
            Some(unsafe {
                // Safety: Only returning values that have been inserted
                self.buf[(self.tail + index) % N].assume_init_ref()
            })
        }
    }
    /// Newest value by index
    ///
    /// 0 = most recent
    pub fn newest(&self, n: usize) -> Option<&T> {
        if n >= self.len() {
            None
        } else {
            Some(unsafe { self.buf[(self.head + N - 1 - n) % N].assume_init_ref() })
        }
    }

    /// Provides an iterator over the values present, from oldest to newest
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        RingBufIter {
            ring: self,
            pos: self.tail,
            remaining: self.len(),
        }
    }
}

pub struct RingBufIter<'a, T: Copy, const N: usize> {
    ring: &'a RingBuf<T, N>,
    pos: usize,
    remaining: usize,
}

impl<'a, T: Copy, const N: usize> Iterator for RingBufIter<'a, T, N> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            None
        } else {
            let val = unsafe { self.ring.buf[self.pos].assume_init_ref() };
            self.pos = (self.pos + 1) % N;
            self.remaining -= 1;
            Some(val)
        }
    }
}

/// Lock-free single-producer single-consumer ring buffer
pub struct SpscRingBuf<T: Copy, const N: usize> {
    buf: [UnsafeCell<MaybeUninit<T>>; N],
    head: AtomicUsize,
    tail: AtomicUsize,
}

// Safety: Single-producer (interrupt handler) and single-consumer (main thread)
// never access the same slot — the head/tail indices guarantee separation.
unsafe impl<T: Copy, const N: usize> Sync for SpscRingBuf<T, N> {}

impl<T: Copy, const N: usize> SpscRingBuf<T, N> {
    pub const fn new() -> Self {
        Self {
            buf: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Push a T into the ring buffer. If this fails, return the T
    pub fn push(&self, val: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        let pos = (head + 1) % N;
        if pos == tail {
            // Buffer is full
            Err(val)
        } else {
            // Safety: Head and tail never point to the same cell at the same time
            unsafe { (*self.buf[head].get()).write(val) };
            self.head.store(pos, Ordering::Release);
            Ok(())
        }
    }

    /// Pop a T from the ring buffer
    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            // Empty
            None
        } else {
            let val = unsafe { (*self.buf[tail].get()).assume_init_read() };
            self.tail.store((tail + 1) % N, Ordering::Release);
            Some(val)
        }
    }

    /// Ring buffer is full
    pub fn is_full(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        let pos = (head + 1) % N;
        pos == tail
    }

    /// Ring buffer is empty
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head == tail
    }
}

#[cfg(all(test, feature = "test-collections"))]
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

    // ── RingBuf tests ──

    #[test_case]
    fn test_ring_push_and_get() {
        let mut r: RingBuf<u32, 8> = RingBuf::new();
        r.push(10);
        r.push(20);
        r.push(30);
        assert_eq!(r.len(), 3);
        assert_eq!(r.get(0), Some(&10)); // oldest
        assert_eq!(r.get(1), Some(&20));
        assert_eq!(r.get(2), Some(&30)); // newest
    }

    #[test_case]
    fn test_ring_get_out_of_bounds() {
        let mut r: RingBuf<u32, 8> = RingBuf::new();
        r.push(1);
        assert_eq!(r.get(1), None);
        assert_eq!(r.get(100), None);
    }

    #[test_case]
    fn test_ring_empty() {
        let r: RingBuf<u32, 4> = RingBuf::new();
        assert!(r.is_empty());
        assert!(!r.is_full());
        assert_eq!(r.len(), 0);
        assert_eq!(r.get(0), None);
        assert_eq!(r.newest(0), None);
    }

    #[test_case]
    fn test_ring_full() {
        // N=4 means max 3 elements (one slot reserved)
        let mut r: RingBuf<u32, 4> = RingBuf::new();
        r.push(1);
        r.push(2);
        r.push(3);
        assert!(r.is_full());
        assert_eq!(r.len(), 3);
    }

    #[test_case]
    fn test_ring_newest() {
        let mut r: RingBuf<u32, 8> = RingBuf::new();
        r.push(10);
        r.push(20);
        r.push(30);
        assert_eq!(r.newest(0), Some(&30)); // most recent
        assert_eq!(r.newest(1), Some(&20));
        assert_eq!(r.newest(2), Some(&10)); // oldest
        assert_eq!(r.newest(3), None);
    }

    #[test_case]
    fn test_ring_wraparound() {
        // N=4, capacity=3. Push 5 values so it wraps and overwrites.
        let mut r: RingBuf<u32, 4> = RingBuf::new();
        r.push(1);
        r.push(2);
        r.push(3); // full: [1, 2, 3]
        r.push(4); // overwrites 1: [2, 3, 4]
        r.push(5); // overwrites 2: [3, 4, 5]
        assert_eq!(r.len(), 3);
        assert_eq!(r.get(0), Some(&3)); // oldest surviving
        assert_eq!(r.get(1), Some(&4));
        assert_eq!(r.get(2), Some(&5)); // newest
        assert_eq!(r.newest(0), Some(&5));
        assert_eq!(r.newest(2), Some(&3));
    }

    #[test_case]
    fn test_ring_iter() {
        let mut r: RingBuf<u32, 8> = RingBuf::new();
        r.push(10);
        r.push(20);
        r.push(30);
        let vals: StackVec<u32, 8> = {
            let mut v = StackVec::new();
            for &x in r.iter() {
                v.push(x).unwrap();
            }
            v
        };
        assert_eq!(vals.as_slice(), &[10, 20, 30]);
    }

    #[test_case]
    fn test_ring_iter_after_wraparound() {
        let mut r: RingBuf<u32, 4> = RingBuf::new();
        r.push(1);
        r.push(2);
        r.push(3);
        r.push(4); // [2, 3, 4]
        r.push(5); // [3, 4, 5]
        let vals: StackVec<u32, 4> = {
            let mut v = StackVec::new();
            for &x in r.iter() {
                v.push(x).unwrap();
            }
            v
        };
        assert_eq!(vals.as_slice(), &[3, 4, 5]);
    }

    #[test_case]
    fn test_ring_iter_empty() {
        let r: RingBuf<u32, 4> = RingBuf::new();
        let mut count = 0;
        for _ in r.iter() {
            count += 1;
        }
        assert_eq!(count, 0);
    }

    // ── SpscRingBuf tests ──

    #[test_case]
    fn test_spsc_push_pop() {
        let rb: SpscRingBuf<u8, 4> = SpscRingBuf::new();
        assert_eq!(rb.pop(), None);
        assert!(rb.push(42).is_ok());
        assert_eq!(rb.pop(), Some(42));
        assert_eq!(rb.pop(), None);
    }

    #[test_case]
    fn test_spsc_full() {
        // N=4 means 3 usable slots
        let rb: SpscRingBuf<u8, 4> = SpscRingBuf::new();
        assert!(rb.push(1).is_ok());
        assert!(rb.push(2).is_ok());
        assert!(rb.push(3).is_ok());
        assert_eq!(rb.push(4), Err(4)); // full
    }

    #[test_case]
    fn test_spsc_fifo_order() {
        let rb: SpscRingBuf<u8, 8> = SpscRingBuf::new();
        rb.push(10).unwrap();
        rb.push(20).unwrap();
        rb.push(30).unwrap();
        assert_eq!(rb.pop(), Some(10));
        assert_eq!(rb.pop(), Some(20));
        assert_eq!(rb.pop(), Some(30));
    }

    #[test_case]
    fn test_spsc_wraparound() {
        let rb: SpscRingBuf<u8, 4> = SpscRingBuf::new();
        // Fill and drain twice to force wraparound
        rb.push(1).unwrap();
        rb.push(2).unwrap();
        rb.push(3).unwrap();
        assert_eq!(rb.pop(), Some(1));
        assert_eq!(rb.pop(), Some(2));
        assert_eq!(rb.pop(), Some(3));
        // Now head=3, tail=3 — push again wraps around index 0
        rb.push(4).unwrap();
        rb.push(5).unwrap();
        assert_eq!(rb.pop(), Some(4));
        assert_eq!(rb.pop(), Some(5));
        assert_eq!(rb.pop(), None);
    }

    #[test_case]
    fn test_spsc_interleaved() {
        let rb: SpscRingBuf<u32, 4> = SpscRingBuf::new();
        rb.push(1).unwrap();
        rb.push(2).unwrap();
        assert_eq!(rb.pop(), Some(1));
        rb.push(3).unwrap();
        rb.push(4).unwrap();
        assert_eq!(rb.pop(), Some(2));
        assert_eq!(rb.pop(), Some(3));
        assert_eq!(rb.pop(), Some(4));
        assert_eq!(rb.pop(), None);
    }

    #[test_case]
    fn test_spsc_bool() {
        let rb: SpscRingBuf<bool, 4> = SpscRingBuf::new();
        rb.push(true).unwrap();
        rb.push(false).unwrap();
        rb.push(true).unwrap();
        assert_eq!(rb.pop(), Some(true));
        assert_eq!(rb.pop(), Some(false));
        assert_eq!(rb.pop(), Some(true));
        assert_eq!(rb.pop(), None);
    }

    #[test_case]
    fn test_spsc_is_full() {
        let rb: SpscRingBuf<u8, 3> = SpscRingBuf::new();
        assert!(!rb.is_full());
        rb.push(1).unwrap();
        assert!(!rb.is_full());
        rb.push(2).unwrap();
        assert!(rb.is_full());
        rb.pop();
        assert!(!rb.is_full());
    }

    #[test_case]
    fn test_spsc_size_one() {
        // N=1 means 0 usable slots — always full
        let rb: SpscRingBuf<u8, 1> = SpscRingBuf::new();
        assert!(rb.is_full());
        assert_eq!(rb.push(1), Err(1));
        assert_eq!(rb.pop(), None);
    }

    #[test_case]
    fn test_spsc_size_two() {
        // N=2 means 1 usable slot
        let rb: SpscRingBuf<u8, 2> = SpscRingBuf::new();
        rb.push(99).unwrap();
        assert!(rb.is_full());
        assert_eq!(rb.push(100), Err(100));
        assert_eq!(rb.pop(), Some(99));
        assert!(!rb.is_full());
        rb.push(100).unwrap();
        assert_eq!(rb.pop(), Some(100));
    }

    #[test_case]
    fn test_spsc_tuple() {
        let rb: SpscRingBuf<(u8, u16), 4> = SpscRingBuf::new();
        rb.push((1, 100)).unwrap();
        rb.push((2, 200)).unwrap();
        assert_eq!(rb.pop(), Some((1, 100)));
        assert_eq!(rb.pop(), Some((2, 200)));
        assert_eq!(rb.pop(), None);
    }
}
