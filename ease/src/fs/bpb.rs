//! BIOS Parameter Block

/*
 * The BPB is at sector 0 of a Reserved region
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
 * │ 17     │ 2    │ root dir entry count (FAT16 capacity)    | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 19     │ 2    │ total sectors (16-bit) or 0 (32-bit)     | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 21     │ 1    │ media descriptor                         | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 22     │ 2    │ sectors per FAT (16-bit) or 0 (32-bit)   | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 24–31  │ 8    │ sectors/track, heads, hidden sectors     | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 32     │ 4    │ total sectors (32-bit)                   | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 36     │ 4    │ sectors per FAT (32-bit)                 | parsed          │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 40–53  │ 18   │ drive no, boot sig, vol serial + label   | skipped         │
 * ├────────┼──────┼──────────────────────────────────────────┼─────────────────┤
 * │ 54     │ 8    │ filesystem type string e.g. "FAT16   "   | skipped         │
 * └────────┴──────┴──────────────────────────────────────────┴─────────────────┘
 *
 * The OEM name and type string are ignored as they are not diagnostic.
 *
 */

use crate::fs::SECTOR_SIZE;

use super::BOOT_SECTOR_SIG;
use super::VolumeType;
use super::dir;

const FAT_X86_JUMP_OPCODES: [u8; 2] = [0xEB, 0xE9];
const FAT16_MIN: u32 = 4085;
const FAT16_MAX: u32 = 65524;
const SECTORS_PER_CLUSTER_MAX: u32 = 64;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BpbError {
    InvalidBpb,
    UnknownFileSystem,
    UnsupportedFileSystem,
    UnsupportedSectorSize,
}

// BIOS Parameter Block
// Used by both FAT12 and FAT32 and holds the parsed BPB fields
pub(crate) struct Bpb {
    pub(super) volume_type: VolumeType,
    pub(super) total_sectors: u32,
    pub(super) sectors_per_cluster: u32,
    reserved_sector_count: u32,
    pub(super) fat_count: u32,
    pub(super) sectors_per_fat: u32,
}

