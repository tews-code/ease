# Free-List + Bump Allocator Design

## The problem

Every `Vec`, `String`, `Box` allocation permanently consumes heap. With 64KB, the shell will exhaust it during a long session (each command creates/drops Strings).

## Simplest viable fix: free-list on top of bump

Keep the bump allocator for fresh allocations. On `dealloc`, put freed blocks onto a linked list. On `alloc`, check the free list first, then fall back to bumping.

The trick: store the free-list node **inside the freed memory itself** — a freed block is unused, so you repurpose it as a list node. Each node needs a `next` pointer and a `size` (8 bytes on 32-bit). This means the minimum allocation size is 8 bytes.

## How it works

```
alloc(layout):
    1. Walk free list for a block >= requested size (with correct alignment)
    2. If found, remove from list and return it
    3. Otherwise, bump allocate as before

dealloc(ptr, layout):
    1. Compute actual block size (max of layout.size(), 8)  — must fit a free node
    2. Write a FreeNode { size, next } into the freed memory
    3. Prepend to free list head
```

## Data structure

```rust
struct FreeNode {
    size: usize,    // size of this free block
    next: *mut FreeNode,  // next free block (or null)
}
```

`BumpAllocatorInner` gets one new field:

```rust
struct BumpAllocatorInner {
    heap_start: usize,
    heap_end: usize,
    next: usize,
    free_list: *mut FreeNode,  // head of free list (null = empty)
}
```

## Key details

- **Minimum block size:** `size_of::<FreeNode>()` = 8 bytes. When allocating, round up to at least 8.
- **Alignment:** when scanning the free list, skip blocks that can't satisfy the requested alignment.
- **No coalescing needed yet** — adjacent free blocks aren't merged. This causes fragmentation but keeps the code simple. Add coalescing later if 64KB proves tight.
- **The free list is protected by the existing SpinLock** — no new synchronization needed.
- **~20-30 lines of new code** on top of the existing bump allocator.
