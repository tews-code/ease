//! Virtio queue for QEMU board

use alloc::boxed::Box;
use core::ptr;

pub(super) const VIRTQ_ENTRY_NUM: usize = 16;
pub(super) const VIRTIO_DEVICE_BLK: u32 = 2;
pub(super) const VIRTIO_BLK_PADDR: u32 = 0x10001000;
pub(super) const VIRTIO_REG_MAGIC: u32 = 0x00;
pub(super) const VIRTIO_REG_VERSION: u32 = 0x04;
pub(super) const VIRTIO_REG_DEVICE_ID: u32 = 0x08;
pub(super) const VIRTIO_REG_QUEUE_SEL: u32 = 0x30;
#[expect(dead_code)]
pub(super) const VIRTIO_REG_QUEUE_NUM_MAX: u32 = 0x34;
pub(super) const VIRTIO_REG_QUEUE_NUM: u32 = 0x38;
pub(super) const VIRTIO_REG_QUEUE_ALIGN: u32 = 0x3c;
pub(super) const VIRTIO_REG_QUEUE_PFN: u32 = 0x40;
#[expect(dead_code)]
pub(super) const VIRTIO_REG_QUEUE_READY: u32 = 0x44;
pub(super) const VIRTIO_REG_QUEUE_NOTIFY: u32 = 0x50;
pub(super) const VIRTIO_REG_DEVICE_STATUS: u32 = 0x70;
pub(super) const VIRTIO_REG_DEVICE_CONFIG: u32 = 0x100;
pub(super) const VIRTIO_STATUS_ACK: u32 = 1;
pub(super) const VIRTIO_STATUS_DRIVER: u32 = 2;
pub(super) const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub(super) const VIRTIO_STATUS_FEAT_OK: u32 = 8;
pub(super) const VIRTQ_DESC_F_NEXT: u32 = 1;
pub(super) const VIRTQ_DESC_F_WRITE: u32 = 2;
#[expect(dead_code)]
pub(super) const VIRTQ_AVAIL_F_NO_INTERRUPT: u32 = 1;

// Virtqueue Descriptor area entry.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub(super) struct VirtqDesc {
    pub(super) addr: u64,
    pub(super) len: u32,
    pub(super) flags: u16,
    pub(super) next: u16,
}

// Virtqueue Available Ring.
#[repr(C, packed)]
#[derive(Debug)]
pub(super) struct VirtqAvail {
    flags: u16,
    index: u16,
    ring: [u16; VIRTQ_ENTRY_NUM],
}

// Virtqueue Used Ring entry.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub(super) struct VirtqUsedElem {
    id: u32,
    len: u32,
}

// Virtqueue Used Ring.
#[repr(C, packed)]
#[derive(Debug)]
pub(super) struct VirtqUsed {
    flags: u16,
    index: u16,
    ring: [VirtqUsedElem; VIRTQ_ENTRY_NUM],
}

// Page-aligned VirtqUsed
#[repr(C, align(4096))]
#[derive(Debug)]
pub(super) struct AlignedVirtqUsed(VirtqUsed);

// Virtqueue.
#[repr(C)] // Not packed, as VirtqUsed is aligned to page size
#[derive(Debug)]
pub(super) struct VirtioVirtq {
    pub(super) descs: [VirtqDesc; VIRTQ_ENTRY_NUM],
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

pub(super) fn virtio_reg_read32(offset: u32) -> u32 {
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

pub(super) fn virtio_reg_read64(offset: u32) -> u64 {
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

pub(super) fn virtio_reg_write32(offset: u32, value: u32) {
    assert_eq!((VIRTIO_BLK_PADDR + offset) % align_of::<u32>() as u32, 0);
    unsafe {
        // Safety:
        // * VIRTIO_BLK_PADDR + offset is valid for writes.
        // * VIRTIO_BLK_PADDR + offset is properly 32-bit aligned.
        ptr::write_volatile((VIRTIO_BLK_PADDR + offset) as *mut u32, value)
    }
}

pub(super) fn virtio_reg_fetch_and_or32(offset: u32, value: u32) {
    virtio_reg_write32(offset, virtio_reg_read32(offset) | value);
}

pub(super) fn virtq_init(index: usize) -> Box<VirtioVirtq> {
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
pub(super) fn virtq_kick(vq: &mut VirtioVirtq, desc_index: u16) {
    let index = vq.avail.index as usize % VIRTQ_ENTRY_NUM;
    vq.avail.ring[index] = desc_index;
    vq.avail.index += 1;

    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst); // Equivalent to __sync_synchronise();

    virtio_reg_write32(VIRTIO_REG_QUEUE_NOTIFY, vq.queue_index.into()); // converting `u16` to `u32` cannot fail
    vq.last_used_index += 1;
}

// Returns whether there are requests being processed by the device.
pub(super) fn virtq_is_busy(vq: &VirtioVirtq) -> bool {
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
