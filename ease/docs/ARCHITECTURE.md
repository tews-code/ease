# EASE OS - Architecture & Implementation Notes

This file documents the architecture of subsystems that have been implemented.
See `EASE_PROJECT_PLAN.md` for the active plan.

---

## Dual-Testing Architecture

The crate uses a split architecture to support testing on both QEMU (hardware-dependent code) and the host machine (pure logic):

```
src/main.rs - QEMU tests
  - #![feature(custom_test_frameworks)]
  - #[test_case] functions run on QEMU virt machine
  - Tests UART, memory-mapped I/O, interrupts, boot
  - Uses sifive_test device for clean QEMU exit
  - Run: cargo test --bin ease

src/lib.rs - Host tests
  - Standard #[test] functions run on host machine
  - Automatically uses std (no no_std constraint)
  - Tests parsers, data structures, algorithms
  - Run: cargo test --lib
```

**Note:** `tests/` directory integration tests are blocked because `cargo test --tests` also compiles `main.rs` which contains RISC-V assembly. Requires target-conditional compilation.

## I/O Abstraction (`src/io.rs`)

The `Writer` trait abstracts byte-level output:

```rust
pub trait Writer {
    fn write_byte(&self, byte: u8);
    fn write_str(&self, s: &str);
}

// UartWriter writes to UART and (in test mode) captures to buffer
impl Writer for UartWriter {
    fn write_byte(&self, byte: u8) {
        unsafe { write_volatile(UART_ADDRESS as *mut u8, byte); }
        #[cfg(test)]
        test_io::capture(byte);
    }
}
```

The capture buffer uses `UnsafeCell<[u8; 4096]>` with `AtomicUsize` length for lock-free single-threaded capture. Tests verify output via `test_io::contains()` and `test_io::equals()`.

## Benchmarking (`src/bench.rs`)

Uses RISC-V cycle counter CSRs (`rdcycle`/`rdcycleh`) for precise measurements:

- `measure(f)` - Returns cycle count for closure
- `run(name, f)` - Prints single measurement
- `run_avg(name, iters, f)` - Prints averaged measurement
- `check(name, baseline, iters, f)` - Fails if >20% regression

Baselines stored in `src/main.rs`:
```rust
mod baselines {
    pub const PRINTLN_HELLO: u64 = 32_000;
}
```

## Panic Testing Limitation

QEMU tests cannot use `#[should_panic]` — this attribute is part of the built-in test harness and not available with `custom_test_frameworks`. Workarounds:
- **Preferred:** Test panic conditions in host tests (`cargo test --lib`) using `#[should_panic]`
- **Alternative:** Test that invalid inputs return `Result::Err` or `Option::None`

## Kernel Module Structure

```
src/
├── main.rs                # Entry point, panic handler, QEMU tests
├── lib.rs                 # Library crate for host-testable pure logic
├── io.rs                  # Re-exports UartWriter, test capture buffer
├── qemu.rs                # QEMU exit mechanism (sifive_test device)
├── bench.rs               # Benchmarking via RISC-V cycle counter
│
├── arch/                  # Architecture-specific
│   ├── mod.rs
│   ├── boot.rs            # Startup assembly, stack init
│   ├── trap.rs            # Trap vector and handler
│   ├── timer.rs           # CLINT timer, ticks, sleep
│   └── multicore.rs       # Core 1 launch, parking (future)
│
├── hal/                   # Hardware Abstraction Layer
│   ├── mod.rs             # HAL traits (Writer)
│   ├── qemu_virt.rs       # QEMU virt implementation (UartWriter)
│   └── rp2350/            # (future)
│
├── kernel/                # Core kernel services
│   ├── mod.rs
│   ├── alloc.rs           # Bump allocator, GlobalAlloc
│   ├── sync.rs            # Spinlocks, mutexes, channels
│   └── sched.rs           # Preemptive scheduler (future)
│
├── drivers/               # Device drivers
│   ├── mod.rs
│   ├── ramfb.rs           # QEMU ramfb framebuffer
│   ├── keyboard.rs        # Keyboard driver
│   └── ...                # Future: eink, sdcard, audio
│
├── fs/                    # Filesystem (future)
├── syscall/               # System call interface (future)
├── shell/                 # Command shell (future)
├── apps/                  # Applications (future)
└── libc/                  # Minimal libc for Doom (future)
```

**Note:** The panic handler currently lives in `main.rs`. It will move to `kernel/panic.rs` when adding hardware support (Phase 13+).
