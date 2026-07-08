//! FAT32

/* FAT32 Details (refer to documentation in fat16.rs)
 *
 *
 * ┌─────────────────────────┬──────────────────────────────────────────────────────┬─────────────────────────────────────────────────────────────────────┐
 * │                         │                        FAT16                         │                                FAT32                                │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ FAT entry size          │ 2 bytes, all 16 bits used                            │ 4 bytes, only low 28 bits are the cluster number                    │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ What makes it that type │ 4,085–65,524 clusters                                │ 65,525+ clusters                                                    │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ Free marker             │ 0x0000                                               │ 0x00000000                                                          │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ End-of-chain            │ ≥ 0xFFF8                                             │ ≥ 0x0FFFFFF8 (after masking)                                        │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ Bad cluster             │ 0xFFF7                                               │ 0x0FFFFFF7                                                          │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ Sectors-per-FAT field   │ offset 22, 16-bit                                    │ offset 36, 32-bit (offset 22 must read 0)                           │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ Root directory          │ fixed region after the FATs, size from BPB offset 17 │ ordinary cluster chain; start cluster at BPB offset 44              │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ Directory entry format  │ identical 32 bytes                                   │ identical, but the high-cluster word at entry offset 20 is now used │
 * ├─────────────────────────┼──────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
 * │ Practical volume size   │ up to ~2 GB                                          │ up to 2 TB                                                          │
 * └─────────────────────────┴──────────────────────────────────────────────────────┴─────────────────────────────────────────────────────────────────────┘
 *
 * Note:
 * 1. The top 4 bits of a FAT32 entry need to be masked off when reading and preserved when writing
 * 2. There is an FSInfo sector (a hint cache of the free-cluster count, so mounting doesn't scan the whole FAT)
 * 3. There is a backup copy of each volume's boot sector at sector 6. The backup is actually three sectors — volume sectors 0–2 copied to 6–8 — because the boot sector, FSInfo, and a spillover sector form a unit. The BPB even records where the backup sits (offset 50, conventionally 6),'
 *
 * Here's the reserved region of a FAT32 volume — everything before the first FAT, drawn volume-relative (a typical format reserves 32 sectors, so FAT 0 starts at sector 32):
 *
 * Sector: 0          1          2         3–5       6          7          8         9–3*1
 * ┌────────────┬──────────┬────────────┬────────┬────────────┬──────────┬────────────┬────────┐
 * │ Boot       │ FSInfo   │ Boot code  │ unused │ backup     │ backup   │ backup     │ unused │
 * │ sector     │          │ spillover  │        │ of 0       │ of 1     │ of 2       │        │
 * └────────────┴──────────┴────────────┴────────┴────────────┴──────────┴────────────┴────────┘
 * └────── the working set ─────────┘└────── copy written at format time ─────────┘
 *
 *  FsInfo
 *
 * ┌────────┬──────┬───────────────────────────────────────────────┐
 * │ Offset │ Size │                   Contents                    │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 0      │ 4    │ Lead signature 0x41615252 ("RRaA")            │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 4      │ 480  │ Reserved, zeros                               │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 484    │ 4    │ Second signature 0x61417272 ("rrAa")          │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 488    │ 4    │ Free cluster count (0xFFFFFFFF = unknown)     │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 492    │ 4    │ Next-free-cluster hint (0xFFFFFFFF = unknown) │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 496    │ 12   │ Reserved, zeros                               │
 * ├────────┼──────┼───────────────────────────────────────────────┤
 * │ 508    │ 4    │ Trail signature 0xAA550000                    │
 * └────────┴──────┴───────────────────────────────────────────────┘
 */
