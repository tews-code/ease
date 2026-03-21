//! FAT16 File System

/*


● A FAT16 disk is laid out in four consecutive regions:

Sector 0                                                    Last sector
┌──────────┬───────────┬────────────────┬──────────────────────┐
│ Reserved │ FAT(s)    │ Root Directory │ Data                 │
│          │           │                │                      │
│ BPB here │ cluster   │ file/dir       │ actual file contents │
│          │ chain map │ entries        │ stored in clusters   │
└──────────┴───────────┴────────────────┴──────────────────────┘

Using your disk image's values:

┌──────────┬──────────────┬─────────────────────┬───────────────────────────────────────┐
│  Region  │ Start sector │        Size         │             What's there              │
├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
│ Reserved │ 0            │ 4 sectors           │ Boot sector (BPB) at sector 0         │
├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
│ FAT × 2  │ 4            │ 2 × 32 = 64 sectors │ Two copies of the cluster chain table │
├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
│ Root dir │ 68           │ 32 sectors          │ 512 directory entries (fixed size)    │
├──────────┼──────────────┼─────────────────────┼───────────────────────────────────────┤
│ Data     │ 100          │ rest of disk        │ File contents, stored in clusters     │
└──────────┴──────────────┴─────────────────────┴───────────────────────────────────────┘

A file's directory entry stores its first cluster number. To find the rest of the file's data, you use that cluster number as an
index into the FAT:

Directory entry for HELLO.TXT:
first_cluster = 5
file_size = 6000

FAT table:
[0]  0xFFF8   (reserved - media descriptor)
[1]  0xFFFF   (reserved)
[2]  0x0000   (free)
[3]  0x0000   (free)
[4]  0x0000   (free)
[5]  0x0006   ← entry 5 says "next cluster is 6"
[6]  0x0008   ← entry 6 says "next cluster is 8"
[7]  0x0000   (free)
[8]  0xFFFF   ← entry 8 says "end of chain"

So HELLO.TXT's cluster chain is: 5 → 6 → 8 (cluster 7 was free, so the file is fragmented).

 - Directory entry layout (each entry is 32 bytes):

┌────────┬──────┬───────────────────────────────────┐
│ Offset │ Size │               Field               │
├────────┼──────┼───────────────────────────────────┤
│ 0      │ 8    │ Filename (space-padded)           │
├────────┼──────┼───────────────────────────────────┤
│ 8      │ 3    │ Extension (space-padded)          │
├────────┼──────┼───────────────────────────────────┤
│ 11     │ 1    │ Attributes                        │
├────────┼──────┼───────────────────────────────────┤
│ 26     │ 2    │ First cluster (little-endian u16) │
├────────┼──────┼───────────────────────────────────┤
│ 28     │ 4    │ File size (little-endian u32)     │
└────────┴──────┴───────────────────────────────────┘

Special first-byte values:
- 0x00 — entry is empty and no more entries follow
- 0xE5 — entry is deleted

Attribute flags:
- 0x0F — long filename entry
- 0x08 — volume label
*/

#![allow(dead_code)]
use core::ops::ControlFlow;

use alloc::vec::Vec;

use crate::drivers::virtio::{BlkError, read_block};
use crate::kernel::collection::StackVec;
use crate::kernel::sync::SpinLock;

const SECTOR_SIZE: usize = 512;

#[derive(Debug)]
pub enum FsError {
    DeviceError(BlkError),
    InvalidBpb,
    NotFat16,
    NotFound,
    UnsupportedSectorSize,
}

const DIR_ENTRY_BYTES: usize = 32;

// BIOS Parameter Block
struct Bpb {
    sectors_per_cluster: usize,
    reserved_sectors: usize,
    fat_count: usize,
    root_entry_count: usize,
    total_sectors_16: usize,
    sectors_per_fat: usize,
}

