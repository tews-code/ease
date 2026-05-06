# Slab and Buddy Allocator Plan

**Goal:** Implement a slab allocator and a buddy allocator as additional
allocator implementations alongside the existing bump and freelist allocators,
selectable by feature flag. Use them to compare performance, fragmentation
behaviour, and code complexity. Build toward a layered slab-on-buddy
architecture that mirrors how production kernels (Linux, FreeBSD) organise
their memory subsystems.

**Audience and roles:** This is a learning exercise. You write all the code; this
plan is the map and I (Claude) am here to hint, review, and unblock. When you
get stuck, ask for hints first, not solutions.

**Prerequisites:** Bump and freelist allocators implemented (`alloc-bump`, `
alloc-freelist` features). Familiarity with `GlobalAlloc`, `Layout`, and the
provenance lessons learned from the freelist refactor (see "Background" below).

- - -
## Why Now

Three threads converge on this plan:

1.  **The freelist is structurally hard to make Miri-clean.** Variable-size
    blocks force embedded linked-list pointers, and embedded pointers conflict
    with Stacked Borrows whenever you walk through `\&mut FreeBlock` references.
    Slab and buddy allocators sidestep this by expressing their bookkeeping as
    indices into sidecar arrays — safe Rust slice indexing — and confining the `
    unsafe` to the `GlobalAlloc` boundary.
2.  **Real kernels layer their allocators.** The canonical embedded/kernel
    architecture is buddy-at-the-bottom (page-granularity, contiguous regions)
    with slab-on-top (object-granularity, fast cached allocation). Building
    both gives you the architecture and the building blocks at once.
3.  **The freelist makes a great baseline for comparison.** Keeping it under a
    feature flag lets you measure actual differences in latency, fragmentation,
    and memory overhead between four real allocators on the same workload.

- - -
## Background: The Lesson from the Freelist

This plan is shaped by what went wrong (or rather, what Miri caught) with the
freelist. Two principles to carry forward:

**Provenance comes from the type you name.** A `\&mut FreeBlock` borrow has
permission to touch *exactly* the bytes of the `FreeBlock` struct, even if the
allocation behind it is much larger. Pointer arithmetic past the end of the
borrowed type is a Stacked Borrows violation. The fix is to never narrow:
either work in raw pointer space from a wide-provenance source, or work in
index space against an array whose slice has the wide provenance.

**Index-based bookkeeping is Miri's friend.** If your free list is `Vec\<u32>`
(or `\[u32; N]`) of indices into a slot array, every access is a bounds-checked
safe slice index. Miri sees a normal Rust program with no unsafe at all in the
bookkeeping path. The only unsafe lives at the boundary where you must turn a `
(class, index)` pair into a `\*mut u8` for the `GlobalAlloc` trait — and that
boundary is small, well-defined, and easy to audit.

The slab and buddy designs in this plan both follow that principle.

- - -
## What Does Not Change

These invariants hold across all phases of this plan:

- `GlobalAlloc`** trait.** Each new allocator implements `GlobalAlloc::alloc`
  and `GlobalAlloc::dealloc`. No caller code changes when switching allocators.
- **Feature-flag selection.** Following the existing pattern in `Cargo.toml`:
  add `alloc-slab` and `alloc-buddy` features alongside `alloc-bump` and `
  alloc-freelist`. Exactly one is enabled at a time. The `\#\[global_allocator]`
  static lives in `main.rs` and is `cfg`\-gated on the chosen feature.
- **Heap region.** `__heap_start`/`__heap_end` linker symbols continue to define
  the kernel heap. The new allocators get the same region as the freelist.
- **Host-testability.** The allocator *logic* lives in the lib crate and is
  host-testable under Miri. The `\#\[global_allocator]` instance and the
  linker-symbol coupling stay in the binary crate, behind the same scaffold the
  freelist already uses.
- `scripts/ci.sh`**.** Every commit runs the full CI including Miri host tests.
- `heap`** shell command.** Continues to report whatever stats the active
  allocator exposes. New allocators add their own stats variants but reuse the
  command surface.

- - -
## Roadmap

