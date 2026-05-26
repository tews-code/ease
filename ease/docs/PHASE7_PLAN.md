# Phase 7: Storage & Filesystem — Detailed Sub-Plan

**Goal:** Read and write files on a FAT16 filesystem via QEMU virtio-blk
**Prerequisites:** Phase 6 complete (shell for testing commands)
**Existing module:** `src/drivers/virtio.rs` (driver written, needs integration)
**New modules:** `src/fs/mod.rs`, `src/fs/fat16.rs`

---

## Block Device Choice: virtio-blk

virtio-blk was chosen over pflash because its command/response driver model closely
mirrors the SD-over-SPI protocol used on real Pico 2 hardware (Phase 13). Both
require: initialization handshake, structured read/write commands, completion
polling, and error checking. This means the `BlockDevice` trait and driver
instincts developed here carry directly to hardware.

**Existing driver:** `src/drivers/virtio.rs` implements the full virtio-blk MMIO
protocol: device init, virtqueue setup, 3-descriptor request chains, and
read/write with status checking. It was written for a prior project ("os1k")
and needs adaptation for EASE.

**QEMU args to add:**
```
-drive file=disk.img,format=raw,if=none,id=drive0 -device virtio-blk-device,drive=drive0
```

---

## Step 1: Create Test Disk Image Infrastructure

**What:** Set up the tools and scripts to create FAT16-formatted disk images on the host.

### Tasks
1. Write a `mkdisk.sh` script that:
   - Creates a 1MB zeroed image: `dd if=/dev/zero of=disk.img bs=512 count=2048`
   - Formats it as FAT16: `mkfs.fat -F 16 -n EASE disk.img`
   - Copies a test file onto it: `echo "Hello from EASE!" > /tmp/hello.txt && mcopy -i disk.img /tmp/hello.txt ::/HELLO.TXT`
   - (Requires `dosfstools` and `mtools` packages)
2. Add `disk.img` to `.gitignore`
3. Verify: inspect the image on host with `mdir -i disk.img` and confirm the test file is present

### Notes
- `mtools` allows manipulating FAT images without mounting (no root needed)
- 1MB = 2048 sectors of 512 bytes. Plenty for development; can increase later
- FAT16 minimum is ~4MB for some tools, but `mkfs.fat -F 16` works on 1MB with small cluster size

---

## Step 2: Integrate virtio-blk Driver into EASE

**What:** Adapt the existing `virtio.rs` for EASE's module structure and wire it into the QEMU launch.

### Tasks
1. Fix import: `use crate::spinlock::SpinLock` → `use crate::kernel::sync::SpinLock`
2. Update module doc comment from "os1k" to "EASE"
3. Add `pub mod virtio;` to `src/drivers/mod.rs`
4. Update `.cargo/config.toml` runner:
   ```
   runner = "qemu-system-riscv32 -M virt -device ramfb -serial stdio -bios none -drive file=disk.img,format=raw,if=none,id=drive0 -device virtio-blk-device,drive=drive0 -kernel"
   ```
5. Update `ci.sh` to run `mkdisk.sh` before tests (ensure disk.img exists)
6. Call `virtio_blk_init()` from `main()` (both test and non-test entry points), after allocator init
7. Remove debug `print!(".")` from the spin loop in `read_write_disk()` (line 309) — floods output

### Tests
- Existing tests should pass once the import path is fixed and QEMU has the drive attached
- Fix `read_virtio_64bit_reg` test: capacity assertion is hardcoded to 20 sectors (10KB) — update to match the 1MB disk image (2048 sectors)
- Consider merging `fetch_and_or_reg` and `read_and_write_virtio_reg_status` into a single init-sequence test, or resetting device state at the start, since they depend on test execution order

### Verification
- `cargo test --bin ease` passes with virtio-blk device attached
- `write_to_file_and_read_back` test confirms read/write works on the FAT16-formatted image

---

## Step 3: `BlockDevice` Trait

**What:** Define the `BlockDevice` trait and implement it by wrapping the existing virtio free functions.

