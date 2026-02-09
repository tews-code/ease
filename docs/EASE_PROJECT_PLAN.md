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
- **Rust 2024 Edition** — `#[unsafe(no_mangle)]`, `#[unsafe(link_section)]`, etc.

### Educational Objectives & Collaboration Model

This is an educational project. The primary goal is learning through hands-on implementation.

**Human Role:** Write all code, make architectural decisions, debug with guidance, learn by doing.

**Claude Role:** Code review, run tests, help debug, answer questions, provide reference examples (not solutions), validate against plan. Focus on embedded/hardware topics over basic Rust.

**Principles:**
1. Claude will not write production code for the project
2. Claude will explain *why* something is wrong, not just *what* to change
3. When stuck, Claude provides hints before solutions
4. Learning takes priority over speed of completion

**Allowed Third-Party Crates:** `cc` (for compiling Doom's C code in build.rs)

### Related Documentation
- `docs/COMPLETED_PHASES.md` — Detailed checklists for phases 0-6
- `docs/ARCHITECTURE.md` — Implementation details of completed subsystems
- `docs/DESIGN_NOTES.md` — Reference designs for future phases (syscalls, scheduler, sync)
- `docs/STYLE_GUIDE.md` — Rust idioms and patterns used in the project
- `docs/uart.md` — UART protocol and QEMU 16550 documentation

---

## Hardware Specification

| Component | Choice | Notes |
|-----------|--------|-------|
| MCU | RP2350 (Hazard3 RISC-V cores) | Dual-core RV32IMAC @ 150MHz |
| Board | Raspberry Pi Pico 2 | Standard board + modifications |
| PSRAM | APS6404L-3SQR (8MB) | Dead-bug soldered to QSPI pins |
| SRAM | 520KB (built-in) | For stack, heap, atomics |
| Display | Waveshare 10.3" E-Ink HAT | 1872x1404, IT8951, SPI |
| Keyboard | USB low-profile keyboard | USB Host mode |
| Audio | PWM + RC low-pass filter | Single GPIO pin, 8ohm speaker |
| Storage | MicroSD card (SPI) | FAT16 filesystem |
| Battery | LiPo pouch cell | ~1500mAh, TP4056 charging |
| Debug | Raspberry Pi Debug Probe | SWD + UART |

---

## Software Architecture

### Memory Layout (520KB SRAM + 8MB PSRAM)

```
SRAM (520KB) - 0x20000000
├── Kernel Stack (4KB)
├── Core 1 I/O Stack (4KB)
├── Kernel Heap (64KB)
├── Shared IPC Region (4KB)
├── Thread Stack Pool (40KB, 8 slots)
├── TCBs + Scheduler State (2KB)
├── Reserved (~398KB) — display buffer, working memory
└── End: 0x20082000

PSRAM (8MB) - 0x11000000
├── Doom WAD data (~4MB)
├── Document buffers, large heap allocations
└── Framebuffer (if needed)

Flash (4MB) - 0x10000000
├── Bootloader, kernel binary, read-only assets
```

### Dual-Core Design (AMP)

- **Core 0 (Application Core):** Kernel (Machine mode) + user applications (User mode)
- **Core 1 (I/O Core):** Dedicated interrupt handling — timer, USB, SPI, UART (Machine mode only)

| Principle | Implementation |
|-----------|----------------|
| Interrupt-driven I/O | Core 1 handles all hardware interrupts |
| Bounded latency | Audio/USB interrupts on Core 1 have deterministic response |
| User/kernel separation | Apps on Core 0 run in User mode, syscall into kernel |
| Lock-free IPC | SPSC queues between cores avoid blocking |

See `docs/DESIGN_NOTES.md` for syscall interface, sync primitives, and scheduler design.

---

## Completed Phases (0-6)

| Phase | Milestone | Key Result |
|-------|-----------|------------|
| 0 | Project Setup | Repo, toolchain, CI, rustdoc |
| 1 | Hello QEMU | UART output, test/bench infrastructure |
| 2 | Kernel Foundations | Boot sequence, panic handler, modules |
| 3 | Memory Management | Bump allocator, GlobalAlloc, Spinlock |
| 4 | Time & Interrupts | Trap handler, CLINT timer, sleep_ms |
| 5 | Display Output | QEMU ramfb framebuffer, clear/set_pixel |
| 6 | Input & Shell | Keyboard input, text console, interactive shell |

See `docs/COMPLETED_PHASES.md` for detailed checklists.

---

## Phase 7: Storage & Filesystem (Weeks 15-18)
**Goal:** Read and write files on FAT16 filesystem
**New concepts:** Block devices, filesystem structures, file handles, buffering
**Prerequisites:** Phase 6 complete (shell for testing commands)

### Week 15: Block Device Abstraction
- [ ] Create `BlockDevice` trait: `read_block(n, &mut buf)`, `write_block(n, &buf)`
- [ ] **Choose interface:** pflash (simplest) vs virtio-blk (more realistic). Recommend pflash for QEMU.
- [ ] Use QEMU's `-drive` option to attach a disk image
- [ ] Create test disk image: `dd if=/dev/zero of=disk.img bs=512 count=2048`
- [ ] Test: read block 0, print first 16 bytes as hex

### Week 16: FAT16 Basics
- [ ] Parse boot sector: bytes per sector, sectors per cluster, FAT count, etc.
- [ ] Locate FAT table and root directory
- [ ] Read and print FAT table entries
- [ ] Create `Fat16` struct holding parsed boot sector info

### Week 17: Reading Files
- [ ] Read root directory, list all files (name, size, first cluster)
- [ ] Implement shell `ls` command
- [ ] Implement cluster chain following
- [ ] Implement `open(filename)` and `read(handle, buf)`
- [ ] Implement shell `cat` command
- [ ] Test: create file on host, read it in EASE

### Week 18: Writing Files & VFS
- [ ] Implement `write(handle, data)` — allocate clusters, update FAT
- [ ] Implement `create(filename)` — create directory entry
- [ ] Create VFS trait: `open()`, `read()`, `write()`, `close()`, `readdir()`
- [ ] Implement `cd`, `hexdump` commands
- [ ] Test: create file in EASE, verify on host

**Milestone 7:** Can list, read, create, and write files on FAT16 filesystem

---

### Phase 8: Text Editor (Weeks 19-21)
**Goal:** Simple text editor with load/save
**New concepts:** Gap buffer, modal interfaces
**Prerequisites:** Phase 7 (filesystem), Phase 6 (keyboard), Phase 5 (display)

- [ ] Implement `GapBuffer`: insert, delete, move cursor, buffer growth
- [ ] Create editor state: buffer, cursor, scroll offset, status line
- [ ] Render buffer to display with cursor and scrolling
- [ ] Handle input: characters, Backspace/Delete, arrows, Home/End
- [ ] Implement Ctrl+S (save), Ctrl+O (load), Ctrl+Q (quit with prompt)
- [ ] Add `edit` shell command

**Milestone 8:** Functional text editor can create, edit, save, and load files

---

### Phase 9: Alarm Clock (Weeks 22-23)
**Goal:** Time display, alarm setting and triggering
**New concepts:** Time formatting, persistent storage
**Prerequisites:** Phase 8, Phase 7 (filesystem for saving alarms)

**Note:** No RTC — time-of-day set manually via `settime` command after each boot.

- [ ] Track time-of-day as offset from boot ticks, format as HH:MM:SS
- [ ] Create alarm clock UI with large time display, update every second
- [ ] Implement `date` and `settime HH:MM:SS` shell commands
- [ ] Store/load alarms in `alarms.txt`
- [ ] Check for triggered alarms each second, flash display or print message
- [ ] Implement `alarm` and `clock` shell commands

**Milestone 9:** Alarm clock can display time, set alarms, and trigger them

---

### Phase 10: Doom Integration (Weeks 24-28)
**Goal:** Run Doom using doomgeneric
**New concepts:** FFI, C interop, libc stubs, build.rs
**Prerequisites:** Phases 3-7 (allocator, timer, display, keyboard, filesystem)

- [ ] Set up `build.rs` with `cc` crate to compile doomgeneric C source
- [ ] Implement libc stubs in `src/libc/`: memcpy, memset, memmove, strlen, strcpy, strcmp, malloc/free, printf
- [ ] Implement doomgeneric interface (6 functions):
  - `DG_Init()`, `DG_GetTicksMs()`, `DG_SleepMs()`, `DG_SetWindowTitle()` (stub)
  - `DG_DrawFrame()` — convert ARGB to grayscale, scale 320x200, dither
  - `DG_GetKey()` — map key events to Doom key codes
- [ ] Load WAD from filesystem (or embed in binary)
- [ ] Add `doom` shell command

**Milestone 10:** Doom runs, displays, and accepts input

---

### Phase 11: Audio System (Weeks 29-30)
**Goal:** Sound output for alarm beeps and Doom
**New concepts:** Digital audio, sample buffers, mixing

**Note:** QEMU virt has limited audio support. Design `Audio` trait now, but real output may only work on hardware (PWM). Use stub for QEMU.

- [ ] Create `Audio` trait: `sample_rate()`, `queue_samples(&[i16])`, `is_available()`
- [ ] Implement `StubAudio` for QEMU (logs instead of playing)
- [ ] Generate simple tones programmatically
- [ ] Create audio buffer/queue system (producer-consumer)
- [ ] Add `beep` shell command
- [ ] Prepare interface for alarm and Doom sound integration

**Milestone 11:** Audio trait designed, stub working, ready for hardware

---

### Phase 12: Dual-Core & Concurrency (Weeks 31-32)
**Goal:** Dual-core execution in QEMU, advanced synchronization
**New concepts:** SMP simulation, inter-core communication, lock-free queues
**Prerequisites:** Phase 11; basic Spinlock from Phase 3

- [ ] Implement `SpscQueue<T, N>` (lock-free ring buffer with atomic head/tail)
- [ ] Configure QEMU for 2 cores (`-smp 2`)
- [ ] Implement core ID detection (`mhartid` CSR) and Core 1 startup
- [ ] Test: Core 0 produces to SPSC queue, Core 1 consumes
- [ ] Test: both cores printing interleaved (with mutex protection)

**Milestone 12:** Dual-core running in QEMU with working synchronization

---

### Phase 12A: Preemptive Scheduler (Weeks 33-34)
**Goal:** Round-robin preemptive multitasking on Core 0
**New concepts:** Context switching, TCBs, timer-driven preemption
**Prerequisites:** Phase 12 (dual-core, SPSC queues)

- [ ] Implement interrupt-disabling `Mutex<T>` (deferred from Phase 4)
- [ ] Define `SavedContext` (31 regs + mepc + mstatus) and `TCB` structs
- [ ] Define `Scheduler` with static array of 8 TCBs, round-robin selection
- [ ] Implement context switch assembly (~50 lines): save/restore to/from TCB
- [ ] Modify trap handler for timer preemption (10ms time slice)
- [ ] Implement thread API: `spawn()`, `yield_now()`, `exit()`, `sleep_ms()`, `current()`
- [ ] Implement idle thread (runs `wfi` in loop)
- [ ] Test: multiple threads, interleaved output, sleep accuracy, slot reuse

See `docs/DESIGN_NOTES.md` for TCB structure, context switch flow, and scheduler API.

**Milestone 12A:** Preemptive round-robin scheduler with 8 thread slots

---

## QEMU MVP (Week 34)

At the end of Phase 12A, the QEMU build is **feature-complete**:

Boot & UART, memory allocation, timer & sleep, graphical display, keyboard input, shell with commands, FAT16 filesystem, text editor, alarm clock, Doom, audio, dual-core, preemptive scheduler.

**This is a complete, usable operating system running in QEMU.** Everything beyond this is porting to real hardware.

---

## Hardware Phases

Two paths available after QEMU MVP:

- **Path A (MVP, Weeks 35-38):** Stock Pico 2 with UART I/O, no modifications
- **Path B (Full, Weeks 35-48):** Complete device with PSRAM, e-ink, USB keyboard, audio, battery

Path A can be completed first as a quick win, then extended with Path B.

### Phase 13: MVP Hardware — Stock Pico 2 (Weeks 35-38)

- [ ] Set up Debug Probe (SWD), flash minimal LED-blink program
- [ ] Create RP2350 linker script, add `rp2350` cargo feature
- [ ] Implement RP2350 UART and Timer drivers
- [ ] Get "Hello from EASE!" printing to debug probe UART
- [ ] Implement SPI driver, connect SD card, verify FAT16
- [ ] Port dual-core startup to RP2350 (SIO FIFO)
- [ ] Test shell, editor, alarm via UART

**Milestone 13:** EASE shell running on stock Pico 2 via UART

### Phase 14: PSRAM (Weeks 39-40)

- [ ] Solder APS6404L-3SQR to Pico 2 QSPI pins
- [ ] Implement PSRAM init sequence, test read/write and data integrity
- [ ] Integrate with allocator (large allocs to PSRAM; atomics stay in SRAM)
- [ ] Load Doom WAD into PSRAM from SD card

**Milestone 14:** 8MB PSRAM working, Doom runs with full WAD

### Phase 15: E-ink Display (Weeks 41-42)

- [ ] Implement IT8951 driver (init, write buffer, refresh)
- [ ] Design `Display` trait covering both ramfb and IT8951
- [ ] Implement partial refresh for faster updates
- [ ] Test console, editor, Doom rendering on e-ink

**Milestone 15:** E-ink display showing EASE shell and applications

### Phase 16: USB Host Keyboard (Weeks 43-44)

- [ ] Configure RP2350 USB controller for Host mode
- [ ] Implement USB enumeration and HID report parsing
- [ ] Convert USB HID key codes to `KeyEvent` format
- [ ] Implement software key repeat

**Milestone 16:** USB keyboard working, can type in shell

### Phase 17: Audio Hardware (Weeks 45-46)

- [ ] Configure RP2350 PWM for audio-rate output
- [ ] Implement DMA transfer to PWM, build RC low-pass filter
- [ ] Port `Audio` trait to PWM output
- [ ] Test alarm beep and Doom sound effects

**Milestone 17:** Audio working through speaker

### Phase 18: Power & Final Integration (Weeks 47-48)

- [ ] Connect LiPo battery via TP4056
- [ ] Implement battery voltage reading (ADC) and low-battery warning
- [ ] Full system integration testing, bug fixes, documentation

**Final Milestone:** Portable EASE device complete

---

## Key Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| E-ink too slow for Doom | High | Accept low fps; optimize partial refresh |
| USB Host complexity | Medium | Start with PS/2 if stuck; use references |
| PSRAM soldering failure | Medium | Buy extra chips; consider pre-built board |
| Atomics limitation in PSRAM | High | Keep all sync structures in SRAM |
| FAT16 write complexity | Medium | Implement read-only first; add write later |

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

## Milestone Summary

| Milestone | Week | Description |
|-----------|------|-------------|
| M0-M6 | 0-14 | Foundations through interactive shell (complete) |
| M7 | 18 | FAT16 filesystem |
| M8 | 21 | Text editor |
| M9 | 23 | Alarm clock |
| M10 | 28 | Doom running |
| M11 | 30 | Audio working |
| M12 | 32 | Dual-core working |
| M12A | 34 | Preemptive scheduler |
| **QEMU MVP** | **34** | **Feature-complete in emulation** |
| M13 | 38 | Stock Pico 2 via UART |
| **HW MVP** | **38** | **Running on real hardware** |
| M14-M18 | 40-48 | PSRAM, e-ink, USB, audio, battery |
| **FINAL** | **48** | **Portable device complete** |

---

*Plan created: January 2026*
*Target completion: ~12 months (48 weeks at 8 hours/week)*
*Total estimated effort: ~385 hours*
