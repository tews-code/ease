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
# Uncomment when tests/ directory exists:
# cargo test --package ease --tests

echo ""
echo "=== Documentation ==="
cargo doc --no-deps --target riscv32imac-unknown-none-elf

echo ""
echo "✓ All checks passed!"
