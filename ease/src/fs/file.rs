//! File operations

// Functions take the lock on the volume (and delegate as needed)

use super::volume::{EntrySlot, with_volume};

use super::{FileInfo, FsError};

#[derive(Debug, Clone, Copy)]
pub(crate) struct FileHandle {
    entry: u32,
    position: u32,
    current_cluster: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DirHandle {
    pub(crate) start_cluster: u32,
}

pub(crate) fn find_file_entry(
    dir: DirHandle,
    filename: &str,
) -> Result<(EntrySlot, FileInfo), FsError> {
    // Use the directory iterator
    with_volume(|vol| vol.find_dir_entry_location(dir, filename))
}
