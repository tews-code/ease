# Deferred preemption

A plan to decouple the scheduling *decision* (made inside the trap
handler) from the scheduling *action* (`switch_to` itself). Required
before the SRAM8 mscratch IRQ-stack swap (step 4 of
`best-use-of-SRAM8.md`) can be applied safely, and also a more
defensible design in its own right.

Captured 2026-05-21.

## 0. Why this matters

Today, the timer ISR ends up calling `Scheduler::preempt`, which calls
`switch_to` *synchronously, from inside the trap handler*. That means:

1. Trap entry pushes a trap frame onto **whatever stack the
   interrupted thread was using** — currently its own thread stack.
2. The handler decides to switch, calls `switch_to`. `switch_to`
   stores its callee-saved state on the same stack and saves `sp`
   into the outgoing thread's TCB.
3. Control flow tail-jumps into the incoming thread's saved state —
   "returning" from the incoming thread's own previous `switch_to`
   call, deep inside *its* own pending trap handler.
4. Eventually the incoming thread's trap handler reaches `mret`,
   pops its own trap frame from its own stack, returns to user code.

This works *only* because each thread's trap frame and `switch_to`
save-area live on **that thread's own stack**. They persist across an
arbitrary number of context switches because each thread carries its
own kernel state with it.

The SRAM8 plan wants to push the trap frame onto a **shared per-hart
IRQ stack** instead (via the mscratch swap). On a shared stack, frames
from different threads collide:

- Thread A is interrupted: frame pushed at `irq_top - 128`.
- Handler calls `switch_to` to thread B. A's `switch_to` save-area
  ends up at `irq_top - 128 - X` on the same shared stack. A's TCB
  records `A.sp = irq_top - 128 - X`.
- Thread B runs, is interrupted later. B's trap entry pushes its
  frame at `irq_top - 128` — overwriting A's stale state.
- B's handler decides to switch back to A. `switch_to` loads
  `sp = A.sp = irq_top - 128 - X` and restores callee-saved regs
  from there — but those bytes are now B's clobbered trap frame.
  Garbage gets restored into `ra` etc.; the kernel hangs or jumps
  wild.

So the SRAM8 IRQ-stack idea is **incompatible with synchronous
preemption from inside a trap handler**. To unblock it, we need to
arrange that `switch_to` never runs on the IRQ stack — only on
per-thread stacks. That's what deferred preemption is for.

## 1. The two-phase model

Split the existing `preempt` flow into two phases that run in
different contexts:

- **Phase 1 — Decide (inside the trap handler).** Run the
  bookkeeping that needs trap-time observation: wake sleeping
  threads, account stride, advance the slice deadline. If the
  outcome of that work is "the current thread should be replaced,"
  set a per-hart `need_resched` flag. **Do not call `switch_to`.**
- **Phase 2 — Act (outside the trap).** When kernel code reaches a
  *preemption point* (defined below) and sees `need_resched`
  asserted, it calls `schedule()`. `schedule()` performs the actual
  `switch_to` — but now `sp` is on the current thread's own kernel
  stack, exactly as it would be for a voluntary `yield_now`. The
  IRQ stack is empty and unaffected.

The invariant that makes the SRAM8 swap safe: **the IRQ stack only
ever holds at most one trap frame, always pushed and popped within
the same trap entry/exit pair, never observed by `switch_to`.**

## 2. The `need_resched` flag

Add a per-hart boolean. The natural home is inside `PerCpu`:

```rust
#[repr(C, align(8))]
pub struct PerCpu {
    pub current_thread_idx: UnsafeCell<usize>,
    pub current_stack_base: UnsafeCell<*mut u8>,
    pub switching_thread_idx: UnsafeCell<Option<usize>>,
    pub need_resched: AtomicBool,              // <-- new
}
```

Putting it in `PerCpu` (rather than a separate static array) means:

- It rides into SRAM8/SRAM9 with the rest of the per-hart hot data
  — cheap to read from the trap path *and* from kernel code.
- The fields are already cache-line co-located, which matters on
  real HW when the trap handler reads `current_thread_idx` and
  `need_resched` in quick succession.

Access pattern:

- `this_cpu().need_resched.store(true, Release)` from the trap
  handler when a switch is wanted.
- `this_cpu().need_resched.swap(false, Acquire)` from `schedule()`
  to consume the request.

`AtomicBool` is sufficient — `need_resched` is set by the trap
handler (interrupt context) and consumed by kernel code on the same
hart, both running on the same physical core. Acquire/Release pairs
the consume with the set.

## 3. Refactoring `preempt`

Today's `Scheduler::preempt` (stride.rs ~487) does five things:

1. Wakes sleeping threads (`wake_sleeping_threads`).
2. Computes the earliest deadline.
3. Accounts stride for the running thread (`curr.stride(ran)`).
4. Picks the next thread (`pick_next_if_fairer_mut`) and updates
   state to `Switching`/`Running`.
5. Calls `switch_to`.

Steps 1–4 are decision/bookkeeping and must run from trap context
to be responsive (we want the accounting to reflect the moment the
timer fired). Step 5 is the action.

Split it:

```rust
/// Called from the timer ISR. Performs all per-tick accounting
/// and decides whether the current thread should yield. Sets
/// need_resched on this hart if so. Never calls switch_to.
pub(super) fn mark_for_preempt(&self) {
    // (steps 1-4 of the old preempt, ending with a state-only
    // transition: curr.state = Switching, next.state = Running)
    // If a switch was selected:
    percpu::this_cpu().need_resched.store(true, Release);
}

/// Called from non-trap kernel code at a preemption point.
/// Consumes need_resched and performs the actual switch_to
/// on the current thread's own stack.
pub fn schedule() {
    if !percpu::this_cpu().need_resched.swap(false, Acquire) {
        return;
    }
    // Re-pick the next thread (it may have changed since
    // mark_for_preempt set the flag — see "Subtleties" below).
    // ... read curr/next from TCBs, drop lock, switch_to ...
}
```

