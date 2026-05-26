# EASE Cold Code Review

Critical review of the EASE codebase for design, architectural, and idiomatic
faults. Performed 2026-03-22 at ~6.8k lines of Rust.

## Architecture

### 1. Bump allocator never frees — heap will exhaust

`kernel/alloc.rs:55` — `dealloc` is a no-op. Every `Vec`, `String`, or `Box`
allocation is permanent. This is especially problematic in `volume.rs:124`
where `read_file` returns `Vec<u8>` — every `cat` command permanently consumes
heap. Run `cat BIG.TXT` a few times and you're out of memory.

At minimum, document this prominently. Long-term, consider a free-list or slab
allocator, or at least a "reset the bump pointer" mechanism between shell
commands.

### 2. No block device abstraction — filesystem hardcoded to VirtIO

`fs/volume.rs` directly calls `crate::drivers::virtio::{read_block,
write_block}`. When you port to RP2350 with an SD card, every filesystem
function must change. A `BlockDevice` trait in `hal/` would make the filesystem
portable:

```rust
pub trait BlockDevice {
    fn read(&self, block: u32, buf: &mut [u8; 512]) -> Result<(), BlkError>;
    fn write(&self, block: u32, buf: &[u8; 512]) -> Result<(), BlkError>;
}
```

### 3. Console dual-output is scattered and inconsistent

UART echoing is ad-hoc and duplicated across multiple methods:

- `Console::put_char` (line 215): calls `direct_write_byte`
- `Console::write_str` (lines 280-284): calls `direct_write_byte` per byte
- `Shell::run` (line 86): separately echoes the last typed byte
- `Shell::uart_redraw_line`: separately redraws on UART

This creates bugs. For example, `put_char` echoes to UART but `process_char`
doesn't — so the choice of which to call determines whether UART gets output.
This should be centralised in one output dispatch layer.

### 4. Large code duplication in volume.rs

`create_empty_file` (lines 158-183) and `write_file` (lines 284-313) contain
nearly identical "find free root directory slot" loops. Extract a
`find_free_dir_slot` helper that returns `(sector, offset)`.

Similarly, `find_dir_entry_location` (lines 190-213) duplicates the root
directory scanning loop from `read_root_dir` — it could use `read_root_dir`
internally.

## Design / Correctness

### 5. `write_file` is non-atomic — data loss on failure

`volume.rs:256` — `write_file` deletes the old file first, then writes the new
one. If power is lost (or the write fails) between delete and completion of the
new write, the file is gone. The clusters for the new data are allocated but
the directory entry may not exist yet. A safer approach: write new data to new
clusters, create a new directory entry, *then* free the old clusters.

### 6. `SpinLock::try_lock` uses `compare_exchange_weak` — spurious failures

`sync.rs:170` — `compare_exchange_weak` can spuriously fail on some
architectures, meaning `try_lock()` may return `None` even when the lock is
free. Since callers of `try_lock` expect the only failure reason to be "someone
else holds it", use `compare_exchange` (strong) instead.

### 7. `SpscRingBuf::push` over-synchronises

`collection.rs:287` — The producer loads its own `head` with `Acquire`, but
it's the only writer of `head`. `Relaxed` is sufficient. Only `tail` (written
by the consumer) needs `Acquire`. Same issue in `pop` reading its own `tail`.
This isn't a bug, just unnecessary overhead on the interrupt-driven UART path.

### 8. VirtIO `req` field has aliasing concerns

`drivers/virtio/mod.rs` — `VirtioBlkReq` is accessed via
`write_volatile`/`read_volatile` through `&mut self`, but the device DMA also
reads/writes the same memory concurrently (between submit and completion). The
`&mut` reference technically grants exclusive access under Rust's aliasing
model. The DMA-accessible fields (`data`, `status`, `sector`, `req_type`)
should be wrapped in `UnsafeCell` to signal that they have shared mutability.

### 9. `mmio::read32`/`write32` assert alignment on every call

`arch/mmio.rs:28,35` — These `assert_eq!` checks run in hot paths like the
UART interrupt handler and timer handler. Since MMIO addresses are compile-time
constants (base + constant offset), these should be `debug_assert!` so they
compile away in release mode.

### 10. Lock ordering is undocumented

The codebase has a three-level lock hierarchy: `VOLUME` (SpinLock) →
`IO_IN_PROGRESS` (SpinLock) → `BLK_DEV` (IrqSpinLock). This ordering is
correct and consistent, but nothing documents it. A comment at the top of
`volume.rs` or `virtio/mod.rs` should record the required lock ordering to
prevent future deadlocks.

### 11. `test_io::capture` has a data race

`io.rs:77` — `capture` is called from both the main thread (via `print!`) and
from interrupt context (UART TX handler calls `direct_write_byte` which calls
`capture`). While the index is atomic, the subsequent write to `UnsafeCell`
constitutes a data race under the Rust memory model. The "single-threaded"
safety comment is incorrect when interrupts are involved.

## Idiomatic Rust

### 12. Doc comments reference wrong type name

