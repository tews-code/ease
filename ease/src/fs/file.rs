//! File operations

// Functions take the lock on the volume (and delegate as needed)
use crate::kernel::sync::IrqSpinLock;

use super::volume::with_volume;
use super::{Dir, FileInfo, FsError, Location};

const OPEN_FILES_MAX: usize = 128;

static OPEN_FILE_TABLE: IrqSpinLock<OpenFileTable> =
    IrqSpinLock::new(OpenFileTable([const { None }; OPEN_FILES_MAX]));

#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum Access {
    Read,
    Write,
}

pub(super) struct OpenFile {
    access: Access,
    pub(super) dir_entry_location: Location,
    pub(super) position: u32,
    pub(super) size: u32,
    pub(super) first_cluster: u32,
    pub(super) current_cluster: u32,
}

#[derive(Clone, Copy)]
pub(super) struct OpenFileSnapshot {
    pub(super) dir_entry_location: Location,
    pub(super) position: u32,
    pub(super) size: u32,
    pub(super) first_cluster: u32,
    pub(super) current_cluster: u32,
}

impl From<&OpenFile> for OpenFileSnapshot {
    fn from(open_file: &OpenFile) -> Self {
        Self {
            dir_entry_location: open_file.dir_entry_location,
            position: open_file.position,
            size: open_file.size,
            first_cluster: open_file.first_cluster,
            current_cluster: open_file.current_cluster,
        }
    }
}

/// Table with open files
///
/// Each open file has its own slot (no reference counting)
/// Table indices get reused after close. This is safe only because FileHandle can't be cloned
/// and its Drop frees the slot — single ownership.
/// If ever adding `dup`, this is the first thing that breaks.
///
/// # Invariants #
/// - Slot indices stay valid because FileHandle can't be cloned and Drop frees the slot.
/// - Stored Locations stay valid because every operation that invalidates a location (e.g. rm) refuses open files.
struct OpenFileTable([Option<OpenFile>; OPEN_FILES_MAX]);

impl OpenFileTable {
    /// Get mutable reference to a free slot in the table and the slot index
    fn free_slot(&mut self) -> Option<(usize, &mut Option<OpenFile>)> {
        self.0.iter_mut().enumerate().find(|(_, s)| s.is_none())
    }

    /// Check if a file is open for the directory entry location
    fn is_open(&self, location: Location) -> bool {
        self.0
            .iter()
            .flatten()
            .any(|s| s.dir_entry_location == location)
    }
    /// Get the access kind of a file by its directory entry location
    ///
    /// Returns None if the file is not found in the open file table
    fn access_by_location(&self, entry: Location) -> Option<Access> {
        self.0
            .iter()
            .flatten()
            .find(|of| of.dir_entry_location == entry)
            .map(|of| of.access)
    }
    /// Save the open file snapshot details
    ///
    /// # Panics #
    /// Panics if the file handle does not correspond with an open file entry in the table
    fn set_snapshot(&mut self, file: &FileHandle, snapshot: OpenFileSnapshot) {
        let open_file = self.0[file.open_file_table_idx]
            .as_mut()
            .expect("file handle should index the open file table to an open file entry");
        open_file.position = snapshot.position;
        open_file.size = snapshot.size;
        open_file.first_cluster = snapshot.first_cluster;
        open_file.current_cluster = snapshot.current_cluster;
    }
    /// Gets a file state snapshot of a file opened with write access
    ///
    /// Returns None if the file is open for read
    /// # Panics #
    /// Panics if the file handle does not correspond to an open file entry in the table
    fn writable_snapshot(&self, file: &FileHandle) -> Option<OpenFileSnapshot> {
        let open_file = self.0[file.open_file_table_idx]
            .as_ref()
            .expect("file handle should index the open file table to an open file entry");
        (open_file.access == Access::Write).then(|| open_file.into())
    }
    /// Gets a snapshot copy of open file state
    /// Note that a separate write back method needs to be called to save updated values
    ///
    /// # Panics #
    /// Panics if the file handle does not correspond with an open file entry in the table
    fn snapshot(&self, file: &FileHandle) -> OpenFileSnapshot {
        let open_file = self.0[file.open_file_table_idx]
            .as_ref()
            .expect("file handle should index the open file table to an open file entry");
        open_file.into()
    }
    /// Create a new entry in the open file table
    ///
    /// Takes an access type and directory entry location
    /// Returns an index to the open file table on success or a file system error
    fn open_for(
        &mut self,
        access: Access,
        dir_entry: Location,
        file_info: &FileInfo,
    ) -> Result<usize, FsError> {
        match (access, self.access_by_location(dir_entry)) {
            (Access::Write, Some(_)) => return Err(FsError::OpeningForWriteButAlreadyOpen),
            (Access::Read, Some(Access::Write)) => {
                return Err(FsError::OpeningForReadButWriteInProgress);
            }
            _ => {}
        }
        // Find an empty slot and insert the entry - no problem if mutiple read users
        // of the same file
        if let Some((i, new_slot)) = self.free_slot() {
            *new_slot = Some(OpenFile {
                access,
                dir_entry_location: dir_entry,
                position: 0,
                size: file_info.file_size,
                current_cluster: file_info.first_cluster,
                first_cluster: file_info.first_cluster,
            });
            return Ok(i);
        }
        Err(FsError::TooManyOpenFiles)
    }

