# kalloc + PMP redesign for user-mode apps on EASE

A design for two coupled changes that together unblock user-mode app
support on RP2350:

1. Drop the NAPOT requirement from kernel-internal `kalloc`, replacing
   the stack-pointer bitmask trick with an explicit `stack_base_ptr`
   in each TCB.
2. Introduce a separate NAPOT-only allocation path for user-app
   memory regions, sized to the Hazard3 PMP encoding rules.

The two changes are independently motivated — arbitrary-sized heap
allocations are a quality-of-life win on their own — but they share a
single underlying picture of how memory is partitioned between kernel
and user, so they belong in one doc.

Captured 2026-05-19, from a design conversation on the same date.

## 0. Why this matters

Today, `kalloc` returns NAPOT-aligned, NAPOT-sized blocks, and the
"find the current TCB from any SP" lookup is done by masking the SP
with `STACK_MASK`. That couples three things together:

- Heap allocation granularity (forced to powers of two).
- Stack alignment (must equal stack size).
- Current-TCB lookup (depends on the stack alignment invariant).

Two of those couplings get in the way as soon as we try to run
user-mode apps under PMP sandboxing on the Hazard3 cores:

- Kernel heap can't grow incrementally — it can only *double*. That
  caps the practical kernel heap size well below available SRAM.
- User-app stacks have their own PMP-driven NAPOT requirement, which
  is fundamentally different from the kernel's: it comes from the
  hardware, not from a software convenience.

The cleanest way out is to split the two cases. Kernel-internal
allocations become arbitrary-sized; user-app regions go through a
dedicated NAPOT-only path that exists specifically to feed the PMP
encoder.

## 1. Hazard3 PMP constraints (the hardware facts)

From the RP2350 datasheet §10.4:

- **8 dynamic PMP regions per core**, plus 3 hardwired regions
  (indexes 8–10) for peripherals (U-mode R+W) and ROM (U-mode R+X).
- **NAPOT addressing only.** Hazard3 supports neither TOR nor NA4.
  Every protected region is power-of-two-sized and naturally aligned.
- **32-byte minimum granule.** The smallest possible region is 32 B
  (NAPOT order 5).
- **M-mode is unfiltered by default.** The Hazard3-specific
  `PMPCFGM0` register opts M-mode into the same filter as U-mode
  without using the standard L bit's one-way lock — useful for
  debug-build protection against accidental kernel writes.
- **Lower-numbered regions take precedence.** Dynamic regions 0–7
  can override the hardwired peripheral/ROM rules.
- **Default-deny in U-mode** for any address outside all defined
  regions.
- **Per-hart.** Each core has its own independent PMP register set.

> Note: RP2350 also provides **ACCESSCTRL**, a separate bus-filter
> mechanism for peripheral access. It's complementary to PMP and is
> out of scope for this doc.

## 2. Kernel/user memory split

The redesign rests on one principle:

**PMP protection is required only for U-mode memory. M-mode
(kernel) memory needs no PMP rule by default, and therefore no
NAPOT constraint.**

This gives us two allocation paths:

- **Kernel-internal: arbitrary sizes, no NAPOT.** Kernel heap,
  kernel-thread stacks, TCBs, kernel data structures. Backing
  allocator can be freelist / slab / buddy — choice is independent
  of this design.
- **User-app regions: NAPOT-only.** Allocated through a dedicated
  path used by the app loader. Every region must be a power-of-two
  size, naturally aligned, and ≥32 bytes.

User apps never call either allocator directly — they receive
pre-allocated regions from the loader.

## 3. Allocator API

```
kalloc(size: usize) -> *mut u8
    Arbitrary-size allocation. Replaces today's NAPOT-only kalloc as
    the default path for all kernel-internal callers.

kalloc_napot(order: u8) -> *mut u8
    Single NAPOT-sized, NAPOT-aligned block of 2^order bytes.
    Minimum order is 5 (32 B). Used by the loader for user-app
    regions; nothing else should call this.

kfree(ptr, size_or_order) -> ()
    Handles both paths.
```

