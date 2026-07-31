//! File operations

// Functions take the lock on the volume (and delegate as needed)
use crate::kernel::sync::IrqSpinLock;

use super::volume::with_volume;
use super::{Dir, FsError, Location};

const OPEN_FILES_MAX: usize = 128;

static OPEN_FILE_TABLE: IrqSpinLock<OpenFileTable> =
    IrqSpinLock::new(OpenFileTable([const { None }; OPEN_FILES_MAX]));

#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum Access {
    Read,
    Write,
}

struct OpenFile {
    access: Access,
    dir_entry: Location,
}

struct OpenFileTable([Option<OpenFile>; OPEN_FILES_MAX]);

impl OpenFileTable {
    fn get_slot_mut(&mut self) -> Option<(usize, &mut Option<OpenFile>)> {
        self.0.iter_mut().enumerate().find(|(_, f)| f.is_none())
    }

    /// Checks if a file is open
    ///
    /// Returns the index into the open file table on success
    /// or None if not found
    fn get_open(&self, dir_entry: Location) -> Option<usize> {
        self.0
            .iter()
            .position(|f| f.as_ref().is_some_and(|of| of.dir_entry == dir_entry))
    }

    fn get_access(&self, dir_entry: Location) -> Option<Access> {
        self.0
            .iter()
            .flatten()
            .find(|of| of.dir_entry == dir_entry)
            .map(|of| of.access)
    }

    fn mark_file_for_read(&mut self, dir_entry: Location) -> Result<usize, FsError> {
        // Is this file already being used for writes (we respect exclusive write access)
        if self.get_access(dir_entry) == Some(Access::Write) {
            return Err(FsError::OpeningForReadButWriteInProgress);
        }
        // Find an empty slot and insert the entry - no problem if mutiple read users
        // of the same file
        if let Some((i, new_slot)) = self.get_slot_mut() {
            *new_slot = Some(OpenFile {
                access: Access::Read,
                dir_entry,
            });
            return Ok(i);
        }
        Err(FsError::TooManyOpenFiles)
    }

    fn mark_file_for_write(&mut self, dir_entry: Location) -> Result<usize, FsError> {
        // First check if this file is already open
        if self.get_open(dir_entry).is_some() {
            return Err(FsError::OpeningForWriteButAlreadyOpen);
        }
        // It's not already open, so mark this file as open for writing
        if let Some((i, new_entry)) = self.get_slot_mut() {
            *new_entry = Some(OpenFile {
                access: Access::Write,
                dir_entry,
            });
            return Ok(i);
        }
        Err(FsError::TooManyOpenFiles)
    }

    fn mark_file_closed(&mut self, idx: usize) {
        self.0[idx].take();
    }
}

#[derive(Debug)]
pub(crate) struct FileHandle {
    open_file_table_idx: usize,
    closed: bool,
    pub(super) dir_entry_location: Location,
    pub(super) position: u32,
    pub(super) size: u32,
    pub(super) first_cluster: u32,
    pub(super) current_cluster: u32,
}

impl FileHandle {
    pub(crate) fn close(mut self) -> Result<(), FsError> {
        // Record that we have tried to close the file
        self.closed = true;
        // Check whether the file was write access
        if OPEN_FILE_TABLE.lock().0[self.open_file_table_idx]
            .as_ref()
            .is_some_and(|of| of.access == Access::Write)
        {
            match with_volume(|vol| vol.save_file_meta_data(&self)) {
                Ok(_) => Ok(()),
                Err(fs_error) => Err(fs_error),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for FileHandle {
    fn drop(&mut self) {
        if !self.closed {
            // Dropping without a close - make a best effort try to save metadata
            if OPEN_FILE_TABLE.lock().0[self.open_file_table_idx]
                .as_ref()
                .is_some_and(|of| of.access == Access::Write)
            {
                let _ = with_volume(|vol| vol.save_file_meta_data(self));
            }
        }
        OPEN_FILE_TABLE
            .lock()
            .mark_file_closed(self.open_file_table_idx);
    }
}

/// Change working directory
pub(crate) fn change_directory(wd: &mut Dir, dirname: &str) -> Result<(), FsError> {
    with_volume(|vol| vol.change_directory(wd, dirname))
}

/// Seek within file
///
/// Does not support seek beyond file end
pub(crate) fn lseek(file: &mut FileHandle, seek_bytes: u32) -> Result<(), FsError> {
    with_volume(|vol| vol.seek(file, seek_bytes))
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
        let idx = match access {
            Access::Read => OPEN_FILE_TABLE.lock().mark_file_for_read(location)?,
            Access::Write => OPEN_FILE_TABLE.lock().mark_file_for_write(location)?,
        };
        // Construct the file handle with private idx member
        Ok(FileHandle {
            open_file_table_idx: idx,
            closed: false,
            dir_entry_location: location,
            position: 0,
            size: file_info.file_size,
            first_cluster: file_info.first_cluster,
            current_cluster: file_info.first_cluster,
        })
    })
}

pub(crate) fn read_at(file: &mut FileHandle, buf: &mut [u8]) -> Result<usize, FsError> {
    with_volume(|vol| {
        if OPEN_FILE_TABLE
            .lock()
            .get_open(file.dir_entry_location)
            .is_none()
        {
            return Err(FsError::FileNotOpen);
        }
        vol.read_at(file, buf)
    })
}

pub(crate) fn write_at(file: &mut FileHandle, buf: &[u8]) -> Result<usize, FsError> {
    with_volume(|vol| {
        // Check if the file is open for writing
        if OPEN_FILE_TABLE.lock().get_access(file.dir_entry_location) != Some(Access::Write) {
            return Err(FsError::OpenForWriteButReadAccess);
        }
        vol.write_at(file, buf)
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
        if OPEN_FILE_TABLE.lock().get_open(location).is_some() {
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
    // Try to open the file for writing
    let mut file = open(Access::Write, dir, filename).map_err(|e| match e {
        FsError::OpeningForWriteButAlreadyOpen => FsError::FileInUse,
        other => other,
    })?;
    with_volume(|vol| vol.truncate_file(&mut file))?;
    file.close()
}

pub(crate) fn touch(dir: Dir, filename: &str) -> Result<(), FsError> {
    match with_volume(|vol| vol.create_empty_file(dir, filename)) {
        Ok(_) => Ok(()),
        Err(FsError::AlreadyExists) => Ok(()), // Touch does not error if the file is already in existance
        Err(fs_error) => Err(fs_error),
    }
}
