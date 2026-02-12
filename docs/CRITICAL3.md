# Architectural & Design Review: EASE (CRITICAL3)

## 1. Off-By-One Bug in `set_pixels` (Actual Bug)

`src/drivers/ramfb.rs:121` — The bounds check uses `<` where it should use `<=`:

```rust
if x + colours.len() < self.width() && y < self.height() {
```

With width=640, drawing 8 pixels starting at x=632 gives `632 + 8 = 640`, which fails `640 < 640`. The rightmost 8-pixel column of every character position at the screen edge is silently dropped. The fix is `<=`. Note that `set_pixel` correctly uses `<` because it's a single pixel, but `set_pixels` needs `<=` because `x + len == width` is the last valid position.

## 2. Bell Handler Blocks With Interrupts Disabled (~50ms)

`src/drivers/console.rs:62-71` — The BELL handler calls `busy_wait_ms(25)` twice inside `put_char`, which is called while the CONSOLE `SpinLock` is held. SpinLock disables interrupts on acquisition. This means:

- Interrupts are disabled for ~50ms
- Timer ticks are lost (50 ticks at 1ms granularity)
- The entire system is unresponsive during a bell character

Any output path that could emit `0x07` (BELL) will freeze the system for 50ms with interrupts masked. A visual bell should be deferred or handled outside the lock.

## 3. VirtIO Lock-Drop-Reacquire Race Window

`src/drivers/virtio/mod.rs:90-116` — `read_block` drops the BLK lock while polling for device completion, then reacquires to read the result:

```rust
let virtq_token = virtio_queue_submit(addr, blk.vq.as_mut(), VIRTQ_DESC_F_WRITE);
drop(blk_guard);           // unlock
while !virtq_token.is_complete() { ... }
let mut blk_guard = BLK.lock();   // relock
buf.copy_from_slice(&blk.req.data);  // read result
```

Between the drop and reacquire, another caller could lock BLK and submit a new request, overwriting `blk.req` (the sector, data, and status fields) before the first caller reads the result. This is safe today because you're single-threaded, but the API *looks* thread-safe (it uses `SpinLock`), so this is a trap for Phase 7+ when you add concurrent I/O. The single shared `req` field is the root issue — each in-flight operation needs its own request buffer.

## 4. Bump Allocator: Inevitable OOM With No Recovery Path

`src/kernel/alloc.rs` — The bump allocator never frees. Every `Vec`, `String`, `Box` is permanent. With only 64KB of heap and no deallocation:

- The benchmark tests allocate Vecs and Strings that are never freed
- Each test run consumes heap that's never reclaimed
- The shell can't do anything non-trivial (file I/O, path manipulation) without exhausting the heap

Rust's `alloc` infrastructure (Vec growth, String formatting) calls `dealloc` on old buffers during reallocation — those calls silently leak. You'll hit OOM as soon as any Vec grows past its initial capacity a few times. This is the most critical architectural constraint for Phase 7 (filesystem work will need buffer management).

## 5. `print!` Macro Double-Formats and Dual-Locks

`src/io.rs:13-21` — The `print!` macro evaluates `write!` twice: once for UartWriter, once for CONSOLE:

```rust
let _ = write!($crate::io::UartWriter, $($arg)*);
let mut c = $crate::drivers::console::CONSOLE.lock();
let _ = write!(c, $($arg)*);
```

This means `format_args!` is expanded and evaluated twice. For `print!("{}", expensive_fn())`, the function runs twice. More subtly, if the arguments involve mutable state, the two outputs can differ. A single format into a buffer written to both outputs would be cleaner, but that requires allocation (which circles back to the bump allocator problem).

## 6. Stack Guard Is Only 4 Bytes

`memory-qemu.x:35-36` — The guard between heap and stack is a single 4-byte canary. If a function allocates a large stack frame (say 128 bytes) and the stack pointer jumps past the canary in one go, the corruption is undetected until the next timer tick checks. A 4KB guard *page* (with hardware fault support, or at minimum a larger guard region) would be significantly more robust. With a large stack frame (recursive function, big local array), this guard is trivially skippable.

## 7. Hardcoded Address Duplication

Addresses are defined in multiple places that must be kept in sync manually:

| Address | Defined in | Also in |
|---------|-----------|---------|
| `0x80200000` (FB) | `memory-qemu.x:8` | `ramfb.rs:19` |
| `0x10000000` (UART) | `uart.rs:8` | nowhere else |
| `0x10001000` (VirtIO) | `queue.rs:11` | nowhere else |
| `0x2000000` (CLINT) | `timer.rs:7` | nowhere else |

