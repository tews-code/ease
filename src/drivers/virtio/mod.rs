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

// Virtio-blk request.
#[repr(C, packed)]
#[derive(Debug)]
struct VirtioBlkReq {
    req_type: u32,
    reserved: u32,
    sector: u64,
    data: [u8; BLOCK_SIZE],
    status: u8,
}

struct VirtioBlkState {
    capacity: u64,
    req: VirtioBlkReq,
    vq: Box<VirtioVirtq>,
}

impl VirtioBlkState {
    #[allow(clippy::identity_op)]
    fn new() -> Self {
        if virtio_reg_read32(VIRTIO_REG_MAGIC) != 0x74726976 {
            panic!("virtio: invalid magic value");
        }
        if virtio_reg_read32(VIRTIO_REG_VERSION) != 1 {
            panic!("virtio: invalid version");
        }
        if virtio_reg_read32(VIRTIO_REG_DEVICE_ID) != VIRTIO_DEVICE_BLK {
            panic!("virtio: invalid version");
        }

        // 1. Reset the device
        virtio_reg_write32(VIRTIO_REG_DEVICE_STATUS, 0);
        // 2. Set the ACKNOWLEDGE status bit: the guest OS has noticed the device
        virtio_reg_fetch_and_or32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_ACK);
        // 3. Set the DRIVER status bit.
        virtio_reg_fetch_and_or32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER);
        // 5. Set the FEATURES_OK status bit
        virtio_reg_fetch_and_or32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_FEAT_OK);
        // 7. Perform device-specific setup, including discovery of virtqueues for the device
        let vq = virtq_init(0);
        // 8. Set the DRIVER_OK status bit.
        virtio_reg_write32(VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER_OK);

        // Get the disk capacity.
        let capacity = virtio_reg_read64(VIRTIO_REG_DEVICE_CONFIG + 0) * BLOCK_SIZE as u64;

        println!("virtio-blk: capacity is {} bytes", capacity);

        // Allocate a region to store requests to the device.
        let req: VirtioBlkReq = unsafe { core::mem::zeroed() };

        Self { capacity, req, vq }
    }
}

static BLK: SpinLock<Option<VirtioBlkState>> = SpinLock::new(None);

pub fn virtio_blk_init() {
    *BLK.lock() = Some(VirtioBlkState::new());
}

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
        disk_op(block as u64, BlkOp::Read, buf)
    }

    fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), Self::BlkError> {
        let mut tmp = *buf;
        disk_op(block as u64, BlkOp::Write, &mut tmp)
    }

    fn block_count(&self) -> Option<u32> {
        let blk_guard = BLK.lock();
        let blk = blk_guard.as_ref()?;
        Some((blk.capacity / BLOCK_SIZE as u64) as u32)
    }
}

// Helper function to set up virtio queues
fn virtio_queue(blk_req_addr: usize, vq: &mut VirtioVirtq, flags: u32) {
    // Descriptor 0: request header
    vq.descs[0] = VirtqDesc {
        addr: blk_req_addr as u64,
        len: (mem::size_of::<u32>() * 2 + mem::size_of::<u64>()) as u32,
        flags: VIRTQ_DESC_F_NEXT as u16,
        next: 1,
    };

    // Descriptor 1: data buffer
    vq.descs[1] = VirtqDesc {
        addr: (blk_req_addr + offset_of!(VirtioBlkReq, data)) as u64,
        len: BLOCK_SIZE as u32,
        flags: (VIRTQ_DESC_F_NEXT | flags) as u16,
        next: 2,
    };

    // Descriptor 2: status byte
    vq.descs[2] = VirtqDesc {
        addr: (blk_req_addr + offset_of!(VirtioBlkReq, status)) as u64,
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
}

enum BlkOp {
    Read,
    Write,
}

// Helper function for block reads/writes
fn disk_op(sector: u64, op: BlkOp, data: &mut [u8; BLOCK_SIZE]) -> Result<(), VirtioBlkError> {
    let mut blk_guard = BLK.lock();
    let blk = blk_guard.as_mut().ok_or(VirtioBlkError::NotInitialized)?;

    if sector >= blk.capacity / BLOCK_SIZE as u64 {
        return Err(VirtioBlkError::SectorOutOfRange);
    }

    blk.req.sector = sector;
    let flags = match op {
        BlkOp::Write => {
            blk.req.req_type = VIRTIO_BLK_T_OUT;
            blk.req.data.copy_from_slice(data);
            0
        }
        BlkOp::Read => {
            blk.req.req_type = VIRTIO_BLK_T_IN;
            VIRTQ_DESC_F_WRITE
        }
    };

    let addr = &blk.req as *const VirtioBlkReq as usize;
    virtio_queue(addr, blk.vq.as_mut(), flags);

    if blk.req.status != 0 {
        return Err(VirtioBlkError::DeviceError(blk.req.status));
    }

    if matches!(op, BlkOp::Read) {
        data.copy_from_slice(&blk.req.data);
    }
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
        let capacity = BLK
            .lock()
            .as_ref()
            .expect("capacity should be initialized")
            .capacity;
        assert_eq!(capacity, 32768 * BLOCK_SIZE as u64);
    }

    #[test_case]
    fn read_block_zero_fat16_signature() {
        // Block 0 of a FAT16 volume has "FAT16" at byte offset 54
        let mut buf = [0u8; BLOCK_SIZE];
        disk_op(0, BlkOp::Read, &mut buf).unwrap();
        assert_eq!(&buf[54..59], b"FAT16");
    }

    #[test_case]
    fn write_and_read_back() {
        let s = "hello from kernel!!!";
        let mut buf = [0u8; BLOCK_SIZE];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        disk_op(1, BlkOp::Write, &mut buf).unwrap();

        let mut buf2 = [0u8; BLOCK_SIZE];
        disk_op(1, BlkOp::Read, &mut buf2).unwrap();
        assert_eq!(&buf2[..s.len()], s.as_bytes());
    }
}

#[cfg(test)]
mod baselines {
    // Measured on QEMU virt, set with wide margin for variance
    //   READ_BLOCK:       200,000  (measured ~49,000)
    //   WRITE_BLOCK:    1,000,000  (measured ~277,000)
    //   WRITE_READ_BLOCK: 1,000,000  (measured ~246,000)
    pub const READ_BLOCK: u64 = 1_200_000;
    pub const WRITE_BLOCK: u64 = 2_000_000;
    pub const WRITE_READ_BLOCK: u64 = 2_000_000;
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
            "disk_op(Read, sector 0)",
            baselines::READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                disk_op(0, BlkOp::Read, &mut buf).unwrap();
            },
        );
    }

    #[test_case]
    fn regression_write_block() {
        bench::check(
            "disk_op(Write, sector 1)",
            baselines::WRITE_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                disk_op(1, BlkOp::Write, &mut buf).unwrap();
            },
        );
    }

    #[test_case]
    fn regression_write_read_block() {
        bench::check(
            "disk_op(Write+Read, sector 1)",
            baselines::WRITE_READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; BLOCK_SIZE];
                disk_op(1, BlkOp::Write, &mut buf).unwrap();
                disk_op(1, BlkOp::Read, &mut buf).unwrap();
            },
        );
    }
}
