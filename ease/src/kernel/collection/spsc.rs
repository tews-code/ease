//! Single producer single consumer ring buffer
//!
//! Lock free ring buffer on type `T`. `T` must be `Copy` as the ring never drops slot contents
//! (and Copy makes that correct because Copy types have no destructor).
//! The size of the buffer must be power of two.
//!
//! In order to protect the single producer / single consumer requirements, the producer and
//! consumer structs are only safely provided by the split() method.
//!```
//!
//!      SPSC         THREAD1
//!      N = 8       (Producer::push())
//!     +-----+
//!  0  |     |
//!     +-----+
//!  1  | "H" | <-- Head
//!     +-----+
//!  2  | "e" |
//!     +-----+
//!  3  | "l" |
//!     +-----+
//!  4  | "l" |
//!     +-----+
//!  5  | "o" |
//!     +-----+
//!  6  | "!" |
//!     +-----+
//!  7  |     | <-- Tail
//!     +-----+
//!              THREAD2
//!             (Consumer::pop())
//! ```
//! - Queue is a simple array
//! - Slots are UnsafeCell to allow for static queues (with interior mutability)
//! - Each UnsafeCell is on MaybeUninit to account for empty cells
//! - The queue is Sync by design, &queue can be sent between threads since one of the Producer or
//!   Consumer will be moved out of the creating thread and need to reference the queue. Also,as part
//!   of the design, `T` needs to be `Send`, as it will be sent by value from the producing thread
//!   to the consuming thread.
//! - [Producer::push] and [Consumer::pop] do not need to be unsafe as the handles enforce the
//!   single-writer rule.
//!

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

/// The queue
///
/// By design, T is Send so that we can send from the producer thread to the consumer thread.
/// We also insist that T is Copy as we do not Drop the held slots when the queue is dropped.
pub(crate) struct Queue<T: Copy + Send, const N: usize> {
    buf: [UnsafeCell<MaybeUninit<T>>; N],
    head: AtomicUsize, // Free running and wrapping; allows for max size of u32::MAX / 2;
    tail: AtomicUsize, // Free running and wrapping
}
/// The queue is Sync by design - once created, at least one of the Producer or Consumer handles will
/// be sent by reference to another thread.
// Safety: UnsafeCell is !Sync but we have a single writer per slot (enforced by the handles and the counters)
unsafe impl<T: Copy + Send, const N: usize> Sync for Queue<T, N> {}

