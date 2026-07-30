//! Volume for FAT

// The volume is the mounted instance and translates from relative sectors to absolute blocks
// Methods do not take the lock
// See file.rs for API

use core::ops::ControlFlow;

use crate::drivers::virtio::blk::BlkError;
use crate::kernel::sync::SpinLock;

use super::blockcache::BlockCache;
use super::bpb::Bpb;
use super::dir::{self, DirEntry, DirEntryKind, FileInfo};
use super::fat::{self, FatEntry};
use super::{Dir, FileHandle, FsError, Location, SECTOR_SIZE, VolumeType};

const BASE_CLUSTER: u32 = 2;

// Need to lock with interrupts enabled waiting for IO completion
// SpinLock (not IrqSpinLock) because I/O needs interrupts enabled for virtio
// completion. Must never be accessed from an interrupt handler.
pub(super) static VOLUME: SpinLock<Option<Volume>> = SpinLock::new(None);

pub(crate) struct Volume {
    bpb: Bpb,
    lba: u32,
    last_alloc_cluster: u32,
    block_cache: BlockCache,
}

// Methods on Volume never take the lock
impl Volume {
    pub fn new(lba: u32, bpb: Bpb) -> Self {
        Self {
            bpb,
            lba,
            last_alloc_cluster: BASE_CLUSTER,
            block_cache: BlockCache::new(),
        }
    }

    #[inline(always)]
    fn fat_entry_size(&self) -> usize {
        match self.bpb.volume_type {
            VolumeType::Fat16(_) => core::mem::size_of::<u16>(),
            VolumeType::Fat32(_) => core::mem::size_of::<u32>(),
        }
    }

    fn fat_entry_count(&self) -> u32 {
        let entry_size = self.fat_entry_size();
        (SECTOR_SIZE / entry_size) as u32
    }

    fn read_sector(&mut self, sector: u32) -> Result<&[u8; SECTOR_SIZE], BlkError> {
        // Translate to block
        let block = sector + self.lba;
        self.block_cache.read(block)
    }

