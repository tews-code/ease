//! BIOS Parameter Block

/* Reserved (boot) sector
 *
 * BPB is in sector 0 of the Reserved region (sectors 0-3)
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
 * │ 54     │ 8    │ filesystem type string "FAT16   "        | ignored         │
 * └────────┴──────┴──────────────────────────────────────────┴─────────────────┘
 */

/// BIOS Parameter Block for FAT16
use crate::fs::{FsError, SECTOR_SIZE};

use super::BOOT_SECTOR_SIG;
use super::DIR_ENTRY_BYTES;

const X86_JUMP_OPCODES: [u8; 2] = [0xEB, 0xE9];
const FAT16_MIN: usize = 4085;
const FAT16_MAX: usize = 65524;

// BIOS Parameter Block
// Holds the parsed BPB fields, widened to usize for geometry arithmetic
pub(super) struct Bpb {
    pub(super) sectors_per_cluster: usize,
    reserved_sectors: usize,
    pub(super) fat_count: usize,
    root_dir_entry_count: usize,
    total_sectors: usize,
    pub(super) sectors_per_fat: usize,
}

impl Bpb {
    pub fn parse(sector: &[u8; SECTOR_SIZE]) -> Result<Bpb, FsError> {
        // FAT16 starts with one of two x86 jump codes
        if !X86_JUMP_OPCODES.contains(&sector[0]) {
            return Err(FsError::NotFat16);
        };
        // FAT16 signature at end of first sector
        if sector[SECTOR_SIZE - 2..] != BOOT_SECTOR_SIG {
            return Err(FsError::NotFat16);
        }
        if u16::from_le_bytes([sector[11], sector[12]]) != SECTOR_SIZE as u16 {
            // Expecting 512 byte sectors
            return Err(FsError::UnsupportedSectorSize);
        };
        let sectors_per_cluster = sector[13] as usize;
        // FAT16 sectors must be power of two
        if !sectors_per_cluster.is_power_of_two() || sectors_per_cluster > 64 {
            return Err(FsError::InvalidBpb);
        }
        let root_dir_entry_count = u16::from_le_bytes([sector[17], sector[18]]) as usize;
        let sectors_per_fat = u16::from_le_bytes([sector[22], sector[23]]) as usize;
        if sectors_per_fat == 0 || root_dir_entry_count == 0 {
            // FAT32
            return Err(FsError::NotFat16);
        }
        let total_sectors = u16::from_le_bytes([sector[19], sector[20]]) as usize;
        if total_sectors == 0 {
            return Err(FsError::InvalidBpb);
        }
        let reserved_sectors = u16::from_le_bytes([sector[14], sector[15]]) as usize;
        let fat_count = sector[16] as usize;
        // Microsoft formula for determining FAT12/FAT16/FAT32
        let data_sectors = total_sectors.saturating_sub(
            reserved_sectors
                + fat_count * sectors_per_fat
                + (root_dir_entry_count * DIR_ENTRY_BYTES).div_ceil(SECTOR_SIZE),
        );
        let cluster_count = data_sectors / sectors_per_cluster;
        if !(FAT16_MIN..=FAT16_MAX).contains(&cluster_count) {
            return Err(FsError::NotFat16);
        }
        Ok(Self {
            sectors_per_cluster,
            reserved_sectors,
            fat_count,
            root_dir_entry_count,
            total_sectors,
            sectors_per_fat,
        })
    }

    pub(super) fn fat_start_sector(&self) -> usize {
        self.reserved_sectors // FAT starts after reserved region
    }

    pub(super) fn root_dir_start_sector(&self) -> usize {
        self.reserved_sectors + self.fat_count * self.sectors_per_fat
    }

    pub(super) fn root_dir_sectors(&self) -> usize {
        (self.root_dir_entry_count * DIR_ENTRY_BYTES).div_ceil(SECTOR_SIZE)
    }

    fn data_start_sector(&self) -> usize {
        self.root_dir_start_sector() + self.root_dir_sectors()
    }

    pub(super) fn cluster_to_sector(&self, cluster: u16) -> usize {
        self.data_start_sector() + (cluster as usize - 2) * self.sectors_per_cluster
    }

    pub(super) fn total_data_clusters(&self) -> usize {
        (self.total_sectors - self.data_start_sector()) / self.sectors_per_cluster
    }
}

