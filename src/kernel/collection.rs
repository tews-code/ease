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
#[cfg(all(test, not(target_os = "none")))]
mod host_tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::vec::Vec;

    #[test]
    fn spsc_basic_push_pop_fifo() {
        // Single-threaded sanity: push to capacity, pop in FIFO order.
        // SpscRingBuf<_, N> has usable capacity N-1 because one slot is
        // sacrificed to distinguish empty from full.
        let rb: SpscRingBuf<u32, 8> = SpscRingBuf::new();
        for i in 0..7u32 {
            assert!(rb.push(i).is_ok(), "push {} unexpectedly failed", i);
        }
        // Buffer is now full
        assert!(rb.is_full());
        assert_eq!(rb.push(99), Err(99), "push to full should return value");
        // Pop them back in order
        for i in 0..7u32 {
            assert_eq!(rb.pop(), Some(i));
        }
        assert!(rb.is_empty());
        assert_eq!(rb.pop(), None);
    }

    #[test]
    fn spsc_wraparound_preserves_fifo() {
        // Exercise wraparound by repeatedly pushing/popping batches that
        // don't divide the buffer size evenly. Each batch advances head
        // and tail by 3, so over many iterations they cross the modulus
        // boundary several times.
        let rb: SpscRingBuf<u32, 4> = SpscRingBuf::new();
        for batch in 0..20u32 {
            let base = batch * 3;
            assert!(rb.push(base).is_ok());
            assert!(rb.push(base + 1).is_ok());
            assert!(rb.push(base + 2).is_ok());
            assert_eq!(rb.pop(), Some(base));
            assert_eq!(rb.pop(), Some(base + 1));
            assert_eq!(rb.pop(), Some(base + 2));
        }
        assert!(rb.is_empty());
    }

    #[test]
    fn spsc_concurrent_producer_consumer() {
        // The headline Miri test. Producer pushes 0..COUNT in order on
        // one thread; consumer pops COUNT values on another. SPSC must
        // preserve FIFO order regardless of how the threads interleave.
        //
        // What Miri verifies here:
        //   - No data race on the storage cells (would happen if Acquire
        //     /Release ordering on head/tail were too weak — e.g. Relaxed)
        //   - No read of an uninitialised slot (would happen if the
        //     consumer's "is there data" check synced incorrectly)
        //   - FIFO order across all observed interleavings
        //
        // COUNT is small enough that Miri finishes in a few seconds even
        // when stressed with many seeds.
        const COUNT: u32 = 100;
        let rb: Arc<SpscRingBuf<u32, 8>> = Arc::new(SpscRingBuf::new());

        let producer = {
            let rb = Arc::clone(&rb);
            thread::spawn(move || {
                for i in 0..COUNT {
                    while rb.push(i).is_err() {
                        std::hint::spin_loop();
                    }
                }
            })
        };

        let consumer = {
            let rb = Arc::clone(&rb);
            thread::spawn(move || {
                let mut received: Vec<u32> = Vec::with_capacity(COUNT as usize);
                while received.len() < COUNT as usize {
                    if let Some(v) = rb.pop() {
                        received.push(v);
                    } else {
                        std::hint::spin_loop();
                    }
                }
                received
            })
        };

        producer.join().expect("producer panicked");
        let received = consumer.join().expect("consumer panicked");

        // Producer pushed 0..COUNT in order; SPSC must preserve that.
        let expected: Vec<u32> = (0..COUNT).collect();
        assert_eq!(received, expected, "FIFO order violated");
    }

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

    // ─────────────────────────────────────────────────────────────────
    // RingBuf
    //
    // RingBuf is the overwrite-when-full variant: pushing into a full
    // buffer overwrites the oldest element rather than failing. Usable
    // capacity is N - 1 (one slot is sacrificed to distinguish empty
    // from full via head == tail).
    //
    // The Miri-relevant invariant is the same as StackVec — every
    // `assume_init_ref` inside get/iter/newest must access a slot that
    // was previously written. The interesting bug class here is
    // *wraparound off-by-one*, where head/tail arithmetic could read
    // a slot that was already logically discarded.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn ringbuf_basic_get_oldest_to_newest() {
        let mut rb: RingBuf<u32, 5> = RingBuf::new();
        // Push three values; usable capacity is 4 (= N - 1).
        rb.push(10);
        rb.push(20);
        rb.push(30);
        assert_eq!(rb.len(), 3);
        // get(0) is oldest, get(len-1) is newest.
        assert_eq!(rb.get(0), Some(&10));
        assert_eq!(rb.get(1), Some(&20));
        assert_eq!(rb.get(2), Some(&30));
        assert_eq!(rb.get(3), None);
    }

    #[test]
    fn ringbuf_overwrites_oldest_when_full() {
        // RingBuf with N = 4 has usable capacity 3. Pushing four
        // values should overwrite the first one; pushing a fifth
        // should overwrite the second; etc.
        let mut rb: RingBuf<u32, 4> = RingBuf::new();
        rb.push(1);
        rb.push(2);
        rb.push(3);
        assert_eq!(rb.len(), 3);
        assert!(rb.is_full());
        // This push overwrites 1.
        rb.push(4);
        assert_eq!(rb.len(), 3);
        let collected: Vec<u32> = rb.iter().copied().collect();
        assert_eq!(collected, vec![2, 3, 4]);
        // And another, overwriting 2.
        rb.push(5);
        let collected: Vec<u32> = rb.iter().copied().collect();
        assert_eq!(collected, vec![3, 4, 5]);
    }

    #[test]
    fn ringbuf_iter_yields_oldest_first() {
        let mut rb: RingBuf<u32, 5> = RingBuf::new();
        for i in [10, 20, 30, 40] {
            rb.push(i);
        }
        // Each iteration step calls assume_init_ref on the visited
        // slot; Miri verifies it's an initialised slot.
        let collected: Vec<u32> = rb.iter().copied().collect();
        assert_eq!(collected, vec![10, 20, 30, 40]);
    }

    #[test]
    fn ringbuf_newest_indexing() {
        let mut rb: RingBuf<u32, 5> = RingBuf::new();
        rb.push(10);
        rb.push(20);
        rb.push(30);
        // newest(0) is the most recent push, newest(len-1) the oldest.
        assert_eq!(rb.newest(0), Some(&30));
        assert_eq!(rb.newest(1), Some(&20));
        assert_eq!(rb.newest(2), Some(&10));
        assert_eq!(rb.newest(3), None);
    }

    #[test]
    fn ringbuf_wraparound_preserves_order() {
        // Push enough values to wrap head/tail past N several times.
        // Each push of more than capacity overwrites the oldest. After
        // the loop the ring should hold the LAST `cap` values pushed,
        // in oldest-to-newest order.
        let mut rb: RingBuf<u32, 4> = RingBuf::new();
        for i in 0..20u32 {
            rb.push(i);
        }
        // usable capacity is 3, so the buffer holds [17, 18, 19].
        let collected: Vec<u32> = rb.iter().copied().collect();
        assert_eq!(collected, vec![17, 18, 19]);
        // Indexing via get/newest should agree.
        assert_eq!(rb.get(0), Some(&17));
        assert_eq!(rb.get(2), Some(&19));
        assert_eq!(rb.newest(0), Some(&19));
        assert_eq!(rb.newest(2), Some(&17));
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
