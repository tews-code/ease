//! File operations

// Functions take the lock on the volume (and delegate as needed)

use crate::kernel::sync::IrqSpinLock;

use super::volume::with_volume;
use super::{Dir, FsError, Location};

const OPEN_FILES_MAX: usize = 128;

static OPEN_FILE_TABLE: OpenFileTable =
    OpenFileTable(IrqSpinLock::new([const { None }; OPEN_FILES_MAX]));

pub(crate) enum Access {
    Read,
    Write,
}

struct OpenFile {
    access: Access,
    dir_entry: Location,
}

struct OpenFileTable(IrqSpinLock<[Option<OpenFile>; OPEN_FILES_MAX]>);

fn is_open(dir_entry: Location) -> bool {
    let file_table = OPEN_FILE_TABLE.0.lock();
    file_table
        .iter()
        .flatten()
        .find(|f| f.dir_entry == dir_entry)
        .is_some()
}

fn mark_file_for_read(dir_entry: Location) -> Result<usize, FsError> {
    let mut file_table = OPEN_FILE_TABLE.0.lock();
    // Find an empty slot and insert the entry - no problem if mutiple read users
    // of the same file
    for (i, entry) in file_table.iter_mut().enumerate() {
        if entry.is_none() {
            *entry = Some(OpenFile {
                access: Access::Read,
                dir_entry,
            });
            return Ok(i);
        }
    }
    Err(FsError::TooManyOpenFiles)
}

fn mark_file_for_write(dir_entry: Location) -> Result<usize, FsError> {
    let mut file_table = OPEN_FILE_TABLE.0.lock();
    // First check if this file is already open
    if file_table
        .iter()
        .flatten()
        .any(|e| e.dir_entry == dir_entry)
    {
        return Err(FsError::OpeningForWriteButAlreadyOpen);
    }
    // It's not already open, so mark this file as open for writing
    for (i, entry) in file_table.iter_mut().enumerate() {
        if entry.is_none() {
            *entry = Some(OpenFile {
                access: Access::Write,
                dir_entry,
            });
            return Ok(i);
        }
    }
    Err(FsError::TooManyOpenFiles)
}

fn mark_file_closed(idx: usize) {
    OPEN_FILE_TABLE.0.lock()[idx].take();
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

pub(crate) fn close(file: &FileHandle) {
    mark_file_closed(file.open_file_table_idx);
}

pub(crate) fn seek(file: &mut FileHandle, position: u32) -> Result<(), FsError> {
    // Ensure file is open
    if !is_open(file.dir_entry_location) {
        return Err(FsError::FileNotOpen);
    }
    if position < file.size {
        file.position = position
    }
    Ok(())
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
