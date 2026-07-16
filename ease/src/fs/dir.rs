//! Directory

/*
* A directory is a table used to find the first cluster in a file chain.
*
* A FAT volume has a root directory and further sub-directories.
*
* Every directory entry is 32 bytes.
*
* ROOT DIRECTORY
*
* FAT16 has a continguous set of sectors acting as the root directory, which is
* found immediately after the FAT sectors. The number of supported entries can be
* found from BPB.
*
* FAT32 puts the root directory in a file, where the file's first cluster can be
* found in the BPB.
*
* SUB-DIRECTORIES
*
* Both FAT16 and FAT32 put subdirectories in files (in the data sectors). Each directory
* file's first cluster is found in it's parent directory table.
*
* In its parent's table, a subdirectory is an ordinary 32-byte entry with the directory
* bit (0x10) set at offset 11, a first_cluster like any file and file_size = 0 always.
*
* Two entries open every subdirectory:
* . (pointing to its own first cluster) and
* .. (pointing to its parent's — with 0 conventionally meaning "parent is root").
*
* DIRECTORY ENTRY
*
* The directory entry (32 bytes) layout is:
* ┌────────┬──────┬────────────────────────────────────────┐
* │ Offset │ Size │               Field                    │
* ├────────┼──────┼────────────────────────────────────────┤
* │ 0      │ 1    │ Marker - either                        |
* |        |      |     empty(0x00) or                     |
* │        │      │     deleted (0xE5)                     |
* │        │      │ The directory is packed to the start,  |
* |        |      | so once 0x00 is found all subsequent   │
* |        │      | entries will be 0x00 too.              |
* ├────────┼──────┼────────────────────────────────────────┤
* │ 0      │ 8    │ If marker is not found then this is    |
* |        |      | Filename (space-padded)                |
* ├────────┼──────┼────────────────────────────────────────┤
* │ 8      │ 3    │ Extension (space-padded)               │
* ├────────┼──────┼────────────────────────────────────────┤
* │ 11     │ 1    │ Attributes                             │
* ├────────┼──────┼────────────────────────────────────────┤
* │ 20     │ 2    │ FAT16 - zero,                          |
* |        |      | FAT32: First cluster high 16 bits      │
* ├────────┼──────┼────────────────────────────────────────┤
* │ 26     │ 2    │ FAT16: First cluster (LE  u16)         │
* |        |      | FAT32: First cluster low 16 bits       │
* ├────────┼──────┼────────────────────────────────────────┤
* │ 28     │ 4    │ File size (little-endian u32)          │
* └────────┴──────┴────────────────────────────────────────┘
*
* In FAT16, cluster numbers are only 16-bit, so the entire value fits in the
* low word at offset 26, and offset 20 is always zero — dead space.
*
* In FAT32, cluster numbers are 28-bit and no longer fit in 16 bits,
* so FAT32 presses offset 20 into service as the high half.
* You reconstruct the real value as (high_word << 16) | low_word.
*
* ATTRIBUTES
*
* ┌─────────────┬───────────────────────────────────────────┐
* │     Bit     │                  Meaning                  │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x01        │ Read-only                                 │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x02        │ Hidden                                    │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x04        │ System                                    │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x08        │ Volume label (volume ID)                  │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x10        │ Directory (subdirectory)                  │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x20        │ Archive                                   │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x40 / 0x80 │ Reserved                                  │
* ├─────────────┼───────────────────────────────────────────┤
* │ 0x0F        │ (0x01|0x02|0x04|0x08) Long-filename entry │
* └─────────────┴───────────────────────────────────────────┘
*
*/

use crate::fs::{FsError, VolumeType};
use crate::kernel::collection::StackVec;

pub(super) const DIR_ENTRY_BYTES: usize = 32; // FAT16 and FAT32 both use 32 bytes

const ENTRY_EMPTY: u8 = 0x00; // Entry is empty
pub(super) const ENTRY_DEL: u8 = 0xE5; // Entry is deleted

#[allow(dead_code)]
const ATTR_READ_ONLY: u8 = 0x01;
#[allow(dead_code)]
const ATTR_HIDDEN: u8 = 0x02;
#[allow(dead_code)]
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_LABEL: u8 = 0x08;
#[allow(dead_code)]
const ATTR_DIR: u8 = 0x10;
#[allow(dead_code)]
pub(super) const ATTR_ARCHIVE: u8 = 0x20;
#[allow(dead_code)]
const ATTR_RESERVED: [u8; 2] = [0x40, 0x80];
const ATTR_LONG_FILENAME: u8 = 0x0F;

