//! Virtio block device

use alloc::boxed::Box;
use core::mem::{self, MaybeUninit};
use core::ptr::{read_volatile, write_volatile};

use super::queue::{
    VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtioVirtq, VirtqDesc, virtq_init, virtq_kick,
};
use super::{VIRTIO_COMPLETE, check_virtio, reset_and_handshake, set_driver_ok};
use crate::arch::mmio;
use crate::board::virtio::blk;
use crate::kernel::sync::IrqSpinLock;

// Block device ID
const VIRTIO_DEVICE_ID: u32 = 2;
// Device queue index
const REQUESTQ: usize = 0;
// Virtio direction
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

static BLK_DEV: IrqSpinLock<Option<VirtioBlkDev>> = IrqSpinLock::new(None);

// Virtio block errors
#[expect(dead_code)]
#[derive(Debug)]
pub enum BlkError {
    SectorOutOfRange,
    DeviceError(u8),
    Timeout,
}

// Virtio-blk request.
#[repr(C, align(4))]
struct VirtioBlkReq {
    req_type: u32,
    reserved: u32,
    sector: u64,
    data: [u8; blk::BLOCK_SIZE],
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
    req: VirtioBlkReq,               // DMA target — device reads and writes this
    pub(super) vq: Box<VirtioVirtq>, // device writes to the used ring
}

// SAFETY: All access is guarded by IrqSpinLock (interrupts disabled while held).
unsafe impl Sync for VirtioBlkDev {}

impl VirtioBlkDev {
    pub(crate) fn new() -> Self {
        check_virtio(blk::BASE, VIRTIO_DEVICE_ID);
        let vq = Self::reset();
        // Get the disk capacity.
        let cap_lo = mmio::read32(blk::BASE, super::VIRTIO_REG_DEVICE_CONFIG) as u64;
        let cap_hi = mmio::read32(blk::BASE, super::VIRTIO_REG_DEVICE_CONFIG + 4) as u64;
        let capacity = (cap_hi << 32 | cap_lo) * blk::BLOCK_SIZE as u64;

        crate::println!("virtio-blk: capacity is {} bytes", capacity);

        // Allocate a region to store requests to the device.
        // Safety: VirtioBlkReq contains only integer types and byte arrays.
        // All-zero bytes is a valid representation for all fields.
        let req: VirtioBlkReq = unsafe { MaybeUninit::zeroed().assume_init() };

        Self { capacity, req, vq }
    }

    pub(super) fn reset() -> Box<VirtioVirtq> {
        reset_and_handshake(blk::BASE);
        // 7. Perform device-specific setup, including discovery of virtqueues for the device
        let vq = virtq_init(blk::BASE, REQUESTQ);
        set_driver_ok(blk::BASE);
        vq
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
                    len: blk::BLOCK_SIZE as u32,
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
        virtq_kick(blk::BASE, vq, 0);
    }

    pub fn block_count(&self) -> u32 {
        (self.capacity / blk::BLOCK_SIZE as u64) as u32
    }

    // Bounds check, clear VIRTIO_COMPLETE, set up sector/type, call queue_submit
    pub(super) fn submit_read(&mut self, block: u32) -> Result<(), BlkError> {
        //  Safe in Virtio's use because IO_IN_PROGRESS serialises I/O.
        unsafe {
            VIRTIO_COMPLETE.reset();
        }
        if block as u64 >= self.capacity / blk::BLOCK_SIZE as u64 {
            return Err(BlkError::SectorOutOfRange);
        }
        unsafe { write_volatile(&raw mut self.req.sector, block as u64) };
        unsafe { write_volatile(&raw mut self.req.req_type, VIRTIO_BLK_T_IN) };
        Self::queue_submit(&mut self.req, &mut self.vq, VIRTQ_DESC_F_WRITE);
        Ok(())
    }

    // Check status byte, copy data out
    pub(super) fn finish_read(&mut self, buf: &mut [u8; blk::BLOCK_SIZE]) -> Result<(), BlkError> {
        let status = unsafe { read_volatile(&raw const self.req.status) };
        if status != 0 {
            return Err(BlkError::DeviceError(status));
        }
        let data = unsafe { read_volatile(&raw const self.req.data) };
        buf.copy_from_slice(&data);
        Ok(())
    }

    // Bounds check, clear flag, copy data in, set up sector/type, queue_submit
    pub(super) fn submit_write(
        &mut self,
        block: u32,
        buf: &[u8; blk::BLOCK_SIZE],
    ) -> Result<(), BlkError> {
        //  Safe in Virtio's use because IO_IN_PROGRESS serialises I/O.
        unsafe {
            VIRTIO_COMPLETE.reset();
        }
        if block as u64 >= self.capacity / blk::BLOCK_SIZE as u64 {
            return Err(BlkError::SectorOutOfRange);
        }
        unsafe { write_volatile(&raw mut self.req.sector, block as u64) };
        unsafe { write_volatile(&raw mut self.req.req_type, VIRTIO_BLK_T_OUT) };
        unsafe { write_volatile(&raw mut self.req.data, *buf) };
        Self::queue_submit(&mut self.req, &mut self.vq, 0);
        Ok(())
    }

    // Just check status byte
    pub(super) fn finish_write(&mut self) -> Result<(), BlkError> {
        let status = unsafe { read_volatile(&raw const self.req.status) };
        if status != 0 {
            return Err(BlkError::DeviceError(status));
        }
        Ok(())
    }
}

/// Initialise the virtio block device. Must be called before any block I/O.
pub fn virtio_blk_init() {
    let mut blk_dev = BLK_DEV.lock();
    assert!(
        blk_dev.is_none(),
        "cannot initialise block device more than once"
    );
    // Enable the block device
    *blk_dev = Some(VirtioBlkDev::new());
}

pub fn with_blk_dev<F, R>(f: F) -> R
where
    F: FnOnce(&mut VirtioBlkDev) -> R,
{
    let mut guard = BLK_DEV.lock();
    let blk_dev = guard.as_mut().expect("virtio should be initialised");
    f(blk_dev)
}
