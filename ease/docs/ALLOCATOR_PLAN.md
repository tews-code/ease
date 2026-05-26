# Allocator Evolution Plan

**Goal:** Progressively improve the EASE memory allocator from a bump allocator
to a free-list allocator with coalescing, building understanding at each step
through measurement and observation.

**Approach:** Each stage is a working, tested, profiled system. We never change
the allocator "because someone said to" — we change it because the profiling
data from the previous stage shows a concrete problem. The `GlobalAlloc` trait
interface stays the same throughout, so no code outside `kernel/alloc.rs` ever
needs to change.

**Current state:** `src/kernel/alloc.rs` contains a bump allocator behind an
`IrqSpinLock`. The heap is 256KB (set in `memory-qemu.x`). `dealloc` is a
no-op. Every `Vec`, `String`, and `Box` allocation is permanent. Cold review
item #1 identified this as the highest-priority architecture issue.

**Resolves:** Cold review item #1 (bump allocator never frees — heap will
exhaust). Stages 3-4 also prepare for Phase 10 (Doom) which requires
`malloc`/`free` via libc stubs.

---

## What Does Not Change

These invariants hold across all five stages:

- **`GlobalAlloc` trait.** The allocator implements `GlobalAlloc` with `alloc`
  and `dealloc`. No caller code changes. `Vec`, `String`, `Box`, and the `alloc`
  crate work identically at every stage.
- **`#[global_allocator]` static.** The single global allocator instance stays
  in `src/kernel/alloc.rs`.
- **`IrqSpinLock` protection.** The allocator inner state is protected by
  `IrqSpinLock` so it is safe to call from interrupt context (though we avoid
  doing so).
- **`kernel::alloc::init()` entry point.** Called once from `kernel_init()` in
  `main.rs`.
- **Heap region.** Defined by `__heap_start` and `__heap_end` linker symbols
  (currently 256KB in `memory-qemu.x`).
- **`heap` shell command.** Added in Stage 1, works across all subsequent stages
  for apples-to-apples comparison.

---

## Stage 1: Instrument the Bump Allocator

**Motivation:** Before changing anything, measure what we have. "How fast does
the heap fill up?" is currently unknowable without instrumentation.

### What to Implement

Add tracking fields to `BumpAllocatorInner`:

- `total_bytes_allocated: usize` — cumulative bytes requested via `alloc`
  (not counting alignment padding)
- `total_bytes_with_padding: usize` — cumulative bytes consumed including
  alignment (this is what the bump pointer actually advances by)
- `allocation_count: usize` — number of `alloc` calls
- `largest_allocation: usize` — largest single `layout.size()` seen
- `peak_usage: usize` — high-water mark of `next - heap_start`

Add public methods to `BumpAllocator`:

- `stats(&self) -> HeapStats` — returns a snapshot struct with all the above,
  plus `heap_size` (total heap), `used` (current `next - heap_start`), and
  `free` (remaining)

Add a `HeapStats` struct (derive `Debug`, `Clone`, `Copy`) that carries these
values. This struct will be reused across all stages.

Add a `heap` shell command in `commands.rs`:

```text
moss> heap
Heap: 256 KB
Used: 12,480 / 262,144 bytes (4%)
Free: 249,664 bytes
Allocations: 47
Largest: 8,192 bytes
Peak: 12,480 bytes
```

### What to Test

- **Host-compatible unit test:** Create a standalone test of `HeapStats`
  formatting (if you extract a `Display` impl).
- **QEMU test:** After `kernel_init()`, call `stats()` and assert
  `allocation_count > 0` (kernel startup allocates things like the `Volume`).
- **QEMU test:** Assert `used <= heap_size`.
- **QEMU test:** Assert `largest_allocation > 0`.

### What to Measure / Profile

Run a manual session in the shell (or script it):

1. Boot, run `heap`. Record baseline.
2. Run `cat HELLO.TXT`, then `heap`. Note the increase.
3. Run `cat BIG.TXT` (create a ~64KB file in mkdisk.sh), then `heap`. Note the
   large jump.
4. Run `cat BIG.TXT` five more times. Watch the heap climb.
5. Run `ls`, `hexdump -C HELLO.TXT`, `write TEST.TXT hello`, `rm TEST.TXT`.
6. Final `heap`. Calculate how many more `cat BIG.TXT` commands before OOM.

