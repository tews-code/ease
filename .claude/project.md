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

**Phase 1: In Progress** (Hello QEMU)

Completed:
- [x] `#![no_std]` and `#![no_main]` in src/lib.rs
- [x] `_start` entry point with `#[unsafe(no_mangle)]` (Rust 2024 syntax)
- [x] `cargo build` succeeds

Next steps:
- [ ] Add `loop {}` inside `_start` to prevent returning
- [ ] Create linker script `memory-qemu.x` (RAM at 0x80000000 for QEMU virt)
- [ ] Add rustflags to .cargo/config.toml: `[target.riscv32imac-unknown-none-elf] rustflags = ["-C", "link-arg=-Tmemory-qemu.x"]`
- [ ] Add panic handler
- [ ] Boot on QEMU and verify it runs
- [ ] Week 2: UART output at 0x10000000, print!/println! macros, "Hello from EASE!"

## Working Branch

Use `develop` branch for day-to-day work. Merge to `main` when stable.