impl Bpb {
    pub fn parse(sector: &[u8; SECTOR_SIZE]) -> Result<Bpb, FsError> {
        // FAT16 starts with x86 jump code and byte 54 has informational text
        if (sector[0] != 0xEB && sector[0] != 0xE9) || &sector[54..62] != b"FAT16   " {
            return Err(FsError::NotFat16);
        };

        // FAT16 sectors must be power of two
        if !sector[13].is_power_of_two() || sector[13] > 64 {
            return Err(FsError::InvalidBpb);
        }

        if u16::from_le_bytes([sector[11], sector[12]]) != SECTOR_SIZE as u16 {
            // Expecting 512 byte sectors
            return Err(FsError::UnsupportedSectorSize);
        };

        Ok(Self {
            sectors_per_cluster: sector[13] as usize,
            reserved_sectors: u16::from_le_bytes([sector[14], sector[15]]) as usize,
            fat_count: sector[16] as usize,
            root_entry_count: u16::from_le_bytes([sector[17], sector[18]]) as usize,
            total_sectors_16: u16::from_le_bytes([sector[19], sector[20]]) as usize,
            sectors_per_fat: u16::from_le_bytes([sector[22], sector[23]]) as usize,
        })
    }

    fn fat_start_sector(&self) -> usize {
        self.reserved_sectors
    }

    fn root_dir_start_sector(&self) -> usize {
        self.reserved_sectors + self.fat_count * self.sectors_per_fat
    }

    fn root_dir_sectors(&self) -> usize {
        (self.root_entry_count * DIR_ENTRY_BYTES).div_ceil(SECTOR_SIZE)
    }

    fn data_start_sector(&self) -> usize {
        self.root_dir_start_sector() + self.root_dir_sectors()
    }

    fn cluster_to_sector(&self, cluster: u16) -> usize {
        self.data_start_sector() + (cluster as usize - 2) * self.sectors_per_cluster
    }
}

pub struct Volume {
    bpb: Bpb,
}

impl Volume {
    /// Read from disk, parse BPB, return initialised FAT16 Volume
    pub fn new() -> Result<Self, FsError> {
        let mut buf = [0u8; SECTOR_SIZE];
        read_block(0, &mut buf).map_err(FsError::DeviceError)?;
        let bpb = Bpb::parse(&buf)?;
        Ok(Self { bpb })
    }

    /// Read a single FAT entry for the given cluster number
    ///
    /// The cluster number is the index into the FAT
    fn fat_entry(&self, cluster: u16) -> Result<u16, FsError> {
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        // A FAT is a flat array of u16 bits, one for each FAT cluster.
        // First find which sector the cluster number refers to
        let num_entries = SECTOR_SIZE / core::mem::size_of::<u16>(); // In a 512 byte block there are 256 entries
        let fat_sector = self.bpb.fat_start_sector() + (cluster as usize / num_entries);
        // Now read the block which holds that sector
        read_block(fat_sector as u32, &mut buf).map_err(FsError::DeviceError)?;
        // Work out the offset within the sector
        let offset = (cluster as usize % num_entries) * core::mem::size_of::<u16>();
        // Now look inside the block (in buffer) to read the offset of the entry
        Ok(u16::from_le_bytes([buf[offset], buf[offset + 1]]))
    }