The `debug_assert` in `ramfb.rs:23` catches the FB mismatch but only in debug builds. The other addresses have no cross-check at all. A single `memory_map` module sourcing from linker symbols or a shared constants file would prevent drift.

## 8. `BlockDevice` Trait Mutability Asymmetry

`src/hal/mod.rs:45-47`:

```rust
fn read_block(&self, block: u32, ...) -> Result<...>;
fn write_block(&mut self, block: u32, ...) -> Result<...>;
```

`VirtioBlkDev` is a zero-sized type — `&self` vs `&mut self` has no semantic meaning since all state lives in the global `BLK` SpinLock. The asymmetry signals to API consumers that reads are safe to share but writes need exclusivity, which is misleading when all operations go through the same global lock. Either both should take `&self` (since the lock handles exclusivity) or the trait should own the state.

## 9. VirtqToken Contains Dangling-Pointer-Waiting-to-Happen

`src/drivers/virtio/queue.rs:35-47` — `VirtqToken` holds a raw `*const u16` into the `Box<VirtioVirtq>` with no lifetime binding:

```rust
pub(super) struct VirtqToken {
    used_index: *const u16,
    last_used_index: u16,
}
```

This pointer is valid today because the bump allocator never frees. But the token outlives the lock guard and is polled outside the lock. If you ever switch allocators or restructure the VirtIO init, this becomes a dangling pointer with no compiler warning.

## 10. Linker Script Missing RISC-V Sections

`memory-qemu.x` doesn't capture `.sbss`, `.sdata`, `.srodata`, or `COMMON` sections. RISC-V compilers can emit these for small-data optimization (GP-relative addressing). If the compiler puts globals into `.sbss`, they won't be zeroed by the boot code's BSS-clearing loop, and they won't be placed in the expected memory region. This is a latent bug that may surface with different optimization levels or compiler versions.

## 11. No OOM Handler Strategy

When the bump allocator returns null, Rust's `#[alloc_error_handler]` (or the default) panics. The panic handler calls `printdln!` which writes to UART — fine. But the production panic handler then enters `loop { wfi; }` with no diagnostic on the console. If you're only watching the console (not serial), OOM looks like a random freeze. There's no way for the shell to gracefully handle "out of memory" since all Rust allocations are infallible by default.

## 12. Console Bell During `fmt::Write` Breaks Atomicity

When `write_str` processes a string containing BELL, the 50ms busy-wait happens *mid-string*. If the string is `"Error\x07: file not found"`, the first half renders, then 50ms pause, then the second half. During multi-line output from `println!`, this interleaves delays into what should be atomic display updates. The character buffer and framebuffer are in an intermediate state during the wait.

## 13. Trap Handler Prints to Console on Unknown Interrupts

`src/arch/trap.rs:106`:

```rust
INTERRUPT_TIMER => crate::arch::timer::handle_interrupt(),
_ => crate::println!("Unknown interrupt {}", code),
```

`println!` acquires the CONSOLE lock. If an unknown interrupt fires while the CONSOLE lock is held (from a `print!` in normal code), this deadlocks. Timer interrupts are safe because the SpinLock disables interrupts, but if you add new interrupt sources later (UART RX interrupt, for example), this becomes a deadlock vector. The trap handler should use `printdln!` (UART-only) for safety, as you already do in the panic handler.

## 14. Test Mode Creates Shell But Never Runs It

`src/main.rs:83`:

```rust
let _shell = shell::Shell::new();  // Created but never used
test_main();
```

The shell is constructed (which creates a `KeyboardInput<UartReader>` and `LineEditor`) but `.run()` is never called. This seems like a leftover — it initializes keyboard state that's never used and could mask initialization bugs.

---

## Summary by Severity

**Bugs**: #1 (set_pixels off-by-one), #10 (missing linker sections)

**Will bite you in Phase 7+**: #3 (VirtIO race), #4 (bump allocator OOM), #9 (VirtqToken lifetime)

**Design debt**: #2 (bell blocking), #5 (double format), #6 (4-byte guard), #7 (address duplication), #8 (trait asymmetry), #13 (trap handler deadlock risk)

**Minor**: #11 (OOM UX), #12 (bell mid-string), #14 (unused shell in tests)