Document the numbers. This is the evidence that motivates Stage 2.

### Questions to Discover

- How many bytes does `cat BIG.TXT` consume? (Answer: at least 65,536 for the
  `Vec<u8>` from `read_file`, plus `Vec` growth overhead, plus alignment.)
- How many `cat BIG.TXT` calls before the 256KB heap is exhausted? (Answer:
  roughly 3-4.)
- Do small commands (`ls`, `echo`, `time`) also leak? (Answer: `ls` uses a
  callback pattern and does not allocate a `Vec` for entries, but `cat` and
  `hexdump` do via `read_file`.)
- Where do the boot-time allocations come from?

### What Does Not Change

Nothing outside `kernel/alloc.rs` and `shell/commands.rs` changes. The
allocator algorithm is still bump. We only add counters.

### Cold Review Items Resolved

None yet — this stage is measurement only.

---

## Stage 2: Bump + Arena Reset (Watermark)

**Motivation:** Stage 1's numbers show that transient allocations (file content
buffers from `cat`, formatting strings) permanently consume heap. But all these
allocations have a clear lifetime: they live only for the duration of one shell
command. If we could reset the bump pointer after each command, those bytes
would be reclaimed.

### What to Implement

Add a watermark concept to `BumpAllocatorInner`:

- `watermark: usize` — a saved position of the bump pointer

Add methods to `BumpAllocator`:

- `set_watermark(&self)` — saves the current `next` as `watermark`
- `reset_to_watermark(&self)` — restores `next` to `watermark`, effectively
  freeing everything allocated since the watermark was set
- Update the `stats()` method to also report `watermark` position and
  `since_watermark` (bytes allocated since watermark was set)

Modify the shell main loop in `Shell::run()`:

1. After `kernel_init()` completes and the shell is constructed, but before
   entering the command loop, call
   `kernel::alloc::BUMP_ALLOCATOR.set_watermark()`. At this point, all
   persistent allocations (Volume, Console state, etc.) have happened.
2. After each command completes (after `Self::execute()` returns and before
   printing the next prompt), call
   `kernel::alloc::BUMP_ALLOCATOR.reset_to_watermark()`.

Update the `heap` command output to show:

```text
moss> heap
Heap: 256 KB
Used: 4,096 / 262,144 bytes (1%)
Free: 258,048 bytes
Watermark: 4,096 bytes (persistent)
Since watermark: 0 bytes (transient, will be reclaimed)
Allocations: 47 (12 persistent + 35 transient)
Resets: 6
```

### What to Test

- **QEMU test:** Set watermark, allocate a `Vec`, reset to watermark. Verify
  `stats().used` returns to the watermark level.
- **QEMU test:** Verify that persistent allocations (made before watermark)
  survive a reset — read the `Volume` after a reset and confirm it still works.
- **QEMU test:** Allocate, reset, allocate again — verify the second allocation
  reuses the same address range (bump pointer was reset).

### What to Measure / Profile

Repeat the same manual session from Stage 1:

1. Boot, `heap` — note the persistent baseline after watermark is set.
2. `cat BIG.TXT`, `heap` — note transient usage.
3. After the command completes and prompt returns, `heap` — transient usage
   should be back to zero.
4. `cat BIG.TXT` ten times. `heap` — used bytes should be the same as after
   one call, because each reset reclaims the transient allocation.

Compare these numbers to Stage 1's numbers. The improvement should be dramatic.

### Questions to Discover

- What if a command needs to create something that persists across commands?
  For example, if we later add a `mount` command that opens a second volume, or
  a `set` command that stores a shell variable. These allocations would be wiped
  by the reset. The watermark model only works when "persistent" and "transient"
  are strictly separated by time (all persistent allocations happen before the
  watermark is set).
- What happens if a `Drop` impl runs during reset? Answer: it does not. The
  watermark reset just moves the pointer — no destructors run. This means any
  resources held by the dropped allocations (e.g., file handles if we had them)
  would leak. For now this is fine because EASE does not have file handles that
  need closing, but it is a real limitation.
- Can we still run out of memory within a single command? Yes. `cat` on a very
  large file would still try to allocate the full file size and may fail. The
  watermark does not increase the heap size, only enables reuse between commands.