#[derive(PartialEq, Eq)]
pub(super) enum DirEntryKind {
    Deleted,
    Empty,
    Used(FileInfo),
    Unsupported,
}

impl DirEntryKind {
    /// Parse directory entry bytes and returns a DirEntryKind
    pub(super) fn parse(
        dir_entry_bytes: [u8; DIR_ENTRY_BYTES],
        volume_type: VolumeType,
    ) -> DirEntryKind {
        // Special first-byte values
        if dir_entry_bytes[0] == ENTRY_EMPTY {
            return DirEntryKind::Empty;
        }
        if dir_entry_bytes[0] == ENTRY_DEL {
            return DirEntryKind::Deleted;
        }
        // Attribute flags to skip. A long-filename entry sets all four low
        // bits at once (0x0F); a volume label sets bit 0x08. Ordinary files
        // (including read-only/hidden/system) fall through to Used.
        if dir_entry_bytes[11] & ATTR_VOLUME_LABEL != 0
            || dir_entry_bytes[11] & ATTR_LONG_FILENAME == ATTR_LONG_FILENAME
        {
            return DirEntryKind::Unsupported;
        }

        DirEntryKind::Used(FileInfo::parse(dir_entry_bytes, volume_type))
    }

    // Convert a DirEntryKind into its 32 byte format
    #[cfg(test)]
    pub(super) fn as_bytes(&self, volume_type: VolumeType) -> [u8; DIR_ENTRY_BYTES] {
        let mut entry = [0u8; DIR_ENTRY_BYTES];
        match self {
            Self::Deleted => {
                entry[0] = ENTRY_DEL;
                entry
            }
            Self::Empty => {
                entry[0] = ENTRY_EMPTY;
                entry
            }
            Self::Unsupported => {
                entry[0] = b'U'; // Need to clear the marker, use any character
                entry[11] = ATTR_VOLUME_LABEL;
                entry
            }
            Self::Used(file_info) => file_info.as_bytes(volume_type),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileInfo {
    pub(super) name: [u8; 8],
    pub(super) extension: [u8; 3],
    pub(super) attributes: u8,
    pub(super) first_cluster: u32, // FAT16 is u16 but we store as parsed u32
    pub(crate) file_size: u32,
}

impl FileInfo {
    // Parses a raw byte array as FileInfo
    fn parse(entry: [u8; DIR_ENTRY_BYTES], volume_type: VolumeType) -> Self {
        Self {
            name: entry[0..8].try_into().unwrap(),
            extension: entry[8..11].try_into().unwrap(),
            attributes: entry[11],
            first_cluster: match volume_type {
                VolumeType::Fat16(_) => u16::from_le_bytes([entry[26], entry[27]]) as u32,
                VolumeType::Fat32(_) => {
                    u32::from_le_bytes([entry[26], entry[27], entry[20], entry[21]])
                }
            },
            file_size: u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]),
        }
    }

    // Convert FileInfo to a used directory raw bytes
    pub(super) fn as_bytes(&self, volume_type: VolumeType) -> [u8; DIR_ENTRY_BYTES] {
        let mut entry = [0u8; DIR_ENTRY_BYTES];
        entry[0..8].copy_from_slice(&self.name);
        entry[8..11].copy_from_slice(&self.extension);
        entry[11] = self.attributes;
        match volume_type {
            VolumeType::Fat16(_) => {
                entry[20..22].copy_from_slice(&[0u8, 0u8]);
                let first_cluster = self.first_cluster as u16;
                entry[26..28].copy_from_slice(&first_cluster.to_le_bytes());
            }
            VolumeType::Fat32(_) => {
                let first_cluster_hi = (self.first_cluster >> 16) as u16;
                let first_cluster_lo = self.first_cluster as u16;
                entry[20..22].copy_from_slice(&first_cluster_hi.to_le_bytes());
                entry[26..28].copy_from_slice(&first_cluster_lo.to_le_bytes());
            }
        }
        entry[28..32].copy_from_slice(&self.file_size.to_le_bytes());
        entry
    }

    /// Formats the filename as a string (e.g. "HELLO   TXT" → "HELLO.TXT")
    pub fn filename(&self) -> StackVec<u8, 12> {
        let name = str::from_utf8(&self.name)
            .expect("should be UTF-8")
            .trim_end();
        let ext = str::from_utf8(&self.extension)
            .expect("should be UTF-8")
            .trim_end();
        let mut full_name = StackVec::<u8, 12>::new();
        for &b in name.as_bytes() {
            let _ = full_name.push(b);
        }
        if !ext.is_empty() {
            let _ = full_name.push(b'.');
            for &b in ext.as_bytes() {
                let _ = full_name.push(b);
            }
        }
        full_name
    }

    // Parse string into 8.3 file name
    // Takes "TEST.TXT" and produces (b"TEST    ", b"TXT").
    // Rules:
    // - split on .
    // - uppercase
    // - pad name to 8 with spaces
    // - pad extension to 3 with spaces
    // - Reject names that are too long (>8 name or >3 extension) or contain invalid characters.
    pub fn parse_83_name(filename: &str) -> Result<([u8; 8], [u8; 3]), FsError> {
        if !filename.is_ascii() {
            return Err(FsError::InvalidName);
        }
        let (name_str, ext_str) = match filename.split_once('.') {
            Some((n, e)) => (n, e),
            None => (filename, ""),
        };
        // Validation. Before copying, check lengths:
        if !(1..=8).contains(&name_str.len()) {
            return Err(FsError::InvalidName);
        }
        if ext_str.len() > 3 || ext_str.contains('.') {
            return Err(FsError::InvalidName);
        }

        let mut name = [b' '; 8];
        let mut ext = [b' '; 3];

        for (i, &b) in name_str.as_bytes().iter().enumerate() {
            name[i] = b.to_ascii_uppercase()
        }
        for (i, &b) in ext_str.as_bytes().iter().enumerate() {
            ext[i] = b.to_ascii_uppercase()
        }
        Ok((name, ext))
    }
}

// DirEntry tests. All tests below are pure logic with no driver
// dependencies, so they run in BOTH contexts:
//   - Kernel target (`cargo test --bin ease`): each test gets the
//     custom `#[test_case]` attribute and runs in QEMU.
//   - Host (`cargo test --lib`): each test gets the standard `#[test]`
//     attribute and runs natively in milliseconds.
// `cfg_attr` selects the right attribute per target.
#[cfg(all(test, feature = "test-fs"))]
mod test {
    use super::*;