    fn close(&mut self, idx: usize) {
        self.0[idx].take();
    }
}

#[derive(Debug)]
pub(crate) struct FileHandle {
    open_file_table_idx: usize,
    closed: bool,
}

impl FileHandle {
    pub(crate) fn close(mut self) -> Result<(), FsError> {
        // Record that we have tried to close the file
        self.closed = true;
        // Check whether the file was write access
        if let Some(open_file) = OPEN_FILE_TABLE.lock().writable_snapshot(&self) {
            with_volume(|vol| {
                vol.save_file_meta_data(
                    open_file.dir_entry_location,
                    open_file.first_cluster,
                    open_file.size,
                )
            })?
        }
        Ok(())
    }
}

impl Drop for FileHandle {
    fn drop(&mut self) {
        if !self.closed {
            // Dropping without a close - make a best effort try to save metadata
            // Check whether the file was write access
            if let Some(open_file) = OPEN_FILE_TABLE.lock().writable_snapshot(self) {
                with_volume(|vol| {
                    let _ = vol.save_file_meta_data(
                        open_file.dir_entry_location,
                        open_file.first_cluster,
                        open_file.size,
                    );
                })
            }
        }
        OPEN_FILE_TABLE.lock().close(self.open_file_table_idx);
    }
}

/// Change working directory
pub(crate) fn change_directory(wd: &mut Dir, dirname: &str) -> Result<(), FsError> {
    with_volume(|vol| vol.change_directory(wd, dirname))
}

/// Seek within file
///
/// Does not support seek beyond file end
pub(crate) fn lseek(file: &FileHandle, seek_bytes: u32) -> Result<(), FsError> {
    let snapshot = OPEN_FILE_TABLE.lock().snapshot(file);
    let snapshot = with_volume(|vol| vol.seek(snapshot, seek_bytes))?;
    OPEN_FILE_TABLE.lock().set_snapshot(file, snapshot);
    Ok(())
}

pub(crate) fn mkdir(dir: Dir, dirname: &str) -> Result<(), FsError> {
    with_volume(|vol| vol.make_dir(dir, dirname))
}

