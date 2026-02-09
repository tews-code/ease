//! Virtio for EASE

#![allow(dead_code)]

use alloc::boxed::Box;
use core::mem;
use core::mem::offset_of;

mod queue;

use crate::hal::{BLOCK_SIZE, BlockDevice};
use crate::kernel::sync::SpinLock;
use crate::println;
use queue::*;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

pub struct VirtioBlk;

#[derive(Debug)]
pub enum VirtioBlkError {
    NotInitialized,   // Capacity or virtqueue not set up
    SectorOutOfRange, // Sector exceeds capacity
    DeviceError(u8),  // Device returned non-zero status
}

impl BlockDevice for VirtioBlk {
    type BlkError = VirtioBlkError;

    fn read_block(&self, block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), Self::BlkError> {
        read_disk(buf, block as u64)?;
        Ok(())
    }

    fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), Self::BlkError> {
        write_disk(buf, block as u64)?;
        Ok(())
    }

    fn block_count(&self) -> u32 {
        let guard = BLK_CAPACITY.lock();
        let Some(cap) = *guard else {
            return 0;
        };
        (cap / BLOCK_SIZE as u64) as u32
    }
}

// Virtio-blk request.
#[repr(C, packed)]
#[derive(Debug)]
struct VirtioBlkReq {
    req_type: u32,
    reserved: u32,
    sector: u64,
    data: [u8; 512],
    status: u8,
}

impl VirtioBlkReq {
    fn zeroed() -> Self {
        // SAFETY: VirtioBlkReq is a packed C struct with only integer/array fields.
        // All-zero bytes is a valid representation for this type.
        unsafe { core::mem::MaybeUninit::zeroed().assume_init() }
    }
}

static BLK_REQUEST_VQ: SpinLock<Option<Box<VirtioVirtq>>> = SpinLock::new(None);

static BLK_REQ: SpinLock<Option<Box<VirtioBlkReq>>> = SpinLock::new(None);

static BLK_CAPACITY: SpinLock<Option<u64>> = SpinLock::new(None);

#[allow(clippy::identity_op)]
pub fn virtio_blk_init() {
    if virtio_reg_read32(VIRTIO_REG_MAGIC) != 0x74726976 {
        panic!("virtio: invalid magic value");
    };
    if virtio_reg_read32(VIRTIO_REG_VERSION) != 1 {
        panic!("virtio: invalid version");
    };

    if virtio_reg_read32(VIRTIO_REG_DEVICE_ID) != VIRTIO_DEVICE_BLK {
        panic!("virtio: invalid version");
    };

    // 1. Reset the device
    virtio_reg_write32(VIRTIO_REG_DEVICE_STATUS, 0);
    // 2. Set the ACKNOWLEDGE status bit: the guest OS has noticed the device
    virtio_reg_fetch_and_or32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_ACK);
    // 3. Set the DRIVER status bit.
    virtio_reg_fetch_and_or32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER);
    // 5. Set the FEATURES_OK status bit
    virtio_reg_fetch_and_or32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_FEAT_OK);
    // 7. Perform device-specific setup, including discovery of virtqueues for the device
    *BLK_REQUEST_VQ.lock() = Some(virtq_init(0));
    // 8. Set the DRIVER_OK status bit.
    virtio_reg_write32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER_OK);

    // Get the disk capacity.
    *BLK_CAPACITY.lock() =
        Some(virtio_reg_read64(VIRTIO_REG_DEVICE_CONFIG + 0) * BLOCK_SIZE as u64);

    match *BLK_CAPACITY.lock() {
        Some(capacity) => println!("virtio-blk: capacity is {} bytes", capacity),
        None => println!("virtio-blk: capacity is not initialized yet"),
    }

    // Allocate a region to store requests to the device.
    *BLK_REQ.lock() = Some(Box::new(VirtioBlkReq::zeroed()));
}

// Helper function to set up virtio queues
fn virtio_queue(
    blk_req_paddr: usize,
    vq: &mut VirtioVirtq,
    flags: u32,
) -> Result<(), VirtioBlkError> {
    // Descriptor 0: request header
    vq.descs[0] = VirtqDesc {
        addr: blk_req_paddr as u64,
        len: (mem::size_of::<u32>() * 2 + mem::size_of::<u64>()) as u32,
        flags: VIRTQ_DESC_F_NEXT as u16,
        next: 1,
    };

    // Descriptor 1: data buffer
    vq.descs[1] = VirtqDesc {
        addr: (blk_req_paddr + offset_of!(VirtioBlkReq, data)) as u64,
        len: BLOCK_SIZE as u32,
        flags: (VIRTQ_DESC_F_NEXT | flags) as u16,
        next: 2,
    };

    // Descriptor 2: status byte
    vq.descs[2] = VirtqDesc {
        addr: (blk_req_paddr + offset_of!(VirtioBlkReq, status)) as u64,
        len: mem::size_of::<u8>() as u32,
        flags: VIRTQ_DESC_F_WRITE as u16,
        next: 0,
    };

    // Notify the device that there is a new request.
    virtq_kick(vq, 0);

    // Wait until the device finishes processing.
    while virtq_is_busy(vq) {
        core::hint::spin_loop();
    }

    Ok(())
}

