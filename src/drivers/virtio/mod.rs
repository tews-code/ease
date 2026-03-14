//! QEMU virtio block device

#![allow(dead_code)]

use alloc::boxed::Box;

use core::mem::{self, MaybeUninit};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::mmio;
use crate::board::virtio_blk;
use crate::hal::BLOCK_SIZE;
use crate::kernel::sync::IrqSpinLock;
use crate::kernel::timer::ticks_ms;

mod queue;

use queue::{
    VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtioVirtq, VirtqDesc, virtq_init, virtq_kick,
};

// Virtio MMIO register offsets
const VIRTIO_REG_MAGIC: usize = 0x00;
const VIRTIO_REG_VERSION: usize = 0x04;
const VIRTIO_REG_DEVICE_ID: usize = 0x08;
const VIRTIO_REG_INTERRUPT_STATUS: usize = 0x60;
const VIRTIO_REG_INTERRUPT_ACK: usize = 0x64;
const VIRTIO_REG_DEVICE_STATUS: usize = 0x70;
const VIRTIO_REG_DEVICE_CONFIG: usize = 0x100;

// Virtio device status flags
const VIRTIO_STATUS_ACK: u32 = 1;
const VIRTIO_STATUS_DRIVER: u32 = 2;
const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
const VIRTIO_STATUS_FEAT_OK: u32 = 8;

// Virtio magic and version
const VIRTIO_MAGIC: u32 = 0x74726976;
const VIRTIO_VERSION: u32 = 1;
const VIRTIO_DEVICE_ID: u32 = 2;

// Virtio block errors
#[derive(Debug)]
pub enum BlkError {
    SectorOutOfRange,
    DeviceError(u8),
    Timeout,
}

// Virtio block direction
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

// Virtio-blk request.
#[repr(C, align(4))]
struct VirtioBlkReq {
    req_type: u32,
    reserved: u32,
    sector: u64,
    data: [u8; BLOCK_SIZE],
    status: u8,
}

// Virtio Block Device
//
// DMA design: QEMU holds permanent pointers to the virtqueue (via QUEUE_PFN)
// and per-request pointers to req (via descriptors). The vq is heap-allocated
// (Box) so its address is stable from the moment QEMU receives it. The req is
// inline in the static and its address is recomputed on each queue_submit, so
// it does not need a stable address across requests. Pin is not used — the
// static is never moved in practice, and the Box provides address stability
// for the virtqueue which requires it.
pub struct VirtioBlkDev {
    capacity: u64,
    req: VirtioBlkReq,    // DMA target — device reads and writes this
    vq: Box<VirtioVirtq>, // device writes to the used ring
}

pub type Blk = VirtioBlkDev;

// SAFETY: All access is guarded by IrqSpinLock (interrupts disabled while held).
unsafe impl Sync for VirtioBlkDev {}

// Set the bits in `value` to the 32 bit MMIO register at `base` + `offset`
//
// This does not disable interrupts
fn set_bits32(base: usize, offset: usize, value: u32) {
    mmio::write32(base, offset, mmio::read32(base, offset) | value);
}

