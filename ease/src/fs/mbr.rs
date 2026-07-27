//! Master Boot Record

/* Master Boot Record — device sector 0 (512 bytes)
*
* ┌─────────┬──────┬─────────────────────────────────────────┬─────────────┐
* │ Offset  │ Size │                 Field                   │    EASE?    │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 0       │ 446  │ x86 boot code (bootstrap loader)        │ skipped     │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 446     │ 16   │ Partition entry 1                       │ parsed      │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 462     │ 16   │ Partition entry 2                       │ parsed      │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 478     │ 16   │ Partition entry 3                       │ parsed      │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 494     │ 16   │ Partition entry 4                       │ parsed      │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 510     │ 2    │ Boot signature 0x55 0xAA                │ validated   │
* └─────────┴──────┴─────────────────────────────────────────┴─────────────┘
*
* Partition entry (16 bytes, offsets relative to entry start)
*
* ┌─────────┬──────┬─────────────────────────────────────────┬─────────────┐
* │ Offset  │ Size │                 Field                   │    EASE?    │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 0       │ 1    │ Status (0x80 bootable, 0x00 inactive)   │ skipped     │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 1       │ 3    │ CHS address of first sector             │ skipped (1) │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 4       │ 1    │ Partition type (2)                      │ parsed      │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 5       │ 3    │ CHS address of last sector              │ skipped (1) │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 8       │ 4    │ LBA of first sector (u32 LE)            │ parsed      │
* ├─────────┼──────┼─────────────────────────────────────────┼─────────────┤
* │ 12      │ 4    │ Sector count (u32 LE)                   │ parsed      │
* └─────────┴──────┴─────────────────────────────────────────┴─────────────┘
*
* (1) Cylinder/head/sector fields are floppy-era geometry; modern readers
*     use the LBA fields exclusively.
* (2) Types EASE accepts: 0x04 (FAT16 <32MB), 0x06 (FAT16), 0x0E (FAT16
*     LBA); later 0x0B/0x0C (FAT32). 0x00 marks an empty slot — check it
*     before the type match, all-zero entries are the common case.
*
* The partition types are
* ┌───────┬──────────────────────────────┬─────────────────────────────────┐
* │ Type  │           Meaning            │        Relevance to EASE        │
* ├───────┼──────────────────────────────┼─────────────────────────────────┤
* │ 0x00  │ Empty slot                   │ skip (most slots, most cards)   │
* │ 0x01  │ FAT12                        │ reject politely                 │
* │ 0x04  │ FAT16, volume < 32 MiB       │ accept                          │
* │ 0x06  │ FAT16, volume ≥ 32 MiB       │ accept                          │
* │ 0x0E  │ FAT16 (LBA)                  │ accept — the modern spelling    │
* │ 0x0B  │ FAT32 (CHS)                  │ accept when FAT32 lands         │
* │ 0x0C  │ FAT32 (LBA)                  │ accept when FAT32 lands — the   │
* │       │                              │ one shop cards actually use     │
* │ 0x07  │ NTFS or exFAT                │ recognize → helpful error (1)   │
* │ 0x05  │ Extended partition container │ ignore (nested table scheme)    │
* │ 0x0F  │ Extended (LBA)               │ ignore                          │
* │ 0x83  │ Linux filesystem             │ ignore                          │
* │ 0x82  │ Linux swap                   │ ignore                          │
* │ 0xEE  │ GPT protective (2)           │ recognize → helpful error       │
* │ 0xEF  │ EFI system partition         │ ignore                          │
* └───────┴──────────────────────────────┴─────────────────────────────────┘
*
*/

use super::{BOOT_SECTOR_SIG, SECTOR_SIZE};

