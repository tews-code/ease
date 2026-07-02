//! QEMU virtio devices

#[cfg(feature = "profile")]
use ease_macros::profile;

use crate::arch::mmio;
use crate::board::virtio;
use crate::kernel::sync::{Completion, Mutex, TimedOut};

pub(crate) mod blk;
pub(crate) mod input;

mod queue;

pub(crate) use blk::BlkError;
use blk::{VirtioBlkDev, with_blk_dev};

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

const IO_TIMEOUT_MS: u64 = 1_000;

// Lock held when IO is in progress (interrupts enabled)
static IO_IN_PROGRESS: Mutex<()> = Mutex::new(());

pub fn read_block(block: u32, buf: &mut [u8; virtio::blk::BLOCK_SIZE]) -> Result<(), BlkError> {
    let _guard = IO_IN_PROGRESS.lock();
    with_blk_dev(|blk| blk.submit_read(block))?;

    wait_for_completion()?;

    with_blk_dev(|blk| blk.finish_read(buf))?;
    Ok(())
}

pub fn write_block(block: u32, buf: &[u8; virtio::blk::BLOCK_SIZE]) -> Result<(), BlkError> {
    let _guard = IO_IN_PROGRESS.lock();
    with_blk_dev(|blk| blk.submit_write(block, buf))?;

    wait_for_completion()?;

    with_blk_dev(|blk| blk.finish_write())?;
    Ok(())
}

// Check for completion of a VirtIO block
fn wait_for_completion() -> Result<(), BlkError> {
    match VIRTIO_COMPLETE.wait_with_deadline(IO_TIMEOUT_MS) {
        Ok(_) => Ok(()),
        Err(TimedOut) => {
            with_blk_dev(|blk| {
                blk.vq = VirtioBlkDev::reset();
            });
            Err(BlkError::Timeout)
        }
    }
}

// Flag tracks completion for interrupt-driven IO
// static VIRTIO_COMPLETE: AtomicBool = AtomicBool::new(false);
static VIRTIO_COMPLETE: Completion = Completion::new();

#[cfg_attr(feature = "profile", profile)]
pub fn handle_virtio_interrupt() {
    let status = mmio::read32(virtio::blk::BASE, VIRTIO_REG_INTERRUPT_STATUS);
    mmio::write32(virtio::blk::BASE, VIRTIO_REG_INTERRUPT_ACK, status);
    VIRTIO_COMPLETE.signal();
}

#[cfg(all(test, feature = "test-virtio"))]
mod test {
    use super::*;

    #[test_case]
    fn device_status_after_init() {
        // virtio::blk_init() already ran in main(); verify DRIVER_OK is set
        let status = mmio::read32(virtio::blk::BASE, VIRTIO_REG_DEVICE_STATUS);
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
        let mut buf = [0u8; virtio::blk::BLOCK_SIZE];
        read_block(0, &mut buf).unwrap();
        assert_eq!(&buf[54..59], b"FAT16");
    }

    #[test_case]
    fn write_and_read_back() {
        let s = "hello from kernel!!!";
        let mut buf = [0u8; virtio::blk::BLOCK_SIZE];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        write_block(1, &buf).unwrap();

        let mut buf2 = [0u8; virtio::blk::BLOCK_SIZE];
        read_block(1, &mut buf2).unwrap();
        assert_eq!(&buf2[..s.len()], s.as_bytes());
    }
}

#[cfg(all(test, feature = "bench"))]
mod baselines {
    // Measured on QEMU virt, set with wide margin for variance
    //   READ_BLOCK:       200,000  (measured ~49,000)
    //   WRITE_BLOCK:    1,000,000  (measured ~277,000)
    //   WRITE_READ_BLOCK: 1,000,000  (measured ~246,000)
    pub const READ_BLOCK: u64 = 4_000_000;
    pub const WRITE_BLOCK: u64 = 4_000_000;
    pub const WRITE_READ_BLOCK: u64 = 4_000_000;
}

#[cfg(all(test, feature = "bench"))]
mod benchmarks {
    use super::baselines;
    use super::*;
    use crate::bench;

    const ITERATIONS: u32 = 10;

    #[test_case]
    fn virtio_block_benchmarks() {
        println!();
        println!("====== VIRTIO BLOCK ====== ");
        println!();

        bench::check(
            "read_block(sector 0)",
            baselines::READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; virtio::blk::BLOCK_SIZE];
                read_block(0, &mut buf).unwrap();
            },
        );

        println!();

        bench::check(
            "write_block(sector 1)",
            baselines::WRITE_BLOCK,
            ITERATIONS,
            || {
                let buf = [0u8; virtio::blk::BLOCK_SIZE];
                write_block(1, &buf).unwrap();
            },
        );

        println!();

        bench::check(
            "write+read_block(sector 1)",
            baselines::WRITE_READ_BLOCK,
            ITERATIONS,
            || {
                let mut buf = [0u8; virtio::blk::BLOCK_SIZE];
                write_block(1, &buf).unwrap();
                read_block(1, &mut buf).unwrap();
            },
        );

        println!();
        println!("===================== ");
        println!();
    }
}
