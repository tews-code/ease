#!/bin/bash
# Local CI script for EASE
# Run this before committing to catch issues early

set -e  # Exit immediately on any failure

echo "=== Format Check ==="
cargo fmt --check

echo ""
echo "=== Clippy ==="
cargo clippy --target riscv32imac-unknown-none-elf -- -D warnings

echo ""
echo "=== QEMU Tests ==="
cargo test --bin ease

echo ""
echo "=== Host Tests ==="
# Host tests run on native target, not RISC-V (which has no std)
# Note: Only --lib works because --tests also compiles the binary which has RISC-V asm
HOST_TARGET=$(rustc --version --verbose | grep host | cut -d' ' -f2)
cargo test --package ease --lib --target "$HOST_TARGET"
# TODO: Enable when binary is target-conditional
# cargo test --package ease --tests --target "$HOST_TARGET"

echo ""
echo "=== Documentation ==="
cargo doc --no-deps --target riscv32imac-unknown-none-elf

echo ""
echo "✓ All checks passed!"