    // =========================================================================
    // DirEntry::parse tests
    // =========================================================================

    /// Build a raw 32-byte directory entry with the given 8.3 name,
    /// attributes, first cluster, and file size (FAT16 layout: the cluster
    /// goes in the low word at offset 26).
    fn make_dir_entry(
        name: &[u8; 8],
        ext: &[u8; 3],
        attrs: u8,
        cluster: u16,
        size: u32,
    ) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(name);
        e[8..11].copy_from_slice(ext);
        e[11] = attrs;
        e[26..28].copy_from_slice(&cluster.to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        e
    }

    /// Build a FileInfo (the payload of DirEntry::Used) with archive attrs.
    fn make_file_entry(
        name: &[u8; 8],
        ext: &[u8; 3],
        first_cluster: u32,
        file_size: u32,
    ) -> FileInfo {
        FileInfo {
            name: *name,
            extension: *ext,
            attributes: ATTR_ARCHIVE,
            first_cluster,
            file_size,
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_normal_file_is_used() {
        let bytes = make_dir_entry(b"HELLO   ", b"TXT", ATTR_ARCHIVE, 5, 1234);
        let DirEntryKind::Used(f) = DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))) else {
            panic!("expected Used");
        };
        assert_eq!(&f.name, b"HELLO   ");
        assert_eq!(&f.extension, b"TXT");
        assert_eq!(f.attributes, ATTR_ARCHIVE);
        assert_eq!(f.first_cluster, 5);
        assert_eq!(f.file_size, 1234);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_no_extension_is_used() {
        let bytes = make_dir_entry(b"README  ", b"   ", ATTR_ARCHIVE, 3, 100);
        let DirEntryKind::Used(f) = DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))) else {
            panic!("expected Used");
        };
        assert_eq!(&f.name, b"README  ");
        assert_eq!(&f.extension, b"   ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_empty_slot_is_empty() {
        let bytes = [0u8; DIR_ENTRY_BYTES];
        assert!(matches!(
            DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))),
            DirEntryKind::Empty
        ));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_deleted_slot_is_deleted() {
        let mut bytes = make_dir_entry(b"OLD     ", b"TXT", ATTR_ARCHIVE, 2, 50);
        bytes[0] = ENTRY_DEL;
        assert!(matches!(
            DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))),
            DirEntryKind::Deleted
        ));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_lfn_entry_is_unsupported() {
        let mut bytes = [0x42u8; DIR_ENTRY_BYTES]; // non-zero first byte
        bytes[11] = ATTR_LONG_FILENAME;
        assert!(matches!(
            DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))),
            DirEntryKind::Unsupported
        ));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_volume_label_is_unsupported() {
        let bytes = make_dir_entry(b"MOSSVOL ", b"   ", ATTR_VOLUME_LABEL, 0, 0);
        assert!(matches!(
            DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))),
            DirEntryKind::Unsupported
        ));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_volume_label_with_archive_is_unsupported() {
        // Volume-label bit set alongside the archive bit
        let bytes = make_dir_entry(b"MOSSVOL ", b"   ", ATTR_VOLUME_LABEL | ATTR_ARCHIVE, 0, 0);
        assert!(matches!(
            DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))),
            DirEntryKind::Unsupported
        ));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_read_only_file_is_used() {
        // A read-only file is still an ordinary file, not an LFN entry. This
        // pins the correct LFN rule: an entry is a long-filename entry only
        // when ALL four low attribute bits are set (attr & 0x0F == 0x0F), not
        // when any single one is (a read-only file sets only 0x01).
        let bytes = make_dir_entry(b"READONLY", b"TXT", ATTR_ARCHIVE | ATTR_READ_ONLY, 7, 10);
        assert!(matches!(
            DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1))),
            DirEntryKind::Used(_)
        ));
    }

    // =========================================================================
    // DirEntry <-> bytes roundtrip
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn roundtrip_used_entry_fat16() {
        let original = make_file_entry(b"HELLO   ", b"TXT", 5, 1234);
        let bytes = DirEntryKind::Used(original.clone()).as_bytes(VolumeType::Fat16((0, 1)));
        let DirEntryKind::Used(parsed) = DirEntryKind::parse(bytes, VolumeType::Fat16((0, 1)))
        else {
            panic!("expected Used");
        };
        assert_eq!(parsed, original);
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn roundtrip_used_entry_fat32() {
        // A first cluster with a nonzero high word exercises the split across
        // offsets 20-21 (high) and 26-27 (low).
        let original = make_file_entry(b"BIG     ", b"DAT", 0x0012_3456, 4096);
        let bytes = DirEntryKind::Used(original.clone()).as_bytes(VolumeType::Fat32(2));
        let DirEntryKind::Used(parsed) = DirEntryKind::parse(bytes, VolumeType::Fat32(2)) else {
            panic!("expected Used");
        };
        assert_eq!(parsed, original);
    }

    // =========================================================================
    // FileEntry::filename tests
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_with_extension() {
        let f = make_file_entry(b"HELLO   ", b"TXT", 5, 100);
        assert_eq!(f.filename().as_str(), Ok("HELLO.TXT"));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_no_extension() {
        let f = make_file_entry(b"README  ", b"   ", 3, 100);
        assert_eq!(f.filename().as_str(), Ok("README"));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_full_length() {
        let f = make_file_entry(b"12345678", b"ABC", 2, 50);
        assert_eq!(f.filename().as_str(), Ok("12345678.ABC"));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_short_name_short_ext() {
        let f = make_file_entry(b"A       ", b"C  ", 2, 10);
        assert_eq!(f.filename().as_str(), Ok("A.C"));
    }

    // =========================================================================
    // parse_83_name tests
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_with_extension() {
        let (name, ext) = FileInfo::parse_83_name("TEST.TXT").unwrap();
        assert_eq!(&name, b"TEST    ");
        assert_eq!(&ext, b"TXT");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_no_extension() {
        let (name, ext) = FileInfo::parse_83_name("README").unwrap();
        assert_eq!(&name, b"README  ");
        assert_eq!(&ext, b"   ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_lowercased() {
        let (name, ext) = FileInfo::parse_83_name("a.b").unwrap();
        assert_eq!(&name, b"A       ");
        assert_eq!(&ext, b"B  ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_full_length() {
        let (name, ext) = FileInfo::parse_83_name("12345678.ABC").unwrap();
        assert_eq!(&name, b"12345678");
        assert_eq!(&ext, b"ABC");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_single_char() {
        let (name, ext) = FileInfo::parse_83_name("X.Y").unwrap();
        assert_eq!(&name, b"X       ");
        assert_eq!(&ext, b"Y  ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_empty() {
        assert!(FileInfo::parse_83_name("").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_long_name() {
        assert!(FileInfo::parse_83_name("TOOLONGNAME.TXT").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_long_ext() {
        assert!(FileInfo::parse_83_name("TEST.LONG").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_multiple_dots() {
        assert!(FileInfo::parse_83_name("A.B.C").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_non_ascii() {
        assert!(FileInfo::parse_83_name("café.txt").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_dot_only_name() {
        // ".TXT" has empty name part
        assert!(FileInfo::parse_83_name(".TXT").is_err());
    }
}