impl<T: Copy + Send, const N: usize> Queue<T, N> {
    /// New queue
    pub(crate) const fn new() -> Self {
        const {
            assert!(
                N.is_power_of_two(),
                "N must be a power of two for the mask to work"
            )
        };
        const {
            assert!(
                N <= (u32::MAX / 2) as usize,
                "N must be at most half the u32 counter range"
            )
        };
        Self {
            buf: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }
    /// Create the Producer and the Consumer handles for a borrowed queue.
    ///
    /// We take an exclusive borrow of the queue to hold it, enforcing that only
    /// a single Producer and Consumer pair can exist at a time and the queue cannot
    /// be used by anyone else for the duration of the borrow.
    ///
    /// Once both the Producer and Consumer handles are dropped the queue can be split again and reused.
    ///
    /// Split shares two non-exclusive sibling reborrows for Producer and Consumer.
    /// The Producer and Consumer structs use & borrows as they use UnsafeCell
    /// for interior mutability and atomics for the indices.
    ///
    ///         q               Owner
    ///         │
    ///      &mut q             One exclusive borrow (what the outside sees)
    ///      /    \
    ///     &q      &q          Two shared reborrows, children of the &mut
    ///     │       │
    ///  Producer  Consumer
    ///
    /// Bare in mind that Rust aliasing refers to _bytes_, not the entire object. In
    /// our case `push` only writes bytes at the head, and `pop` only reads/writes bytes
    /// at the tail, and from the logic and atomic indices these never overlap.
    pub(crate) fn split<'a>(&'a mut self) -> (Producer<'a, T, N>, Consumer<'a, T, N>) {
        // If set tail and head to zero, no stale data in the queue can be read
        *self.head.get_mut() = 0; // We can skip the atomic methods as this is held under a &mut borrow
        *self.tail.get_mut() = 0;
        // Now create the shared refs (under the safety of the common mut ref)
        (Producer { queue: &*self }, Consumer { queue: &*self })
    }
    /// Unsafe constructor for the Producer for static queues
    ///
    /// Safety: the caller must ensure that this is the only Producer created
    /// and that [Self::split] is never called on this queue
    pub(crate) const unsafe fn producer_unchecked(&'static self) -> Producer<'static, T, N> {
        Producer { queue: self }
    }
    /// Unsafe constructor for the Consumer
    ///
    /// Safety: the caller must ensure that this is the only Consumer created
    /// and that [Self::split] is never called on this queue
    pub(crate) const unsafe fn consumer_unchecked(&'static self) -> Consumer<'static, T, N> {
        Consumer { queue: self }
    }
    /// Gives the number of used slots
    pub(crate) fn len(&self) -> usize {
        self.tail
            .load(Ordering::Relaxed)
            .wrapping_sub(self.head.load(Ordering::Relaxed))
    }
    /// Gives the number of remaining slots
    pub(crate) fn remaining(&self) -> usize {
        N - self.len()
    }
    /// Checks if the queue is empty
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Checks if the queue is full
    pub(crate) fn is_full(&self) -> bool {
        self.len() == N
    }
}

/// Producer thread holds this struct to access push()
pub(crate) struct Producer<'a, T: Copy + Send, const N: usize> {
    queue: &'a Queue<T, N>,
}

impl<'a, T: Copy + Send, const N: usize> Producer<'a, T, N> {
    /// Push a `T` onto the queue.
    /// `Ok(())` on success or `Err(T)` if the queue is full.
    /// Takes &mut on `Producer` so simultaneous calls impossible
    pub(crate) fn push(&mut self, val: T) -> Result<(), T> {
        // Safe to use a local without a critical section as this is the only function that updates tail
        let tail = self.queue.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(self.queue.head.load(Ordering::Acquire)) == N {
            return Err(val);
        }
        // Mask on counters (efficient version of `%` when N is power of two)
        // Size: N must be power of two to allow for masking trick (to avoid %)
        // tail & (N - 1) is identical to tail % N if N is power of two. E.g. 25 % 8 = b11001 & (b01000 - 1) = 1
        let slot = tail & (N - 1);
        // Safety: Only one writer to the slot, and atomic Acquire on head ensures its free
        unsafe { (*self.queue.buf[slot].get()).write(val) };
        self.queue
            .tail
            .store(tail.wrapping_add(1), Ordering::Release); // Wraps on overflow
        Ok(())
    }
}
/// Consumer thread holds this struct to access pop()
pub(crate) struct Consumer<'a, T: Copy + Send, const N: usize> {
    queue: &'a Queue<T, N>,
}

impl<'a, T: Copy + Send, const N: usize> Consumer<'a, T, N> {
    /// Pop a `T` off the queue
    ///
    /// Returns `None` if the queue is empty.
    /// Takes &mut on `Consumer` so no possible simultaneous callers
    pub(crate) fn pop(&mut self) -> Option<T> {
        // Safe to use a local without a critical section as this is the only function that updates head
        let head = self.queue.head.load(Ordering::Relaxed);
        if self.queue.tail.load(Ordering::Acquire).wrapping_sub(head) == 0 {
            // Queue empty
            return None;
        }
        let slot = head & (N - 1);
        // Safety: Only one reader of the slot, and atomic Acquire on tail ensures data
        let val = unsafe { (*self.queue.buf[slot].get()).assume_init_read() };
        self.queue
            .head
            .store(head.wrapping_add(1), Ordering::Release);
        Some(val)
    }
}

// Host-runnable tests for the collection types. Run with
//     cargo test --lib --target $HOST_TARGET
// and verified clean under Miri:
//     cargo +nightly miri test --lib --target $HOST_TARGET
//
// The Queue concurrency test is the headline value-add: Miri can
// simulate multi-threaded execution and check the atomic ordering on
// push()/pop() against the actual reads and writes of the underlying
// storage cells. The single-threaded tests cover the index arithmetic:
// full at exactly N (free-running indices waste no slot), the
// power-of-two mask across the buffer boundary, and u32 index rollover.
// `T: Copy`, so there are no Drop concerns to test.
//
// To stress-test more interleavings of the concurrent Queue
// test, run with
//     MIRIFLAGS="-Zmiri-many-seeds=0..32" cargo +nightly miri test --lib ...
//
// Gated on `not(target_os = "none")` so the kernel build (and the
// existing QEMU `#[test_case]` framework) is unaffected.
#[cfg(all(test, not(target_os = "none"), feature = "test-collections"))]
mod host_tests {
    use super::*;
    use std::thread;
    use std::vec::Vec;

