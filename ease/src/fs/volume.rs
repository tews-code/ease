//! Volume for FAT

// The volume is the mounted instance and translates from relative sectors to absolute blocks

use alloc::vec::Vec;
use core::ops::ControlFlow;

use crate::drivers::virtio::blk::BlkError;
use crate::kernel::sync::SpinLock;

use super::bpb::Bpb;
use super::dir::{ATTR_ARCHIVE, DIR_ENTRY_BYTES, DirEntryKind, FileInfo};
use super::fat::{self, FatEntry};
use super::{DirHandle, FsError, SECTOR_SIZE, VolumeType};

// Need to lock with interrupts enabled waiting for IO completion
// SpinLock (not IrqSpinLock) because I/O needs interrupts enabled for virtio
// completion. Must never be accessed from an interrupt handler.
pub(super) static VOLUME: SpinLock<Option<Volume>> = SpinLock::new(None);

pub(super) struct SectorLocation {
    sector: u32,
    offset: usize,
}

pub(crate) struct Volume {
    bpb: Bpb,
    lba: u32,
    fat_cache_sector: Option<u32>,
    fat_cache: [u8; SECTOR_SIZE],
}

// Methods on Volume never take the lock
impl Volume {
    pub fn new(lba: u32, bpb: Bpb) -> Self {
        Self {
            bpb,
            lba,
            fat_cache_sector: None,
            fat_cache: [0u8; SECTOR_SIZE],
        }
    }

    #[inline(always)]
    fn fat_entry_size(&self) -> u32 {
        (match self.bpb.volume_type {
            VolumeType::Fat16(_) => core::mem::size_of::<u16>(),
            VolumeType::Fat32(_) => core::mem::size_of::<u32>(),
        }) as u32
    }

    fn fat_entry_count(&self) -> u32 {
        let entry_size = self.fat_entry_size();
        SECTOR_SIZE as u32 / entry_size
    }

    fn read_sector(&self, sector: u32, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), BlkError> {
        // Translate to block
        let block = sector + self.lba;
        crate::drivers::virtio::blk::read_block(block, buf)
    }

    fn write_sector(&self, sector: u32, buf: &[u8; SECTOR_SIZE]) -> Result<(), BlkError> {
        // Translate to block
        let block = sector + self.lba;
        crate::drivers::virtio::blk::write_block(block, buf)
    }

    fn update_fat_cache(&mut self, cluster: u32) -> Result<(u32, u32), FsError> {
        // First find which sector the cluster number refers to
        let num_entries = self.fat_entry_count();
        let fat_sector = self.bpb.fat_start_sector() + (cluster / num_entries);
        // Check if this is already in cache, otherwise update
        if self.fat_cache_sector.is_none_or(|s| s != fat_sector) {
            // Store in FAT cache
            let mut buf = [0u8; SECTOR_SIZE]; // Temporary buffer to avoid double mut ref
            self.read_sector(fat_sector, &mut buf)?;
            self.fat_cache = buf;
            self.fat_cache_sector = Some(fat_sector);
        }
        Ok((num_entries, self.fat_entry_size()))
    }

    /// Read a single FAT entry for the given cluster number
    ///
    /// The cluster number is the index into the FAT
    /// The cache is updated to the FAT sector that has been read.
    fn fat_entry(&mut self, cluster: u32) -> Result<FatEntry, FsError> {
        // Update the FAT cache if needed
        let (num_entries, entry_size) = self.update_fat_cache(cluster)?;
        // Work out the offset within the sector
        let offset = ((cluster % num_entries) * self.fat_entry_size()) as usize;
        // Now look inside the block (in buffer) to read the offset of the entry
        let fat_entry = match entry_size {
            2 => fat::parse_entry_fat16(&[self.fat_cache[offset], self.fat_cache[offset + 1]]),
            4 => fat::parse_entry_fat32(&[
                self.fat_cache[offset],
                self.fat_cache[offset + 1],
                self.fat_cache[offset + 2],
                self.fat_cache[offset + 3],
            ]),
            _ => return Err(FsError::Bpb(super::bpb::BpbError::InvalidBpb)),
        };
        Ok(fat_entry)
    }

