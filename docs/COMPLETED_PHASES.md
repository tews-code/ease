# EASE OS - Completed Phases (Archive)

This file contains the detailed week-by-week checklists for phases that have been completed.
See `EASE_PROJECT_PLAN.md` for the active plan.

---

## Phase 0: Project Setup (Week 0)
**Goal:** Repository, toolchain, and documentation infrastructure ready
**New concepts:** GitHub workflow, rustdoc, cargo workspace

### GitHub Repository Setup
- [x] Create GitHub account (if needed)
- [x] Create new repository: `ease`
- [x] Initialize with README.md and .gitignore (Rust template)
- [x] Clone locally: `git clone git@github.com:<username>/ease.git`
- [x] Set up SSH key for GitHub (if not done)

### Project Structure
- [x] Create Cargo.toml with project metadata
- [x] Add `#![warn(missing_docs)]` to enforce documentation
- [x] Create initial directory structure
- [x] First commit: "Initial project structure"

### Toolchain Setup
- [x] Install Rust via rustup
- [x] Add target: `rustup target add riscv32imac-unknown-none-elf`
- [x] Install QEMU: `apt install qemu-system-riscv32` (or equivalent)
- [x] Install GDB: `apt install gdb-multiarch`
- [x] Verify: `qemu-system-riscv32 --version`

### Documentation Setup
- [x] Configure Cargo.toml for rustdoc
- [x] Test rustdoc: `cargo doc --open`
- [x] Add initial module documentation to lib.rs

### Continuous Integration (CI)
- [x] Create `ci.sh` local CI script (format, clippy, QEMU tests, host tests, docs)
- [x] Run `./ci.sh` before commits to catch issues early
- [ ] (Optional) Add GitHub Actions later if needed

### Git Workflow
- [x] Create develop branch: `git checkout -b develop`
- [ ] Set branch protection on main (optional)
- [x] Practice: make change, commit, push, create PR, merge
- [x] Tag setup complete: `git tag -a v0.0.1 -m "Project setup complete"`

**Milestone 0:** Repository ready, toolchain working, local CI passing, `cargo doc` generates documentation

---

## Phase 1: Hello QEMU (Weeks 1-2)
**Goal:** Get Rust code running on QEMU and printing to console
**New concepts:** `no_std`, linker scripts, QEMU basics, volatile access, inline assembly

### Week 1: Environment & First Boot
- [x] Install Rust, add `riscv32imac-unknown-none-elf` target
- [x] Install QEMU (`qemu-system-riscv32`)
- [x] Create new Cargo project with `#![no_std]`, `#![no_main]`
- [x] Write minimal `_start` function (just infinite loop)
- [x] Create simple linker script for QEMU virt machine
- [x] Boot on QEMU - confirm it doesn't crash
- [x] Add module docs explaining the boot process

### Week 2: UART Output
- [x] Find QEMU virt UART address (0x10000000)
- [x] Set up stack pointer before any function calls
- [x] Write single character to UART using `core::ptr::write_volatile`
- [x] Implement `print!` / `println!` macros using `core::fmt::Write`
- [x] Print "Hello from EASE!"

### Week 2 (continued): Testing Infrastructure
- [x] Set up custom test framework for QEMU (`#![feature(custom_test_frameworks)]`)
- [x] Set up QEMU exit mechanism (`src/qemu.rs` with sifive_test device)
- [x] Verify QEMU tests pass with exit code 0, failures exit with code 1
- [ ] Set up host-side integration tests (`tests/` directory) — blocked, see ARCHITECTURE.md
- [x] Add `rust-toolchain.toml` to pin nightly toolchain
- [x] Create I/O abstraction for testable output (`src/io.rs`)
- [x] Add tests that verify print output content
- [x] Create benchmarking infrastructure (`src/bench.rs`)
- [ ] Add example benchmarks demonstrating `bench::run()` and `bench::run_avg()`

**Milestone 1:** "Hello from EASE!" prints to QEMU console; test and benchmark infrastructure operational

---

