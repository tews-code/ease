#!/bin/bash
# Local CI script for EASE
# Run this before committing to catch issues early

set -e  # Exit immediately on any failure

ALLOCATOR="${1:-alloc-freelist}"
FEATURES="${TEST_SET:-test-all},$ALLOCATOR"

case "$ALLOCATOR" in
    alloc-slab|alloc-freelist|alloc-bump) ;;
    *) echo "error: unknown allocator '$ALLOCATOR'" >&2
       echo "       expected one of: alloc-slab, alloc-freelist, alloc-bump" >&2
       exit 1 ;;
esac

# Unconditionally reformat to pass clippy
cargo fmt

echo ""
echo "=== Disk Image ==="
./scripts/mkdisk.sh

echo ""
echo "=== Clippy ==="
cargo clippy --target riscv32imac-unknown-none-elf -- -D warnings

echo ""
echo "=== QEMU Tests ==="
cargo test --bin ease --no-default-features --features "$FEATURES"

echo ""
echo "=== Host Tests ==="
# Host tests run on native target, not RISC-V (which has no std)
# Note: Only --lib works because --tests also compiles the binary which has RISC-V asm
HOST_TARGET=$(rustc --version --verbose | grep host | cut -d' ' -f2)
cargo test --package ease --lib --target "$HOST_TARGET"
# TODO: Enable when binary is target-conditional
# cargo test --package ease --tests --target "$HOST_TARGET"

echo ""
echo "=== Miri (host) ==="
if rustup +nightly component list --installed 2>/dev/null | grep -q '^miri'; then
    cargo +nightly miri test --package ease --lib --target "$HOST_TARGET"
else
    echo "skipped: 'rustup +nightly component add miri' to enable"
fi

echo ""
echo "=== Documentation ==="
cargo doc --no-deps --target riscv32imac-unknown-none-elf

echo ""
echo "✓ All checks passed!"
