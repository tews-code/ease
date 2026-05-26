# `kernel/sched/roundrobin.rs` — cold review

A critical review of design, architecture and idiomatic-Rust flaws in
`src/kernel/sched/roundrobin.rs`, captured 2026-05-09.

## 1. Correctness / soundness issues (most serious)

### 1.1 `bootstrap` advances the global thread-id counter from a fixed slot
`bootstrap` calls `next_thread_id()` *before* taking the lock and asserts
`id == 0` for hart 0 / `id > 0` otherwise. If `spawn` ever runs (or is
invoked from another hart) between hart 0 and hart 1 bootstrapping, the
assert holds but `id` is no longer guaranteed unique relative to slot
ordering. More importantly, the check is brittle: it asserts a global
ordering invariant that is owned by the boot sequence in `main.rs`, not by
the scheduler. Better to either:
- Pass `id` in explicitly, or
- Drop the asserts and let `bootstrap(hartid)` simply allocate any free id.

Using `slot_for_hart(hartid)` to *place* the boot TCB is fine, but coupling
the index to the id ordering is a hidden contract.

### 1.2 `bootstrap` violates the "TCB array is locked-by-spinlock" invariant
`bootstrap` writes to `control_blocks[slot_for_hart(hartid)]` *without
verifying the slot is `Avail`*. If a `spawn` had happened first (e.g. from
the tests, or in some future refactor), it could land in that slot and be
silently overwritten, leaking its heap-backed stack.

### 1.3 Race in `preempt`: cycle attribution / state changes outside the trap context
`preempt` is called from `trap_handler` (machine timer interrupt). At that
point `mstatus.MIE` is already 0 (machine mode trap entry clears MIE). But:
- `preempt` takes `IrqSpinLock`, which calls `disable_interrupts()` again
  (idempotent on RISC-V — `csrrc` returns previous status), and on guard
  drop will `restore_interrupts(prev)`. `prev` here is the mstatus
  *captured inside the trap* with MIE already cleared, so
  `restore_interrupts` correctly does nothing. OK in practice, but this is
  a subtle invariant that should be asserted (e.g.
  `debug_assert!(!arch::interrupts_enabled())` at entry).
- After `switch_to`, `post_switch_cleanup()` is called. For a thread that
  was preempted (not yet first-run), control returns through the saved
  `ra` of `switch_to` — which was the instruction after `switch_to` in the
  *previous* `preempt` call. That returns up through `trap_handler` and
  `mret` re-enables MIE. Fine. But for a *new* thread, `ra =
  thread_entry`, which calls `thread_first_run` which calls
  `post_switch_cleanup()` and then `enable_interrupts()` and then
  `entry()`. So `post_switch_cleanup` runs once with interrupts disabled.
  Good.
