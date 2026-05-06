# Buddy Allocator

A working description suitable for implementing a simple buddy allocator from
scratch. This builds on the mental model from the freelist work — raw pointers,
heap-wide provenance, sentinel-style list heads, and FreeBlock headers written
into unused memory.

## 1\. The core idea

A buddy allocator divides the heap into **power-of-two-sized blocks** and
maintains a set of free lists, one per block size. The key insight: every block
can be either allocated as-is, or split in half to produce two smaller blocks
of the next-smaller size. When those smaller blocks are both freed, they can be
merged back into the parent.

The thing that makes this elegant (and fast) is that every block has exactly
one **buddy** — the other half of the block it was split from — and the address
of the buddy can be **computed in O(1)** from the address of the block itself. No
searching, no metadata lookups. Just one XOR operation.

## 2\. Orders

Every block has an **order** — its size expressed as a power-of-two multiple of
the minimum block size.

- Order 0 = minimum block size (e.g. 64 bytes)
- Order 1 = 2 × minimum = 128 bytes
- Order 2 = 4 × minimum = 256 bytes
- Order N = 2^N × minimum

For a 256 KiB heap with 64-byte minimum blocks:

```
Order   Size       How many fit in 256 KiB
  0      64 B            4096
  1     128 B            2048
  2     256 B            1024
  3     512 B             512
  4      1 KiB            256
  5      2 KiB            128
  6      4 KiB             64
  7      8 KiB             32
  8     16 KiB             16
  9     32 KiB              8
 10     64 KiB              4
 11    128 KiB              2
 12    256 KiB              1   <- the whole heap at max order
```
So there are **13 orders** (0 through 12), and the maximum order represents the
entire heap as a single block. This is how `init` starts: one free block at the
max order.

## 3\. Blocks are naturally aligned to their size

An order-N block is always aligned to 2^N bytes (relative to the heap start). A
4 KiB block always lives at a heap-relative address that is a multiple of 4096.
A 64 KiB block at a multiple of 65536. And so on.

This is not a convention you enforce — it's a consequence of how splitting
works. If you start with an aligned block and split it in half, both halves are
aligned to their new (smaller) size. Start aligned, stay aligned forever.

## 4\. Buddies and the XOR trick

When you split an order-N block, the two resulting order-(N-1) blocks are
called **buddies**. Each has exactly one buddy — the other half of the same split.

Here's the beautiful part: given a block at heap-relative address A of order N,
its buddy's address is:

```
buddy_addr = A XOR (1 << (N + min_order_log2))
```
Or, equivalently, `A XOR block_size_in_bytes(N)`.

