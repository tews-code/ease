# Single-Class Slab Allocator: Design Brief

**Purpose:** Enough detail to implement the basic slab allocator offline, without
needing to look anything up. Covers data layout, alloc/dealloc algorithms,
project wiring, host tests, and gotchas.

- - -
## The Big Picture

A slab is an array of N identical slots. Free slots form a linked list — but
the links are **indices** into a parallel array, not pointers embedded in the
slots. That's the whole trick.

```
Storage (the actual heap bytes):
┌────────┬────────┬────────┬────────┬────────┐
│ slot 0 │ slot 1 │ slot 2 │ slot 3 │ slot 4 │  ...  N slots
│ 64 B   │ 64 B   │ 64 B   │ 64 B   │ 64 B   │
└────────┴────────┴────────┴────────┴────────┘

Sidecar metadata (the free list, as indices):
next_free: [1, 2, 3, 4, 5, ... , u32::MAX]
              │     │              └── end of list
              │     └── slot 2's next free is slot 3
              └── slot 1's next free is slot 2

free_head: 0  (first free slot is slot 0)
```
**Alloc**: pop `free_head`. The slot at that index is now allocated. Set `
free_head = next_free\[old_head]`. Return a pointer to slot `old_head`'s bytes. **
O(1).**

**Dealloc**: given a pointer, compute the slot index. Set `next_free\[index] =
free_head`, then `free_head = index`. **O(1).**

No splitting. No merging. No fragmentation. No provenance issues in the
bookkeeping.

- - -
## Data Structures

```
struct SlabInner {
    free_head: u32,              // index of first free slot, u32::MAX = empty
    next_free: [u32; N],         // per-slot: index of next free slot
    storage: [u8; N * SLOT_SIZE] // the actual bytes handed out
}
```
**Important**: the storage should be wrapped in `UnsafeCell` so you can hand out `
\*mut u8` pointers from a `\&self` reference (which is what `GlobalAlloc` gives
you — it takes `\&self`, not `\&mut self`).

The whole `SlabInner` lives inside an `AllocatorLock\<SlabInner>` so it's
protected by your existing lock.

- - -
## The Struct Layout You'll Actually Write

```rust
use core::cell::UnsafeCell;
use crate::kernel::sync::AllocatorLock;

const SLOT_SIZE: usize = 64;
const NUM_SLOTS: usize = 4096;  // 4096 x 64 = 256 KB of storage

struct SlabInner {
    free_head: u32,
    next_free: [u32; NUM_SLOTS],
}

pub struct Slab {
    inner: AllocatorLock<SlabInner>,
    storage: UnsafeCell<[u8; NUM_SLOTS * SLOT_SIZE]>,
}
```
Why `storage` is **outside** the lock, wrapped in `UnsafeCell`:

- `GlobalAlloc::alloc` returns a `\*mut u8`. The caller will read/write those
  bytes *without holding the lock*. That's correct — the lock protects the *
  metadata* (free list), not the *allocated bytes*. The bytes belong to the
  caller after `alloc` returns.
- `UnsafeCell` tells the compiler "interior mutability here" so you can get a `
  \*mut u8` from a `\&self`.
- The whole `Slab` is `static`, so both the lock and the storage live for `
  'static`.

- - -
## Init

Link every slot into the free list at construction:

```
next_free[0] = 1
next_free[1] = 2
next_free[2] = 3
...
next_free[N-2] = N-1
next_free[N-1] = u32::MAX   // end of list

free_head = 0
```
You'll do this in an `init()` method, since `const fn` can't loop in all cases.
Or if your nightly supports it, in a `const fn new()`. Either way — the pattern
is:

```
for i in 0..N-1 { next_free[i] = (i + 1) as u32; }
next_free[N-1] = u32::MAX;
free_head = 0;
```
- - -
## Alloc