### What Does Not Change

The `GlobalAlloc::dealloc` is still a no-op. The `alloc` and `dealloc` trait
methods are unchanged. Only the shell loop and the allocator's internal state
management change.

### Cold Review Items Resolved

- **#1 (partially):** Heap no longer exhausts under normal interactive use.
  Repeated `cat` calls work indefinitely. But `dealloc` is still a no-op, so
  any code that relies on `Box::drop` or `Vec::drop` actually freeing memory
  within a single command's lifetime still wastes heap until the next reset.

---

## Stage 3: Free-List Allocator

**Motivation:** Stage 2's watermark approach works for the shell's
"allocate-during-command, reclaim-after-command" pattern. But it breaks down
when:

- An allocation needs to survive across commands but was not made at boot time.
- Multiple allocations within a command have different lifetimes (e.g., build a
  list, drop some elements, build more).
- Phase 10 (Doom) requires `malloc`/`free` semantics for its C code via libc
  stubs. Doom allocates and frees memory continuously during gameplay — there
  is no "reset point."

A real `dealloc` is needed. The simplest approach: a linked-list free-list
allocator.

### What to Implement

Replace the bump allocator internals with a free-list allocator. The outer
structure stays the same: `BumpAllocator` (consider renaming to `HeapAllocator`)
wrapping an `IrqSpinLock<AllocatorInner>`, implementing `GlobalAlloc`.

**Data structure:**

Each free block is stored as a node in a singly-linked list. The node is stored
in-place in the free memory itself (the free memory is big enough to hold the
node because the minimum allocation size is `size_of::<FreeNode>()`):

```
struct FreeNode {
    size: usize,          // total size of this free block (including the node)
    next: *mut FreeNode,  // pointer to next free block, or null
}
```

`FreeNode` is 8 bytes on RV32 (two `usize` fields). This means the minimum
allocation size is 8 bytes — any request smaller than 8 bytes is rounded up.

**`AllocatorInner` fields:**

- `free_list_head: *mut FreeNode` — head of the free list
- `heap_start: usize`
- `heap_end: usize`
- Stats fields (carried over from Stage 1):
  - `total_allocated: usize`
  - `allocation_count: usize`
  - `deallocation_count: usize`
  - `largest_allocation: usize`
  - `peak_usage: usize` (tracked as `heap_size - total_free`)
  - `free_block_count: usize` — number of blocks in the free list
  - `total_free: usize` — sum of all free block sizes

**`alloc` algorithm (first-fit):**

1. Walk the free list looking for the first block with `size >= requested_size`
   (after adjusting for alignment).
2. If the block is significantly larger than needed (e.g., remainder >=
   `size_of::<FreeNode>()` + some minimum), split it: carve off the requested
   amount and leave the remainder as a smaller free block.
3. If the block is close enough in size, use the whole thing (avoid tiny
   unusable fragments).
4. Return the allocated pointer.
5. If no block is large enough, return `null_mut()`.

**`dealloc` algorithm:**

1. Create a `FreeNode` at the freed pointer's address.
2. Insert it into the free list. Keep the list sorted by address (this makes
   coalescing in Stage 4 straightforward).
3. Update stats.

**Initialization:**

At `init()`, create a single `FreeNode` spanning the entire heap. The free list
starts as one large block.

**Remove the watermark mechanism** from Stage 2. It is no longer needed because
`dealloc` actually frees memory now. The shell loop no longer needs to call
`reset_to_watermark`.

**Rename** the struct from `BumpAllocator` to `Allocator` (or `HeapAllocator`).
Update the `#[global_allocator]` static name accordingly. This is optional but
improves clarity.

### What to Test

- **QEMU test:** Allocate a `Box<u64>`, drop it, allocate another `Box<u64>`.
  The second allocation should succeed and reuse the freed memory. Verify
  `stats().used` does not grow.
- **QEMU test:** Allocate a `Vec`, push 100 items, drop the `Vec`. Verify
  `stats().deallocation_count` increases and `stats().free` increases.
- **QEMU test:** Allocate until OOM (fill the heap), then free everything.
  Verify `stats().free` returns to approximately the full heap size (minus
  overhead).
- **QEMU test:** Stress test — in a loop, allocate a `Vec<u8>` of 1024 bytes,
  drop it, repeat 1000 times. Verify `stats().used` stays bounded (does not
  grow).