```
Phase A:  Single-class slab        (simplest possible, learn the pattern)
   ↓
Phase B:  Multi-class slab         (size classes, segregated free lists)
   ↓
Phase C:  Buddy allocator          (variable size via power-of-two splitting)
   ↓
Phase D:  Layered: slab on buddy   (production architecture)
   ↓
(future)  Comparison and tuning
```
Each phase produces a working, tested, profiled, Miri-clean allocator that can
be selected via feature flag. You can stop after any phase and still have a
useful artifact.

- - -
## Phase A: Single-Class Slab Allocator

**Educational goal:** Understand the simplest possible non-trivial allocator.
Internalise *why* index-based bookkeeping is structurally safer than embedded
pointers, by writing one and seeing Miri stay quiet.

**Architectural goal:** A new module `src/kernel/alloc/slab.rs` providing a slab
that hands out fixed-size slots from a contiguous storage region. One slot size
only. Hard-coded for now — choose something useful like 64 bytes.

### What to Implement

A `Slab\<const SLOT_SIZE: usize, const N: usize>` type with:

- A storage region of `N` slots, each `SLOT_SIZE` bytes. Think about how to
  represent this so the storage has wide provenance (hint: `UnsafeCell` is your
  friend, and a `\[[u8; SLOT_SIZE]; N]` array gives you exactly the layout you
  want).
- A free-list head and a sidecar `next_free` array of `u32` indices. Use `
  u32::MAX` as the "end of list" sentinel. (Why `u32` rather than `usize`? Hint:
  how many slots will you ever realistically have? What does this save?)
- An `init` method that links every slot into the free list at construction.
- An `alloc` method that pops the head and returns a `\*mut u8`. The only `unsafe`
  here should be the final `byte_add` to compute the slot's address from the
  storage base — and it should carry wide provenance from the storage cell.
- A `dealloc` method that takes a `\*mut u8`, computes the slot index from the
  pointer-base offset, and pushes the index onto the free list head.
- A `GlobalAlloc` impl that wraps the above behind your existing `IrqSpinLock`
  (or `AllocatorLock` host-testable shim). For a single slot size, `
  GlobalAlloc::alloc` should reject any request whose size or alignment exceeds
  the slot — hand back null in those cases.

### Hints, Not Solutions

- `UnsafeCell::get()` returns a `\*mut T` whose provenance covers the entire
  cell contents. That's where your wide provenance comes from. Every later
  pointer into the storage should be derived from that single `get()` call via `
  byte_add` or `add`, never via int-to-ptr.
- The `next_free` array can be `\[Cell<u32>; N]` if you want interior mutability
  without `\&mut self`, or a plain `\[u32; N]` field of the inner state if
  you're willing to take `\&mut self`. Both work; choose based on whether you
  want the inner state under a lock (`IrqSpinLock\<Inner>`) or whether you want
  lock-free atomic operations (probably not yet — start simple).
- Computing the slot index in `dealloc`: `((ptr as usize) - (base as usize)) /
  SLOT_SIZE`. Yes, you're using `as usize` here. Pointer-to-integer is fine for
  a *comparison or arithmetic* result; the rule is don't go *back* to a pointer
  through `usize`. You're using the integer as an index into a safe slice, which
  is fine.
- Think about what assertions belong at the boundary. What can you check?
  Pointer falls within the storage range? Pointer is slot-aligned? Index is in
  bounds? The earlier these fire, the easier debugging is.

### What to Test (host, Miri-verified)

In `kernel/alloc/slab.rs` itself, in a `\#\[cfg(test)]` module gated on `
\#\[cfg(not(target_os = "none"))]` so it only runs on host:

- **alloc-then-dealloc-returns-same-slot**: After one allocate and one free, the
  next allocate should return the same address.
- **fill-the-slab**: Allocate `N` slots back-to-back. Check that all returned
  pointers are distinct and within the storage region. The `(N+1)`th allocation
  should return null (OOM).
- **interleaved-alloc-dealloc**: Allocate 10, free every other one, allocate 5
  more — check that the freed slots are reused.
