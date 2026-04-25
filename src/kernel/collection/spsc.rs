//! Single producer single consumer ring buffer

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

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
#[cfg(all(test, not(target_os = "none"), feature = "test-collections"))]
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
}

// QEMU tests: gated on target_os = "none" so the kernel-only `#[test_case]`
// custom test framework does not collide with libtest when this file is
// compiled as part of the lib crate's host tests.
#[cfg(all(test, target_os = "none", feature = "test-collections"))]
mod tests {
    use super::*;

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
