//! Volume tests
//!
//! Volume tests are kernel-only because they exercise the real virtio
//! block device. The lib crate stubs out `read_block`/`write_block` so
//! volume.rs's production code compiles, but its tests can't run on
//! host without a disk mock — that's a follow-up refactor (the
//! "BlockDevice trait" approach). For now this gate keeps lib-test
//! builds clean.

use super::*;

// =========================================================================
// Volume::open tests
// =========================================================================

#[test_case]
fn open_finds_hello_txt() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "HELLO.TXT").unwrap();
        assert!(entry.file_size > 0);
    });
}

#[test_case]
fn open_case_insensitive() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "hello.txt").unwrap();
        assert!(entry.file_size > 0);
    });
}

#[test_case]
fn open_not_found() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let result = vol.open(current_dir, "NOPE.TXT");
        assert!(matches!(result, Err(FsError::NotFound)));
    });
}

#[test_case]
fn open_empty_file() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "EMPTY.TXT").unwrap();
        assert_eq!(entry.file_size, 0);
    });
}

#[test_case]
fn open_no_extension() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "SHORT").unwrap();
        assert!(entry.file_size > 0);
    });
}

// =========================================================================
// Volume::read_file tests
// =========================================================================

#[test_case]
fn read_file_hello_txt() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "HELLO.TXT").unwrap();
        let content = vol.read_file(&entry).unwrap();
        let text = core::str::from_utf8(&content).unwrap();
        assert_eq!(text, "Text file contents\n");
    });
}

#[test_case]
fn read_file_empty() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "EMPTY.TXT").unwrap();
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(content.len(), 0);
    });
}

#[test_case]
fn read_file_short() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "SHORT").unwrap();
        let content = vol.read_file(&entry).unwrap();
        let text = core::str::from_utf8(&content).unwrap();
        assert_eq!(text, "This is a file with a short name.\n");
    });
}

#[test_case]
fn read_file_size_matches_dir_entry() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "HELLO.TXT").unwrap();
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(content.len(), entry.file_size as usize);
    });
}

#[test_case]
fn read_file_longname() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let entry = vol.open(current_dir, "LONGNAME.END").unwrap();
        let content = vol.read_file(&entry).unwrap();
        let text = core::str::from_utf8(&content).unwrap();
        assert_eq!(text, "This is a file with a long name.\n");
    });
}

#[test_case]
fn read_file_64kb() {
    let current_dir = DirHandle { start_cluster: 0 };
    // 64KB file spans many clusters — tests cluster chain following at scale
    with_volume(|vol| {
        let entry = vol.open(current_dir, "BIG.TXT").unwrap();
        assert_eq!(entry.file_size, 64 * 1024);
        let content = vol.read_file(&entry).unwrap();
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
    });
}

#[test_case]
fn open_multi_cluster_file() {
    let current_dir = DirHandle { start_cluster: 0 };
    // The PDF is ~7.9MB — too large to read into heap, but verify open finds it
    // and the dir entry has the expected size.
    with_volume(|vol| {
        let entry = vol.open(current_dir, "RP-008~1.PDF").unwrap();
        assert_eq!(entry.file_size, 7_968_417);
    });
}

#[test_case]
fn disk_image_fat_entry_hello_txt() {
    let current_dir = DirHandle { start_cluster: 0 };
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
    let root_dir = DirHandle { start_cluster: 0 };
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
    let root_dir = DirHandle { start_cluster: 0 };
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
fn disk_image_fat_entry_reserved() {
    // FAT entries 0 and 1 are reserved slots, not chain entries: FAT[0]
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

// =========================================================================
// Volume::create_empty_file tests
// =========================================================================

#[test_case]
fn touch_creates_file() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.create_empty_file(current_dir, "NEW.TXT").unwrap();
        let entry = vol.open(current_dir, "NEW.TXT").unwrap();
        assert_eq!(entry.file_size, 0);
    });
}