## Phase 2: Kernel Foundations (Weeks 3-4)
**Goal:** Proper boot sequence, panic handling, basic project structure
**New concepts:** BSS initialization, panic handlers, modules

### Week 3: Proper Boot Sequence
- [x] Define stack region properly in linker script
- [x] Define BSS section with `__bss_start`, `__bss_end` symbols
- [x] Implement BSS zeroing in early Rust
- [x] Create `src/arch/mod.rs` and `src/arch/boot.rs`
- [x] Move `_start` and boot code to `src/arch/boot.rs`
- [x] Verify BSS works: static variable starts as zero

### Week 4: Panic Handler & Project Structure
- [x] Implement panic handler that prints message and location
- [x] Implement infinite loop after panic (with `wfi` instruction)
- [x] Test panic with `panic!("test panic")`
- [x] Create module structure: `src/arch/`, `src/kernel/`, `src/hal/`
- [x] Move UART code to `src/hal/qemu_virt.rs`

**Milestone 2:** Proper boot sequence, panic handler works, clean project structure

---

## Phase 3: Memory Management (Weeks 5-6)
**Goal:** Dynamic memory allocation working
**New concepts:** Allocators, `GlobalAlloc`, unsafe, heap vs stack, `Spinlock`

### Week 5: Bump Allocator
- [x] Define heap region in linker script (64KB)
- [x] Implement basic `Spinlock` using `AtomicBool`
- [x] Implement bump allocator with `Spinlock<BumpAllocatorInner>`
- [ ] Align QEMU memory layout with RP2350 (deferred to Phase 13)

### Week 6: GlobalAlloc Integration
- [x] Implement `GlobalAlloc` for bump allocator
- [x] Enable `alloc` crate (`extern crate alloc`)
- [x] Test with `Vec`, `String`
- [x] Add out-of-memory handler
- [ ] Stretch: Implement free-list allocator

**Milestone 3:** Can use `Vec`, `String`, `Box` in kernel code; basic `Spinlock` working

---

## Phase 4: Time & Interrupts (Weeks 7-8)
**Goal:** Interrupt-driven timer, proper system tick
**New concepts:** RISC-V trap handling, interrupt-safe state

### Week 7: Trap Handler Foundation
- [x] Write trap vector in assembly (save all registers to stack)
- [x] Implement Rust trap dispatcher (reads mcause, calls appropriate handler)
- [x] Set up `mtvec` to point to trap vector
- [x] Test with illegal instruction exception
- [ ] ~~Implement `Mutex<T>`~~ — deferred to Phase 12A

### Week 8: Timer Interrupt
- [x] Enable machine timer interrupt (`mie.MTIE` bit)
- [x] Handle timer interrupt: update system tick counter, reset mtimecmp
- [x] Use `AtomicUsize` for tick counter
- [x] Implement `ticks_ms()` and `sleep_ms()`
- [x] Test: sleep 1 second, verify timing

**Milestone 4:** Timer interrupt fires regularly, `sleep_ms()` works correctly

---

## Phase 5: Display Output (Weeks 9-11)
**Goal:** Render to QEMU's graphical framebuffer
**New concepts:** Framebuffers, QEMU device configuration

### Week 9: QEMU Framebuffer Setup
- [x] Configure QEMU with `-device ramfb`
- [x] Implement fw_cfg protocol for ramfb registration
- [x] 640x480 resolution, 32-bit XR24 pixel format
- [x] Implement clear screen, set pixel
- [ ] Option B: virtio-gpu (skipped)
- [ ] Option C: Serial/text only (skipped)

### Weeks 10-11: Text & Console (deferred to Phase 6)

**Milestone 5:** QEMU ramfb framebuffer working with clear/set_pixel

---

## Phase 6: Input & Shell (Weeks 12-14)
**Goal:** Interactive command-line shell
**New concepts:** Input handling, parsing, command dispatch, state machines

*Note: Text rendering and console (originally Phase 5 Weeks 10-11) was completed as part of this phase.*

**Milestone 6:** Interactive shell with basic built-in commands
