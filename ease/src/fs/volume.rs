//! Volume for FAT16

use alloc::vec::Vec;
use core::ops::ControlFlow;

use crate::drivers::virtio::blk::{read_block, write_block};
use crate::kernel::sync::SpinLock;

use super::bpb::Bpb;
use super::dir_entry::{DirEntry, DirParseResult};
use super::{DIR_ENTRY_BYTES, FsError, SECTOR_SIZE};

// Need to lock with interrupts enabled waiting for IO completion
// SpinLock (not IrqSpinLock) because I/O needs interrupts enabled for virtio
// completion. Must never be accessed from an interrupt handler.
pub(super) static VOLUME: SpinLock<Option<Volume>> = SpinLock::new(None);

pub(crate) struct Volume {
    bpb: Bpb,
    lba: u32,
}

impl Volume {
    pub fn new(lba: u32, bpb: Bpb) -> Self {
        Self { bpb, lba }
    }

    /// Read a single FAT entry for the given cluster number
    ///
    /// The cluster number is the index into the FAT
    fn fat_entry(&self, cluster: u16) -> Result<u16, FsError> {
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        // A FAT is a flat array of u16 bits, one for each FAT cluster.
        // First find which sector the cluster number refers to
        let num_entries = SECTOR_SIZE / core::mem::size_of::<u16>(); // In a 512 byte block there are 256 entries
        let fat_sector = self.bpb.fat_start_sector() + (cluster as u32 / num_entries as u32);
        // Now read the block which holds that sector
        read_block(fat_sector as u32, &mut buf)?;
        // Work out the offset within the sector
        let offset = (cluster as usize % num_entries) * core::mem::size_of::<u16>();
        // Now look inside the block (in buffer) to read the offset of the entry
        Ok(u16::from_le_bytes([buf[offset], buf[offset + 1]]))
    }

