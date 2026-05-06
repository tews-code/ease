# Filesystem Module Refactoring Opportunities

Review of `src/fs/` — March 2026

## Priority Refactorings

### 1. Extract `write_dir_entry(name, ext, cluster, size)` (~30 lines saved)

The "find free slot, write entry, write sector back" loop is duplicated between
`create_empty_file` and `write_file`. A shared helper would take the 8.3 name,
first cluster, and file size.

**Locations:**
- `volume.rs:158-183` (`create_empty_file`)
- `volume.rs:284-314` (`write_file`)

### 2. Extract `fat_sector_and_offset(cluster)` (~8 lines saved)

`fat_entry` and `set_fat_entry` both compute the same sector number and byte
offset arithmetic. A small helper returns `(sector, offset)`.

**Locations:**
- `volume.rs:35-45` (`fat_entry`)
- `volume.rs:50-58` (`set_fat_entry`)

### 3. Rewrite `find_dir_entry_location` using `read_root_dir` (~25 lines saved)

If `read_root_dir`'s callback received the sector and offset alongside the
`DirEntry`, `find_dir_entry_location` could be eliminated entirely — it's a
re-implementation of the same scan.

**Location:** `volume.rs:190-214`

### 4. Add `is_end_of_chain()` helper

The `0xFFF8..=0xFFFF` match appears in `read_file` and `delete_file`. A named
function or constant makes the intent clearer.

**Locations:**
- `volume.rs:139-142` (`read_file`)
- `volume.rs:230-233` (`delete_file`)

## Quick Hygiene Fixes

- `volume.rs:130` — hardcoded `512` should be `SECTOR_SIZE`
- `dir_entry.rs` — `#[allow(dead_code)]` on `parse_83_name` is no longer needed (it's called now)
- `volume.rs` — `allocate_cluster` should be private, not `pub`
- Audit blanket `#[allow(dead_code)]` on `FsError`, `Bpb`, `DirEntry` — may be masking unused fields; apply narrowly instead