- **QEMU test:** Mixed sizes — allocate boxes of varying sizes (8, 64, 512,
  4096 bytes), free them in random order, verify no crash and free list is
  consistent.
- **Benchmark:** Compare `alloc` speed to the bump allocator baseline from
  Stage 1. The free-list allocator will be slower because it must walk the list.
  Record the regression.

### What to Measure / Profile

Update the `heap` command to show fragmentation info:

```text
moss> heap
Heap: 256 KB
Used: 12,480 / 262,144 bytes (4%)
Free: 249,664 bytes in 3 blocks
  Largest free block: 248,000 bytes
  Smallest free block: 128 bytes
Allocations: 47 (41 freed)
Largest: 8,192 bytes
Peak: 78,000 bytes
```

Run the same manual session:

1. Boot, `heap`. Note the single large free block.
2. `cat BIG.TXT`, `heap`. Note that after the command completes, the `Vec` has
   been dropped and memory returned.
3. Run `cat BIG.TXT` 20 times, `heap`. Memory usage should stay stable.
4. Run a mixed workload: `ls`, `cat HELLO.TXT`, `touch A.TXT`,
   `write A.TXT hello`, `cat A.TXT`, `hexdump -C A.TXT`, `rm A.TXT`. Check
   `heap` — how many free blocks are there? Is fragmentation starting?

### Questions to Discover

- After many allocate/free cycles of different sizes, how many free blocks
  are there? If you see the free block count growing over time, that is
  fragmentation. Two adjacent free blocks that could be one larger block are
  being kept separate — this is what motivates Stage 4.
- What is the allocation speed impact? The bump allocator was O(1). The
  free-list walk is O(n) where n is the number of free blocks. Is this
  noticeable in benchmarks?
- What happens when you allocate many small objects, then free every other one?
  You get a "Swiss cheese" heap with many small free blocks and no large ones.
  A subsequent large allocation fails even though total free bytes is sufficient.
  This is external fragmentation.

### What Does Not Change

`GlobalAlloc` trait interface. Shell commands. File system code. Everything
outside `kernel/alloc.rs` (plus the `heap` command in `commands.rs`) is
untouched.

### Cold Review Items Resolved

- **#1 (fully):** `dealloc` now returns memory. Heap exhaustion under normal
  use is eliminated. Repeated `cat` calls, `Vec` creation/destruction, and
  `String` formatting all properly free memory.
- Prepares for **Phase 10 (Doom):** The `malloc`/`free` libc stubs can now
  wrap `GlobalAlloc::alloc` and `GlobalAlloc::dealloc` and get real memory
  management.

---

## Stage 4: Free-List + Coalescing

**Motivation:** Stage 3's profiling shows fragmentation. After many
allocate/free cycles, the free list has many small blocks. A large allocation
may fail even when total free bytes is sufficient, because no single contiguous
block is large enough.

### What to Implement

Modify `dealloc` to coalesce adjacent free blocks:

**Coalescing algorithm (on `dealloc`):**

1. Insert the freed block into the free list at the correct position (list is
   sorted by address).
2. Check if the freed block is adjacent to the **next** block in the list. If
   `freed_end == next_block_start`, merge them: increase the freed block's size
   by the next block's size, update the pointer to skip the next block.
3. Check if the freed block is adjacent to the **previous** block in the list.
   If `prev_block_end == freed_start`, merge them: increase the previous block's
   size by the freed block's size, update the pointer to skip the freed block.
4. Both merges can happen (previous + freed + next become one block).

**New stats:**

- `coalesce_count: usize` — number of times a coalesce happened
- Keep all existing stats from Stage 3.

Update `heap` command:

```text
moss> heap
Heap: 256 KB
Used: 12,480 / 262,144 bytes (4%)
Free: 249,664 bytes in 1 block
  Largest free block: 249,664 bytes
Allocations: 147 (141 freed, 23 coalesced)
Peak: 78,000 bytes
```

### What to Test

- **QEMU test:** Allocate three adjacent blocks A, B, C. Free B, then free A.
  Verify that A and B are coalesced into one free block (check
  `stats().free_block_count`).
- **QEMU test:** Free C after the above. Verify all three coalesce into one
  block.