- However, the `preempt` path returns from `switch_to` *with interrupts
  disabled* (we're still inside the trap). Then `post_switch_cleanup` runs
  (locks, fine), then we return to `trap_handler` which `mret`s and
  re-enables MIE. OK.

The subtle issue: `reschedule` (the cooperative path) calls
`disable_interrupts` itself, then locks (also disables), then after
`switch_to` calls `post_switch_cleanup`, then `restore_interrupts(prev)`.
But the comment says "mret has restored MIE via the thread trampoline" —
which is **only true for the first run**. For a regular cooperative resume
of an already-running thread, no trampoline is involved; the resumer
returns straight out of `switch_to` with interrupts disabled (because the
*original* `reschedule` saved them disabled). The `restore_interrupts(prev)`
line is the thing that re-enables them — the comment is misleading. Either
the comment is wrong or you're relying on a fragile coincidence.

### 1.4 `reschedule` and `preempt` are nearly identical — duplicated logic, divergent behaviour
The two functions duplicate ~30 lines that must stay in lockstep. They
differ in:
- `reschedule` disables interrupts, locks, then restores at the end.
  `preempt` does not.
- `reschedule` early-returns through `restore_interrupts`. `preempt`
  early-returns *without* doing anything special (because the trap is the
  implicit critical section).

The duplication will rot. A single internal function parameterised on the
post-state and on whether to wrap in `disable/restore_interrupts` would
avoid drift. It would also localise the "running_on_hart / switch_on_hart
/ state book-keeping must match" invariant to one place.

### 1.5 `Switching` state and per-hart `switch_on_hart` are racy for the *outgoing* thread
After `reschedule` releases the lock and calls `switch_to`, the *outgoing*
thread is in state `Switching(...)`. If another hart then runs
`get_current_and_next_mut`, it will not pick this thread because
`Switching` is not in the filter. Good. But:
- If another hart's timer interrupt fires *before* we reach
  `post_switch_cleanup` after the new thread eventually resumes us, nobody
  on any hart could run us — fine, that's the point.
- But `switch_on_hart` is keyed by **the hart that began the switch**. The
  `post_switch_cleanup` reads `switch_on_hart[cpu_id()]`. If somehow a
  thread resumes on a *different* hart from the one that began the switch
  (you don't currently migrate, but this implicit assumption isn't
  documented or enforced), `post_switch_cleanup` will look up the wrong
  slot and either find `None` (panic) or the wrong index (silent
  corruption).

This is the kind of thing that bites you the day someone adds work-stealing.
Add an `assert!` or, better, store the index in a per-thread location
(e.g. on the *new* thread's TCB, or in a register-pinned per-hart struct).

### 1.6 `wake_threads` runs O(THREADS_MAX) on every reschedule
Cheap today (32) but it is also called from a held spinlock with
interrupts disabled on every preempt tick on every hart, and `ticks_ms()`
itself takes an atomic read. Acceptable as a starting point; a sleep heap
or per-deadline queue is the obvious next rung.

### 1.7 `pass_baseline` returns `0` when there is no other ready thread
A newly-spawned thread can therefore start with `pass = 0`, which on a
system that has been running for a long time means it monopolises the CPU
until the existing threads catch up to it. If you then *do* have other
ready threads with large `pass` values, the new thread gets unbounded
priority. This is a stride-scheduling bug. Fix: include
`Running`/`Sleeping` in the baseline (you already include `Running`, but a
system that just had everyone go to sleep produces `min == 0`). A safer
rule is: `baseline = max(self.global_min_pass, last_observed_pass)`
maintained as a scalar.

### 1.8 `stride()` adds `priority` as the stride
Stride scheduling defines stride as `STRIDE_CONST / weight`, where higher
weight ⇒ smaller stride ⇒ more time. Here the priority value itself is
added (and your convention is "lower = higher priority"). So `priority =
0` adds 0 each iteration — **`pass` never advances**, so a priority-0
thread monopolises the CPU forever (the comment acknowledges this). That's
not really stride scheduling anymore; it's "priority 0 disables fairness".
That's a footgun:
- If two priority-0 threads exist they will starve each other in
  alternation tied to whoever the iterator finds first (deterministic but
  not fair).
- Mixing priority-0 with non-zero priorities breaks the stride invariant,
  because priority-0 threads always have the minimum pass.

A more standard formulation: `pass += BIG_CONST / (PRIORITY_MIN - priority
+ 1)` with priority-0 reserved as a real-time class handled outside the
stride bucket.

### 1.9 `pass: u64` will eventually wrap
Stride pass values must be compared circularly (`a - b` interpreted as
signed) or rebased periodically. With u64 and 32 ms ticks plus reasonable
weights this won't bite for years, but the data type encodes a contract
that isn't enforced in `min_by_key`.

### 1.10 `last_started_cycles` is only *set* by `bootstrap`/`reschedule`/`preempt`, never on `spawn`
On `spawn`, `last_started_cycles` defaults to 0 (via
`..ThreadControlBlock::new()`). When that thread is first picked,
`curr_cycles - curr.last_started_cycles` is computed for the *outgoing*
(current) thread, not the new one, so this is fine. But the *new* thread's
`last_started_cycles` is set to `curr_cycles` at the moment it's picked,
which is also fine. So actually OK — but a future `release_stack` or
migration could expose this. Worth a comment.

### 1.11 `release_stack` is dead code (`#[expect(dead_code)]`) — there is no thread exit path
Threads cannot exit cleanly. `entry()` is `fn() -> !`. If a thread `loop {
wait }`'s its way to retirement, there is no API to mark `state = Avail`
and reclaim the stack. The slot is permanently consumed. Worth noting:
the type signature lies — the scheduler claims to allocate stacks but
cannot deallocate them.

Also, `release_stack`'s `else` branch panics with the wrong message — the
panic fires when `stack_owned == false` but the message says "class not
configured". The branches of the `if`/`else` don't correspond to the
messages.

### 1.12 `ThreadStack::canary_ok` exists but is never called and `STACK_CANARY` is never re-checked
The canary is written once in `init_for_entry` and never inspected
anywhere — not on context switch, not on panic, not on syscall return. The
purported safety net does nothing. Either wire it in (cheap: check on
every reschedule), remove it, or make it a debug-only feature.

### 1.13 `Scheduler::get_current_cycles(tcb_idx)` is misnamed
It just reads `run_cycles[tcb_idx]` — there is no "current" here, the
caller passes the index. Either:
- rename to `cycles_for(tcb_idx)`, or
- make it actually look up the running thread on this hart.

The `pub fn get_current_cycles` re-export at module level is confusing for
the same reason and is dead code.

### 1.14 `Send`/`Sync` claims
```rust
unsafe impl Send for ThreadsInner {}
```
but the field `sp: *mut u8` is what you're really apologising for. `Sync`
is provided by `IrqSpinLock<T: Send>`; the `Send` impl on `ThreadsInner` is
needed because of the raw pointer. The accompanying SAFETY comment is OK,
but I'd move it to live next to the `sp` field (`// SAFETY: pointer
accessed only under threads.lock(), and only by the owning hart between
context switches`).

## 2. Architecture

### 2.1 Scheduler is a single global, with one global lock, in a multi-hart kernel
You declare `static SCHEDULER: Scheduler = Scheduler::new();` and protect
*all* state with one `IrqSpinLock`. Every preempt tick on every hart
contends on this lock. With `HARTS_MAX = 2` this is fine; it does not
scale. The forward path (per-hart runqueues, work stealing) is hinted at
by `running_on_hart` and `switch_on_hart` but the current design still
serialises everything. That's a fine starting rung — but the structure
should *prepare* for per-hart runqueues, e.g. by giving `ThreadsInner` a
`runqueues: [PerHartQueue; HARTS_MAX]` field stub.

### 2.2 Cyclic dependency: scheduler → arch::context → scheduler
`thread_first_run` (in `arch/context.rs`) calls
`crate::kernel::sched::post_switch_cleanup`. The arch layer should be
ignorant of the scheduler. Ideal layering: the scheduler hands `switch_to`
a function pointer (or a small ABI struct) to call after switch, or simply
provides its own assembly trampoline that does post-switch fixup before
jumping to `entry`.

### 2.3 Global allocator is used for thread stacks during scheduler init
`spawn` calls `alloc::alloc::alloc(...)` while interrupts are presumably
enabled (you allocate before locking). This is a soft layering issue: the
scheduler is now coupled to the global allocator. Stack pools per
`StackClass` would be cheaper, never fragment, and remove that coupling.

### 2.4 `THREADS_MAX = 32` and `HARTS_MAX = 2` placement
`slot_for_hart(hartid) = THREADS_MAX - HARTS_MAX + hartid` reserves the
*highest* indices for boot threads. That works, but it's an undocumented
invariant: `spawn` searches via `iter().find`, which finds the lowest-
index `Avail` slot, so user-spawned threads never collide with boot slots.
Fine — but if anyone changes the search order to "round-robin start from
last-used" it breaks. A first-class `BootSlots`/`UserSlots` split, or
just `boot[HARTS_MAX] | user[THREADS_MAX-HARTS_MAX]`, would make the
invariant local.

### 2.5 `State` enum embeds two flavours of sleep deadline (`Sleeping(d)` vs `Switching(PostSwitch::Sleeping(d))`)
`wake_threads` has to enumerate both. This is a smell: the state machine
has two "sleeping with deadline" states because `Switching` is the
transient between them. A cleaner model:
- `state: Phase` (`Avail`/`Ready`/`Running`/`Sleeping`)
- `wake_at: Option<u64>`
- `pending_phase: Option<Phase>` for the in-flight switch

This separates the "what is this thread" from the "what is happening to it
right now".

### 2.6 Boot/init code is interleaved with steady-state code
`bootstrap`, `slot_for_hart` and the priority-id assertion only run once
per hart. They could live in a small `boot.rs` submodule under `sched/`,
leaving the steady-state file lean.

### 2.7 No `Thread` handle type
`spawn` returns a raw `u32` id. There is no API to query state, kill,
join, or set affinity. That's acceptable for the current rung but the Id
should be a newtype:
```rust
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct ThreadId(u32);
```
to prevent misuse and to gate future API around it.

## 3. Idiomatic Rust

### 3.1 `*mut u8` for `sp`
`sp` is a raw pointer that is always non-null after `spawn`/`bootstrap`.
Use `NonNull<u8>` or, better, a wrapper newtype `StackPointer(NonNull<u8>)`
with `unsafe fn` constructors. Same for the `ThreadStack::allocate`
result — already `NonNull<u8>`, but `init_for_entry` immediately drops
back to `*mut u8`.

### 3.2 `Option<StackClass>` as "maybe initialised"
`stack_class: Option<StackClass>` is `None` for the bootstrap slots in
early init. But `bootstrap` immediately sets it to `Some(KB4)` (lying —
the boot stack is the linker-script stack, not a `KB4` heap stack). The
type encodes a state machine more cleanly handled by:
```rust
enum StackOrigin { Boot { /* size known at link time */ }, Heap(StackClass) }
```
That also lets `release_stack` panic statically on `Boot`.

### 3.3 `stack_owned: bool` is a discriminant for `stack_class`
With `StackOrigin` as above, `stack_owned` disappears.

### 3.4 `expect(dead_code)` and `allow(dead_code)` everywhere
Eight or so `#[allow(dead_code)]` / `#[expect(dead_code)]` on top-level
items signal half-built APIs. Either delete the helpers or make them
`pub(crate)` and call them. A future reader can't tell whether
`release_stack` is broken or just unwired.

### 3.5 `unsafe { core::ptr::write_bytes(context_ptr, 0, 1); }` immediately followed by `*context_ptr = Context::for_entry(entry);`
The zero-fill is wasted: the next assignment fully initialises the struct.
Drop the `write_bytes`.

Also `*context_ptr = Context::for_entry(entry)` writes through a raw
pointer that has only just been formed and is not guaranteed to be aligned
for `Context` unless the stack base is — and stack base is
`class.size()`-aligned (≥ 1 KB), and `class.size() - sizeof(Context)` is a
multiple of `align_of::<Context>()` *only* because `Context` is `align(16)`
and 16 divides both. The static_assert checks `size_of::<Context>() %
align_of::<Context>() == 0` (which is always true for `repr(C,
align(N))`); it doesn't check what you actually need: that `class.size()
% align_of::<Context>() == 0`. Add:
```rust
const _: () = assert!(StackClass::KB1.size() % core::mem::align_of::<Context>() == 0);
```

### 3.6 The corrupted comment line in `init_for_entry`
```rust
// write§§§§§§§§§able memory of at least class.size() bytes
```
This is a real bug — looks like an editor accident. Worth fixing.

### 3.7 `ptr::write` on stack_base for the canary
```rust
core::ptr::write(stack_base.as_ptr() as *mut usize, STACK_CANARY);
```
Use `NonNull::cast::<usize>().as_ptr().write(...)` or
`stack_base.as_ptr().cast::<usize>().write(STACK_CANARY)`. Preserves
provenance and avoids the as-cast lint.

### 3.8 `min_by_key` on a 32-element array on every reschedule
Acceptable, but you're paying for an O(N) min on every preempt tick across
all harts. A sorted runqueue (e.g. binary heap keyed on `pass`) is the
natural next step. A simpler micro-optimisation: keep `Running` excluded
from the search (you already filter on `State::Ready` only — good).

### 3.9 `get_disjoint_mut([curr_idx, next_idx])`
This is unstable API status: confirm your toolchain pin requires nightly
`get_many_mut` / `get_disjoint_mut`. If pinned, fine — but flag it in a
comment.

### 3.10 `&raw mut curr.sp` / `&raw mut next.sp`
The use of `&raw mut` is correct and modern. But you compute these
*before* dropping the lock, then pass them to `switch_to` *after*
dropping the lock. Between the two, the TCB array is unlocked and another
hart could (in principle) acquire the lock and observe these slots. That
hart will *not* mutate them because `state == Switching`/`Running`
excludes them from `get_current_and_next_mut`, but the soundness
reasoning for holding raw pointers across the lock boundary deserves a
SAFETY comment.

### 3.11 Public API naming is inconsistent
- `yield_now()` (verb)
- `sleep(deadline_ms: u64)` — actually takes a *duration*, not a deadline.
  Rename to `sleep_ms` or change the parameter name. The doc says "Blocks
  for `deadline` milliseconds" which is itself a contradiction.
- `sleep_until(deadline_ms: u64)` — fine.
- `bootstrap(hartid)` — fine but lower-level than the other names;
  consider `init_hart(hartid)`.
- `idle_thread() -> !` — inconsistent with the other `pub fn` items
  because it's a function that can be passed to `spawn`. Should perhaps be
  in a separate `entry` namespace.

The doc `/// Blocks for `deadline` millseconds` has a typo (`millseconds`).

### 3.12 `pub fn get_current_cycles(tcb_idx: usize) -> u64` exposes an internal index
Callers can't correlate a `tcb_idx` to a `ThreadId` they got from `spawn`.
The function is useless from outside this module unless you also expose
the index. Better: `pub fn cycles_for(id: ThreadId) -> Option<u64>` and do
an internal lookup.

### 3.13 `cfg(all(test, feature = "test-sched"))` tests live below the public API
Conventional: tests at the bottom in a `#[cfg(test)] mod tests`, OK. But
the `partner_thread`/`PARTNER_SPAWNED` shared-state pattern means tests
are stateful and order-dependent. Specifically,
`sleep_blocks_for_duration` runs *after* `yield_makes_progress` has
already made the partner busy; if they ever run in parallel they'll
interfere. Document the "tests run sequentially in this runner"
assumption.

### 3.14 `THREADS_MAX` is hard-coded to 32
This is a configuration knob that belongs in `board.rs` next to
`HARTS_MAX`, not in the scheduler.

### 3.15 `PRIORITY_MIN: u8 = u8::MAX - 1`
Why not `u8::MAX`? Off-by-one is fine but unexplained. Add a comment, or
use a `Priority(u8)` newtype with associated constants and arithmetic that
won't overflow.

### 3.16 The big commented-out `stack_ok_panic` block
Either restore it (with a TODO) or delete it. Living dead code is noise.

### 3.17 Boot-thread struct initialisation
`Scheduler::bootstrap` writes `last_started_cycles:
crate::arch::csr::rdcycles()` but the new `..ThreadControlBlock::new()`
resets `pass: 0`, `priority: PRIORITY_DEFAULT` (then re-overrides). The
struct-literal-then-`..default()` pattern reads cleanly but you've had to
set `priority: PRIORITY_MIN` and `state: Running` which are easy to
forget. Consider a constructor `ThreadControlBlock::for_boot(...)` so the
invariants live in one place.

## 4. Documentation

- The module-level doc is one line: `//! Preemptive multitasking with
  round robin scheduling`. There's no description of the state machine,
  the role of `Switching`, the per-hart book-keeping, or who must call
  `post_switch_cleanup`. The implicit contract that
  `post_switch_cleanup` *must* run exactly once after each `switch_to`,
  on the same hart, is critical and undocumented.
- `Safety:` comments are inconsistent in style. Some say `Safety:`, some
  `# Safety`, some omit. Stick with `# Safety`.
- The cycle/cpu/wall accounting is documented in the test block but not
  in the production code — reverse it.

## 5. Suggested triage

1. Fix the priority-0 stride bug (or document it and refuse to spawn at
   priority 0).
2. Deduplicate `reschedule` / `preempt` into one helper.
3. Either wire the stack canary into reschedule or drop it.
4. Make `next_thread_id` independent of bootstrap ordering; remove the
   brittle `assert!(id == 0)`.
5. Replace `*mut u8` sp with `NonNull<u8>` (or a `StackPointer` newtype).
6. Replace `pub fn sleep(deadline_ms)` with `sleep_ms(duration_ms)`; fix
   the doc typo.
7. Add a comment + `debug_assert!` documenting that
   `post_switch_cleanup` must run on the same hart that issued
   `switch_to`.
8. Add a thread-exit path so `release_stack` becomes reachable, or remove
   it.
9. Pull `THREADS_MAX` into `board.rs`.
10. Plan per-hart runqueues — `running_on_hart`/`switch_on_hart` are the
    seed.

The code reads as a thoughtful, mid-rung scheduler with clearly demarcated
unsafe surfaces and good test coverage. The biggest gap is that the
*state machine* has grown beyond what an `enum State` cleanly expresses,
and the `reschedule` / `preempt` pair has begun to drift — both signs
that the next refactor should consolidate the switch path before adding
features (priorities-by-class, per-hart queues, thread exit).