```rust
fn alloc(&self, layout: Layout) -> *mut u8 {
    // 1. Reject requests that don't fit a slot
    if layout.size() > SLOT_SIZE || layout.align() > SLOT_SIZE {
        return core::ptr::null_mut();
    }

    // 2. Lock, pop the free list head
    let mut inner = self.inner.lock();
    let idx = inner.free_head;
    if idx == u32::MAX {
        return core::ptr::null_mut();  // OOM
    }
    inner.free_head = inner.next_free[idx as usize];

    // 3. Compute the pointer -- THIS is the only unsafe boundary
    //    storage.get() returns *mut [u8; N * SLOT_SIZE] with WIDE provenance.
    //    byte_add preserves that provenance.
    let base: *mut u8 = self.storage.get().cast::<u8>();
    unsafe { base.byte_add(idx as usize * SLOT_SIZE) }
}
```
Key points:

- The provenance comes from `self.storage.get()` — one `UnsafeCell::get()` call,
  giving you a `\*mut` covering the entire storage array. Every slot pointer is
  derived from that via `byte_add`. Wide provenance, Miri-clean.
- The lock is dropped (guard goes out of scope) *before* the pointer is
  returned. The caller accesses the bytes lock-free. This is correct.
- `layout.align() > SLOT_SIZE` — since every slot starts at a `SLOT_SIZE`
  \-aligned offset from the base, and `SLOT_SIZE` is a power of two, any
  alignment <= SLOT_SIZE is satisfied. Reject anything larger.

- - -
## Dealloc

```rust
unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
    // 1. Compute the slot index from the pointer
    let base = self.storage.get().cast::<u8>();
    let offset = unsafe { ptr.offset_from(base) } as usize;
    let idx = (offset / SLOT_SIZE) as u32;

    // 2. Debug assertions (optional but very helpful)
    debug_assert!(offset % SLOT_SIZE == 0, "pointer not slot-aligned");
    debug_assert!((idx as usize) < NUM_SLOTS, "pointer out of range");

    // 3. Lock, push onto the free list head
    let mut inner = self.inner.lock();
    inner.next_free[idx as usize] = inner.free_head;
    inner.free_head = idx;
}
```
Key points:

- `ptr.offset_from(base)` is `unsafe` but gives a signed byte distance. Both
  pointers must be in the same allocation — which they are, since `ptr` was
  derived from `base.byte_add(...)` in `alloc`. Miri-clean.
- The division `offset / SLOT_SIZE` gives the slot index. No pointer-to-integer
  round-trip needed for the *bookkeeping* — only for computing the index, which
  is a number not a pointer.
- No double-free protection in this sketch. You can add it later (an 
  `is_allocated` bitmap, or poisoning the `next_free` entry).

- - -
## GlobalAlloc Impl

Same pattern as the freelist:

```rust
unsafe impl GlobalAlloc for Slab {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.alloc(layout)  // delegate to the method above
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.dealloc(ptr, layout) }
    }
}

// Safety: The AllocatorLock serialises all access; the Slab is safe to
// share across interrupt contexts.
unsafe impl Sync for Slab {}
```
- - -
## Wiring into the Project

Follow the exact pattern the freelist uses:

`Cargo.toml` — add the feature:

```toml
alloc-slab = []
```
Add it to `default` and `test-all` as appropriate when you're ready to test.

`src/kernel/alloc/mod.rs` — add the module:

```rust
#[cfg(feature = "alloc-slab")]
pub mod slab;
```
`src/lib.rs` — the inline `mod kernel` block already loads `pub mod alloc;`,
which loads `alloc/mod.rs`, which will conditionally load `slab.rs`. Nothing to
change in lib.rs.

`src/main.rs` — add the `\#\[global_allocator]` static and `init` call, gated on `
alloc-slab`, next to the existing freelist version. The slab's `init` is simpler
than the freelist's — it just links the free list internally. You still call it
from `init_global_allocator()` with the same pattern.