#[test_case]
fn touch_case_insensitive_open() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.create_empty_file(current_dir, "LOWER.TXT").unwrap();
        let entry = vol.open(current_dir, "lower.txt").unwrap();
        assert_eq!(entry.file_size, 0);
    });
}

#[test_case]
fn touch_no_extension() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.create_empty_file(current_dir, "NOEXT").unwrap();
        let entry = vol.open(current_dir, "NOEXT").unwrap();
        assert_eq!(entry.file_size, 0);
    });
}

#[test_case]
fn touch_invalid_name_rejected() {
    with_volume(|vol| {
        let result = vol.create_empty_file(DirHandle { start_cluster: 0 }, "TOOLONGNAME.TXT");
        assert!(matches!(result, Err(FsError::InvalidName)));
    });
}

#[test_case]
fn touch_created_file_visible_in_ls() {
    let current_dir = DirHandle { start_cluster: 0 };
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
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.create_empty_file(current_dir, "DEL1.TXT").unwrap();
        assert!(vol.open(current_dir, "DEL1.TXT").is_ok());
        vol.delete_file(current_dir, "DEL1.TXT").unwrap();
        assert!(matches!(
            vol.open(current_dir, "DEL1.TXT"),
            Err(FsError::NotFound)
        ));
    });
}

#[test_case]
fn delete_file_not_found() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let result = vol.delete_file(current_dir, "NOPE.TXT");
        assert!(matches!(result, Err(FsError::NotFound)));
    });
}

#[test_case]
fn delete_file_with_content() {
    let current_dir = DirHandle { start_cluster: 0 };
    // DELETE.ME exists solely for this test — no other test depends on it
    with_volume(|vol| {
        let entry = vol.open(current_dir, "DELETE.ME").unwrap();
        let first_cluster = entry.first_cluster;
        assert!(first_cluster >= 2);

        vol.delete_file(current_dir, "DELETE.ME").unwrap();

        // File should no longer be found
        assert!(matches!(
            vol.open(current_dir, "DELETE.ME"),
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
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.create_empty_file(current_dir, "REUSE.TXT").unwrap();
        vol.delete_file(current_dir, "REUSE.TXT").unwrap();
        // Slot marked 0xE5 should be reusable
        vol.create_empty_file(current_dir, "REUSE.TXT").unwrap();
        let entry = vol.open(current_dir, "REUSE.TXT").unwrap();
        assert_eq!(entry.file_size, 0);
    });
}

// =========================================================================
// set_fat_entry tests
// =========================================================================

#[test_case]
fn set_fat_entry_roundtrip() {
    with_volume(|vol| {
        // Find a free cluster to test with
        let mut test_cluster = 0u32;
        for c in 2..100 {
            if vol.fat_entry(c).unwrap() == FatEntry::Free {
                test_cluster = c;
                break;
            }
        }
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
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.write_file(current_dir, "WTEST1.TXT", b"hello world\n")
            .unwrap();
        let entry = vol.open(current_dir, "WTEST1.TXT").unwrap();
        assert_eq!(entry.file_size, 12);
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(&content, b"hello world\n");
    });
}

#[test_case]
fn write_file_empty_data() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.write_file(current_dir, "WTEST2.TXT", b"").unwrap();
        let entry = vol.open(current_dir, "WTEST2.TXT").unwrap();
        assert_eq!(entry.file_size, 0);
    });
}

#[test_case]
fn write_file_overwrite() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.write_file(current_dir, "WTEST3.TXT", b"first").unwrap();
        vol.write_file(current_dir, "WTEST3.TXT", b"second")
            .unwrap();
        let entry = vol.open(current_dir, "WTEST3.TXT").unwrap();
        assert_eq!(entry.file_size, 6);
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(&content, b"second");
    });
}

#[test_case]
fn write_file_multi_sector() {
    let current_dir = DirHandle { start_cluster: 0 };
    // Write more than one sector (512 bytes)
    with_volume(|vol| {
        let data = [b'A'; 1024];
        vol.write_file(current_dir, "WTEST4.TXT", &data).unwrap();
        let entry = vol.open(current_dir, "WTEST4.TXT").unwrap();
        assert_eq!(entry.file_size, 1024);
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(content.len(), 1024);
        assert!(content.iter().all(|&b| b == b'A'));
    });
}