    #[test]
    fn spsc_resplit_resets_queue() {
        // A second split is only possible once the first pair of handles is
        // gone (borrow checker), and it must start from an empty queue.
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, _c) = q.split();
        p.push(1).unwrap();
        p.push(2).unwrap();
        let (mut p, mut c) = q.split();
        assert!(p.queue.is_empty(), "re-split must reset the indices");
        assert_eq!(
            c.pop(),
            None,
            "stale data must not be readable after re-split"
        );
        p.push(3).unwrap();
        assert_eq!(c.pop(), Some(3));
    }

    #[test]
    fn spsc_full_at_exactly_n() {
        // Free-running indices: every one of the N slots is usable.
        let mut q: Queue<u32, 8> = Queue::new();
        let (mut p, mut c) = q.split();
        assert!(p.queue.is_empty());
        for i in 0..8u32 {
            assert!(!p.queue.is_full());
            assert!(p.push(i).is_ok(), "push {i} unexpectedly failed");
        }
        assert!(p.queue.is_full());
        assert_eq!(p.push(99), Err(99), "push to full must return the value");
        for i in 0..8u32 {
            assert_eq!(c.pop(), Some(i));
        }
        assert!(p.queue.is_empty());
        assert_eq!(c.pop(), None);
    }

    #[test]
    fn spsc_wraparound_preserves_fifo() {
        // Batches of 3 through a ring of 4 cross the mask boundary on
        // most iterations, so slot reuse is exercised many times.
        let mut q: Queue<u32, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        for batch in 0..20u32 {
            let base = batch * 3;
            assert!(p.push(base).is_ok());
            assert!(p.push(base + 1).is_ok());
            assert!(p.push(base + 2).is_ok());
            assert_eq!(c.pop(), Some(base));
            assert_eq!(c.pop(), Some(base + 1));
            assert_eq!(c.pop(), Some(base + 2));
        }
        assert!(p.queue.is_empty());
    }

    #[test]
    fn spsc_index_rollover() {
        // Start both indices just below u32::MAX so a few pushes carry
        // them through the wrap. The count (tail - head) and the slot
        // (index & mask) must both survive it. Fields are reachable
        // because this is a child module of the ring's own module.
        let mut q: Queue<u32, 8> = Queue::new();
        let start = usize::MAX - 2;
        q.head.store(start, Ordering::Relaxed);
        q.tail.store(start, Ordering::Relaxed);
        let (mut p, mut c) = q.split();
        for i in 0..8u32 {
            assert!(p.push(i).is_ok(), "push {i} failed across rollover");
        }
        assert!(p.queue.is_full(), "full must be detected across rollover");
        assert_eq!(p.push(99), Err(99));
        for i in 0..8u32 {
            assert_eq!(c.pop(), Some(i), "FIFO broken across rollover");
        }
        assert!(p.queue.is_empty(), "empty must be detected across rollover");
        // Both indices have wrapped past zero by now.
        assert!(p.queue.tail.load(Ordering::Relaxed) < start);
        assert!(q.head.load(Ordering::Relaxed) < start);
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
        // Scoped threads let the handles borrow a stack queue and move
        // to their own threads without leaking anything, which keeps
        // Miri's leak checker quiet. COUNT is small enough that Miri
        // finishes in a few seconds even when stressed with many seeds.
        const COUNT: u32 = 100;
        let mut q: Queue<u32, 8> = Queue::new();
        let (mut p, mut c) = q.split();

        let received = thread::scope(|s| {
            s.spawn(move || {
                for i in 0..COUNT {
                    while p.push(i).is_err() {
                        std::hint::spin_loop();
                    }
                }
            });
            let consumer = s.spawn(move || {
                let mut received: Vec<u32> = Vec::with_capacity(COUNT as usize);
                while received.len() < COUNT as usize {
                    if let Some(v) = c.pop() {
                        received.push(v);
                    } else {
                        std::hint::spin_loop();
                    }
                }
                received
            });
            consumer.join().expect("consumer panicked")
        });

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

    // ── Queue tests ──

    #[test_case]
    fn test_spsc_resplit_resets_queue() {
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, _c) = q.split();
        p.push(1).unwrap();
        p.push(2).unwrap();
        let (mut p, mut c) = q.split();
        assert!(p.queue.is_empty());
        assert_eq!(c.pop(), None);
        p.push(3).unwrap();
        assert_eq!(c.pop(), Some(3));
    }

    #[test_case]
    fn test_spsc_push_pop() {
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        assert_eq!(c.pop(), None);
        assert!(p.push(42).is_ok());
        assert_eq!(c.pop(), Some(42));
        assert_eq!(c.pop(), None);
    }

    #[test_case]
    fn test_spsc_full_at_n() {
        // Free-running indices: all 4 slots usable
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, _c) = q.split();
        assert!(p.push(1).is_ok());
        assert!(p.push(2).is_ok());
        assert!(p.push(3).is_ok());
        assert!(p.push(4).is_ok());
        assert!(p.queue.is_full());
        assert_eq!(p.push(5), Err(5)); // full
    }

    #[test_case]
    fn test_spsc_fifo_order() {
        let mut q: Queue<u8, 8> = Queue::new();
        let (mut p, mut c) = q.split();
        p.push(10).unwrap();
        p.push(20).unwrap();
        p.push(30).unwrap();
        assert_eq!(c.pop(), Some(10));
        assert_eq!(c.pop(), Some(20));
        assert_eq!(c.pop(), Some(30));
    }

    #[test_case]
    fn test_spsc_wraparound() {
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        // Fill and drain, then push again so the slot index wraps to 0
        p.push(1).unwrap();
        p.push(2).unwrap();
        p.push(3).unwrap();
        assert_eq!(c.pop(), Some(1));
        assert_eq!(c.pop(), Some(2));
        assert_eq!(c.pop(), Some(3));
        p.push(4).unwrap();
        p.push(5).unwrap();
        assert_eq!(c.pop(), Some(4));
        assert_eq!(c.pop(), Some(5));
        assert_eq!(c.pop(), None);
    }

    #[test_case]
    fn test_spsc_index_rollover() {
        // Indices start near usize::MAX and wrap during the test
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        // split() zeroes the indices, so set them afterwards via the shared borrow
        let start = usize::MAX - 1;
        p.queue.head.store(start, Ordering::Relaxed);
        p.queue.tail.store(start, Ordering::Relaxed);
        for i in 0..4u8 {
            p.push(i).unwrap();
        }
        assert!(p.queue.is_full());
        for i in 0..4u8 {
            assert_eq!(c.pop(), Some(i));
        }
        assert!(p.queue.is_empty());
        assert!(p.queue.tail.load(Ordering::Relaxed) < start);
    }

    #[test_case]
    fn test_spsc_interleaved() {
        let mut q: Queue<u32, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        p.push(1).unwrap();
        p.push(2).unwrap();
        assert_eq!(c.pop(), Some(1));
        p.push(3).unwrap();
        p.push(4).unwrap();
        assert_eq!(c.pop(), Some(2));
        assert_eq!(c.pop(), Some(3));
        assert_eq!(c.pop(), Some(4));
        assert_eq!(c.pop(), None);
    }

    #[test_case]
    fn test_spsc_bool() {
        let mut q: Queue<bool, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        p.push(true).unwrap();
        p.push(false).unwrap();
        p.push(true).unwrap();
        assert_eq!(c.pop(), Some(true));
        assert_eq!(c.pop(), Some(false));
        assert_eq!(c.pop(), Some(true));
        assert_eq!(c.pop(), None);
    }

    #[test_case]
    fn test_spsc_is_full() {
        let mut q: Queue<u8, 4> = Queue::new();
        let (mut p, mut c) = q.split();
        assert!(!p.queue.is_full());
        for i in 1..=3u8 {
            p.push(i).unwrap();
            assert!(!p.queue.is_full());
        }
        p.push(4).unwrap();
        assert!(p.queue.is_full());
        c.pop();
        assert!(!p.queue.is_full());
    }

    #[test_case]
    fn test_spsc_size_one() {
        // N=1 is a power of two and holds exactly one item
        let mut q: Queue<u8, 1> = Queue::new();
        let (mut p, mut c) = q.split();
        assert!(p.queue.is_empty());
        assert!(p.push(1).is_ok());
        assert!(p.queue.is_full());
        assert_eq!(p.push(2), Err(2));
        assert_eq!(c.pop(), Some(1));
        assert_eq!(c.pop(), None);
    }

    #[test_case]
    fn test_spsc_size_two() {
        let mut q: Queue<u8, 2> = Queue::new();
        let (mut p, mut c) = q.split();
        p.push(99).unwrap();
        assert!(!p.queue.is_full());
        p.push(100).unwrap();
        assert!(p.queue.is_full());
        assert_eq!(p.push(101), Err(101));
        assert_eq!(c.pop(), Some(99));
        assert!(!p.queue.is_full());
        p.push(101).unwrap();
        assert_eq!(c.pop(), Some(100));
        assert_eq!(c.pop(), Some(101));
    }

    #[test_case]
    fn test_spsc_tuple() {
        let mut q: Queue<(u8, u16), 4> = Queue::new();
        let (mut p, mut c) = q.split();
        p.push((1, 100)).unwrap();
        p.push((2, 200)).unwrap();
        assert_eq!(c.pop(), Some((1, 100)));
        assert_eq!(c.pop(), Some((2, 200)));
        assert_eq!(c.pop(), None);
    }
}