    #[cfg(test)]
    fn read_sector_uncached(
        &mut self,
        sector: u32,
        buf: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlkError> {
        // Translate to block
        let block = sector + self.lba;
        self.block_cache.read_uncached(block, buf)
    }

    pub(super) fn modify_sector<F>(&mut self, sector: u32, f: F) -> Result<(), BlkError>
    where
        F: FnOnce(&mut [u8; SECTOR_SIZE]),
    {
        // Translate to block
        let block = sector + self.lba;
        self.block_cache.modify(block, f)
    }

    fn write_sector_uncached(
        &mut self,
        sector: u32,
        buf: &[u8; SECTOR_SIZE],
    ) -> Result<(), BlkError> {
        // Translate to block
        let block = sector + self.lba;
        self.block_cache.write_uncached(block, buf)
    }

    /// Extend a cluster-based file chain by one new cluster
    ///
    /// Cluster is zeroed
    fn extend_cluster_chain(&mut self, current_cluster: u32) -> Result<u32, FsError> {
        let next_cluster = self.allocate_cluster()?;
        self.set_fat_entry(current_cluster, FatEntry::Next(next_cluster))?;
        // Now zero the new cluster
        let sector = self.bpb.cluster_to_sector(next_cluster);
        for i in 0..self.bpb.sectors_per_cluster {
            self.write_sector_uncached(sector + i, &[0u8; SECTOR_SIZE])?;
        }
        Ok(next_cluster)
    }

    /// Move to the next cluster in a file chain: follow the existing FAT link,
    /// or allocate and link a new cluster if `current_cluster` is end-of-chain.
    fn next_or_new_cluster(&mut self, current_cluster: u32) -> Result<u32, FsError> {
        match self.fat_entry(current_cluster)?.next_in_chain()? {
            Some(next) => Ok(next),
            None => self.extend_cluster_chain(current_cluster),
        }
    }

    /// Read a single FAT entry for the given cluster number
    ///
    /// The cluster number is the index into the FAT
    /// The cache is updated to the FAT sector that has been read.
    fn fat_entry(&mut self, cluster: u32) -> Result<FatEntry, FsError> {
        // Update the FAT cache if needed
        let num_entries = self.fat_entry_count();
        let entry_size = self.fat_entry_size();
        let volume_type = self.bpb.volume_type;
        // Work out the FAT sector and load
        let fat_sector = self.bpb.fat_start_sector() + (cluster / num_entries);
        let buf = self.read_sector(fat_sector)?;
        // Work out the offset within the sector
        let offset = (cluster % num_entries) as usize * entry_size;
        // Now look inside the block (in buffer) to read the offset of the entry
        let fat_entry = match volume_type {
            VolumeType::Fat16(_) => {
                fat::parse_entry_fat16(&buf[offset..offset + entry_size].try_into().unwrap())
            }
            VolumeType::Fat32(_) => {
                fat::parse_entry_fat32(&buf[offset..offset + entry_size].try_into().unwrap())
            }
        };
        Ok(fat_entry)
    }

    /// Set FAT entry to a value
    fn set_fat_entry(&mut self, cluster: u32, fat_entry: FatEntry) -> Result<(), FsError> {
        // Update the FAT cache if needed
        // let (num_entries, entry_size) = self.update_fat_cache(cluster)?;
        let num_entries = self.fat_entry_count();
        let entry_size = self.fat_entry_size();
        let volume_type = self.bpb.volume_type;
        // Work out the FAT sector and load
        let fat_sector = self.bpb.fat_start_sector() + (cluster / num_entries);
        let offset = (cluster % num_entries) as usize * entry_size;
        self.modify_sector(fat_sector, |buf| match volume_type {
            VolumeType::Fat16(_) => {
                buf[offset..offset + entry_size].copy_from_slice(&fat_entry.as_bytes_16())
            }
            VolumeType::Fat32(_) => {
                let current = buf[offset] as u32;
                buf[offset..offset + entry_size].copy_from_slice(&fat_entry.as_bytes_32(current));
            }
        })?;
        // Re-read and write back to all FAT copies
        let sectors_per_fat = self.bpb.sectors_per_fat;
        let buf = *self.read_sector(fat_sector)?;
        for i in 1..self.bpb.fat_count {
            self.write_sector_uncached(fat_sector + i * sectors_per_fat, &buf)?;
        }
        Ok(())
    }

    /// Finds directory entry by name and returns sector, offset and DirEntry
    ///
    /// Helper function for rm and file write
    pub(super) fn find_file_dir_entry(
        &mut self,
        dir: Dir,
        name: &str,
    ) -> Result<(Location, FileInfo), FsError> {
        for dir_entry in self.dir_iter(dir)? {
            let dir_entry = dir_entry?;
            if let DirEntryKind::Used(file_info) = dir_entry.kind
                && let Ok(n) = file_info.filename().as_str()
                && n.eq_ignore_ascii_case(name)
            {
                // Found a match - return location and file info
                return Ok((dir_entry.location, file_info));
            }
        }
        Err(FsError::NotFound)
    }

    /// Finds the next available directory entry, reusing deleted entries.
    ///
    /// Returns the sector and offset.
    fn get_avail_dir_entry(&mut self, dir: Dir) -> Result<Location, FsError> {
        for dir_entry in self.dir_iter(dir)? {
            let dir_entry = dir_entry?;
            match dir_entry.kind {
                DirEntryKind::Deleted | DirEntryKind::Empty => {
                    return Ok(dir_entry.location);
                }
                _ => {}
            }
        }
        // Reached the end of the directory without finding an available slot
        match (dir, self.bpb.volume_type) {
            (Dir::Root, VolumeType::Fat16(_)) => Err(FsError::DirFull),
            (Dir::Root, VolumeType::Fat32(start_cluster)) | (Dir::SubDir(start_cluster), _) => {
                // Extend the directory file to accommodate extra directory slots
                // First walk the FAT chain to the end
                let mut curr_cluster = start_cluster;
                while let Some(next) = self.fat_entry(curr_cluster)?.next_in_chain()? {
                    curr_cluster = next;
                }
                // Now extend the current cluster
                let next_cluster = self.extend_cluster_chain(curr_cluster)?;
                // Finally offer the first slot as available
                Ok(Location {
                    sector: self.bpb.cluster_to_sector(next_cluster),
                    offset: 0,
                })
            }
        }
    }

    // /// Make a new directory
    // pub fn make_dir(&mut self, dir: Dir, dirname: &str) -> Result<(), FsError> {
    //     // Check if the directory name already exists in this directory
    //     let name = FileInfo::parse_83_name(dirname);
    //     self.read_dir(dir, |entry| {
    //         if entry
    //     })
    // }

    /// Read directory
    pub fn read_dir<B, F>(&mut self, dir: Dir, mut f: F) -> Result<ControlFlow<B>, FsError>
    where
        F: FnMut(&FileInfo) -> ControlFlow<B>,
    {
        for dir_entry in self.dir_iter(dir)? {
            let dir_entry = dir_entry?;
            match dir_entry.kind {
                DirEntryKind::Used(entry) => {
                    if let ControlFlow::Break(val) = f(&entry) {
                        return Ok(ControlFlow::Break(val));
                    }
                }
                DirEntryKind::Unsupported => continue,
                DirEntryKind::Deleted | DirEntryKind::Empty => {
                    return Ok(ControlFlow::Continue(()));
                }
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Finds the FileInfo for a given directory entry location
    fn get_file_info(&mut self, location: Location) -> Result<FileInfo, FsError> {
        // Read the current file_info and update
        let sector_buf = self.read_sector(location.sector)?;
        let dir_entry_bytes = &sector_buf[location.offset..location.offset + dir::ENTRY_BYTES];
        if let DirEntryKind::Used(file_info) =
            DirEntryKind::parse(dir_entry_bytes.try_into().unwrap(), self.bpb.volume_type)
        {
            Ok(file_info)
        } else {
            Err(FsError::DirectoryEntryNotInUse)
        }
    }

    /// Convert a file position to a location
    ///
    /// We assume that `position` must be within `current_cluster`.
    fn get_file_position_location(&self, file: &FileHandle) -> Location {
        let current_cluster_start_sector = self.bpb.cluster_to_sector(file.current_cluster);
        let sector = current_cluster_start_sector
            + (file.position / SECTOR_SIZE as u32) % self.bpb.sectors_per_cluster;
        let offset = file.position as usize % SECTOR_SIZE;
        Location { sector, offset }
    }

    // Create an empty file ("touch")
    pub fn create_empty_file(&mut self, dir: Dir, filename: &str) -> Result<(), FsError> {
        // Search the directory for the existance of the file
        if self.find_file_dir_entry(dir, filename).is_ok() {
            // File already exists
            return Err(FsError::AlreadyExists);
        }
        // Find an available directoy slot
        let slot = self.get_avail_dir_entry(dir)?;
        // Construct the directory entry file infos
        let (name, extension) = FileInfo::parse_83_name(filename)?;
        let file_info = FileInfo {
            name,
            extension,
            attributes: dir::ATTR_ARCHIVE,
            first_cluster: 0,
            file_size: 0,
        };
        let volume_type = self.bpb.volume_type;
        // Read the sector and write back with new entry
        self.modify_sector(slot.sector, |buf| {
            buf[slot.offset..slot.offset + dir::ENTRY_BYTES]
                .copy_from_slice(&file_info.as_bytes(volume_type));
        })?;
        Ok(())
    }

    /// Read from a given postion in a file
    pub(super) fn read_at(
        &mut self,
        file: &mut FileHandle,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
        // If the file is empty or we are at the end then we are already done
        if file.size == 0 || file.size == file.position {
            return Ok(0);
        }
        // Check that our position is within the file
        if file.size < file.position {
            return Err(FsError::ReadPastEndOfFile);
        }
        let loc = self.get_file_position_location(file);
        let byte_count = (SECTOR_SIZE - loc.offset)
            .min((file.size - file.position) as usize)
            .min(buf.len());
        // Read `byte_count` bytes out of the sector
        let sector_buf = self.read_sector(loc.sector)?;
        buf[..byte_count].copy_from_slice(&sector_buf[loc.offset..loc.offset + byte_count]);
        let old_position = file.position;
        file.position += byte_count as u32;
        // Check whether we have crossed into a new cluster
        let bytes_per_cluster = self.bpb.sectors_per_cluster * SECTOR_SIZE as u32;
        if old_position / bytes_per_cluster != file.position / bytes_per_cluster {
            // We need to advance to the next cluster
            let next_cluster = self.fat_entry(file.current_cluster)?;
            if let Some(next) = next_cluster.next_in_chain()? {
                file.current_cluster = next;
            } else {
                // We have reached the end of the file
                return Ok(byte_count);
            }
        }
        Ok(byte_count)
    }

    // Helper function to delete all the clusters in a file cluster chain
    fn delete_file_chain(&mut self, first_cluster: u32) -> Result<(), FsError> {
        if first_cluster != 0 {
            // Walk through the cluster chain and unlink each in turn
            let mut cluster = first_cluster;
            loop {
                let next = self.fat_entry(cluster)?;
                self.set_fat_entry(cluster, FatEntry::Free)?;
                if let Some(c) = next.next_in_chain()? {
                    cluster = c;
                } else {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    // Delete a file
    //
    // First unlink any clusters, then mark the directory entry as deleted
    pub(super) fn delete_file(
        &mut self,
        location: Location,
        file_info: &FileInfo,
    ) -> Result<(), FsError> {
        // Check if this is a file
        if file_info.attributes & dir::ATTR_DIR != 0 {
            return Err(FsError::DirectoryInsteadOfFile);
        }
        self.delete_file_chain(file_info.first_cluster)?;
        self.modify_sector(location.sector, |buf| {
            // Change the directory entry first byte to mark as deleted.
            // For FAT filesystems we only change the first byte - all the
            // rest of the entry bytes remain in place
            buf[location.offset] = dir::ENTRY_DEL;
        })?;
        Ok(())
    }

    // First unlink any clusters, then mark the directory entry as deleted
    pub(super) fn truncate_file(&mut self, file: &mut FileHandle) -> Result<(), FsError> {
        let file_info = self.get_file_info(file.dir_entry_location)?;
        // Check if this is a file
        if file_info.attributes & dir::ATTR_DIR != 0 {
            return Err(FsError::DirectoryInsteadOfFile);
        }
        file.first_cluster = 0;
        self.delete_file_chain(file_info.first_cluster)?;
        file.size = 0;
        file.current_cluster = 0;
        Ok(())
    }

    // // Remove a directory
    // //
    // // First check whether the directory is empty and not the root directory.
    // // Then mark the directory entry as deleted
    // // Does not delete if the directory handle is the same directory
    // pub fn delete_directory(&mut self, curr_dir: Dir, dirname: &str) -> Result<(), FsError> {
    //     // Get the sector and offset of the directory by name
    //     let (location, dir_entry) = self.find_dir_entry_location(curr_dir, dirname)?;
    //     // Check if this is a file
    //     if dir_entry.attributes != dir::ATTR_DIR {
    //         return Err(FsError::FileInsteadOfDirectory);
    //     }
    //     // Check if it is the root directory
    //     match self.bpb.volume_type {
    //         VolumeType::Fat16((root_dir_start_sector, _)) && root_dir_start_sector == self.bpb.cluster_to_sector() =>
    //     }
    //     // Check if it is the current directory
    //     if dir.start_cluster == dir_entry.first_cluster {
    //         return Err(FsError::DirectoryIsCurrent);
    //     }
    //     // Check if the directory is empty
    //     if dir_entry.first_cluster == 0 {
    //         return Err(FsError::DirectoryInvalid);
    //     }
    //     // Construct a handle for the directory to be deleted
    //     let dir = Dir { start_cluster: dir_entry.first_cluster };
    //     // Read the directory and ensure it is empty other than the first two entries
    //     for (i, entry) in self.dir_iter(dir)?.enumerate() {
    //         // First entry should be "."
    //         match entry? {
    //             DirEntryKind::Used(d) => {
    //                 d.name
    //             }
    //         }
    //     }
    //     // Read the first sector and look at the first two directory entries
    //     let dir_first_sector = self.read_sector(self.bpb.cluster_to_sector(dir_entry.first_cluster))?;
    //     // Check first entry is parent directory link
    //     match DirEntryKind::parse(dir_first_sector[0..dir::ENTRY_BYTES].try_into().unwrap(), self.bpb.volume_type) {
    //         DirEntryKind::Used(d) => {
    //             if d.name[0] != b"."
    //                 || d.extension != b"   ",
    //                 || d.attributes != dir::ATTR_DIR
    //                 || d.file_size > 0 {
    //                     return Err(FsError::DirectoryInvalid);
    //                 }
    //         },
    //         _ => { return Err(FsError::DirectoryInvalid); }
    //     }
    //     // Check second entry is same directory link
    //     match DirEntryKind::parse(dir_first_sector[dir::ENTRY_BYTES..2 * dir::ENTRY_BYTES].try_into().unwrap(), self.bpb.volume_type) {
    //         DirEntryKind::Used(d) => {
    //             if d.name[0..2] != b".."
    //                 || d.extension != b"   ",
    //                 || d.attributes != dir::ATTR_DIR
    //                 || d.file_size > 0 {
    //                     return Err(FsError::DirectoryInvalid);
    //                 }
    //         },
    //         _ => { return Err(FsError::DirectoryInvalid); },
    //     }
    //     // Check third entry is empty
    //     match DirEntryKind::parse(dir_first_sector[dir::ENTRY_BYTES * 2..dir::ENTRY_BYTES * 3].try_into().unwrap(), self.bpb.volume_type) {
    //         DirEntryKind::Empty => {}
    //         _ => { return Err(FsError::DirectoryInvalid); }
    //     }
    //     self.delete_file_chain(dir_entry.first_cluster)?;
    //     // Now remove the directory entry itself
    //     self.modify_sector(location.sector, |buf| {
    //         // Change the directory entry first byte to mark as deleted.
    //         // For FAT filesystems we only change the first byte - all the
    //         // rest of the entry bytes remain in place
    //         buf[location.offset] = dir::ENTRY_DEL;
    //     })?;
    //     Ok(())
    // }

    /// Helper file write function to allocate the first cluster of a new file
    pub fn allocate_cluster(&mut self) -> Result<u32, FsError> {
        // We start at the last_alloc_cluster but wrap back to BASE_CLUSTER to scan the full drive
        for cluster in (self.last_alloc_cluster..self.bpb.total_data_clusters())
            .chain(BASE_CLUSTER..BASE_CLUSTER + self.last_alloc_cluster)
        {
            if self.fat_entry(cluster)? == FatEntry::Free {
                // read from FAT table
                self.set_fat_entry(cluster, FatEntry::End)?; // Write end of file to FAT table
                self.last_alloc_cluster = cluster + 1;
                return Ok(cluster);
            }
        }
        Err(FsError::DiskFull)
    }

    /// Writes from a buffer to an existing file starting at a position
    ///
    /// Will overwrite existing data.
    /// Returns the number of bytes written on success
    /// Does not update file metadata - expect user to call file::close
    pub(super) fn write_at(&mut self, file: &mut FileHandle, buf: &[u8]) -> Result<usize, FsError> {
        // If the write buffer is empty we are done
        if buf.is_empty() {
            return Ok(0);
        }
        // If this is an empty file we need to allocate the first cluster
        if file.first_cluster == 0 {
            file.first_cluster = self.allocate_cluster()?;
            file.current_cluster = file.first_cluster;
        }
        // Find the location associated with the current file position and set cursors
        let bytes_per_cluster = self.bpb.sectors_per_cluster * SECTOR_SIZE as u32;
        if file.position > 0 && file.position.is_multiple_of(bytes_per_cluster) {
            // position sits on a cluster boundary: current_cluster is the cluster
            // that ends here, so step to the one that starts here.
            file.current_cluster = self.next_or_new_cluster(file.current_cluster)?;
        }
        let loc = self.get_file_position_location(file);
        let mut sector = loc.sector;
        let mut offset = loc.offset;
        let mut write_pos = 0;
        let mut num_bytes_written = 0u32;
        loop {
            // Copy as many bytes as fit between the cursor and the end of the sector.
            let byte_count = (SECTOR_SIZE - offset).min(buf.len() - write_pos);
            self.modify_sector(sector, |sector_buf| {
                sector_buf[offset..offset + byte_count]
                    .copy_from_slice(&buf[write_pos..write_pos + byte_count])
            })?;
            write_pos += byte_count;
            num_bytes_written += byte_count as u32;
            // Done once the whole write buffer has been consumed.
            if write_pos == buf.len() {
                break;
            }
            // The sector is full but data remains: advance to the next sector,
            // following the FAT chain (or allocating a cluster) at a cluster boundary.
            let last_sector_in_cluster =
                self.bpb.cluster_to_sector(file.current_cluster) + self.bpb.sectors_per_cluster - 1;
            if sector == last_sector_in_cluster {
                file.current_cluster = self.next_or_new_cluster(file.current_cluster)?;
                sector = self.bpb.cluster_to_sector(file.current_cluster);
            } else {
                sector += 1;
            }
            offset = 0;
        }
        // Update the file details
        file.position += num_bytes_written;
        file.size = file.size.max(file.position);
        Ok(num_bytes_written as usize)
    }

    /// Saves the file size and first_cluster of an existing file
    pub(super) fn save_file_meta_data(&mut self, file: &FileHandle) -> Result<(), FsError> {
        let mut file_info = self.get_file_info(file.dir_entry_location)?;
        // Update the file info with new metadata
        file_info.first_cluster = file.first_cluster;
        file_info.file_size = file.size;
        let volume_type = self.bpb.volume_type;
        // Now read sector, update entry and write back
        self.modify_sector(file.dir_entry_location.sector, |buf| {
            buf[file.dir_entry_location.offset..file.dir_entry_location.offset + dir::ENTRY_BYTES]
                .copy_from_slice(&file_info.as_bytes(volume_type));
        })?;
        Ok(())
    }

    // Create an interator struct for directory entries
    pub(super) fn dir_iter<'a>(&'a mut self, dir: Dir) -> Result<DirEntryIter<'a>, FsError> {
        // Match the starting directory entry
        let (current_sector, sectors_in_run, next_cluster) = match (dir, self.bpb.volume_type) {
            (Dir::Root, VolumeType::Fat16((root_dir_start_sector, root_dir_sector_count))) => {
                (root_dir_start_sector, root_dir_sector_count, None)
            }
            (Dir::Root, VolumeType::Fat32(start_cluster)) | (Dir::SubDir(start_cluster), _) => {
                let next_cluster = self.fat_entry(start_cluster)?.next_in_chain()?;
                let start_sector = self.bpb.cluster_to_sector(start_cluster);
                (start_sector, self.bpb.sectors_per_cluster, next_cluster)
            }
        };

        self.read_sector(current_sector)?;

        Ok(DirEntryIter {
            volume: self,
            current_location: Location {
                offset: 0,
                sector: current_sector,
            },
            sectors_left_in_run: sectors_in_run as usize - 1,
            next_cluster,
            returned_empty: false,
        })
    }
}

pub(super) struct DirEntryIter<'a> {
    volume: &'a mut Volume,
    current_location: Location,
    sectors_left_in_run: usize,
    next_cluster: Option<u32>,
    returned_empty: bool,
}

impl<'a> DirEntryIter<'a> {
    fn next_inner(&mut self) -> Result<Option<DirEntry>, FsError> {
        // Are we already done?
        if self.returned_empty {
            return Ok(None);
        }
        // Use cached values to return next DirEntry
        if self.current_location.offset == SECTOR_SIZE {
            // We are at the end of this sector, get the next one
            if self.sectors_left_in_run == 0 {
                // We are at the end of a sector run, load the next cluster if possible
                if let Some(next) = self.next_cluster {
                    self.current_location.sector = self.volume.bpb.cluster_to_sector(next);
                    self.next_cluster = self.volume.fat_entry(next)?.next_in_chain()?;
                    self.sectors_left_in_run = self.volume.bpb.sectors_per_cluster as usize;
                } else {
                    // We are done - no entries left in this directory
                    return Ok(None);
                }
            } else {
                self.sectors_left_in_run -= 1;
                self.current_location.sector += 1;
            }
            self.current_location.offset = 0;
        };
        // We use the cached values to get the directory entry raw bytes and parse
        let buf = self.volume.read_sector(self.current_location.sector)?;
        let offset = self.current_location.offset;
        self.current_location.offset = offset + dir::ENTRY_BYTES;
        let kind = DirEntryKind::parse(
            buf[offset..self.current_location.offset]
                .try_into()
                .unwrap(),
            self.volume.bpb.volume_type,
        );
        if kind == DirEntryKind::Empty {
            // We only return the first Empty, afterwards the iterator is done
            self.returned_empty = true;
        }
        Ok(Some(DirEntry {
            location: Location {
                sector: self.current_location.sector,
                offset,
            },
            kind,
        }))
    }
}

// Iterator over directory entries
impl Iterator for DirEntryIter<'_> {
    type Item = Result<DirEntry, FsError>;

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

pub fn fat_init(lba: u32, bpb: Bpb) {
    let vol = Volume::new(lba, bpb);
    *VOLUME.lock() = Some(vol);
}

// Kernel-only QEMU tests (see the module doc in volume/tests.rs)
#[cfg(all(test, target_os = "none", feature = "test-fs"))]
mod tests;
