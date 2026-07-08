//! File Allocation Table for FAT16

/* File Allocation Table
*
* There are two FATs that are kept synchronised.
* Each FAT is placed in a region 32 sectors long.
*
* A FAT is a flat array of u16s across its used sectors (2 in this case).
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
*/

use alloc::boxed::Box;

use crate::fs::SECTOR_SIZE;

const FAT_COUNT: usize = 2;
const FAT_SECTOR_COUNT: usize = 2;
const FAT_ENTRY_SIZE: usize = core::mem::size_of::<u16>();
const FAT_ENTRY_COUNT: usize = FAT_SECTOR_COUNT * SECTOR_SIZE / FAT_ENTRY_SIZE;

const MARKER_FREE: u16 = 0x0000;
const MARKER_BAD: u16 = 0xFFF7;
const MARKER_RESERVED: [u16; 8] = [
    0x0001, 0xFFF0, 0xFFF1, 0xFFF2, 0xFFF3, 0xFFF4, 0xFFF5, 0xFFF6,
];
const MARKER_END: [u16; 8] = [
    0xFFF8, 0xFFF9, 0xFFFA, 0xFFFB, 0xFFFC, 0xFFFD, 0xFFFE, 0xFFFF,
];

enum FatError {
    TableMismatch,
}

#[derive(Clone, Copy)]
enum FatEntry {
    Bad,
    Free,
    InUseAndNext(Option<usize>),
    Reserved,
}

struct FatTable([FatEntry; FAT_ENTRY_COUNT]);

impl FatTable {
    fn parse(
        fats: &[[u8; FAT_SECTOR_COUNT * SECTOR_SIZE]; FAT_COUNT],
    ) -> Result<Box<FatTable>, FatError> {
        // FATs must match
        if !(fats[0] == fats[1]) {
            return Err(FatError::TableMismatch);
        }
        // Create heap allocated FAT
        let mut fat = Box::new(FatTable([FatEntry::Reserved; FAT_ENTRY_COUNT]));
        // Populate from first FAT
        for i in (0..FAT_SECTOR_COUNT * SECTOR_SIZE).step_by(2) {
            let raw_entry = u16::from_le_bytes([fats[0][i], fats[0][i + 1]]);
            match raw_entry {
                MARKER_FREE => fat.0[i / 2] = FatEntry::Free,
                MARKER_BAD => fat.0[i / 2] = FatEntry::Bad,
                e if MARKER_RESERVED.contains(&e) => fat.0[i / 2] = FatEntry::Reserved,
                e if MARKER_END.contains(&e) => fat.0[i / 2] = FatEntry::InUseAndNext(None),
                e => fat.0[i / 2] = FatEntry::InUseAndNext(Some(e as usize)),
                // List is exhaustive, no need for catch-all arm
            }
        }
        Ok(fat)
    }

    // Provide an iterator over a file cluster chain
    fn file_chain_as_iter(&self, start_idx: usize) -> impl Iterator<Item = usize> {
        core::iter::successors(Some(start_idx), |&idx| match self.0[idx] {
            FatEntry::InUseAndNext(Some(next)) => Some(next),
            _ => None,
        })
    }

    // Create sectors for writeback
    fn as_sectors(&self) -> [[u8; FAT_SECTOR_COUNT * SECTOR_SIZE]; FAT_COUNT] {
        let mut fats = [[0u8; FAT_SECTOR_COUNT * SECTOR_SIZE]; FAT_COUNT];
        for (i, entry) in self.0.iter().enumerate() {
            let byte_idx = i * 2;
            let fat_raw_entry: u16 = match entry {
                FatEntry::Bad => MARKER_BAD,
                FatEntry::Free => MARKER_FREE,
                FatEntry::Reserved => MARKER_RESERVED[0],
                FatEntry::InUseAndNext(next) => {
                    if let Some(n) = next {
                        *n as u16
                    } else {
                        MARKER_END[0]
                    }
                }
            };
            let bytes = fat_raw_entry.to_le_bytes();
            fats[0][byte_idx..byte_idx + 2].copy_from_slice(&bytes);
            fats[1][byte_idx..byte_idx + 2].copy_from_slice(&bytes);
        }
        fats
    }
}