    /// Read entry from root directory
    ///
    /// The method reads sectors from root_dir_start_sector for root_dir_sectors count,
    /// parses each 32-byte chunk, calls f for each
    /// Parsed entry, skips Skip entries, and returns early on End.
    /// Returns a ControlFlow to allow early break
    pub fn read_root_dir<B, F>(&self, mut f: F) -> Result<ControlFlow<B>, FsError>
    where
        F: FnMut(&DirEntry) -> ControlFlow<B>,
    {
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        let start = self.bpb.root_dir_start_sector();
        let count = self.bpb.root_dir_sectors();
        for sector in start..start + count {
            // First read the sector
            read_block(sector as u32, &mut buf).map_err(FsError::DeviceError)?;

            // 16 entries per sector (512 / 32)
            for entry in 0..SECTOR_SIZE / DIR_ENTRY_BYTES {
                let offset = entry * DIR_ENTRY_BYTES;
                let bytes: &[u8; 32] = buf[offset..offset + DIR_ENTRY_BYTES].try_into().unwrap();
                match DirEntry::parse(bytes) {
                    DirParseResult::Parsed(entry) => {
                        if let ControlFlow::Break(val) = f(&entry) {
                            return Ok(ControlFlow::Break(val));
                        }
                    }
                    DirParseResult::Skip => continue,
                    DirParseResult::End => return Ok(ControlFlow::Continue(())),
                }
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Find a file by file name
    pub fn open(&self, filename: &str) -> Result<DirEntry, FsError> {
        match self.read_root_dir(|entry| {
            if let Ok(name) = entry.filename().as_str() {
                if name.eq_ignore_ascii_case(filename) {
                    ControlFlow::Break(entry.clone())
                } else {
                    ControlFlow::Continue(())
                }
            } else {
                ControlFlow::Continue(())
            }
        })? {
            ControlFlow::Break(entry) => Ok(entry),
            ControlFlow::Continue(()) => Err(FsError::NotFound),
        }
    }

    /// Read a file from a given DirEntry
    pub fn read_file(&self, entry: &DirEntry) -> Result<Vec<u8>, FsError> {
        if entry.file_size == 0 {
            return Ok(Vec::new());
        }
        let mut content = Vec::<u8>::new();
        let mut current_cluster = entry.first_cluster;
        let mut buf = [0u8; 512];
        loop {
            let start_sector = self.bpb.cluster_to_sector(current_cluster);
            for s in 0..self.bpb.sectors_per_cluster as u32 {
                read_block(start_sector as u32 + s, &mut buf).map_err(FsError::DeviceError)?;
                content.extend_from_slice(&buf);
            }
            let next_cluster = self.fat_entry(current_cluster)?;

            match next_cluster {
                0xFFF8..=0xFFFF => break,
                _ => current_cluster = next_cluster,
            }
        }
        content.truncate(entry.file_size as usize);
        Ok(content)
    }
}

enum DirParseResult {
    Parsed(DirEntry),
    Skip,
    End,
}

#[derive(Clone)]
pub struct DirEntry {
    filename: [u8; 8],
    extension: [u8; 3],
    attributes: u8,
    first_cluster: u16, // Little Endian
    pub file_size: u32, // Little Endian
}

impl DirEntry {
    /// Parse bytes and returns a DirEntry
    ///
    /// - None for empty/deleted/skipped
    fn parse(bytes: &[u8; 32]) -> DirParseResult {
        // Special first-byte values:
        // - 0x00 — entry is empty and no more entries follow (stop scanning)
        // - 0xE5 — entry is deleted (skip it)
        if bytes[0] == 0x00 {
            return DirParseResult::End;
        }
        if bytes[0] == 0xE5 {
            return DirParseResult::Skip;
        }

        // Attribute flags to skip:
        // - 0x0F — long filename entry (skip)
        // - 0x08 — volume label (skip)
        if bytes[11] == 0x0F || bytes[11] & 0x08 != 0 {
            return DirParseResult::Skip;
        }

        DirParseResult::Parsed(DirEntry {
            filename: bytes[0..8].try_into().unwrap(),
            extension: bytes[8..11].try_into().unwrap(),
            attributes: bytes[11],
            first_cluster: u16::from_le_bytes([bytes[26], bytes[27]]),
            file_size: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
        })
    }

    /// Formats the filename as a string (e.g. "HELLO   TXT" → "HELLO.TXT")
    pub fn filename(&self) -> StackVec<u8, 12> {
        let name = str::from_utf8(&self.filename)
            .expect("should be UTF-8")
            .trim_end();
        let ext = str::from_utf8(&self.extension)
            .expect("should be UTF-8")
            .trim_end();
        let mut full_name = StackVec::<u8, 12>::new();
        for &b in name.as_bytes() {
            let _ = full_name.push(b);
        }
        if !ext.is_empty() {
            let _ = full_name.push(b'.');
            for &b in ext.as_bytes() {
                let _ = full_name.push(b);
            }
        }
        full_name
    }
}

// Need to lock with interrupts enabled waiting for IO completion
// SpinLock (not IrqSpinLock) because I/O needs interrupts enabled for virtio
// completion. Must never be accessed from an interrupt handler.
static VOLUME: SpinLock<Option<Volume>> = SpinLock::new(None);

pub fn fat16_init() {
    let vol = Volume::new().expect("FAT16 init failed");
    *VOLUME.lock() = Some(vol);
}

pub fn with_volume<F, R>(f: F) -> R
where
    F: FnOnce(&mut Volume) -> R,
{
    let mut guard = VOLUME.lock();
    let vol = guard.as_mut().expect("FAT16 not initialized");
    f(vol)
}

#[cfg(test)]
mod test {
    use super::*;

    // =========================================================================
    // Helpers
    // =========================================================================

    /// Build a minimal valid FAT16 BPB sector with known values
    fn make_test_bpb() -> [u8; SECTOR_SIZE] {
        let mut sector = [0u8; SECTOR_SIZE];
        // Jump boot code
        sector[0] = 0xEB;
        sector[1] = 0x3C;
        sector[2] = 0x90;
        // Bytes per sector = 512 (little-endian)
        sector[11] = 0x00;
        sector[12] = 0x02;
        // Sectors per cluster = 4
        sector[13] = 4;
        // Reserved sectors = 4 (little-endian)
        sector[14] = 0x04;
        sector[15] = 0x00;
        // FAT count = 2
        sector[16] = 2;
        // Root entry count = 512 (little-endian)
        sector[17] = 0x00;
        sector[18] = 0x02;
        // Total sectors = 32768 (little-endian)
        sector[19] = 0x00;
        sector[20] = 0x80;
        // Sectors per FAT = 32 (little-endian)
        sector[22] = 0x20;
        sector[23] = 0x00;
        // FS type at offset 54
        sector[54..62].copy_from_slice(b"FAT16   ");
        sector
    }

    /// Build a 32-byte directory entry with the given 8.3 name, attributes,
    /// first cluster, and file size.
    fn make_dir_entry(
        name: &[u8; 8],
        ext: &[u8; 3],
        attrs: u8,
        cluster: u16,
        size: u32,
    ) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(name);
        e[8..11].copy_from_slice(ext);
        e[11] = attrs;
        e[26..28].copy_from_slice(&cluster.to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        e
    }

    // =========================================================================
    // Bpb tests
    // =========================================================================

    #[test_case]
    fn bpb_parse_valid() {
        let sector = make_test_bpb();
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sectors, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.root_entry_count, 512);
        assert_eq!(bpb.total_sectors_16, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
    }

    #[test_case]
    fn bpb_rejects_bad_jump_code() {
        let mut sector = make_test_bpb();
        sector[0] = 0x00;
        assert!(Bpb::parse(&sector).is_err());
    }

    #[test_case]
    fn bpb_rejects_missing_fat16_marker() {
        let mut sector = make_test_bpb();
        sector[54..62].copy_from_slice(b"FAT12   ");
        assert!(Bpb::parse(&sector).is_err());
    }

    #[test_case]
    fn bpb_rejects_wrong_sector_size() {
        let mut sector = make_test_bpb();
        // Set bytes_per_sector to 1024
        sector[11] = 0x00;
        sector[12] = 0x04;
        assert!(Bpb::parse(&sector).is_err());
    }

    #[test_case]
    fn bpb_derived_geometry() {
        let sector = make_test_bpb();
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.fat_start_sector(), 4);
        assert_eq!(bpb.root_dir_start_sector(), 68);
        assert_eq!(bpb.root_dir_sectors(), 32);
        assert_eq!(bpb.data_start_sector(), 100);
        assert_eq!(bpb.cluster_to_sector(2), 100);
        assert_eq!(bpb.cluster_to_sector(3), 104);
    }

    #[test_case]
    fn bpb_accepts_alternate_jump_code() {
        let mut sector = make_test_bpb();
        sector[0] = 0xE9; // Second valid jump opcode
        assert!(Bpb::parse(&sector).is_ok());
    }

    // =========================================================================
    // DirEntry::parse tests
    // =========================================================================

    #[test_case]
    fn dir_entry_parse_normal_file() {
        let bytes = make_dir_entry(b"HELLO   ", b"TXT", 0x20, 5, 1234);
        match DirEntry::parse(&bytes) {
            DirParseResult::Parsed(entry) => {
                assert_eq!(&entry.filename, b"HELLO   ");
                assert_eq!(&entry.extension, b"TXT");
                assert_eq!(entry.attributes, 0x20);
                assert_eq!(entry.first_cluster, 5);
                assert_eq!(entry.file_size, 1234);
            }
            _ => panic!("expected Parsed"),
        }
    }

    #[test_case]
    fn dir_entry_parse_no_extension() {
        let bytes = make_dir_entry(b"README  ", b"   ", 0x20, 3, 100);
        match DirEntry::parse(&bytes) {
            DirParseResult::Parsed(entry) => {
                assert_eq!(&entry.filename, b"README  ");
                assert_eq!(&entry.extension, b"   ");
            }
            _ => panic!("expected Parsed"),
        }
    }

    #[test_case]
    fn dir_entry_parse_empty_entry_returns_end() {
        let bytes = [0u8; 32];
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::End));
    }

