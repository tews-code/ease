# EASE OS - Project Plan

## Overview

**EASE** is a hobby operating system written in Rust for the Raspberry Pi Pico 2 (RP2350) running in RISC-V mode. It features dual-core support, a simple shell, and runs three applications: Doom (shareware), a text editor with spell checking, and an alarm clock.

### Design Philosophy
- Minimal code size and maximum simplicity
- Highly idiomatic Rust using many language features
- Clean, educational code structure
- Avoid third-party crates (implement from scratch for learning)
- Robustness over performance
- **No `static mut`** — use safe abstractions (`Mutex`, `OnceCell`, atomics)
- **Interrupt-driven I/O** where appropriate, not pure polling
- **User/kernel separation** — applications run in RISC-V User mode
- **Test-driven development** — each feature requires tests before commit
  - Tests run via `cargo test` (QEMU for hardware, host for pure logic)
  - Commit workflow: implement → test → **`./ci.sh`** → commit → update status → push
  - **`./ci.sh` is mandatory before every commit.** This is not optional. Only skip with explicit human approval and a stated reason. Claude must never skip CI on its own judgement.
  - **Status tracking:** Update `.claude/project.md` "Current Status" section when completing phase milestones
- **Rust 2024 Edition** — uses latest language features including:
  - New `unsafe` attribute syntax (`#[unsafe(no_mangle)]`, `#[unsafe(link_section)]`)
  - Stricter unsafe block requirements
  - Improved ergonomics and diagnostics

### Documentation
- **rustdoc** for all public APIs with examples
- Run `cargo doc --open` to view
- Every public function, struct, and module gets `///` doc comments
- Use `#![warn(missing_docs)]` to enforce

### Repository Management
- **GitHub** for version control and collaboration
- Repository: `github.com/<username>/ease`
- Branch strategy: `main` for stable, feature branches for development
- Issues for tracking tasks and bugs
- Releases/tags at each major milestone

### Educational Objectives & Collaboration Model

This is an educational project. The primary goal is learning through hands-on implementation.

**Your Background:**
- Rust: Intermediate (comfortable with ownership/borrowing, written projects)
- Embedded: Familiar with registers, interrupts, linker scripts, QEMU; new to physical microcontrollers
- This means: Less explanation needed for Rust concepts, more focus on hardware specifics and embedded patterns

**Human Role (You):**
- Write all code personally
- Make architectural decisions
- Debug with guidance
- Learn by doing

**Claude Role (Assistant):**
- Code review and feedback
- Build and run tests
- Help with debugging (explain errors, suggest fixes)
- Answer questions about concepts
- Provide reference examples when stuck (not copy-paste solutions)
- Validate against the project plan
- Focus explanations on embedded/hardware topics rather than basic Rust

**Principles:**
1. Claude will not write production code for the project
2. Claude will explain *why* something is wrong, not just *what* to change
3. When stuck, Claude provides hints before solutions
4. Claude encourages idiomatic Rust and best practices
5. Learning takes priority over speed of completion

**Allowed Third-Party Crates:**
- `cc` — For compiling Doom's C code in build.rs (no learning value in reimplementing)

---

## Hardware Specification

### Core Platform
| Component | Choice | Notes |
|-----------|--------|-------|
| MCU | RP2350 (Hazard3 RISC-V cores) | Dual-core RV32IMAC @ 150MHz |
| Board | Raspberry Pi Pico 2 | Standard board + modifications |
| PSRAM | APS6404L-3SQR (8MB) | Dead-bug soldered to QSPI pins |
| SRAM | 520KB (built-in) | For stack, heap, atomics |

### Display
| Component | Choice | Notes |
|-----------|--------|-------|
| Display | Waveshare 10.3" E-Ink HAT | 1872×1404, IT8951 controller |
| Interface | SPI | Via IT8951 driver board |
| Refresh | Partial refresh supported | ~0.3s partial, ~2s full |
| Resolution | Native 1872×1404 | Doom renders at 320×200, scaled/centered |

**Display Model:** Waveshare 10.3inch e-Paper E-Ink Display HAT
- Black/white with 16 grayscale levels
- USB/SPI/I80 interface options (use SPI)
- IT8951 controller handles partial refresh
- ~$80-100 USD

**Note on Doom:** E-ink will have significant ghosting and low frame rate (~5-10fps best case). This is accepted as a deliberate design choice.

### Input
| Component | Choice | Notes |
|-----------|--------|-------|
| Keyboard | USB low-profile keyboard | Apple Magic Keyboard style |
| Interface | USB Host mode | TinyUSB or custom HID driver |

### Audio
| Component | Choice | Notes |
|-----------|--------|-------|
| Output | PWM audio | Single GPIO pin |
| Filter | RC low-pass filter | Simple resistor + capacitor |
| Speaker | Small 8Ω speaker or 3.5mm jack | User preference |

### Storage
| Component | Choice | Notes |
|-----------|--------|-------|
| Media | MicroSD card | FAT16 filesystem |
| Interface | SPI | Standard SD card breakout |
| Capacity | 4GB+ recommended | Stores WAD, documents, settings |

### Power
| Component | Choice | Notes |
|-----------|--------|-------|
| Battery | LiPo pouch cell, 5-6mm thick | ~1000-2000mAh |
| Charging | TP4056 module or similar | USB charging |
| Regulation | Pico 2 built-in | 3.3V from LiPo |

### Debug
| Component | Choice | Notes |
|-----------|--------|-------|
| Probe | Raspberry Pi Debug Probe | ~$12 USD |
| Interface | SWD + UART | GDB debugging + serial console |

---

## Software Architecture

### Memory Layout (520KB SRAM + 8MB PSRAM)

```
SRAM (520KB) - 0x20000000
├── 0x20000000: Kernel Stack (4KB)
│   └── Used during trap handling and scheduling
├── 0x20001000: Core 1 I/O Stack (4KB)
├── 0x20002000: Kernel Heap (64KB)
│   └── Sync primitives, small allocations
├── 0x20012000: Shared IPC Region (4KB)
│   ├── Spinlock-protected queues
│   ├── Audio double-buffer (2KB)
│   └── Input event queue (1KB)
├── 0x20013000: Thread Stack Pool (40KB)
│   ├── Thread 0 (Idle): 2KB
│   ├── Thread 1 (Shell): 2KB
│   ├── Thread 2 (Doom): 16KB
│   ├── Thread 3 (Editor): 4KB
│   ├── Thread 4 (Alarm): 2KB
│   └── Threads 5-7: 4KB each (reserved)
├── 0x2001D000: TCBs + Scheduler State (2KB)
├── 0x2001D800: Reserved (remaining ~398KB)
│   └── Display buffer, working memory
└── 0x20082000: End of SRAM

PSRAM (8MB) - 0x11000000 (XIP region)
├── Doom WAD data (~4MB)
├── Document buffers
├── Large heap allocations
└── Framebuffer (if needed)

Flash (4MB) - 0x10000000
├── Bootloader
├── Kernel binary
└── Read-only assets
```

### Dual-Core Design

The dual-core architecture follows the **asymmetric multiprocessing (AMP)** pattern common in embedded systems:

- **Core 0 (Application Core):** Runs kernel and user applications
- **Core 1 (I/O Core):** Dedicated to interrupt handling and I/O servicing

This separation provides predictable latency for I/O operations and keeps the application core free from interrupt jitter.