impl VirtioBlkDev {
    fn new() -> Self {
        assert_eq!(
            mmio::read32(virtio_blk::BASE, VIRTIO_REG_MAGIC),
            VIRTIO_MAGIC
        );
        assert_eq!(
            mmio::read32(virtio_blk::BASE, VIRTIO_REG_VERSION),
            VIRTIO_VERSION
        );
        assert_eq!(
            mmio::read32(virtio_blk::BASE, VIRTIO_REG_DEVICE_ID),
            VIRTIO_DEVICE_ID
        );

        // 1. Reset the device
        mmio::write32(virtio_blk::BASE, VIRTIO_REG_DEVICE_STATUS, 0);
        // 2. Set the ACKNOWLEDGE status bit: the guest OS has noticed the device
        set_bits32(
            virtio_blk::BASE,
            VIRTIO_REG_DEVICE_STATUS,
            VIRTIO_STATUS_ACK,
        );
        // 3. Set the DRIVER status bit.
        set_bits32(
            virtio_blk::BASE,
            VIRTIO_REG_DEVICE_STATUS,
            VIRTIO_STATUS_DRIVER,
        );
        // 5. Set the FEATURES_OK status bit
        set_bits32(
            virtio_blk::BASE,
            VIRTIO_REG_DEVICE_STATUS,
            VIRTIO_STATUS_FEAT_OK,
        );
        // 7. Perform device-specific setup, including discovery of virtqueues for the device
        let vq = virtq_init(virtio_blk::BASE, 0);
        // 8. Set the DRIVER_OK status bit.
        set_bits32(
            virtio_blk::BASE,
            VIRTIO_REG_DEVICE_STATUS,
            VIRTIO_STATUS_DRIVER_OK,
        );

        // Get the disk capacity.
        let cap_lo = mmio::read32(virtio_blk::BASE, VIRTIO_REG_DEVICE_CONFIG) as u64;
        let cap_hi = mmio::read32(virtio_blk::BASE, VIRTIO_REG_DEVICE_CONFIG + 4) as u64;
        let capacity = (cap_hi << 32 | cap_lo) * BLOCK_SIZE as u64;

        crate::println!("virtio-blk: capacity is {} bytes", capacity);

        // Allocate a region to store requests to the device.
        // Safety: VirtioBlkReq contains only integer types and byte arrays.
        // All-zero bytes is a valid representation for all fields.
        let req: VirtioBlkReq = unsafe { MaybeUninit::zeroed().assume_init() };

        Self { capacity, req, vq }
    }

    // Set up descriptors and kick the virtio queue
    fn queue_submit(req: &mut VirtioBlkReq, vq: &mut VirtioVirtq, flags: u32) {
        let addr = req as *const VirtioBlkReq as usize;

        // Descriptor 0: request header
        unsafe {
            write_volatile(
                &raw mut vq.descs[0],
                VirtqDesc {
                    addr: addr as u64,
                    len: (mem::size_of::<u32>() * 2 + mem::size_of::<u64>()) as u32,
                    flags: VIRTQ_DESC_F_NEXT as u16,
                    next: 1,
                },
            )
        };

        // Descriptor 1: data buffer
        unsafe {
            write_volatile(
                &raw mut vq.descs[1],
                VirtqDesc {
                    addr: (addr + mem::offset_of!(VirtioBlkReq, data)) as u64,
                    len: BLOCK_SIZE as u32,
                    flags: (VIRTQ_DESC_F_NEXT | flags) as u16,
                    next: 2,
                },
            )
        };

        // Descriptor 2: status byte
        unsafe {
            write_volatile(
                &raw mut vq.descs[2],
                VirtqDesc {
                    addr: (addr + mem::offset_of!(VirtioBlkReq, status)) as u64,
                    len: mem::size_of::<u8>() as u32,
                    flags: VIRTQ_DESC_F_WRITE as u16,
                    next: 0,
                },
            )
        };

        // Notify the device that there is a new request.
        virtq_kick(virtio_blk::BASE, vq, 0);
    }

    pub fn block_count(&self) -> u32 {
        (self.capacity / BLOCK_SIZE as u64) as u32
    }

    // Bounds check, clear VIRTIO_COMPLETE, set up sector/type, call queue_submit
    fn submit_read(&mut self, block: u32) -> Result<(), BlkError> {
        if block as u64 >= self.capacity / BLOCK_SIZE as u64 {
            return Err(BlkError::SectorOutOfRange);
        }
        VIRTIO_COMPLETE.store(false, Ordering::Relaxed);

        unsafe { write_volatile(&raw mut self.req.sector, block as u64) };
        unsafe { write_volatile(&raw mut self.req.req_type, VIRTIO_BLK_T_IN) };
        Self::queue_submit(&mut self.req, &mut self.vq, VIRTQ_DESC_F_WRITE);
        Ok(())
    }

