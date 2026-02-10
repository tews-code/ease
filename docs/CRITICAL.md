# EASE Critical Architecture Improvements

Identified during full codebase review, February 2025. Ordered by severity.

## 1. SpinLock Must Disable Interrupts [HIGH]

**Problem**: `SpinLock` does not disable interrupts on acquire. On a single-core system this creates a guaranteed deadlock: if a timer interrupt fires while the CONSOLE lock is held, any interrupt handler that calls `println!()` will spin forever waiting for the lock that the interrupted code holds.

**Related**: `Console::put_char(BELL)` calls `sleep_ms(25)` twice while holding the CONSOLE lock, blocking the system for 50ms inside a spinlock.

**Files**: `src/kernel/sync.rs`, `src/drivers/console.rs`

**Fix**: `SpinLock::lock()` should save and clear `mstatus.MIE` (disable interrupts). The guard's `Drop` should restore the saved interrupt state. This is the standard pattern for single-core kernel spinlocks.

**When**: Before Phase 7. Any new interrupt source (UART RX, virtio completion) that touches locked state will hit this.

---

## 2. Virtio State Split Across Three Independent Locks [MEDIUM]

**Problem**: `BLK_REQUEST_VQ`, `BLK_REQ`, and `BLK_CAPACITY` are three separate `SpinLock<Option<...>>` globals. Every read/write acquires multiple locks in sequence, creating inconsistency windows, implicit lock ordering requirements, and redundant `Option` checks.

**Files**: `src/drivers/virtio/mod.rs`

**Fix**: Combine into a single struct behind one lock:
```rust
struct VirtioBlkState {
    capacity: u64,
    request: VirtioBlkReq,
    virtq: VirtioVirtq,
}
static BLK: SpinLock<Option<VirtioBlkState>> = ...;
```

**When**: Before Phase 7 (many sequential block reads).

---

## 3. Memory Layout Has No Collision Detection [MEDIUM]

**Problem**: Stack (`0x80100000` hardcoded in `boot.rs`), heap (placed by linker after `.bss`), and framebuffer (`0x80200000` hardcoded in `ramfb.rs`) are not validated against each other. If `.bss` + heap grows past the stack base, or the stack grows into heap, corruption occurs silently.

**Files**: `memory-qemu.x`, `src/arch/boot.rs`, `src/drivers/ramfb.rs`

**Fix**: Add linker script assertions:
```
ASSERT(__heap_end <= 0x80100000, "heap overlaps stack")
```
Consider defining stack base and framebuffer address as linker symbols rather than hardcoded constants in Rust source, so there is a single source of truth.

**When**: Before the kernel grows significantly.

---

## 4. FAT16 Not Connected to BlockDevice Trait [MEDIUM]

**Problem**: The BPB parser calls `crate::drivers::virtio::read_disk()` directly in tests. The filesystem layer has no reference to a `BlockDevice`. The `BlockDevice` trait is implemented on `VirtioBlk` but never used polymorphically — `VirtioBlk` is a unit struct and callers use free functions.

**Files**: `src/fs/fat16.rs`, `src/hal/mod.rs`, `src/drivers/virtio/mod.rs`

**Fix**: Design decision needed for Phase 7: the FS layer should accept a `&dyn BlockDevice` (or generic `B: BlockDevice`) so it can be tested against mock block devices and isn't hardwired to virtio.

**When**: Phase 7 design phase.

---

## 5. Duplicated Init Sequence [LOW]

**Problem**: The `#[cfg(test)]` and `#[cfg(not(test))]` versions of `main()` duplicate the entire init sequence (alloc, timer, virtio, framebuffer, console). Changes must be made in both places.

**Files**: `src/main.rs`

**Fix**: Extract a shared `kernel_init()` function called by both paths.

**When**: Next time init changes.

---

## 6. `print!` Silently Drops Console Output [LOW]

**Problem**: The `print!` macro uses `try_lock()` for the console. If the lock is held, framebuffer output is silently discarded. Additionally, arguments are formatted twice (once for UART, once for Console), doubling formatting cost.

**Files**: `src/io.rs`

**Fix**: Will be largely resolved by the interrupt-disabling SpinLock (item 1), which makes `lock()` safe to use from `print!`. The double-format issue could be addressed by writing to a small intermediate buffer, but is low priority.

**When**: After item 1.

---

## 7. Shell Busy-Polls With No wfi [LOW]

**Problem**: The shell inner loop tight-polls `keyboard.poll()` with no backoff, burning 100% CPU.

**Files**: `src/shell/mod.rs`

**Fix**: Add `core::arch::asm!("wfi")` between poll attempts. The timer interrupt already fires every 1ms, giving 1ms polling granularity with near-zero idle power.

**When**: Before real hardware bring-up.

---

## 8. Escape Sequence Timeout Is Not Time-Based [LOW]

**Problem**: Keyboard escape sequence timeout is `for _ in 0..100 { spin_loop() }` — a CPU-speed-dependent iteration count, not a real time measurement.

**Files**: `src/input/keyboard.rs`

**Fix**: Use `arch::timer::ticks_ms()` to implement a real timeout (e.g., 10ms).

**When**: Before real hardware bring-up.

---

## 9. FrameBuffer Ownership Not Enforced by Types [LOW]

**Problem**: `FrameBuffer` is a zero-sized type. The `attach_fb`/`release_fb` pattern moves a ZST to enforce single ownership, but anyone can create another `FrameBuffer` and write to the same memory. The type system doesn't enforce the invariant.

**Files**: `src/drivers/ramfb.rs`, `src/drivers/console.rs`

**Fix**: Make the `FrameBuffer` constructor private or use a marker to ensure only one instance exists. Even just making `init()` set a flag and panic on double-init would catch mistakes.

**When**: If multiple framebuffer consumers are added.

---

## 10. Shell Stack Usage (~8.3KB) [LOW]

**Problem**: `LineEditor` holds `line` + `saved_line` + 30-entry history, totalling ~8.3KB on the stack. Stack has no defined size limit and no guard page.

**Files**: `src/shell/line_editor.rs`

**Fix**: Consider moving history to the heap (it's a good use case for the allocator). Alternatively, document the stack budget and add a linker assertion for minimum stack space.

**When**: When adding more subsystems that also use significant stack.