- **QEMU test:** Allocate many small blocks, free all of them. Verify the free
  list returns to a single block spanning the entire heap (minus persistent
  allocations).
- **QEMU test:** Allocate blocks of size 64, free every other one (creating
  gaps), then free the rest. Verify full coalescing.
- **QEMU test:** Stress test — 1000 iterations of allocate-various-sizes,
  free-all, check free block count is 1 (or a small number accounting for
  persistent allocations).
- **Benchmark:** Measure `dealloc` speed. Coalescing adds work to the free
  path. Compare to Stage 3's dealloc speed.

### What to Measure / Profile

Run the same mixed workload from Stage 3. Compare:

- Free block count after the workload. Stage 3 might show 5-10 blocks; Stage 4
  should show 1-2.
- Largest free block. Stage 3 might show a fragmented heap where the largest
  block is smaller than total free. Stage 4 should show the largest block close
  to total free.
- The "Swiss cheese" test: allocate 100 blocks of 128 bytes, free every other
  one, then try to allocate 6400 bytes (the total free of the 50 freed blocks).
  In Stage 3 this fails. In Stage 4, the freed blocks are not adjacent (there
  are still-allocated blocks between them), so it still fails. This is an
  important lesson: coalescing helps when adjacent blocks are freed, but cannot
  fix interleaved allocation patterns.

### Questions to Discover

- Is fragmentation fully solved? No. Coalescing helps when adjacent blocks are
  freed but cannot merge non-adjacent free regions. For EASE's workload
  (shell commands that allocate then free everything), coalescing is very
  effective because the free pattern tends to be "free everything in reverse
  order." For Doom's more complex allocation patterns, some fragmentation will
  remain. This is acceptable.
- Would a different strategy (best-fit, buddy allocator) help? Perhaps, but
  the complexity is not justified for EASE's workload. First-fit with
  coalescing is the sweet spot for simplicity and correctness.
- Is this allocator fast enough for Doom? Measure the allocation hot path.
  If the free list grows long (many fragments), the O(n) walk becomes a
  bottleneck. Monitor `free_block_count` during Doom in Phase 10. If it does
  become a problem, consider a size-bucketed free list as a future optimisation.

### Structural Preparation for Stage 5

In this stage, structure the allocator so that the free-list logic is
region-agnostic:

- Extract a `FreeList` struct that manages a single contiguous memory region.
  It holds the head pointer, does first-fit allocation, sorted-insert
  deallocation, and coalescing.
- `AllocatorInner` holds one `FreeList` (for SRAM).
- In Stage 5, `AllocatorInner` will hold two: `sram: FreeList` and
  `psram: FreeList`.

This costs nothing now and makes the Stage 5 extension trivial.

### What Does Not Change

Same as all previous stages — `GlobalAlloc` interface, heap region, shell
commands (other than `heap` format updates).

### Cold Review Items Resolved

- **#1 (complete):** The allocator now has real `alloc`, real `dealloc`, and
  coalescing to combat fragmentation. This is a production-quality allocator
  for an embedded OS of this scale.

---

## Stage 5: Two-Region Allocator (Deferred to Phase 14)

**Motivation:** The RP2350 has two distinct memory regions: 520KB SRAM (fast,
supports atomics) and 8MB PSRAM (slower, no atomic support). Different
allocations have different needs:

- **SRAM:** Sync structures (SpinLock, AtomicBool, SPSC queue buffers),
  interrupt handler data, small hot-path allocations, stacks.
- **PSRAM:** Large buffers (Doom WAD data, document buffers, framebuffer),
  anything that benefits from the larger address space.

### When to Implement

**Not now.** PSRAM is only available on real hardware (Phase 14). In QEMU, we
model PSRAM at address `0x81000000` but it behaves identically to SRAM (same
speed, atomics work). There is no benefit to splitting regions in QEMU, and
doing so would complicate testing.

**Design the API now** so the Stage 4 allocator can be extended later without
changing the interface.

### API Design (for future implementation)

The `GlobalAlloc` trait does not support choosing a region — it only provides
`Layout` (size + alignment). The region selection must be a policy decision
inside the allocator:

```rust
fn choose_region(layout: Layout) -> Region {
    if layout.size() > SRAM_LARGE_THRESHOLD {
        Region::Psram
    } else {
        Region::Sram
    }
}
```