- **alignment-and-size-rejection**: Request a layout larger than `SLOT_SIZE` and
  verify it returns null. Request a layout with alignment greater than `SLOT_SIZE`
  's alignment and verify it also returns null.
- **dealloc-stamps-the-free-list**: After dealloc, the `next_free` entry for
  that slot should be the previous head. Inspect via a test-only accessor.
- **stress test**: 10,000 random alloc/free operations against a slab with 64
  slots. Track which slots are "live" in a host-side bookkeeping `Vec` and
  assert the slab's free count matches.

### Done When

- All host tests pass under `cargo test`.
- All host tests pass under `cargo +nightly miri test --lib`. *No Miri warnings,
  no Stacked Borrows errors.* This is the acceptance gate that validates the
  design.
- The QEMU build with `\--features alloc-slab` boots, runs the existing kernel
  workload (shell, file system, etc.), and the existing benchmarks complete
  without OOM. (You may need to bump the slot size or increase `N` to fit
  realistic workloads — that's part of the discovery in Phase B.)
- `scripts/ci.sh` is green.

### Questions to Discover

- What slot size did you pick, and why? What allocations in the kernel workload
  don't fit?
- How big is the metadata overhead? For `N = 1024` slots and `u32` indices, the `
  next_free` array is 4 KB. The slot storage at 64 bytes/slot is 64 KB. Metadata
  is ~6%. How does this compare to the freelist's overhead?
- What happens when the workload requests a 65-byte allocation? The slab
  rejects it. In Phase B you'll handle this with multiple size classes; right
  now, just observe the failure.
- Run the existing alloc benchmarks. Is the slab faster than the freelist? (It
  should be — single-cycle pop instead of an O(n) walk.)

- - -
## Phase B: Multi-Class Segregated Slab

**Educational goal:** Understand size-class allocators, how production allocators
(jemalloc, tcmalloc) organise small-object allocation, and the trade-off
between metadata cost and internal fragmentation.

**Architectural goal:** Replace the single-class slab with an array of slabs at
different sizes. The `GlobalAlloc::alloc` implementation routes the request to
the smallest size class that fits.

### What to Implement

- A `SizeClass` constant table. A common pattern: powers of two from 8 to 256
  bytes (8, 16, 32, 64, 128, 256), or geometric with finer granularity at small
  sizes. Choose 4–8 classes to start.
- A `MultiSlab` struct holding one `Slab` per size class. Each slab can be a
  different `(SLOT_SIZE, N)` instance — use generics or const generics to
  parameterise.
- Routing logic: `alloc(layout)` finds the smallest class with `slot_size >=
  layout.size()` and `slot_align >= layout.align()`. If no class fits, return
  null (or fall through to the freelist if you want a "large allocation" escape
  hatch — see Discovery questions).
- `dealloc` needs to determine which slab a pointer belongs to. There are two
  classic options:
    1.  **Address-based:** lay out the slabs at known address ranges and binary
        search the pointer against the ranges.
    2.  **Pointer-tagged:** store the size class in metadata you can find from
        the pointer (e.g., a header word in front of every allocation). Start
        with option 1 — it's simpler when slabs are statics. Option 2 becomes
        attractive in the layered architecture (Phase D).

### Hints, Not Solutions

- Const generics with arrays of differently-sized types is awkward in Rust. One
  pragmatic approach: don't try to put the slabs in a homogeneous array. Just
  have named fields (`slab_8`, `slab_16`, ...) on the `MultiSlab` struct.
  Verbose, but type-correct and obviously bounded.
- The `dealloc` routing problem is the most interesting design question of this
  phase. Sketch both options on paper before choosing. What are the trade-offs?
- Internal fragmentation: a 65-byte allocation in a 128-byte slot wastes 63
  bytes. Across many allocations of varied sizes, what's the average waste?
  Worth measuring.

### What to Test (host, Miri-verified)

In addition to per-slab tests inherited from Phase A:

- **routing**: Allocate sizes 1, 7, 8, 9, 15, 16, 17. Verify each lands in the
  expected size class.
- **dealloc-routing**: Free pointers from different slabs interleaved, verify
  each goes back to the correct slab.
- **all-classes-exhausted**: Fill every size class. Verify the next request for
  any size returns null.
- **fragmentation accounting**: Allocate a known mix of sizes, sum up the slot
  bytes consumed vs the layout sizes requested. Compute internal fragmentation
  as a percentage. Add this as a test-visible stat.

### Done When

- All host tests pass with Miri clean.
- QEMU build with `\--features alloc-slab` runs the existing workload. The `heap`
  command shows per-class usage.
- Benchmarks run, and you have numbers to compare against the single-class slab
  and the freelist.
- `scripts/ci.sh` green.

### Questions to Discover

- What's the internal fragmentation rate on a realistic workload? Is the size
  class table well-chosen?
- What allocations are too large for any size class? In a single-slab
  architecture you'd be stuck. The escape hatch options are: a) Reject (caller
  has to use a different allocator). b) Fall back to the freelist for large
  allocations. c) Wait until Phase C and route them to the buddy allocator.