```
┌─────────────────────────────────────────────────────────────┐
│                        CORE 0 - Application                  │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ Kernel (Machine Mode)                                │    │
│  │ - Syscall handler                                   │    │
│  │ - Memory management                                 │    │
│  │ - Process/context management                        │    │
│  │ - IPC with Core 1                                   │    │
│  └─────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ User Mode Applications                               │    │
│  │ - Shell / Doom / Editor / Alarm                     │    │
│  │ - Syscalls for I/O (trapped to kernel)              │    │
│  │ - No direct hardware access                         │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
                          │
                    IPC (SRAM)
                    Lock-free queues
                    Shared buffers
                          │
┌─────────────────────────────────────────────────────────────┐
│                        CORE 1 - I/O                          │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ Interrupt Handlers (Machine Mode)                    │    │
│  │ - Timer interrupt → system tick, audio refill       │    │
│  │ - USB interrupt → keyboard HID reports              │    │
│  │ - SPI interrupt → SD card DMA completion            │    │
│  │ - UART interrupt → serial I/O                       │    │
│  └─────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ I/O Service Loop (background, low priority)          │    │
│  │ - Process completed DMA transfers                   │    │
│  │ - Refill audio buffers when low                     │    │
│  │ - Handle non-urgent housekeeping                    │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

**Design Rationale:**

| Principle | Implementation |
|-----------|----------------|
| **Interrupt-driven I/O** | Core 1 handles all hardware interrupts; Core 0 never interrupted by peripherals |
| **Bounded latency** | Audio/USB interrupts on Core 1 have deterministic response time |
| **User/kernel separation** | Apps on Core 0 run in User mode, syscall into kernel |
| **Lock-free IPC** | SPSC queues between cores avoid blocking |
| **No shared interrupts** | Each core has dedicated interrupt sources |

**Privilege Levels:**

| Core | Mode | Purpose |
|------|------|---------|
| Core 0 | Machine | Kernel, syscall handler, trap handler |
| Core 0 | User | Applications (Shell, Doom, Editor, Alarm) |
| Core 1 | Machine | I/O handlers (never enters User mode) |

**Note:** RISC-V User mode provides memory protection via PMP (Physical Memory Protection), preventing applications from accessing kernel memory or hardware directly.

### Kernel Modules

```
ease/
├── Cargo.toml
├── rust-toolchain.toml        # Pins nightly for custom_test_frameworks
├── ci.sh                      # Local CI script (format, clippy, tests)
├── memory-qemu.x              # QEMU virt linker script
├── memory-rp2350.x            # RP2350 linker script (future)
├── build.rs                   # Compiles Doom C code (future)
│
├── .cargo/
│   └── config.toml            # Build target, runner config
│
├── docs/
│   ├── EASE_PROJECT_PLAN.md   # This file
│   └── uart.md                # UART documentation
│
├── src/
│   ├── main.rs                # Entry point, panic handler, QEMU tests
│   ├── lib.rs                 # Library crate for host-testable pure logic
│   ├── io.rs                  # Re-exports UartWriter, test capture buffer
│   ├── qemu.rs                # QEMU exit mechanism (sifive_test device)
│   ├── bench.rs               # Benchmarking via RISC-V cycle counter
│   │
│   ├── arch/                  # Architecture-specific
│   │   ├── mod.rs
│   │   ├── boot.rs            # Startup assembly, stack init
│   │   ├── trap.rs            # Trap vector and handler
│   │   ├── timer.rs           # CLINT timer, ticks, sleep
│   │   └── multicore.rs       # Core 1 launch, parking (future)
│   │
│   ├── hal/                   # Hardware Abstraction Layer
│   │   ├── mod.rs             # HAL traits (Writer)
│   │   ├── qemu_virt.rs       # QEMU virt implementation (UartWriter)
│   │   └── rp2350/            # (future)
│   │       ├── mod.rs
│   │       ├── gpio.rs
│   │       ├── spi.rs
│   │       ├── uart.rs
│   │       ├── timer.rs
│   │       ├── pwm.rs
│   │       ├── dma.rs
│   │       ├── usb_host.rs
│   │       └── psram.rs
│   │
│   ├── kernel/                # Core kernel services
│   │   ├── mod.rs
│   │   ├── alloc.rs           # Bump allocator, GlobalAlloc
│   │   ├── sync.rs            # Spinlocks, mutexes, channels
│   │   └── sched.rs           # Preemptive scheduler, threads (future)
│   │
│   ├── drivers/               # Device drivers
│   │   ├── mod.rs
│   │   ├── ramfb.rs           # QEMU ramfb framebuffer
│   │   ├── eink/              # (future)
│   │   │   ├── mod.rs
│   │   │   └── it8951.rs      # IT8951 controller driver
│   │   ├── sdcard.rs          # SPI SD card (future)
│   │   ├── keyboard.rs        # USB HID keyboard (future)
│   │   └── audio.rs           # PWM audio output (future)
│   │
│   ├── fs/                    # Filesystem (future)
│   │   ├── mod.rs
│   │   ├── fat16.rs           # FAT16 implementation
│   │   └── vfs.rs             # Virtual filesystem layer
│   │
│   ├── syscall/               # System call interface (future)
│   │   ├── mod.rs
│   │   ├── io.rs              # read, write, open, close
│   │   ├── mem.rs             # malloc, free
│   │   ├── time.rs            # ticks, sleep
│   │   ├── display.rs         # draw, refresh
│   │   └── input.rs           # getkey
│   │
│   ├── shell/                 # Command shell (future)
│   │   ├── mod.rs
│   │   ├── parser.rs          # Command line parsing
│   │   └── commands/
│   │       ├── mod.rs
│   │       ├── ls.rs
│   │       ├── cd.rs
│   │       ├── cat.rs
│   │       ├── hexdump.rs
│   │       ├── date.rs
│   │       ├── doom.rs        # Launch Doom
│   │       ├── edit.rs        # Launch editor
│   │       └── alarm.rs       # Launch alarm
│   │
│   ├── apps/                  # Applications (future)
│   │   ├── mod.rs
│   │   ├── doom/
│   │   │   ├── mod.rs
│   │   │   └── bridge.rs      # FFI to doomgeneric
│   │   ├── editor/
│   │   │   ├── mod.rs
│   │   │   ├── buffer.rs      # Gap buffer implementation
│   │   │   ├── display.rs     # Text rendering
│   │   │   └── spellcheck.rs  # Dictionary lookup
│   │   └── alarm/
│   │       ├── mod.rs
│   │       └── ui.rs
│   │
│   └── libc/                  # Minimal libc for Doom (future)
│       ├── mod.rs
│       ├── string.rs          # memcpy, strlen, etc.
│       ├── stdlib.rs          # atoi, abs, etc.
│       └── stdio.rs           # printf stub
│
├── tests/                     # Host integration tests (future)
│   └── *.rs                   # Standard #[test] functions
│
├── doom/                      # doomgeneric C source (future)
│   └── doomgeneric/
│       └── *.c
│
└── assets/                    # (future)
    ├── doom1.wad              # Shareware WAD (not in repo)
    └── dictionary.txt         # Spell check word list
```

**Note:** The panic handler currently lives in `main.rs` for simplicity. It will move to `kernel/panic.rs` when adding hardware support (Phase 13+), to accommodate platform-specific behavior like LED blink codes and UART-only output.

---

## Syscall Interface

Applications run in **RISC-V User mode** and request kernel services via the `ecall` instruction. This provides:

- **Memory protection:** Apps cannot access kernel memory or hardware directly
- **Controlled I/O:** All hardware access goes through the kernel
- **Fault isolation:** App crashes don't take down the kernel

### Syscall Mechanism

```
User Mode (Application)          Machine Mode (Kernel)
        │                               │
        │  ecall instruction            │
        ├──────────────────────────────►│
        │                               │ - Read a0-a7 for args
        │                               │ - Dispatch to handler
        │                               │ - Perform operation
        │                               │ - Set a0 for return
        │  mret instruction             │
        │◄──────────────────────────────┤
        │                               │
```

### Syscall Numbers and ABI

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
| `sys_display_* ` | 30-39 | (varies) | (varies) |
| `sys_audio_*` | 40-49 | (varies) | (varies) |
| `sys_malloc` | 50 | a0: size | a0: ptr or null |
| `sys_free` | 51 | a0: ptr | — |

### User-space Wrapper Library

Applications link against a small library providing safe Rust wrappers:

```rust
// In user-space library (linked by apps)
pub fn print(s: &str) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 1,        // syscall number
            in("a0") s.as_ptr(),
            in("a1") s.len(),
            options(nostack)
        );
    }
}

pub fn ticks_ms() -> u32 {
    let result: u32;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 10,
            lateout("a0") result,
            options(nostack)
        );
    }
    result
}

// ... similar wrappers for other syscalls
```

### PMP Configuration

Physical Memory Protection restricts user mode access:

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

impl<const N: u32> HwSpinlock<N> {
    pub fn lock(&self) -> HwSpinlockGuard<N>;
}

// Software spinlock (for when hardware locks exhausted)
pub struct Spinlock {
    locked: AtomicBool,  // Must be in SRAM!
}

// Mutex with data protection
pub struct Mutex<T> {
    lock: Spinlock,
    data: UnsafeCell<T>,
}

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

## Development Phases

### Philosophy: QEMU-First Development

This project takes a **simulation-first approach**. We develop as much as possible in QEMU before touching real hardware. Benefits:

- Faster iteration (no flash/reboot cycle)
- Better debugging (full GDB support, printf debugging)
- No hardware damage risk during learning
- Work without physical components

### Learning Progression

The phases are sequenced to introduce concepts gradually:

```
Phase 0: Setup            → GitHub, toolchain, project structure
Phase 1-2: Foundations    → no_std basics, boot, UART output
Phase 3-4: Core Services  → memory management, interrupts
Phase 5-6: I/O Systems     → display, input, shell
Phase 7-8: Applications    → editor, alarm (simpler apps first)
Phase 9-10: Doom          → FFI, libc, complex integration
Phase 11: Audio           → optional enhancement
Phase 12-12A: Concurrency → dual-core, preemptive scheduler
Phase 13-18: Hardware     → port everything to real Pico 2
```

Each phase builds directly on the previous. No phase requires concepts not yet introduced.

---

### Phase 0: Project Setup (Week 0)
**Goal:** Repository, toolchain, and documentation infrastructure ready
**New concepts:** GitHub workflow, rustdoc, cargo workspace

Complete this before Week 1 starts. This is setup, not development.

#### GitHub Repository Setup
- [x] Create GitHub account (if needed)
- [x] Create new repository: `ease`
- [x] Initialize with README.md and .gitignore (Rust template)
- [x] Clone locally: `git clone git@github.com:<username>/ease.git`
- [x] Set up SSH key for GitHub (if not done)

#### Project Structure
- [x] Create Cargo.toml with project metadata
- [x] Add `#![warn(missing_docs)]` to enforce documentation
- [x] Create initial directory structure:
  ```
  ease/
  ├── .github/
  │   └── workflows/       # CI later
  ├── src/
  │   └── lib.rs          # Start here
  ├── Cargo.toml
  ├── README.md
  └── EASE_PROJECT_PLAN.md
  ```
- [x] First commit: "Initial project structure"

#### Toolchain Setup
- [x] Install Rust via rustup
- [x] Add target: `rustup target add riscv32imac-unknown-none-elf`
- [x] Install QEMU: `apt install qemu-system-riscv32` (or equivalent)
- [x] Install GDB: `apt install gdb-multiarch`
- [x] Verify: `qemu-system-riscv32 --version`

#### Documentation Setup
- [x] Configure Cargo.toml for rustdoc:
  ```toml
  [package]
  name = "ease"
  version = "0.1.0"
  edition = "2024"
  description = "E-ink Alarm Shell Editor - A hobby OS for RP2350"
  documentation = "https://<username>.github.io/ease"

  [package.metadata.docs.rs]
  targets = ["riscv32imac-unknown-none-elf"]
  ```
- [x] Test rustdoc: `cargo doc --open`
- [x] Add initial module documentation to lib.rs

#### Continuous Integration (CI)
- [x] Create `ci.sh` local CI script with:
  - **Format check:** `cargo fmt --check`
  - **Clippy:** `cargo clippy --target riscv32imac-unknown-none-elf -- -D warnings`
  - **QEMU tests:** `cargo test --bin ease` (runs on QEMU virt machine)
  - **Host tests:** `cargo test --lib` (tests lib.rs on host machine)
  - **Documentation:** `cargo doc --no-deps`
- [x] Run `./ci.sh` before commits to catch issues early
- [ ] (Optional) Add GitHub Actions later if needed for collaboration

#### Git Workflow
- [x] Create develop branch: `git checkout -b develop`
- [ ] Set branch protection on main (optional)
- [x] Practice: make change, commit, push, create PR, merge
- [x] Tag setup complete: `git tag -a v0.0.1 -m "Project setup complete"`

#### Commit Checklist
Before each commit:
1. **Run `./ci.sh` — all checks must pass.** This is a hard requirement, not a suggestion. Never commit without a passing CI run. The only exception is an explicit human override with a stated reason (e.g., CI infrastructure is broken). Claude must never decide to skip this step independently.
2. If completing a phase milestone, update `.claude/project.md` "Current Status" section
3. Commit with descriptive message
4. Push to remote

