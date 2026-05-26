# Best use of SRAM8 (and SRAM9) on EASE

A concrete plan for filling the per-core 4 KB SRAM banks with the things
that actually benefit from being in tightly-coupled fast memory: the IRQ
stack, a per-hart percpu block, and the hot-path code (trap vector,
context switch, timer ISR).

Captured 2026-05-09.

## 0. Why this matters

Real RP2350 SRAM8 and SRAM9 are two 4 KB *non-striped* scratch blocks
at fixed addresses (`0x20080000` and `0x20081000`), both in power
domain SRAM1. They sit outside the striped main pool, so they don't
contend with it on the bus. The datasheet (§2.2.3) calls them out as
good homes for high-bandwidth structures like processor stacks; by
convention SRAM8 is paired with core0 and SRAM9 with core1, so each
core gets a private fast block. On QEMU virt this distinction
vanishes, but the layout you compile to must already be hart-correct
so that the same binary runs on metal. **Therefore HART0's percpu
block lives in SRAM8 and HART1's in SRAM9** — by convention, not
hardware enforcement, but never deviate.

The current layout uses the entire 4 KB of SRAM8 as HART0's boot stack
(and 4 KB of SRAM9 for HART1's boot stack). That's a perfectly reasonable
starting point, but it leaves the *real* TCM wins on the table:

- The IRQ stack switch (via `mscratch`) is the textbook RISC-V technique
  for making trap entry cheap. Putting that stack into SRAM8 means trap
  entry never touches a thread cacheline.
- A `PerCpu` block in SRAM8 makes "what's running on this hart?" a single
  load from the fastest bank instead of a global-lock + array index.
- Putting the trap vector and `switch_to` into `.sram8_text` means the
  hot path of the kernel is fetched at L0 speeds.

Realistic SRAM8 budget for HART0 (4096 bytes total):

| Region                        | Size      | Notes                                   |
|-------------------------------|-----------|------------------------------------------|
| `.sram8.irq_stack`            | 1024 B    | Used by trap entry only                  |
| `.sram8.percpu`               | ~256 B    | `PerCpu` struct (current TCB, tick, etc.) |
| `.sram8_text` (hot code)      | ~2 KB     | `_trap_vector`, `switch_to`, `wfi` idle  |
| slack                         | ~800 B    | Headroom for inlining drift              |

## Power-domain constraint (RP2350 datasheet §6.2.1)

The RP2350 splits its core logic into five power domains; SRAM is
covered by two of them. From §6.2.1:

> **SRAM1** — SRAM Power Domain 1 — the upper half of the large SRAM
> banks, **and the scratch SRAMs**.

So SRAM8 and SRAM9 (the two non-striped scratch banks) sit in PD1
alongside SRAM4–7 (the upper 256 KB of the striped main pool, four
banks of 64 KB each). If SRAM1 is powered down, both scratch banks
lose their contents — as does the entire upper half of the striped
main pool. This mapping is fixed — we cannot place scratch RAM in a
different domain.

Implications for this plan:

- **No retention across a sleep that drops SRAM1.** Anything we put in
  SRAM8 (percpu block, IRQ stack, copied `.sram8_text`) must either be
  reinitialised at wake or only relied on while SRAM1 is up. The boot
  copy-loop in §2 is already idempotent on cold boot, so `.sram8_text`
  handles this naturally; the percpu block needs explicit re-zeroing
  on any wake path that came through a SRAM1-off state.
- **The pairing is asymmetric.** SRAM0 (lower half of the large banks)
  can be off while SRAM1 (upper half + scratch) stays on, and vice
  versa. A future "tickless idle, scratch holds the world" mode is
  achievable: keep SRAM1 + AON up, drop SRAM0 and SWCORE.
- **SWCORE off does not kill SRAM1.** Per the datasheet: *"SRAMs that
  are powered on retain their contents when the switched core is
  powered off."* That's the explicit permission for the pattern above.

None of this changes the QEMU-side rollout — QEMU has no power
domains — but it does mean any future low-power work must treat
"is SRAM1 currently powered?" as load-bearing state for the scheduler.

## 1. Linker script changes (`memory-qemu.x`)

Replace the current `.sram8 (NOLOAD)` block with this. The key new ideas:

- A loaded `.sram8_text` section whose **LMA is FLASH**, **VMA is SRAM8**
  (copied at boot like `.data`).
- Two NOLOAD subsections for the stack and percpu.
- Symbols for each region so Rust/asm can `la` against them.
- `mscratch`-style IRQ stack top.

```ld
/* Hot code copied from FLASH to SRAM8 at boot (LMA != VMA) */
.sram8_text 0x80080000 : ALIGN(4) {
    __sram8_text_start = .;
    *(.sram8_text .sram8_text.*)
    . = ALIGN(4);
    __sram8_text_end = .;
} > SRAM AT > FLASH
__sram8_text_lma = LOADADDR(.sram8_text);

/* HART0 IRQ stack — grows down from __hart0_irq_stack_top */
.sram8_irq_stack : ALIGN(16) {
    __hart0_irq_stack_start = .;
    . = . + 1K;
    __hart0_irq_stack_top = .;
} > SRAM

/* HART0 percpu block — zero-initialised, single instance */
.sram8_percpu (NOLOAD) : ALIGN(8) {
    __hart0_percpu_start = .;
    . = . + 256;
    __hart0_percpu_end = .;
} > SRAM

/* Whatever fits in the remaining ~2.7K becomes a tiny task stack
 * for the boot thread; keep your existing __hart0_stack_top symbol
 * pointing at the *end* of the 4K bank for back-compat. */
.sram8_task_stack (NOLOAD) : ALIGN(16) {
    __hart0_stack_start = .;
    . = 0x80081000 - 0x10;       /* fill to end of SRAM8 bank */
    __hart0_stack_top = .;
} > SRAM

ASSERT(__sram8_text_end <= __hart0_irq_stack_start, ".sram8_text overflows IRQ stack");
ASSERT(__hart0_percpu_end <= __hart0_stack_start,   "percpu overflows task stack");
ASSERT(__hart0_stack_top <= 0x80081000,             "HART0 SRAM8 bank overflow");
```

Then do the *mirror* for SRAM9 (`__hart1_*`, `0x80081000..0x80082000`).
You only need `.sram8_text` once; the *same physical code* is fetched by
both cores (real HW: both can read either bank, just slower across the
divide).

## 2. Boot: copy `.sram8_text` from FLASH and set `mscratch`

In `src/arch/boot.rs`, extend `_start`:

```rust
unsafe extern "C" {
    static __sram8_text_start: u8;
    static __sram8_text_end: u8;
    static __sram8_text_lma: u8;
    static __hart0_irq_stack_top: u8;
    static __hart1_irq_stack_top: u8;
    static __hart0_percpu_start: u8;
    static __hart1_percpu_start: u8;
}
```

In the asm body, after the BSS-zero loop and *before* `csrw mtvec`, add a
`.sram8_text` copy loop (HART0 only — HART1 spins on `INIT_COMPLETE`):

```asm
// Copy .sram8_text from FLASH to SRAM8 (HART0 only)
"la t0, {sram8_text_lma}",
"la t1, {sram8_text_start}",
"la t2, {sram8_text_end}",
"5:",
"bge t1, t2, 6f",
"lw t3, 0(t0)",
"sw t3, 0(t1)",
"addi t0, t0, 4",
"addi t1, t1, 4",
"j 5b",
"6:",
```

Then, **on both harts**, after `mtvec` is set:

```asm
// Park IRQ stack top in mscratch so the trap vector can swap to it
"la t0, {hartN_irq_stack_top}",
"csrw mscratch, t0",
```

(HART1 uses `__hart1_irq_stack_top`; you may want a small subroutine.)

## 3. Trap entry: switch to the IRQ stack via `mscratch`

This is the largest behaviour change. Today `_trap_vector` saves the
frame on whatever stack the interrupted thread was using — so every
thread stack must reserve `sizeof(TrapFrame) = 128 B` of headroom *plus*
whatever the trap handler then spends. With a per-hart IRQ stack, you
reclaim that headroom and you get the trap handler's hot path executing
out of fast SRAM8.

Replace the top of `_trap_vector` in `src/arch/trap.rs`:

```asm
.section .sram8_text, "ax"      ; <-- the whole vector lives in SRAM8 now
.global _trap_vector
.align 4
_trap_vector:
    # Swap sp <-> mscratch: now sp = IRQ stack top, mscratch = thread sp
    csrrw sp, mscratch, sp

    # Push the thread sp onto the IRQ stack
    addi sp, sp, -4 * 32
    csrr t0, mscratch              # t0 = thread sp (what we swapped out)
    sw   t0, 4 * 30(sp)            # stash thread sp in a frame slot
    # ... rest of the save sequence as today, but mepc/mstatus stay where
    # they are and we add a slot for the thread sp.
```

Two ways to do this:

**Option A (minimal):** widen `TrapFrame` by one word (`saved_sp:
usize`), keep everything else the same.

**Option B (cleaner):** keep `TrapFrame` exactly as-is, but at the *top*
of the IRQ stack reserve a 4-byte slot that the vector writes to. The
downside: requires a fresh `addi sp, sp, -4` before the frame push.

On exit, before `mret`:

```asm
    # Restore thread sp into mscratch, restore IRQ-stack sp temporarily
    lw   t0, 4 * 30(sp)            # thread sp
    addi sp, sp, 4 * 32            # pop our frame from IRQ stack
    csrw mscratch, t0              # park thread sp back in mscratch
    csrrw sp, mscratch, sp         # sp = thread sp, mscratch = IRQ top again
    mret
```

This is the classic RISC-V "mscratch holds the IRQ stack top while the
thread runs; trap entry swaps to it" pattern (xv6 / SiFive bootloaders).
With it:

- Thread stacks shrink — no more reserving 128+B of frame.
- Trap entry/exit no longer pollutes thread cachelines.
- The vector itself lives in SRAM8 (`.sram8_text` section attribute on
  `global_asm!`).

`switch_to` (in `arch/context.rs`) gets the same `.section .sram8_text`
attribute — drop a section directive in the `global_asm!`:

```rust
global_asm!(r#"
    .section .sram8_text, "ax"
    .global switch_to
    .align 4
    switch_to:
        ...
"#);
```

## 4. The `PerCpu` block

Create `src/kernel/percpu.rs`:

```rust
use core::cell::UnsafeCell;
use crate::board::HARTS_MAX;

#[repr(C, align(8))]
pub struct PerCpu {
    /// Index into Scheduler's TCB array of the thread running on this hart.
    pub current_tcb: UnsafeCell<usize>,
    /// Most-recently-observed wall tick (millis); used to short-circuit
    /// the deadline scan in wake_threads.
    pub last_tick_ms: UnsafeCell<u64>,
    /// Saved trap depth (nested IRQs); useful for assertions.
    pub trap_depth: UnsafeCell<u32>,
    /// IRQ stack top for this hart — duplicated from mscratch for
    /// cheap reads from Rust.
    pub irq_stack_top: UnsafeCell<usize>,
    // Pad to ~256 B as you grow.
    _pad: [u8; 256 - 8 - 8 - 4 - 8],
}

unsafe extern "C" {
    #[link_name = "__hart0_percpu_start"]
    pub static HART0_PERCPU: PerCpu;
    #[link_name = "__hart1_percpu_start"]
    pub static HART1_PERCPU: PerCpu;
}

#[inline(always)]
pub fn this_cpu() -> &'static PerCpu {
    match crate::arch::cpu_id() {
        0 => unsafe { &HART0_PERCPU },
        1 => unsafe { &HART1_PERCPU },
        _ => unreachable!(),
    }
}
```

Then in `Scheduler::reschedule`/`preempt`, replace

```rust
threads.running_on_hart[cpu_id()] = next_idx;
```

with

```rust
unsafe { *this_cpu().current_tcb.get() = next_idx; }
```

The `running_on_hart` array goes away, and lookups become a single load
from SRAM8. Same for `last_tick_ms` — `wake_threads` can now skip its
scan when `ticks_ms() == self.last_tick_ms` on this hart.

The percpu block being in SRAM8 means **every reschedule reads/writes
hot fields from the fastest bank**, which is the dominant cycle-count
win you're after.

## 5. Mark Rust functions for SRAM8 placement

Annotate the hot leaf functions with a section attribute so LLVM emits
them into `.sram8_text`:

```rust
#[unsafe(link_section = ".sram8_text")]
#[inline(never)]
pub fn handle_interrupt() { /* timer ISR */ }

#[unsafe(link_section = ".sram8_text")]
#[inline(never)]
extern "C" fn trap_handler() { /* ... */ }
```

Be selective:

- `trap_handler` (Rust side) — yes
- `kernel::timer::handle_interrupt` — yes
- `Scheduler::preempt` — yes
- `Scheduler::get_current_and_next_mut` — maybe; it's only called from
  `preempt`/`reschedule`. If LLVM inlines, you get it for free.
- `arch::wait_for_interrupt` — sure, it's two instructions.

Check the totals with:

```bash
cargo build --release && \
  riscv32-unknown-elf-size -A target/riscv32imac-unknown-none-elf/release/ease \
  | grep sram8
```

Iterate by moving items in or out until `.sram8_text` is comfortably
under ~2 KB.

## 6. Reclaim the headroom from thread stacks

Once trap entry stops using the thread stack, you can drop
`StackClass::KB1` from "minimum that holds a TrapFrame" to "minimum that
holds a Context". Your `_MIN_CLASS_SIZE_CHECK` becomes:

```rust
const _MIN_CLASS_SIZE_CHECK: () =
    assert!(StackClass::KB1.size() >= core::mem::size_of::<Context>() + 64 /* slack */);
```

That's a free 25 % reduction in per-thread memory for small threads.

## 7. Test it

Add a tiny test to `roundrobin.rs`:

```rust
#[cfg(all(test, feature = "test-sched"))]
#[test_case]
fn percpu_in_sram8() {
    let addr = &crate::kernel::percpu::HART0_PERCPU as *const _ as usize;
    assert!((0x80080000..0x80081000).contains(&addr),
        "HART0 percpu not in SRAM8: {:#x}", addr);
}
```

And a bench that asserts the trap-entry path got faster. With your
existing `bench::measure`, wrap a forced timer interrupt and read
`cpu`/`wall`. You should see context-switch round-trip drop noticeably on
real HW (and stay the same on QEMU, because QEMU SRAM is just RAM).

## 8. Order of operations (safe rollout)

1. Land the linker-script split (no behaviour change yet —
   `__hart0_stack_top` still points at end of SRAM8).
2. Add the `PerCpu` struct and use it as a *read-through cache* (still
   write `running_on_hart` too).
3. Move `_trap_vector` and `switch_to` into `.sram8_text` (still on
   thread stack).
4. Switch trap entry to `mscratch`-swap onto `.sram8_irq_stack`.
5. Delete `running_on_hart` once the percpu copy is the source of truth.
6. Annotate Rust hot functions with `#[link_section = ".sram8_text"]`
   and tune.

Each step is independently testable, and each one gives you a measurable
cycle improvement on metal (steps 3 and 4 are the big ones).

     
  A couple of small things to keep in mind as you start working through the rollout:

  - QEMU won't show you the speed-up. On QEMU virt, all RAM is the same. So .sram8_text and the IRQ-stack swap won't make benchmarks go
  faster there — they'll be break-even. The real numbers only show up on the Pico 2. That's actually fine: you want the layout to be
  correct in CI long before you flash metal.
  - The mscratch swap is the high-leverage step. Steps 3 and 4 in the rollout are where the cycle savings live. Steps 1, 2, 5, 6 are
  mostly housekeeping — but they unlock step 4 cleanly.
  - Watch for .sram8_text over-fill. Every time you add a #[link_section = ".sram8_text"] annotation, re-run the size -A check. LLVM is
  happy to keep inlining call-graph dependents into your hot function until the section overflows. The linker ASSERT will catch it, but
  earlier feedback is nicer.


