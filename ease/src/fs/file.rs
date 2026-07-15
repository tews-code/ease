//! File operations

// Functions take the lock on the volume (and delegate as needed)

use crate::fs::SECTOR_SIZE;

use super::volume::{SectorLocation, with_volume};

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
) -> Result<(SectorLocation, FileInfo), FsError> {
    let sector_buf = &mut [0u8; SECTOR_SIZE];
    // Use the directory iterator
    with_volume(|vol| vol.find_dir_entry_location(sector_buf, dir, filename))
}