### Tasks
1. Add `BlockDevice` trait to `src/hal/mod.rs` (alongside `Writer` and `Reader`):
   ```
   pub trait BlockDevice {
       type Error;
       fn read_block(&self, block: u32, buf: &mut [u8; 512]) -> Result<(), Self::Error>;
       fn write_block(&mut self, block: u32, buf: &[u8; 512]) -> Result<(), Self::Error>;
       fn block_count(&self) -> u32;
   }
   ```
2. Create a `VirtioBlk` struct (in `virtio.rs` or a thin wrapper) that implements `BlockDevice`:
   - `read_block` calls `read_write_disk(buf, sector, false)` internally
   - `write_block` calls `read_write_disk(buf, sector, true)` internally
   - `block_count` returns capacity from `BLK_CAPACITY`
3. Update `read_write_disk` to return `Result<(), VirtioError>` instead of silently printing on error
   - Define `VirtioError` enum: `NotInitialized`, `SectorOutOfRange`, `DeviceError(u8)`

### Design Note
The existing virtio driver uses global statics (`BLK_REQUEST_VQ`, `BLK_REQ`, `BLK_CAPACITY`)
behind `SpinLock`. The `VirtioBlk` struct can be a zero-size or unit struct that accesses
these globals — this is the same pattern as `UartWriter`. Alternatively, refactor the
globals into the struct if you prefer owned state. Either approach works.

### Tests
- **QEMU test:** Read block 0 via `BlockDevice` trait, verify first bytes are FAT16 boot jump (`0xEB`, `0x3C`, `0x90` or similar)
- **QEMU test:** Read block 0, verify bytes 54-58 contain `"FAT16"` (or bytes 36-40 depending on BPB version)

---

## Step 4: Parse the FAT16 Boot Sector (BPB)

**What:** Read and parse the BIOS Parameter Block from block 0 to understand the filesystem geometry.

### Tasks
1. Create `src/fs/mod.rs` and `src/fs/fat16.rs`
2. Define a `Bpb` struct (parsed boot sector) with fields:
   - `bytes_per_sector: u16` (typically 512)
   - `sectors_per_cluster: u8` (1, 2, 4, 8, etc.)
   - `reserved_sectors: u16` (typically 1 for FAT16)
   - `fat_count: u8` (typically 2)
   - `root_entry_count: u16` (typically 512)
   - `total_sectors: u16` (or `total_sectors_32: u32` for large volumes)
   - `sectors_per_fat: u16`
   - `volume_label: [u8; 11]`
3. Implement `Bpb::parse(block: &[u8; 512]) -> Result<Bpb, FsError>`
   - Read little-endian fields at their BPB offsets
   - Validate: `bytes_per_sector == 512`, FAT type marker is `"FAT16"`
4. Compute derived values (methods on `Bpb`):
   - `fat_start_sector = reserved_sectors`
   - `root_dir_start_sector = reserved_sectors + (fat_count * sectors_per_fat)`
   - `root_dir_sectors = (root_entry_count * 32 + 511) / 512`
   - `data_start_sector = root_dir_start_sector + root_dir_sectors`
   - `cluster_to_sector(n) = data_start_sector + (n - 2) * sectors_per_cluster`

### Tests
- **Host test:** Construct a known BPB byte array by hand, parse it, verify all fields
- **Host test:** Verify computed sector offsets are correct
- **QEMU test:** Parse BPB from the actual disk image, print geometry, verify it looks sane

---

## Step 5: Read the FAT Table

**What:** Read FAT entries to follow cluster chains.

### Tasks
1. Define the `Fat16` struct (which wraps a `BlockDevice` + parsed `Bpb`):
   ```
   pub struct Fat16<D: BlockDevice> {
       device: D,
       bpb: Bpb,
   }
   ```
2. Implement `fat_entry(&self, cluster: u16) -> Result<u16, FsError>`:
   - Each FAT16 entry is 2 bytes (little-endian)
   - 256 entries per 512-byte sector
   - Sector = `bpb.fat_start_sector + (cluster / 256)`
   - Offset within sector = `(cluster % 256) * 2`
   - Read the sector, extract the 2-byte entry