- Did adding size classes change the per-allocation latency? (It shouldn't —
  routing is O(1) for a fixed-size class table.)
- How much memory does the multi-class slab use *just for the slab arrays*,
  before any allocations? That's your fixed overhead.

- - -
## Phase C: Buddy Allocator

**Educational goal:** Understand how a single allocator can serve variable-size
requests without embedded linked lists, by managing a power-of-two address
space and exploiting the XOR-buddy trick. Internalise splitting, merging, and
the size-class hierarchy.

**Architectural goal:** A new module `src/kernel/alloc/buddy.rs` providing a
buddy allocator over a contiguous region of size `2^N` bytes. Selectable via `
alloc-buddy` feature flag. Coexists with bump/freelist/slab.

### What to Implement

- A `BuddyAllocator` managing a region of `2^MAX_ORDER` bytes, with a minimum
  allocation of `2^MIN_ORDER` bytes. (Reasonable starting values: `MIN_ORDER = 6`
  for 64-byte minimum, `MAX_ORDER = 18` for a 256 KB region.)
- A free list per *order*. There are `MAX_ORDER - MIN_ORDER + 1` orders, so this
  is a small fixed array. Each free list is index-based, like the slab.
- A bitmap (or one bit per block per order) tracking which blocks are currently
  free. The bitmap is what makes "is my buddy free?" O(1).
- `alloc(layout)`:
    1.  Round the requested size up to the next power of two ≥ `2^MIN_ORDER`.
        Call this the *target order*.
    2.  Look in the free list at the target order. If non-empty, pop and return.
    3.  Otherwise, walk *up* to higher orders looking for a free block.
    4.  When found, recursively *split* it: pop the higher block, mark its two
        halves as free at the next order down, push the second half onto that
        order's free list, repeat until you reach the target order.
    5.  The first half of the final split is the allocation.
- `dealloc(ptr, layout)`:
    1.  Compute the order from the layout (same rounding as alloc).
    2.  Compute the block index at that order.
    3.  Compute the *buddy* index: `block_index ^ 1`. (At each order, blocks
        come in pairs; XOR with 1 toggles which member of the pair you mean.)
    4.  Check the bitmap: is the buddy currently free?
    5.  If yes: remove the buddy from its free list, merge the two blocks into
        a single block at the next order up (whose index is `block_index >> 1`),
        and recurse — the merged block might in turn have a free buddy at the
        higher order.
    6.  If no: just push the freed block onto its order's free list and update
        the bitmap.

### Hints, Not Solutions

- The XOR trick deserves a moment of staring. At order *k*, block 0 occupies
  bytes `\[0, 2^k)` and block 1 occupies bytes `\[2^k, 2^(k+1))`. They're
  buddies — splitting their parent at order *k+1* (which spanned `\[0, 2^(k+1))`
  ) produced exactly these two. The parent's index at order *k+1* is 0; its
  children at order *k* are 0 and 1. In general: a block at order *k* with index *
  i* has buddy *i ^ 1*, and their merged parent at order *k+1* has index *i >> 1*
  . Convince yourself this is symmetric (the buddy of the buddy is the
  original).
