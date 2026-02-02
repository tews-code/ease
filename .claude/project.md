# EASE Project

See [EASE_PROJECT_PLAN.md](../docs/EASE_PROJECT_PLAN.md) for the full project plan.

## Rust 2024 Edition Notes

This project uses **Rust 2024 edition**. Key differences from 2021:
- `#[no_mangle]` requires `#[unsafe(no_mangle)]`
- `#[export_name]` requires `#[unsafe(export_name)]`
- `#[link_section]` requires `#[unsafe(link_section)]`

## Tooling Reminders

- **Target is riscv32**, not riscv64. Use `riscv32-unknown-elf-*` tools or `llvm-objdump`/`cargo objdump`
- Example: `llvm-objdump -d target/riscv32imac-unknown-none-elf/debug/ease`

## Current Status

**Phase 0: Complete** (tagged v0.0.1)
- GitHub repo: github.com/tews-code/ease
- Toolchain: Rust 1.93.0, QEMU 9.2.4, GDB 16.3
- Edition: Rust 2024
- Target: riscv32imac-unknown-none-elf (configured in .cargo/config.toml)

**Phase 1: Complete** (Hello QEMU)
- `#![no_std]` and `#![no_main]`
- Linker script `memory-qemu.x` with proper memory layout
- UART output at 0x10000000, `print!`/`println!` macros
- Custom test framework with QEMU exit mechanism
- I/O abstraction (`src/io.rs`) with test capture
- Benchmarking infrastructure (`src/bench.rs`) using RISC-V cycle counter

**Phase 2: Complete** (Kernel Foundations)
- Proper boot sequence with BSS initialization
- Panic handler with location info
- Module structure (`src/arch/`, `src/kernel/`, `src/hal/`)

**Phase 3: Complete** (Memory Management)
- Bump allocator with `GlobalAlloc`
- `Spinlock` for thread-safe allocation
- `alloc` crate enabled (`Vec`, `String`, `Box` available)

**Phase 4: Complete** (Time & Interrupts)
- RISC-V trap handling (mtvec, mcause, mepc)
- Timer interrupt via CLINT
- `ticks_ms()` and `sleep_ms()` working

**Phase 5: Complete** (Display Output)
- QEMU ramfb framebuffer driver
- fw_cfg protocol implementation

**Phase 6: Next** (Input & Shell)
- Keyboard input via UART RX
- Line editor
- Shell with basic commands

## Working Branch

Use `develop` branch for day-to-day work. Merge to `main` when stable.