    /// Set FAT entry to a value
    fn set_fat_entry(&self, cluster: u16, value: u16) -> Result<(), FsError> {
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        // A FAT is a flat array of u16 bits, one for each FAT cluster.
        // First find which sector the cluster number refers to
        let num_entries = SECTOR_SIZE / core::mem::size_of::<u16>(); // In a 512 byte block there are 256 entries
        let fat_sector = self.bpb.fat_start_sector() + (cluster as u32 / num_entries as u32);
        // Now read the block which holds that sector
        read_block(fat_sector as u32, &mut buf)?;
        // Write the value at that offset
        let offset = (cluster as usize % num_entries) * core::mem::size_of::<u16>();
        buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        // Write back
        write_block(fat_sector as u32, &buf)?;
        if self.bpb.fat_count == 2 {
            // Write to 2nd copy of FAT
            let second_fat_sector = fat_sector + self.bpb.sectors_per_fat;
            write_block(second_fat_sector as u32, &buf)?;
        }
        Ok(())
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
            read_block(sector, &mut buf)?;

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
        let mut content = Vec::<u8>::with_capacity(entry.file_size as usize);
        let mut current_cluster = entry.first_cluster;
        let mut buf = [0u8; 512];
        loop {
            let start_sector = self.bpb.cluster_to_sector(current_cluster as u32);
            for s in 0..self.bpb.sectors_per_cluster {
                read_block(start_sector + s, &mut buf)?;
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

    /// Read a file from offset to end
    ///
    /// Note: chain walk is O(offset); positioned-read callers with large files need a fd-layer cluster cache
    pub(crate) fn read_at(
        &self,
        entry: &DirEntry,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
        let to_read = if offset >= entry.file_size {
            0 // Also covers empty files (entry.file_size == 0)
        } else {
            (buf.len() as u32).min(entry.file_size - offset)
        };
        if to_read == 0 {
            return Ok(0);
        }
        // Skip to the starting cluster
        // skip to the starting cluster. With cluster_bytes = sectors_per_cluster × SECTOR_SIZE: the target is offset / cluster_bytes links down the chain from entry.first_cluster. Walk with self.fat_entry exactly as read_file does — but here an end-of-chain marker (0xFFF8..=0xFFFF) during the walk means the chain is shorter than file_size claims: on-disk corruption. Return an error (worth a new FsError variant that says so), never panic — disk contents are device data, the same trust rule as keyboard events.
        // This first version walks through the file which takes time O(offset)
        let mut current_cluster = entry.first_cluster;
        let mut sector_buf = [0u8; SECTOR_SIZE];
        loop {
            let start_sector = self.bpb.cluster_to_sector(current_cluster as u32);
            for s in 0..self.bpb.sectors_per_cluster {
                read_block(start_sector + s, &mut sector_buf)?;
            }
            let next_cluster = self.fat_entry(current_cluster)?;

            match next_cluster {
                0xFFF8..=0xFFFF => break,
                _ => current_cluster = next_cluster,
            }
        }
        // content.truncate(entry.file_size as usize);
        // Ok(content)

        Ok(to_read as usize)
    }

    // Create an empty file ("touch")
    pub fn create_empty_file(&mut self, filename: &str) -> Result<(), FsError> {
        if self.open(filename).is_ok() {
            return Ok(()); // file already exists, nothing to do
        }

        // First parse the file name for FAT16 8.3 validity
        let (name, ext) = DirEntry::parse_83_name(filename)?;

        // Loop through root dir to find empty slot (first byte is 0x00 or 0xE5)
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        let start = self.bpb.root_dir_start_sector();
        let count = self.bpb.root_dir_sectors();
        for sector in start..start + count {
            // First read the sector
            read_block(sector, &mut buf)?;

            // 16 entries per sector (512 / 32)
            for entry in 0..SECTOR_SIZE / DIR_ENTRY_BYTES {
                let offset = entry * DIR_ENTRY_BYTES;
                if buf[offset] != 0x00 && buf[offset] != 0xE5 {
                    continue;
                } else {
                    // Found a free slot
                    buf[offset..offset + 8].copy_from_slice(&name);
                    buf[offset + 8..offset + 11].copy_from_slice(&ext);
                    buf[offset + 11] = 0x20; // Attributes
                    // Zero out the rest (cluster = 0, size = 0, timestamps, etc.)
                    buf[offset + 12..offset + 32].fill(0);

                    // Write back
                    write_block(sector, &buf)?;
                    return Ok(());
                }
            }
        }
        Err(FsError::DirFull)
    }

    // Finds file by name and returns sector and offset
    //
    // Helper function for rm and file write
    fn find_dir_entry_location(&self, filename: &str) -> Result<(u32, usize), FsError> {
        let mut buf = [0u8; SECTOR_SIZE];
        let start = self.bpb.root_dir_start_sector();
        let count = self.bpb.root_dir_sectors();
        for sector in start..start + count {
            read_block(sector, &mut buf)?;

            for entry in 0..SECTOR_SIZE / DIR_ENTRY_BYTES {
                let offset = entry * DIR_ENTRY_BYTES;
                let bytes: &[u8; 32] = buf[offset..offset + DIR_ENTRY_BYTES].try_into().unwrap();
                match DirEntry::parse(bytes) {
                    DirParseResult::Parsed(dir_entry) => {
                        if let Ok(name) = dir_entry.filename().as_str()
                            && name.eq_ignore_ascii_case(filename)
                        {
                            return Ok((sector, offset));
                        }
                    }
                    DirParseResult::End => return Err(FsError::NotFound),
                    DirParseResult::Skip => continue,
                }
            }
        }
        Err(FsError::NotFound)
    }

    // Delete a file
    pub fn delete_file(&mut self, filename: &str) -> Result<(), FsError> {
        // Get the sector and offset of the file by file name
        let (sector, offset) = self.find_dir_entry_location(filename)?;
        // First read the sector
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        read_block(sector, &mut buf)?;
        // Find the directory entry details
        let first_cluster = u16::from_le_bytes([buf[offset + 26], buf[offset + 27]]);
        if first_cluster != 0 {
            let mut cluster = first_cluster;
            loop {
                let next = self.fat_entry(cluster)?;
                self.set_fat_entry(cluster, 0x0000)?;
                match next {
                    0xFFF8..=0xFFFF => break, // was end of chain, done
                    _ => cluster = next,      // keep walking
                }
            }
        }
        // Change the directory entry to deleted 0xE5
        buf[offset] = 0xE5;
        // Write back the change
        write_block(sector, &buf)?;
        Ok(())
    }

    /// Helper file write function to allocate the first cluster of a new file
    pub fn allocate_cluster(&self) -> Result<u16, FsError> {
        for cluster in 2..2 + self.bpb.total_data_clusters() as u16 {
            if self.fat_entry(cluster)? == 0x0000 {
                // read from FAT table
                self.set_fat_entry(cluster, 0xFFFF)?; // write to FAT table
                return Ok(cluster);
            }
        }
        Err(FsError::DiskFull)
    }

    /// Write file to disk
    pub fn write_file(&mut self, filename: &str, data: &[u8]) -> Result<(), FsError> {
        // Remove existing file if present (ignore NotFound)
        match self.delete_file(filename) {
            Ok(()) | Err(FsError::NotFound) => {}
            Err(e) => return Err(e),
        }
        // First parse the file name for FAT16 8.3 validity
        let (name, ext) = DirEntry::parse_83_name(filename)?;

        let mut prev_cluster = 0u16;
        let mut first_cluster = 0u16;
        let bytes_per_cluster = self.bpb.sectors_per_cluster as usize * SECTOR_SIZE;
        for (ci, data_chunk) in data.chunks(bytes_per_cluster).enumerate() {
            let cluster = self.allocate_cluster()?;
            if ci == 0 {
                first_cluster = cluster;
            } else {
                self.set_fat_entry(prev_cluster, cluster)?;
            }
            let start_sector = self.bpb.cluster_to_sector(cluster as u32);
            for (si, sector) in data_chunk.chunks(SECTOR_SIZE).enumerate() {
                let mut sector_buf = [0u8; SECTOR_SIZE];
                sector_buf[..sector.len()].copy_from_slice(sector);
                write_block(start_sector + si as u32, &sector_buf)?;
            }
            prev_cluster = cluster;
        }
        // Loop through root dir to find empty slot (first byte is 0x00 or 0xE5)
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        let start = self.bpb.root_dir_start_sector();
        let count = self.bpb.root_dir_sectors();
        for sector in start..start + count {
            // First read the sector
            read_block(sector, &mut buf)?;

            // 16 entries per sector (512 / 32)
            for entry in 0..SECTOR_SIZE / DIR_ENTRY_BYTES {
                let offset = entry * DIR_ENTRY_BYTES;
                if buf[offset] != 0x00 && buf[offset] != 0xE5 {
                    continue;
                } else {
                    // Found a free slot
                    buf[offset..offset + 8].copy_from_slice(&name);
                    buf[offset + 8..offset + 11].copy_from_slice(&ext);
                    buf[offset + 11] = 0x20; // Attributes
                    // Zero out (cluster = 0, size = 0, timestamps, etc.)
                    buf[offset + 12..offset + 26].fill(0);
                    buf[offset + 26..offset + 28].copy_from_slice(&first_cluster.to_le_bytes());
                    buf[offset + 28..offset + 32]
                        .copy_from_slice(&(data.len() as u32).to_le_bytes());

                    // Write back
                    write_block(sector, &buf)?;
                    return Ok(());
                }
            }
        }
        Err(FsError::DirFull)
    }
}

pub fn with_volume<F, R>(f: F) -> R
where
    F: FnOnce(&mut Volume) -> R,
{
    let mut guard = VOLUME.lock();
    let vol = guard.as_mut().expect("FAT16 not initialized");
    f(vol)
}

pub fn fat16_init(lba: u32, bpb: Bpb) {
    let vol = Volume::new(lba, bpb);
    *VOLUME.lock() = Some(vol);
}

// Volume tests are kernel-only because they exercise the real virtio
// block device. The lib crate stubs out `read_block`/`write_block` so
// volume.rs's production code compiles, but its tests can't run on
// host without a disk mock — that's a follow-up refactor (the
// "BlockDevice trait" approach). For now this gate keeps lib-test
// builds clean.
#[cfg(all(test, target_os = "none", feature = "test-fs"))]
mod test {
    use super::*;

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
    fn read_file_64kb() {
        // 64KB file spans many clusters — tests cluster chain following at scale
        with_volume(|vol| {
            let entry = vol.open("BIG.TXT").unwrap();
            assert_eq!(entry.file_size, 64 * 1024);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 64 * 1024);
            // Verify first line content
            let first_line_end = content.iter().position(|&b| b == b'\n').unwrap();
            let first_line = core::str::from_utf8(&content[..first_line_end]).unwrap();
            assert_eq!(
                first_line,
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 abcdefghijklmnopqrstuvwxyz"
            );
            // Verify last byte
            assert_eq!(content[content.len() - 1], b'X');
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

    // =========================================================================
    // Volume::create_empty_file tests
    // =========================================================================

    #[test_case]
    fn touch_creates_file() {
        with_volume(|vol| {
            vol.create_empty_file("NEW.TXT").unwrap();
            let entry = vol.open("NEW.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn touch_case_insensitive_open() {
        with_volume(|vol| {
            vol.create_empty_file("LOWER.TXT").unwrap();
            let entry = vol.open("lower.txt").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn touch_no_extension() {
        with_volume(|vol| {
            vol.create_empty_file("NOEXT").unwrap();
            let entry = vol.open("NOEXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn touch_invalid_name_rejected() {
        with_volume(|vol| {
            let result = vol.create_empty_file("TOOLONGNAME.TXT");
            assert!(matches!(result, Err(FsError::InvalidName)));
        });
    }

    #[test_case]
    fn touch_created_file_visible_in_ls() {
        with_volume(|vol| {
            vol.create_empty_file("VISIBLE.TXT").unwrap();
            let mut found = false;
            let _ = vol.read_root_dir(|entry| {
                if entry.filename().as_str() == Ok("VISIBLE.TXT") {
                    found = true;
                }
                ControlFlow::<()>::Continue(())
            });
            assert!(
                found,
                "created file should appear in root directory listing"
            );
        });
    }

    // =========================================================================
    // Volume::delete_file tests
    // =========================================================================

    #[test_case]
    fn delete_empty_file() {
        with_volume(|vol| {
            vol.create_empty_file("DEL1.TXT").unwrap();
            assert!(vol.open("DEL1.TXT").is_ok());
            vol.delete_file("DEL1.TXT").unwrap();
            assert!(matches!(vol.open("DEL1.TXT"), Err(FsError::NotFound)));
        });
    }

    #[test_case]
    fn delete_file_not_found() {
        with_volume(|vol| {
            let result = vol.delete_file("NOPE.TXT");
            assert!(matches!(result, Err(FsError::NotFound)));
        });
    }

    #[test_case]
    fn delete_file_with_content() {
        // DELETE.ME exists solely for this test — no other test depends on it
        with_volume(|vol| {
            let entry = vol.open("DELETE.ME").unwrap();
            let first_cluster = entry.first_cluster;
            assert!(first_cluster >= 2);

            vol.delete_file("DELETE.ME").unwrap();

            // File should no longer be found
            assert!(matches!(vol.open("DELETE.ME"), Err(FsError::NotFound)));

            // Cluster should be freed (0x0000)
            let fat_val = vol.fat_entry(first_cluster).unwrap();
            assert_eq!(fat_val, 0x0000, "cluster should be freed after delete");
        });
    }

    #[test_case]
    fn delete_then_recreate() {
        with_volume(|vol| {
            vol.create_empty_file("REUSE.TXT").unwrap();
            vol.delete_file("REUSE.TXT").unwrap();
            // Slot marked 0xE5 should be reusable
            vol.create_empty_file("REUSE.TXT").unwrap();
            let entry = vol.open("REUSE.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    // =========================================================================
    // set_fat_entry tests
    // =========================================================================

    #[test_case]
    fn set_fat_entry_roundtrip() {
        with_volume(|vol| {
            // Find a free cluster to test with
            let mut test_cluster = 0u16;
            for c in 2..100 {
                if vol.fat_entry(c).unwrap() == 0x0000 {
                    test_cluster = c;
                    break;
                }
            }
            assert!(test_cluster >= 2, "no free cluster found for test");

            // Write a value, read it back
            vol.set_fat_entry(test_cluster, 0x1234).unwrap();
            assert_eq!(vol.fat_entry(test_cluster).unwrap(), 0x1234);

            // Clean up — set it back to free
            vol.set_fat_entry(test_cluster, 0x0000).unwrap();
            assert_eq!(vol.fat_entry(test_cluster).unwrap(), 0x0000);
        });
    }

    // =========================================================================
    // Volume::write_file tests
    // =========================================================================

    #[test_case]
    fn write_file_and_read_back() {
        with_volume(|vol| {
            vol.write_file("WTEST1.TXT", b"hello world\n").unwrap();
            let entry = vol.open("WTEST1.TXT").unwrap();
            assert_eq!(entry.file_size, 12);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(&content, b"hello world\n");
        });
    }

    #[test_case]
    fn write_file_empty_data() {
        with_volume(|vol| {
            vol.write_file("WTEST2.TXT", b"").unwrap();
            let entry = vol.open("WTEST2.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn write_file_overwrite() {
        with_volume(|vol| {
            vol.write_file("WTEST3.TXT", b"first").unwrap();
            vol.write_file("WTEST3.TXT", b"second").unwrap();
            let entry = vol.open("WTEST3.TXT").unwrap();
            assert_eq!(entry.file_size, 6);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(&content, b"second");
        });
    }

    #[test_case]
    fn write_file_multi_sector() {
        // Write more than one sector (512 bytes)
        with_volume(|vol| {
            let data = [b'A'; 1024];
            vol.write_file("WTEST4.TXT", &data).unwrap();
            let entry = vol.open("WTEST4.TXT").unwrap();
            assert_eq!(entry.file_size, 1024);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 1024);
            assert!(content.iter().all(|&b| b == b'A'));
        });
    }

    #[test_case]
    fn write_file_multi_cluster() {
        // Write more than one cluster (sectors_per_cluster * 512 = 2048 bytes)
        with_volume(|vol| {
            let data = [b'B'; 4096];
            vol.write_file("WTEST5.TXT", &data).unwrap();
            let entry = vol.open("WTEST5.TXT").unwrap();
            assert_eq!(entry.file_size, 4096);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 4096);
            assert!(content.iter().all(|&b| b == b'B'));
        });
    }

    #[test_case]
    fn write_file_invalid_name() {
        with_volume(|vol| {
            let result = vol.write_file("TOOLONGNAME.TXT", b"data");
            assert!(matches!(result, Err(FsError::InvalidName)));
        });
    }

    #[test_case]
    fn touch_does_not_overwrite_existing() {
        with_volume(|vol| {
            vol.write_file("WTEST6.TXT", b"keep this").unwrap();
            vol.create_empty_file("WTEST6.TXT").unwrap();
            let entry = vol.open("WTEST6.TXT").unwrap();
            assert_eq!(entry.file_size, 9); // unchanged
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(&content, b"keep this");
        });
    }

    // =========================================================================
    // Volume::allocate_cluster tests
    // =========================================================================

    #[test_case]
    fn allocate_cluster_returns_valid() {
        with_volume(|vol| {
            let cluster = vol.allocate_cluster().unwrap();
            assert!(cluster >= 2);
            // Verify it's marked as end-of-chain
            assert_eq!(vol.fat_entry(cluster).unwrap(), 0xFFFF);
            // Clean up
            vol.set_fat_entry(cluster, 0x0000).unwrap();
        });
    }
}
