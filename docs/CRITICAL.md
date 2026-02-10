# EASE Critical Architecture Improvements

Identified during full codebase review, February 2025. Ordered by severity.

## 1. SpinLock Must Disable Interrupts [HIGH]

**Problem**: `SpinLock` does not disable interrupts on acquire. On a single-core system this creates a guaranteed deadlock: if a timer interrupt fires while the CONSOLE lock is held, any interrupt handler that calls `println!()` will spin forever waiting for the lock that the interrupted code holds.

**Related**: `Console::put_char(BELL)` calls `sleep_ms(25)` twice while holding the CONSOLE lock, blocking the system for 50ms inside a spinlock.

**Files**: `src/kernel/sync.rs`, `src/drivers/console.rs`

**Fix**: `SpinLock::lock()` should save and clear `mstatus.MIE` (disable interrupts). The guard's `Drop` should restore the saved interrupt state. This is the standard pattern for single-core kernel spinlocks.

**When**: Before Phase 7. Any new interrupt source (UART RX, virtio completion) that touches locked state will hit this.

---

## ~~2. Virtio State Split Across Three Independent Locks [MEDIUM]~~ DONE

Resolved in `ba94748`. Three globals consolidated into `BLK: SpinLock<Option<VirtioBlkState>>`. Also merged `read_disk`/`write_disk` into unified `disk_op` helper.

---

## ~~3. Memory Layout Has No Collision Detection [MEDIUM]~~ DONE

Resolved in `860faed`. Linker symbols (`__stack_top`, `__fb_addr`, `__fb_size`) defined in `memory-qemu.x` with three `ASSERT` statements. `boot.rs` loads SP from linker symbol. `ramfb.rs` validates `FB_ADDR` matches linker symbol via `debug_assert`. Runtime stack overflow detection deferred to Phase 12A (PMP guard pages).

---

## ~~4. FAT16 Not Connected to BlockDevice Trait [MEDIUM]~~ PARTIALLY DONE

Tests migrated to use `BlockDevice` trait in `ba94748`. Decision made: `Fat16` will be generic over `B: BlockDevice` (zero-cost, enables mock block devices for host tests). Will be fully resolved when `Fat16` struct is implemented in Phase 7 Step 5.

---

## ~~5. Duplicated Init Sequence [LOW]~~ DONE

Resolved in `21146f3`. Extracted `kernel_init()` called by both test and non-test `main()`.

---

## ~~6. `print!` Silently Drops Console Output [LOW]~~ DONE

Resolved in `896f13b`. `print!` macro now uses `lock()` instead of `try_lock()`, safe because SpinLock disables interrupts (item 1). Double-format issue remains but is low priority.

---

## ~~7. Shell Busy-Polls With No wfi [LOW]~~ DONE

Resolved in `d3c9e50`. Added `wfi` in the `else` branch of the shell poll loop. CPU sleeps until the next interrupt (1ms timer tick) instead of busy-polling.

---

## ~~8. Escape Sequence Timeout Is Not Time-Based [LOW]~~ DONE

Resolved in `39b912d`. Replaced iteration-based loop with `ticks_ms()` deadline of 10ms.

---

## ~~9. FrameBuffer Ownership Not Enforced by Types [LOW]~~ DONE

Resolved in `9650684`. Made `FrameBuffer` a tuple struct with private field (`FrameBuffer(())`), preventing external construction. Only `init()` can create an instance.

---

## 10. Shell Stack Usage (~8.3KB) [LOW]

**Problem**: `LineEditor` holds `line` + `saved_line` + 30-entry history, totalling ~8.3KB on the stack. Stack has no defined size limit and no guard page.

**Files**: `src/shell/line_editor.rs`

**Fix**: Consider moving history to the heap (it's a good use case for the allocator). Alternatively, document the stack budget and add a linker assertion for minimum stack space.

**When**: When adding more subsystems that also use significant stack.