// Writes to virtio-blk device.
pub fn write_disk(buf: &[u8], sector: u64) -> Result<(), VirtioBlkError> {
    let blk_capacity = {
        let guard = BLK_CAPACITY.lock();
        let Some(cap) = *guard else {
            return Err(VirtioBlkError::NotInitialized);
        };
        cap
    };

    if sector >= (blk_capacity / BLOCK_SIZE as u64) {
        println!(
            "virtio: tried to read/write sector={}, but capacity is {}",
            sector,
            blk_capacity / BLOCK_SIZE as u64
        );
        return Err(VirtioBlkError::SectorOutOfRange);
    };

    let mut br_guard = BLK_REQ.lock();
    let Some(br) = br_guard.as_mut() else {
        return Err(VirtioBlkError::NotInitialized);
    };

    br.sector = sector;
    br.req_type = VIRTIO_BLK_T_OUT;
    br.data.copy_from_slice(buf);

    // Construct the virtqueue descriptors (using 3 descriptors).
    let mut vq_guard = BLK_REQUEST_VQ.lock();
    let Some(vq) = vq_guard.as_mut() else {
        return Err(VirtioBlkError::NotInitialized);
    };

    let blk_req_paddr = &**br as *const VirtioBlkReq as usize; // Double deference to get address from heap, not of the Box

    virtio_queue(blk_req_paddr, vq, 0)?;

    // virtio-blk: If a non-zero value is returned, it's an error.
    if br.status != 0 {
        println!(
            "virtio: warn: failed to read/write sector={} status={}",
            sector, br.status
        );
        return Err(VirtioBlkError::DeviceError(br.status));
    }
    Ok(())
}

// Reads from virtio-blk device.
pub fn read_disk(buf: &mut [u8], sector: u64) -> Result<(), VirtioBlkError> {
    let blk_capacity = {
        let guard = BLK_CAPACITY.lock();
        let Some(cap) = *guard else {
            return Err(VirtioBlkError::NotInitialized);
        };
        cap
    };

    if sector >= (blk_capacity / BLOCK_SIZE as u64) {
        println!(
            "virtio: tried to read/write sector={}, but capacity is {}",
            sector,
            blk_capacity / BLOCK_SIZE as u64
        );
        return Err(VirtioBlkError::SectorOutOfRange);
    };

    let mut br_guard = BLK_REQ.lock();
    let Some(br) = br_guard.as_mut() else {
        return Err(VirtioBlkError::NotInitialized);
    };

    br.sector = sector;
    br.req_type = VIRTIO_BLK_T_IN;

    // Construct the virtqueue descriptors (using 3 descriptors).
    let mut vq_guard = BLK_REQUEST_VQ.lock();
    let Some(vq) = vq_guard.as_mut() else {
        return Err(VirtioBlkError::NotInitialized);
    };

    let blk_req_paddr = &**br as *const VirtioBlkReq as usize; // Double deference to get address from heap, not of the Box

    virtio_queue(blk_req_paddr, vq, VIRTQ_DESC_F_WRITE)?;

    // virtio-blk: If a non-zero value is returned, it's an error.
    if br.status != 0 {
        println!(
            "virtio: warn: failed to read/write sector={} status={}",
            sector, br.status
        );
        return Err(VirtioBlkError::DeviceError(br.status));
    }

    // For read operations, copy the data into the buffer.
    buf.copy_from_slice(&br.data);

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test_case]
    fn device_status_after_init() {
        // virtio_blk_init() already ran in main(); verify DRIVER_OK is set
        let status = virtio_reg_read32(VIRTIO_REG_DEVICE_STATUS);
        assert_eq!(status & VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_DRIVER_OK);
    }

    #[test_case]
    fn capacity_matches_disk_image() {
        // 16MB disk image = 32768 sectors of 512 bytes
        let capacity = BLK_CAPACITY.lock().expect("capacity should be initialized");
        assert_eq!(capacity, 32768 * BLOCK_SIZE as u64);
    }

    #[test_case]
    fn read_block_zero_fat16_signature() {
        // Block 0 of a FAT16 volume has "FAT16" at byte offset 54
        let mut buf = [0u8; BLOCK_SIZE];
        read_disk(&mut buf, 0).unwrap();
        assert_eq!(&buf[54..59], b"FAT16");
    }

    #[test_case]
    fn write_and_read_back() {
        let s = "hello from kernel!!!";
        let mut buf = [0u8; BLOCK_SIZE];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        write_disk(&buf, 1).unwrap();

        let mut buf2 = [0u8; BLOCK_SIZE];
        read_disk(&mut buf2, 1).unwrap();
        assert_eq!(&buf2[..s.len()], s.as_bytes());
    }
}

#[cfg(test)]
mod baselines {
    // Measured on QEMU virt, set with wide margin for variance
    //   READ_BLOCK:     2,000,000  (measured ~37,000-1,600,000)
    //   WRITE_BLOCK:     5,000,000  (measured ~1,300,000-3,400,000)
    //   WRITE_READ_BLOCK: 3,000,000  (measured ~308,000-1,600,000)
    pub const READ_BLOCK: u64 = 2_000_000;
    pub const WRITE_BLOCK: u64 = 5_000_000;
    pub const WRITE_READ_BLOCK: u64 = 3_000_000;
}

#[cfg(test)]
mod benchmarks {
    use super::baselines;
    use super::*;
    use crate::bench;

    const ITERATIONS: u32 = 10;

    #[test_case]
    fn regression_read_block() {
        crate::printdln!("\n=== Virtio Block Regression Checks ===");
        bench::check(
            "read_disk(sector 0)",
            baselines::READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                read_disk(&mut buf, 0).unwrap();
            },
        );
    }

    #[test_case]
    fn regression_write_block() {
        let buf = [0u8; BLOCK_SIZE];
        bench::check(
            "write_disk(sector 1)",
            baselines::WRITE_BLOCK,
            ITERATIONS,
            || {
                write_disk(&buf, 1).unwrap();
            },
        );
    }

    #[test_case]
    fn regression_write_read_block() {
        bench::check(
            "write+read(sector 1)",
            baselines::WRITE_READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                write_disk(&buf, 1).unwrap();
                read_disk(&mut buf, 1).unwrap();
            },
        );
    }
}
