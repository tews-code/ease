//! Minimal bitmap
//
// Not interrupt safe

#![allow(dead_code)]

const BITS_PER_WORD: usize = u32::BITS as usize; // Use u32 on our 32 bit system

// Bitmap storage based on u32 words
pub struct Bitmap<const BITS: usize, const WORDS: usize> {
    bits: [u32; WORDS],
}

/// Helper function to convert required bits to number of `words`
///
/// Use to set up the bitmap by number of required bits.
/// ```text
/// const REQ_BITS: usize = 11;
/// let bitmap = Bitmap::<REQ_BITS, { bitmap_words_for(REQ_BITS) }>::new();
/// ```
pub const fn bitmap_words_for(bit_count: usize) -> usize {
    bit_count.div_ceil(BITS_PER_WORD)
}

impl<const BITS: usize, const WORDS: usize> Bitmap<BITS, WORDS> {
    /// Create a new bitmap
    ///
    /// Requires both required number of bits and word count
    /// Note that word count can be found from the helper function
    /// bitmap_words_for().
    /// ```text
    /// const REQ_BITS: usize = 11;
    /// let bitmap = Bitmap::<REQ_BITS,{ bitmap_words_for(REQ_BITS)}>::new();
    /// ```
    /// The bitmap is initialised as all flags cleared.
    /// There must be at least one bit (zero-sized bitmap not allowed).
    pub const fn new() -> Self {
        const {
            assert!(WORDS == bitmap_words_for(BITS));
        }
        const {
            assert!(BITS > 0);
        }
        Self { bits: [0; WORDS] }
    }

    // Return the word index and bitmask for a bit
    const fn word_and_mask(bit: usize) -> (usize, u32) {
        (bit / BITS_PER_WORD, 1u32 << (bit % BITS_PER_WORD))
    }

    /// Return the value at `bit` as a boolean
    ///
    /// Panics: Panics if the requested bit is outside of the bitmap range
    #[inline]
    pub const fn get(&self, bit: usize) -> bool {
        debug_assert!(bit < BITS);
        let (word_index, bit_mask) = Self::word_and_mask(bit);
        (self.bits[word_index] & bit_mask) != 0
    }

    /// Set the flag at `bit`
    ///
    /// Panics: Panics if the requested bit is outside of the bitmap range
    #[inline]
    pub const fn set(&mut self, bit: usize) {
        debug_assert!(bit < BITS);
        let (word_index, bit_mask) = Self::word_and_mask(bit);
        self.bits[word_index] |= bit_mask;
    }

    /// Clear the flag at `bit`
    ///
    /// Panics: Panics if the requested bit is outside of the bitmap range
    #[inline]
    pub const fn clear(&mut self, bit: usize) {
        debug_assert!(bit < BITS);
        let (word_index, bit_mask) = Self::word_and_mask(bit);
        self.bits[word_index] &= !bit_mask;
    }

    /// Toggle the flag at `bit`
    ///
    /// Panics: Panics if the requested bit is outside of the bitmap range
    #[inline]
    pub const fn toggle(&mut self, bit: usize) -> bool {
        debug_assert!(bit < BITS);
        let (word_index, bit_mask) = Self::word_and_mask(bit);
        self.bits[word_index] ^= bit_mask;
        (self.bits[word_index] & bit_mask) != 0
    }
}

// Host-runnable tests. The bitmap is pure logic with no hardware
// dependency, so everything is verified on the host target:
//     cargo test --lib --target $HOST_TARGET
// and under Miri:
//     cargo +nightly miri test --lib --target $HOST_TARGET
//
// Gated on `not(target_os = "none")` so the kernel build (and the
// existing QEMU `#[test_case]` framework) is unaffected.
#[cfg(all(test, not(target_os = "none"), feature = "test-collections"))]
mod host_tests {
    use super::*;

    // The four interesting behaviours:
    //   1. round-trip — set(n) then get(n) returns true.
    //   2. independence — touching bit N never perturbs bit M.
    //   3. toggle-twice — toggle(n) twice returns to the starting state
    //      and yields the correct post-state booleans on each call.
    //   4. bounds-failure — accessing a bit >= BITS panics in debug.
    //
    // A `Bitmap<64, 2>` straddles a u32 word boundary, so tests that use
    // it also verify that bit 32 lands in word 1 rather than word 0.

