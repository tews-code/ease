//! FAT Root Directory

use crate::fs::FsError;
use crate::kernel::collection::StackVec;

pub(super) const DIR_ENTRY_BYTES: usize = 32;

pub(super) enum DirParseResult {
    Parsed(DirEntry),
    Skip,
    End,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct DirEntry {
    filename: [u8; 8],
    extension: [u8; 3],
    pub(super) attributes: u8,
    pub(super) first_cluster: u16, // Little Endian
    pub file_size: u32,            // Little Endian
}

impl DirEntry {
    /// Parse bytes and returns a DirEntry
    ///
    /// - None for empty/deleted/skipped
    pub(super) fn parse(bytes: &[u8; 32]) -> DirParseResult {
        // Special first-byte values:
        // - 0x00 — entry is empty and no more entries follow (stop scanning)
        // - 0xE5 — entry is deleted (skip it)
        if bytes[0] == 0x00 {
            return DirParseResult::End;
        }
        if bytes[0] == 0xE5 {
            return DirParseResult::Skip;
        }

        // Attribute flags to skip:
        // - 0x0F — long filename entry (skip)
        // - 0x08 — volume label (skip)
        if bytes[11] == 0x0F || bytes[11] & 0x08 != 0 {
            return DirParseResult::Skip;
        }

        DirParseResult::Parsed(DirEntry {
            filename: bytes[0..8].try_into().unwrap(),
            extension: bytes[8..11].try_into().unwrap(),
            attributes: bytes[11],
            first_cluster: u16::from_le_bytes([bytes[26], bytes[27]]),
            file_size: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
        })
    }

    /// Formats the filename as a string (e.g. "HELLO   TXT" → "HELLO.TXT")
    pub fn filename(&self) -> StackVec<u8, 12> {
        let name = str::from_utf8(&self.filename)
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

    /// Parse string into 8.3 file name
    #[allow(dead_code)]
    // take "TEST.TXT" and produce (b"TEST    ", b"TXT"). Rules: split
    // on ., uppercase, pad name to 8 with spaces, pad extension to 3 with spaces. Reject names that are too long
    // (>8 name or >3 extension) or contain invalid characters.
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

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_normal_file() {
        let bytes = make_dir_entry(b"HELLO   ", b"TXT", 0x20, 5, 1234);
        match DirEntry::parse(&bytes) {
            DirParseResult::Parsed(entry) => {
                assert_eq!(&entry.filename, b"HELLO   ");
                assert_eq!(&entry.extension, b"TXT");
                assert_eq!(entry.attributes, 0x20);
                assert_eq!(entry.first_cluster, 5);
                assert_eq!(entry.file_size, 1234);
            }
            _ => panic!("expected Parsed"),
        }
    }

    /// Build a 32-byte directory entry with the given 8.3 name, attributes,
    /// first cluster, and file size.
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

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_no_extension() {
        let bytes = make_dir_entry(b"README  ", b"   ", 0x20, 3, 100);
        match DirEntry::parse(&bytes) {
            DirParseResult::Parsed(entry) => {
                assert_eq!(&entry.filename, b"README  ");
                assert_eq!(&entry.extension, b"   ");
            }
            _ => panic!("expected Parsed"),
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_empty_entry_returns_end() {
        let bytes = [0u8; 32];
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::End));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_deleted_entry_returns_skip() {
        let mut bytes = make_dir_entry(b"OLD     ", b"TXT", 0x20, 2, 50);
        bytes[0] = 0xE5; // Mark as deleted
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_lfn_entry_returns_skip() {
        let mut bytes = [0x42u8; 32]; // Non-zero first byte
        bytes[11] = 0x0F; // LFN attribute
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_volume_label_returns_skip() {
        let bytes = make_dir_entry(b"MOSSVOL ", b"   ", 0x08, 0, 0);
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn dir_entry_parse_volume_label_with_other_attrs_returns_skip() {
        // Volume label bit set alongside archive bit
        let bytes = make_dir_entry(b"MOSSVOL ", b"   ", 0x28, 0, 0);
        assert!(matches!(DirEntry::parse(&bytes), DirParseResult::Skip));
    }

    // =========================================================================
    // DirEntry::filename tests
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_with_extension() {
        let bytes = make_dir_entry(b"HELLO   ", b"TXT", 0x20, 5, 100);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("HELLO.TXT"));
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_no_extension() {
        let bytes = make_dir_entry(b"README  ", b"   ", 0x20, 3, 100);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("README"));
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_full_length() {
        let bytes = make_dir_entry(b"12345678", b"ABC", 0x20, 2, 50);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("12345678.ABC"));
        }
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn filename_short_name_short_ext() {
        let bytes = make_dir_entry(b"A       ", b"C  ", 0x20, 2, 10);
        if let DirParseResult::Parsed(entry) = DirEntry::parse(&bytes) {
            assert_eq!(entry.filename().as_str(), Ok("A.C"));
        }
    }

    // =========================================================================
    // parse_83_name tests
    // =========================================================================

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_with_extension() {
        let (name, ext) = DirEntry::parse_83_name("TEST.TXT").unwrap();
        assert_eq!(&name, b"TEST    ");
        assert_eq!(&ext, b"TXT");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_no_extension() {
        let (name, ext) = DirEntry::parse_83_name("README").unwrap();
        assert_eq!(&name, b"README  ");
        assert_eq!(&ext, b"   ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_lowercased() {
        let (name, ext) = DirEntry::parse_83_name("a.b").unwrap();
        assert_eq!(&name, b"A       ");
        assert_eq!(&ext, b"B  ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_full_length() {
        let (name, ext) = DirEntry::parse_83_name("12345678.ABC").unwrap();
        assert_eq!(&name, b"12345678");
        assert_eq!(&ext, b"ABC");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_single_char() {
        let (name, ext) = DirEntry::parse_83_name("X.Y").unwrap();
        assert_eq!(&name, b"X       ");
        assert_eq!(&ext, b"Y  ");
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_empty() {
        assert!(DirEntry::parse_83_name("").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_long_name() {
        assert!(DirEntry::parse_83_name("TOOLONGNAME.TXT").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_long_ext() {
        assert!(DirEntry::parse_83_name("TEST.LONG").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_multiple_dots() {
        assert!(DirEntry::parse_83_name("A.B.C").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_rejects_non_ascii() {
        assert!(DirEntry::parse_83_name("café.txt").is_err());
    }

    #[cfg_attr(target_os = "none", test_case)]
    #[cfg_attr(not(target_os = "none"), test)]
    fn parse_83_name_dot_only_name() {
        // ".TXT" has empty name part
        assert!(DirEntry::parse_83_name(".TXT").is_err());
    }
}
