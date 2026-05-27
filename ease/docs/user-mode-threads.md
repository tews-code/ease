# Preemptible User-Mode Threads

Design for running application threads in RISC-V User mode (U-mode) as
first-class, preemptible scheduler threads — the successor to the one-shot
`user_entry` excursion.

## Where we're coming from

Today U-mode is a *synchronous excursion*: a kernel thread calls
`user_entry(fn)`, which saves the kernel context, drops to U-mode with
**interrupts off**, runs the user function, and returns via the `EXIT`
syscall or a fault (`resume_kernel`). It is not a scheduler thread, and it
cannot be preempted — interrupts are deliberately masked so a timer can
never fire mid-excursion.

A *real* user thread is the opposite: it runs in U-mode with **interrupts
on**, can be preempted by the timer mid-computation, is scheduled like any
other thread, and resumes back into U-mode later.

## Key decisions (and why)

**1. Two stacks per user thread.** A kernel stack (PD1 heap, exactly like
today's thread stacks) and a user stack (PD0, the PMP-granted NAPOT
region). The TCB's `sp`/`stack_base` keep meaning *kernel stack*, identical
to a kernel thread — so `switch_to`, `Context`, and the scheduler need **no
changes**. The user sp is not a TCB field; it lives in the saved trap frame
while the thread is in the kernel.

**2. `mscratch` carries the current thread's trap stack.** Set on context
switch: the per-hart IRQ stack for kernel threads (as today), the thread's
kernel-stack top for user threads. The trap vector's existing
`csrrw sp, mscratch, sp` then lands on the correct stack for *both* thread
types with **no privilege branch** — the hot trap path stays byte-for-byte
unchanged. The cost is one conditional store in the (non-hot) context-switch
path.

**3. Frame-based reschedule for user threads; the trampoline stays for
kernel threads.** The deferred-preemption trampoline is an *optimization for
kernel threads*: it reuses the C ABI (caller-saved on the stack,
callee-saved in `Context`) to avoid a full register save. A preempted
U-mode thread has arbitrary registers, so it must save *all* of them
anyway — and the trap entry already builds that complete frame on the
kernel stack. So user threads skip the trampoline entirely: the handler
reschedules directly (it is already running on the kernel stack) and resumes
by returning *through the saved frame* (the xv6 `usertrap`/`usertrapret` +
`swtch` split). The handler chooses the path by branching on
`TrapFrame::is_from_user()`.

This is what lets decision #2 work. The `mscratch` trick is clean on trap
*entry* but the vector's *exit* swap restores `sp` to the interrupted (user)
sp — correct for a normal return to U-mode, but wrong if we tried to bounce
a U-trap into the kernel-thread trampoline (which needs `sp` to stay on the
kernel stack). By not routing user threads through the trampoline at all,
that conflict never arises.

**Consequence:** the earlier trampoline `MPP=M` fix is redundant under this
design — the trampoline only ever runs for kernel threads, which are already
`MPP=M`. It is a harmless no-op; keep it as defensive or drop it.

## Mechanisms

| Path | Behaviour |
|------|-----------|
| Trap entry | Unchanged vector. `csrrw sp, mscratch, sp` → kernel stack (U-thread) or IRQ stack (kernel thread). |
| Context switch | Set `mscratch = is_user ? kernel_stack_top : irq_stack_top`. |
| First run (user thread) | A `Context::for_entry` trampoline that, in M-mode on the kernel stack, sets `mepc` = user entry, `MPP=U`, `MPIE=1`, user `sp`, `mscratch` = kernel-stack top, then `mret`s. Replaces `thread_first_run`'s closure path for user threads. |
| Reschedule — kernel thread | Existing deferred trampoline, untouched. |
| Reschedule — user thread | Frame-based: the full frame (incl. `user_sp`) is already on the kernel stack; save `user_sp` into it, then `switch_to` directly. On resume, restore `mscratch = frame.user_sp` and return through the normal vector exit. |
| Syscall returning to U (no reschedule) | Normal vector return — `mscratch` still holds `user_sp` from entry. |
| Exit / fault | Terminate the thread (`SCHEDULER.exit()`); there is no synchronous caller to return to. |

## Data-model changes

- **TCB** (`kernel/sched/types.rs`): add `is_user`, the user stack base, and
  the user entry point. `sp`/`stack_base` continue to mean the kernel stack.
- **`TrapFrame`** (`arch/trap.rs`): add a `user_sp` slot, so a descheduled
  U-thread's stack pointer persists in the saved frame.
- **Stacks**: kernel stack from the PD1 kernel heap (as existing thread
  stacks); user stack from PD0 via the NAPOT allocator (`kalloc_pd0_napot`).

## Build order

Each step is independently checkable.

1. **`mscratch` per-thread on context switch.** For kernel threads it equals
   the IRQ stack as before, so this is a behaviour-preserving refactor: land
   it and confirm the system still boots and preempts kernel threads
   normally. Foundation for everything else.
2. **`spawn_user` + first-run bootstrap.** Allocate both stacks, mark
   `is_user`, bootstrap into U-mode. Test with a U-thread that immediately
   syscalls/exits (interrupts may stay off) — proves creation and the
   two-stack layout without preemption.
3. **`user_sp` in the trap frame + the handler reschedule branch.** Wire the
   frame-based path and the `mscratch` restore on resume.
4. **Enable timer preemption of U-threads.** Payoff test: a U-thread running
   a long busy loop is preempted and interleaves with other threads.
5. **Exit / fault terminate the thread** cleanly; the scheduler moves on.

## Open questions / follow-ups

- User-stack canary: `check_curr_canary` only checks the kernel stack base;
  a user thread's user stack should also get a canary.
- Blocking primitives (`park`, sleep) from U-mode require a syscall
  interface rather than direct scheduler calls.
- User-side rodata routing remains an artifact of the single-binary layout;
  the eventual user-space-as-separate-package split resolves it.
