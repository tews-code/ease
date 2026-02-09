# EASE OS - Design Notes for Future Phases

Reference designs and code sketches for upcoming features.
These are plans, not implementations — details may change.

---

## Syscall Interface

Applications run in RISC-V User mode and request kernel services via `ecall`.

### Mechanism

```
User Mode (Application)          Machine Mode (Kernel)
        │                               │
        │  ecall instruction            │
        ├──────────────────────────────►│
        │                               │ - Read a0-a7 for args
        │                               │ - Dispatch to handler
        │                               │ - Set a0 for return
        │  mret instruction             │
        │◄──────────────────────────────┤
```

### Syscall Numbers

| Syscall | Number | Arguments | Return |
|---------|--------|-----------|--------|
| `sys_exit` | 0 | a0: exit code | — |
| `sys_print` | 1 | a0: ptr, a1: len | a0: bytes written |
| `sys_read` | 2 | a0: fd, a1: buf, a2: len | a0: bytes read or error |
| `sys_write` | 3 | a0: fd, a1: buf, a2: len | a0: bytes written or error |
| `sys_open` | 4 | a0: path_ptr, a1: path_len, a2: flags | a0: fd or error |
| `sys_close` | 5 | a0: fd | a0: 0 or error |
| `sys_ticks_ms` | 10 | — | a0: milliseconds |
| `sys_sleep_ms` | 11 | a0: ms | — |
| `sys_getkey` | 20 | — | a0: key or 0 |
| `sys_wait_key` | 21 | — | a0: key |
| `sys_display_*` | 30-39 | (varies) | (varies) |
| `sys_audio_*` | 40-49 | (varies) | (varies) |
| `sys_malloc` | 50 | a0: size | a0: ptr or null |
| `sys_free` | 51 | a0: ptr | — |

### User-space Wrapper Example

```rust
pub fn print(s: &str) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 1,
            in("a0") s.as_ptr(),
            in("a1") s.len(),
            options(nostack)
        );
    }
}
```

### PMP Configuration

| Region | User Access | Purpose |
|--------|-------------|---------|
| Flash (kernel) | Execute only | Kernel code |
| Flash (app) | Read + Execute | Application code |
| SRAM (kernel) | None | Kernel data, stacks |
| SRAM (app) | Read + Write | Application heap/stack |
| PSRAM | Read + Write | Large data (WAD, documents) |
| Peripherals | None | Hardware (kernel only) |

---

## Synchronization Primitives

All sync primitives use SRAM (atomics don't work in PSRAM).

```rust
// Hardware spinlock wrapper (RP2350 has 32 hardware spinlocks)
pub struct HwSpinlock<const N: u32>;

// Software spinlock (for when hardware locks exhausted)
pub struct Spinlock { locked: AtomicBool }

// Mutex with data protection
pub struct Mutex<T> { lock: Spinlock, data: UnsafeCell<T> }

// Single-producer single-consumer ring buffer
pub struct SpscQueue<T, const N: usize> {
    buffer: [MaybeUninit<T>; N],
    head: AtomicUsize,
    tail: AtomicUsize,
}

// Inter-core FIFO (hardware, 8 words each direction)
pub mod fifo {
    pub fn send(data: u32);
    pub fn recv() -> u32;
    pub fn try_recv() -> Option<u32>;
}
```

---

## Preemptive Scheduler (Phase 12A)

### Thread Stack Allocation (static pool of 8 slots)

| Slot | Stack | Purpose |
|------|-------|---------|
| 0 | 2KB | Idle thread |
| 1 | 2KB | Shell |
| 2 | 16KB | Doom (C code, deep recursion) |
| 3 | 4KB | Editor |
| 4 | 2KB | Alarm |
| 5-7 | 4KB each | Reserved/future |

### TCB Structure

```rust
struct TCB {
    context: SavedContext,      // 31 regs + mepc + mstatus = 132 bytes
    stack_base: *mut u8,
    stack_size: usize,
    state: ThreadState,         // Free, Ready, Running, Blocked
    id: u8,
    wake_at: Option<u64>,       // For sleep support
}
```

### Interrupt-disabling Mutex

```rust
impl<T> SpinLock<T> {
    pub fn lock(&self) -> SpinLockGuard<T> {
        let mstatus = disable_interrupts();
        while self.locked.swap(true, Acquire) {
            core::hint::spin_loop();
        }
        SpinLockGuard { lock: self, prev_mstatus: mstatus }
    }
}
```

### Context Switch Flow

```
Timer interrupt fires (every 1ms)
        │
        ▼
  _trap_vector (asm)
  - Switch to kernel stack
  - Save all regs to TCB
  - Call rust trap_handler
        │
        ▼
  trap_handler (Rust)
  - Identify: timer interrupt
  - If tick_count >= 10: context_switch(regs)
        │
        ▼
  _trap_vector (asm)
  - Restore regs from new TCB
  - Switch to thread stack
  - mret
```

### Scheduler API

```rust
const MAX_THREADS: usize = 8;
const TIME_SLICE_MS: u32 = 10;

pub fn spawn(entry: fn(), stack_size: usize) -> Option<ThreadId>;
pub fn yield_now();
pub fn exit() -> !;
pub fn sleep_ms(ms: u32);
pub fn current() -> ThreadId;
```
