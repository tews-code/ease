//! Filesystem benchmarks.
//!
//! The meaningful performance metric for a filesystem is the number of
//! block-I/O operations, not CPU cycles: each `read_block`/`write_block` is a
//! virtio round-trip that dwarfs the surrounding CPU work, and the count is
//! deterministic — immune to QEMU timing. These benchmarks assert on block-op
//! counts, so a regression (e.g. a broken FAT sector cache) shows up as an
//! op-count spike rather than as noise. CPU cycles are printed alongside as
//! informational only.
//!
//! Kernel-only: they drive the real virtio disk through `with_volume`.
#![cfg(all(test, target_os = "none", feature = "bench"))]

use crate::bench;
use crate::drivers::virtio::blk::bench_counters;
use crate::fs::Dir;
use crate::fs::file::{self, Access};
use crate::fs::volume::with_volume;

// Upper bounds on block ops per operation. Reads/writes are deterministic on a
// given disk image, so these are tight enough to catch a cache or read/write
// amplification regression while leaving ~12-15% headroom for layout shifts.
// Read and write differ by FAT type (cluster size / FAT layout differ), so
// those bounds are per-format; the active one is selected by the fat16/fat32
// feature (ci.sh passes it to the benchmark stage alongside `bench`).
// Measured (QEMU virt, streaming read_at/write_at):
//   open reads: fat32=2  fat16=1
//   read_at(64KB): fat32=129 fat16=160
//   allocate_cluster: fat32=122 fat16=15
//   write_at(8KB) writes: fat32=96 fat16=45  (write amplification — write_at
//     re-zeroes each new cluster; see #4)
const OPEN_MAX_READS: u32 = 4;
const READ_BIG_MAX_READS_FAT16: u32 = 180;
const READ_BIG_MAX_READS_FAT32: u32 = 140;
const ALLOC_MAX_READS_FAT16: u32 = 30;
const ALLOC_MAX_READS_FAT32: u32 = 130;
const WRITE_MAX_WRITES_FAT16: u32 = 52;
const WRITE_MAX_WRITES_FAT32: u32 = 110;

/// Streaming buffer size for the write benchmark. write_at is write-through per
/// sector, so this directly affects the write count; one sector keeps a
/// partially-filled sector from being re-written on every call.
const WRITE_BUF_LEN: usize = 512;

/// Run `f`, returning (block reads, block writes, cpu cycles) attributed to it.
fn count<F: FnOnce()>(f: F) -> (u32, u32, u64) {
    bench_counters::reset();
    let cycles = bench::measure(f);
    let (reads, writes) = bench_counters::snapshot();
    (reads, writes, cycles.cpu)
}

fn report(name: &str, reads: u32, writes: u32, cpu: u64) {
    println!("  {name}: reads={reads} writes={writes} (cpu={cpu} cycles)");
}

#[test_case]
fn fs_block_io_benchmarks() {
    println!();
    println!("====== FILESYSTEM (block I/O) ======");
    println!();

    let current_dir = Dir::Root;
    // 1. Metadata lookup: scan the root directory for a file by name.
    let (reads, writes, cpu) = count(|| {
        // file::open takes the volume lock internally, so no with_volume here.
        let entry = file::open(Access::Read, current_dir, "HELLO.TXT").unwrap();
        let _ = entry.close();
    });
    report("open(HELLO.TXT)", reads, writes, cpu);
    assert_eq!(writes, 0, "open should not write");
    assert!(
        reads <= OPEN_MAX_READS,
        "open reads regressed: {reads} > {OPEN_MAX_READS}"
    );

    // 2. Sequential read of a 64 KB multi-cluster file. Ideal is ~1 sector read
    //    per 512 bytes of payload (128 data reads) plus a small, cache-amortised
    //    number of FAT-sector reads.
    let (reads, writes, cpu) = count(|| {
        let mut entry = file::open(Access::Read, current_dir, "BIG.TXT").unwrap();
        // Stream the whole file through read_at into a one-sector buffer.
        let mut buf = [0u8; 512];
        while file::read_at(&mut entry, &mut buf).unwrap() != 0 {}
        let _ = entry.close();
    });
    report("read_at(BIG.TXT, 64KB)", reads, writes, cpu);
    #[cfg(feature = "fat16")]
    assert!(
        reads <= READ_BIG_MAX_READS_FAT16,
        "read reads regressed (amplification/cache?): {reads} > {READ_BIG_MAX_READS_FAT16}"
    );
    #[cfg(feature = "fat32")]
    assert!(
        reads <= READ_BIG_MAX_READS_FAT32,
        "read reads regressed (amplification/cache?): {reads} > {READ_BIG_MAX_READS_FAT32}"
    );

    // 3. FAT free-cluster scan. `allocate_cluster` walks the FAT looking for a
    //    free entry; on this image the first free cluster sits past the ~7.9 MB
    //    PDF, so the scan covers thousands of clusters. With the one-sector FAT
    //    cache that costs ~ceil(clusters / 256) sector reads; without it, one
    //    read per cluster. This is the guard on that cache — the only
    //    optimisation currently in the FS.
    let (reads, writes, cpu) = count(|| {
        with_volume(|vol| {
            vol.allocate_cluster().unwrap();
        });
    });
    report("allocate_cluster (FAT scan)", reads, writes, cpu);
    #[cfg(feature = "fat16")]
    assert!(
        reads <= ALLOC_MAX_READS_FAT16,
        "FAT-scan reads regressed (cache broken?): {reads} > {ALLOC_MAX_READS_FAT16}"
    );
    #[cfg(feature = "fat32")]
    assert!(
        reads <= ALLOC_MAX_READS_FAT32,
        "FAT-scan reads regressed (cache broken?): {reads} > {ALLOC_MAX_READS_FAT32}"
    );

    // 4. Streaming write of a multi-cluster file: create + truncate + stream the
    //    buffer through write_at + close (commit). Gates on writes (the mutation
    //    cost); reads are dominated by the allocation scans covered by #3.
    //    Note: write_at is write-through per sector, so a smaller WRITE_BUF_LEN
    //    re-writes a partially-filled sector on each call — the write count is
    //    sensitive to this buffer size, not just the payload.
    let data = [b'Z'; 8 * 1024];
    let (reads, writes, cpu) = count(|| {
        file::touch(current_dir, "BENCH.TMP").unwrap();
        file::truncate(current_dir, "BENCH.TMP").unwrap();
        let mut handle = file::open(Access::Write, current_dir, "BENCH.TMP").unwrap();
        for chunk in data.chunks(WRITE_BUF_LEN) {
            file::write_at(&mut handle, chunk).unwrap();
        }
        handle.close().unwrap();
    });
    report("write_at(BENCH.TMP, 8KB)", reads, writes, cpu);
    #[cfg(feature = "fat16")]
    assert!(
        writes <= WRITE_MAX_WRITES_FAT16,
        "write writes regressed: {writes} > {WRITE_MAX_WRITES_FAT16}"
    );
    #[cfg(feature = "fat32")]
    assert!(
        writes <= WRITE_MAX_WRITES_FAT32,
        "write writes regressed: {writes} > {WRITE_MAX_WRITES_FAT32}"
    );

    println!();
}