**However**: the slab doesn't take `start: \*mut u8, size: usize` from the
linker symbols the way the freelist does. The storage is *inside* the slab
struct itself (it's a `\[u8; N * SLOT_SIZE]` array). So the slab is a big `static`
that includes the heap inline. This means:

- The `__heap_start` / `__heap_end` linker symbols aren't used by the slab. The
  slab's storage *is* the heap.
- The `.bss` section will be much larger (256 KB for the storage alone). That's
  fine — it comes from the same RAM.
- If you want the slab to use the linker-defined region instead (more
  flexible), you'd store a `\*mut u8` and `N` at runtime and init the slab
  against that pointer. This is more complex but matches the freelist's
  pattern. **Start with inline storage** — it's simpler and you can refactor
  later.

- - -
## Host Tests

Create a `\#\[cfg(all(test, not(target_os = "none")))]` module inside `slab.rs`.
The tests don't need linker symbols or `\#\[global_allocator]` — they construct
a `Slab` directly.

**Gotcha**: the `Slab` struct is large (256 KB of storage inline). Don't put it
on the stack in a test — it'll overflow. Use `Box::leak(Box::new(Slab::new()))`
to get a `&'static Slab` for testing. Since you're in a host test with std, `Box`
uses the system allocator, not your slab. This bootstrapping is fine.

**Or**: make the test slab much smaller — `NUM_SLOTS = 16, SLOT_SIZE = 16`.
Easier to reason about, fits on the stack, and exercises the same logic.

### Test list

1.  **alloc_then_dealloc_roundtrip** — alloc, dealloc, alloc again. Second alloc
    returns the same pointer.
2.  **fill_the_slab** — alloc N times, all succeed with distinct pointers. N+1
    returns null.
3.  **dealloc_reuses_slot** — alloc 3, dealloc the middle one, alloc again. New
    alloc returns the middle slot's address.
4.  **reject_too_large** — request `layout.size() > SLOT_SIZE`, get null.
5.  **reject_too_aligned** — request `layout.align() > SLOT_SIZE`, get null.
6.  **stress** — 1000 random alloc/free against a 64-slot slab. Track live
    allocations in a `Vec\<*mut u8>`. After freeing all, verify `free_head`
    walks the full chain of N slots.

- - -
## Miri Check

Once tests pass:

```
cargo +nightly miri test --lib --target $HOST_TARGET
```
This should be clean with zero warnings. If you see *any* Stacked Borrows or
provenance complaint, stop and think — the design above shouldn't produce any.

- - -
## Things to Watch Out For

1.  `const`** initialisation.** You need to initialise `next_free` at compile
    time or in `init()`. A `const fn new()` that builds the array with a loop
    requires `const` loop support (nightly has this). If it's awkward, just use `
    \[u32::MAX; N]` in `new()` and link the chain in `init()`.
2.  **Alignment of the storage.** The storage array `\[u8; N * SLOT_SIZE]` has
    alignment 1 (it's bytes). If a caller requests `Layout::new::\<u64>()`
    (alignment 8), the returned pointer must be 8-aligned. Since each slot
    starts at offset `idx * SLOT_SIZE` from the base, and `SLOT_SIZE` is 64 (a
    multiple of 8, 16, 32, 64), every slot is naturally 64-aligned *relative to
    the base*. But the *base itself* must be 64-aligned too. To ensure this, add `
    \#\[repr(align(64))]` to a wrapper around the storage, or to the `Slab`
    struct itself. Otherwise you'll get alignment violations on the first slot.
3.  `UnsafeCell`** and `Sync`.** `UnsafeCell\<T>` is `\!Sync` by default, which
    means `Slab` won't be `Sync` either. You need `unsafe impl Sync for Slab {}`
    because the lock serialises all metadata access, and once bytes are handed
    out, they're exclusively owned by the caller. This is the same safety
    argument the freelist makes.
4.  **Don't forget `Send` for the inner types.** `SlabInner` contains only
    integers, so it's `Send` automatically. `Slab` contains an `UnsafeCell`, so
    you may need to explicitly assert `Send` depending on how strict your build
    is.

- - -
## Summary: What to Build


|File                    |What                                            |
|------------------------|------------------------------------------------|
|`src/kernel/alloc/slab.rs`|`Slab`, `SlabInner`, `GlobalAlloc` impl, host tests|
|`src/kernel/alloc/mod.rs`|Add `\#\[cfg(feature = "alloc-slab")] pub mod slab;`|
|`Cargo.toml`            |Add `alloc-slab = []` feature                   |

You should be able to get the struct definitions, `init`, `alloc`, `dealloc`,
and the first few host tests done in ~2 hours. Leave `main.rs` wiring and QEMU
integration for after landing — those need `ci.sh` which needs a network-free
QEMU run.

- - -
*Design brief created from conversation about freelist Miri issues and the
slab/buddy allocator plan.*

