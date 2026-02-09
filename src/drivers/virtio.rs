//! Virtio for EASE

#![allow(dead_code)]

use core::mem;
use core::mem::offset_of;
use core::ptr;

use alloc::boxed::Box;

use crate::kernel::sync::SpinLock;
use crate::println;

pub const SECTOR_SIZE: usize = 512;
const VIRTQ_ENTRY_NUM: usize = 16;
const VIRTIO_DEVICE_BLK: u32 = 2;
pub const VIRTIO_BLK_PADDR: u32 = 0x10001000;
const VIRTIO_REG_MAGIC: u32 = 0x00;
const VIRTIO_REG_VERSION: u32 = 0x04;
const VIRTIO_REG_DEVICE_ID: u32 = 0x08;
const VIRTIO_REG_QUEUE_SEL: u32 = 0x30;
#[expect(dead_code)]
const VIRTIO_REG_QUEUE_NUM_MAX: u32 = 0x34;
const VIRTIO_REG_QUEUE_NUM: u32 = 0x38;
const VIRTIO_REG_QUEUE_ALIGN: u32 = 0x3c;
const VIRTIO_REG_QUEUE_PFN: u32 = 0x40;
#[expect(dead_code)]
const VIRTIO_REG_QUEUE_READY: u32 = 0x44;
const VIRTIO_REG_QUEUE_NOTIFY: u32 = 0x50;
const VIRTIO_REG_DEVICE_STATUS: u32 = 0x70;
const VIRTIO_REG_DEVICE_CONFIG: u32 = 0x100;
const VIRTIO_STATUS_ACK: u32 = 1;
const VIRTIO_STATUS_DRIVER: u32 = 2;
const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
const VIRTIO_STATUS_FEAT_OK: u32 = 8;
const VIRTQ_DESC_F_NEXT: u32 = 1;
const VIRTQ_DESC_F_WRITE: u32 = 2;
#[expect(dead_code)]
const VIRTQ_AVAIL_F_NO_INTERRUPT: u32 = 1;
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

// Virtqueue Descriptor area entry.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

// Virtqueue Available Ring.
#[repr(C, packed)]
#[derive(Debug)]
struct VirtqAvail {
    flags: u16,
    index: u16,
    ring: [u16; VIRTQ_ENTRY_NUM],
}

// Virtqueue Used Ring entry.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

// Virtqueue Used Ring.
#[repr(C, packed)]
#[derive(Debug)]
struct VirtqUsed {
    flags: u16,
    index: u16,
    ring: [VirtqUsedElem; VIRTQ_ENTRY_NUM],
}

// Page-aligned VirtqUsed
#[repr(C, align(4096))]
#[derive(Debug)]
struct AlignedVirtqUsed(VirtqUsed);

// Virtqueue.
#[repr(C)] // Not packed, as VirtqUsed is aligned to page size
#[derive(Debug)]
struct VirtioVirtq {
    descs: [VirtqDesc; VIRTQ_ENTRY_NUM],
    avail: VirtqAvail,
    used: AlignedVirtqUsed, // Needs align to page size
    queue_index: u16,
    used_index: *mut u16, // Only access using ptr::read_volatile
    last_used_index: u16,
}

impl VirtioVirtq {
    fn zeroed() -> Self {
        // SAFETY: VirtioVirtq contains only structs/arrays of integers and pointers.
        // All-zero bytes is a valid representation: integers become 0, pointer becomes null.
        unsafe { core::mem::MaybeUninit::zeroed().assume_init() }
    }
}

// Safety: Single threaded OS
unsafe impl Sync for VirtioVirtq {}
// Safety: VirtioVirtq contains a pointer to memory-mapped I/O registers.
// This pointer is only accessed while holding the SpinLock, ensuring
// no concurrent access occurs. The hardware is accessible from any CPU core.
unsafe impl Send for VirtioVirtq {}

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

fn virtio_reg_read32(offset: u32) -> u32 {
    assert_eq!((VIRTIO_BLK_PADDR + offset) % align_of::<u32>() as u32, 0);
    unsafe {
        // Safety:
        // * VIRTIO_BLK_PADDR + offset is valid for reads
        // * VIRTIO_BLK_PADDR is 32-bit aligned and offset is 32-bit aligned
        // * VIRTIO_BLK_PADDR + offset points to a QEMU initialized `u32`
        // * `u32` is Copy
        ptr::read_volatile((VIRTIO_BLK_PADDR + offset) as *const u32)
    }
}

fn virtio_reg_read64(offset: u32) -> u64 {
    assert_eq!((VIRTIO_BLK_PADDR + offset) % align_of::<u64>() as u32, 0);
    unsafe {
        // Safety:
        // * VIRTIO_BLK_PADDR + offset is valid for reads
        // * VIRTIO_BLK_PADDR is 64-bit aligned and offset is 64-bit aligned
        // * VIRTIO_BLK_PADDR + offset points to a QEMU initialized `u64`
        // * `u64` is Copy
        ptr::read_volatile((VIRTIO_BLK_PADDR + offset) as *const u64)
    }
}

fn virtio_reg_write32(offset: u32, value: u32) {
    assert_eq!((VIRTIO_BLK_PADDR + offset) % align_of::<u32>() as u32, 0);
    unsafe {
        // Safety:
        // * VIRTIO_BLK_PADDR + offset is valid for writes.
        // * VIRTIO_BLK_PADDR + offset is properly 32-bit aligned.
        ptr::write_volatile((VIRTIO_BLK_PADDR + offset) as *mut u32, value)
    }
}

