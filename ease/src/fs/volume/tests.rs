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

/// Open `name`, read the whole file via `read_file`, then close.
///
/// `file::open` takes the volume lock internally, so it must run OUTSIDE
/// `with_volume`; the returned `FileHandle` is what the volume read paths
/// (`read_file`, `read_at`) operate on.
fn read_whole(name: &str) -> Vec<u8> {
    let handle = file::open(Access::Read, ROOT, name).unwrap();
    let content = with_volume(|vol| vol.read_file(&handle)).unwrap();
    file::close(&handle).expect("should not error closing Access::Read file");
    content
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
    // buffers whose edges land mid-sector and mid-cluster. A defect in the
    // sector-within-cluster index or the FAT-chain-follow at a cluster
    // boundary corrupts bytes exactly at those offsets, so reassembling the
    // stream and comparing it against `read_file` (an independent read path,
    // via `read_sector_uncached`) pins the whole `read_at` boundary logic.
    // `read_file` is the oracle, but it holds the whole 64 KB file in heap —
    // so compare each read_at chunk against it in-place and never accumulate a
    // second full copy (only one 64 KB buffer plus the stack chunk is live).
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
    file::close(&handle).expect("should not error closing Access::Read file");

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
// Volume::delete_file tests
// =========================================================================

#[test_case]
fn delete_empty_file() {
    with_volume(|vol| vol.create_empty_file(ROOT, "DEL1.TXT")).unwrap();
    assert!(find_size("DEL1.TXT").is_ok());
    with_volume(|vol| vol.delete_file(ROOT, "DEL1.TXT")).unwrap();
    assert!(matches!(find_size("DEL1.TXT"), Err(FsError::NotFound)));
}

#[test_case]
fn delete_file_not_found() {
    let current_dir = Dir::Root;
    with_volume(|vol| {
        let result = vol.delete_file(current_dir, "NOPE.TXT");
        assert!(matches!(result, Err(FsError::NotFound)));
    });
}

#[test_case]
fn delete_file_with_content() {
    let current_dir = Dir::Root;
    // DELETE.ME exists solely for this test — no other test depends on it
    with_volume(|vol| {
        let (_, fi) = vol.find_file_dir_entry(current_dir, "DELETE.ME").unwrap();
        let first_cluster = fi.first_cluster;
        assert!(first_cluster >= 2);

        vol.delete_file(current_dir, "DELETE.ME").unwrap();

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
        vol.delete_file(ROOT, "REUSE.TXT").unwrap();
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
// Volume::write_file tests
// =========================================================================

#[test_case]
fn write_file_and_read_back() {
    with_volume(|vol| vol.write_file(ROOT, "WTEST1.TXT", b"hello world\n")).unwrap();
    assert_eq!(find_size("WTEST1.TXT").unwrap(), 12);
    assert_eq!(&read_whole("WTEST1.TXT"), b"hello world\n");
}

#[test_case]
fn write_file_empty_data() {
    with_volume(|vol| vol.write_file(ROOT, "WTEST2.TXT", b"")).unwrap();
    assert_eq!(find_size("WTEST2.TXT").unwrap(), 0);
}

#[test_case]
fn write_file_overwrite() {
    with_volume(|vol| vol.write_file(ROOT, "WTEST3.TXT", b"first")).unwrap();
    with_volume(|vol| vol.write_file(ROOT, "WTEST3.TXT", b"second")).unwrap();
    assert_eq!(find_size("WTEST3.TXT").unwrap(), 6);
    assert_eq!(&read_whole("WTEST3.TXT"), b"second");
}

#[test_case]
fn write_file_multi_sector() {
    // Write more than one sector (512 bytes)
    let data = [b'A'; 1024];
    with_volume(|vol| vol.write_file(ROOT, "WTEST4.TXT", &data)).unwrap();
    assert_eq!(find_size("WTEST4.TXT").unwrap(), 1024);
    let content = read_whole("WTEST4.TXT");
    assert_eq!(content.len(), 1024);
    assert!(content.iter().all(|&b| b == b'A'));
}

#[test_case]
fn write_file_multi_cluster() {
    // Write more than one cluster (sectors_per_cluster * 512 = 2048 bytes)
    let data = [b'B'; 4096];
    with_volume(|vol| vol.write_file(ROOT, "WTEST5.TXT", &data)).unwrap();
    assert_eq!(find_size("WTEST5.TXT").unwrap(), 4096);
    let content = read_whole("WTEST5.TXT");
    assert_eq!(content.len(), 4096);
    assert!(content.iter().all(|&b| b == b'B'));
}

#[test_case]
fn write_file_invalid_name() {
    let current_dir = Dir::Root;
    with_volume(|vol| {
        let result = vol.write_file(current_dir, "TOOLONGNAME.TXT", b"data");
        assert!(matches!(result, Err(FsError::InvalidName)));
    });
}

#[test_case]
fn touch_does_not_overwrite_existing() {
    with_volume(|vol| {
        vol.write_file(ROOT, "WTEST6.TXT", b"keep this").unwrap();
        // create_empty_file refuses to clobber an existing file
        assert!(matches!(
            vol.create_empty_file(ROOT, "WTEST6.TXT"),
            Err(FsError::AlreadyExists)
        ));
    });
    assert_eq!(find_size("WTEST6.TXT").unwrap(), 9); // unchanged
    assert_eq!(&read_whole("WTEST6.TXT"), b"keep this");
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
            vol.delete_file(root, &name).unwrap();
        }
    });
}