3. Define FAT entry meanings:
   - `0x0000` — free cluster
   - `0x0001` — reserved
   - `0x0002..=0xFFEF` — next cluster in chain
   - `0xFFF0..=0xFFF6` — reserved
   - `0xFFF7` — bad cluster
   - `0xFFF8..=0xFFFF` — end of chain
4. Implement `cluster_chain(&self, start: u16) -> Result<Vec<u16>, FsError>`:
   - Follow the chain from `start` until end-of-chain marker
   - Cap at a reasonable limit (e.g., 1024 clusters) to avoid infinite loops on corrupt data

### Tests
- **Host test:** Construct a mock FAT table in memory, verify chain following
- **Host test:** Test end-of-chain detection, free cluster detection, bad cluster detection
- **QEMU test:** Read the FAT from the real disk image, verify the test file's cluster chain

---

## Step 6: Read the Root Directory

**What:** Parse the root directory to list files.

### Tasks
1. Define `DirEntry` struct:
   - `name: [u8; 8]` (space-padded)
   - `ext: [u8; 3]` (space-padded)
   - `attrs: u8` (read-only, hidden, system, volume label, directory, archive)
   - `first_cluster: u16`
   - `file_size: u32`
2. Define attribute constants:
   - `ATTR_READ_ONLY = 0x01`
   - `ATTR_HIDDEN = 0x02`
   - `ATTR_SYSTEM = 0x04`
   - `ATTR_VOLUME_LABEL = 0x08`
   - `ATTR_DIRECTORY = 0x10`
   - `ATTR_ARCHIVE = 0x20`
   - `ATTR_LFN = 0x0F` (long filename entry — skip these)
3. Implement `DirEntry::parse(bytes: &[u8; 32]) -> Option<DirEntry>`:
   - Return `None` for empty entries (first byte `0x00` = end, `0xE5` = deleted)
   - Skip LFN entries (`attrs == 0x0F`)
   - Skip volume label entries
4. Implement `Fat16::read_root_dir(&self) -> Result<Vec<DirEntry>, FsError>`:
   - Read sectors from `root_dir_start_sector` for `root_dir_sectors` count
   - Parse 16 entries per sector (each entry is 32 bytes)
   - Stop on first `0x00` entry
5. Add `DirEntry::filename(&self) -> String`:
   - Trim spaces from name and extension
   - Format as `"NAME.EXT"` (or just `"NAME"` if no extension)
   - Handle the `0x05` first-byte special case (means `0xE5` in Shift-JIS)

### Tests
- **Host test:** Construct directory entries by hand, verify parsing
- **Host test:** Test skip logic for deleted, LFN, volume label entries
- **QEMU test:** List root directory of disk image, verify `HELLO.TXT` appears with correct size

Steps 5 and 6 can be done in either order (both depend on Step 4, not on each other).

---

## Step 7: `ls` Shell Command

**What:** Add an `ls` command that lists files in the root directory.

### Tasks
1. Initialize the `Fat16` instance during kernel boot (after allocator and virtio init):
   - Create `VirtioBlk` instance
   - Read block 0, parse the BPB
   - Construct `Fat16 { device, bpb }`
   - Store the `Fat16` instance (consider ownership — passed into shell, or global behind SpinLock)
2. Add `ls` command to shell:
   - Call `fat16.read_root_dir()`
   - Print each entry: `filename  size_bytes`
   - Format sizes nicely (right-aligned, or with KB/MB suffixes)