// Bpb parser tests. The synthetic-sector tests below are pure logic
// and run in BOTH contexts:
//   - Kernel target (`cargo test --bin ease`): each test gets the
//     custom `#[test_case]` attribute and runs in QEMU.
//   - Host (`cargo test --lib`): each test gets the standard `#[test]`
//     attribute and runs natively in milliseconds.
// `cfg_attr` selects the right attribute per target. The `disk_image`
// test at the bottom uses real virtio and is gated kernel-only.
#[cfg(all(test, feature = "test-fs"))]
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
        // FS signature
        sector[SECTOR_SIZE - 2] = 0x55;
        sector[SECTOR_SIZE - 1] = 0xAA;
        sector
    }

    // =========================================================================
    // Bpb tests (run in both QEMU and host)
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_parse_valid() {
        let sector = make_test_bpb();
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sectors, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.root_dir_entry_count, 512);
        assert_eq!(bpb.total_sectors, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_rejects_bad_jump_code() {
        let mut sector = make_test_bpb();
        sector[0] = 0x00;
        assert!(Bpb::parse(&sector).is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_rejects_wrong_sector_size() {
        let mut sector = make_test_bpb();
        // Set bytes_per_sector to 1024
        sector[11] = 0x00;
        sector[12] = 0x04;
        assert!(Bpb::parse(&sector).is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
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

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_accepts_alternate_jump_code() {
        let mut sector = make_test_bpb();
        sector[0] = 0xE9; // Second valid jump opcode
        assert!(Bpb::parse(&sector).is_ok());
    }

    // =========================================================================
    // FAT type determination edges (spec constants 4085 and 65524)
    // =========================================================================

    /// Build a BPB with minimal overhead so `total_sectors` steers the
    /// cluster count directly: 1 sector/cluster, 1 reserved, 1 FAT of 1
    /// sector, 16 root entries (1 sector) => overhead 3 sectors, so
    /// cluster_count = total_sectors - 3. The FAT is far too small to
    /// actually map that many clusters — the parser doesn't check FAT
    /// capacity, and these tests pin only the type-determination bands.
    fn make_edge_bpb(total_sectors: u16) -> [u8; SECTOR_SIZE] {
        let mut sector = make_test_bpb();
        sector[13] = 1; // sectors per cluster
        sector[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved
        sector[16] = 1; // FAT count
        sector[17..19].copy_from_slice(&16u16.to_le_bytes()); // root entries
        sector[19..21].copy_from_slice(&total_sectors.to_le_bytes());
        sector[22..24].copy_from_slice(&1u16.to_le_bytes()); // sectors per FAT
        sector
    }

    const EDGE_OVERHEAD: u16 = 3; // reserved 1 + FAT 1 + root dir 1

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_rejects_fat12_cluster_count() {
        // 4084 clusters is the top of the FAT12 band
        let sector = make_edge_bpb(4084 + EDGE_OVERHEAD);
        assert!(Bpb::parse(&sector).is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_accepts_min_fat16_cluster_count() {
        // 4085 clusters is the bottom of the FAT16 band
        let sector = make_edge_bpb(4085 + EDGE_OVERHEAD);
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.total_data_clusters(), 4085);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_accepts_max_fat16_cluster_count() {
        // 65524 clusters is the top of the FAT16 band
        let sector = make_edge_bpb(65524 + EDGE_OVERHEAD);
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.total_data_clusters(), 65524);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_rejects_fat32_cluster_count() {
        // 65525 clusters is the bottom of the FAT32 band
        let sector = make_edge_bpb(65525 + EDGE_OVERHEAD);
        assert!(Bpb::parse(&sector).is_err());
    }

    // =========================================================================
    // Disk image tests (kernel-only — require QEMU + virtio-blk)
    // =========================================================================

    #[cfg(target_os = "none")]
    #[test_case]
    fn disk_image_bpb() {
        let mut buf = [0u8; SECTOR_SIZE];
        crate::drivers::virtio::blk::read_block(0, &mut buf).unwrap();
        let bpb = Bpb::parse(&buf).unwrap();
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sectors, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.root_dir_entry_count, 512);
        assert_eq!(bpb.total_sectors, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
        assert_eq!(bpb.data_start_sector(), 100);
    }
}