const PARTITIONS_MAX: usize = 4;
const PARTITION_TABLE_OFFSET: usize = 446;
const PARTITION_ENTRY_SIZE: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MbrError {
    ExFatDetected,
    Fat12Detected,
    GptDetected,
    NoPartitionFound,
    InvalidMbr,
    UnknownPartitionDetected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartitionType {
    Empty,
    ExFat,
    Fat12,
    Fat16,
    Fat32,
    Gpt,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
struct Partition {
    partition_type: PartitionType,
    lba: u32,
    sector_count: u32,
}

impl Partition {
    fn parse(partition_entry: &[u8; PARTITION_ENTRY_SIZE]) -> Partition {
        let partition_type = match partition_entry[4] {
            0x00 => PartitionType::Empty,
            0x01 => PartitionType::Fat12,
            0x04 | 0x06 | 0x0E => PartitionType::Fat16,
            0x0B | 0x0C => PartitionType::Fat32,
            0x07 => PartitionType::ExFat,
            0xEE => PartitionType::Gpt,
            _ => PartitionType::Unknown,
        };
        let lba = u32::from_le_bytes([
            partition_entry[8],
            partition_entry[9],
            partition_entry[10],
            partition_entry[11],
        ]);
        let sector_count = u32::from_le_bytes([
            partition_entry[12],
            partition_entry[13],
            partition_entry[14],
            partition_entry[15],
        ]);
        Partition {
            partition_type,
            lba,
            sector_count,
        }
    }
}

pub(super) struct Mbr([Partition; PARTITIONS_MAX]);

impl Mbr {
    pub(super) fn parse(sector: &[u8; SECTOR_SIZE]) -> Result<Mbr, MbrError> {
        // Check signature
        if sector[SECTOR_SIZE - 2..] != BOOT_SECTOR_SIG {
            return Err(MbrError::InvalidMbr);
        }

        let partitions: [Partition; PARTITIONS_MAX] = core::array::from_fn(|i| {
            let offset = PARTITION_TABLE_OFFSET + PARTITION_ENTRY_SIZE * i;
            let partition_entry: &[u8; PARTITION_ENTRY_SIZE] =
                sector[offset..offset + 16].try_into().unwrap();
            Partition::parse(partition_entry)
        });
        Ok(Self(partitions))
    }

    // Search the partition table for the first supported type
    //
    // Returns (lba, sector_count)
    pub(super) fn find_partition(&self) -> Result<(u32, u32), MbrError> {
        for partition in &self.0 {
            if matches!(partition.partition_type, PartitionType::Fat16) {
                return Ok((partition.lba, partition.sector_count));
            }
            if matches!(partition.partition_type, PartitionType::Fat32) {
                return Ok((partition.lba, partition.sector_count));
            }
        }
        // Nothing mountable, diagnose reason
        let any = |t: fn(&PartitionType) -> bool| self.0.iter().any(|p| t(&p.partition_type));

        if any(|t| matches!(t, PartitionType::Gpt)) {
            Err(MbrError::GptDetected)
        } else if any(|t| matches!(t, PartitionType::ExFat)) {
            Err(MbrError::ExFatDetected)
        } else if any(|t| matches!(t, PartitionType::Fat12)) {
            Err(MbrError::Fat12Detected)
        } else if any(|t| matches!(t, PartitionType::Unknown)) {
            Err(MbrError::UnknownPartitionDetected)
        } else {
            Err(MbrError::NoPartitionFound)
        }
    }
}

// Mbr parser tests: synthetic sectors, pure logic, run in BOTH contexts
// (QEMU #[test_case] and host #[test]) like the Bpb tests above them in
// this directory.
#[cfg(all(test, feature = "test-fs"))]
mod test {
    use super::*;

    // =========================================================================
    // Helpers
    // =========================================================================

    /// Zeroed sector with a valid boot signature: an MBR with four empty slots
    fn make_test_mbr() -> [u8; SECTOR_SIZE] {
        let mut sector = [0u8; SECTOR_SIZE];
        sector[SECTOR_SIZE - 2] = 0x55;
        sector[SECTOR_SIZE - 1] = 0xAA;
        sector
    }

    /// Write a partition entry into slot 0..=3 (LBA fields little-endian)
    fn set_entry(sector: &mut [u8; SECTOR_SIZE], slot: usize, ptype: u8, lba: u32, count: u32) {
        let offset = PARTITION_TABLE_OFFSET + PARTITION_ENTRY_SIZE * slot;
        sector[offset + 4] = ptype;
        sector[offset + 8..offset + 12].copy_from_slice(&lba.to_le_bytes());
        sector[offset + 12..offset + 16].copy_from_slice(&count.to_le_bytes());
    }

    // =========================================================================
    // Parse tests
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_rejects_missing_signature() {
        let mut sector = make_test_mbr();
        sector[SECTOR_SIZE - 2] = 0x00;
        assert!(matches!(Mbr::parse(&sector), Err(MbrError::InvalidMbr)));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_lba_and_count_roundtrip_little_endian() {
        // Distinct byte values in every position pin the endianness
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 0, 0x06, 0x0102_0304, 0x0A0B_0C0D);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Ok((0x0102_0304, 0x0A0B_0C0D)));
    }

    // =========================================================================
    // Selection tests
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_empty_table_reports_no_partition() {
        let sector = make_test_mbr();
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Err(MbrError::NoPartitionFound));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_accepts_every_fat16_type_byte() {
        // 0x04 (<32MB), 0x06 (>=32MB), 0x0E (LBA) all mean FAT16
        for ptype in [0x04, 0x06, 0x0E] {
            let mut sector = make_test_mbr();
            set_entry(&mut sector, 0, ptype, 2048, 32768);
            let mbr = Mbr::parse(&sector).unwrap();
            assert_eq!(mbr.find_partition(), Ok((2048, 32768)));
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_skips_empty_and_unknown_slots() {
        // Slot 0 empty, slot 1 Linux (unknown to EASE), FAT16 in slot 2:
        // the scan must pass over both non-mountable slots
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 1, 0x83, 4096, 1000);
        set_entry(&mut sector, 2, 0x06, 8192, 2000);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Ok((8192, 2000)));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_first_fat16_wins() {
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 1, 0x06, 2048, 1000);
        set_entry(&mut sector, 2, 0x0E, 4096, 1000);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Ok((2048, 1000)));
    }

    // =========================================================================
    // Diagnosis tests (nothing mountable: most actionable finding wins)
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_diagnoses_fat32() {
        for ptype in [0x0B, 0x0C] {
            let mut sector = make_test_mbr();
            set_entry(&mut sector, 0, ptype, 2048, 32768);
            let mbr = Mbr::parse(&sector).unwrap();
            dprintln!("{:?}", mbr.find_partition());
            assert!(mbr.find_partition().is_ok());
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_diagnoses_gpt_protective() {
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 0, 0xEE, 1, 0xFFFF_FFFF);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Err(MbrError::GptDetected));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_diagnoses_exfat() {
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 0, 0x07, 2048, 32768);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Err(MbrError::ExFatDetected));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_diagnoses_fat12() {
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 0, 0x01, 2048, 32768);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(mbr.find_partition(), Err(MbrError::Fat12Detected));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_diagnoses_unknown_type() {
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 0, 0x83, 2048, 32768);
        let mbr = Mbr::parse(&sector).unwrap();
        assert_eq!(
            mbr.find_partition(),
            Err(MbrError::UnknownPartitionDetected)
        );
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn mbr_fat32_diagnosis_outranks_unknown() {
        // A nearly-usable FAT32 partition is the actionable finding even
        // when an unknown type sits in an earlier slot
        let mut sector = make_test_mbr();
        set_entry(&mut sector, 0, 0x83, 2048, 1000);
        set_entry(&mut sector, 1, 0x0C, 4096, 32768);
        let mbr = Mbr::parse(&sector).unwrap();
        assert!(mbr.find_partition().is_ok());
    }
}
