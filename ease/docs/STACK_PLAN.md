# Stack tightening plan

Goal: shrink the per-core stack reservation from 64 KB to 4 KB, matching
the RP2350 bootrom's scratch-SRAM convention, without breaking the code.

Guiding principle: measure before cutting, migrate incrementally, keep
the tree compiling at every step.

## Step 1 — measure before cutting

Paint the unused stack region with a known pattern at startup; later
walk the region bottom-up and find the highest address still painted.
The distance from that word to `__stack_top` is the observed peak
stack usage.

- In `_start`, after BSS zeroing, loop from `__stack_guard` upward,
  writing `0xDEADBEEF` (or similar) every 4 bytes, stopping a safe
  distance below the current `sp`.
- Add `stack_used_peak() -> usize` in Rust that walks the same region
  and returns the peak.
- Wire it to a `stack` shell command so the number can be read
  interactively.

This change is compile-clean and invisible to everything else — good
first commit.

## Step 2 — exercise representative workloads

With the watermark in place, drive the system through representative
code paths: run tests, use the shell, trigger a panic, exercise the
filesystem and virtio-blk paths. Record peak values.

Decision point:
- Peak < 3.5 KB: jump to Step 4.
- Peak ≥ 3.5 KB: continue to Step 3.

## Step 3 — migrate big stack objects

The usual suspects are long-lived collections allocated on the stack.
Find candidates by size:

    rg 'StackVec<.*, *[0-9]+' src/
    rg 'RingBuf<.*, *[0-9]+' src/

Rank by `N * sizeof(T)`. For each, choose:

- Move to `'static` (in `.bss`) if truly singleton — simplest, no
  allocator dependency.
- Move to `Box<T>` / `Vec<T>` if per-task or per-call — requires the
  buddy allocator to be production-ready.
- Shrink `N` if oversized for real workloads.

Migrate one at a time. Each migration is its own commit, each re-runs
the watermark to confirm forward progress. Stop when peak is under
budget.

## Step 4 — shrink the reservation

Only once the watermark says we're under 3.5 KB (leaving headroom):

- Reduce the distance between `__stack_top` and `__heap_end` in the
  linker script to 4 KB.
- Consider growing `.stack_guard` from 4 bytes to 256 bytes and
  checking the guard words periodically (timer tick, shell prompt,
  trap entry) so overflow becomes a catchable fault rather than silent
  corruption.
- Update the memory-layout diagram at the top of `memory-qemu.x` to
  reflect the tighter budget.

## Post-condition

- Per-core stack: 4 KB.
- Stack guard region: ≥ 256 bytes, periodically verified.
- Recovered SRAM: ~60 KB, available to the heap.
- Documented peak stack usage on representative workloads.