`collection.rs:24` — The doc says `Vec<u8, 256> = Vec::new()` but the type is
`StackVec`. Same at line 63. These are copy-paste artefacts that will confuse
readers.

### 13. `CriticalSection` is dead code

`sync.rs:14` — `CriticalSection` and `with_interrupts_disabled` are defined,
marked `#[allow(dead_code)]`, and never used. The actual interrupt management
is done through `IrqSpinLock`. Either use this abstraction (it's a good one) or
remove it.

### 14. `lib.rs` is a hollow shell

`lib.rs` has placeholder modules commented out and a single placeholder test.
All real code lives in `main.rs` as private modules. This means nothing is
unit-testable from the lib crate. The integration test file
`tests/lib_tests.rs` can't test anything. Consider moving shared kernel code
into the lib crate.

### 15. `DirEntry::filename()` panics on corrupt data

`dir_entry.rs:57-61` — `str::from_utf8(&self.filename).expect("should be
UTF-8")` will panic on a corrupt filesystem. Since `FsError` already exists,
return `Result<StackVec<u8, 12>, FsError>` instead.

### 16. Magic numbers throughout fs code

Examples:

- `volume.rs:174`: `0x20` (archive attribute)
- `volume.rs:224`: raw byte offsets 26-27 for cluster, 28-31 for size
- `dir_entry.rs:42`: `0x0F` (LFN), `0x08` (volume label)

These should be named constants in `dir_entry.rs`, e.g.
`const ATTR_ARCHIVE: u8 = 0x20`.

### 17. Wildcard match hides future `RenderCommand` variants

`console.rs:203` — `_ => {}` silently ignores `DrawCursor` in `process_char`.
If you add a new `RenderCommand` variant, the compiler won't warn you. Use an
explicit `RenderCommand::DrawCursor(..) => {}` arm.

### 18. Inconsistent test module naming

Some files use `mod tests`, others `mod test`, others `mod benchmarks`. Pick
one convention (`mod tests` is idiomatic Rust) and apply it uniformly.

### 19. `with_foo` pattern is repeated and could be generic

`with_clint`, `with_blk_dev`, `with_volume` all follow the identical pattern
of `static LOCK<Option<T>>` + `.as_mut().expect(...)`. A generic
`StaticResource<T, L>` wrapper would eliminate this boilerplate and centralise
the "not initialised" panic message.

## `ls` Performance Analysis

The `ls` command is visibly slow when printing filenames. Root cause analysis:

**Lock chain per character printed:**

```
VOLUME (SpinLock) ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ (entire ls)
  IO_IN_PROGRESS (SpinLock) ━━━━━━━━━━  (per sector read)
    BLK_DEV (IrqSpinLock) ━━━            (submit, IRQ disabled)
    [wait for virtio interrupt]
    BLK_DEV (IrqSpinLock) ━━━            (finish, IRQ disabled)
  [parse dir entries]
  [direct_write_byte × N]               ← busy-waits UART per byte,
                                           still holding VOLUME
```

**Two things make this slow:**

1. **`direct_write_byte` is synchronous/polling** — it waits for each byte to
   physically transmit on the UART wire before returning.
   `Console::write_str` explicitly uses `direct_write_byte` for UART output.
   At 115200 baud each byte takes ~87 us. For a filename like `"HELLO.TXT  "`
   that's ~11 bytes x 87 us = ~1 ms per filename just waiting for UART.
   The interrupt-driven `UartWriter` (used by `print!`/`println!`) would be
   faster since it pushes to `TX_BUF` and returns, letting the THRE interrupt
   drain asynchronously.

2. **Everything happens under `VOLUME` lock** — even though the volume isn't
   needed during console output. The `with_volume` closure in `ls` wraps both
   the disk reads and the printing.

**Potential fixes:**

- Use the interrupt-driven `UartWriter` in `Console::write_str` instead of
  `direct_write_byte` — return near-instantly per byte instead of waiting
  ~87 us.
- Alternatively, collect results first, release `VOLUME`, then print — so the
  slow UART output doesn't hold the filesystem lock.

## Priority Summary

| Priority | Issue | Impact |
|----------|-------|--------|
| **High** | Bump allocator never frees (#1) | Heap exhaustion in normal use |
| **High** | `write_file` non-atomic (#5) | Data loss on failure |
| **High** | No block device trait (#2) | Blocks RP2350 port |
| **Medium** | DMA aliasing UB (#8) | Potential miscompilation |
| **Medium** | Test I/O data race (#11) | Corrupt test output |
| **Medium** | Scattered UART echoing (#3) | Bug-prone output path |
| **Medium** | Volume code duplication (#4) | Maintenance burden |
| **Medium** | `filename()` panics on corrupt FS (#15) | Kernel panic on bad disk |
| **Medium** | `ls` slow due to sync UART under lock (#20) | Visible user-facing lag |
| **Low** | Magic numbers (#16) | Readability |
| **Low** | Dead `CriticalSection` (#13) | Code clutter |
| **Low** | Over-synchronisation in SPSC (#7) | Minor perf |
| **Low** | Naming inconsistencies (#12, #18) | Polish |
