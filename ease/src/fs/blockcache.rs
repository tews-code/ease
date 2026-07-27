//! Block cache

use crate::arch::csr::rdcycles;
use crate::board::virtio::blk::BLOCK_SIZE;
use crate::drivers::virtio::blk::{BlkError, read_block, write_block};

const BLOCK_CACHE_COUNT: usize = 4;

struct Meta {
    block: u32,
    last_used_cycles: u64,
}
struct BlockBuf {
    data: [u8; BLOCK_SIZE],
    meta: Option<Meta>,
}

impl BlockBuf {
    const EMPTY: Self = Self {
        data: [0u8; BLOCK_SIZE],
        meta: None,
    };
}

pub(super) struct BlockCache([BlockBuf; BLOCK_CACHE_COUNT]);

impl BlockCache {
    pub(super) const fn new() -> Self {
        Self([BlockBuf::EMPTY; BLOCK_CACHE_COUNT])
    }

    // Helper function to ensure block is cached
    //
    // Ensures cache is updated if needed, returns cache index
    fn ensure_cached(&mut self, block: u32) -> Result<usize, BlkError> {
        // Iterate through the array to find a valid block match, return index
        let idx = if let Some(idx) = self
            .0
            .iter()
            .position(|b| b.meta.as_ref().is_some_and(|m| m.block == block))
        {
            idx
        } else {
            // Cache miss
            let (idx, buf) = self
                .0
                .iter_mut()
                .enumerate()
                .min_by_key(|(_, b)| b.meta.as_ref().map_or(0, |m| m.last_used_cycles))
                .expect("cache has at least one slot");
            // Clear meta data in case of block error
            buf.meta = None;
            read_block(block, &mut buf.data)?;
            idx
        };
        self.0[idx].meta = Some(Meta {
            block,
            last_used_cycles: rdcycles(),
        });
        Ok(idx)
    }

    // Linear scan of the cache
    //
    // A cache hit returns a borrow of the entry's buffer
    // A cache miss evicts least recently used cache, then fills from read_block
    // and returns the borrow.
    pub(super) fn read(&mut self, block: u32) -> Result<&[u8; BLOCK_SIZE], BlkError> {
        let idx = self.ensure_cached(block)?;
        Ok(&self.0[idx].data)
    }

    // Modify a block - write through the cache
    pub(super) fn modify<F>(&mut self, block: u32, f: F) -> Result<(), BlkError>
    where
        F: FnOnce(&mut [u8; BLOCK_SIZE]),
    {
        // Check that the relevant block is cached
        let idx = self.ensure_cached(block)?;
        // Get the cache block as a mutable buffer
        let buf = &mut self.0[idx].data;
        // Run closure on the block
        f(buf);
        // Write back the cached block
        if let Err(blk_error) = write_block(block, buf) {
            // Invalidate the cache on write failure
            self.0[idx].meta = None;
            return Err(blk_error);
        }
        Ok(())
    }

    // Whole-block write for bulk data (avoids cache)
    pub(super) fn write_uncached(
        &mut self,
        block: u32,
        buf: &[u8; BLOCK_SIZE],
    ) -> Result<(), BlkError> {
        // Check if the block is in the cache
        if let Some(block_cache) = self
            .0
            .iter_mut()
            .find(|b| b.meta.as_ref().is_some_and(|m| m.block == block))
        {
            // Hit - write the block directly to the device then
            // update the cache accordingly
            if let Err(blk_error) = write_block(block, buf) {
                // Write error - invalidate the cache
                block_cache.meta = None;
                return Err(blk_error);
            } else {
                // Write success - copy the same buf into cache
                block_cache.data.copy_from_slice(buf);
                return Ok(());
            }
        }
        // Not in cache - just write directly to device
        write_block(block, buf)?;
        Ok(())
    }

    // Whole-block read for bulk data (avoids cache)
    #[cfg(test)]
    pub(super) fn read_uncached(
        &mut self,
        block: u32,
        buf: &mut [u8; BLOCK_SIZE],
    ) -> Result<(), BlkError> {
        if let Some(block_cache) = self
            .0
            .iter()
            .find(|b| b.meta.as_ref().is_some_and(|m| m.block == block))
        {
            // Hit
            buf.copy_from_slice(&block_cache.data)
        } else {
            // Miss - read the block from the device
            read_block(block, buf)?;
        }
        Ok(())
    }
}

// Kernel-only QEMU tests (see the module doc in blockcache/tests.rs)
#[cfg(all(test, target_os = "none", feature = "test-fs"))]
mod tests {
    //! Block cache tests
    //!
    //! Kernel-only: these exercise the real virtio block device. Each test
    //! builds its own private `BlockCache`, so nothing here disturbs the
    //! mounted volume's cache. The hit/miss tests observe cache behaviour
    //! by tampering with the cached copy through the private fields: a
    //! subsequent read that returns the tampered byte was served from the
    //! cache; one that returns clean bytes went to the device.

    use super::*;
    use crate::drivers::virtio::blk::read_block;