    /// Set FAT entry to a value
    fn set_fat_entry(&mut self, cluster: u32, value: FatEntry) -> Result<(), FsError> {
        // Update the FAT cache if needed
        let (num_entries, entry_size) = self.update_fat_cache(cluster)?;
        // Write the value at that offset
        let offset = ((cluster % num_entries) * entry_size) as usize;
        match entry_size {
            2 => self.fat_cache[offset..offset + 2].copy_from_slice(&value.as_bytes_16()),
            4 => {
                let current = self.fat_cache[offset] as u32;
                self.fat_cache[offset..offset + 4].copy_from_slice(&value.as_bytes_32(current));
            }
            _ => return Err(FsError::Bpb(super::bpb::BpbError::InvalidBpb)),
        }
        // Write back to all FAT copies
        for i in 0..self.bpb.fat_count {
            self.write_sector(
                self.fat_cache_sector.unwrap() + i * self.bpb.sectors_per_fat,
                &self.fat_cache,
            )?;
        }
        Ok(())
    }

    /// Finds the next available directory entry, reusing deleted entries.
    ///
    /// Returns the sector and offset.
    fn get_avail_dir_entry(&mut self, dir: DirHandle) -> Result<(u32, u32), FsError> {
        fn get_avail_dir_entry_in_sector(
            volume_type: VolumeType,
            sector: [u8; SECTOR_SIZE],
        ) -> Option<u32> {
            // Parse the sector as directory table entries
            for entry_offset in (0..SECTOR_SIZE).step_by(DIR_ENTRY_BYTES) {
                match DirEntryKind::parse(
                    sector[entry_offset..entry_offset + DIR_ENTRY_BYTES]
                        .try_into()
                        .unwrap(),
                    volume_type,
                ) {
                    DirEntryKind::Deleted | DirEntryKind::Empty => {
                        return Some(entry_offset as u32);
                    }
                    DirEntryKind::Used(_) | DirEntryKind::Unsupported => continue,
                }
            }
            None
        }

        let mut buf = [0u8; SECTOR_SIZE];
        if dir.start_cluster == 0
            && let VolumeType::Fat16((root_dir_first_sector, root_dir_num_sectors)) =
                self.bpb.volume_type
        {
            // This is a FAT16 root directory
            for sector in root_dir_first_sector..root_dir_first_sector + root_dir_num_sectors {
                // Load the sector
                self.read_sector(sector, &mut buf)?;
                if let Some(offset) = get_avail_dir_entry_in_sector(self.bpb.volume_type, buf) {
                    return Ok((sector, offset));
                }
                // None found, look at next sector
            }
            return Err(FsError::DirFull);
        };
        // Update the starting cluster if this is a FAŦ32 root directory
        let mut current_cluster = if dir.start_cluster == 0
            && let VolumeType::Fat32(root_dir_start_cluster) = self.bpb.volume_type
        {
            root_dir_start_cluster
        } else {
            dir.start_cluster
        };
        // File-based directory table, read the file cluster chain to scan
        let mut buf = [0u8; SECTOR_SIZE];
        loop {
            let start_sector = self.bpb.cluster_to_sector(current_cluster);
            for sector in 0..self.bpb.sectors_per_cluster {
                self.read_sector(start_sector + sector, &mut buf)?;
                if let Some(offset) = get_avail_dir_entry_in_sector(self.bpb.volume_type, buf) {
                    return Ok((sector, offset));
                }
            }
            // Nothing found in that cluster, let's look at the next
            let next_cluster = self.fat_entry(current_cluster)?;
            if let Some(next) = next_cluster.next_in_chain()? {
                current_cluster = next;
            } else {
                break;
            }
        }
        // Reached the end of the directory without finding an available slot
        Err(FsError::DirFull)
    }