    // Check status byte, copy data out
    fn finish_read(&mut self, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlkError> {
        let status = unsafe { read_volatile(&raw const self.req.status) };
        if status != 0 {
            return Err(BlkError::DeviceError(status));
        }

        let data = unsafe { read_volatile(&raw const self.req.data) };
        buf.copy_from_slice(&data);
        Ok(())
    }

    // Bounds check, clear flag, copy data in, set up sector/type, queue_submit
    fn submit_write(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlkError> {
        if block as u64 >= self.capacity / BLOCK_SIZE as u64 {
            return Err(BlkError::SectorOutOfRange);
        }

        VIRTIO_COMPLETE.store(false, Ordering::Relaxed);

        unsafe { write_volatile(&raw mut self.req.sector, block as u64) };
        unsafe { write_volatile(&raw mut self.req.req_type, VIRTIO_BLK_T_OUT) };
        unsafe { write_volatile(&raw mut self.req.data, *buf) };
        Self::queue_submit(&mut self.req, &mut self.vq, 0);
        Ok(())
    }

    // Just check status byte
    fn finish_write(&mut self) -> Result<(), BlkError> {
        let status = unsafe { read_volatile(&raw const self.req.status) };
        if status != 0 {
            return Err(BlkError::DeviceError(status));
        }
        Ok(())
    }

    pub fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlkError> {
        self.submit_read(block)?;

        let expected = self.vq.last_used_index;
        while self.vq.read_used_index() != expected {
            crate::hal::wait_for_interrupt();
        }

        self.finish_read(buf)
    }

    fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlkError> {
        self.submit_write(block, buf)?;

        let expected = self.vq.last_used_index;
        while self.vq.read_used_index() != expected {
            crate::hal::wait_for_interrupt();
        }

        self.finish_write()
    }
}

const IO_TIMEOUT_MS: usize = 1_000;

pub fn read_block_async(block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlkError> {
    assert!(
        !IO_IN_PROGRESS.swap(true, Ordering::Relaxed),
        "IO should not already be in progress"
    );
    let result = with_blk_dev(|blk| blk.submit_read(block));
    if result.is_err() {
        IO_IN_PROGRESS.store(false, Ordering::Relaxed);
        return result;
    }

    let start = ticks_ms();
    while !VIRTIO_COMPLETE.load(Ordering::Acquire) {
        if ticks_ms().wrapping_sub(start) >= IO_TIMEOUT_MS {
            IO_IN_PROGRESS.store(false, Ordering::Relaxed);
            return Err(BlkError::Timeout);
        }
        crate::hal::wait_for_interrupt();
    }

    let result = with_blk_dev(|blk| blk.finish_read(buf));
    IO_IN_PROGRESS.store(false, Ordering::Relaxed);
    result
}

pub fn write_block_async(block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlkError> {
    assert!(
        !IO_IN_PROGRESS.swap(true, Ordering::Relaxed),
        "IO should not already be in progress"
    );
    let result = with_blk_dev(|blk| blk.submit_write(block, buf));
    if result.is_err() {
        IO_IN_PROGRESS.store(false, Ordering::Relaxed);
        return result;
    }

    let start = ticks_ms();
    while !VIRTIO_COMPLETE.load(Ordering::Acquire) {
        if ticks_ms().wrapping_sub(start) >= IO_TIMEOUT_MS {
            IO_IN_PROGRESS.store(false, Ordering::Relaxed);
            return Err(BlkError::Timeout);
        }
        crate::hal::wait_for_interrupt();
    }

    let result = with_blk_dev(|blk| blk.finish_write());
    IO_IN_PROGRESS.store(false, Ordering::Relaxed);
    result
}

static BLK_DEV: IrqSpinLock<Option<VirtioBlkDev>> = IrqSpinLock::new(None);

/// Initialise the virtio block device. Must be called before any block I/O.
pub fn virtio_blk_init() {
    *BLK_DEV.lock() = Some(VirtioBlkDev::new());
}

pub fn with_blk_dev<F, R>(f: F) -> R
where
    F: FnOnce(&mut VirtioBlkDev) -> R,
{
    let mut guard = BLK_DEV.lock();
    let blk_dev = guard.as_mut().expect("virtio should be initialised");
    f(blk_dev)
}

// Flag tracks completion for interrupt-driven IO
static VIRTIO_COMPLETE: AtomicBool = AtomicBool::new(false);

// Flag tracks when IO is in progress (interrupts disabled)
static IO_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn handle_virtio_interrupt() {
    let status = mmio::read32(virtio_blk::BASE, VIRTIO_REG_INTERRUPT_STATUS);
    mmio::write32(virtio_blk::BASE, VIRTIO_REG_INTERRUPT_ACK, status);
    VIRTIO_COMPLETE.store(true, Ordering::Release);
}

#[cfg(test)]
mod test {
    use super::*;

    #[test_case]
    fn device_status_after_init() {
        // virtio_blk_init() already ran in main(); verify DRIVER_OK is set
        let status = mmio::read32(virtio_blk::BASE, VIRTIO_REG_DEVICE_STATUS);
        assert_eq!(status & VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_DRIVER_OK);
    }

    #[test_case]
    fn capacity_matches_disk_image() {
        with_blk_dev(|blk| {
            assert_eq!(blk.block_count(), 32768);
        });
    }

    #[test_case]
    fn read_block_zero_fat16_signature() {
        // Block 0 of a FAT16 volume has "FAT16" at byte offset 54
        let mut buf = [0u8; BLOCK_SIZE];
        with_blk_dev(|blk| blk.read_block(0, &mut buf).unwrap());
        assert_eq!(&buf[54..59], b"FAT16");
    }

    #[test_case]
    fn write_and_read_back() {
        let s = "hello from kernel!!!";
        let mut buf = [0u8; BLOCK_SIZE];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        with_blk_dev(|blk| blk.write_block(1, &buf).unwrap());

        let mut buf2 = [0u8; BLOCK_SIZE];
        with_blk_dev(|blk| blk.read_block(1, &mut buf2).unwrap());
        assert_eq!(&buf2[..s.len()], s.as_bytes());
    }
}

#[cfg(test)]
mod baselines {
    // Measured on QEMU virt, set with wide margin for variance
    //   READ_BLOCK:       200,000  (measured ~49,000)
    //   WRITE_BLOCK:    1,000,000  (measured ~277,000)
    //   WRITE_READ_BLOCK: 1,000,000  (measured ~246,000)
    pub const READ_BLOCK: u64 = 4_000_000;
    pub const WRITE_BLOCK: u64 = 4_000_000;
    pub const WRITE_READ_BLOCK: u64 = 4_000_000;
}

#[cfg(test)]
mod benchmarks {
    use super::baselines;
    use super::*;
    use crate::bench;

    const ITERATIONS: u32 = 10;

    #[test_case]
    fn regression_read_block() {
        crate::println!("\n=== Virtio Block Regression Checks ===");
        bench::check(
            "read_block(sector 0)",
            baselines::READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                with_blk_dev(|blk| blk.read_block(0, &mut buf).unwrap());
            },
        );
    }

    #[test_case]
    fn regression_write_block() {
        bench::check(
            "write_block(sector 1)",
            baselines::WRITE_BLOCK,
            ITERATIONS,
            || {
                let buf = [0u8; BLOCK_SIZE];
                with_blk_dev(|blk| blk.write_block(1, &buf).unwrap());
            },
        );
    }

    #[test_case]
    fn regression_write_read_block() {
        bench::check(
            "write+read_block(sector 1)",
            baselines::WRITE_READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                with_blk_dev(|blk| {
                    blk.write_block(1, &buf).unwrap();
                    blk.read_block(1, &mut buf).unwrap();
                });
            },
        );
    }
}