#[test_case]
fn write_file_multi_cluster() {
    let current_dir = DirHandle { start_cluster: 0 };
    // Write more than one cluster (sectors_per_cluster * 512 = 2048 bytes)
    with_volume(|vol| {
        let data = [b'B'; 4096];
        vol.write_file(current_dir, "WTEST5.TXT", &data).unwrap();
        let entry = vol.open(current_dir, "WTEST5.TXT").unwrap();
        assert_eq!(entry.file_size, 4096);
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(content.len(), 4096);
        assert!(content.iter().all(|&b| b == b'B'));
    });
}

#[test_case]
fn write_file_invalid_name() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        let result = vol.write_file(current_dir, "TOOLONGNAME.TXT", b"data");
        assert!(matches!(result, Err(FsError::InvalidName)));
    });
}

#[test_case]
fn touch_does_not_overwrite_existing() {
    let current_dir = DirHandle { start_cluster: 0 };
    with_volume(|vol| {
        vol.write_file(current_dir, "WTEST6.TXT", b"keep this")
            .unwrap();
        vol.create_empty_file(current_dir, "WTEST6.TXT").unwrap();
        let entry = vol.open(current_dir, "WTEST6.TXT").unwrap();
        assert_eq!(entry.file_size, 9); // unchanged
        let content = vol.read_file(&entry).unwrap();
        assert_eq!(&content, b"keep this");
    });
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
    let root = DirHandle { start_cluster: 0 };
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
    let root = DirHandle { start_cluster: 0 };
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
                empty_loc = Some(item.slot);
            }
        }
        let empty_loc = empty_loc.expect("root dir should have a free slot");
        // The slot after the terminator; may roll into the next sector
        let (sector, offset) = if empty_loc.offset + DIR_ENTRY_BYTES == SECTOR_SIZE {
            (empty_loc.sector + 1, 0)
        } else {
            (empty_loc.sector, empty_loc.offset + DIR_ENTRY_BYTES)
        };
        // Plant the phantom entry, keeping the original bytes
        let mut buf = *vol.read_sector(sector).unwrap();
        let mut original = [0u8; DIR_ENTRY_BYTES];
        original.copy_from_slice(&buf[offset..offset + DIR_ENTRY_BYTES]);
        let phantom = FileInfo {
            name: *b"PHANTOM ",
            extension: *b"TXT",
            attributes: ATTR_ARCHIVE,
            first_cluster: 2,
            file_size: 5,
        };
        buf[offset..offset + DIR_ENTRY_BYTES]
            .copy_from_slice(&phantom.as_bytes(vol.bpb.volume_type));
        vol.write_sector_uncached(sector, &buf).unwrap();

        let result = vol.open(root, "PHANTOM.TXT");

        // Restore the on-disk bytes before asserting so a green run
        // leaves the image untouched for later tests
        let _ = vol.modify_sector(sector, |buf| {
            buf[offset..offset + DIR_ENTRY_BYTES].copy_from_slice(&original);
        });
        assert!(
            matches!(result, Err(FsError::NotFound)),
            "entry beyond the 0x00 terminator must be invisible"
        );
    });
}

#[test_case]
fn dir_iter_walks_into_second_root_sector() {
    use alloc::format;
    let root = DirHandle { start_cluster: 0 };
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
            let (slot, _) = vol.find_dir_entry_location(root, &name).unwrap();
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
        let (slot, _) = vol.find_dir_entry_location(root, &last_name).unwrap();
        let mut buf = [0u8; SECTOR_SIZE];
        vol.read_sector_uncached(slot.sector, &mut buf).unwrap();
        let raw: [u8; DIR_ENTRY_BYTES] = buf[slot.offset..slot.offset + DIR_ENTRY_BYTES]
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
