//! FAT16 File System for EASE

#![allow(dead_code)]

use crate::fs::FsError;
use crate::hal::BLOCK_SIZE;

const DIR_ENTRY_BYTES: usize = 32;

// BIOS Parameter Block
struct Bpb {
    sector_size: usize,
    sectors_per_cluster: usize,
    reserved_sectors: usize,
    fat_count: usize,
    root_entry_count: usize,
    total_sectors_16: usize,
    sectors_per_fat: usize,
}

impl Bpb {
    pub fn parse(sector: &[u8; BLOCK_SIZE]) -> Result<Bpb, FsError> {
        // FAT16 starts with x86 jump code and byte 54 has identifier
        if (sector[0] != 0xEB && sector[0] != 0xE9) || &sector[54..62] != b"FAT16   " {
            return Err(FsError::NotFat16);
        };

        if u16::from_le_bytes([sector[11], sector[12]]) != BLOCK_SIZE as u16 {
            // Expecting 512 byte sectors
            return Err(FsError::DeviceError);
        };

        Ok(Self {
            sector_size: BLOCK_SIZE,
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
        (self.root_entry_count * DIR_ENTRY_BYTES).div_ceil(self.sector_size)
    }

    fn data_start_sector(&self) -> usize {
        self.root_dir_start_sector() + self.root_dir_sectors()
    }

    fn cluster_to_sector(&self, cluster: usize) -> usize {
        self.data_start_sector() + (cluster - 2) * self.sectors_per_cluster
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Build a minimal valid FAT16 BPB sector with known values
    fn make_test_bpb() -> [u8; BLOCK_SIZE] {
        let mut sector = [0u8; BLOCK_SIZE];
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

    #[test_case]
    fn parse_valid_bpb() {
        let sector = make_test_bpb();
        let bpb = Bpb::parse(&sector).unwrap();
        assert_eq!(bpb.sector_size, 512);
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sectors, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.root_entry_count, 512);
        assert_eq!(bpb.total_sectors_16, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
    }

    #[test_case]
    fn parse_rejects_bad_jump_code() {
        let mut sector = make_test_bpb();
        sector[0] = 0x00;
        assert!(Bpb::parse(&sector).is_err());
    }

    #[test_case]
    fn parse_rejects_missing_fat16_marker() {
        let mut sector = make_test_bpb();
        sector[54..62].copy_from_slice(b"FAT12   ");
        assert!(Bpb::parse(&sector).is_err());
    }

    #[test_case]
    fn parse_rejects_wrong_sector_size() {
        let mut sector = make_test_bpb();
        // Set bytes_per_sector to 1024
        sector[11] = 0x00;
        sector[12] = 0x04;
        assert!(Bpb::parse(&sector).is_err());
    }

    #[test_case]
    fn derived_geometry() {
        let sector = make_test_bpb();
        let bpb = Bpb::parse(&sector).unwrap();
        // fat_start = reserved_sectors = 4
        assert_eq!(bpb.fat_start_sector(), 4);
        // root_dir_start = 4 + 2*32 = 68
        assert_eq!(bpb.root_dir_start_sector(), 68);
        // root_dir_sectors = (512*32 + 511)/512 = 32
        assert_eq!(bpb.root_dir_sectors(), 32);
        // data_start = 68 + 32 = 100
        assert_eq!(bpb.data_start_sector(), 100);
        // cluster 2 -> sector 100, cluster 3 -> sector 104
        assert_eq!(bpb.cluster_to_sector(2), 100);
        assert_eq!(bpb.cluster_to_sector(3), 104);
    }

    #[test_case]
    fn parse_disk_image_bpb() {
        // Read block 0 from the real disk image via virtio
        let mut buf = [0u8; BLOCK_SIZE];
        crate::drivers::virtio::read_disk(&mut buf, 0).unwrap();
        let bpb = Bpb::parse(&buf).unwrap();
        // Values from the 16MB mkdisk.sh image
        assert_eq!(bpb.sector_size, 512);
        assert_eq!(bpb.sectors_per_cluster, 4);
        assert_eq!(bpb.reserved_sectors, 4);
        assert_eq!(bpb.fat_count, 2);
        assert_eq!(bpb.root_entry_count, 512);
        assert_eq!(bpb.total_sectors_16, 32768);
        assert_eq!(bpb.sectors_per_fat, 32);
        assert_eq!(bpb.data_start_sector(), 100);
    }
}