- The bitmap can be `\[u64; ORDERS]\[N_BLOCKS_AT_THIS_ORDER / 64]`, but the
  block count varies by order (more blocks at lower orders). One pragmatic
  layout: a flat bitmap sized for the *smallest* order (where there are most
  blocks), and "synthetic" higher-order checks that AND together pairs. Or: a
  per-order bitmap, each smaller than the previous. Choose for clarity; you can
  optimise later.
- The free lists per order can be the same `next_free` index trick from the
  slab. Each order has its own head and its own `next_free` array. Index 0 at
  order *k* refers to bytes `\[0, 2^k)`; index 1 to bytes `\[2^k, 2\*2^k)`; etc.
  Index-to-byte-offset is just `index << k`.
- The `unsafe` boundary is the same as the slab: turning an `(order, index)`
  pair into a `\*mut u8` via `storage_base.byte_add(index << order)`. The base
  pointer's wide provenance carries forward.
- Start with no alignment handling (assume requests are naturally aligned to
  their power-of-two size, which is true for any `Layout` whose size is a power
  of two ≥ alignment). Add a separate "aligned alloc" path only if you find you
  need it.

### What to Test (host, Miri-verified)

- **single-block-roundtrip**: Allocate the smallest size, free it, allocate
  again, get the same address.
- **split-then-allocate**: With the heap empty (one big free block at `MAX_ORDER`
  ), allocate the smallest size. Verify that splitting produced the correct
  chain of free blocks at every order between `MAX_ORDER` and `MIN_ORDER`. Free
  the allocation. Verify the heap returns to a single free block at `MAX_ORDER`.
- **buddy-merge-on-free**: Allocate two minimum-size blocks (which will be
  buddies of the same parent). Free one, verify the bitmap shows just one free
  block at `MIN_ORDER`. Free the other, verify the merge cascades all the way
  back up to a single free block at `MAX_ORDER`.
- **non-buddy-no-merge**: Allocate four minimum-size blocks. Free the first and
  the third (not buddies). Verify you have two free blocks at `MIN_ORDER`, no
  merging.
- **internal-fragmentation**: Allocate 65 bytes. Verify the actual block size
  used is 128 (next power of two above 64). The leftover 63 bytes are internal
  fragmentation — quantifiable and expected.
- **stress test**: 10,000 random alloc/free of varied sizes. Track ground truth
  in a host-side `Vec` of `(ptr, layout)`. After all frees, verify the heap is
  back to a single max-order free block.

### Done When

- All host tests pass with Miri clean.
- QEMU build with `\--features alloc-buddy` boots and runs the workload.
- The `heap` command shows per-order free counts and total internal
  fragmentation.
- Benchmarks comparable across all four allocators (bump, freelist, slab,
  buddy).
- `scripts/ci.sh` green.

### Questions to Discover

- What's the worst-case internal fragmentation? (Hint: just under 50% — a
  request of `2^k + 1` bytes consumes `2^(k+1)` bytes.) On the actual workload,
  what's the average?
- How does buddy allocation latency compare to the slab? Why?
- How does buddy allocation latency compare to the freelist? Why?
- Can the buddy allocator handle the same workload that exhausted the freelist
  via fragmentation? (Probably yes for most workloads — coalescing is
  structural, not best-effort.)
- What happens if you ask for an allocation larger than `2^MAX_ORDER`? You
  should return null. Test it.

- - -
## Phase D: Layered Slab-on-Buddy

**Educational goal:** Understand how production kernels combine allocators to get
the best of each. The slab handles small-object allocation with near-zero
overhead; the buddy provides backing pages and handles large allocations
directly.

**Architectural goal:** Replace the multi-class slab's static storage with
buddy-allocated backing storage. The slab requests one or more pages from the
buddy when it needs to grow a size class; it returns pages to the buddy when a
slab is fully empty (eventually). Large allocations bypass the slab and go
straight to the buddy.

### What to Implement

- A `LayeredAllocator` (or similar) that owns a `BuddyAllocator` *and* a `
  MultiSlab`. The buddy is the source of truth for memory.
