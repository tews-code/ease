//! File Allocation Table for FAT16

/* File Allocation Table
*
* There are multiple FATs that are kept synchronised.
* Each FAT is placed one after the other.
*
* FAT16
*
* A FAT is a flat array of u16s across its used sectors.
* The index of the FAT is directly the cluster number in the data region.
* Each u16 holds a number that is marker for the next cluster's role.
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
* Example:
* *
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
* FAT32
*
* ┌─────────────────────────┬──────────────────────────────┬─────────────────────────────────────────────────────────┐
* │                         │            FAT16             │                        FAT32                            │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ FAT entry size          │ 2 bytes, all 16 bits used    │ 4 bytes, only low 28 bits are the cluster number        │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ What makes it that type │ 4,085–65,524 clusters        │ 65,525+ clusters                                        │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Free marker             │ 0x0000                       │ 0x00000000                                              │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ End-of-chain            │ ≥ 0xFFF8                     │ ≥ 0x0FFFFFF8 (after masking)                            │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Bad cluster             │ 0xFFF7                       │ 0x0FFFFFF7 (after masking)                              │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Reserved                │ 0xFFF0–0xFFF6                │ 0x0FFFFFF0–0x0FFFFFF6 (after masking)                   │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Sectors-per-FAT field   │ offset 22, 16-bit            │ offset 36, 32-bit (offset 22 must read 0)               │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Root directory          │ fixed region after the FATs, │ ordinary cluster chain;                                 │
* │                         │ size from BPB offset 17      │ start cluster at BPB offset 44                          │
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Directory entry format  │ identical 32 bytes           │ identical, with the high-cluster word at entry offset 20│
* ├─────────────────────────┼──────────────────────────────┼─────────────────────────────────────────────────────────┤
* │ Practical volume size   │ up to ~2 GB                  │ up to 2 TB                                              │
* └─────────────────────────┴──────────────────────────────┴─────────────────────────────────────────────────────────┘
*
* Note:
* 1. The top 4 bits of a FAT32 entry need to be masked off when reading and preserved when writing
* 2. There is an FSInfo sector (a hint cache of the free-cluster count, so mounting doesn't scan the whole FAT)
* 3. There is a backup copy of each volume's boot sector at sector 6. The backup is actually three sectors
*        — volume sectors 0–2 copied to 6–8 — because the boot sector, FSInfo, and a spillover sector form a unit.
*       The BPB even records where the backup sits (offset 50, conventionally 6)
*
* Here's the reserved region of a FAT32 volume — everything before the first FAT,
* drawn volume-relative (a typical format reserves 32 sectors, so FAT 0 starts at sector 32):
*
* Sector: 0          1          2         3–5       6          7          8         9–3*1
* ┌────────────┬──────────┬────────────┬────────┬────────────┬──────────┬────────────┬────────┐
* │ Boot       │ FSInfo   │ Boot code  │ unused │ backup     │ backup   │ backup     │ unused │
* │ sector     │          │ spillover  │        │ of 0       │ of 1     │ of 2       │        │
* └────────────┴──────────┴────────────┴────────┴────────────┴──────────┴────────────┴────────┘
* └────── the working set ─────────┘└────── copy written at format time ─────────┘
*
*  FsInfo
*
* ┌────────┬──────┬───────────────────────────────────────────────┐
* │ Offset │ Size │                   Contents                    │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 0      │ 4    │ Lead signature 0x41615252 ("RRaA")            │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 4      │ 480  │ Reserved, zeros                               │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 484    │ 4    │ Second signature 0x61417272 ("rrAa")          │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 488    │ 4    │ Free cluster count (0xFFFFFFFF = unknown)     │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 492    │ 4    │ Next-free-cluster hint (0xFFFFFFFF = unknown) │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 496    │ 12   │ Reserved, zeros                               │
* ├────────┼──────┼───────────────────────────────────────────────┤
* │ 508    │ 4    │ Trail signature 0xAA550000                    │
* └────────┴──────┴───────────────────────────────────────────────┘
*/

