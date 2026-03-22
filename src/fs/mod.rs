//! FAT16 File System

/*
 *
 *
 * ● A FAT16 disk is laid out in four consecutive regions:
 *
 * Sector 0                                                    Last sector
 * ┌──────────┬───────────┬────────────────┬──────────────────────┐
 * │ Reserved │ FAT(s)    │ Root Directory │ Data                 │
 * │          │           │                │                      │
 * │ BPB here │ cluster   │ file/dir       │ actual file contents │
 * │          │ chain map │ entries        │ stored in clusters   │
 * └──────────┴───────────┴────────────────┴──────────────────────┘
 *
 * Using your disk image's values:
 *
 * ┌──────────┬──────────────┬─────────────────────┬───────────────────────────────────────┐
 * │  Region  │ Start sector │        Size         │             What's there              │
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
 * A file's directory entry stores its first cluster number. To find the rest of the file's data, you use that cluster number as an
 * index into the FAT:
 *
 * Directory entry for HELLO.TXT:
 * first_cluster = 5
 * file_size = 6000
 *
 * FAT table:
 * [0]  0xFFF8   (reserved - media descriptor)
 * [1]  0xFFFF   (reserved)
 * [2]  0x0000   (free)
 * [3]  0x0000   (free)
 * [4]  0x0000   (free)
 * [5]  0x0006   ← entry 5 says "next cluster is 6"
 * [6]  0x0008   ← entry 6 says "next cluster is 8"
 * [7]  0x0000   (free)
 * [8]  0xFFFF   ← entry 8 says "end of chain"
 *
 * So HELLO.TXT's cluster chain is: 5 → 6 → 8 (cluster 7 was free, so the file is fragmented).
 *
 * - Directory entry layout (each entry is 32 bytes):
 *
 * ┌────────┬──────┬───────────────────────────────────┐
 * │ Offset │ Size │               Field               │
 * ├────────┼──────┼───────────────────────────────────┤
 * │ 0      │ 8    │ Filename (space-padded)           │
 * ├────────┼──────┼───────────────────────────────────┤
 * │ 8      │ 3    │ Extension (space-padded)          │
 * ├────────┼──────┼───────────────────────────────────┤
 * │ 11     │ 1    │ Attributes                        │
 * ├────────┼──────┼───────────────────────────────────┤
 * │ 26     │ 2    │ First cluster (little-endian u16) │
 * ├────────┼──────┼───────────────────────────────────┤
 * │ 28     │ 4    │ File size (little-endian u32)     │
 * └────────┴──────┴───────────────────────────────────┘
 *
 * Special first-byte values:
 * - 0x00 — entry is empty and no more entries follow
 * - 0xE5 — entry is deleted
 *
 * Attribute flags:
 * - 0x0F — long filename entry
 * - 0x08 — volume label
 */

use crate::drivers::virtio::BlkError;

const SECTOR_SIZE: usize = 512;

#[derive(Debug)]
#[allow(dead_code)]
pub enum FsError {
    DeviceError(BlkError),
    DirFull,
    DiskFull,
    InvalidBpb,
    InvalidName,
    NotFat16,
    NotFound,
    UnsupportedSectorSize,
}

pub mod bpb;
pub mod dir_entry;
pub mod volume;