pub(crate) fn open(access: Access, dir: Dir, filename: &str) -> Result<FileHandle, FsError> {
    // Need both volume and table locks for safe opening
    // The table lock disables IRQs, so must always be the inner lock
    with_volume(|vol| {
        // Resolve any leading path, then look up the final name.
        let (dir, filename) = vol.resolve_parent(dir, filename)?;
        // Check if this file already exists
        let (location, file_info) = vol.find_file_dir_entry(dir, filename)?;
        // Set up the open file table
        let idx = OPEN_FILE_TABLE
            .lock()
            .open_for(access, location, &file_info)?;
        // Construct the file handle with private idx member
        Ok(FileHandle {
            open_file_table_idx: idx,
            closed: false,
        })
    })
}

pub(crate) fn read_at(file_handle: &FileHandle, buf: &mut [u8]) -> Result<usize, FsError> {
    with_volume(|vol| {
        let snapshot = OPEN_FILE_TABLE.lock().snapshot(file_handle);
        if snapshot.position > snapshot.size {
            return Err(FsError::ReadPastEndOfFile);
        }
        // If the file is empty or we are at the end then we are already done
        if snapshot.size == 0 || snapshot.size == snapshot.position {
            return Ok(0);
        }

        // Take lock on volume and perform the read
        let (snapshot, num_bytes) = vol.read_at(snapshot, buf)?;

        if snapshot.position > snapshot.size {
            return Err(FsError::ReadPastEndOfFile);
        }
        // Re-lock open file table and save
        OPEN_FILE_TABLE.lock().set_snapshot(file_handle, snapshot);
        Ok(num_bytes)
    })
}

pub(crate) fn write_at(file_handle: &FileHandle, buf: &[u8]) -> Result<usize, FsError> {
    with_volume(|vol| {
        // Check if the file is open for writing
        let snapshot = OPEN_FILE_TABLE.lock()
            .writable_snapshot(file_handle)
            .ok_or(FsError::OpenForWriteButReadAccess)?;
        // Without holding the open file table lock, pass the small Copy structs to volume
        let (num_bytes, snapshot) = vol.write_at(snapshot, buf)?;
        // Take the open file table lock again, now that vol lock is dropped
        // Drop on the file handle will take these details from the open file
        // table and save the file metadata
        OPEN_FILE_TABLE.lock().set_snapshot(file_handle, snapshot);
        Ok(num_bytes)
    })
}

pub(crate) fn rm(dir: Dir, filename: &str) -> Result<(), FsError> {
    // Need both volume and table locks for safe deletion
    // The table lock disables IRQs, so must always be the inner lock
    with_volume(|vol| {
        // Resolve any leading path, then look up the final name.
        let (dir, filename) = vol.resolve_parent(dir, filename)?;
        // Decline if the file is currently open
        let (location, file_info) = vol.find_file_dir_entry(dir, filename)?;
        if OPEN_FILE_TABLE.lock().is_open(location) {
            return Err(FsError::FileInUse);
        }
        vol.delete_file(location, &file_info)?;
        Ok(())
    })
}

pub(crate) fn rmdir(dir: Dir, dirname: &str) -> Result<(), FsError> {
    with_volume(|vol| vol.delete_directory(dir, dirname))
}

pub(crate) fn truncate(dir: Dir, filename: &str) -> Result<(), FsError> {
    // Try to open the file for write access to ensure exclusive use
    let file = open(Access::Write, dir, filename).map_err(|e| match e {
        FsError::OpeningForWriteButAlreadyOpen => FsError::FileInUse,
        other => other,
    })?;
    let snapshot = OPEN_FILE_TABLE.lock().snapshot(&file);
    let snapshot = with_volume(|vol| vol.truncate_file(snapshot))?;

    OPEN_FILE_TABLE.lock().set_snapshot(&file, snapshot);
    file.close()
}

pub(crate) fn touch(dir: Dir, filename: &str) -> Result<(), FsError> {
    match with_volume(|vol| vol.create_empty_file(dir, filename)) {
        Ok(_) => Ok(()),
        Err(FsError::AlreadyExists) => Ok(()), // Touch does not error if the file is already in existence
        Err(fs_error) => Err(fs_error),
    }
}
