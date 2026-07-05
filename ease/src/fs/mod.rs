//! FAT16 File System

/*
 * Device (SD card)
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
 *  LBA is Logical Block Address: a sector named by a single linear number (0, 1, 2, … up the disk)
 */

/*
 * The device block size is 512 bytes, all data in or out of storage is in block sizes of bytes.
 *
 * The disk's sector size is 512 bytes, and all file operations work in sector size blocks.
 *
 * The cluster size is chosen as 4 sectors (2KiB), the file system's unit of allocation.
 *
 * The FAT16 disk is laid out in four consecutive regions:
 *
 * Sector 0   Sector 4    Sector 68        Sector 100         Last sector
 * ┌────────────────┬───────────┬────────────────┬──────────────────────┐
 * │ Reserved       │ FAT(s)    │ Root Directory │ Data                 │
 * │                │           │                │                      │
 * │ BPB in first   │ cluster   │ file/dir       │ actual file contents │
 * │ 3 empty sectors│ chain map │ entries        │ stored in clusters   │
 * └────────────────┴───────────┴────────────────┴──────────────────────┘
 *
 * For EASE these values are:
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
 * Reserved (boot) sector
 *
 * The BPB bytes 11–61 of it are a parameter table the format embeds
 * so a reader can derive the entire disk layout from one sector read - where the FATs start,
 * where the root directory lives, how cluster numbers become sector numbers:
 *
 * ┌────────┬──────┬──────────────────────────────────────────┬─────────────────┐
 * │ Offset │ Size │                  Field                   │     EASE?       │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 0      │ 3    │ x86 jump instruction                     │ validated       │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 3      │ 8    │ OEM name (e.g. mkfs.fat)                 │ skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 11     │ 2    │ bytes per sector                         │ validated = 512 │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 13     │ 1    │ sectors per cluster                      │ parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 14     │ 2    │ reserved sector count                    │ parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 16     │ 1    │ number of FATs                           | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 17     │ 2    │ root directory entry count               | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 19     │ 2    │ total sectors (16-bit)                   | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 21     │ 1    │ media descriptor                         | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 22     │ 2    │ sectors per FAT                          | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 24–31  │ 8    │ sectors/track, heads, hidden sectors     | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 32     │ 4    │ total sectors (32-bit)                   | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 36–53  │ 18   │ drive no, boot sig, vol serial + label   | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 54     │ 8    │ filesystem type string "FAT16   "        | ignored (1)     │
 * └────────┴──────┴──────────────────────────────────────────┴─────────────────┘
 *
 * (1) The type string is informational only; per the FAT spec the type is
 *     determined by the cluster count (4085..=65524 means FAT16).
 *
 * File Allocation Table
 *
 * A FAT is a flat array of u16s across its sectors (2 sectors in this case).
 * The index of the FAT is directly the cluster number in the data region.
 * Each u16 holds a number that is marker for the next cluster's role.
 *
 * ┌──────────┬──────────────┬───────────────────────────────────────┐
 * │  Index   │     u16      │             Meaning                   │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   0      │   0xFFF8     │ Reserved - media descriptor           │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   1      │   0xFFFF     │ Reserved                              │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   2      │   0x0000     │ Free                                  │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   3      │   0x0000     │ Free                                  │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   4      │   0x0000     │ Free                                  │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   5      │   0x0006     │ Next cluster is 6                     │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   6      │   0x0008     │ Next cluster is 8                     │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   7      │   0x0000     │ Free                                  │
 * ├──────────┼──────────────┼───────────────────────────────────────┤
 * │   8      │   0xFFFF     │ End of chain                          │
 * └──────────┴──────────────┴───────────────────────────────────────┘
 *
 * For FAT16 the u16 values are:
 * ┌─────────────────────────┬─────────────────────────────────────────────────────────────┐
 * │         Value           │                         Meaning                             │
 * ├─────────────────────────┼─────────────────────────────────────────────────────────────┤
 * │ 0x0000                  │ Cluster is free                                             │
 * ├─────────────────────────┼─────────────────────────────────────────────────────────────┤
 * │ 0x0002–0xFFEF           │ Cluster in use; value = next cluster in this file's chain   │
 * ├─────────────────────────┼─────────────────────────────────────────────────────────────┤
 * │ 0xFFF7                  │ Bad cluster — never allocate                                │
 * ├─────────────────────────┼─────────────────────────────────────────────────────────────┤
 * │ 0xFFF8–0xFFFF           │ Cluster in use; it's the last one in its chain              │
 * ├─────────────────────────┼─────────────────────────────────────────────────────────────┤
 * │ 0x0001, 0xFFF0–0xFFF6   │ reserved oddities                                           │
 * └─────────────────────────┴─────────────────────────────────────────────────────────────┘
 *
 * Root Directory
 *
 * The root directory is used to find the first cluster in a file.
 * The directory is a flat array of directory entrys of 32 bytes:
 *
 * Directory entry layout:
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
 *
 * Sub Directories
 *
 * In its parent's table, a subdirectory is an ordinary 32-byte entry with the directory bit
 * (0x10) set at offset 11, a first_cluster like any file — and file_size = 0 always.
 *
 * Its contents are the same 32-byte entry format — an array of DirEntries — but stored in
 * data-region clusters, chained through the FAT, exactly like file contents. Same 0x00 end
 * marker, same 0xE5 deleted marker, same parser. Only the root directory is the special
 * fixed-region case; every subdirectory is cluster-dwelling, growable by chain extension,
 * findable by the same walk.
 *
 * Two entries open every subdirectory:
 * . (pointing to its own first cluster) and
 * .. (pointing to its parent's — with 0 conventionally meaning "parent is root").
 *
 */

use crate::drivers::virtio::blk::BlkError;

const BOOT_SECTOR_SIG: [u8; 2] = [0x55, 0xAA];
const DIR_ENTRY_BYTES: usize = 32;
const SECTOR_SIZE: usize = 512;

const _: () = assert!(SECTOR_SIZE == crate::board::virtio::blk::BLOCK_SIZE);

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
    FileSizeMismatch,
}

pub mod bpb;
pub mod dir_entry;
mod mbr;
pub mod volume;