    #[test_case]
    fn dir_entry_parse_deleted_entry_returns_skip() {
        let mut bytes = make_dir_entry(b"OLD     ", b"TXT", 0x20, 2, 50);
        bytes[0] = 0xE5; // Mark as deleted
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    #[test_case]
    fn dir_entry_parse_lfn_entry_returns_skip() {
        let mut bytes = [0x42u8; 32]; // Non-zero first byte
        bytes[11] = 0x0F; // LFN attribute
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    #[test_case]
    fn dir_entry_parse_volume_label_returns_skip() {
        let bytes = make_dir_entry(b"MOSSVOL ", b"   ", 0x08, 0, 0);
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    #[test_case]
    fn dir_entry_parse_volume_label_with_other_attrs_returns_skip() {
        // Volume label bit set alongside archive bit
        let bytes = make_dir_entry(b"MOSSVOL ", b"   ", 0x28, 0, 0);
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    // =========================================================================
    // DirEntry::filename tests
    // =========================================================================

    #[test_case]
    fn filename_with_extension() {
        let bytes = make_dir_entry(b"HELLO   ", b"TXT", 0x20, 5, 100);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("HELLO.TXT"));
        }
    }

    #[test_case]
    fn filename_no_extension() {
        let bytes = make_dir_entry(b"README  ", b"   ", 0x20, 3, 100);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("README"));
        }
    }