**Milestone 0:** Repository ready, toolchain working, local CI script (`./ci.sh`) passing, `cargo doc` generates documentation

---

### Phase 1: Hello QEMU (Weeks 1-2)
**Goal:** Get Rust code running on QEMU and printing to console
**New concepts:** `no_std`, linker scripts, QEMU basics, volatile access, inline assembly

This is your "Hello World" moment. Everything else builds on this.

**Prerequisites:** None (first phase)

#### Week 1: Environment & First Boot
- [x] Install Rust, add `riscv32imac-unknown-none-elf` target
- [x] Install QEMU (`qemu-system-riscv32`)
- [x] Create new Cargo project with `#![no_std]`, `#![no_main]`
- [x] **Learn:** What `no_std` means and why embedded needs it
- [x] Write minimal `_start` function (just infinite loop)
- [x] **Learn:** What a linker script does (where code/data goes in memory)
- [x] Create simple linker script for QEMU virt machine
  - **Note:** Use `.text.init` section and `#[link_section]` to ensure `_start` is placed at entry point
- [x] Boot on QEMU - confirm it doesn't crash (verified via QEMU monitor `info registers`)
- [x] **Doc:** Add `//!` module docs explaining the boot process

#### Week 2: UART Output
- [x] **Learn:** What UART is (serial communication)
- [x] **Learn:** Memory-mapped I/O and volatile access
- [x] Find QEMU virt UART address (0x10000000)
- [x] **Prerequisite:** Set up stack pointer before any function calls
  - Use `#[naked]` function with inline asm to set `sp` before jumping to Rust code
  - **Why:** `write_volatile` and other functions may use the stack; without valid `sp`, CPU will fault
- [x] Write single character to UART using `core::ptr::write_volatile`
- [x] See character appear in QEMU console - celebrate!
- [x] Implement `print!` / `println!` macros using `core::fmt::Write`
- [x] Print "Hello from EASE!"
- [x] **Learn:** `core::fmt::Write` trait
- [x] **Doc:** Create `docs/uart.md` documenting:
  - UART protocol basics (baud rate, framing, flow control)
  - QEMU virt 16550 UART memory map and registers
  - How `hal/mod.rs` defines `Writer` trait
  - Usage examples for `print!`/`println!` macros

#### Week 2 (continued): Testing Infrastructure
- [x] Set up custom test framework for QEMU (`#![feature(custom_test_frameworks)]`)
- [x] Set up QEMU exit mechanism (`src/qemu.rs` with sifive_test device)
- [x] Verify QEMU tests pass with exit code 0, failures exit with code 1
- [ ] Set up host-side integration tests (`tests/` directory)
  - Create `tests/` directory for integration tests that run on host
  - Tests automatically use std (no `no_std` constraint)
  - **Note:** Currently blocked — `cargo test --tests` tries to compile `main.rs` which contains RISC-V assembly. Requires making binary target-conditional.
  - For now, use `cargo test --lib` to test `lib.rs` on host
- [x] Add `rust-toolchain.toml` to pin nightly toolchain
- [x] Create I/O abstraction for testable output (`src/io.rs`)
  - `Writer` trait for byte-level output
  - `UartWriter` implementation for QEMU UART
  - Test capture buffer (4KB static buffer with atomic length)
  - `test_io::clear()`, `output()`, `contains()`, `equals()` helpers
- [x] Add tests that verify print output content
- [x] Create benchmarking infrastructure (`src/bench.rs`)
  - RISC-V cycle counter via `rdcycle`/`rdcycleh` CSRs
  - `bench::measure()`, `run()`, `run_avg()` functions
  - `bench::check()` for regression detection with baselines
  - Regression tests for `print!`/`println!` operations
  - Tests fail if performance degrades beyond 20% tolerance
- [ ] Add example benchmarks demonstrating `bench::run()` and `bench::run_avg()`
  - Single-run benchmark example
  - Averaged benchmark example (useful for noisy operations)
- [x] **Learn:** `UnsafeCell`, `AtomicUsize`, trait-based abstraction, inline asm for CSRs

##### Implementation Approach: Testing & Benchmarking

**Dual-Testing Architecture:**

The crate uses a split architecture to support testing on both QEMU (hardware-dependent code) and the host machine (pure logic):

```
┌─────────────────────────────────────────────────────────────┐
│  src/main.rs - QEMU tests                                   │
│  - #![feature(custom_test_frameworks)]                      │
│  - #[test_case] functions run on QEMU virt machine          │
│  - Tests UART, memory-mapped I/O, interrupts, boot          │
│  - Uses sifive_test device for clean QEMU exit              │
│  - Run: cargo test --bin ease                               │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  src/lib.rs - Host tests                                    │
│  - Standard #[test] functions run on host machine           │
│  - Automatically uses std (no no_std constraint)            │
│  - Tests parsers, data structures, algorithms               │
│  - No hardware dependencies                                 │
│  - Run: cargo test --lib                                    │
└─────────────────────────────────────────────────────────────┘

Note: tests/ directory integration tests are blocked because
`cargo test --tests` also compiles main.rs which contains
RISC-V assembly. Requires target-conditional compilation.
```

**I/O Abstraction (`src/io.rs`):**

The `Writer` trait abstracts byte-level output, enabling:
1. Hardware output via `UartWriter` (writes to 0x10000000)
2. Test capture via a 4KB static buffer with atomic length counter

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

**Benchmarking (`src/bench.rs`):**

Uses RISC-V cycle counter CSRs (`rdcycle`/`rdcycleh`) for precise measurements without timer setup:

```rust
fn cycles() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        asm!("rdcycleh {hi}", "rdcycle {lo}", ...);
    }
    ((hi as u64) << 32) | (lo as u64)
}
```

Key functions:
- `measure(f)` - Returns cycle count for closure
- `run(name, f)` - Prints single measurement
- `run_avg(name, iters, f)` - Prints averaged measurement
- `check(name, baseline, iters, f)` - Fails if >20% regression

**Regression Detection:**

Baselines stored in `src/main.rs`:
```rust
mod baselines {
    pub const PRINTLN_HELLO: u64 = 32_000;
    // Update when intentionally changing performance
}
```

Tests call `bench::check()` with baseline; failure triggers panic with "REGRESSION" message.

**Testing commands:**
```bash
cargo test --bin ease    # QEMU tests + benchmarks (runs on QEMU virt)
cargo test --lib         # Host tests in lib.rs (runs on host machine)
cargo run                # Normal run (boots on QEMU)
```

**Milestone 1:** "Hello from EASE!" prints to QEMU console; test and benchmark infrastructure operational

**Rust concepts introduced:** `no_std`, `no_main`, raw pointers, volatile, traits (`Write`, `Writer`), macros, `#[naked]` functions, `UnsafeCell`, atomics, `cfg(test)`, inline asm

---

### Phase 2: Kernel Foundations (Weeks 3-4)
**Goal:** Proper boot sequence, panic handling, basic project structure
**New concepts:** BSS initialization, panic handlers, modules

**Prerequisites:** Phase 1 complete (stack pointer already set up in Week 2)

#### Week 3: Proper Boot Sequence
- [x] **Learn:** What happens before `main()` (stack, BSS, etc.)
- [x] **Note:** Stack pointer setup already done in Phase 1 Week 2
- [x] **Learn:** RISC-V calling convention basics (just sp and ra)
- [x] Define stack region properly in linker script (e.g., 8KB with symbols)
- [x] Define BSS section, add symbols for start/end (`__bss_start`, `__bss_end`)
- [x] Implement BSS zeroing in early Rust (before using any statics)
- [x] Create `src/arch/mod.rs` and `src/arch/boot.rs`
- [x] Move `_start` and boot code to `src/arch/boot.rs`
- [x] Verify BSS works: add a `static` variable, confirm it starts as zero

#### Week 4: Panic Handler & Project Structure
- [x] **Learn:** Why `#[panic_handler]` is required in `no_std`
- [x] Implement panic handler that prints message and location
- [x] Implement infinite loop after panic (with `wfi` instruction)
- [x] Test panic with `panic!("test panic")`
- [x] Create module structure:
  - `src/arch/` - architecture-specific code
  - `src/kernel/` - kernel services (empty for now)
  - `src/hal/` - hardware abstraction (empty for now)
- [x] Move UART code to `src/hal/qemu_virt.rs`
- [x] **Learn:** Rust module system, `pub`, `pub(crate)`

**Milestone 2:** Proper boot sequence, panic handler works, clean project structure

**Rust concepts practiced:** Inline assembly (minimal), modules, visibility

---

### Phase 3: Memory Management (Weeks 5-6)
**Goal:** Dynamic memory allocation working
**New concepts:** Allocators, `GlobalAlloc`, unsafe, heap vs stack, `Spinlock`

**Prerequisites:** Phase 2 complete (BSS working, project structure)

#### Week 5: Bump Allocator
- [x] **Learn:** Stack vs heap, why dynamic allocation matters
- [x] **Learn:** What an allocator does (manage free memory)
- [ ] **Align QEMU memory layout with RP2350:** (deferred to Phase 13)
  - Update `memory-qemu.x` to simulate RP2350's memory constraints
  - Define SRAM region at 0x20000000 (520KB, matching RP2350)
  - Define PSRAM region at 0x11000000 (8MB, for large allocations)
  - Keep total memory realistic so QEMU development reveals real constraints
  - **Note:** QEMU virt uses different addresses; create abstraction or remap
- [x] Define heap region in linker script (e.g., 64KB at known address within SRAM)
- [x] **First:** Implement basic `Spinlock` using `AtomicBool`
  - Needed for thread-safe allocator (and will be used throughout project)
  - Simple spin-wait loop with `Acquire`/`Release` ordering
- [x] Implement simple bump allocator:
  - Use `Spinlock<BumpAllocatorInner>` — **not `static mut`** (design philosophy)
  - `alloc()` bumps pointer forward, returns old value
  - `dealloc()` does nothing (memory never freed)