    #[test]
    fn new_starts_all_zero() {
        let bm: Bitmap<100, 4> = Bitmap::new();
        for bit in 0..100 {
            assert!(!bm.get(bit));
        }
    }

    #[test]
    fn set_then_get_round_trip() {
        let mut bm: Bitmap<64, 2> = Bitmap::new();
        for bit in [0usize, 1, 7, 31, 32, 33, 63] {
            bm.set(bit);
            assert!(bm.get(bit), "bit {} should be set", bit);
        }
    }

    #[test]
    fn clear_only_affects_target_bit() {
        let mut bm: Bitmap<64, 2> = Bitmap::new();
        bm.set(5);
        bm.set(40);
        bm.clear(5);
        assert!(!bm.get(5));
        assert!(bm.get(40), "clear(5) must not touch bit 40");
    }

    #[test]
    fn set_is_independent_of_other_bits() {
        // Set one bit, verify every other bit stays zero. Covers the
        // cross-word case (bit 32 and beyond live in word 1).
        for target in [0usize, 1, 15, 31, 32, 33, 47, 63] {
            let mut bm: Bitmap<64, 2> = Bitmap::new();
            bm.set(target);
            for probe in 0..64 {
                let expected = probe == target;
                assert_eq!(
                    bm.get(probe),
                    expected,
                    "set({}) unexpectedly changed bit {}",
                    target,
                    probe
                );
            }
        }
    }

    #[test]
    fn toggle_twice_is_identity() {
        let mut bm: Bitmap<100, 4> = Bitmap::new();
        for bit in [0usize, 31, 32, 63, 64, 99] {
            let first = bm.toggle(bit);
            let second = bm.toggle(bit);
            assert!(first, "first toggle({}) should return true", bit);
            assert!(!second, "second toggle({}) should return false", bit);
            assert!(!bm.get(bit), "bit {} should be back to 0", bit);
        }
    }

    #[test]
    fn toggle_returns_post_state() {
        let mut bm: Bitmap<32, 1> = Bitmap::new();
        // 0 -> 1 returns true
        assert!(bm.toggle(5));
        assert!(bm.get(5));
        // 1 -> 0 returns false
        assert!(!bm.toggle(5));
        assert!(!bm.get(5));
    }

    #[test]
    fn word_boundary_indexing() {
        // Bits 31, 32, 33 straddle the u32 word boundary; this verifies
        // the word/mask arithmetic across the boundary.
        let mut bm: Bitmap<64, 2> = Bitmap::new();
        bm.set(31);
        bm.set(32);
        bm.set(33);
        assert!(bm.get(31));
        assert!(bm.get(32));
        assert!(bm.get(33));
        for bit in 0..64 {
            if matches!(bit, 31 | 32 | 33) {
                continue;
            }
            assert!(!bm.get(bit), "bit {} unexpectedly set", bit);
        }
    }

    #[test]
    fn last_bit_is_addressable() {
        let mut bm: Bitmap<11, 1> = Bitmap::new();
        bm.set(10);
        assert!(bm.get(10));
    }

    #[test]
    #[should_panic]
    fn get_out_of_range_panics_in_debug() {
        let bm: Bitmap<10, 1> = Bitmap::new();
        let _ = bm.get(10);
    }

    #[test]
    #[should_panic]
    fn set_out_of_range_panics_in_debug() {
        let mut bm: Bitmap<10, 1> = Bitmap::new();
        bm.set(10);
    }

    #[test]
    #[should_panic]
    fn clear_out_of_range_panics_in_debug() {
        let mut bm: Bitmap<10, 1> = Bitmap::new();
        bm.clear(10);
    }

    #[test]
    #[should_panic]
    fn toggle_out_of_range_panics_in_debug() {
        let mut bm: Bitmap<10, 1> = Bitmap::new();
        let _ = bm.toggle(10);
    }
}
