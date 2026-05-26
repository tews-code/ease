# Profile-guard plan

## Goal

Instrument selected kernel functions to capture entry/exit cycles and sp depth,
then play back the records to identify which functions consume the most stack
and time. Initial target: locate the function in the virtio interrupt path that
overflows the debug-build IRQ stack.

## Approach

RAII guard records entry on construction, exit on `Drop`. Records land in a
per-HART `SpscRingBuf`. A `dump()` function (invoked from a shell command)
drains the buffers and prints. Once hand-instrumentation works end-to-end, a
proc macro `#[profile]` automates inserting the guard.

## Components

### 1. `ProfileRecord` (Copy)

| field    | type           | source                       |
|----------|----------------|------------------------------|
| `name`   | `&'static str` | call site                    |
| `cycles` | `u64`          | `arch::rdcycles()`           |
| `sp`     | `usize`        | inline asm `mv {}, sp`       |
| `hart`   | `u8`           | `arch::cpu_id()`             |
| `kind`   | `u8`           | `Entry` (0) or `Exit` (1)    |

### 2. Storage

Two `static SpscRingBuf<ProfileRecord, 256>` instances, one per HART. Each HART
is the single producer for its own buffer (satisfies SPSC invariant). Dump is
the single consumer.

Buffer full → `push` returns `Err`. Records dropped silently. Size 256 is
generous for a typical capture; bump if needed.

### 3. `ProfileGuard`

```text
ProfileGuard::new(name) ->
    push Record { name, cycles=rdcycles(), sp=read_sp(), hart=cpu_id(), kind=Entry }

impl Drop ->
    push Record { name, cycles=rdcycles(), sp=read_sp(), hart=cpu_id(), kind=Exit }
```

Hot-path discipline: no formatting, no allocation, no locking. Just CSR reads,
sp read, and one atomic push.

### 4. `dump()`

Pops records from both buffers and prints each as one line. Initial format is
raw chronological:

```
[hart] kind name cycles sp
```

Pair-matching (entry/exit deltas, tree view) is a later improvement once the
raw capture is solid.

### 5. Trigger

New shell command (`profile`) that calls `dump()`. Allows on-demand capture
inspection without rebooting.

## Build order

1. Create `kernel/profile.rs`. Define `ProfileRecord`, the two static buffers,
   `ProfileGuard` with `new`/`Drop`, and `dump()`. Compile.
2. Add `profile` shell command in `shell/commands.rs`. Verify dump prints
   nothing on a fresh boot (buffers empty).
3. Hand-instrument one function with `let _g = ProfileGuard::new("name");` —
   start with `trap_handler` since that's the diagnostic target. Verify dump
   shows entry/exit records after a few traps.
4. Hand-instrument the rest of the trap-path call chain
   (`handle_external_irq`, `handle_virtio_interrupt`, `mark_for_preempt`,
   `with_plic`'s closure body, etc.). Trigger a virtio IRQ overflow scenario
   (cargo run, debug build), dump and analyze.
5. Once useful, replace `let _g = ProfileGuard::new("...")` with `#[profile]`
   attribute via the proc macro in `ease-macros`.

## Open decisions

- **Overflow behaviour:** stays `Err`-on-full for now. Revisit if buffer is
  consistently exhausted before dump.
- **Pair matching in dump:** raw chronological for v1. Add tree-view + per-call
  cycles/sp deltas if the raw output is too dense to read by eye.
- **Buffer size:** 256 records (≈ 6 KiB). Increase if traces routinely
  truncate.
