# FAT16 Explained

## The Problem

You have a disk -- a flat array of 512-byte blocks (sectors). You need to store files on it. Each file has a name, a size, and data that might span multiple sectors. You need a way to:

- Find a file by name
- Know which sectors contain its data
- Know where free space is (for writing new files)

FAT16 is one of the simplest solutions to this problem.

## Disk Layout

A FAT16 disk is divided into four regions, laid out in order:

```
+──────────────+───────────+────────────────+──────────────────+
| Boot Sector  | FAT Table | Root Directory  | Data Area        |
| (1 sector)   | (N sects) | (M sectors)     | (rest of disk)   |
+──────────────+───────────+────────────────+──────────────────+
  sector 0       sector 1    sector 1+N        sector 1+N+M
```

## 1. Boot Sector (BPB) -- "The Map"

Sector 0 contains the **BIOS Parameter Block** -- metadata that describes the filesystem geometry. It tells you:

- How big are sectors (always 512 for us)
- How many sectors per cluster (grouping unit -- explained below)
- How many FAT tables (usually 2, for redundancy)
- How big is each FAT table
- How many root directory entries are allowed
- Total sector count

From these fields you can calculate where everything else starts on disk. It's the map to the whole filesystem.

### BPB Field Layout

All multi-byte fields are little-endian. Example values from the EASE 16MB disk image:

| Offset | Size | Field                 | Example value              |
|--------|------|-----------------------|----------------------------|
| 0      | 3    | Jump boot code        | `EB 3C 90`                 |
| 3      | 8    | OEM name              | `"mkfs.fat"`               |
| 11     | 2    | `bytes_per_sector`    | `0x0200` = 512             |
| 13     | 1    | `sectors_per_cluster` | `0x04` = 4                 |
| 14     | 2    | `reserved_sectors`    | `0x0004` = 4               |
| 16     | 1    | `fat_count`           | `0x02` = 2                 |
| 17     | 2    | `root_entry_count`    | `0x0200` = 512             |
| 19     | 2    | `total_sectors_16`    | `0x8000` = 32768           |
| 21     | 1    | Media type            | `0xF8` (fixed disk)        |
| 22     | 2    | `sectors_per_fat`     | `0x0020` = 32              |
| 36     | 1    | Drive number          | `0x80`                     |
| 38     | 1    | Boot signature        | `0x29`                     |
| 43     | 11   | Volume label          | `"NO NAME    "`            |
| 54     | 8    | FS type string        | `"FAT16   "`               |

### Derived Geometry

From the BPB fields you can calculate the start of each disk region:

- `fat_start_sector` = `reserved_sectors` = 4
- `root_dir_start` = `reserved_sectors + fat_count * sectors_per_fat` = 4 + 2*32 = 68
- `root_dir_sectors` = `(root_entry_count * 32 + 511) / 512` = (512*32+511)/512 = 32
- `data_start_sector` = `root_dir_start + root_dir_sectors` = 68 + 32 = 100
- `cluster_to_sector(n)` = `data_start_sector + (n - 2) * sectors_per_cluster`

## 2. Clusters -- "The Allocation Unit"

FAT16 doesn't track individual sectors for file data. Instead, it groups sectors into **clusters** (e.g., 4 sectors = 1 cluster = 2KB). Clusters are the smallest unit the filesystem allocates to files.

Why? Because tracking every 512-byte sector individually would need a huge table. Grouping them into clusters keeps the table small. The tradeoff is wasted space -- a 1-byte file still uses a full cluster.

Clusters are numbered starting at **2** (0 and 1 are reserved). The data area starts at cluster 2.

## 3. FAT Table -- "The Chain"

The **File Allocation Table** is an array of 16-bit (2-byte) entries, one per cluster. It answers two questions:

- **Is this cluster free?** Entry is `0x0000`
- **What's the next cluster of this file?** Entry contains the next cluster number

A file's data is stored as a **linked list of clusters**, called a cluster chain. You follow the chain through the FAT:

```
File "HELLO.TXT" starts at cluster 4

FAT[4] = 5      -> next cluster is 5
FAT[5] = 6      -> next cluster is 6
FAT[6] = 0xFFFF -> end of chain (no more clusters)

So the file's data is in clusters 4, 5, 6 (in order).
```

Special values:
- `0x0000` -- free cluster
- `0xFFF7` -- bad cluster (damaged, don't use)
- `0xFFF8`-`0xFFFF` -- end of chain

The "16" in FAT16 means each entry is 16 bits, so you can address up to ~65,536 clusters. That's why FAT16 has a maximum volume size (about 2GB with 32KB clusters).

## 4. Root Directory -- "The Filing Cabinet"

The root directory is a fixed-size table of **32-byte entries**, one per file. Each entry contains:

```
Bytes 0-7:    Filename (8 chars, space-padded)    "HELLO   "
Bytes 8-10:   Extension (3 chars, space-padded)   "TXT"
Byte  11:     Attributes (read-only, hidden, directory, etc.)
Bytes 26-27:  First cluster number (little-endian)
Bytes 28-31:  File size in bytes (little-endian)
```

This is the "8.3 filename" format -- 8 characters for name, 3 for extension. `HELLO.TXT` is stored as `"HELLO   TXT"`.

Special first bytes:
- `0x00` -- entry is empty and all following entries are empty (stop scanning)
- `0xE5` -- entry was deleted (skip it, but the slot can be reused)

## Putting It All Together: Reading a File

To read `HELLO.TXT`:

1. **Read sector 0** -- parse the BPB to learn filesystem geometry
2. **Scan the root directory** -- read directory sectors, check each 32-byte entry until you find one where name+ext matches `"HELLO   TXT"`
3. **Get the starting cluster** -- from bytes 26-27 of the directory entry (say it's cluster 4)
4. **Follow the FAT chain** -- read FAT[4] to get next cluster, then FAT[next], etc., until you hit `0xFFF8+`
5. **Read each cluster's sectors** -- convert cluster numbers to sector numbers using the BPB geometry, read the sectors, concatenate the data
6. **Truncate to file size** -- the last cluster may be only partially used; the directory entry's file size tells you exactly how many bytes are valid

## Why FAT16 for EASE?

- It's genuinely simple -- the whole filesystem fits in your head
- No complex tree structures (unlike ext4, NTFS)
- Host tools (`mkfs.fat`, `mtools`) make it easy to create and inspect images
- The Pico 2's SD card will likely use FAT as well (SD cards are commonly FAT-formatted)