Crucially: `mark_for_preempt` sets up the state transitions
(`Switching` / `Running`) but does *not* drop the scheduler lock and
*does not* call `switch_to`. `schedule` either reuses the prepared
transition (if next-thread state hasn't drifted) or re-picks.

Edge case to design for: `mark_for_preempt` selects thread N, sets
the flag — then thread N exits (or gets a different reason to be
non-Ready) before `schedule()` runs. `schedule()` must defensively
re-pick rather than blindly switch to a non-runnable thread.

The simplest invariant: `mark_for_preempt` only sets `need_resched`;
the *re-picking* happens entirely inside `schedule()`. The trap-time
bookkeeping (stride accounting, wake sleeping threads) still
happens in `mark_for_preempt`, but the next-thread selection moves
to `schedule()`. Cleaner separation, slight cost in re-doing the
search.

## 4. Preemption points

The model only matters if `schedule()` actually gets called regularly.
EASE is M-mode-only — there's no user/kernel boundary where Linux
would normally check `need_resched`. So we have to define the points
ourselves.

Three categories, in order of how cheaply they cover real workloads:

**Free wins (already exist as switch points).**

- `sched::yield_now()` — already calls into the scheduler. Make it
  consume `need_resched` and switch as part of its work.
- `sched::sleep_until` / `sched::sleep` / `sched::sleep_with_leeway`
  — wakeup path. After resuming, check the flag and re-schedule if
  asserted (a higher-priority thread became Ready while we were
  asleep).
- `Completion::wait` / `Completion::wait_with_deadline` —
  after-park check, same shape.

**The idle thread.**

`sched::idle_thread`'s WFI loop is the catch-all for "nothing else
to do." Reshape it so that after each WFI wake, it consults
`need_resched` and calls `schedule()` if set. This handles the
common case where the timer fires while a hart is idle:

```rust
pub fn idle_thread() -> ! {
    loop {
        crate::arch::wait_for_interrupt();
        if percpu::this_cpu().need_resched.load(Acquire) {
            schedule();
        }
    }
}
```

With these two categories, every voluntary kernel call into the
scheduler becomes a preemption point, and the idle hart can wake to
schedule.

**Explicit `cond_resched()`.**

For long-running CPU-bound kernel paths (FAT scan, virtio queue
drain, framebuffer scroll burst, etc.), add explicit calls:

```rust
pub fn cond_resched() {
    if percpu::this_cpu().need_resched.load(Acquire) {
        schedule();
    }
}
```

Sprinkle it through any loop that can run for "many" ticks without
otherwise touching the scheduler. Equivalent to Linux's
`cond_resched()` — tedious but flexible.

## 5. Wakeups and signal()

A second source of "want a switch" beyond the timer: a high-priority
thread transitioning to Ready (e.g., via `Completion::signal()`,
IPI, or any `unpark`-style operation). Today these eventually get
serviced by the next timer tick because `preempt`'s
`pick_next_if_fairer_mut` notices the newly-Ready higher-priority
thread.

Under deferred preemption, the wakeup paths should *also* set
`need_resched` directly:

- `Completion::signal` already calls `unpark` for any parked waiter.
  After the unpark, if the waiter has higher priority than the
  currently-running thread on its target hart, set `need_resched`
  on that hart.
- `unpark` more generally: if the unparked thread has higher
  priority than what's running on its target hart, set
  `need_resched` on that hart.
- IPI handlers: same shape — they're already running in trap
  context, so they fall under "mark_for_preempt" naturally.

This keeps the latency of priority-driven wakeups bounded: high-pri
becomes Ready → flag set → next preemption point (which the running
thread will hit at the next scheduler interaction, idle WFI return,
or `cond_resched`) does the switch.

## 6. Subtleties

**Locking.** Today `preempt` holds the scheduler `IrqSpinLock`
across both the decision and the `switch_to`. Splitting it means
the lock is released between `mark_for_preempt` (trap context) and
`schedule()` (kernel context). That's fine — `schedule()` re-locks
when it re-picks. But the state machine has to tolerate the
window: when `schedule()` looks again, the runnable set may have
changed.

Resolve by making `mark_for_preempt` *not* commit a specific next
thread — it only sets the flag (and does the per-tick accounting
that has to run at trap time). `schedule()` does its own pick.

**Stride accounting.** Must happen at trap time, because it's about
how much CPU the current thread has used *up to the moment of the
trap*. Keep it inside `mark_for_preempt`.

**Slice deadline / next timer.** The current `preempt` sets the
next timer deadline based on the running thread. Under deferred,
the deadline-setting also stays in `mark_for_preempt` — it's
trap-time bookkeeping. (When `schedule()` actually switches, it can
optionally re-set the deadline for the *new* running thread; if it
doesn't, the timer just fires earlier than strictly necessary and
the next tick redoes the math. Harmless.)

**No-preempt regions.** Some kernel code must not be preempted —
the most obvious example is inside an `IrqSpinLock`, where the lock
guard holds an interrupt-disable. While interrupts are disabled,
`mark_for_preempt` never runs. So critical sections inside
`IrqSpinLock` are already preempt-safe by construction. If we ever
add lighter-weight preempt-disabling (e.g., a "preempt_count" like
Linux), `schedule()` would consult it.

**The trap handler doesn't change topology.** The Rust trap handler
still gets called with a `TrapFrame` pointer. The frame still lives
on whatever stack the entry vector put it on (in the old layout,
the thread's own stack; once SRAM8 step 4 lands, the per-hart IRQ
stack). What changes is that the handler no longer calls
`switch_to`. It runs accounting, sets the flag, returns. The frame
pops and `mret` cleanly.

## 7. The mepc-rewrite trampoline (later, optional)

With just sections 1–6, **CPU-bound threads that never hit a
preemption point cannot be preempted**. A tight `loop { do_work() }`
that doesn't touch the scheduler will monopolize the hart
indefinitely. That's a real regression vs today's behavior.

The standard fix on RISC-V is the **mepc-rewrite trampoline**:

1. In the trap handler, when `mark_for_preempt` decides a switch is
   wanted, *modify the saved `mepc` in the trap frame* to point at a
   kernel function `preempt_trampoline` instead of the
   interrupted PC.
2. Stash the *original* `mepc` somewhere the trampoline can find
   it. Either:
   - In the trap frame itself (add a slot), or
   - In a per-hart "pending_mepc" PerCpu field, since only one trap
     can be deferred at a time on a given hart.
3. `mret` jumps to `preempt_trampoline`, which is running in
   normal kernel context on the *thread's own stack* (because the
   trap exit restored the thread's sp from mscratch). All preempt-
   safe ground.
4. The trampoline saves the caller-saved registers it's about to
   clobber by calling `schedule()` (`ra`, `t0`-`t6`, `a0`-`a7`)
   into a small per-hart save area, calls `schedule()`, restores
   them, then `jr` to the original mepc.

This gives you forced preemption of any thread, without breaking
the IRQ-stack invariant. It's mechanically delicate (caller-saved
register save/restore, possibly disabling preemption-of-the-
trampoline-itself), so don't attempt it in the same diff as the
core deferred-preemption refactor. Treat it as a follow-up once
the simpler "flag + preemption points" model is green.