    /// True if some valid entry is tagged with `block`
    fn holds(cache: &BlockCache, block: u32) -> bool {
        cache
            .0
            .iter()
            .any(|b| b.meta.as_ref().is_some_and(|m| m.block == block))
    }

    /// Number of valid entries
    fn valid_count(cache: &BlockCache) -> usize {
        cache.0.iter().filter(|b| b.meta.is_some()).count()
    }

    /// Index of the valid entry tagged with `block`
    fn index_of(cache: &BlockCache, block: u32) -> usize {
        cache
            .0
            .iter()
            .position(|b| b.meta.as_ref().is_some_and(|m| m.block == block))
            .expect("block should be cached")
    }

    #[test_case]
    fn read_hit_serves_cached_bytes_without_refetch() {
        let mut cache = BlockCache::new();
        let original = cache.read(0).unwrap()[0];
        // Tamper with the cached copy. A hit returns the tampered byte; a
        // refetch from the device would restore the original.
        let idx = index_of(&cache, 0);
        cache.0[idx].data[0] ^= 0xFF;
        let second = cache.read(0).unwrap()[0];
        assert_eq!(
            second,
            original ^ 0xFF,
            "second read must be served from the cache, not the device"
        );
    }

    #[test_case]
    fn fills_empty_slots_before_evicting() {
        let mut cache = BlockCache::new();
        for block in 0..BLOCK_CACHE_COUNT as u32 {
            cache.read(block).unwrap();
        }
        assert_eq!(valid_count(&cache), BLOCK_CACHE_COUNT);
        for block in 0..BLOCK_CACHE_COUNT as u32 {
            assert!(holds(&cache, block), "block {block} should still be cached");
        }
    }

    #[test_case]
    fn evicts_least_recently_used() {
        let mut cache = BlockCache::new();
        for block in 0..BLOCK_CACHE_COUNT as u32 {
            cache.read(block).unwrap();
        }
        // Touch block 0 so block 1 becomes the least recently used
        cache.read(0).unwrap();
        // One more distinct block must evict block 1, not block 0
        let extra = BLOCK_CACHE_COUNT as u32;
        cache.read(extra).unwrap();
        assert!(!holds(&cache, 1), "LRU block 1 should have been evicted");
        assert!(holds(&cache, 0), "recently touched block 0 must survive");
        assert!(holds(&cache, extra));
        assert_eq!(valid_count(&cache), BLOCK_CACHE_COUNT);
    }

    #[test_case]
    fn failed_fill_leaves_no_claim() {
        let mut cache = BlockCache::new();
        for block in 0..BLOCK_CACHE_COUNT as u32 {
            cache.read(block).unwrap();
        }
        // A block far past the device capacity must fail to fill
        let bogus = u32::MAX;
        assert!(cache.read(bogus).is_err(), "expected out-of-range error");
        // The failure must not leave a valid tag: not for the bogus block,
        // and not for the victim whose data may have been clobbered
        assert!(!holds(&cache, bogus));
        assert_eq!(
            valid_count(&cache),
            BLOCK_CACHE_COUNT - 1,
            "exactly the invalidated victim should be empty"
        );
    }

    #[test_case]
    fn modify_writes_through_and_write_uncached_updates_cache() {
        let mut cache = BlockCache::new();
        // Byte 3 is in the boot sector's OEM name — read by nothing after
        // mount. Flipped and restored below so the image ends unchanged.
        let original = *cache.read(0).unwrap();
        cache.modify(0, |buf| buf[3] ^= 0xAA).unwrap();
        // The device itself must hold the modification (read it raw)
        let mut fresh = [0u8; BLOCK_SIZE];
        read_block(0, &mut fresh).unwrap();
        assert_eq!(fresh[3], original[3] ^ 0xAA, "modify must write through");
        // Restore via write_uncached: device AND the cached copy must both
        // return to the original bytes
        cache.write_uncached(0, &original).unwrap();
        assert_eq!(
            cache.read(0).unwrap()[3],
            original[3],
            "write_uncached must update an already-cached block"
        );
        read_block(0, &mut fresh).unwrap();
        assert_eq!(fresh, original, "image must be restored byte-identical");
    }

    #[test_case]
    fn read_uncached_serves_hits_from_cache_without_populating_misses() {
        let mut cache = BlockCache::new();
        cache.read(1).unwrap();
        // Hit path: must come from the cache (tampered byte visible)
        let idx = index_of(&cache, 1);
        cache.0[idx].data[7] ^= 0x55;
        let mut buf = [0u8; BLOCK_SIZE];
        cache.read_uncached(1, &mut buf).unwrap();
        assert_eq!(
            buf[7], cache.0[idx].data[7],
            "read_uncached hit must be served from the cache"
        );
        // Miss path: reads the device but must not claim a slot
        cache.read_uncached(2, &mut buf).unwrap();
        assert!(
            !holds(&cache, 2),
            "read_uncached miss must not populate the cache"
        );
    }
}