    /// Read directory
    pub fn read_dir<B, F>(&mut self, dir: DirHandle, mut f: F) -> Result<ControlFlow<B>, FsError>
    where
        F: FnMut(&FileInfo) -> ControlFlow<B>,
    {
        fn parse_sector<B, F>(
            sector: [u8; SECTOR_SIZE],
            volume_type: VolumeType,
            mut f: F,
        ) -> Option<ControlFlow<B>>
        where
            F: FnMut(&FileInfo) -> ControlFlow<B>,
        {
            for entry_offset in (0..SECTOR_SIZE).step_by(DIR_ENTRY_BYTES) {
                match DirEntryKind::parse(
                    sector[entry_offset..entry_offset + DIR_ENTRY_BYTES]
                        .try_into()
                        .unwrap(),
                    volume_type,
                ) {
                    DirEntryKind::Used(entry) => {
                        if let ControlFlow::Break(val) = f(&entry) {
                            return Some(ControlFlow::Break(val));
                        }
                    }
                    DirEntryKind::Unsupported => continue,
                    DirEntryKind::Deleted | DirEntryKind::Empty => {
                        return Some(ControlFlow::Continue(()));
                    }
                }
            }
            None
        }

        let mut buf = [0u8; SECTOR_SIZE]; // Scratch buffer
        if dir.start_cluster == 0
            && let VolumeType::Fat16((root_dir_first_sector, root_dir_num_sectors)) =
                self.bpb.volume_type
        {
            // This is a FAT16 root directory
            for sector in root_dir_first_sector..root_dir_first_sector + root_dir_num_sectors {
                // Load the sector
                self.read_sector(sector, &mut buf)?;
                if let Some(control_flow_result) = parse_sector(buf, self.bpb.volume_type, &mut f) {
                    return Ok(control_flow_result);
                }
            }
        };
        // Update the starting cluster if this is a FAŦ32 root directory
        let mut current_cluster = if dir.start_cluster == 0
            && let VolumeType::Fat32(root_dir_start_cluster) = self.bpb.volume_type
        {
            root_dir_start_cluster
        } else {
            dir.start_cluster
        };
        // Reading the directory table from file using cluster chain
        loop {
            let start_sector = self.bpb.cluster_to_sector(current_cluster);
            for sector in 0..self.bpb.sectors_per_cluster {
                self.read_sector(start_sector + sector, &mut buf)?;
                if let Some(control_flow_result) = parse_sector(buf, self.bpb.volume_type, &mut f) {
                    return Ok(control_flow_result);
                } else {
                    continue;
                }
            }
            // Nothing found in that cluster, let's look at the next
            let next_cluster = self.fat_entry(current_cluster)?;
            if let Some(next) = next_cluster.next_in_chain()? {
                current_cluster = next;
            } else {
                break;
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Find a file by file name
    pub fn open(&mut self, dir: DirHandle, filename: &str) -> Result<FileInfo, FsError> {
        let sector_buf = &mut [0u8; SECTOR_SIZE];
        let (_, file_info) = self.find_dir_entry_location(sector_buf, dir, filename)?;
        Ok(file_info)
    }

    /// Read a file from a given FileEntry
    pub fn read_file(&mut self, entry: &FileInfo) -> Result<Vec<u8>, FsError> {
        if entry.file_size == 0 {
            return Ok(Vec::new());
        }
        let mut content = Vec::<u8>::with_capacity(entry.file_size as usize);
        let mut current_cluster = entry.first_cluster;
        let mut buf = [0u8; SECTOR_SIZE];
        loop {
            let start_sector = self.bpb.cluster_to_sector(current_cluster);
            for s in 0..self.bpb.sectors_per_cluster {
                self.read_sector(start_sector + s, &mut buf)?;
                content.extend_from_slice(&buf);
            }
            let next_cluster = self.fat_entry(current_cluster)?;
            if let Some(next) = next_cluster.next_in_chain()? {
                current_cluster = next;
            } else {
                break;
            }
        }
        content.truncate(entry.file_size as usize);
        Ok(content)
    }

    // Create an empty file ("touch")
    pub fn create_empty_file(&mut self, dir: DirHandle, filename: &str) -> Result<(), FsError> {
        if self.open(dir, filename).is_ok() {
            return Ok(()); // file already exists, nothing to do
        }
        // First parse the file name for FAT16 8.3 validity
        let (name, extension) = FileInfo::parse_83_name(filename)?;
        // Get an avaiable entry slot in this directory
        let (sector, offset) = self.get_avail_dir_entry(dir)?;
        // Construct the directory entry file info
        let file_info = FileInfo {
            name,
            extension,
            attributes: ATTR_ARCHIVE,
            first_cluster: 0,
            file_size: 0,
        };
        // Read the sector and write back with new entry
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        // First read the sector
        self.read_sector(sector, &mut buf)?;
        // Now add the empty file entry
        buf[offset as usize..offset as usize + DIR_ENTRY_BYTES]
            .copy_from_slice(&file_info.as_bytes(self.bpb.volume_type));
        // Write back
        self.write_sector(sector, &buf)?;
        Ok(())
    }

    // Finds file by name and returns sector, offset and DirEntry
    //
    // Helper function for rm and file write
    pub(super) fn find_dir_entry_location(
        &mut self,
        sector_buf: &mut [u8; SECTOR_SIZE],
        dir: DirHandle,
        filename: &str,
    ) -> Result<(SectorLocation, FileInfo), FsError> {
        for dir_entry_result in self.dir_iter(sector_buf, dir)? {
            let dir_entry_result = dir_entry_result?;
            if let DirEntryKind::Used(file_info) = dir_entry_result.dir_entry_kind
                && let Ok(name) = file_info.filename().as_str()
                && name.eq_ignore_ascii_case(filename)
            {
                // Found a match - return it with its location
                return Ok((dir_entry_result.sector_location, file_info));
            }
        }
        Err(FsError::NotFound)
    }

    // Delete a file
    pub fn delete_file(&mut self, dir: DirHandle, filename: &str) -> Result<(), FsError> {
        let sector_buf = &mut [0u8; SECTOR_SIZE];
        // Get the sector and offset of the file by file name
        let (location, dir_entry) = self.find_dir_entry_location(sector_buf, dir, filename)?;
        // First read the sector
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        self.read_sector(location.sector, &mut buf)?;
        // Find the directory entry details
        if dir_entry.first_cluster != 0 {
            let mut cluster = dir_entry.first_cluster;
            loop {
                let next = self.fat_entry(cluster)?;
                self.set_fat_entry(cluster, FatEntry::Free)?;
                if let Some(c) = next.next_in_chain()? {
                    cluster = c;
                } else {
                    break;
                }
            }
        }
        // Change the directory entry to deleted 0xE5
        buf[location.offset] = super::dir::ENTRY_DEL;
        // Write back the change
        self.write_sector(location.sector, &buf)?;
        Ok(())
    }

    /// Helper file write function to allocate the first cluster of a new file
    pub fn allocate_cluster(&mut self) -> Result<u32, FsError> {
        for cluster in 2..2 + self.bpb.total_data_clusters() {
            if self.fat_entry(cluster)? == FatEntry::Free {
                // read from FAT table
                self.set_fat_entry(cluster, FatEntry::End)?; // Write end of file to FAT table
                return Ok(cluster);
            }
        }
        Err(FsError::DiskFull)
    }

    /// Write file to disk
    pub fn write_file(
        &mut self,
        dir: DirHandle,
        filename: &str,
        data: &[u8],
    ) -> Result<(), FsError> {
        // Remove existing file if present (ignore NotFound)
        match self.delete_file(dir, filename) {
            Ok(()) | Err(FsError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let mut prev_cluster = 0u32;
        let mut first_cluster = 0u32;
        let bytes_per_cluster = self.bpb.sectors_per_cluster as usize * SECTOR_SIZE;
        for (ci, data_chunk) in data.chunks(bytes_per_cluster).enumerate() {
            let cluster = self.allocate_cluster()?;
            if ci == 0 {
                first_cluster = cluster;
            } else {
                self.set_fat_entry(prev_cluster, FatEntry::Next(cluster))?;
            }
            let start_sector = self.bpb.cluster_to_sector(cluster);
            for (si, sector) in data_chunk.chunks(SECTOR_SIZE).enumerate() {
                let mut sector_buf = [0u8; SECTOR_SIZE];
                sector_buf[..sector.len()].copy_from_slice(sector);
                self.write_sector(start_sector + si as u32, &sector_buf)?;
            }
            prev_cluster = cluster;
        }
        // Loop through dir to find empty slot
        let (sector, offset) = self.get_avail_dir_entry(dir)?;
        // First parse the file name for FAT16 8.3 validity
        let (name, ext) = FileInfo::parse_83_name(filename)?;
        let file_info = FileInfo {
            name,
            extension: ext,
            attributes: ATTR_ARCHIVE,
            first_cluster,
            file_size: data.len() as u32,
        };
        // Now read sector, update entry and write back
        let mut buf = [0u8; SECTOR_SIZE]; // scratch buffer for sector reads
        self.read_sector(sector, &mut buf)?;
        buf[offset as usize..offset as usize + DIR_ENTRY_BYTES]
            .copy_from_slice(&file_info.as_bytes(self.bpb.volume_type));
        self.write_sector(sector, &buf)?;
        Ok(())
    }

    // Create an interator struct for directory entries
    pub(super) fn dir_iter<'a>(
        &'a mut self,
        sector_buf: &'a mut [u8; SECTOR_SIZE],
        dir: DirHandle,
    ) -> Result<DirEntryIter<'a>, FsError> {
        // We are just starting, need to load the first sector
        let (current_sector, sectors_in_run, next_cluster) = if dir.start_cluster == 0
            && let VolumeType::Fat16((root_dir_start_sector, root_dir_sector_count)) =
                self.bpb.volume_type
        {
            // This is a FAT16 root directory - read from BPB-provided sectors
            (root_dir_start_sector, root_dir_sector_count, None)
        } else {
            // This is a file-based directory. Check if it is FAT32 root
            let current_cluster = if dir.start_cluster == 0
                && let VolumeType::Fat32(root_dir_start_cluster) = self.bpb.volume_type
            {
                root_dir_start_cluster
            } else {
                dir.start_cluster
            };
            let current_sector = self.bpb.cluster_to_sector(current_cluster);
            let next_cluster = self.fat_entry(current_cluster)?.next_in_chain()?;
            (current_sector, self.bpb.sectors_per_cluster, next_cluster)
        };
        self.read_sector(current_sector, sector_buf)?;

        Ok(DirEntryIter {
            volume: self,
            sector_buf,
            current_offset: 0,
            next_sector: current_sector + 1,
            sectors_left_in_run: sectors_in_run as usize - 1,
            next_cluster,
            returned_empty: false,
        })
    }
}

pub(super) struct DirEntryIter<'a> {
    volume: &'a mut Volume,
    sector_buf: &'a mut [u8; SECTOR_SIZE],
    next_sector: u32,
    sectors_left_in_run: usize,
    current_offset: usize,
    next_cluster: Option<u32>,
    returned_empty: bool,
}

pub(super) struct DirEntryResult {
    pub(super) sector_location: SectorLocation,
    pub(super) dir_entry_kind: DirEntryKind,
}

impl<'a> DirEntryIter<'a> {
    fn next_inner(&mut self) -> Result<Option<DirEntryResult>, FsError> {
        // Are we already done?
        if self.returned_empty {
            return Ok(None);
        }
        // Use cached values to return next DirEntryKind
        if self.current_offset == SECTOR_SIZE {
            // We are at the end of this sector, get the next one
            if self.sectors_left_in_run == 0 {
                // We are at the end of a sector run, load the next cluster if possible
                if let Some(next) = self.next_cluster {
                    self.next_sector = self.volume.bpb.cluster_to_sector(next);
                    self.next_cluster = self.volume.fat_entry(next)?.next_in_chain()?;
                    self.sectors_left_in_run = self.volume.bpb.sectors_per_cluster as usize;
                } else {
                    // We are done - no entries left in this directory
                    return Ok(None);
                }
            }
            // Update the sector and iter details
            self.volume.read_sector(self.next_sector, self.sector_buf)?;
            self.next_sector += 1;
            self.sectors_left_in_run -= 1;
            self.current_offset = 0;
        }
        // We use the cached values to get the directory entry raw bytes and parse
        let offset = self.current_offset;
        self.current_offset = offset + DIR_ENTRY_BYTES;
        let dir_entry_kind = DirEntryKind::parse(
            self.sector_buf[offset..self.current_offset]
                .try_into()
                .unwrap(),
            self.volume.bpb.volume_type,
        );
        if dir_entry_kind == DirEntryKind::Empty {
            // We only return the first Empty, afterwards the iterator is done
            self.returned_empty = true;
        }
        Ok(Some(DirEntryResult {
            sector_location: SectorLocation {
                sector: self.next_sector - 1,
                offset,
            },
            dir_entry_kind,
        }))
    }
}

// Iterator over directory entries
impl Iterator for DirEntryIter<'_> {
    type Item = Result<DirEntryResult, FsError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_inner().transpose()
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
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "HELLO.TXT").unwrap();
            assert!(entry.file_size > 0);
        });
    }

    #[test_case]
    fn open_case_insensitive() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "hello.txt").unwrap();
            assert!(entry.file_size > 0);
        });
    }

    #[test_case]
    fn open_not_found() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let result = vol.open(current_dir, "NOPE.TXT");
            assert!(matches!(result, Err(FsError::NotFound)));
        });
    }

    #[test_case]
    fn open_empty_file() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "EMPTY.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn open_no_extension() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "SHORT").unwrap();
            assert!(entry.file_size > 0);
        });
    }

    // =========================================================================
    // Volume::read_file tests
    // =========================================================================

    #[test_case]
    fn read_file_hello_txt() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "HELLO.TXT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            let text = core::str::from_utf8(&content).unwrap();
            assert_eq!(text, "Text file contents\n");
        });
    }

    #[test_case]
    fn read_file_empty() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "EMPTY.TXT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 0);
        });
    }

    #[test_case]
    fn read_file_short() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "SHORT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            let text = core::str::from_utf8(&content).unwrap();
            assert_eq!(text, "This is a file with a short name.\n");
        });
    }

    #[test_case]
    fn read_file_size_matches_dir_entry() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "HELLO.TXT").unwrap();
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), entry.file_size as usize);
        });
    }

    #[test_case]
    fn read_file_longname() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let entry = vol.open(current_dir, "LONGNAME.END").unwrap();
            let content = vol.read_file(&entry).unwrap();
            let text = core::str::from_utf8(&content).unwrap();
            assert_eq!(text, "This is a file with a long name.\n");
        });
    }

    #[test_case]
    fn read_file_64kb() {
        let current_dir = DirHandle { start_cluster: 0 };
        // 64KB file spans many clusters — tests cluster chain following at scale
        with_volume(|vol| {
            let entry = vol.open(current_dir, "BIG.TXT").unwrap();
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
        let current_dir = DirHandle { start_cluster: 0 };
        // The PDF is ~7.9MB — too large to read into heap, but verify open finds it
        // and the dir entry has the expected size.
        with_volume(|vol| {
            let entry = vol.open(current_dir, "RP-008~1.PDF").unwrap();
            assert_eq!(entry.file_size, 7_968_417);
        });
    }

    #[test_case]
    fn disk_image_fat_entry_hello_txt() {
        let current_dir = DirHandle { start_cluster: 0 };
        // HELLO.TXT is a small file — its first cluster should be end-of-chain
        with_volume(|vol| {
            // Find HELLO.TXT's first cluster from the directory
            let result = vol
                .read_dir(current_dir, |entry| {
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
                next == FatEntry::End,
                "expected EOC for small file, got {:?}",
                next
            );
        });
    }

    #[test_case]
    fn disk_image_root_dir_contains_hello_txt() {
        let root_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let result = vol
                .read_dir(root_dir, |entry| {
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
        let root_dir = DirHandle { start_cluster: 0 };
        // read_root_dir should skip volume labels — none should come through
        with_volume(|vol| {
            let _ = vol
                .read_dir(root_dir, |entry| {
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
        // FAT entries 0 and 1 are reserved slots, not chain entries: FAT[0]
        // holds the media descriptor and FAT[1] an EOC/flags marker. Their bit
        // pattern (>= 0xFFF8) is exactly what the value-based FatEntry classifier
        // reads as end-of-chain, so classifying them is meaningless (chains only
        // ever start at cluster 2). Check the raw reserved values directly.
        with_volume(|vol| {
            let mut buf = [0u8; SECTOR_SIZE];
            vol.read_sector(vol.bpb.fat_start_sector(), &mut buf)
                .unwrap();
            let fat0 = u16::from_le_bytes([buf[0], buf[1]]);
            let fat1 = u16::from_le_bytes([buf[2], buf[3]]);
            // FAT[0] low byte is the media descriptor (0xF8 = fixed disk) with
            // the upper bits set; FAT[1] is the reserved / end-of-chain marker.
            assert!(
                fat0 >= 0xFFF8,
                "FAT[0] should be media descriptor, got {fat0:#06x}"
            );
            assert!(
                fat1 >= 0xFFF8,
                "FAT[1] should be reserved/EOC, got {fat1:#06x}"
            );
        });
    }

    // =========================================================================
    // Volume::create_empty_file tests
    // =========================================================================

    #[test_case]
    fn touch_creates_file() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.create_empty_file(current_dir, "NEW.TXT").unwrap();
            let entry = vol.open(current_dir, "NEW.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn touch_case_insensitive_open() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.create_empty_file(current_dir, "LOWER.TXT").unwrap();
            let entry = vol.open(current_dir, "lower.txt").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn touch_no_extension() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.create_empty_file(current_dir, "NOEXT").unwrap();
            let entry = vol.open(current_dir, "NOEXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn touch_invalid_name_rejected() {
        with_volume(|vol| {
            let result = vol.create_empty_file(DirHandle { start_cluster: 0 }, "TOOLONGNAME.TXT");
            assert!(matches!(result, Err(FsError::InvalidName)));
        });
    }

    #[test_case]
    fn touch_created_file_visible_in_ls() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.create_empty_file(current_dir, "VISIBLE.TXT").unwrap();
            let mut found = false;
            let _ = vol.read_dir(current_dir, |entry| {
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
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.create_empty_file(current_dir, "DEL1.TXT").unwrap();
            assert!(vol.open(current_dir, "DEL1.TXT").is_ok());
            vol.delete_file(current_dir, "DEL1.TXT").unwrap();
            assert!(matches!(
                vol.open(current_dir, "DEL1.TXT"),
                Err(FsError::NotFound)
            ));
        });
    }

    #[test_case]
    fn delete_file_not_found() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let result = vol.delete_file(current_dir, "NOPE.TXT");
            assert!(matches!(result, Err(FsError::NotFound)));
        });
    }

    #[test_case]
    fn delete_file_with_content() {
        let current_dir = DirHandle { start_cluster: 0 };
        // DELETE.ME exists solely for this test — no other test depends on it
        with_volume(|vol| {
            let entry = vol.open(current_dir, "DELETE.ME").unwrap();
            let first_cluster = entry.first_cluster;
            assert!(first_cluster >= 2);

            vol.delete_file(current_dir, "DELETE.ME").unwrap();

            // File should no longer be found
            assert!(matches!(
                vol.open(current_dir, "DELETE.ME"),
                Err(FsError::NotFound)
            ));

            // Cluster should be freed (0x0000)
            let fat_val = vol.fat_entry(first_cluster).unwrap();
            assert_eq!(
                fat_val,
                FatEntry::Free,
                "cluster should be freed after delete"
            );
        });
    }

    #[test_case]
    fn delete_then_recreate() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.create_empty_file(current_dir, "REUSE.TXT").unwrap();
            vol.delete_file(current_dir, "REUSE.TXT").unwrap();
            // Slot marked 0xE5 should be reusable
            vol.create_empty_file(current_dir, "REUSE.TXT").unwrap();
            let entry = vol.open(current_dir, "REUSE.TXT").unwrap();
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
            let mut test_cluster = 0u32;
            for c in 2..100 {
                if vol.fat_entry(c).unwrap() == FatEntry::Free {
                    test_cluster = c;
                    break;
                }
            }
            assert!(test_cluster >= 2, "no free cluster found for test");

            // Write a value, read it back
            vol.set_fat_entry(test_cluster, FatEntry::Next(0x1234))
                .unwrap();
            assert_eq!(vol.fat_entry(test_cluster).unwrap(), FatEntry::Next(0x1234));

            // Clean up — set it back to free
            vol.set_fat_entry(test_cluster, FatEntry::Free).unwrap();
            assert_eq!(vol.fat_entry(test_cluster).unwrap(), FatEntry::Free);
        });
    }

    // =========================================================================
    // Volume::write_file tests
    // =========================================================================

    #[test_case]
    fn write_file_and_read_back() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.write_file(current_dir, "WTEST1.TXT", b"hello world\n")
                .unwrap();
            let entry = vol.open(current_dir, "WTEST1.TXT").unwrap();
            assert_eq!(entry.file_size, 12);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(&content, b"hello world\n");
        });
    }

    #[test_case]
    fn write_file_empty_data() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.write_file(current_dir, "WTEST2.TXT", b"").unwrap();
            let entry = vol.open(current_dir, "WTEST2.TXT").unwrap();
            assert_eq!(entry.file_size, 0);
        });
    }

    #[test_case]
    fn write_file_overwrite() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.write_file(current_dir, "WTEST3.TXT", b"first").unwrap();
            vol.write_file(current_dir, "WTEST3.TXT", b"second")
                .unwrap();
            let entry = vol.open(current_dir, "WTEST3.TXT").unwrap();
            assert_eq!(entry.file_size, 6);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(&content, b"second");
        });
    }

    #[test_case]
    fn write_file_multi_sector() {
        let current_dir = DirHandle { start_cluster: 0 };
        // Write more than one sector (512 bytes)
        with_volume(|vol| {
            let data = [b'A'; 1024];
            vol.write_file(current_dir, "WTEST4.TXT", &data).unwrap();
            let entry = vol.open(current_dir, "WTEST4.TXT").unwrap();
            assert_eq!(entry.file_size, 1024);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 1024);
            assert!(content.iter().all(|&b| b == b'A'));
        });
    }

    #[test_case]
    fn write_file_multi_cluster() {
        let current_dir = DirHandle { start_cluster: 0 };
        // Write more than one cluster (sectors_per_cluster * 512 = 2048 bytes)
        with_volume(|vol| {
            let data = [b'B'; 4096];
            vol.write_file(current_dir, "WTEST5.TXT", &data).unwrap();
            let entry = vol.open(current_dir, "WTEST5.TXT").unwrap();
            assert_eq!(entry.file_size, 4096);
            let content = vol.read_file(&entry).unwrap();
            assert_eq!(content.len(), 4096);
            assert!(content.iter().all(|&b| b == b'B'));
        });
    }

    #[test_case]
    fn write_file_invalid_name() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            let result = vol.write_file(current_dir, "TOOLONGNAME.TXT", b"data");
            assert!(matches!(result, Err(FsError::InvalidName)));
        });
    }

    #[test_case]
    fn touch_does_not_overwrite_existing() {
        let current_dir = DirHandle { start_cluster: 0 };
        with_volume(|vol| {
            vol.write_file(current_dir, "WTEST6.TXT", b"keep this")
                .unwrap();
            vol.create_empty_file(current_dir, "WTEST6.TXT").unwrap();
            let entry = vol.open(current_dir, "WTEST6.TXT").unwrap();
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
            assert_eq!(vol.fat_entry(cluster).unwrap(), FatEntry::End);
            // Clean up
            vol.set_fat_entry(cluster, FatEntry::Free).unwrap();
        });
    }

    // =========================================================================
    // DirEntryIter tests
    // =========================================================================

    #[test_case]
    fn dir_iter_yields_single_trailing_empty() {
        let root = DirHandle { start_cluster: 0 };
        // The fused contract: the Empty terminator is yielded exactly once,
        // as the final item, and the iterator stays finished afterwards.
        with_volume(|vol| {
            let mut buf = [0u8; SECTOR_SIZE];
            let mut iter = vol.dir_iter(&mut buf, root).unwrap();
            let mut empties = 0;
            let mut items_after_empty = 0;
            for item in iter.by_ref() {
                match item.unwrap().dir_entry_kind {
                    DirEntryKind::Empty => empties += 1,
                    _ if empties > 0 => items_after_empty += 1,
                    _ => {}
                }
            }
            assert_eq!(empties, 1, "expected exactly one Empty item");
            assert_eq!(
                items_after_empty, 0,
                "no items may follow the Empty frontier"
            );
            assert!(
                iter.next().is_none(),
                "iterator must stay finished after returning None"
            );
        });
    }

    #[test_case]
    fn dir_iter_ignores_stale_bytes_beyond_terminator() {
        let root = DirHandle { start_cluster: 0 };
        // The FAT spec says nothing after the first 0x00 entry is valid, but
        // disks formatted elsewhere can carry stale bytes there. Plant a
        // convincing used entry one slot past the terminator and check it is
        // not reachable through the iterator path.
        with_volume(|vol| {
            // Locate the terminator via the iterator
            let mut buf = [0u8; SECTOR_SIZE];
            let mut empty_loc = None;
            for item in vol.dir_iter(&mut buf, root).unwrap() {
                let item = item.unwrap();
                if matches!(item.dir_entry_kind, DirEntryKind::Empty) {
                    empty_loc = Some(item.sector_location);
                }
            }
            let empty_loc = empty_loc.expect("root dir should have a free slot");
            // The slot after the terminator; may roll into the next sector
            let (sector, offset) = if empty_loc.offset + DIR_ENTRY_BYTES == SECTOR_SIZE {
                (empty_loc.sector + 1, 0)
            } else {
                (empty_loc.sector, empty_loc.offset + DIR_ENTRY_BYTES)
            };
            // Plant the phantom entry, keeping the original bytes
            let mut sector_buf = [0u8; SECTOR_SIZE];
            vol.read_sector(sector, &mut sector_buf).unwrap();
            let mut original = [0u8; DIR_ENTRY_BYTES];
            original.copy_from_slice(&sector_buf[offset..offset + DIR_ENTRY_BYTES]);
            let phantom = FileInfo {
                name: *b"PHANTOM ",
                extension: *b"TXT",
                attributes: ATTR_ARCHIVE,
                first_cluster: 2,
                file_size: 5,
            };
            sector_buf[offset..offset + DIR_ENTRY_BYTES]
                .copy_from_slice(&phantom.as_bytes(vol.bpb.volume_type));
            vol.write_sector(sector, &sector_buf).unwrap();

            let result = vol.open(root, "PHANTOM.TXT");

            // Restore the on-disk bytes before asserting so a green run
            // leaves the image untouched for later tests
            vol.read_sector(sector, &mut sector_buf).unwrap();
            sector_buf[offset..offset + DIR_ENTRY_BYTES].copy_from_slice(&original);
            vol.write_sector(sector, &sector_buf).unwrap();

            assert!(
                matches!(result, Err(FsError::NotFound)),
                "entry beyond the 0x00 terminator must be invisible"
            );
        });
    }
}
