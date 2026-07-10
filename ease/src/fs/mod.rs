//! FAT File System

/*
 * Volume Layout
 *
 * In a simple case the entire device is one volume (e.g. QEMU image, USB stick) this is
 * "superfloppy" format. As an example for FAT16:
 *
 *  * Sector 0   Sector 4    Sector 68        Sector 100         Last sector
 * ┌────────────────┬───────────┬────────────────┬──────────────────────┐
 * │ Reserved       │ FAT(s)    │ Root Directory │ Data                 │
 * │                │           │                │                      │
 * │ BPB in first   │ cluster   │ file/dir       │ actual file contents │
 * │ 3 empty sectors│ chain map │ entries        │ stored in clusters   │
 * └────────────────┴───────────┴────────────────┴──────────────────────┘
 *
 * Where the device as partitions (e.g. SD Card) the device has a Master Boot Record and
 * multiple partitions:
 * ┌──────────┬─────────────┬───────────────────────────────────────────────┐
 * │ MBR      │ (alignment  │ Partition 1 = the FAT16 volume                │
 * │ (dev 0)  │  gap)       │ ┌──────────┬────────┬──────────┬────────────┐ │
 * │          │             │ │ Reserved │ FAT(s) │ Root Dir │ Data       │ │
 * │ entry 1: │             │ │ BPB here │        │          │            │ │
 * │ LBA 2048 ──────────────▶ vol 0     │ vol 4  │ vol 68   │ vol 100    │ │
 * └──────────┴─────────────┴─┴──────────┴────────┴──────────┴────────────┘─┘
 *   device
 *   sector :0                 device:2048  :2052    :2116      :2148
 *
 * The QEMU virtio device block size is 512 bytes, all data in or out of storage is in block sizes of bytes.
 *
 * The disk's sector size is chosen as 512 bytes, and all file operations work in sector size blocks.
 *
 * LBA is Logical Block Address: a sector named by a single linear number (0, 1, 2, … up the disk)
 *
 * A cluster is a contigous power-of-two number of sectors, and all file system units of allocation
 * are in clusters. For QEMU the cluster size is chosen as 4 (2 KiB).
 *
 * For an example FAT16 volume:
 *
 * ┌──────────┬──────────────┬─────────────────────┬───────────────────────────────────────┐
 * │  Region  │ Start sector │        Size         │             Contains                  │
 * ├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
 * │ Reserved │ 0            │ 4 sectors           │ Boot sector (BPB) at sector 0         │
 * ├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
 * │ FAT × 2  │ 4            │ 2 × 32 = 64 sectors │ Two copies of the cluster chain table │
 * ├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
 * │ Root dir │ 68           │ 32 sectors          │ 512 directory entries (fixed size)    │
 * ├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
 * │ Data     │ 100          │ rest of disk        │ File contents, stored in clusters     │
 * └──────────┴──────────────┴─────────────────────┴───────────────────────────────────────┘
 *
 *  For an example FAT32 volume:
 *
 * Sector: 0          1          2         3–5       6          7          8         9–31       32
 * ┌────────────┬──────────┬────────────┬────────┬────────────┬──────────┬────────────┬────────┬────────┬────────┐
 * │ Boot       │ FSInfo   │ Boot code  │ unused │ backup     │ backup   │ backup     │ unused │ FAT    │ Data   │
 * │ sector     │          │ spillover  │        │ of 0       │ of 1     │ of 2       │        │        │        │
 * └────────────┴──────────┴────────────┴────────┴────────────┴──────────┴────────────┴────────┴────────┴────────┘
 * └────── the working set ─────────┘└────── copy written at format time ─────────┘
 *
 * Note: The root directory is a file cluster, rather than a designated set of sectors.
 *
 */

use crate::board::virtio::blk;
use crate::drivers::virtio::blk::{BlkError, read_block};

mod bpb;
mod dir;
mod fat;
mod mbr;
pub(crate) mod volume;

use bpb::{Bpb, BpbError};
use fat::FatChainClusterError;
use mbr::{Mbr, MbrError};

const BOOT_SECTOR: u32 = 0;
const BOOT_SECTOR_SIG: [u8; 2] = [0x55, 0xAA];
const SECTOR_SIZE: usize = 512;

const _: () = assert!(SECTOR_SIZE == blk::BLOCK_SIZE);

#[derive(Debug)]
#[allow(dead_code)]
pub enum FsError {
    Bpb(BpbError),
    Device(BlkError),
    Mbr(MbrError),
    FatChain(FatChainClusterError),
    BadSectorFound,
    DirFull,
    DiskFull,
    FreeSectorFound,
    InvalidName,
    FileSizeMismatch,
    NotFound,
    UnknownFormat,
    VolumeExceedsPartition,
}

impl From<BlkError> for FsError {
    fn from(e: BlkError) -> Self {
        FsError::Device(e)
    }
}

impl From<BpbError> for FsError {
    fn from(e: BpbError) -> Self {
        FsError::Bpb(e)
    }
}

impl From<MbrError> for FsError {
    fn from(e: MbrError) -> Self {
        FsError::Mbr(e)
    }
}

impl From<FatChainClusterError> for FsError {
    fn from(e: FatChainClusterError) -> Self {
        FsError::FatChain(e)
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum VolumeType {
    Fat16(u32),
    Fat32(u32),
}

#[derive(Debug)]
#[expect(dead_code)]
pub(crate) enum MountError {
    Fs(FsError),
    UnsupportedVolumeType,
}

impl From<FsError> for MountError {
    fn from(e: FsError) -> Self {
        MountError::Fs(e)
    }
}

pub(crate) fn init() -> Result<VolumeType, MountError> {
    let (lba, bpb) = mount()?;
    if matches!(bpb.volume_type, VolumeType::Fat16(_)) {
        volume::fat16_init(lba, bpb);
        Ok(VolumeType::Fat16(0))
    } else {
        Err(MountError::UnsupportedVolumeType)
    }
}

// Attempt to mount the storage device
fn mount() -> Result<(u32, Bpb), FsError> {
    let mut buf = [0u8; blk::BLOCK_SIZE];
    read_block(BOOT_SECTOR, &mut buf)?;
    // First try to read sector 0 as a BPB, then as MBR
    match Bpb::parse(&buf) {
        Ok(bpb) => Ok((0, bpb)),
        Err(_) => match Mbr::parse(&buf) {
            Ok(mbr) => {
                // Parse the MBR to get the first valid partition (which is all we support)
                let (lba, sector_count) = mbr.find_partition()?;
                // Read the BPB at this location
                read_block(lba, &mut buf)?;
                let bpb = Bpb::parse(&buf)?;
                // Make sure that the partition size and volume size fit
                if sector_count < bpb.total_sectors {
                    return Err(FsError::VolumeExceedsPartition);
                }
                Ok((lba, bpb))
            }
            Err(_) => Err(FsError::UnknownFormat),
        },
    }
}