    #[test_case]
    fn filename_full_length() {
        let bytes = make_dir_entry(b"12345678", b"ABC", 0x20, 2, 50);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("12345678.ABC"));
        }
    }

    #[test_case]
    fn filename_short_name_short_ext() {
        let bytes = make_dir_entry(b"A       ", b"C  ", 0x20, 2, 10);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("A.C"));
        }
    }

    // =========================================================================
    // Disk image tests (require QEMU + virtio-blk)
    // =========================================================================

    #[test_case]
    fn disk_image_bpb() {
        let mut buf = [0u8; SECTOR_SIZE];
        crate::drivers::virtio::read_block(0, &mut buf).unwrap();
        let bpb = Bpb::parse(&buf).unwrap();
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sectors, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.root_entry_count, 512);
        assert_eq!(bpb.total_sectors_16, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
        assert_eq!(bpb.data_start_sector(), 100);
    }

    #[test_case]
    fn disk_image_fat_entry_reserved() {
        // FAT entries 0 and 1 are reserved
        with_volume(|vol| {
            let entry0 = vol.fat_entry(0).unwrap();
            assert!(
                entry0 >= 0xFFF8,
                "FAT[0] should be media descriptor, got {:#06x}",
                entry0
            );
            let entry1 = vol.fat_entry(1).unwrap();
            assert!(
                entry1 >= 0xFFF8,
                "FAT[1] should be reserved/EOC, got {:#06x}",
                entry1
            );
        });
    }

    #[test_case]
    fn disk_image_fat_entry_hello_txt() {
        // HELLO.TXT is a small file — its first cluster should be end-of-chain
        with_volume(|vol| {
            // Find HELLO.TXT's first cluster from the directory
            let result = vol
                .read_root_dir(|entry| {
                    if entry.filename().as_str() == Ok("HELLO.TXT") {
                        ControlFlow::Break(entry.first_cluster)
                    } else {
                        ControlFlow::Continue(())
                    }
                })
                .unwrap();
            let first_cluster = match result {
                ControlFlow::Break(c) => c,
                ControlFlow::Continue(()) => panic!("HELLO.TXT not found"),
            };
            assert!(first_cluster >= 2, "invalid cluster");
            // Small file should be a single cluster (end-of-chain)
            let next = vol.fat_entry(first_cluster).unwrap();
            assert!(
                next >= 0xFFF8,
                "expected EOC for small file, got {:#06x}",
                next
            );
        });
    }

    #[test_case]
    fn disk_image_root_dir_contains_hello_txt() {
        with_volume(|vol| {
            let result = vol
                .read_root_dir(|entry| {
                    if entry.filename().as_str() == Ok("HELLO.TXT") {
                        assert!(entry.file_size > 0, "HELLO.TXT should not be empty");
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                })
                .unwrap();
            assert!(
                matches!(result, ControlFlow::Break(())),
                "HELLO.TXT not found in root directory"
            );
        });
    }

    #[test_case]
    fn disk_image_root_dir_no_volume_label_entries() {
        // read_root_dir should skip volume labels — none should come through
        with_volume(|vol| {
            let _ = vol
                .read_root_dir(|entry| {
                    assert_eq!(
                        entry.attributes & 0x08,
                        0,
                        "volume label entry should not be returned"
                    );
                    ControlFlow::<()>::Continue(())
                })
                .unwrap();
        });
    }

    // =========================================================================
    // Volume::open tests
    // =========================================================================

    #[test_case]
    fn open_finds_hello_txt() {
        with_volume(|vol| {
            let entry = vol.open("HELLO.TXT").unwrap();
            assert!(entry.file_size > 0);
        });
    }

    #[test_case]
    fn open_case_insensitive() {
        with_volume(|vol| {
            let entry = vol.open("hello.txt").unwrap();
            assert!(entry.file_size > 0);
        });
    }

    #[test_case]
    fn open_not_found() {
        with_volume(|vol| {
            let result = vol.open("NOPE.TXT");
            assert!(matches!(result, Err(FsError::NotFound)));
        });
    }

    #[test_case]
    fn open_empty_file() {
        with_volume(|vol| {
            let entry = vol.open("EMPTY.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn open_no_extension() {
        with_volume(|vol| {
            let entry = vol.open("SHORT").unwrap();
            assert!(entry.file_size > 0);
        });
    }

    // =========================================================================
    // Volume::read_file tests
    // =========================================================================

    #[test_case]
    fn read_file_hello_txt() {
        with_volume(|vol| {
            let entry = vol.open("HELLO.TXT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            let text = core::str::from_utf8(&content).unwrap();
            assert_eq!(text, "Text file contents\n");
        });
    }

    #[test_case]
    fn read_file_empty() {
        with_volume(|vol| {
            let entry = vol.open("EMPTY.TXT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 0);
        });
    }

    #[test_case]
    fn read_file_short() {
        with_volume(|vol| {
            let entry = vol.open("SHORT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            let text = core::str::from_utf8(&content).unwrap();
            assert_eq!(text, "This is a file with a short name.\n");
        });
    }

    #[test_case]
    fn read_file_size_matches_dir_entry() {
        with_volume(|vol| {
            let entry = vol.open("HELLO.TXT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), entry.file_size as usize);
        });
    }

    #[test_case]
    fn read_file_longname() {
        with_volume(|vol| {
            let entry = vol.open("LONGNAME.END").unwrap();
            let content = vol.read_file(&entry).unwrap();
            let text = core::str::from_utf8(&content).unwrap();
            assert_eq!(text, "This is a file with a long name.\n");
        });
    }

    #[test_case]
    fn open_multi_cluster_file() {
        // The PDF is ~7.9MB — too large to read into heap, but verify open finds it
        // and the dir entry has the expected size.
        with_volume(|vol| {
            let entry = vol.open("RP-008~1.PDF").unwrap();
            assert_eq!(entry.file_size, 7_968_417);
        });
    }
}