impl Bpb {
    pub fn parse(sector: &[u8; SECTOR_SIZE]) -> Result<Bpb, BpbError> {
        // FAT (and MBR) have a signature at end of first sector
        if sector[SECTOR_SIZE - 2..] != BOOT_SECTOR_SIG {
            return Err(BpbError::UnknownFileSystem);
        }
        // Check for FAT op code
        if !FAT_X86_JUMP_OPCODES.contains(&sector[0]) {
            return Err(BpbError::UnknownFileSystem);
        }
        // This looks like a BPB - validate key fields
        // Only work with 512 byte sectors
        if u16::from_le_bytes([sector[11], sector[12]]) != SECTOR_SIZE as u16 {
            return Err(BpbError::UnsupportedSectorSize);
        };
        // Clusters must be power of two contiguous sectors up to 64
        let sectors_per_cluster = sector[13] as u32;
        if !sectors_per_cluster.is_power_of_two() || sectors_per_cluster > SECTORS_PER_CLUSTER_MAX {
            return Err(BpbError::InvalidBpb);
        }
        let mut total_sectors = u16::from_le_bytes([sector[19], sector[20]]) as u32;
        if total_sectors == 0 {
            // Get 32-bit total
            total_sectors = u32::from_le_bytes([sector[32], sector[33], sector[34], sector[35]]);
            if total_sectors == 0 {
                return Err(BpbError::InvalidBpb);
            }
        }
        let mut sectors_per_fat = u16::from_le_bytes([sector[22], sector[23]]) as u32;
        if sectors_per_fat == 0 {
            // Get 32-bit total
            sectors_per_fat = u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]);
            if sectors_per_fat == 0 {
                return Err(BpbError::InvalidBpb);
            }
        }
        let fat16_root_dir_capacity = u16::from_le_bytes([sector[17], sector[18]]) as u32; // Zero for FAT32
        let fat16_root_dir_sector_count =
            (fat16_root_dir_capacity * dir::ENTRY_BYTES as u32).div_ceil(SECTOR_SIZE as u32);
        let reserved_sector_count = u16::from_le_bytes([sector[14], sector[15]]) as u32;
        let fat_count = sector[16] as u32;
        let fat_sector_count = fat_count * sectors_per_fat;
        // Microsoft formula for determining FAT12/FAT16/FAT32
        let data_sectors = total_sectors
            .saturating_sub(reserved_sector_count + fat_sector_count + fat16_root_dir_sector_count);
        let cluster_count = data_sectors / sectors_per_cluster;
        // Now match on cluster_count
        let volume_type = match cluster_count {
            0..FAT16_MIN => return Err(BpbError::UnsupportedFileSystem), // Don't support FAT12
            FAT16_MIN..=FAT16_MAX => {
                let fat16_root_dir_start_sector = reserved_sector_count + fat_sector_count;
                VolumeType::Fat16((fat16_root_dir_start_sector, fat16_root_dir_sector_count))
            }
            _ => {
                let root_dir_cluster_start =
                    u32::from_le_bytes([sector[44], sector[45], sector[46], sector[47]]);
                if root_dir_cluster_start == 0 {
                    return Err(BpbError::InvalidBpb);
                }
                VolumeType::Fat32(root_dir_cluster_start)
            }
        };
        Ok(Self {
            volume_type,
            total_sectors,
            sectors_per_cluster,
            reserved_sector_count,
            fat_count,
            sectors_per_fat,
        })
    }

    pub(super) fn fat_start_sector(&self) -> u32 {
        self.reserved_sector_count // FAT region starts immediately after reserved region
    }

    fn data_start_sector(&self) -> u32 {
        match self.volume_type {
            VolumeType::Fat16((root_dir_start_sector, root_dir_sector_count)) => {
                root_dir_start_sector + root_dir_sector_count // FAT16 data follows the root dir
            }
            VolumeType::Fat32(_) => {
                self.reserved_sector_count + self.fat_count * self.sectors_per_fat // FAT32 data follows the FAT
            }
        }
    }

    // Converts a cluster number to the sector number (first sector of that cluster)
    pub(super) fn cluster_to_sector(&self, cluster: u32) -> u32 {
        debug_assert!(cluster > 1, "cluster index too small");
        // FAT entry 0 holds the media descriptor, entry 1 is a reserved end-of-chain marker
        self.data_start_sector() + (cluster - 2) * self.sectors_per_cluster
    }

    pub(super) fn total_data_clusters(&self) -> u32 {
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
        assert_eq!(bpb.reserved_sector_count, 4);
        assert_eq!(bpb.fat_count, 2);
        let VolumeType::Fat16((_, root_dir_sector_count)) = bpb.volume_type else {
            panic!("expecting FAT16");
        };
        assert_eq!(
            root_dir_sector_count as usize * SECTOR_SIZE / dir::ENTRY_BYTES,
            512
        );
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
        let VolumeType::Fat16((root_dir_start_sector, root_dir_sector_count)) = bpb.volume_type
        else {
            panic!("should be FAT16")
        };
        assert_eq!(root_dir_start_sector, 68);
        assert_eq!(root_dir_sector_count, 32);
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
    fn make_edge_bpb_fat16(total_sectors: u16) -> [u8; SECTOR_SIZE] {
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

    /// Build a minimal FAT32-shaped BPB that forces the 16→32-bit fallback.
    /// The 16-bit total-sectors (offset 19) and sectors-per-FAT (offset 22)
    /// fields are zero, so the parser must read the 32-bit fields at offsets
    /// 32 and 36 to recover the real values. Root entry count is 0, as a real
    /// FAT32 volume has no fixed root directory. With 1 sector/cluster, 1
    /// reserved, 1 FAT of 1 sector and no root-dir region, overhead is 2
    /// sectors, so cluster_count = total_sectors - 2.
    fn make_edge_bpb_fat32(total_sectors: u32) -> [u8; SECTOR_SIZE] {
        let mut sector = make_test_bpb();
        sector[13] = 1; // sectors per cluster
        sector[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved
        sector[16] = 1; // FAT count
        sector[17..19].copy_from_slice(&0u16.to_le_bytes()); // root entries = 0 (FAT32)
        sector[19..21].copy_from_slice(&0u16.to_le_bytes()); // 16-bit total = 0 => fallback
        sector[22..24].copy_from_slice(&0u16.to_le_bytes()); // 16-bit sectors/FAT = 0 => fallback
        sector[32..36].copy_from_slice(&total_sectors.to_le_bytes()); // 32-bit total
        sector[36..40].copy_from_slice(&1u32.to_le_bytes()); // 32-bit sectors/FAT
        sector[44..48].copy_from_slice(&2u32.to_le_bytes()); // root dir cluster = 2
        sector
    }

    const EDGE_OVERHEAD_FAT32: u32 = 2; // reserved 1 + FAT 1 (no fixed root dir)

    /// Build a realistic FAT32 BPB with distinct, non-degenerate geometry so
    /// the derived-geometry test genuinely exercises the cluster→sector math
    /// (a fixture with 1 sector/cluster and reserved == root cluster would let
    /// several wrong formulas coincide on the right answer). Values: 8
    /// sectors/cluster, 32 reserved, 2 FATs of 100 sectors each, no fixed root
    /// directory, root directory at cluster 2, 600000 total sectors.
    fn make_fat32_bpb() -> [u8; SECTOR_SIZE] {
        let mut sector = make_test_bpb();
        sector[13] = 8; // sectors per cluster
        sector[14..16].copy_from_slice(&32u16.to_le_bytes()); // reserved
        sector[16] = 2; // FAT count
        sector[17..19].copy_from_slice(&0u16.to_le_bytes()); // root entries = 0 (FAT32)
        sector[19..21].copy_from_slice(&0u16.to_le_bytes()); // 16-bit total = 0 => fallback
        sector[22..24].copy_from_slice(&0u16.to_le_bytes()); // 16-bit sectors/FAT = 0 => fallback
        sector[32..36].copy_from_slice(&600_000u32.to_le_bytes()); // 32-bit total
        sector[36..40].copy_from_slice(&100u32.to_le_bytes()); // 32-bit sectors/FAT
        sector[44..48].copy_from_slice(&2u32.to_le_bytes()); // root dir cluster = 2
        sector
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_rejects_fat12_cluster_count() {
        // 4084 clusters is the top of the FAT12 band
        let sector = make_edge_bpb_fat16(4084 + EDGE_OVERHEAD);
        assert!(Bpb::parse(&sector).is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_accepts_min_fat16_cluster_count() {
        // 4085 clusters is the bottom of the FAT16 band
        let sector = make_edge_bpb_fat16(4085 + EDGE_OVERHEAD);
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.total_data_clusters(), 4085);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_accepts_max_fat16_cluster_count() {
        // 65524 clusters is the top of the FAT16 band
        let sector = make_edge_bpb_fat16(65524 + EDGE_OVERHEAD);
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.total_data_clusters(), 65524);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_detects_fat32_via_32bit_fallback_fields() {
        // A genuine FAT32 layout: the 16-bit total-sectors and sectors-per-FAT
        // fields are zero, so the real values are only reachable through the
        // 32-bit fallback fields. 65525 clusters is the bottom of the FAT32
        // band. This exercises the fallback path itself, not just the
        // cluster-count band boundary.
        let sector = make_edge_bpb_fat32(65525 + EDGE_OVERHEAD_FAT32);
        let bpb = Bpb::parse(&sector).unwrap();
        assert!(matches!(bpb.volume_type, VolumeType::Fat32(_)));
        // Confirm the parser actually recovered the wide-field values
        assert_eq!(bpb.total_sectors, 65525 + EDGE_OVERHEAD_FAT32);
        assert_eq!(bpb.sectors_per_fat, 1);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_rejects_fat32_zero_root_cluster() {
        // A FAT32 volume must name a root-directory cluster; zero is invalid.
        let mut sector = make_edge_bpb_fat32(65525 + EDGE_OVERHEAD_FAT32);
        sector[44..48].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(Bpb::parse(&sector), Err(BpbError::InvalidBpb)));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn bpb_fat32_derived_geometry() {
        // Realistic FAT32 geometry: 8 sectors/cluster, 32 reserved, 2 FATs of
        // 100 sectors. The data region begins right after the FATs (no fixed
        // root-directory region), and the root directory is cluster 2, which
        // is the first data cluster and so lands exactly at data_start.
        let sector = make_fat32_bpb();
        let bpb = Bpb::parse(&sector).unwrap();
        assert!(matches!(bpb.volume_type, VolumeType::Fat32(_)));
        assert_eq!(bpb.fat_start_sector(), 32); // after reserved region
        assert_eq!(bpb.data_start_sector(), 232); // 32 + 2 * 100
        let VolumeType::Fat32(root_dir_start_cluster) = bpb.volume_type else {
            panic!("should be FAT32")
        };
        assert_eq!(bpb.cluster_to_sector(root_dir_start_cluster), 232); // cluster 2 == data_start
        assert_eq!(bpb.cluster_to_sector(2), 232);
        assert_eq!(bpb.cluster_to_sector(3), 240); // one cluster (8 sectors) later
        assert_eq!(bpb.total_data_clusters(), 74971); // (600000 - 232) / 8
    }

    // =========================================================================
    // Disk image tests (kernel-only — require QEMU + virtio-blk)
    // =========================================================================

    #[test_case]
    #[cfg(target_os = "none")]
    #[cfg(feature = "fat16")]
    fn disk_image_bpb() {
        let mut buf = [0u8; SECTOR_SIZE];
        crate::drivers::virtio::blk::read_block(0, &mut buf).unwrap();
        let bpb = Bpb::parse(&buf).unwrap();
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sector_count, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.total_sectors, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
        assert_eq!(bpb.data_start_sector(), 100);
    }

    #[test_case]
    #[cfg(target_os = "none")]
    #[cfg(feature = "fat32")]
    fn disk_image_bpb() {
        // The FAT32 image is MBR-partitioned, so the BPB lives at the partition
        // start (LBA 2048), not sector 0 (which holds the MBR).
        let mut buf = [0u8; SECTOR_SIZE];
        crate::drivers::virtio::blk::read_block(2048, &mut buf).unwrap();
        let bpb = Bpb::parse(&buf).unwrap();
        assert!(matches!(bpb.volume_type, VolumeType::Fat32(2)));
        assert_eq!(bpb.sectors_per_cluster, 1);
        assert_eq!(bpb.reserved_sector_count, 32);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.total_sectors, 194560);
        assert_eq!(bpb.sectors_per_fat, 1497);
        assert_eq!(bpb.data_start_sector(), 3026);
    }
}
