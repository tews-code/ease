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
mod tests;