Why does this work? An order-N block is 2^N × min_size bytes. Two buddies come
from splitting a block of twice that size. Both buddies share all their upper
address bits (because they're in the same parent), and they differ **only in the
single bit at position log2(block_size_in_bytes(N))**. XORing with the block size
flips exactly that bit.

A concrete example with 64-byte minimum blocks and heap-relative addresses:

```
Split a 512-byte block (order 3) at heap-relative address 0x400:

    0x400  +------------------+
           |                  |
           |  order-2 block   |  (256 bytes)
           |   addr = 0x400   |
           |                  |
    0x500  +------------------+  <- split point, offset 256 from parent
           |                  |
           |  order-2 block   |  (256 bytes)
           |   addr = 0x500   |
           |                  |
    0x600  +------------------+

The two order-2 blocks are buddies:
    0x400 XOR 0x100 = 0x500   <- buddy of left is right
    0x500 XOR 0x100 = 0x400   <- buddy of right is left

(0x100 = 256 = block size at order 2)
```
The XOR is symmetric: the buddy of my buddy is me. And the parent block's
address is always the lower of the two buddy addresses — that is, `min(A,
A_XOR_size)`, which is just `A & \\\\\\\!size`.

## 5\. Data structures

The allocator needs two main things:

### 5a. An array of free-list heads, one per order

```
struct BuddyAllocator {
    heap_start: *mut u8,                          // base of heap, for provenance
    free_lists: [*mut FreeBlock; NUM_ORDERS],     // one head per order
}
```
Each `free_lists\\\\\\\[n]` is the head of a doubly-linked list of free blocks
at order `n`. Doubly-linked so that removing an arbitrary block (not just the
head) is O(1) — this matters because when you merge, you need to pull the buddy
out of the free list wherever it happens to sit.

### 5b. A FreeBlock header in each free block

Same technique as your freelist: the `FreeBlock` struct is written into the
first bytes of each free block's memory. When the block is allocated, those
bytes become user data; when it's freed again, a new header is written.

```
struct FreeBlock {
    next: *mut FreeBlock,   // next free block at the same order
    prev: *mut FreeBlock,   // previous free block at the same order
    // note: no `size` field — order is implied by which list the block
    // is on, not stored in the block itself.
}
```
The `prev` field is the new thing compared to your freelist. It lets you remove
a block from the middle of a free list in O(1) — essential for the merge
operation, where you pull the buddy out of wherever it currently sits.

The minimum block size must be at least `size_of::\\\\\\\<FreeBlock>()`, same
constraint you already know. On RV32 this is 8 bytes (two 4-byte pointers).
Your minimum of 64 gives comfortable headroom.

### 5c. (Optional) A bitmap for "is this block split?"

For a first implementation, you can skip this — just walk the free list at
order N to check if the buddy is free. A 256-byte heap with 64-byte min has at
most 4096 / 2 = 2048 buddies per order, and the free lists are usually much
shorter than that in practice.

If performance matters later, a per-order bitmap with one bit per block tells
you in O(1) whether a given block is free. That's ~1 KiB of BSS for a 256 KiB
heap across all orders. For this project, I'd recommend **skipping the bitmap for
your first version** and adding it later if you care about the O(n) walk cost.

## 6\. Initialisation

At boot, the heap is one giant free block at the maximum order. Init:

1.  Zero all the free list heads.
2.  Write a `FreeBlock` header at the heap start with `next: null, prev: null`.
3.  Set `free_lists\\\\\\\[MAX_ORDER]` to point at the heap start.

That's it. Everything else is empty.

**Constraint**: the heap size must be a power of 2, and the heap start must be
aligned to the heap size. For EASE with a 256 KiB heap, both are naturally
satisfied.

If you wanted to support non-power-of-two heap sizes later, you'd carve the
heap into the largest power-of-two blocks that fit and put each on its
respective free list. Skip this complication for now.

## 7\. Allocation algorithm

Given a requested layout (size + alignment), allocate:

```
1. Compute the required order:
   needed_size = max(layout.size, layout.align, MIN_BLOCK_SIZE)
   order = ceil(log2(needed_size / MIN_BLOCK_SIZE))

   For a 64-byte request: order = 0
   For a 100-byte request: order = 1 (rounds up to 128)
   For a 4096-byte request: order = 6 (exactly 4 KiB)

   If order > MAX_ORDER: return null (request too large).

2. Walk up the order array looking for a non-empty free list:
   current_order = order
   while current_order <= MAX_ORDER:
       if free_lists[current_order] is not null:
           break
       current_order += 1
   else:
       return null  (OOM — no block large enough)

3. Pop the first block from free_lists[current_order]:
   block = free_lists[current_order]
   free_lists[current_order] = block.next
   (also fix up prev pointer on new head if it exists)

4. If current_order > order, split down:
   while current_order > order:
       current_order -= 1
       buddy_addr = block.addr XOR (1 << (current_order + MIN_ORDER_LOG2))
       // Write a new FreeBlock header at buddy_addr
       // Push that buddy onto free_lists[current_order]
       // Keep `block` pointing at the half we'll continue to split/return

5. Return `block` as *mut u8.
```
The splitting step is the recursive splitting you'd expect: every time you
split an order-N block into two order-(N-1) blocks, one becomes the allocation
(or gets split further), and the other goes onto the order-(N-1) free list.

## 8\. Deallocation algorithm

Given a pointer and layout, deallocate:

```
1. Compute the order from the layout (same formula as alloc).

2. Start with `block = ptr as *mut FreeBlock`, `order = computed_order`.

3. Loop:
   if order == MAX_ORDER:
       break  (can't merge further — we're at the whole heap)

   buddy_addr = block.addr XOR (1 << (order + MIN_ORDER_LOG2))

   if buddy is in free_lists[order]:
       // Merge: remove buddy from its free list, step up to parent
       unlink buddy from free_lists[order]
       block = pointer to min(block_addr, buddy_addr)   // the parent
       order += 1
       continue loop
   else:
       break  (buddy is allocated; can't merge)

4. Push `block` onto free_lists[order] (with new FreeBlock header).
```
The key trick: when you find that the buddy is free, you remove the buddy from
its free list (O(1) because doubly-linked), compute the parent address (always
the lower of the two), and try to merge at the next order up. You keep
recursing until either you can't merge anymore, or you've merged back up to the
whole heap.

**Note**: "is buddy in free_lists\[order]?" is the step that's O(n) without a
bitmap. You walk the free list at that order looking for a block whose address
equals `buddy_addr`. For small heaps this is fine; it's the thing you'd optimise
later with a bitmap.

## 9\. A worked example

Let's walk through a tiny heap: 256 bytes, minimum block size 64, so 4 orders
(0 through 2):

- Order 0 = 64 B (4 of them fit)
- Order 1 = 128 B (2 of them fit)
- Order 2 = 256 B (1 of them, the whole heap)

Heap starts at 0x1000.

### Init

```
free_lists[0] = null
free_lists[1] = null
free_lists[2] = 0x1000  (single block covering whole heap)
```
### Alloc 64 bytes (order 0)

Need order 0. free_lists\[0] is empty. Walk up. free_lists\[1] is empty. Walk
up. free_lists\[2] = 0x1000. Pop it.

Split down from order 2 to order 0:

- Split order 2 at 0x1000 → two order-1 blocks at 0x1000 and 0x1080. Push
  0x1080 onto free_lists\[1]. Keep 0x1000 for further splitting.
- Split order 1 at 0x1000 → two order-0 blocks at 0x1000 and 0x1040. Push
  0x1040 onto free_lists\[0]. Return 0x1000 as the allocation.

```
free_lists[0] = 0x1040
free_lists[1] = 0x1080
free_lists[2] = null
Allocated: [0x1000, 0x1040)   (the 64-byte block)
```
### Alloc another 64 bytes

Need order 0. free_lists\[0] = 0x1040. Pop it. Return.

```
free_lists[0] = null
free_lists[1] = 0x1080
free_lists[2] = null
Allocated: [0x1000, 0x1080)   (two 64-byte blocks)
```
### Free 0x1000

Order 0. Buddy of 0x1000 at order 0 = 0x1000 XOR 0x40 = 0x1040. Is 0x1040 in
free_lists\[0]? No (it's allocated). So just push 0x1000 onto free_lists\[0].

```
free_lists[0] = 0x1000
free_lists[1] = 0x1080
free_lists[2] = null
Allocated: [0x1040, 0x1080)
```
### Free 0x1040

Order 0. Buddy of 0x1040 at order 0 = 0x1040 XOR 0x40 = 0x1000. Is 0x1000 in
free_lists\[0]? Yes! Unlink it. Parent = min(0x1000, 0x1040) = 0x1000. Step up
to order 1.

Buddy of 0x1000 at order 1 = 0x1000 XOR 0x80 = 0x1080. Is 0x1080 in
free_lists\[1]? Yes! Unlink it. Parent = min(0x1000, 0x1080) = 0x1000. Step up
to order 2.

Order 2 is MAX_ORDER. Stop. Push 0x1000 onto free_lists\[2].

```
free_lists[0] = null
free_lists[1] = null
free_lists[2] = 0x1000   (back to initial state!)
```
Notice how the allocator correctly recovered the whole heap once all the
sub-allocations were freed. That's the buddy magic — it doesn't matter what
order the frees happened in; the merges cascade as soon as both halves of any
split are free.

## 10\. Provenance notes

Everything you learned from the freelist applies:

- Store `heap_start` as `\\\\\\\*mut u8` with heap-wide provenance.
- The free-list-head pointers are `\\\\\\\*mut FreeBlock`, derived from `
  heap_start`.
- When you compute a buddy address via XOR, you have a `usize`, not a pointer.
  To convert back to a pointer with provenance, use `
  heap_start.with_addr(buddy_abs_addr)` (stable since Rust 1.84), or
  equivalently `heap_start.byte_add(buddy_relative_addr)`.
- Never derive a buddy pointer from the block-pointer directly via 
  `block.byte_add(size)` or similar — the block pointer was created via `
  \\\\\\\&mut` narrowing at some point, and its provenance might be narrowed to
  just the FreeBlock bytes. Always derive from `heap_start`.

This last point is subtle but important. The freelist's `split_right` got away
with `this.byte_add(offset)` because the block pointers came through a pure
raw-pointer chain from init. If you're strict about "buddy addresses are
computed from heap-relative offsets and reconstituted from heap_start", you
stay provenance-safe by construction.

## 11\. Practical parameters for EASE

```
Heap size:      256 KiB (2^18 bytes)
Min block:       64 B   (2^6 bytes, same as your existing BASE_ALIGN-ish)
Max block:      256 KiB (whole heap)
Num orders:      13
Free list heads: 13 * 4 bytes = 52 bytes in BSS
FreeBlock size:  8 bytes (next + prev on RV32)
```
Suggested struct layout:

```
const MIN_ORDER_LOG2: usize = 6;   // 64 bytes
const MAX_ORDER: usize = 12;        // 256 KiB
const NUM_ORDERS: usize = 13;

struct FreeBlock {
    next: *mut FreeBlock,
    prev: *mut FreeBlock,
}

struct BuddyAllocator {
    heap_start: *mut u8,
    free_lists: [*mut FreeBlock; NUM_ORDERS],
}

// wrap it in AllocatorLock<BuddyAllocator> in FreeBlockList-style
```
## 12\. Implementation order (suggested 2-hour path)

1.  **Define the struct and constants.** Empty `alloc` returning null, empty `
    dealloc`. Make sure it compiles.
2.  **Write `init`.** Install one max-order block. Verify via a test that `
    free_lists\\\\\\\[MAX_ORDER]` is non-null after init.
3.  **Write a helper to compute order from `Layout`.** Test it on a few sizes by
    hand.
4.  **Write a helper to compute buddy address from a block address and order.**
    XOR the correct bit. Test it with a few examples.
5.  **Write the doubly-linked list push/unlink helpers.** These will be called
    from alloc (split), alloc (pop from head), dealloc (push), and dealloc
    (unlink buddy).
6.  **Write `alloc`.** Walk up for a free block, split down to the target order,
    return. Test with single allocations of various sizes.
7.  **Write `dealloc` without coalescing.** Just push the block onto its order's
    free list. Test alloc+dealloc cycles.
8.  **Add coalescing to `dealloc`.** Test that two alloc/free pairs end up back
    at the max-order free list.
9.  **Run the existing host tests.** They should all pass, same tests that
    currently work for the freelist.
10. **Run Miri.** Fix any provenance issues that come up. The "always derive
    buddy from heap_start" rule should keep things clean.

## 13\. Common pitfalls

- **Forgetting the heap must be max-order aligned.** On the EASE kernel the
  linker script places `__heap_start` at a page-aligned address, which is
  conveniently aligned to 256 KiB if the heap is 256 KiB.
- **XORing absolute addresses instead of heap-relative ones.** The XOR trick only
  works for addresses *within the heap*. Subtract `heap_start` first if needed,
  or be careful that the high bits of your addresses are zero within the heap
  (which they will be for a small heap starting at a heap-size-aligned address).
- **Not handling the "no buddy exists at max order" case.** The loop in dealloc
  must stop when `order == MAX_ORDER` — there's no larger block to merge into.
- **Requesting an order larger than MAX_ORDER.** Alloc must check this upfront
  and return null.
- **Doubly-linked list bugs.** The classic: forgetting to fix up `prev` when you
  change `next`, or vice versa. Drawing the before/after on paper for each list
  operation catches most of these.
- **Forgetting that the FreeBlock header doesn't contain the order.** The order
  is implied by which free list the block is on. This means during dealloc, the
  caller (via the Layout) tells you the order; during merge, you track it as a
  local variable as you step up.

## 14\. Connection to what you've built

The buddy allocator is structurally similar to your freelist: same
embedded-header-in-free-memory technique, same lock-protected state, same
raw-pointer traversal, same init-from-linker-symbols pattern. What's different:

- Multiple free lists instead of one.
- Blocks are fixed power-of-two sizes instead of arbitrary.
- Coalescing is O(log n) via XOR instead of O(n) via walk-and-compare.
- No per-block size field needed (size is implicit in the list).
- Doubly-linked lists (for O(1) unlink) instead of singly-linked.

The provenance and `unsafe`\-block principles you built up with the freelist
transfer directly. The trickiest new thing is the XOR math and getting
comfortable with "the buddy is always exactly this one other address." Once
that clicks, the rest writes itself.

Good luck. The moment where you realise you can find a buddy with a single XOR
instead of searching a data structure — that's the satisfying click this whole
design is built around.

