//! Virtio queue for QEMU board

use alloc::boxed::Box;

use crate::arch::mmio;
use crate::board::virtio_blk;
use crate::hal::PAGE_SIZE;

const VIRTIO_REG_GUEST_PAGE_SIZE: usize = 0x28;
const VIRTIO_REG_QUEUE_SEL: usize = 0x30;
#[expect(dead_code)]
const VIRTIO_REG_QUEUE_NUM_MAX: usize = 0x34;
const VIRTIO_REG_QUEUE_NUM: usize = 0x38;
const VIRTIO_REG_QUEUE_ALIGN: usize = 0x3c;
const VIRTIO_REG_QUEUE_PFN: usize = 0x40;
#[expect(dead_code)]
const VIRTIO_REG_QUEUE_READY: usize = 0x44;
const VIRTIO_REG_QUEUE_NOTIFY: usize = 0x50;
const VIRTQ_ENTRY_NUM: usize = 16;
#[expect(dead_code)]
const VIRTQ_AVAIL_F_NO_INTERRUPT: u32 = 1;

pub(super) struct VirtqToken {
    used_index: *const u16,
    last_used_index: u16,
}

impl VirtqToken {
    pub fn is_complete(&self) -> bool {
        unsafe {
            // Safety: Caller must ensure virtio queue remains in memory (bump allocator)
            core::ptr::read_volatile(self.used_index) == self.last_used_index
        }
    }
}

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

// The Used Ring starts at a 4096-byte boundary relative to the virtqueue start
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

pub(super) fn virtq_init(index: usize) -> Box<VirtioVirtq> {
    // Allocate a region for the virtqueue.
    let mut vq = Box::new(VirtioVirtq::zeroed());

    vq.queue_index = index as u16;
    vq.used_index = &raw mut vq.used.0.index; // Create pointer for read_volatile

    // 1. Select the queue writing its index (first queue is 0) to QueueSel.
    mmio::write32(virtio_blk::BASE, VIRTIO_REG_QUEUE_SEL, index as u32);
    // 5. Notify the device about the queue size by writing the size to QueueNum.
    mmio::write32(
        virtio_blk::BASE,
        VIRTIO_REG_QUEUE_NUM,
        VIRTQ_ENTRY_NUM as u32,
    );
    // 6. Notify the device about the used alignment by writing its value in bytes to QueueAlign. Align to 4096;
    mmio::write32(virtio_blk::BASE, VIRTIO_REG_QUEUE_ALIGN, PAGE_SIZE as u32);
    // 7. Notify the device about the guest page size
    mmio::write32(
        virtio_blk::BASE,
        VIRTIO_REG_GUEST_PAGE_SIZE,
        PAGE_SIZE as u32,
    );
    // 8. Write the physical number of the first page of the queue to the QueuePFN register.
    let addr = &*vq as *const _ as u32;
    debug_assert!(
        addr.is_multiple_of(PAGE_SIZE as u32),
        "virtqueue not page-aligned"
    );
    mmio::write32(
        virtio_blk::BASE,
        VIRTIO_REG_QUEUE_PFN,
        addr / PAGE_SIZE as u32,
    ); // In our OS the virtual address matches the physical address

    vq
}

// Notifies the device that there is a new request. `desc_index` is the index of the head descriptor of the new request
pub(super) fn virtq_kick(vq: &mut VirtioVirtq, desc_index: u16) -> VirtqToken {
    let index = vq.avail.index as usize % VIRTQ_ENTRY_NUM;
    vq.avail.ring[index] = desc_index;
    vq.avail.index += 1;

    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst); // Equivalent to __sync_synchronise();

    mmio::write32(
        virtio_blk::BASE,
        VIRTIO_REG_QUEUE_NOTIFY,
        vq.queue_index.into(),
    ); // converting `u16` to `u32` cannot fail
    vq.last_used_index += 1;

    VirtqToken {
        used_index: vq.used_index,
        last_used_index: vq.last_used_index,
    }
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
