//! Ring buffer on the stack

use core::mem::MaybeUninit;

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
        assert!(r.iter().copied().eq([10u32, 20, 30]));
    }

    #[test_case]
    fn test_ring_iter_after_wraparound() {
        let mut r: RingBuf<u32, 4> = RingBuf::new();
        r.push(1);
        r.push(2);
        r.push(3);
        r.push(4); // [2, 3, 4]
        r.push(5); // [3, 4, 5]
        assert!(r.iter().copied().eq([3u32, 4, 5]));
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
}