fn virtio_reg_fetch_and_or32(offset: u32, value: u32) {
    virtio_reg_write32(offset, virtio_reg_read32(offset) | value);
}

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
        Some(virtio_reg_read64(VIRTIO_REG_DEVICE_CONFIG + 0) * SECTOR_SIZE as u64);

    match *BLK_CAPACITY.lock() {
        Some(capacity) => println!("virtio-blk: capacity is {} bytes", capacity),
        None => println!("virtio-blk: capacity is not initialized yet"),
    }

    // Allocate a region to store requests to the device.
    *BLK_REQ.lock() = Some(Box::new(VirtioBlkReq::zeroed()));
}

fn virtq_init(index: usize) -> Box<VirtioVirtq> {
    // Allocate a region for the virtqueue.
    let mut vq = Box::new(VirtioVirtq::zeroed());

    vq.queue_index = index as u16;
    vq.used_index = &raw mut vq.used.0.index; // Create pointer for read_volatile

    // 1. Select the queue writing its index (first queue is 0) to QueueSel.
    virtio_reg_write32(VIRTIO_REG_QUEUE_SEL, index as u32);
    // 5. Notify the device about the queue size by writing the size to QueueNum.
    virtio_reg_write32(VIRTIO_REG_QUEUE_NUM, VIRTQ_ENTRY_NUM as u32);
    // 6. Notify the device about the used alignment by writing its value in bytes to QueueAlign.
    virtio_reg_write32(VIRTIO_REG_QUEUE_ALIGN, 0);
    // 7. Write the physical number of the first page of the queue to the QueuePFN register.
    virtio_reg_write32(VIRTIO_REG_QUEUE_PFN, &*vq as *const _ as u32); // In our OS the virtual address matches the physical address

    vq
}

// Notifies the device that there is a new request. `desc_index` is the index of the head descriptor of the new request
fn virtq_kick(vq: &mut VirtioVirtq, desc_index: u16) {
    let index = vq.avail.index as usize % VIRTQ_ENTRY_NUM;
    vq.avail.ring[index] = desc_index;
    vq.avail.index += 1;

    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst); // Equivalent to __sync_synchronise();

    virtio_reg_write32(VIRTIO_REG_QUEUE_NOTIFY, vq.queue_index.into()); // converting `u16` to `u32` cannot fail
    vq.last_used_index += 1;
}

// Returns whether there are requests being processed by the device.
fn virtq_is_busy(vq: &VirtioVirtq) -> bool {
    assert_eq!(vq.used_index as usize % align_of::<u16>(), 0);
    unsafe {
        // Safety:
        // * vq.used_index is valid for reads
        // * vq.used_index is 16-bit aligned
        // * vq.used_index points to a value properly initialised by QEMU
        // * `u16` is Copy
        vq.last_used_index != core::ptr::read_volatile(vq.used_index)
    }
}

// Reads/writes from/to virtio-blk device.
pub fn read_write_disk(buf: &mut [u8], sector: u64, is_write: bool) {
    let blk_capacity = BLK_CAPACITY
        .lock()
        .expect("block capacity should be initialised before read_write_disk call.");
    if sector >= (blk_capacity / SECTOR_SIZE as u64) {
        println!(
            "virtio: tried to read/write sector={}, but capacity is {}",
            sector,
            blk_capacity / SECTOR_SIZE as u64
        );
        return;
    }

    let mut br_guard = BLK_REQ.lock();
    let br = br_guard.as_mut().expect("BLK_REQ not initialised");

    br.sector = sector;
    br.req_type = if is_write {
        VIRTIO_BLK_T_OUT
    } else {
        VIRTIO_BLK_T_IN
    };

    if is_write {
        br.data.copy_from_slice(buf);
    };

    // Construct the virtqueue descriptors (using 3 descriptors).
    let mut vq_guard = BLK_REQUEST_VQ.lock();
    let vq = vq_guard.as_mut().expect("BLK_REQUEST_VQ not initialised");

    let blk_req_paddr = &**br as *const VirtioBlkReq as usize; // Double deference to get address from heap, not of the Box

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
        len: SECTOR_SIZE as u32,
        flags: (VIRTQ_DESC_F_NEXT | (if is_write { 0 } else { VIRTQ_DESC_F_WRITE })) as u16,
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
    virtq_kick(vq.as_mut(), 0);

    // Wait until the device finishes processing.
    while virtq_is_busy(vq.as_ref()) {
        core::hint::spin_loop();
    }

    // virtio-blk: If a non-zero value is returned, it's an error.
    if br.status != 0 {
        println!(
            "virtio: warn: failed to read/write sector={} status={}",
            sector, br.status
        );
        return;
    }

    // For read operations, copy the data into the buffer.
    if !is_write {
        buf.copy_from_slice(&br.data);
    }
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
        assert_eq!(capacity, 32768 * SECTOR_SIZE as u64);
    }

    #[test_case]
    fn read_block_zero_fat16_signature() {
        // Block 0 of a FAT16 volume has "FAT16" at byte offset 54
        let mut buf = [0u8; SECTOR_SIZE];
        read_write_disk(&mut buf, 0, false);
        assert_eq!(&buf[54..59], b"FAT16");
    }

    #[test_case]
    fn write_and_read_back() {
        let s = "hello from kernel!!!";
        let mut buf = [0u8; SECTOR_SIZE];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        read_write_disk(&mut buf, 1, true);

        let mut buf2 = [0u8; SECTOR_SIZE];
        read_write_disk(&mut buf2, 1, false);
        assert_eq!(&buf2[..s.len()], s.as_bytes());
    }
}