The minimum order of 5 isn't optional — it's the PMP granule. The
allocator should reject smaller requests rather than silently
rounding up.

## 4. TCB changes

```
pub struct Tcb {
    ...
    pub stack_base_ptr: NonNull<u8>,
    pub stack_size: usize,
    ...
}
```

Both fields are NAPOT-quantised for user threads (so they double as
the PMP region descriptor for the stack) and arbitrary for kernel
threads.

The SP-mask `current_tcb()` lookup is retired and replaced by a load
from the SRAM8 percpu block — see `best-use-of-SRAM8.md` for the
mechanism. Net effect: TCB lookup is decoupled from stack layout
entirely.

## 5. PMP layout per app

Five regions, leaving 3 dynamic entries free as headroom:

| # | Region                          | Perm | Updated on    |
|---|---------------------------------|------|---------------|
| 0 | code (XIP or copied)            | R+X  | app switch    |
| 1 | rodata                          | R    | app switch    |
| 2 | data + bss + heap               | R+W  | app switch    |
| 3 | *current thread's* stack        | R+W  | thread switch |
| 4 | shared data (e.g. WAD in PSRAM) | R    | app switch    |

Thread switch within the same app rewrites only entry 3 (one
`csrw pmpaddr3`, cfg byte unchanged). App switch rewrites entries
0–4. Both are cheap.

If an app needs more shared regions (multiple data files, IPC
buffers), they spill into entries 5–7.

### Worked example: Doom

Doom with two user threads (main + sound), WAD file in PSRAM:

- Entries 0–2 cover the Doom binary's code/rodata/data segments.
  Identical for both threads.
- Entry 4 covers the WAD in PSRAM. R-only; same for both threads.
- Entry 3 swaps between main-thread stack and sound-thread stack on
  thread switch.

If the two threads run on different cores, each core programs its
own copy of entries 0–2 and 4, with entry 3 pointing at the locally
running thread's stack.

The WAD itself must live at a NAPOT-aligned PSRAM address and be
padded to a power-of-two size at packaging time. A 10 MB WAD
either rounds up to a 16 MB region (waste) or splits into two
NAPOT regions (costs an extra PMP entry and complicates the mmap
layout).

## 6. Stack-overflow detection (free)

For user threads, no dedicated guard region is needed. With U-mode
default-deny outside defined regions, an overflow off the bottom of
the stack lands in unmapped territory and traps as a PMP access
fault. The handler:

1. Reads `mtval` (faulting address) and `mcause` (= PMP access fault).
2. Looks up `current_tcb.stack_base_ptr` from the percpu block.
3. If `mtval` is immediately below `stack_base_ptr`, it's a stack
   overflow on this thread — report and terminate per policy.
4. Otherwise, it's a generic access violation — terminate the app.

This is a key payoff: the bitmask trick can't do step 3 cleanly,
because once SP has overflowed past the stack base, masking it no
longer recovers the correct TCB. The explicit `stack_base_ptr` does.

For kernel threads (M-mode, PMP unfiltered):

- Size kernel stacks conservatively. You control all kernel code, so
  max depth is auditable.
- Drop a magic-value canary at the bottom of each kernel stack and
  check it on context switch.
- Optionally, in debug builds, place a 32-byte NAPOT deny region
  just below each kernel stack and use `PMPCFGM0` to make it filter
  M-mode too. Spends one of the 8 dynamic entries, so debug only.

## 7. SRAM8 implications

See `best-use-of-SRAM8.md` for the full plan. With user apps in
play, three pieces of the SRAM8 layout become *more* valuable, not
less:

- **IRQ stack.** Every U↔M-mode transition uses it. Frequent.
- **PerCpu block.** Read on every reschedule.
- **`.sram8_text` hot code.** Trap vector, `switch_to`, idle WFI —
  all on the hot path of mode transitions.