Open question for when you get there: does the trampoline run with
interrupts enabled? If enabled, a *second* timer tick could fire
during the trampoline and stack another preemption — risky. Cleaner
to run with interrupts disabled around the `schedule()` call and
re-enable at the original-PC jump.

## 8. Impacts on existing tests

Many of the existing scheduler tests assume preemption is forced
(timer-driven) and reliably switches threads on a tick boundary.
Under deferred preemption *without the trampoline*, that's no
longer true — preemption happens at a preemption point, not at a
tick. Some tests will need to adjust:

- Tests that use `sleep_until` / yield-loops keep working — those
  are preemption points.
- Tests that spin in user-mode-style busy loops expecting forced
  preemption will hang or fail. Either add `cond_resched()` to the
  test workload, or convert to yield-based.
- Affinity tests (which thread runs on which hart) keep working —
  the decision logic is unchanged, only the timing of the act
  shifts.

Plan to do a tests pass right after stage 1 lands, before stage 2's
preemption-point sprinkling becomes load-bearing.

## 9. Order of operations

1. **Add `need_resched` to `PerCpu`.** No behaviour change yet —
   nothing sets or reads the field.
2. **Refactor `preempt` into `mark_for_preempt` + `schedule`.** The
   ISR calls `mark_for_preempt` (which sets the flag and does
   accounting); existing call sites of `preempt` are updated to
   either set the flag or call `schedule()` depending on whether
   they're in trap context or kernel context. At this point the
   timer ISR no longer calls `switch_to`.
3. **Wire up preemption points.** `yield_now`, `sleep_*`,
   `Completion::wait`, `idle_thread`. Verify the scheduler tests
   still pass (with any necessary test adjustments).
4. **Wire up wakeup-driven `need_resched`.** `unpark`,
   `Completion::signal`, IPI handlers.
5. **Commit.** This is the "deferred preemption" baseline. Step 4
   of the SRAM8 plan is now unblocked.
6. **Apply SRAM8 step 4** (mscratch swap) as originally planned.
   The IRQ-stack invariant now holds.
7. **(Optional, future)** Add the mepc-rewrite trampoline to
   recover forced preemption of CPU-bound threads.

Each stage 1–4 is independently testable and shippable. Stage 6
depends on stages 1–5 being green.

## 10. What this unblocks

- SRAM8 step 4 (mscratch IRQ-stack swap) becomes safe.
- Thread stacks can drop their TrapFrame headroom budget (SRAM8
  step 6's `_MIN_CLASS_SIZE_CHECK` tightening). Today
  `StackClass::KB1.size() >= sizeof(Context) + sizeof(TrapFrame) +
  slack`; after the swap lands, the TrapFrame term goes away.
- Future "tickless idle" / "low-power sleep" work that wants to
  drop SRAM1 (and with it, the percpu block and IRQ stack) becomes
  more tractable, because the only sched state that has to be
  reanimated on wake is `need_resched` + the TCB array — no
  IRQ-stack frame to recover.
- General sanity: "the trap handler does the minimum and never
  tail-calls into thread-switching machinery" is a more
  defensible invariant than today's "preempt synchronously from
  inside the trap, and hope each thread's stack still holds
  its earlier frame." The current design works *only* because
  trap frames live on per-thread stacks; the moment that
  changes for any reason, the implicit invariant breaks.