- Boot-time initialisation: the buddy gets the entire heap. The slab is
  initialised empty; it asks the buddy for backing pages on first use of each
  size class.
- `alloc(layout)` routing:
  - If `layout.size() <= LARGE_THRESHOLD` (e.g., 256 bytes): route to the slab.
    If the slab's chosen size class is empty, the slab asks the buddy for a
    fresh page (or several pages), carves it up into slots, and links them into
    the free list.
  - Otherwise: route directly to `buddy.alloc(layout)`.
- `dealloc(ptr, layout)`: same routing decision based on `layout.size()`. The
  slab's `dealloc` may eventually decide a backing page is fully empty and could
  be returned to the buddy (an *epoch* or *highwatermark* policy is one way;
  "never return until heap pressure" is another).
- Update the `heap` command to show buddy stats and slab stats together.

### Hints, Not Solutions

- The slab needs to know which buddy block backed each of its slots, so it can
  return them. The simplest design: each slab page is one buddy block at a
  known order; the slab tracks `Vec\<*mut u8>` of its current backing pages. (A `
  Vec` inside the allocator is fine in a `no_std` kernel because the layered
  allocator boots after the buddy is initialised — but think about the
  bootstrapping question carefully.)
- *Bootstrapping problem:* the slab wants to grow its own backing storage, which
  means it needs to allocate metadata. But it can't allocate metadata through
  itself before it has any storage. Think about whether the slab's internal `Vec`
  should live in a different allocator (the buddy directly? a fixed-size
  on-stack array?) to break the cycle.
- The "return pages to buddy" decision is hard. A slab page goes empty, then
  refills. Returning it eagerly costs an immediate buddy alloc next time.
  Returning it lazily costs memory. There is no right answer; pick a policy and
  document why.

### What to Test (host, Miri-verified)

- **routing**: Small allocations go through the slab; large ones hit the buddy.
  Verify with stat counters.
- **slab grows from buddy**: First small allocation triggers a buddy page
  request. Inspect the buddy's free count to confirm.
- **slab returns to buddy** (if your policy allows): Free everything, run the
  policy, verify the page returns.
- **mixed workload**: A realistic mix of small and large allocations runs
  cleanly, no double-frees, no leaks. Track ground truth with a Vec on the host
  side and assert the layered stats match.
- **interaction with the bootstrap**: Verify the slab can be initialised on an
  empty heap and the first allocation succeeds.

### Done When

- All host tests pass with Miri clean.
- QEMU build with `\--features alloc-layered` (new feature) runs the full
  workload with measurable improvement over either slab-alone or buddy-alone.
- `heap` command shows the dual-tier stats clearly.
- `scripts/ci.sh` green.

### Questions to Discover

- How big is `LARGE_THRESHOLD`? Try a few values; this is your first real
  performance tuning knob.
- For the kernel's actual workload, what fraction of allocations go through the
  slab vs the buddy? What fraction of *bytes*?
- Is the layered allocator faster than the buddy alone? Why?
- Is it less fragmented than the slab alone? Why?

- - -
## Performance Comparison Framework

The whole point of keeping multiple allocators is comparison. Across all four
(eventually five) allocators, you want measurements on the same workloads.
Bench rig:

- A "boot" workload: just kernel init, then read the stats.
- A "shell session" workload: a scripted sequence of `cat`, `ls`, `write`, `rm`
  against the FAT16 disk (mirrors the existing manual session in
  ALLOCATOR_PLAN.md stage 1).
- A "stress" workload: a tight loop of allocate/free with a representative size
  mix.

Metrics to capture for each (allocator, workload) pair:

- Cycles per `alloc`, p50 and p99
- Cycles per `dealloc`, p50 and p99
- Peak heap usage (bytes)
- Peak fragmentation (largest allocation that fails when total free exceeds the
  request)