Where `SRAM_LARGE_THRESHOLD` is a tunable constant (e.g., 4096 bytes). Each
region has its own free list, so allocation and deallocation are
region-independent.

**Alternatively**, provide a custom allocation API alongside `GlobalAlloc` for
code that explicitly wants a specific region:

```rust
impl Allocator {
    pub fn alloc_sram(&self, layout: Layout) -> *mut u8 { ... }
    pub fn alloc_psram(&self, layout: Layout) -> *mut u8 { ... }
}
```

The `GlobalAlloc::alloc` implementation uses the automatic policy. Explicit
region selection is available for code that knows what it needs (e.g., DMA
buffers must be in SRAM, Doom WAD must be in PSRAM).

**Key constraint:** PSRAM does not support atomic operations. This means:

- No `SpinLock`, `AtomicBool`, or `IrqSpinLock` data can live in PSRAM.
- The allocator's own free-list nodes in PSRAM must not use atomics.
- Since each region has its own free list, and the free list is protected by
  the allocator's `IrqSpinLock` (which lives in SRAM), this is fine — the
  atomic is on the lock, not on the free-list nodes.

### What to Test (when implemented in Phase 14)

- Allocate a large buffer (>threshold), verify it lands in PSRAM address range.
- Allocate a small object, verify it lands in SRAM address range.
- Verify that `dealloc` returns memory to the correct region based on the
  pointer address.
- Verify that atomic operations work on SRAM allocations and that no atomics
  are attempted on PSRAM allocations.

### Cold Review Items Resolved

- Addresses the "Atomics limitation in PSRAM" risk from the project plan's
  Key Risks table.
- Prepares for Phase 14 (PSRAM integration).

---

## Implementation Order and Commit Strategy

Each stage should be one or a small series of commits:

| Stage | Commits | Description |
|-------|---------|-------------|
| 1a | 1 | Add `HeapStats` struct and tracking fields to bump allocator |
| 1b | 1 | Add `heap` shell command |
| 1c | — | Manual profiling session (document findings in commit message or a note) |
| 2a | 1 | Add watermark save/restore to bump allocator |
| 2b | 1 | Integrate watermark reset into shell loop |
| 3a | 1 | Implement `FreeList` struct with first-fit alloc and sorted-insert dealloc |
| 3b | 1 | Replace bump allocator with free-list allocator, update stats |
| 3c | 1 | Remove watermark mechanism (no longer needed) |
| 3d | 1 | Update benchmarks and baselines |
| 4a | 1 | Add coalescing to dealloc |
| 4b | 1 | Add coalescing stats, update `heap` command |
| 4c | 1 | Stress tests for fragmentation |

**CI requirement:** `scripts/ci.sh` before every commit, as always.

---

## Benchmark Baselines

The existing benchmarks in `alloc.rs` measure allocation speed:

- `Box::new u64`: 360,000 cycles
- `Vec::push 100 items`: 2,000,000 cycles
- `String::from short`: 240,000 cycles

These baselines were measured against the bump allocator (O(1) alloc). The
free-list allocator (Stage 3+) will be slower because `alloc` walks the free
list. When transitioning to Stage 3, re-measure and update the baselines.
Document both the old and new baselines in the commit message so the cost of
real deallocation is visible.

Expect roughly 2-5x slower allocation in the free-list allocator compared to
bump (depending on free list length). This is acceptable because:

1. Allocation is not on the hot path for most shell commands (I/O dominates).
2. The bump allocator's speed was meaningless because it could not free memory.
3. For Doom (Phase 10), allocation frequency is moderate and heap is large
   enough that the free list stays short.

---

## Relationship to Other Cold Review Items

| Cold Review Item | Addressed By |
|-----------------|--------------|
| #1: Bump allocator never frees | Stages 1-4 (fully resolved at Stage 3) |
| #2: No block device trait | Not addressed (separate work) |
| #5: write_file non-atomic | Not addressed (separate work) |
| Phase 10: malloc/free stubs | Stage 3 enables real malloc/free for Doom |
| Phase 14: PSRAM integration | Stage 5 designs the two-region API |

---

*Plan created: March 2026*
*Estimated effort: ~15-20 hours across stages 1-4*
*Stage 5 is design-only until Phase 14*