The ~800 B leftover slot in SRAM8 (currently designated as a
generic kernel task stack) is best dedicated to the idle thread.
Idle's stack is tiny, idle runs whenever no other thread is ready,
and its WFI loop is already in `.sram8_text` — so the entire idle
path ends up SRAM8-resident.

General kernel threads (filesystem, services) won't fit in 800 B
and live in main SRAM via the new arbitrary `kalloc`.

## 8. QEMU vs metal: testing strategy

QEMU's `virt` machine supports PMP, but its capabilities differ
from Hazard3 in ways that can produce code that works on QEMU and
breaks on metal:

|                   | QEMU virt                  | Hazard3 (RP2350)    |
|-------------------|----------------------------|---------------------|
| Addressing modes  | OFF, TOR, NA4, NAPOT       | NAPOT only          |
| Dynamic entries   | typically 16               | 8                   |
| Hardwired entries | 0                          | 3 (peripherals+ROM) |
| Granule           | down to 4 B                | 32 B                |
| M-mode opt-in     | L bit (one-way lock)       | PMPCFGM0 (revocable)|

Encode the Hazard3 limits as kernel-wide invariants from day one:

- `pub const MAX_DYNAMIC_PMP_REGIONS: usize = 8;`
- `pub const PMP_GRANULE_BYTES: usize = 32;`
- The PMP API refuses non-NAPOT regions at the type level (an enum
  whose only variant is `Napot { base, order }`).
- `debug_assert!` against any violation, fires on both QEMU and
  metal.

The one path that genuinely differs between platforms is
peripheral-access setup: QEMU needs explicit dynamic entries;
Hazard3 inherits the hardwired ones. Use a `#[cfg]` boundary.

Two Hazard3 behaviours can't be faithfully tested on QEMU:

- `PMPCFGM0` doesn't exist on QEMU. M-mode filtering via the
  standard L bit is testable but is one-way.
- The 3 hardwired regions don't exist on QEMU.

## 9. Order of operations

Each step is independently testable on QEMU. Land in this order:

1. Add `stack_base_ptr` and `stack_size` to TCB. Populate on thread
   spawn. Keep the bitmask trick working in parallel.
2. Add the SRAM8 percpu lookup path (depends on
   `best-use-of-SRAM8.md` steps 1–2).
3. Switch `current_tcb()` to read from percpu; retire the bitmask
   trick. Stack allocations can now be arbitrary-aligned.
4. Add the arbitrary-size `kalloc` path. Backing allocator choice
   (freelist / slab / buddy) is independent.
5. Rename today's NAPOT kalloc to `kalloc_napot`. Migrate
   kernel-internal callers to the new `kalloc`.
6. Build the `Pmp` module with the Hazard3 constants and the
   NAPOT-only API.
7. Write the per-app PMP setup and the trap handler that maps PMP
   access faults to stack-overflow vs generic access-violation.
8. Loader work for user apps. Out of scope here — this is what
   the redesign unblocks.

Steps 1–5 are kernel-internal and can land on `develop` without
gating on the loader. Steps 6–7 are testable in isolation by
having the kernel itself enter U-mode and trip its own rules.

## 10. Open questions

- **Which backing allocator for the arbitrary-size `kalloc`?**
  Freelist is simplest; slab gives best utilisation for the
  fixed-size-class workload EASE has so far; buddy is a middle
  ground. Decide once the migration is staged.
- **Stack overflow policy for kernel threads.** Canary on context
  switch is cheap, but the canary placement assumes you know where
  the stack ends — which you do once `stack_base_ptr` exists. Wire
  it in step 1.
- **App-switch PMP register write order.** Lowest-numbered region
  wins on match, so the safe transition is: zero all dynamic
  entries first (deny-all), then install the new app's rules
  bottom-up. Worth measuring vs. an in-place rewrite.
