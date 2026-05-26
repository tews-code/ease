//! 64-bit counter support on 32-bit
//-------------------------------------------------------------------------
//
//  CounterU64
//
//-------------------------------------------------------------------------

use core::sync::atomic::{AtomicU32, Ordering};

/// Lock-free monotonic 64-bit counter
///
/// Only supports single writer.
///
/// Uses Acquire / Release ordering
pub struct CounterU64 {
    seq: AtomicU32,
    hi: AtomicU32,
    lo: AtomicU32,
}

impl CounterU64 {
    pub const fn new(v: u64) -> Self {
        Self {
            seq: AtomicU32::new(0),
            hi: AtomicU32::new((v >> 32) as u32),
            lo: AtomicU32::new(v as u32),
        }
    }

    /// Add to the counter
    ///
    /// Returns the previous counter value
    /// Safety: Caller must ensure only single writer (no concurrency)
    pub unsafe fn add(&self, v: u64) -> u64 {
        // Sequence lock to prevent readers from seeing tearing
        let s = self.seq.fetch_add(1, Ordering::Acquire); // Odd - write in progress
        assert!(s & 1 == 0, "multiple writers not allowed");
        let lo = self.lo.fetch_add(v as u32, Ordering::Relaxed);
        let hi = if lo.wrapping_add(v as u32) < lo {
            self.hi
                .fetch_add(((v >> 32) as u32).wrapping_add(1), Ordering::Relaxed)
        } else {
            self.hi.fetch_add((v >> 32) as u32, Ordering::Relaxed)
        };
        self.seq.fetch_add(1, Ordering::Release); // Even - write complete
        ((hi as u64) << 32) | (lo as u64)
    }

    /// Reads the current counter value
    #[allow(dead_code)]
    pub fn get(&self) -> u64 {
        loop {
            // Check if a write is in progress
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                // Odd - write in progress
                core::hint::spin_loop();
                continue;
            }
            let hi = self.hi.load(Ordering::Relaxed);
            let lo = self.lo.load(Ordering::Relaxed);
            let s2 = self.seq.load(Ordering::Acquire);
            if s1 != s2 {
                core::hint::spin_loop();
                continue; // Write happened during read, start again
            }
            return ((hi as u64) << 32) | (lo as u64);
        }
    }

    /// Reset the counter
    ///
    /// Safety: Caller must ensure only single reset caller (no concurrency)
    #[allow(dead_code)]
    pub unsafe fn reset(&self) {
        // Sequence lock to prevent readers from seeing tearing
        let s = self.seq.fetch_add(1, Ordering::Acquire);
        assert!(s & 1 == 0, "multiple writers not allowed");
        self.hi.store(0, Ordering::Relaxed);
        self.lo.store(0, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Set the counter to a u64 value
    ///
    /// Returns the previous counter value
    /// Safety: Caller must ensure only single writer (no concurrency)
    #[allow(dead_code)]
    pub unsafe fn set(&self, v: u64) -> u64 {
        // Sequence lock to prevent readers from seeing tearing
        let s = self.seq.fetch_add(1, Ordering::Acquire);
        assert!(s & 1 == 0, "multiple writers not allowed");
        let hi = self.hi.swap((v >> 32) as u32, Ordering::Relaxed);
        let lo = self.lo.swap(v as u32, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
        ((hi as u64) << 32) | (lo as u64)
    }
}
