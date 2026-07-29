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
}

fn mark_file_for_read(dir_entry: Location) -> Result<usize, FsError> {
    let mut file_table = OPEN_FILE_TABLE.lock();
    // Is this file already being used for writes (we respect exclusive write access)
    if file_table.get_access(dir_entry) == Some(Access::Write) {
        return Err(FsError::OpeningForReadButWriteInProgress);
    }
    // Find an empty slot and insert the entry - no problem if mutiple read users
    // of the same file
    if let Some((i, new_slot)) = file_table.get_slot_mut() {
        *new_slot = Some(OpenFile {
            access: Access::Read,
            dir_entry,
        });
        return Ok(i);
    }
    Err(FsError::TooManyOpenFiles)
}

fn mark_file_for_write(dir_entry: Location) -> Result<usize, FsError> {
    let mut file_table = OPEN_FILE_TABLE.lock();
    // First check if this file is already open
    if file_table.get_open(dir_entry).is_some() {
        return Err(FsError::OpeningForWriteButAlreadyOpen);
    }
    // It's not already open, so mark this file as open for writing
    if let Some((i, new_entry)) = file_table.get_slot_mut() {
        *new_entry = Some(OpenFile {
            access: Access::Write,
            dir_entry,
        });
        return Ok(i);
    }
    Err(FsError::TooManyOpenFiles)
}

fn mark_file_closed(idx: usize) {
    OPEN_FILE_TABLE.lock().0[idx].take();
}

#[derive(Debug)]
pub(crate) struct FileHandle {
    open_file_table_idx: usize,
    pub(super) dir_entry_location: Location,
    pub(super) position: u32,
    pub(super) size: u32,
    pub(super) first_cluster: u32,
    pub(super) current_cluster: u32,
}

pub(crate) fn open(access: Access, dir: Dir, filename: &str) -> Result<FileHandle, FsError> {
    let (location, file_info) = with_volume(|vol| vol.find_file_dir_entry(dir, filename))?;
    // Check if this is fine
    let idx = match access {
        Access::Read => mark_file_for_read(location)?,
        Access::Write => mark_file_for_write(location)?,
    };
    Ok(FileHandle {
        open_file_table_idx: idx,
        dir_entry_location: location,
        position: 0,
        size: file_info.file_size,
        first_cluster: file_info.first_cluster,
        current_cluster: file_info.first_cluster,
    })
}

pub(crate) fn close(file: &FileHandle) -> Result<(), FsError> {
    // Check whether the file was write access
    let file_access = OPEN_FILE_TABLE
        .lock()
        .get_access(file.dir_entry_location)
        .expect("should not be closing a file which is missing from the open file table");
    let result = if file_access == Access::Write {
        with_volume(|vol| vol.save_file_meta_data(file))
    } else {
        Ok(())
    };
    mark_file_closed(file.open_file_table_idx);
    result
}

pub(crate) fn read_at(file: &mut FileHandle, buf: &mut [u8]) -> Result<usize, FsError> {
    with_volume(|vol| vol.read_at(file, buf))
}

pub(crate) fn rm(dir: Dir, filename: &str) -> Result<(), FsError> {
    with_volume(|vol| vol.delete_file(dir, filename))
}

pub(crate) fn touch(dir: Dir, filename: &str) -> Result<(), FsError> {
    match with_volume(|vol| vol.create_empty_file(dir, filename)) {
        Ok(_) => Ok(()),
        Err(FsError::AlreadyExists) => Ok(()), // Touch does not error if the file is already in existance
        Err(fs_error) => Err(fs_error),
    }
}
