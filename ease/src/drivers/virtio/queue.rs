//! Virtio queue for QEMU board

use alloc::boxed::Box;

use core::mem::MaybeUninit;
use core::ptr::{read_volatile, write_volatile};

use crate::arch::mmio;
use crate::board::virtio;

pub(super) const VIRTQ_ENTRY_NUM: usize = 16;
pub(super) const VIRTQ_DESC_F_NEXT: u32 = 1;
pub(super) const VIRTQ_DESC_F_WRITE: u32 = 2;
pub(super) const VIRTIO_REG_QUEUE_NOTIFY: usize = 0x50;

const VIRTIO_REG_QUEUE_SEL: usize = 0x30;
#[allow(dead_code)]
const VIRTIO_REG_QUEUE_NUM_MAX: usize = 0x34;
const VIRTIO_REG_QUEUE_NUM: usize = 0x38;
const VIRTIO_REG_QUEUE_ALIGN: usize = 0x3c;
const VIRTIO_REG_QUEUE_PFN: usize = 0x40;
const VIRTIO_REG_GUEST_PAGE_SIZE: usize = 0x28;

// Virtqueue Descriptor area entry.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct VirtqDesc {
    pub(super) addr: u64,
    pub(super) len: u32,
    pub(super) flags: u16,
    pub(super) next: u16,
}

// Virtqueue Available Ring.
#[repr(C)]
#[derive(Debug)]
pub(super) struct VirtqAvail {
    flags: u16,
    index: u16,
    ring: [u16; VIRTQ_ENTRY_NUM],
}

// Virtqueue Used Ring entry.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct VirtqUsedElem {
    pub(super) id: u32,
    len: u32,
}

// Virtqueue Used Ring.
#[repr(C)]
#[derive(Debug)]
pub(super) struct VirtqUsed {
    flags: u16,
    index: u16,
    ring: [VirtqUsedElem; VIRTQ_ENTRY_NUM],
}

// The Used Ring starts at a 512-byte boundary relative to the virtqueue start
// Page-aligned VirtqUsed
#[repr(C, align(512))]
#[derive(Debug)]
pub(super) struct AlignedVirtqUsed(VirtqUsed);

const _: () =
    assert!(core::mem::align_of::<AlignedVirtqUsed>() == crate::board::virtio::VIRTQ_PAGE_SIZE);

// Virtqueue.
#[repr(C)] // Not packed, as VirtqUsed is aligned to page size
#[derive(Debug)]
pub(super) struct VirtioVirtq {
    pub(super) descs: [VirtqDesc; VIRTQ_ENTRY_NUM],
    avail: VirtqAvail,
    used: AlignedVirtqUsed, // Needs align to page size
    queue_index: u16,
    pub(super) last_used_index: u16,
}

#[allow(dead_code)]
impl VirtioVirtq {
    fn zeroed() -> Self {
        // SAFETY: VirtioVirtq contains only structs/arrays of integers and pointers.
        // All-zero bytes is a valid representation: integers become 0
        unsafe { core::mem::MaybeUninit::zeroed().assume_init() }
    }

    // Helper function to read the DMA result
    //
    // The device can write to this memory at any time, the compiler must not
    // optimize away reads of it.
    pub fn read_used_index(&self) -> u16 {
        unsafe { read_volatile(&raw const self.used.0.index) }
    }

    // If last used index lags used index pop the next used element
    // Otherwise returns None
    pub(super) fn pop_used(&mut self) -> Option<VirtqUsedElem> {
        unsafe {
            if self.last_used_index != read_volatile(&self.used.0.index) {
                let vq_used_elem = read_volatile::<VirtqUsedElem>(
                    &raw const (self.used.0.ring[self.last_used_index as usize % VIRTQ_ENTRY_NUM]),
                );
                self.last_used_index = self.last_used_index.wrapping_add(1);
                Some(vq_used_elem)
            } else {
                None
            }
        }
    }
}

// Safety: Single threaded OS
unsafe impl Sync for VirtioVirtq {}
// Safety: the struct is only accessed under the StaticMutex (single-core, interrupts disabled).
unsafe impl Send for VirtioVirtq {}

pub(super) fn virtq_init(base: usize, queue_idx: usize) -> Box<VirtioVirtq> {
    // Allocate a region for the virtqueue.
    let vq: Box<MaybeUninit<VirtioVirtq>> = Box::new_zeroed();
    // Safety: zero is a valid bit pattern for VirtioVirtq (all integers/arrays)
    let mut vq: Box<VirtioVirtq> = unsafe { vq.assume_init() };

    vq.queue_index = queue_idx as u16;

    // 1. Select the queue writing its index (first queue is 0) to QueueSel.
    mmio::write32(base, VIRTIO_REG_QUEUE_SEL, queue_idx as u32);
    // 5. Notify the device about the queue size by writing the size to QueueNum.
    mmio::write32(base, VIRTIO_REG_QUEUE_NUM, VIRTQ_ENTRY_NUM as u32);
    // 6. Notify the device about the used alignment by writing its value in bytes to QueueAlign. Aligned to VIRTQ_PAGE_SIZE;
    mmio::write32(base, VIRTIO_REG_QUEUE_ALIGN, virtio::VIRTQ_PAGE_SIZE as u32);
    // 7. Notify the device about the guest page size
    mmio::write32(
        base,
        VIRTIO_REG_GUEST_PAGE_SIZE,
        virtio::VIRTQ_PAGE_SIZE as u32,
    );
    // 8. Write the physical number of the first page of the queue to the QueuePFN register.
    let addr = &*vq as *const _ as u32;
    debug_assert!(
        addr.is_multiple_of(virtio::VIRTQ_PAGE_SIZE as u32),
        "virtqueue not page-aligned"
    );
    mmio::write32(
        base,
        VIRTIO_REG_QUEUE_PFN,
        addr / virtio::VIRTQ_PAGE_SIZE as u32,
    ); // In our OS the virtual address matches the physical address

    vq
}

// Writes one avail ring entry and advances avail.idx. Callable in a loop.
pub(super) fn virtq_publish(vq: &mut VirtioVirtq, desc_head: u16) {
    unsafe {
        let index = read_volatile(&raw const vq.avail.index);
        write_volatile(
            &raw mut vq.avail.ring[index as usize % VIRTQ_ENTRY_NUM],
            desc_head,
        );
        write_volatile(&raw mut vq.avail.index, index.wrapping_add(1));
    }
}

// Sets a fence and writes the QueueNotify
pub(super) fn virtq_notify(base: usize, vq: &VirtioVirtq) {
    // SeqCst is correct but stronger than necessary, could be "fence w, o"
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    mmio::write32(base, VIRTIO_REG_QUEUE_NOTIFY, vq.queue_index.into()); // converting `u16` to `u32` cannot fail
}