const FAT16_MARKER_FREE: u16 = 0x0000;
const FAT16_MARKER_INVALID: u16 = 0x0001; // There is no storage at cluster 1 so this is always invalid
const FAT16_MARKER_RESERVED_LO: u16 = 0xFFF0;
const FAT16_MARKER_RESERVED_HI: u16 = 0xFFF6;
const FAT16_MARKER_BAD: u16 = 0xFFF7;
const FAT16_MARKER_END_MIN: u16 = 0xFFF8;

const FAT32_MASK: u32 = 0x0FFF_FFFF;
const FAT32_MARKER_FREE: u32 = 0x0000_0000;
const FAT32_MARKER_INVALID: u32 = 0x0000_0001;
const FAT32_MARKER_BAD: u32 = 0x0FFF_FFF7;
const FAT32_MARKER_RESERVED_LO: u32 = 0x0FFF_FFF0;
const FAT32_MARKER_RESERVED_HI: u32 = 0x0FFF_FFF6;
const FAT32_MARKER_END_MIN: u32 = 0x0FFF_FFF8;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum FatChainClusterError {
    Bad,
    Free,
    Reserved,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum FatEntry {
    Bad,
    End,
    Free,
    Next(u32),
    Reserved,
}

impl FatEntry {
    pub(super) fn as_bytes_16(&self) -> [u8; 2] {
        let entry_val = match self {
            Self::Bad => FAT16_MARKER_BAD,
            Self::Free => FAT16_MARKER_FREE,
            Self::Reserved => FAT16_MARKER_RESERVED_LO,
            Self::End => FAT16_MARKER_END_MIN,
            Self::Next(next) => *next as u16,
        };
        entry_val.to_le_bytes()
    }

    pub(super) fn as_bytes_32(&self, current_entry: u32) -> [u8; 4] {
        let entry_val = match self {
            Self::Bad => FAT32_MARKER_BAD,
            Self::Free => FAT32_MARKER_FREE,
            Self::Reserved => FAT32_MARKER_RESERVED_LO,
            Self::End => FAT32_MARKER_END_MIN,
            Self::Next(next) => *next,
        };
        let top_4 = current_entry & !FAT32_MASK;
        let bottom_28 = entry_val & FAT32_MASK;
        (top_4 | bottom_28).to_le_bytes()
    }

    pub(super) fn next_in_chain(self) -> Result<Option<u32>, FatChainClusterError> {
        match self {
            Self::Bad => Err(FatChainClusterError::Bad)?,
            Self::Free => Err(FatChainClusterError::Free)?,
            Self::Reserved => Err(FatChainClusterError::Reserved)?,
            Self::End => Ok(None),
            Self::Next(next) => Ok(Some(next)),
        }
    }
}

pub(super) fn parse_entry_fat16(entry: &[u8; 2]) -> FatEntry {
    let raw_entry = u16::from_le_bytes([entry[0], entry[1]]);
    match raw_entry {
        FAT16_MARKER_FREE => FatEntry::Free,
        FAT16_MARKER_INVALID => FatEntry::Reserved,
        FAT16_MARKER_RESERVED_LO..=FAT16_MARKER_RESERVED_HI => FatEntry::Reserved,
        FAT16_MARKER_BAD => FatEntry::Bad,
        FAT16_MARKER_END_MIN.. => FatEntry::End,
        e => FatEntry::Next(e as u32),
    }
}

pub(super) fn parse_entry_fat32(entry: &[u8; 4]) -> FatEntry {
    let raw_entry = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
    // Mask off the top 4 bits
    match raw_entry & FAT32_MASK {
        FAT32_MARKER_FREE => FatEntry::Free,
        FAT32_MARKER_INVALID => FatEntry::Reserved,
        FAT32_MARKER_RESERVED_LO..=FAT32_MARKER_RESERVED_HI => FatEntry::Reserved,
        FAT32_MARKER_BAD => FatEntry::Bad,
        FAT32_MARKER_END_MIN.. => FatEntry::End,
        e => FatEntry::Next(e),
    }
}

// FatEntry codec tests. Pure logic with no driver dependencies, so they run
// in BOTH contexts (QEMU #[test_case] and host #[test]) like the other fs
// unit tests. `cfg_attr` selects the right attribute per target.
#[cfg(all(test, feature = "test-fs"))]
mod test {
    use super::*;

    // Every variant that can be produced by a write, for roundtrip checks.
    const ROUNDTRIP_VARIANTS: [FatEntry; 5] = [
        FatEntry::Free,
        FatEntry::Bad,
        FatEntry::Reserved,
        FatEntry::Next(5),
        FatEntry::End,
    ];

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn fat16_bytes_roundtrip() {
        for entry in ROUNDTRIP_VARIANTS {
            let bytes = entry.as_bytes_16();
            assert_eq!(
                parse_entry_fat16(&bytes),
                entry,
                "roundtrip failed for {entry:?}"
            );
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn fat32_bytes_roundtrip() {
        for entry in ROUNDTRIP_VARIANTS {
            // current_entry = 0: no reserved bits set in the existing word
            let bytes = entry.as_bytes_32(0);
            assert_eq!(
                parse_entry_fat32(&bytes),
                entry,
                "roundtrip failed for {entry:?}"
            );
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn fat32_preserves_reserved_bits_writing_cluster() {
        // The top 4 bits of the existing on-disk word must survive a write.
        let current = 0xF000_0000;
        let bytes = FatEntry::Next(5).as_bytes_32(current);
        let raw = u32::from_le_bytes(bytes);
        assert_eq!(
            raw & !FAT32_MASK,
            0xF000_0000,
            "reserved nibble not preserved"
        );
        assert_eq!(raw & FAT32_MASK, 5, "cluster number wrong");
        // Parsing still recovers the entry (parse masks the reserved bits away)
        assert_eq!(parse_entry_fat32(&bytes), FatEntry::Next(5));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn fat32_preserves_reserved_bits_writing_markers() {
        // Regression guard: the non-cluster arms (free / end / bad / reserved)
        // must also preserve the reserved nibble, not just the cluster arm.
        let current = 0xA000_0000;
        for entry in [
            FatEntry::Free,
            FatEntry::End,
            FatEntry::Bad,
            FatEntry::Reserved,
        ] {
            let bytes = entry.as_bytes_32(current);
            let raw = u32::from_le_bytes(bytes);
            assert_eq!(
                raw & !FAT32_MASK,
                0xA000_0000,
                "reserved nibble clobbered writing {entry:?}"
            );
            assert_eq!(
                parse_entry_fat32(&bytes),
                entry,
                "roundtrip failed for {entry:?}"
            );
        }
    }

    // =========================================================================
    // next_in_chain
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn next_in_chain_yields_next_cluster() {
        assert_eq!(FatEntry::Next(5).next_in_chain(), Ok(Some(5)));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn next_in_chain_end_of_chain_is_none() {
        assert_eq!(FatEntry::End.next_in_chain(), Ok(None));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn next_in_chain_bad_cluster_errors() {
        assert_eq!(
            FatEntry::Bad.next_in_chain(),
            Err(FatChainClusterError::Bad)
        );
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn next_in_chain_free_cluster_errors() {
        // A chain that runs into a free cluster is corrupt, not merely ended.
        assert_eq!(
            FatEntry::Free.next_in_chain(),
            Err(FatChainClusterError::Free)
        );
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn next_in_chain_reserved_cluster_errors() {
        assert_eq!(
            FatEntry::Reserved.next_in_chain(),
            Err(FatChainClusterError::Reserved)
        );
    }
}
