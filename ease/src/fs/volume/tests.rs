//! Volume tests
//!
//! Volume tests are kernel-only because they exercise the real virtio
//! block device. The lib crate stubs out `read_block`/`write_block` so
//! volume.rs's production code compiles, but its tests can't run on
//! host without a disk mock — that's a follow-up refactor (the
//! "BlockDevice trait" approach). For now this gate keeps lib-test
//! builds clean.

use super::*;
use crate::fs::file::{self, Access};
use alloc::vec::Vec;

const ROOT: Dir = Dir::Root;

/// Directory-entry lookup — the backend of the (now file-module) `open`.
/// Use for tests that only inspect the entry (size, existence), so they can
/// stay inside a single `with_volume` closure.
fn find_size(name: &str) -> Result<u32, FsError> {
    with_volume(|vol| vol.find_file_dir_entry(ROOT, name)).map(|(_, fi)| fi.file_size)
}

/// Open `name`, read the whole file through the streaming `read_at` cursor,
/// then close. The 100-byte buffer is not a divisor of the 512 sector, so
/// reads land mid-sector and exercise the reassembly path.
///
/// `file::open` takes the volume lock internally, so it must run OUTSIDE
/// `with_volume`; the returned `FileHandle` is what `read_at` operates on.
fn read_whole(name: &str) -> Vec<u8> {
    // Pre-size to the file's length so `extend_from_slice` never reallocates:
    // a growing Vec would hold the old buffer while allocating the larger one,
    // doubling transient heap use and OOMing on a large file like BIG.TXT.
    let size = find_size(name).unwrap_or(0) as usize;
    let mut handle = file::open(Access::Read, ROOT, name).unwrap();
    let mut out = Vec::with_capacity(size);
    let mut buf = [0u8; 100];
    loop {
        let n = file::read_at(&mut handle, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    handle
        .close()
        .expect("should not error closing Access::Read file");
    out
}

/// Write `data` to `name` through the streaming `write_at` path, mirroring
/// the shell `write` command: create if missing, truncate to replace, then
/// stream the buffer in deliberately small non-aligned chunks so the
/// multi-call path — sector advance and cluster-boundary follow/allocate,
/// including a call that ends exactly on a boundary — is exercised. `close`
/// commits the size/first_cluster metadata.
fn write_streamed(name: &str, data: &[u8]) {
    file::touch(ROOT, name).unwrap();
    file::truncate(ROOT, name).unwrap();
    let mut handle = file::open(Access::Write, ROOT, name).unwrap();
    for chunk in data.chunks(7) {
        file::write_at(&mut handle, chunk).unwrap();
    }
    handle.close().expect("close should commit write metadata");
}

// =========================================================================
// open (dir-entry lookup) tests
// =========================================================================

#[test_case]
fn open_finds_hello_txt() {
    assert!(find_size("HELLO.TXT").unwrap() > 0);
}

#[test_case]
fn open_case_insensitive() {
    assert!(find_size("hello.txt").unwrap() > 0);
}

#[test_case]
fn open_not_found() {
    assert!(matches!(find_size("NOPE.TXT"), Err(FsError::NotFound)));
}

#[test_case]
fn open_empty_file() {
    assert_eq!(find_size("EMPTY.TXT").unwrap(), 0);
}

#[test_case]
fn open_no_extension() {
    assert!(find_size("SHORT").unwrap() > 0);
}

// =========================================================================
// Volume::read_file tests
// =========================================================================

#[test_case]
fn read_file_hello_txt() {
    let content = read_whole("HELLO.TXT");
    let text = core::str::from_utf8(&content).unwrap();
    assert_eq!(text, "Text file contents\n");
}

#[test_case]
fn read_file_empty() {
    assert_eq!(read_whole("EMPTY.TXT").len(), 0);
}

#[test_case]
fn read_file_short() {
    let content = read_whole("SHORT");
    let text = core::str::from_utf8(&content).unwrap();
    assert_eq!(text, "This is a file with a short name.\n");
}

#[test_case]
fn read_file_size_matches_dir_entry() {
    let content = read_whole("HELLO.TXT");
    assert_eq!(content.len(), find_size("HELLO.TXT").unwrap() as usize);
}

#[test_case]
fn read_file_longname() {
    let content = read_whole("LONGNAME.END");
    let text = core::str::from_utf8(&content).unwrap();
    assert_eq!(text, "This is a file with a long name.\n");
}

#[test_case]
fn read_file_64kb() {
    // 64KB file spans many clusters — tests cluster chain following at scale
    assert_eq!(find_size("BIG.TXT").unwrap(), 64 * 1024);
    let content = read_whole("BIG.TXT");
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
}

#[test_case]
fn read_at_reassembles_across_sector_and_cluster_boundaries() {
    // BIG.TXT is 64 KB: 128 sectors spread across many 2048-byte clusters.
    // Reading it in 100-byte chunks (deliberately not a divisor of the 512
    // sector or the 2048 cluster) forces `read_at` to hand back partial
    // buffers whose edges land mid-sector and mid-cluster.
    //
    // NOTE: with `read_file` removed, there is no longer an independent read
    // oracle, so `expected` is read via `read_at` too — the byte comparison
    // below is self-referential. What still bites here is the final
    // `offset == len` assertion: it catches `read_at` stalling or stopping
    // short while following the FAT chain across this large multi-cluster
    // file (a scale the write_at round-trip tests don't reach). Independent
    // byte-correctness of read_at now lives in those round-trip tests.
    let filename = "BIG.TXT";
    let expected = read_whole(filename);
    assert_eq!(expected.len(), 64 * 1024);

    let mut handle = file::open(Access::Read, ROOT, filename).unwrap();
    let mut buf = [0u8; 100];
    let mut offset = 0usize;
    loop {
        let n = file::read_at(&mut handle, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        assert!(
            buf[..n] == expected[offset..offset + n],
            "read_at diverges from read_file starting at byte {}",
            offset
        );
        offset += n;
    }
    handle
        .close()
        .expect("should not error closing Access::Read file");

    assert_eq!(
        offset,
        expected.len(),
        "read_at stopped short at byte {}",
        offset
    );
}

#[test_case]
fn open_multi_cluster_file() {
    // The PDF is ~7.9MB — too large to read into heap, but verify the dir
    // entry is found and has the expected size.
    assert_eq!(find_size("RP-008~1.PDF").unwrap(), 7_968_417);
}

#[test_case]
fn disk_image_fat_entry_hello_txt() {
    let current_dir = Dir::Root;
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
    let root_dir = Dir::Root;
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
    let root_dir = Dir::Root;
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
#[cfg(feature = "fat16")]
fn disk_image_fat_entry_reserved() {
    // FAT16 entries 0 and 1 are reserved slots, not chain entries: FAT[0]
    // holds the media descriptor and FAT[1] an EOC/flags marker. Their bit
    // pattern (>= 0xFFF8) is exactly what the value-based FatEntry classifier
    // reads as end-of-chain, so classifying them is meaningless (chains only
    // ever start at cluster 2). Check the raw reserved values directly.
    with_volume(|vol| {
        let buf = vol.read_sector(vol.bpb.fat_start_sector()).unwrap();
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

#[test_case]
#[cfg(feature = "fat32")]
fn disk_image_fat_entry_reserved() {
    // FAT32 entries are 4 bytes. Entries 0 and 1 are reserved: FAT[0] holds the
    // media descriptor in its low byte (0xF8) with the rest of the 28-bit value
    // set; FAT[1] is the end-of-chain / shutdown-flags marker. The top nibble of
    // every FAT32 entry is reserved, so compare on the 28-bit value.
    with_volume(|vol| {
        let buf = vol.read_sector(vol.bpb.fat_start_sector()).unwrap();
        let fat0 = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) & 0x0FFF_FFFF;
        let fat1 = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) & 0x0FFF_FFFF;
        assert_eq!(
            buf[0], 0xF8,
            "FAT[0] low byte should be the media descriptor"
        );
        assert!(
            fat0 >= 0x0FFF_FFF8,
            "FAT[0] should be a reserved marker, got {fat0:#010x}"
        );
        assert!(
            fat1 >= 0x0FFF_FFF8,
            "FAT[1] should be EOC/reserved, got {fat1:#010x}"
        );
    });
}

// =========================================================================
// Volume::create_empty_file tests
// =========================================================================

#[test_case]
fn touch_creates_file() {
    with_volume(|vol| vol.create_empty_file(ROOT, "NEW.TXT")).unwrap();
    assert_eq!(find_size("NEW.TXT").unwrap(), 0);
}

#[test_case]
fn touch_case_insensitive_open() {
    with_volume(|vol| vol.create_empty_file(ROOT, "LOWER.TXT")).unwrap();
    assert_eq!(find_size("lower.txt").unwrap(), 0);
}

#[test_case]
fn touch_no_extension() {
    with_volume(|vol| vol.create_empty_file(ROOT, "NOEXT")).unwrap();
    assert_eq!(find_size("NOEXT").unwrap(), 0);
}

#[test_case]
fn touch_invalid_name_rejected() {
    with_volume(|vol| {
        let result = vol.create_empty_file(Dir::Root, "TOOLONGNAME.TXT");
        assert!(matches!(result, Err(FsError::InvalidName)));
    });
}

#[test_case]
fn touch_created_file_visible_in_ls() {
    let current_dir = Dir::Root;
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
// Volume::make_dir tests
// =========================================================================

#[test_case]
fn mkdir_creates_directory() {
    with_volume(|vol| {
        vol.make_dir(ROOT, "MKDIR1").unwrap();

        // The parent lists it, marked as a directory with an allocated cluster.
        let (_, fi) = vol.find_file_dir_entry(ROOT, "MKDIR1").unwrap();
        assert!(
            fi.attributes & dir::ATTR_DIR != 0,
            "entry should be flagged as a directory"
        );
        let cluster = fi.first_cluster;
        assert!(cluster != 0, "directory should own an allocated cluster");

        // The new directory contains exactly `.` and `..`, and nothing else
        // (read_dir stops at the first empty entry after them).
        let mut names: Vec<[u8; 8]> = Vec::new();
        let mut clusters: Vec<u32> = Vec::new();
        let _ = vol.read_dir(Dir::SubDir(cluster), |entry| {
            names.push(entry.name);
            clusters.push(entry.first_cluster);
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(names.len(), 2, "new dir should contain only . and ..");
        assert_eq!(&names[0], b".       ", "first entry is .");
        assert_eq!(&names[1], b"..      ", "second entry is ..");
        assert_eq!(clusters[0], cluster, ". points to the dir itself");
        assert_eq!(clusters[1], 0, ".. points to the root (cluster 0)");
    });
}

#[test_case]
fn mkdir_duplicate_rejected() {
    with_volume(|vol| {
        vol.make_dir(ROOT, "MKDIR2").unwrap();
        assert!(matches!(
            vol.make_dir(ROOT, "MKDIR2"),
            Err(FsError::DuplicateDirName)
        ));
    });
}

#[test_case]
fn mkdir_name_with_extension_rejected() {
    with_volume(|vol| {
        assert!(matches!(
            vol.make_dir(ROOT, "MKDIR3.TXT"),
            Err(FsError::DirNameHasExtension)
        ));
    });
}

// =========================================================================
// Volume::seek (file::lseek) tests
// =========================================================================
//
// BIG.TXT is 64 KB across many 2048-byte clusters, so seeking exercises the
// FAT-chain walk. Each test seeks, reads, and checks the bytes against the
// ground-truth full read (read_whole), so it catches a wrong current_cluster.

#[test_case]
fn seek_within_first_cluster() {
    let whole = read_whole("BIG.TXT");
    let offset: usize = 100; // still in cluster index 0
    let mut handle = file::open(Access::Read, ROOT, "BIG.TXT").unwrap();
    file::lseek(&mut handle, offset as u32).unwrap();
    let mut buf = [0u8; 32];
    let n = file::read_at(&mut handle, &mut buf).unwrap();
    handle.close().unwrap();
    assert_eq!(&buf[..n], &whole[offset..offset + n]);
}

#[test_case]
fn seek_to_cluster_boundary() {
    let whole = read_whole("BIG.TXT");
    let offset: usize = 2048; // exact start of cluster index 1
    let mut handle = file::open(Access::Read, ROOT, "BIG.TXT").unwrap();
    file::lseek(&mut handle, offset as u32).unwrap();
    let mut buf = [0u8; 32];
    let n = file::read_at(&mut handle, &mut buf).unwrap();
    handle.close().unwrap();
    assert_eq!(&buf[..n], &whole[offset..offset + n]);
}

#[test_case]
fn seek_across_several_clusters() {
    let whole = read_whole("BIG.TXT");
    let offset: usize = 5000; // cluster index 2, mid-sector
    let mut handle = file::open(Access::Read, ROOT, "BIG.TXT").unwrap();
    file::lseek(&mut handle, offset as u32).unwrap();
    let mut buf = [0u8; 64];
    let n = file::read_at(&mut handle, &mut buf).unwrap();
    handle.close().unwrap();
    assert_eq!(&buf[..n], &whole[offset..offset + n]);
}

#[test_case]
fn seek_rewind_to_zero() {
    let whole = read_whole("BIG.TXT");
    let mut handle = file::open(Access::Read, ROOT, "BIG.TXT").unwrap();
    // Advance into a later cluster, then rewind to the start.
    file::lseek(&mut handle, 6000).unwrap();
    file::lseek(&mut handle, 0).unwrap();
    let mut buf = [0u8; 32];
    let n = file::read_at(&mut handle, &mut buf).unwrap();
    handle.close().unwrap();
    assert_eq!(&buf[..n], &whole[..n]);
}

#[test_case]
fn seek_past_end_rejected() {
    let size = find_size("BIG.TXT").unwrap();
    let mut handle = file::open(Access::Read, ROOT, "BIG.TXT").unwrap();
    let result = file::lseek(&mut handle, size + 1);
    handle.close().unwrap();
    assert!(matches!(result, Err(FsError::SeekPastFileEnd)));
}

#[test_case]
fn seek_to_exact_eof() {
    // BIG.TXT's size is an exact multiple of the cluster size, so seeking to
    // exactly the size is the EOF-overshoot case: it must succeed (not walk
    // one cluster too far), and a read there returns 0.
    let size = find_size("BIG.TXT").unwrap();
    let mut handle = file::open(Access::Read, ROOT, "BIG.TXT").unwrap();
    file::lseek(&mut handle, size).unwrap();
    let mut buf = [0u8; 16];
    let n = file::read_at(&mut handle, &mut buf).unwrap();
    handle.close().unwrap();
    assert_eq!(n, 0);
}

// =========================================================================
// Volume::change_directory tests
// =========================================================================

#[test_case]
fn cd_into_subdir_then_back_to_root() {
    with_volume(|vol| {
        vol.make_dir(ROOT, "CDBACK").unwrap();
        let (_, fi) = vol.find_file_dir_entry(ROOT, "CDBACK").unwrap();
        let sub = Dir::SubDir(fi.first_cluster);

        let mut wd = Dir::Root;
        vol.change_directory(&mut wd, "CDBACK").unwrap();
        assert_eq!(wd, sub);

        // `..` reads the on-disk entry (first_cluster 0) and climbs to root.
        vol.change_directory(&mut wd, "..").unwrap();
        assert_eq!(wd, Dir::Root);
    });
}

#[test_case]
fn cd_dotdot_climbs_to_parent_subdir_not_root() {
    with_volume(|vol| {
        // Root / CDOUTER / CDINNER
        vol.make_dir(ROOT, "CDOUTER").unwrap();
        let (_, outer_fi) = vol.find_file_dir_entry(ROOT, "CDOUTER").unwrap();
        let outer = Dir::SubDir(outer_fi.first_cluster);
        vol.make_dir(outer, "CDINNER").unwrap();
        let (_, inner_fi) = vol.find_file_dir_entry(outer, "CDINNER").unwrap();
        let inner = Dir::SubDir(inner_fi.first_cluster);

        let mut wd = Dir::Root;
        vol.change_directory(&mut wd, "CDOUTER").unwrap();
        vol.change_directory(&mut wd, "CDINNER").unwrap();
        assert_eq!(wd, inner);
        // `..` from the inner dir returns to the outer subdir, not root.
        vol.change_directory(&mut wd, "..").unwrap();
        assert_eq!(wd, outer);
    });
}

#[test_case]
fn cd_dot_is_a_noop() {
    with_volume(|vol| {
        let mut wd = Dir::Root;
        vol.change_directory(&mut wd, ".").unwrap();
        assert_eq!(wd, Dir::Root);
    });
}

#[test_case]
fn cd_dotdot_from_root_stays_root() {
    with_volume(|vol| {
        let mut wd = Dir::Root;
        vol.change_directory(&mut wd, "..").unwrap();
        assert_eq!(wd, Dir::Root);
    });
}

#[test_case]
fn cd_into_file_rejected() {
    with_volume(|vol| {
        vol.create_empty_file(ROOT, "CDFILE.TXT").unwrap();
        let mut wd = Dir::Root;
        assert!(matches!(
            vol.change_directory(&mut wd, "CDFILE.TXT"),
            Err(FsError::NotADirectory)
        ));
        assert_eq!(wd, Dir::Root, "wd must be unchanged on error");
    });
}

#[test_case]
fn cd_nonexistent_rejected() {
    with_volume(|vol| {
        let mut wd = Dir::Root;
        assert!(matches!(
            vol.change_directory(&mut wd, "NOSUCHDIR"),
            Err(FsError::NotFound)
        ));
        assert_eq!(wd, Dir::Root);
    });
}

// =========================================================================
// Volume::delete_directory tests
// =========================================================================

#[test_case]
fn rmdir_removes_empty_dir() {
    with_volume(|vol| {
        vol.make_dir(ROOT, "RMD1").unwrap();
        assert!(vol.find_file_dir_entry(ROOT, "RMD1").is_ok());
        vol.delete_directory(ROOT, "RMD1").unwrap();
        assert!(matches!(
            vol.find_file_dir_entry(ROOT, "RMD1"),
            Err(FsError::NotFound)
        ));
    });
}

#[test_case]
fn rmdir_then_recreate_reuses_cleanly() {
    // A clean removal frees the entry and cluster, so the same name can be
    // created again — which would fail (DuplicateDirName) if the entry lingered.
    with_volume(|vol| {
        vol.make_dir(ROOT, "RMD2").unwrap();
        vol.delete_directory(ROOT, "RMD2").unwrap();
        vol.make_dir(ROOT, "RMD2").unwrap();
        assert!(vol.find_file_dir_entry(ROOT, "RMD2").is_ok());
    });
}

#[test_case]
fn rmdir_non_empty_rejected() {
    with_volume(|vol| {
        vol.make_dir(ROOT, "RMD3").unwrap();
        let (_, fi) = vol.find_file_dir_entry(ROOT, "RMD3").unwrap();
        vol.create_empty_file(Dir::SubDir(fi.first_cluster), "CHILD.TXT")
            .unwrap();
        assert!(matches!(
            vol.delete_directory(ROOT, "RMD3"),
            Err(FsError::DirectoryNotEmpty)
        ));
        // A rejected removal leaves the directory in place.
        assert!(vol.find_file_dir_entry(ROOT, "RMD3").is_ok());
    });
}

#[test_case]
fn rmdir_dot_and_dotdot_rejected() {
    with_volume(|vol| {
        assert!(matches!(
            vol.delete_directory(ROOT, "."),
            Err(FsError::DirectoryIsCurrent)
        ));
        assert!(matches!(
            vol.delete_directory(ROOT, ".."),
            Err(FsError::DirectoryIsCurrent)
        ));
    });
}

#[test_case]
fn rmdir_nonexistent_rejected() {
    with_volume(|vol| {
        assert!(matches!(
            vol.delete_directory(ROOT, "NOSUCHRMD"),
            Err(FsError::NotFound)
        ));
    });
}

#[test_case]
fn rmdir_regular_file_rejected() {
    with_volume(|vol| {
        vol.create_empty_file(ROOT, "RMDFILE.TXT").unwrap();
        assert!(matches!(
            vol.delete_directory(ROOT, "RMDFILE.TXT"),
            Err(FsError::NotADirectory)
        ));
    });
}

// =========================================================================
// Path resolution tests (resolve_parent / walk_dir)
// =========================================================================

// These clean up their root-level fixtures (create + remove) so the shared
// disk image is left unchanged for later layout-sensitive tests.

#[test_case]
fn path_create_resolve_and_remove() {
    with_volume(|vol| {
        vol.make_dir(ROOT, "PATHT").unwrap();
        let (_, p) = vol.find_file_dir_entry(ROOT, "PATHT").unwrap();
        let pdir = Dir::SubDir(p.first_cluster);

        // mkdir + touch through a path relative to ROOT.
        vol.make_dir(ROOT, "PATHT/SUB").unwrap();
        vol.create_empty_file(ROOT, "PATHT/F.TXT").unwrap();
        assert!(vol.find_file_dir_entry(pdir, "SUB").is_ok());
        assert!(vol.find_file_dir_entry(pdir, "F.TXT").is_ok());

        // cd through a multi-component path in one call.
        let (_, s) = vol.find_file_dir_entry(pdir, "SUB").unwrap();
        let mut wd = Dir::Root;
        vol.change_directory(&mut wd, "PATHT/SUB").unwrap();
        assert_eq!(wd, Dir::SubDir(s.first_cluster));

        // rmdir through a path.
        vol.delete_directory(ROOT, "PATHT/SUB").unwrap();
        assert!(matches!(
            vol.find_file_dir_entry(pdir, "SUB"),
            Err(FsError::NotFound)
        ));

        // Clean up so root is left as we found it.
        let (loc, info) = vol.find_file_dir_entry(pdir, "F.TXT").unwrap();
        vol.delete_file(loc, &info).unwrap();
        vol.delete_directory(ROOT, "PATHT").unwrap();
    });
}

#[test_case]
fn rmdir_guard_refuses_the_working_dir_via_path() {
    // Standing in PGUARD, a path that resolves back to the wd (../PGUARD) is
    // refused even though the final name isn't "." — the guard is on identity.
    with_volume(|vol| {
        vol.make_dir(ROOT, "PGUARD").unwrap();
        let (_, g) = vol.find_file_dir_entry(ROOT, "PGUARD").unwrap();
        let wd = Dir::SubDir(g.first_cluster);
        assert!(matches!(
            vol.delete_directory(wd, "../PGUARD"),
            Err(FsError::DirectoryIsCurrent)
        ));
        assert!(vol.find_file_dir_entry(ROOT, "PGUARD").is_ok());
        // Clean up.
        vol.delete_directory(ROOT, "PGUARD").unwrap();
    });
}

// =========================================================================
// Volume::delete_file tests
// =========================================================================

#[test_case]
fn delete_empty_file() {
    with_volume(|vol| vol.create_empty_file(ROOT, "DEL1.TXT")).unwrap();
    assert!(find_size("DEL1.TXT").is_ok());

    with_volume(|vol| {
        let (slot, info) = vol.find_file_dir_entry(ROOT, "DEL1.TXT").unwrap();
        vol.delete_file(slot, &info).unwrap();
    });
    assert!(matches!(find_size("DEL1.TXT"), Err(FsError::NotFound)));
}

#[test_case]
fn delete_file_with_content() {
    let current_dir = Dir::Root;
    // DELETE.ME exists solely for this test — no other test depends on it
    with_volume(|vol| {
        let (_, fi) = vol.find_file_dir_entry(current_dir, "DELETE.ME").unwrap();
        let first_cluster = fi.first_cluster;
        assert!(first_cluster >= 2);

        let (slot, info) = vol.find_file_dir_entry(ROOT, "DELETE.ME").unwrap();
        vol.delete_file(slot, &info).unwrap();

        // File should no longer be found
        assert!(matches!(
            vol.find_file_dir_entry(current_dir, "DELETE.ME"),
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
    with_volume(|vol| {
        vol.create_empty_file(ROOT, "REUSE.TXT").unwrap();
        let (slot, info) = vol.find_file_dir_entry(ROOT, "REUSE.TXT").unwrap();
        vol.delete_file(slot, &info).unwrap();
        // Slot marked 0xE5 should be reusable
        vol.create_empty_file(ROOT, "REUSE.TXT").unwrap();
    });
    assert_eq!(find_size("REUSE.TXT").unwrap(), 0);
}

// =========================================================================
// set_fat_entry tests
// =========================================================================

#[test_case]
fn set_fat_entry_roundtrip() {
    with_volume(|vol| {
        // Grab a genuinely-free cluster wherever it lives. A fixed low window
        // (2..100) works on the FAT16 image but not FAT32: with 512-byte
        // clusters the big test files fill clusters 2.. contiguously, so the
        // first free cluster sits well past 15000. allocate_cluster walks the
        // FAT for the first free one; the roundtrip below restores it to Free.
        let test_cluster = vol.allocate_cluster().unwrap();
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
// Streaming write_at / read_at tests
//
// write_streamed pushes data through write_at in 7-byte chunks (create +
// truncate + stream + close); read_whole pulls it back through the read_at
// cursor. The round-trip is self-checking — the source buffer is the oracle,
// so a divergence pins a defect in write_at or read_at without depending on
// any other read path.
// =========================================================================

#[test_case]
fn write_at_and_read_back() {
    write_streamed("WSTREAM1.TXT", b"hello streamed world\n");
    assert_eq!(find_size("WSTREAM1.TXT").unwrap(), 21);
    assert_eq!(&read_whole("WSTREAM1.TXT"), b"hello streamed world\n");
}

#[test_case]
fn write_at_empty_data() {
    // No chunks means no write_at call; close still commits size 0.
    write_streamed("WSTREAM2.TXT", b"");
    assert_eq!(find_size("WSTREAM2.TXT").unwrap(), 0);
    assert!(read_whole("WSTREAM2.TXT").is_empty());
}

#[test_case]
fn write_at_overwrite_replaces() {
    write_streamed("WSTREAM3.TXT", b"the first, longer contents");
    write_streamed("WSTREAM3.TXT", b"second");
    // truncate resets the file, so size is the new (shorter) length, not max.
    assert_eq!(find_size("WSTREAM3.TXT").unwrap(), 6);
    assert_eq!(&read_whole("WSTREAM3.TXT"), b"second");
}

#[test_case]
fn write_at_multi_sector() {
    let data = [b'A'; 1024];
    write_streamed("WSTREAM4.TXT", &data);
    assert_eq!(find_size("WSTREAM4.TXT").unwrap(), 1024);
    assert_eq!(read_whole("WSTREAM4.TXT"), data);
}

#[test_case]
fn write_at_crosses_cluster_boundary_across_calls() {
    // 5000 bytes spans more than two 2048-byte clusters. A prime stride (251)
    // gives every byte a distinct-per-position value that does NOT align to
    // the 512 sector, the 2048 cluster, or the 7-byte write chunk — so a
    // misplaced byte at any sector- or cluster-boundary crossing shows up as
    // a mismatch at exactly that offset.
    let data: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
    write_streamed("WSTREAM5.TXT", &data);
    assert_eq!(find_size("WSTREAM5.TXT").unwrap(), 5000);
    let content = read_whole("WSTREAM5.TXT");
    assert_eq!(content.len(), 5000);
    assert!(content == data, "streamed round-trip diverges from source");
}

#[test_case]
fn write_at_invalid_name() {
    // The 8.3 name check happens on create, so touch rejects an over-long name.
    assert!(matches!(
        file::touch(ROOT, "TOOLONGNAME.TXT"),
        Err(FsError::InvalidName)
    ));
}

#[test_case]
fn create_empty_file_does_not_overwrite_existing() {
    write_streamed("WSTREAM6.TXT", b"keep this");
    // create_empty_file refuses to clobber an existing file
    assert!(matches!(
        with_volume(|vol| vol.create_empty_file(ROOT, "WSTREAM6.TXT")),
        Err(FsError::AlreadyExists)
    ));
    assert_eq!(find_size("WSTREAM6.TXT").unwrap(), 9); // unchanged
    assert_eq!(&read_whole("WSTREAM6.TXT"), b"keep this");
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
    let root = Dir::Root;
    // The fused contract: the Empty terminator is yielded exactly once,
    // as the final item, and the iterator stays finished afterwards.
    with_volume(|vol| {
        let mut iter = vol.dir_iter(root).unwrap();
        let mut empties = 0;
        let mut items_after_empty = 0;
        for item in iter.by_ref() {
            match item.unwrap().kind {
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
    let root = Dir::Root;
    // The FAT spec says nothing after the first 0x00 entry is valid, but
    // disks formatted elsewhere can carry stale bytes there. Plant a
    // convincing used entry one slot past the terminator and check it is
    // not reachable through the iterator path.
    with_volume(|vol| {
        // Locate the terminator via the iterator
        let mut empty_loc = None;
        for item in vol.dir_iter(root).unwrap() {
            let item = item.unwrap();
            if matches!(item.kind, DirEntryKind::Empty) {
                empty_loc = Some(item.location);
            }
        }
        let empty_loc = empty_loc.expect("root dir should have a free slot");
        // The slot after the terminator; may roll into the next sector
        let (sector, offset) = if empty_loc.offset + dir::ENTRY_BYTES == SECTOR_SIZE {
            (empty_loc.sector + 1, 0)
        } else {
            (empty_loc.sector, empty_loc.offset + dir::ENTRY_BYTES)
        };
        // Plant the phantom entry, keeping the original bytes
        let mut buf = *vol.read_sector(sector).unwrap();
        let mut original = [0u8; dir::ENTRY_BYTES];
        original.copy_from_slice(&buf[offset..offset + dir::ENTRY_BYTES]);
        let phantom = FileInfo {
            name: *b"PHANTOM ",
            extension: *b"TXT",
            attributes: dir::ATTR_ARCHIVE,
            first_cluster: 2,
            file_size: 5,
        };
        buf[offset..offset + dir::ENTRY_BYTES]
            .copy_from_slice(&phantom.as_bytes(vol.bpb.volume_type));
        vol.write_sector_uncached(sector, &buf).unwrap();

        let result = vol.find_file_dir_entry(root, "PHANTOM.TXT");

        // Restore the on-disk bytes before asserting so a green run
        // leaves the image untouched for later tests
        let _ = vol.modify_sector(sector, |buf| {
            buf[offset..offset + dir::ENTRY_BYTES].copy_from_slice(&original);
        });
        assert!(
            matches!(result, Err(FsError::NotFound)),
            "entry beyond the 0x00 terminator must be invisible"
        );
    });
}

#[test_case]
#[cfg(feature = "fat16")]
fn dir_iter_walks_into_second_root_sector() {
    use alloc::format;
    let root = Dir::Root;
    // One root sector holds 16 entries, so 20 files guarantee the
    // scan crosses a sector boundary. Pins the sector-advance logic:
    // a walk that advances per-entry, or stamps slots with the wrong
    // sector, passes single-sector lookups but fails here.
    const FILES: usize = 20;
    with_volume(|vol| {
        let mut slot_sectors = [0u32; FILES];
        for (i, slot_sector) in slot_sectors.iter_mut().enumerate() {
            let name = format!("SEC{i:02}.TXT");
            vol.create_empty_file(root, &name).unwrap();
            let (slot, _) = vol.find_file_dir_entry(root, &name).unwrap();
            *slot_sector = slot.sector;
        }
        // The slots must span at least two sectors
        assert!(
            slot_sectors.iter().any(|&s| s != slot_sectors[0]),
            "expected 20 dir entries to span more than one sector"
        );
        // The recorded slot must be honest: the entry's bytes must
        // really be at that sector and offset on disk
        let last_name = format!("SEC{:02}.TXT", FILES - 1);
        let (slot, _) = vol.find_file_dir_entry(root, &last_name).unwrap();
        let mut buf = [0u8; SECTOR_SIZE];
        vol.read_sector_uncached(slot.sector, &mut buf).unwrap();
        let raw: [u8; dir::ENTRY_BYTES] = buf[slot.offset..slot.offset + dir::ENTRY_BYTES]
            .try_into()
            .unwrap();
        match DirEntryKind::parse(raw, vol.bpb.volume_type) {
            DirEntryKind::Used(fi) => {
                assert_eq!(
                    fi.filename().as_str(),
                    Ok(last_name.as_str()),
                    "slot location does not hold the expected entry"
                );
            }
            _ => panic!("slot location does not hold a used entry"),
        }
        // Clean up so later tests see the usual directory
        for i in 0..FILES {
            let name = format!("SEC{i:02}.TXT");
            vol.delete_file(root, &name).unwrap();
        }
    });
}

#[test_case]
#[cfg(feature = "fat32")]
fn dir_iter_walks_into_second_root_cluster() {
    use alloc::format;
    let root = Dir::Root;
    // FAT32's root is a cluster chain (here 1 sector = 16 entries). Creating 20
    // files overflows the first root cluster and forces get_avail_dir_entry to
    // grow the directory — allocating a new cluster and linking it onto the
    // chain. This pins directory growth AND dir_iter following the root chain
    // across the cluster boundary: a walk that stops at the first cluster, or
    // growth that mislinks the chain, passes single-cluster lookups but not this.
    const FILES: usize = 20;
    with_volume(|vol| {
        let root_cluster = match vol.bpb.volume_type {
            VolumeType::Fat32(c) => c,
            _ => unreachable!("fat32-gated test"),
        };
        let mut slot_sectors = [0u32; FILES];
        for (i, slot_sector) in slot_sectors.iter_mut().enumerate() {
            let name = format!("SEC{i:02}.TXT");
            vol.create_empty_file(root, &name).unwrap();
            let (slot, _) = vol.find_file_dir_entry(root, &name).unwrap();
            *slot_sector = slot.sector;
        }
        // The root must actually have grown past its first cluster.
        assert!(
            matches!(vol.fat_entry(root_cluster), Ok(FatEntry::Next(_))),
            "root directory should have grown into a second cluster"
        );
        // The slots must span at least two clusters (different sectors).
        assert!(
            slot_sectors.iter().any(|&s| s != slot_sectors[0]),
            "expected 20 dir entries to span more than one root cluster"
        );
        // The recorded slot must honestly hold the entry on disk.
        let last_name = format!("SEC{:02}.TXT", FILES - 1);
        let (slot, _) = vol.find_file_dir_entry(root, &last_name).unwrap();
        let mut buf = [0u8; SECTOR_SIZE];
        vol.read_sector_uncached(slot.sector, &mut buf).unwrap();
        let raw: [u8; dir::ENTRY_BYTES] = buf[slot.offset..slot.offset + dir::ENTRY_BYTES]
            .try_into()
            .unwrap();
        match DirEntryKind::parse(raw, vol.bpb.volume_type) {
            DirEntryKind::Used(fi) => {
                assert_eq!(
                    fi.filename().as_str(),
                    Ok(last_name.as_str()),
                    "slot location does not hold the expected entry"
                );
            }
            _ => panic!("slot location does not hold a used entry"),
        }
        // Clean up so later tests see the usual directory.
        for i in 0..FILES {
            let name = format!("SEC{i:02}.TXT");
            let (slot, info) = vol.find_file_dir_entry(ROOT, name.as_str()).unwrap();
            vol.delete_file(slot, &info).unwrap();
        }
    });
}
