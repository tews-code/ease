//! QEMU virtio devices

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::arch::mmio;

pub(crate) mod blk;
pub(crate) mod input;
pub(crate) mod keyboard;
mod queue;

// Virtio magic and version
const VIRTIO_MAGIC: u32 = 0x74726976;
const VIRTIO_VERSION: u32 = 1;

// Virtio MMIO register offsets
const VIRTIO_REG_MAGIC: usize = 0x00;
const VIRTIO_REG_VERSION: usize = 0x04;
const VIRTIO_REG_DEVICE_ID: usize = 0x08;
const VIRTIO_REG_GUEST_FEAT: usize = 0x20;
const VIRTIO_REG_INTERRUPT_STATUS: usize = 0x60;
const VIRTIO_REG_INTERRUPT_ACK: usize = 0x64;
const VIRTIO_REG_DEVICE_STATUS: usize = 0x70;
const VIRTIO_REG_DEVICE_CONFIG: usize = 0x100;

// Virtio device status flags
const VIRTIO_STATUS_ACK: u32 = 1;
const VIRTIO_STATUS_DRIVER: u32 = 2;
const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
#[allow(dead_code)]
const VIRTIO_STATUS_FEAT_OK: u32 = 8;

// Virtio feature mask
const VIRTIO_NO_OPTIONAL_FEAT: u32 = 0;

// Check the virtio is in place
fn check_virtio(base: usize, virtio_device_id: u32) {
    assert_eq!(mmio::read32(base, VIRTIO_REG_MAGIC), VIRTIO_MAGIC);
    assert_eq!(mmio::read32(base, VIRTIO_REG_VERSION), VIRTIO_VERSION);
    assert_eq!(mmio::read32(base, VIRTIO_REG_DEVICE_ID), virtio_device_id);
}

// Set the bits in `value` to the 32 bit MMIO register at `base` + `offset`
//
// This does not disable interrupts
fn set_bits32(base: usize, offset: usize, value: u32) {
    mmio::write32(base, offset, mmio::read32(base, offset) | value);
}

// Takes ownership of a virtio device with a reset and handshake
// Note that to use the device call `set_driver_ok`
fn reset_and_handshake(base: usize) {
    // 1. Reset the device
    mmio::write32(base, VIRTIO_REG_DEVICE_STATUS, 0);
    // 2. Set the ACKNOWLEDGE status bit: the guest OS has noticed the device
    set_bits32(base, VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_ACK);
    // 3. Set the DRIVER status bit.
    set_bits32(base, VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER);
    // 4. Declare that we aren't accepting optional features
    mmio::write32(base, VIRTIO_REG_GUEST_FEAT, VIRTIO_NO_OPTIONAL_FEAT);
    // Steps 5. and 6. are not used in QEMU v1
}

fn set_driver_ok(base: usize) {
    // 8. Set the DRIVER_OK status bit.
    set_bits32(base, VIRTIO_REG_DEVICE_STATUS, VIRTIO_STATUS_DRIVER_OK);
}

#[cfg_attr(feature = "profile", profile)]
pub fn ack_interrupt(base: usize) {
    let status = mmio::read32(base, VIRTIO_REG_INTERRUPT_STATUS);
    mmio::write32(base, VIRTIO_REG_INTERRUPT_ACK, status);
}