- Metadata overhead (bytes consumed by the allocator's bookkeeping)
- Code size (bytes of compiled `.text` for the allocator module — handy for
  embedded)

Build a small `bench` shell command (or extend the existing one) that runs each
workload and prints a table. Save the results to `docs/` as a CSV or markdown
table after each phase. The numbers tell the story.

- - -
## Miri Verification Gates

These are non-negotiable acceptance criteria — every phase must pass them
before moving on:

1.  `cargo +nightly miri test --lib --target $HOST_TARGET` is clean (no
    warnings, no Stacked Borrows errors, no UB).
2.  `cargo +nightly miri test --lib --target $HOST_TARGET -- --include-ignored`
    is also clean if you have ignored stress tests.
3.  `MIRIFLAGS="-Zmiri-many-seeds=0..32" cargo +nightly miri test --lib ...` is
    clean for any test that exercises concurrency (locks, cross-thread
    handoffs).

If a Miri error appears, *stop*. Do not work around it. The whole point of this
exercise is to keep the design provably sound, and Miri errors are the canary.
The freelist's history is the cautionary tale: int-to-pointer casts *hid* the
underlying bug for months, and switching to provenance-preserving arithmetic
surfaced it. Don't let that happen here.

- - -
## Open Questions and Decisions Deferred

- **Lock granularity.** The slab and buddy can both use a single `
  IrqSpinLock\<Inner>` to start. If contention becomes a problem (it won't,
  you're single-threaded), per-class locks for the slab and per-order locks for
  the buddy are the standard refinements.
- **Cache coloring.** Real slab allocators sometimes offset slot starts within a
  page to reduce cache associativity collisions. Skip this — it's irrelevant on
  RP2350-class hardware.
- **NUMA-style local caches.** The famous "magazine" layer in tcmalloc caches
  free objects per-CPU. Skip this — single core.
- **Slab return policy.** When to return empty slab pages to the buddy is a
  design question. Don't agonise; pick a simple policy ("never return" or
  "return when fully empty") and revisit if measurements suggest it matters.
- **PSRAM region (Phase 14).** When PSRAM lands, the layered allocator will need
  a "which region for this allocation?" decision similar to `ALLOCATOR_PLAN.md`
  's Stage 5. Defer until then.

- - -
## Commit Strategy

One phase is *not* one commit — that would be too coarse. Suggested commit
granularity per phase:


|Phase|Suggested commits                                                                                                                                                                         |
|-----|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
|A    |(1) `Slab` skeleton + init + `alloc`; (2) `dealloc`; (3) host tests; (4) Miri-clean; (5) `GlobalAlloc` impl + feature flag; (6) QEMU integration                                          |
|B    |(1) `SizeClass` table + routing; (2) per-class slabs; (3) `dealloc` routing; (4) host tests; (5) QEMU                                                                                     |
|C    |(1) data structures (free lists, bitmap); (2) `alloc` with splitting; (3) `dealloc` with merging; (4) host tests for split; (5) host tests for merge; (6) `GlobalAlloc` \+ feature flag; (7) QEMU|
|D    |(1) layered struct skeleton; (2) routing; (3) slab→buddy backing; (4) host tests; (5) QEMU                                                                                                |

`scripts/ci.sh` before every commit. No exceptions.

- - -
## Relationship to Existing Work

- `docs/ALLOCATOR_PLAN.md` describes the bump→freelist evolution. This plan is
  the next chapter, *not* a replacement. The freelist stays for comparison.
- `src/kernel/alloc/freelist.rs` is the host-testable scaffold. The new
  allocator modules follow the same pattern: logic in the lib crate, the `
  \#\[global_allocator]` static and linker-symbol coupling in `main.rs`.
- `src/kernel/alloc/freelist2.rs` is the older pointer-based version. Once the
  slab is working and Miri-clean, consider whether `freelist2.rs` can be deleted
  (its lessons are absorbed) or whether to keep it as a comparison point.
- **Cold review item #1** (heap exhaustion) was resolved by the freelist with
  coalescing. This plan is *not* about that bug — it's about architectural
  improvement and Miri-cleanliness.

- - -
*Plan created during the conversation about the freelist's Miri issues. Author:
Thomas, with Claude as planning collaborator. The user writes all production
code; Claude reviews, hints, and unblocks.*