3. Handle errors gracefully (print error message, don't panic)

### Verification
- Boot EASE, type `ls`, see `HELLO.TXT` with correct file size
- Add more files to disk image in `mkdisk.sh`, verify they all appear

---

## Step 8: Read File Contents (`open` / `read` / `cat`)

**What:** Implement file reading by following cluster chains.

### Tasks
1. Implement `Fat16::open(&self, name: &str) -> Result<DirEntry, FsError>`:
   - Search root directory for matching filename (case-insensitive)
   - Convert input name to 8.3 format for comparison
   - Return the `DirEntry` if found, or `FsError::NotFound`
2. Implement `Fat16::read_file(&self, entry: &DirEntry) -> Result<Vec<u8>, FsError>`:
   - Get the cluster chain starting from `entry.first_cluster`
   - For each cluster, read all sectors (`sectors_per_cluster` sectors)
   - Concatenate into a `Vec<u8>`
   - Truncate to `entry.file_size` (last cluster may be partially filled)
3. Add `cat` shell command:
   - `cat HELLO.TXT` — open file, read contents, print as UTF-8 text
   - Handle missing file: print `"File not found: HELLO.TXT"`
4. Consider a streaming `read` API for large files (read one cluster at a time into a buffer) to avoid allocating the entire file into memory. This can be a stretch goal or deferred.

### Tests
- **Host test:** Mock block device with known FAT + directory + data clusters, verify read returns correct bytes
- **QEMU test:** `cat HELLO.TXT` prints `"Hello from EASE!"`
- **QEMU test:** Create a multi-cluster file on host, verify it reads correctly
- **QEMU test:** `cat NONEXIST.TXT` prints error, doesn't crash

---

## Step 9: Write Files to Filesystem

**What:** Implement creating new files and writing data.

### Tasks
1. Implement `Fat16::allocate_cluster(&mut self) -> Result<u16, FsError>`:
   - Scan FAT for a free entry (`0x0000`)
   - Mark it as end-of-chain (`0xFFFF`)
   - Write updated FAT sector back to device
   - (Update both FAT copies if `fat_count == 2`)
2. Implement `Fat16::extend_chain(&mut self, last: u16) -> Result<u16, FsError>`:
   - Allocate a new cluster
   - Update the previous last cluster's FAT entry to point to the new one
3. Implement `Fat16::create_file(&mut self, name: &str, data: &[u8]) -> Result<(), FsError>`:
   - Validate filename (8.3 format)
   - Find a free root directory entry (first byte `0x00` or `0xE5`)
   - Allocate clusters for the data
   - Write data to the allocated clusters
   - Create the directory entry with name, size, first cluster
   - Write the directory sector back
4. Implement `Fat16::write_file(&mut self, entry: &DirEntry, data: &[u8]) -> Result<(), FsError>`:
   - Free existing cluster chain (set FAT entries to `0x0000`)
   - Allocate new clusters for the new data
   - Write data, update directory entry with new size and first cluster

### Tests
- **QEMU test:** Create a file in EASE, read it back with `cat`, verify contents
- **Host verification:** After QEMU test, inspect disk.img on host with `mdir -i disk.img` or mount it to confirm the file is valid FAT16

---

## Step 10: Additional Shell Commands

**What:** Add `hexdump` and `cd` commands, plus any polish.

### Tasks
1. **`hexdump` command:** `hexdump FILENAME`
   - Read file contents
   - Print in classic hexdump format: offset, hex bytes, ASCII representation
   - 16 bytes per line
   - Example: `00000000  48 65 6C 6C 6F 20 66 72  6F 6D 20 45 41 53 45 21  |Hello from EASE!|`
2. **`write` command (optional):** `write FILENAME`
   - Enter text interactively, Ctrl+D to finish
   - Create/overwrite the file with entered text
3. **`cd` command:** Defer subdirectory support if time is short
   - FAT16 subdirectories are stored as files with `ATTR_DIRECTORY`
   - Their contents are cluster chains of 32-byte directory entries (same format as root dir, but no fixed size)
   - Implementing `cd` requires tracking "current directory" state and reading subdirectory cluster chains
   - This is a stretch goal for Phase 7

### Tests
- **QEMU test:** `hexdump HELLO.TXT` produces correct hex output
- **QEMU test:** `write TEST.TXT` + type content + `cat TEST.TXT` round-trips

---

## Step 11: VFS Trait (Stretch Goal)

**What:** Abstract the filesystem behind a trait for future extensibility.

### Tasks
1. Define VFS trait in `src/fs/mod.rs`:
   ```
   pub trait FileSystem {
       fn open(&self, path: &str) -> Result<FileHandle, FsError>;
       fn read(&self, handle: &FileHandle, buf: &mut [u8]) -> Result<usize, FsError>;
       fn write(&mut self, handle: &FileHandle, data: &[u8]) -> Result<usize, FsError>;
       fn close(&mut self, handle: FileHandle);
       fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
   }
   ```
2. Implement `FileSystem` for `Fat16`
3. Update shell commands to use the trait instead of `Fat16` directly

### Notes
- This is a stretch goal. If the concrete `Fat16` API works well, the trait can wait until Phase 13+ when a second filesystem or device might appear.
- Don't over-engineer: if there's only one implementation, a trait adds indirection without benefit.

---

## Error Type

Define a shared error type early (Step 3 or 4):

```
pub enum FsError {
    DeviceError,       // Block device I/O failed
    NotFat16,          // Boot sector doesn't contain FAT16 signature
    NotFound,          // File not found
    DiskFull,          // No free clusters
    DirFull,           // No free directory entries
    InvalidName,       // Filename doesn't fit 8.3 format
    CorruptFs,         // Inconsistent filesystem structures
}
```

---

## QEMU Runner Changes

The `.cargo/config.toml` runner needs the virtio-blk drive. New runner:

```
runner = "qemu-system-riscv32 -M virt -device ramfb -serial stdio -bios none -drive file=disk.img,format=raw,if=none,id=drive0 -device virtio-blk-device,drive=drive0 -kernel"
```

Ensure `ci.sh` creates `disk.img` before running tests.

---

## File Organization

```
src/
├── drivers/
│   ├── virtio.rs          # VirtioBlk driver (existing, needs adaptation)
│   └── ...
├── fs/
│   ├── mod.rs             # FsError enum
│   └── fat16.rs           # Bpb, DirEntry, Fat16 struct, all FAT16 logic
├── hal/
│   └── mod.rs             # BlockDevice trait (alongside Writer/Reader)
└── shell/
    └── commands.rs         # Add ls, cat, hexdump, write commands
```

---

## Suggested Implementation Order

| Step | Description | Depends On | Difficulty | Status |
|------|-------------|-----------|------------|--------|
| 1 | Disk image infrastructure (`mkdisk.sh`) | — | Easy | New |
| 2 | Integrate virtio driver into EASE | Step 1 | Easy | Adapting existing code |
| 3 | `BlockDevice` trait + `VirtioBlk` wrapper | Step 2 | Easy | New |
| 4 | Parse FAT16 boot sector (BPB) | Step 3 | Medium | New |
| 5 | Read FAT table + cluster chains | Step 4 | Medium | New |
| 6 | Read root directory entries | Step 4 | Medium | New |
| 7 | `ls` shell command | Steps 5, 6 | Easy | New |
| 8 | Read file contents + `cat` command | Steps 5, 6, 7 | Medium | New |
| 9 | Write files to filesystem | Step 8 | Hard | New |
| 10 | `hexdump` + polish | Step 8 | Easy | New |
| 11 | VFS trait (stretch) | Step 9 | Medium | Stretch |

Steps 5 and 6 can be done in parallel (both depend on Step 4, not on each other).

Note: The old pflash Step 9 (write support) is gone — virtio-blk already handles writes.

---

## Virtio Driver Cleanup Checklist

Issues to fix when integrating `virtio.rs` (Step 2):

- [ ] Fix import: `use crate::spinlock::SpinLock` → `use crate::kernel::sync::SpinLock`
- [ ] Update module doc from "os1k" to "EASE"
- [ ] Add `pub mod virtio;` to `src/drivers/mod.rs`
- [ ] Remove `print!(".")` from spin loop (line 309)
- [ ] Make `read_write_disk` return `Result<(), VirtioError>` instead of printing on error
- [ ] Update capacity test assertion to match 1MB disk image (2048 sectors)
- [ ] Fix or merge order-dependent tests (`fetch_and_or_reg` asserts `== 5`)
- [ ] Remove duplicate test status printing (tests print `[ok]` but framework also prints status)

---

## Key References

- **FAT16 spec:** Microsoft FAT specification (fatgen103.doc) — the canonical reference
- **BPB layout:** Bytes 0-61 of sector 0, well documented on OSDev wiki
- **Virtio spec:** Virtual I/O Device Specification v1.0 — Section 5.2 (Block Device)
- **QEMU virt memory map:** `hw/riscv/virt.c` — virtio MMIO at `0x10001000`+

---

*Plan created: February 2026*
*Estimated effort: ~25-35 hours across 11 steps*