- [x] **Learn:** Why bump allocator leaks memory (and why that's okay for now)
- [x] Wrap in a struct with safe interface

#### Week 6: GlobalAlloc Integration
- [x] **Learn:** Rust's `GlobalAlloc` trait and `#[global_allocator]`
- [x] Implement `GlobalAlloc` for your bump allocator
- [x] **Learn:** Why `alloc` requires unsafe (raw pointers, alignment)
- [x] Enable `alloc` crate (`extern crate alloc`)
- [x] Test with `alloc::vec::Vec` - create vector, push items, print length
- [x] Test with `alloc::string::String`
- [x] Add out-of-memory handler (`#[alloc_error_handler]`) — returns null pointer
- [ ] **Stretch:** Implement simple free-list allocator (reuses memory)

**Milestone 3:** Can use `Vec`, `String`, `Box` in kernel code; basic `Spinlock` working

**Rust concepts introduced:** `GlobalAlloc`, `unsafe` blocks, raw pointers, alignment, `alloc` crate, `AtomicBool`, basic spinlock

---

### Phase 4: Time & Interrupts (Weeks 7-8)
**Goal:** Interrupt-driven timer, proper system tick
**New concepts:** RISC-V trap handling, interrupt-safe state, `Mutex` for shared state

**Prerequisites:** Phase 3 complete (`Spinlock` available for building `Mutex`)

Interrupts are fundamental to the system design. Core 1 will be interrupt-driven, so we learn this now.

#### Week 7: Trap Handler Foundation
- [x] **Learn:** RISC-V trap model (mtvec, mcause, mepc, mtval)
- [x] **Learn:** Difference between exceptions and interrupts
- [x] Write trap vector in assembly (save all registers to stack)
- [x] Implement Rust trap dispatcher (reads mcause, calls appropriate handler)
- [x] Set up `mtvec` to point to trap vector
- [x] Test with illegal instruction exception (verify handler runs)
- [x] **Learn:** Why we save/restore registers (context preservation)
- [ ] Implement `Mutex<T>` wrapper using `Spinlock` from Phase 3 (deferred)
  - Disables interrupts while held (critical section)

#### Week 8: Timer Interrupt
- [x] **Learn:** CLINT timer (mtime, mtimecmp registers)
- [x] Enable machine timer interrupt (`mie.MTIE` bit)
- [x] Set `mtimecmp` to trigger interrupt after N ticks
- [x] Handle timer interrupt: update system tick counter, reset mtimecmp
- [x] Use `AtomicUsize` for tick counter (wraps at ~49 days, acceptable for now)
- [x] Implement `ticks_ms()` reading from atomic counter
- [x] Implement `sleep_ms()` using WFI + tick checking
- [x] Test: print timestamp, sleep 1 second, print again
- [x] **Learn:** Critical sections and interrupt masking

**Milestone 4:** Timer interrupt fires regularly, `sleep_ms()` works correctly

**Rust concepts practiced:** Trap handling, atomics, no `static mut`

**Pattern Note:** All shared mutable state uses synchronization primitives:
- `AtomicUsize` for simple counters (used for tick counter)
- `SpinLock<T>` for complex data (used in allocator)
- Never `static mut`

---

### Phase 5: Display Output (Weeks 9-11)
**Goal:** Render text to QEMU's graphical framebuffer
**New concepts:** Framebuffers, bitmap fonts, traits for abstraction, QEMU device configuration

**Prerequisites:** Phase 4 complete (timer working for any refresh timing)

This phase provides **visual feedback**, making subsequent development more satisfying.

#### Week 9: QEMU Framebuffer Setup
- [x] **Learn:** QEMU virt machine's framebuffer device options
- [x] **Option A: ramfb** (complex but flexible)
  - Requires fw_cfg protocol: write framebuffer config to special DMA region
  - fw_cfg at 0x10100000 on QEMU virt (selector + data + DMA registers)
  - Must allocate framebuffer memory, register it with fw_cfg
  - **Note:** This is more complex than simple MMIO — expect to spend time here
- [ ] **Option B: virtio-gpu** (skipped — ramfb sufficient)
- [ ] **Option C: Serial/text only** (skipped — ramfb works)
- [x] Configure QEMU: `-device ramfb` and remove `-nographic`
- [x] Decide on resolution: 640×480 for development (smaller than e-ink)
- [x] Decide on pixel format: 32-bit XR24 (0x00RRGGBB)
- [x] Write directly to framebuffer memory, see pixels appear on screen
- [x] Implement: clear screen, set pixel

#### Week 10: Display Abstraction & Text (deferred to Phase 6)
- [ ] Create `Display` trait: `width()`, `height()`, `set_pixel(x, y, color)`, `clear(color)`
- [ ] Implement `QemuDisplay` using ramfb/virtio-gpu
- [ ] **Learn:** Bitmap fonts (each character is a small pixel grid)
- [ ] Find or create 8×16 bitmap font (common size, good readability)
- [ ] Implement `draw_char(x, y, char, color)` - copy font bitmap to framebuffer
- [ ] Implement `draw_string(x, y, &str)` - draw characters in sequence
- [ ] Handle newlines (move to next row)
- [ ] Test: draw "Hello from EASE!" and see it on screen 🎉

#### Week 11: Console Abstraction (deferred to Phase 6)
- [ ] Create `Console` struct (tracks cursor position, handles scrolling)
- [ ] Implement `core::fmt::Write` trait for `Console`
- [ ] Add scrolling: when cursor reaches bottom, shift all rows up
- [ ] Create `display_println!` macro
- [ ] Test: print many lines, verify scrolling works
- [ ] Add grayscale support (for e-ink compatibility later)
- [ ] **Stretch:** Implement double-buffering to reduce flicker

**Milestone 5:** QEMU ramfb framebuffer working with clear/set_pixel (text rendering deferred)

**Rust concepts practiced:** Defining traits, implementing traits, generics introduction

---

### Phase 6: Input & Shell (Weeks 12-14)
**Goal:** Interactive command-line shell
**New concepts:** Input handling, parsing, command dispatch, state machines

**Prerequisites:** Phase 5 complete (display for visual feedback), Phase 4 (timer for key repeat)

#### Week 12: Keyboard Input
- [ ] **Learn:** How keyboard input works in QEMU (serial input via UART)
- [ ] **Learn:** 16550 UART RX side — check LSR (Line Status Register) bit 0 for data ready
  - TX just writes to THR; RX must poll LSR before reading RBR
  - LSR at UART_BASE + 5, RBR at UART_BASE + 0
- [ ] Implement `read_char()` - poll UART LSR, return `Option<char>` if data ready
- [ ] Implement `wait_char()` - block until character received
- [ ] Create `Keyboard` trait with `poll()` returning `Option<KeyEvent>`
- [ ] Define `KeyEvent` enum: `Press(char)`, `Special(SpecialKey)`
- [ ] **Learn:** VT100/ANSI escape sequences for special keys
  - Arrow keys send `ESC [ A/B/C/D` (up/down/right/left)
  - Need state machine to parse multi-byte sequences
- [ ] Handle special keys: Enter, Backspace, arrow keys (escape sequences)

#### Week 13: Line Editor
- [ ] Implement line buffer (fixed-size array with length)
- [ ] Handle character input: append to buffer, echo to display
- [ ] Handle Backspace: remove last char, update display
- [ ] Handle Enter: return completed line
- [ ] Add cursor display (blinking underscore or block)
- [ ] **Stretch:** Add left/right arrow movement within line

#### Week 14: Shell & Commands
- [ ] Create shell loop: print prompt, read line, parse, execute
- [ ] Implement simple command parser (split on spaces)
- [ ] Implement `clear` command (clear display)
- [ ] Implement `echo` command (print arguments)
- [ ] Implement `help` command (list commands)
- [ ] Implement `time` command (print current ticks/ms)
- [ ] **Learn:** Command pattern (function pointers or match dispatch)

**Milestone 6:** Interactive shell with basic built-in commands

**Rust concepts introduced:** `Option` extensively, pattern matching, slices, function pointers/closures

---

### Phase 7: Storage & Filesystem (Weeks 15-18)
**Goal:** Read and write files on FAT16 filesystem
**New concepts:** Block devices, filesystem structures, file handles, buffering

**Prerequisites:** Phase 6 complete (shell for testing commands)

This phase is longer because filesystems are complex. Take it slow.

#### Week 15: Block Device Abstraction
- [ ] **Learn:** Block devices (read/write fixed-size blocks, typically 512 bytes)
- [ ] Create `BlockDevice` trait: `read_block(n, &mut buf)`, `write_block(n, &buf)`
- [ ] **Choose block device interface for QEMU:**
  - **Option A: pflash** (simplest) — memory-mapped flash, use `-drive if=pflash,file=disk.img,format=raw`
  - **Option B: virtio-blk** (more realistic but requires virtio driver)
  - Recommend pflash for QEMU phase; will use SPI for real hardware anyway
- [ ] Use QEMU's `-drive` option to attach a disk image
- [ ] Create test disk image on host: `dd if=/dev/zero of=disk.img bs=512 count=2048`
- [ ] Test: read block 0, print first 16 bytes as hex

#### Week 16: FAT16 Basics
- [ ] **Learn:** FAT16 structure (boot sector, FAT table, root directory, data)
- [ ] Format test disk as FAT16 on host: `mkfs.fat -F 16 disk.img`
- [ ] Parse boot sector: bytes per sector, sectors per cluster, FAT count, etc.
- [ ] Locate FAT table and root directory from boot sector values
- [ ] Read and print FAT table entries (as hex, just to understand)
- [ ] Create `Fat16` struct holding parsed boot sector info

#### Week 17: Reading Files
- [ ] **Learn:** FAT16 directory entries (32 bytes each, 8.3 filename)
- [ ] Read root directory, list all files (name, size, first cluster)
- [ ] Implement shell `ls` command using this
- [ ] **Learn:** Cluster chains (following FAT links)
- [ ] Implement `open(filename)` - find file, return handle with first cluster
- [ ] Implement `read(handle, buf)` - read data, follow cluster chain
- [ ] Implement shell `cat` command - print file contents
- [ ] Test: create file on host, read it in EASE

#### Week 18: Writing Files & VFS
- [ ] Implement `write(handle, data)` - allocate clusters, update FAT
- [ ] Implement `create(filename)` - create directory entry
- [ ] Handle file size updates on close
- [ ] Create VFS trait: `open()`, `read()`, `write()`, `close()`, `readdir()`
- [ ] Implement `cd` command (track current directory path)
- [ ] Implement `hexdump` command (read file, print hex + ASCII)
- [ ] Test: create file in EASE, verify on host

**Milestone 7:** Can list, read, create, and write files on FAT16 filesystem

**Rust concepts introduced:** More traits, `Result` and error handling, `?` operator, newtypes for handles

---

### Phase 8: Text Editor (Weeks 19-21)
**Goal:** Simple text editor with load/save
**New concepts:** Gap buffer data structure, modal interfaces

**Prerequisites:** Phase 7 complete (filesystem for load/save), Phase 6 (keyboard input), Phase 5 (display)

Building an application using the kernel services developed so far.

#### Week 19: Gap Buffer
- [ ] **Learn:** Gap buffer (efficient text buffer for editing)
- [ ] Implement `GapBuffer`: insert, delete, move cursor
- [ ] Test extensively: insert at start, middle, end; delete; move around
- [ ] Handle buffer growth (reallocate when gap shrinks to zero)

#### Week 20: Editor Display
- [ ] Create editor state: buffer, cursor position, scroll offset
- [ ] Render buffer to display (with cursor highlight)
- [ ] Handle scrolling when cursor moves off-screen
- [ ] Display status line (filename, line/column, modified indicator)

#### Week 21: Editor Commands
- [ ] Handle character input (insert into buffer)
- [ ] Handle Backspace/Delete
- [ ] Handle arrow keys (move cursor)
- [ ] Handle Home/End (move to line start/end)
- [ ] Implement Ctrl+S: save to file
- [ ] Implement Ctrl+O: load from file (prompt for filename)
- [ ] Implement Ctrl+Q: quit (prompt if unsaved changes)
- [ ] Add `edit` shell command to launch editor

**Milestone 8:** Functional text editor can create, edit, save, and load files

**Rust concepts introduced:** More complex state management, enums for modes/actions

---

### Phase 9: Alarm Clock (Weeks 22-23)
**Goal:** Time display, alarm setting and triggering
**New concepts:** Time formatting, persistent storage

**Prerequisites:** Phase 8 complete (editor done), Phase 7 (filesystem for saving alarms)

Simpler than editor, reinforces previous concepts.

**Note on Time:** There is no RTC (real-time clock) — only ticks since boot. Time-of-day must be:
1. Set manually via `settime` command after each boot
2. Stored as offset from boot ticks
3. On real hardware, could add RTC module later (Phase 18 stretch goal)

#### Week 22: Time Display
- [ ] Create alarm clock UI: large time display
- [ ] Track time-of-day as: `boot_ticks_at_settime` + `(current_ticks - boot_ticks_at_settime)`
- [ ] Format time as HH:MM:SS
- [ ] Update display every second (poll timer)
- [ ] Implement `date` shell command (show current time, or "time not set")
- [ ] Implement `settime HH:MM:SS` shell command (stores reference point)

#### Week 23: Alarms
- [ ] Create alarm data structure (hour, minute, enabled)
- [ ] Store alarms in file (`alarms.txt` - simple format)
- [ ] Load alarms on startup
- [ ] Check for triggered alarms each second
- [ ] Trigger alarm: flash display or print message
- [ ] Implement `alarm` shell command to add/list/remove alarms
- [ ] Add `clock` shell command to launch clock UI

**Milestone 9:** Alarm clock can display time, set alarms, and trigger them

**Rust concepts introduced:** Time formatting, simple file formats, periodic checking

---

### Phase 10: Doom Integration (Weeks 24-28)
**Goal:** Run Doom using doomgeneric
**New concepts:** FFI, C interop, libc stubs, build.rs

**Prerequisites:**
- Phase 3 (allocator for malloc/free)
- Phase 4 (timer for DG_GetTicksMs, DG_SleepMs)
- Phase 5 (display for DG_DrawFrame)
- Phase 6 (keyboard for DG_GetKey)
- Phase 7 (filesystem for WAD loading)

This is the most complex integration. Take 5 weeks.

#### Week 24: Build System Setup
- [ ] **Learn:** Rust FFI basics (`extern "C"`, `#[no_mangle]`)
- [ ] Download doomgeneric source code
- [ ] Study doomgeneric interface (6 functions to implement)
- [ ] Set up `build.rs` to compile C code using `cc` crate
- [ ] Get C code compiling (will have linker errors - that's okay)

#### Week 25: Libc Stubs
- [ ] **Learn:** What libc functions Doom needs (memcpy, strlen, malloc, printf, etc.)
- [ ] Implement `memcpy`, `memset`, `memmove` in Rust, export as `#[no_mangle]`
- [ ] Implement `strlen`, `strcpy`, `strcmp`
- [ ] Implement `malloc`/`free` wrapping your allocator
- [ ] Implement `printf` (parse format string, use your `print!`)
- [ ] Create `src/libc/` module for these
- [ ] Get Doom linking successfully (may still crash)

#### Week 26: Doom Core Functions
- [ ] Implement `DG_Init()` - any setup needed
- [ ] Implement `DG_GetTicksMs()` - return your `ticks_ms()`
- [ ] Implement `DG_SleepMs()` - call your `sleep_ms()`
- [ ] Implement `DG_SetWindowTitle()` - stub (do nothing)
- [ ] Get Doom starting (will show nothing without display)

#### Week 27: Doom Display
- [ ] **Learn:** Doom's framebuffer format (320×200, 32-bit ARGB)
- [ ] Implement `DG_DrawFrame()`:
  - Convert ARGB to grayscale
  - Scale 320×200 to your display size
  - Copy to your framebuffer
- [ ] See Doom title screen! 🎉
- [ ] Implement dithering for better grayscale appearance

#### Week 28: Doom Input & WAD Loading
- [ ] Map your key events to Doom key codes
- [ ] Implement `DG_GetKey()` - return next key from queue
- [ ] Modify Doom to load WAD from your filesystem (or embed in binary)
- [ ] Test menu navigation
- [ ] Test actual gameplay
- [ ] Add `doom` shell command

**Milestone 10:** Doom runs, displays, and accepts input

**Rust concepts introduced:** FFI, `extern "C"`, `#[no_mangle]`, build scripts, C string handling

---

### Phase 11: Audio System (Weeks 29-30)
**Goal:** Sound output for alarm beeps and Doom
**New concepts:** Digital audio, sample buffers, mixing, audio queues

**Prerequisites:** Phase 10 complete (Doom working, wants sound)

Audio is important for the alarm clock and enhances Doom significantly.

**Note on QEMU Audio:** The `virt` machine may have limited audio support. Options:
1. **virtio-sound** — requires virtio driver (complex)
2. **Skip QEMU audio** — implement Audio trait as no-op, test on real hardware
3. **PC speaker emulation** — some QEMU machines support, but not virt

Recommendation: Design the `Audio` trait now, but audio output may only work on real hardware (PWM). Use stub implementation for QEMU that logs instead of playing.

#### Week 29: Audio Abstraction
- [ ] **Learn:** Digital audio basics (samples, sample rate, bit depth, buffers)
- [ ] **Research:** QEMU virt audio options (may be limited)
- [ ] Create `Audio` trait: `sample_rate()`, `queue_samples(&[i16])`, `is_available()`
- [ ] Implement `StubAudio` that does nothing (for QEMU if no audio device works)
- [ ] Generate simple tones programmatically (sine wave at frequency)
- [ ] If QEMU audio works: test 440Hz tone
- [ ] If not: defer actual audio testing to Phase 17 (PWM on real hardware)

#### Week 30: Audio Integration
- [ ] Create audio buffer/queue system (producer-consumer for samples)
- [ ] Implement simple sound effects (beep, click)
- [ ] Add `beep` shell command for testing
- [ ] Prepare audio interface for alarm (will use in Phase 9's alarm trigger)
- [ ] **Stretch:** Basic Doom sound effect playback (simpler than music)

**Milestone 11:** Audio plays through QEMU, ready for alarm and Doom integration

**Rust concepts practiced:** Producer-consumer patterns, buffer management

---

### Phase 12: Dual-Core & Concurrency (Weeks 31-32)
**Goal:** Dual-core execution in QEMU, advanced synchronization
**New concepts:** SMP simulation, inter-core communication, lock-free queues

**Prerequisites:** Phase 11 complete; basic `Spinlock` and `Mutex` already implemented in Phases 3-4

Dual-core is required for the final system (Core 1 handles USB/audio while Core 0 runs apps).

#### Week 31: Advanced Synchronization
- [ ] **Review:** Atomics and memory ordering (`Ordering::Relaxed`, `Acquire`, `Release`, `SeqCst`)
- [ ] **Note:** Basic `Spinlock` and `Mutex<T>` already done in Phases 3-4
- [ ] Implement `SpinlockGuard` with `Drop` if not already done (RAII pattern)
- [ ] **Learn:** Interior mutability, `UnsafeCell` (deeper understanding)
- [ ] Implement `SpscQueue<T, N>` (single-producer single-consumer ring buffer)
  - Lock-free using atomics for head/tail
  - Will be used for inter-core communication
- [ ] Test: verify SPSC queue works in single-core scenario first

#### Week 32: Dual-Core in QEMU
- [ ] **Learn:** QEMU SMP options (`-smp 2`)
- [ ] **Learn:** RISC-V SMP boot — QEMU virt uses spin-table or device tree method
  - Secondary cores spin waiting for entry address to be written
  - Need to research exact mechanism for QEMU virt RISC-V
- [ ] Configure QEMU for 2 cores
- [ ] Implement core ID detection (read `mhartid` CSR)
- [ ] Implement Core 1 startup (write entry address, signal core to start)
- [ ] Test: Core 0 produces to SPSC queue, Core 1 consumes, verify FIFO order
- [ ] Test: both cores printing interleaved (with mutex protection)

**Milestone 12:** Dual-core running in QEMU with working synchronization

**Rust concepts practiced:** Atomics (advanced), `UnsafeCell`, RAII guards, const generics, lock-free data structures

---

### Phase 12A: Preemptive Scheduler (Weeks 33-34)
**Goal:** Round-robin preemptive multitasking on Core 0
**New concepts:** Context switching, thread control blocks, timer-driven preemption

**Prerequisites:** Phase 12 complete (dual-core working, SPSC queues)

This phase adds true preemptive multitasking, allowing multiple threads to run concurrently on Core 0 while Core 1 continues dedicated I/O handling (maintaining the AMP design).

#### Memory Budget (520KB SRAM)

The scheduler adds thread stacks to the existing memory layout:

| Component | Size | Notes |
|-----------|------|-------|
| Kernel/interrupt stack | 4KB | Shared for trap handling |
| Core 1 I/O stack | 4KB | Unchanged |
| Kernel heap | 64KB | Unchanged |
| IPC region | 4KB | Unchanged |
| **Thread stacks** | 40KB | Static pool (see below) |
| TCBs + scheduler | 2KB | Static allocation |
| **Total** | ~118KB | Leaves 400KB for app data |

**Thread Stack Allocation (static pool of 8 slots):**

| Slot | Stack | Purpose |
|------|-------|---------|
| 0 | 2KB | Idle thread |
| 1 | 2KB | Shell |
| 2 | 16KB | Doom (C code, deep recursion) |
| 3 | 4KB | Editor |
| 4 | 2KB | Alarm |
| 5-7 | 4KB each | Reserved/future |

Doom needs the largest stack due to C calling conventions and recursion in the renderer.

#### Week 33: Scheduler Infrastructure

- [ ] **Learn:** Context switching theory (save/restore CPU state)
- [ ] **Learn:** RISC-V register set for context save (x1-x31, mepc, mstatus)
- [ ] Update linker script with thread stack pool (static allocation)
- [ ] Implement interrupt-safe `SpinLock`:
  - Must disable interrupts while held to prevent deadlock
  - Save `mstatus.MIE` on lock, restore on unlock
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
- [ ] Define `SavedContext` struct (31 registers + mepc + mstatus = 132 bytes)
- [ ] Define `TCB` (Task Control Block) struct:
  ```rust
  struct TCB {
      context: SavedContext,
      stack_base: *mut u8,
      stack_size: usize,
      state: ThreadState,  // Free, Ready, Running, Blocked
      id: u8,
      wake_at: Option<u64>,  // For sleep support
  }
  ```
- [ ] Define `Scheduler` struct with static array of 8 TCBs
- [ ] Implement `find_next_ready()` - round-robin scan for next Ready thread
- [ ] Test TCB creation and state transitions (unit tests)

#### Week 34: Context Switch & Preemption

- [ ] **Learn:** How to switch stacks safely in assembly
- [ ] Implement context switch assembly (~50 lines):
  - On trap entry: switch to kernel stack, save user registers to current TCB
  - On trap exit: restore registers from new TCB, switch to thread stack
  - Key insight: `sp` must be saved/restored explicitly
- [ ] Modify trap handler for timer preemption:
  ```rust
  fn trap_handler(regs: &mut SavedContext, mcause: u32) {
      if mcause == MACHINE_TIMER_INTERRUPT {
          timer::acknowledge();
          let mut sched = SCHEDULER.lock();
          sched.tick_count += 1;

          if sched.tick_count >= TIME_SLICE_MS {  // 10ms slice
              sched.tick_count = 0;
              // Save current, pick next, restore
              sched.context_switch(regs);
          }
      }
  }
  ```
- [ ] Implement thread API:
  - `spawn(entry: fn(), stack_size: usize) -> Option<ThreadId>`
  - `yield_now()` - voluntary yield (triggers reschedule)
  - `exit() -> !` - terminate current thread, mark slot Free
  - `current() -> ThreadId` - get current thread ID
- [ ] Implement idle thread (runs `wfi` in loop when nothing ready)
- [ ] Implement `sleep_ms(ms: u32)` using `wake_at` field:
  - Set `wake_at = current_ticks + ms`
  - Mark thread Blocked
  - Scheduler checks Blocked threads each tick, wakes if `wake_at` reached
- [ ] Test: spawn two threads incrementing separate counters, verify both progress
- [ ] Test: spawn threads printing interleaved output, verify round-robin fairness
- [ ] Test: `sleep_ms` accuracy with multiple sleeping threads
- [ ] Test: thread exit and slot reuse
- [ ] Stress test: fill all 8 slots, verify no corruption

#### Context Switch Flow

```
Timer interrupt fires (every 1ms)
        │
        ▼
┌─────────────────────────────┐
│ _trap_vector (asm)          │
│ - Switch to kernel stack    │
│ - Save all regs to TCB      │
│ - Call rust trap_handler    │
└─────────────────────────────┘
        │
        ▼
┌─────────────────────────────┐
│ trap_handler (Rust)         │
│ - Identify: timer interrupt │
│ - Increment tick_count      │
│ - If tick_count >= 10:      │
│   - current.state = Ready   │
│   - Find next Ready thread  │
│   - next.state = Running    │
│   - Reset tick_count        │
└─────────────────────────────┘
        │
        ▼
┌─────────────────────────────┐
│ _trap_vector (asm)          │
│ - Restore regs from new TCB │
│ - Switch to thread stack    │
│ - mret                      │
└─────────────────────────────┘
```

#### Scheduler API Summary

```rust
// src/kernel/sched.rs

const MAX_THREADS: usize = 8;
const TIME_SLICE_MS: u32 = 10;

/// Spawn a new thread. Returns None if no slots available.
pub fn spawn(entry: fn(), stack_size: usize) -> Option<ThreadId>;

/// Voluntarily yield CPU to another thread.
pub fn yield_now();

/// Terminate the current thread. Never returns.
pub fn exit() -> !;

/// Sleep for at least `ms` milliseconds.
pub fn sleep_ms(ms: u32);

/// Get the current thread's ID.
pub fn current() -> ThreadId;
```

**Milestone 12A:** Preemptive round-robin scheduler with 8 thread slots, timer-driven context switching, sleep support

**Rust concepts introduced:** Low-level context switching, interrupt-safe locks, thread lifecycle management

---

## 🎯 MVP Milestone: QEMU Feature-Complete (Week 34)

At the end of Phase 12A, the QEMU build is **feature-complete**:

| Feature | Status |
|---------|--------|
| Boot & UART | ✅ Working |
| Memory allocation | ✅ Working |
| Timer & sleep | ✅ Working |
| Graphical display | ✅ Working |
| Keyboard input | ✅ Working |
| Shell with commands | ✅ Working |
| FAT16 filesystem | ✅ Working |
| Text editor | ✅ Working |
| Alarm clock | ✅ Working |
| Doom | ✅ Working |
| Audio | ✅ Working |
| Dual-core | ✅ Working |
| Preemptive scheduler | ✅ Working |

**This is a complete, usable operating system running in QEMU.**

You could stop here and have accomplished something significant. Everything beyond this is porting to real hardware.

---

## Hardware Phases: Two Paths

From here, there are two hardware paths:

### Path A: MVP Hardware (No Modifications)
**Goal:** Run EASE on stock Pico 2 with minimal external hardware
- Uses Pico 2's built-in 520KB SRAM only (no PSRAM)
- Uses UART for I/O (no e-ink display)
- Uses USB serial keyboard input
- Doom runs with reduced WAD or from flash
- **Weeks 35-38**

### Path B: Full Hardware (With Modifications)
**Goal:** Complete EASE device as originally planned
- PSRAM soldered for 8MB extra RAM
- 10.3" e-ink display
- USB Host keyboard
- PWM audio with speaker
- Battery powered
- **Weeks 35-48**

You can complete Path A first (quick win on real hardware), then continue to Path B.

---

### Phase 13: MVP Hardware - Stock Pico 2 (Weeks 35-38)
**Goal:** EASE running on unmodified Pico 2
**New concepts:** Real hardware debugging, RP2350 peripherals, flash constraints

This gives you a working system on real hardware without soldering PSRAM.

#### Week 35: First Hardware Boot
- [ ] Set up Raspberry Pi Debug Probe (SWD connection)
- [ ] **Learn:** probe-rs tool for flashing and debugging
- [ ] Create RP2350 linker script (different memory map from QEMU)
- [ ] Flash minimal program that blinks on-board LED
- [ ] Verify GDB debugging works over SWD
- [ ] **Learn:** RP2350 GPIO registers and configuration

#### Week 36: UART on Real Hardware
- [ ] **Learn:** RP2350 UART peripheral registers
- [ ] Add `rp2350` feature to `Cargo.toml` and make `qemu` the default:
  ```toml
  [features]
  default = ["qemu"]
  qemu = []
  rp2350 = []
  ```
- [ ] Implement `Uart` for RP2350 (different from QEMU's memory-mapped UART)
- [ ] Get "Hello from EASE!" printing to debug probe's UART
- [ ] Implement `Timer` for RP2350 (different from QEMU's CLINT)
- [ ] Verify `sleep_ms()` works on real hardware
- [ ] Use `#[cfg(feature = "rp2350")]` to select HAL implementation

#### Week 37: SD Card & Filesystem on Hardware
- [ ] **Learn:** RP2350 SPI peripheral
- [ ] Connect SD card breakout to Pico 2 (SPI pins)
- [ ] Implement SPI driver for RP2350
- [ ] Test SD card initialization and block read
- [ ] Verify FAT16 filesystem works on real SD card
- [ ] Test `ls`, `cat` commands via UART

#### Week 38: MVP System Complete
- [ ] Port dual-core startup to RP2350 (different from QEMU spin-table)
- [ ] **Learn:** RP2350 multicore launch sequence (SIO FIFO)
- [ ] Test shell interaction via UART (type commands, see responses)
- [ ] Test text editor (limited without proper display, but functional)
- [ ] Test alarm clock (prints to UART when triggered)
- [ ] **Doom:** Embed small WAD or skip (520KB SRAM is tight)
- [ ] Document what works and what needs PSRAM/display

**🎯 MVP Hardware Milestone:** EASE shell running on stock Pico 2 via UART

This proves your OS works on real hardware. Everything from here adds peripherals.

---

### Phase 14: PSRAM & Full Memory (Weeks 39-40)
**Goal:** Add 8MB PSRAM for full Doom support
**New concepts:** QSPI interface, external memory initialization

#### Week 39: PSRAM Hardware
- [ ] Solder APS6404L-3SQR PSRAM chip to Pico 2 (dead-bug style to QSPI pins)
- [ ] **Learn:** QSPI protocol and RP2350's QSPI controller
- [ ] Implement PSRAM initialization sequence
- [ ] Test basic read/write to PSRAM
- [ ] Verify data integrity (write patterns, read back)

#### Week 40: PSRAM Integration
- [ ] Integrate PSRAM with allocator (large allocations go to PSRAM)
- [ ] **Remember:** Atomics don't work in PSRAM - keep sync primitives in SRAM
- [ ] Load Doom WAD into PSRAM from SD card
- [ ] Test Doom runs with full WAD
- [ ] Verify memory-intensive operations work

**Milestone 14:** 8MB PSRAM working, Doom runs with full WAD

---

### Phase 15: E-ink Display (Weeks 41-42)
**Goal:** IT8951-based e-ink display working
**New concepts:** IT8951 command protocol, e-ink refresh modes

#### Week 41: IT8951 Driver
- [ ] Connect Waveshare 10.3" display to Pico 2 via SPI
- [ ] **Learn:** IT8951 command set (init, write, refresh)
- [ ] Implement IT8951 initialization sequence
- [ ] Implement write-to-display-buffer command
- [ ] Implement refresh command (full refresh first)
- [ ] Display test pattern (checkerboard or gradient)

#### Week 42: Display Integration
- [ ] Port `Display` trait to IT8951
- [ ] Implement partial refresh for faster updates
- [ ] Test text console on e-ink
- [ ] Test Doom rendering (accept low fps and ghosting)
- [ ] Tune refresh strategy (partial vs full, regions)

**Milestone 15:** E-ink display showing EASE shell and applications

---

### Phase 16: USB Host Keyboard (Weeks 43-44)
**Goal:** USB keyboard input on real hardware
**New concepts:** USB Host protocol, HID class, enumeration

This is one of the more complex hardware tasks.

#### Week 43: USB Host Basics
- [ ] **Learn:** USB Host vs Device mode
- [ ] **Learn:** USB enumeration process (descriptors, configuration)
- [ ] Configure RP2350 USB controller for Host mode
- [ ] Implement USB reset and enumeration
- [ ] Detect when keyboard is plugged in
- [ ] Parse device descriptor, find HID interface

#### Week 44: HID Keyboard Driver
- [ ] **Learn:** HID report descriptor format
- [ ] Implement HID report polling (interrupt transfers)
- [ ] Parse keyboard HID reports (modifier keys, key codes)
- [ ] Convert USB HID key codes to your `KeyEvent` format
- [ ] Implement key repeat (software timer)
- [ ] Test full shell interaction with USB keyboard

**Milestone 16:** USB keyboard working, can type in shell

---

### Phase 17: Audio Hardware (Weeks 45-46)
**Goal:** PWM audio output with speaker
**New concepts:** PWM for audio, DMA, analog filtering

#### Week 45: PWM Audio Driver
- [ ] **Learn:** Using PWM for audio (high-frequency PWM + low-pass filter = analog)
- [ ] Configure RP2350 PWM for audio-rate output (~44.1kHz or ~22kHz)
- [ ] Implement DMA transfer to PWM (continuous sample streaming)
- [ ] Build RC low-pass filter circuit (resistor + capacitor)
- [ ] Connect filter output to small speaker or 3.5mm jack
- [ ] Test with simple tones - verify audio quality

#### Week 46: Audio Integration & Polish
- [ ] Port `Audio` trait to PWM output
- [ ] Test alarm beep on real hardware
- [ ] Test Doom sound effects
- [ ] Implement audio mixing (if playing multiple sounds)
- [ ] Final volume tuning

**Milestone 17:** Audio working through speaker

---

### Phase 18: Power & Final Integration (Weeks 47-48)
**Goal:** Battery-powered portable device
**New concepts:** Battery management, power optimization

#### Week 47: Battery Integration
- [ ] Connect LiPo battery via TP4056 charging module
- [ ] **Learn:** Pico 2 power input requirements
- [ ] Implement battery voltage reading (ADC on VSYS)
- [ ] Implement low-battery warning (display icon or beep)
- [ ] Test battery life under different loads

#### Week 48: Final Polish
- [ ] Full system integration testing
- [ ] Test all applications on final hardware
- [ ] Fix any remaining bugs
- [ ] Code cleanup and documentation
- [ ] Write brief user guide

**Final Milestone:** Portable EASE device complete

---

## Bill of Materials

| Item | Quantity | Approx. Cost | Notes |
|------|----------|--------------|-------|
| Raspberry Pi Pico 2 | 1 | $5 | Standard board |
| APS6404L-3SQR PSRAM | 1 | $2 | 8MB, SOP-8 |
| Waveshare 10.3" E-ink HAT | 1 | $90 | IT8951 controller included |
| MicroSD card breakout | 1 | $5 | Adafruit or similar |
| MicroSD card (4GB+) | 1 | $5 | FAT16 formatted |
| Raspberry Pi Debug Probe | 1 | $12 | For development |
| USB OTG adapter/hub | 1 | $5 | For keyboard |
| Low-profile USB keyboard | 1 | $30-80 | Apple Magic style |
| LiPo battery (5mm, ~1500mAh) | 1 | $10 | Pouch cell |
| TP4056 charging module | 1 | $1 | Or integrated solution |
| Small speaker (8Ω) | 1 | $2 | For audio |
| RC filter components | 1 | $1 | Resistor + capacitor |
| Wires, connectors, misc | - | $10 | - |
| **Total** | | **~$180** | |

---

## Rust Idioms to Practice

Since this is a learning project emphasizing idiomatic Rust:

### No `static mut` — Safe Alternatives

**Rule:** Never use `static mut`. Always use safe synchronization primitives.

| Instead of... | Use... |
|---------------|--------|
| `static mut COUNTER: u64 = 0` | `static COUNTER: AtomicU64 = AtomicU64::new(0)` |
| `static mut DATA: Option<T> = None` | `static DATA: OnceCell<T> = OnceCell::new()` |
| `static mut SHARED: T = ...` | `static SHARED: Mutex<T> = Mutex::new(...)` |
| `static mut BUFFER: [u8; N] = ...` | `static BUFFER: Mutex<[u8; N]> = ...` |

**Patterns for global state:**

```rust
// For simple counters (interrupt-safe)
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

// For one-time initialization
static ALLOCATOR: OnceCell<BumpAllocator> = OnceCell::new();

// For mutable shared state
static CONSOLE: Mutex<Console> = Mutex::new(Console::new());

// For interrupt handlers (lock-free)
static KEY_QUEUE: SpscQueue<KeyEvent, 16> = SpscQueue::new();
```

### Interrupt-Safe Patterns

| Pattern | When to use |
|---------|-------------|
| `AtomicT` | Simple values accessed from interrupt handlers |
| `Mutex<T>` | Complex data, disable interrupts during access |
| `SpscQueue<T>` | Producer-consumer between interrupt and main code |
| `OnceCell<T>` | One-time initialization at startup |

### Ownership & Borrowing
- Proper lifetime annotations in drivers
- Zero-copy buffer handling where possible
- RAII for locks (guard patterns)

### Type System
- Newtype pattern for handles (`FileHandle(u32)`)
- Builder pattern for configuration
- Typestate pattern for peripheral initialization

### Error Handling
- Custom error types with `thiserror`-style (manual impl)
- `Result<T, E>` everywhere, no panics in normal operation
- `?` operator for propagation

### Traits
- HAL traits for platform abstraction
- `Iterator` for directory listing
- `Read`/`Write` traits for I/O
- `Drop` for resource cleanup

### Generics & Const Generics
- `SpscQueue<T, const N: usize>`
- `HwSpinlock<const N: u32>`
- Generic drivers over SPI/I2C traits

### Unsafe
- Clearly documented unsafe blocks
- Minimal unsafe surface area
- Safe wrappers around hardware access
- **Never** `static mut`

### Modules & Visibility
- `pub(crate)` for internal APIs
- Clear module hierarchy
- Prelude modules for common imports

---

## Testing Strategy

### Two-Tier Testing Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    cargo test --bin ease                     │
│  - Custom test framework on QEMU                            │
│  - Tests UART, interrupts, memory, boot                     │
│  - Prints results, exits with status code                   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                      cargo test --lib                        │
│  - Standard #[test] functions in src/lib.rs                 │
│  - Pure logic tests (parsers, data structures)              │
│  - Runs on development machine with std                     │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                       CI Pipeline                            │
│  - Runs both test suites via ci.sh                          │
│  - QEMU exit code determines pass/fail                      │
└─────────────────────────────────────────────────────────────┘
```

**Note:** Integration tests in `tests/` directory are not currently supported. `cargo test --tests` attempts to compile `main.rs` which contains RISC-V assembly that fails on the host. This would require making the binary target-conditional with `#[cfg(target_arch = "riscv32")]`.

### Test Commands

```bash
# Run QEMU tests (hardware-dependent code)
cargo test --bin ease

# Run host tests (pure logic in lib.rs)
cargo test --lib
```

### Test Files

| File/Directory | Purpose |
|----------------|---------|
| `src/main.rs` | QEMU tests and benchmarks (`#[test_case]`) |
| `src/lib.rs` | Host tests and pure logic (`#[test]`) |
| `src/io.rs` | I/O abstraction with test capture |
| `src/bench.rs` | Benchmarking via RISC-V cycle counter |
| `src/qemu.rs` | QEMU exit mechanism (sifive_test device) |
| `rust-toolchain.toml` | Pins nightly toolchain for `custom_test_frameworks` |
| `tests/` | Future: integration tests (blocked, see note above) |

### QEMU Testing
- Uses custom test framework (`#![feature(custom_test_frameworks)]`)
- Tests run on QEMU virt machine with automatic exit
- Exit code 0 = all tests pass, exit code 1 = test failure
- Panic handler calls `qemu::exit_failure()` on test panic

### Host Testing
- Standard `#[test]` in `tests/` directory for pure logic (parsers, algorithms)
- Runs on development machine (automatically uses std)
- Tests import from `src/lib.rs` which exposes hardware-independent modules
- **Supports `#[should_panic]`** for testing expected panics

### Panic Testing Limitation

**QEMU tests cannot use `#[should_panic]`** — this attribute is part of the built-in test harness and not available with `custom_test_frameworks`. When a QEMU test panics:
1. The panic handler calls `qemu::exit_failure()`
2. QEMU terminates immediately
3. The entire test run fails

**Workarounds:**
- **Preferred:** Test panic conditions in host tests (`tests/`) using `#[should_panic]`
- **Alternative:** Test that invalid inputs return `Result::Err` or `Option::None` instead of panicking
- **Future:** Implement `setjmp`/`longjmp` in RISC-V assembly to enable non-local return from panic handler

**Example — testing OOM in host tests:**
```rust
// In tests/allocator.rs (runs on host with std)
#[test]
#[should_panic(expected = "out of memory")]
fn test_alloc_oom() {
    let mut alloc = BumpAllocator::new(16);  // Tiny heap
    alloc.alloc(1024);  // Should panic
}
```

### I/O Abstraction for Testing

The `src/io.rs` module provides trait-based I/O that enables output verification:

```rust
// Writer trait - implemented by UartWriter, future MockWriter, etc.
pub trait Writer {
    fn write_byte(&mut self, byte: u8);
    fn write_str(&mut self, s: &str) { ... }
}

// In test mode, output is captured to a static buffer
#[cfg(test)]
pub mod test_io {
    pub fn clear();                    // Reset capture buffer
    pub fn output() -> &'static str;   // Get captured output
    pub fn contains(s: &str) -> bool;  // Check substring
    pub fn equals(s: &str) -> bool;    // Check exact match
}
```

**Example test:**
```rust
#[test_case]
fn test_println_output() {
    test_io::clear();
    println!("hello");
    assert!(test_io::equals("hello\n"));
}
```

**Future expansion:** Add `Reader` trait and input buffer for testing interactive features (shell, editor).

### Benchmarking

The `src/bench.rs` module provides cycle-accurate benchmarking using RISC-V's cycle counter CSR:

```rust
// Read cycle counter (works without timer interrupt setup)
fn cycles() -> u64;

// Measure cycles for a single operation
pub fn measure<F: FnOnce()>(f: F) -> u64;

// Run and print a single benchmark
pub fn run<F: FnOnce()>(name: &str, f: F);

// Run multiple iterations and print average
pub fn run_avg<F: Fn()>(name: &str, iterations: u32, f: F);
```

**Example benchmark:**
```rust
#[test_case]
fn bench_my_operation() {
    bench::run("operation name", || {
        // code to measure
    });
}
```

**Sample results (QEMU):**
| Operation | Cycles |
|-----------|--------|
| `print!("hello")` | ~54,500 |
| `println!("hello")` | ~58,000 |
| `println!` with formatting | ~57,300 |
| `println!` 50 chars | ~146,000 |

Benchmarks run as part of `cargo test --bin ease` and report cycle counts for each measured operation.

### Regression Detection

Benchmarks include regression checks that fail if performance degrades beyond a threshold:

```rust
// In src/main.rs - baselines module
mod baselines {
    pub const PRINT_HELLO: u64 = 38_000;      // Update when intentionally changed
    pub const PRINTLN_HELLO: u64 = 32_000;
    // ...
}

// Regression test - fails if cycles exceed baseline + 20%
#[test_case]
fn regression_println_hello() {
    test_io::clear();
    bench::check("println!(hello)", baselines::PRINTLN_HELLO, ITERATIONS, || {
        println!("hello");
    });
}
```

**Workflow before milestone commits:**
1. Run `cargo test --bin ease`
2. Regression checks run automatically
3. If any check fails with "REGRESSION", investigate before committing
4. If performance intentionally changed, update baseline constants

**Output format:**
```
OK println!(hello): 25000 cycles (baseline: 32000, -21%)     # Pass
REGRESSION print!(x): 50000 cycles (baseline: 30000, +66%)   # Fail
```

### Hardware Testing
- Debug probe for step-through debugging
- UART logging at multiple verbosity levels
- LED blink codes for boot failures
- Systematic peripheral bring-up

### Doom Testing
- First test on QEMU (stub display to file)
- Then test rendering path on hardware
- Profile frame timing
- Tune refresh strategy

---

## Key Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| E-ink too slow for Doom | High | Accept low fps; optimize partial refresh; consider "slideshow" mode |
| USB Host complexity | Medium | Start with simpler PS/2 if stuck; use reference implementations |
| PSRAM soldering failure | Medium | Buy extra chips; consider pre-built board as backup |
| Atomics limitation in PSRAM | High | Keep all sync structures in SRAM; documented earlier |
| FAT16 write complexity | Medium | Implement read-only first; add write later |
| 8 hours/week timeline | Medium | Flexible milestone dates; MVP first |

---

## Resources

### Documentation
- [RP2350 Datasheet](https://datasheets.raspberrypi.com/rp2350/rp2350-datasheet.pdf)
- [Pico SDK Documentation](https://www.raspberrypi.com/documentation/pico-sdk/)
- [IT8951 Datasheet](https://www.waveshare.com/wiki/10.3inch_e-Paper_HAT)
- [doomgeneric](https://github.com/ozkl/doomgeneric)

### Rust Embedded
- [Embedded Rust Book](https://docs.rust-embedded.org/book/)
- [riscv-rt crate](https://github.com/rust-embedded/riscv-rt)
- [rp235x-hal](https://github.com/rp-rs/rp-hal) (reference only, implementing from scratch)

### Reference Code
- [Mr-Bossman pi-pico2-linux](https://github.com/Mr-Bossman/pi-pico2-linux) - Device tree, boot reference
- [fbDOOM](https://github.com/maximevince/fbDOOM) - Minimal Doom port

---

## Quick Start Commands

```bash
# Clone and setup
git clone https://github.com/yourusername/ease
cd ease
rustup target add riscv32imac-unknown-none-elf

# Build for QEMU (current default, no feature flag needed)
cargo build --release

# Run on QEMU
qemu-system-riscv32 -M virt -m 128M -nographic \
    -bios none -kernel target/riscv32imac-unknown-none-elf/release/ease

# Build for RP2350 (available after Phase 13)
cargo build --release --features rp2350

# Flash to Pico 2 (via debug probe)
probe-rs run --chip RP2350 target/riscv32imac-unknown-none-elf/release/ease

# Debug
probe-rs gdb --chip RP2350 target/riscv32imac-unknown-none-elf/release/ease
```

**Note:** The `rp2350` feature is added in Phase 13 when hardware support begins. Until then, only QEMU is supported.

---

## Success Criteria

1. **Boot:** System boots to shell prompt in < 5 seconds
2. **Shell:** Can navigate filesystem, run commands
3. **Doom:** Playable (recognizable gameplay, responds to input)
4. **Editor:** Can create, edit, save text files
5. **Alarm:** Can set and trigger alarms
6. **Portable:** Runs on battery for 2+ hours
7. **Robust:** No crashes during normal use

---

*Plan created: January 2026*
*Target completion: ~12 months (48 weeks at 8 hours/week)*
*Total estimated effort: ~385 hours*

## Milestone Summary

| Milestone | Week | Description |
|-----------|------|-------------|
| M0 | 0 | GitHub repo, toolchain, rustdoc ready |
| M1 | 2 | Hello World on QEMU |
| M2 | 4 | Proper boot, panic handler |
| M3 | 6 | Memory allocation working |
| M4 | 8 | Timer interrupt working |
| M5 | 11 | Display console with scrolling |
| M6 | 14 | Interactive shell |
| M7 | 18 | FAT16 filesystem |
| M8 | 21 | Text editor |
| M9 | 23 | Alarm clock |
| M10 | 28 | Doom running |
| M11 | 30 | Audio working |
| M12 | 32 | Dual-core working |
| M12A | 34 | Preemptive scheduler working |
| **🎯 QEMU MVP** | **34** | **Feature-complete in emulation** |
| M13 | 38 | Stock Pico 2 via UART |
| **🎯 HW MVP** | **38** | **Running on real hardware (no mods)** |
| M14 | 40 | PSRAM integrated |
| M15 | 42 | E-ink display working |
| M16 | 44 | USB keyboard working |
| M17 | 46 | Audio hardware working |
| **🎯 FINAL** | **48** | **Portable device complete** |

## Phase Summary

| Phase | Weeks | Focus | Key Concepts |
|-------|-------|-------|--------------|
| 0 | 0 | Project Setup | GitHub, CI, rustdoc, toolchain, Rust 2024 |
| 1 | 1-2 | Hello QEMU | `no_std`, linker scripts, UART |
| 2 | 3-4 | Kernel Foundations | Boot assembly, panic, modules |
| 3 | 5-6 | Memory Management | Allocators, `GlobalAlloc`, Mutex |
| 4 | 7-8 | Time & Interrupts | Trap handling, timer IRQ, no `static mut` |
| 5 | 9-11 | Display | QEMU framebuffer, fonts, traits |
| 6 | 12-14 | Input & Shell | Parsing, commands, state |
| 7 | 15-18 | Filesystem | Block devices, FAT16, VFS |
| 8 | 19-21 | Text Editor | Gap buffer, file I/O |
| 9 | 22-23 | Alarm Clock | Time formatting, persistence |
| 10 | 24-28 | Doom | FFI, libc, C interop |
| 11 | 29-30 | Audio | QEMU audio, samples, mixing |
| 12 | 31-32 | Dual-Core | Atomics, spinlocks, SMP |
| 12A | 33-34 | Preemptive Scheduler | Context switch, TCB, round-robin |
| 13 | 35-38 | MVP Hardware | Stock Pico 2, UART shell |
| 14 | 39-40 | PSRAM | QSPI, external memory |
| 15 | 41-42 | E-ink Display | IT8951, partial refresh |
| 16 | 43-44 | USB Keyboard | USB Host, HID |
| 17 | 45-46 | Audio Hardware | PWM, DMA, filtering |
| 18 | 47-48 | Power & Polish | Battery, integration |

## Complexity Curve

```
Complexity
    │                                                      ┌── Phase 18: Polish
    │                                                ┌─────┘
    │                                          ┌─────┘ Phase 15-17: Display/USB/Audio HW
    │                                    ┌─────┘
    │                              ┌─────┘ Phase 14: PSRAM
    │                        ┌─────┘ Phase 13: First real hardware
    │                  ┌─────┘
    │            ┌─────┘ Phase 10-12A: Doom, Audio, Dual-core, Scheduler
    │      ┌─────┘
    │   ┌──┘ Phases 5-9: Display, Shell, FS, Apps
    │───┘ Phases 1-4: Foundations
    └──────────────────────────────────────────────────────────────────── Time
     W1    W8    W16    W24    W34    W38    W44    W48
                              │       │
                         QEMU MVP  HW MVP
```

## Decision Points

After each MVP, you can decide whether to continue:

1. **After QEMU MVP (Week 34):** You have a complete OS in emulation. Continue to hardware?
2. **After HW MVP (Week 38):** You have EASE running on real Pico 2 via UART. Add display/keyboard/audio?
3. **After each hardware phase:** Each peripheral is independent. Skip or reorder as desired.
