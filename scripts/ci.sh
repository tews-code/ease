#!/bin/bash
# Local CI script for EASE
# Run this before committing to catch issues early

set -e  # Exit immediately on any failure

ALLOCATOR="${1:-alloc-buddy}"
FEATURES="${TEST_SET:-test-all},$ALLOCATOR"

case "$ALLOCATOR" in
    alloc-slab|alloc-freelist|alloc-bump|alloc-buddy) ;;
    *) echo "error: unknown allocator '$ALLOCATOR'" >&2
       echo "       expected one of: alloc-slab, alloc-freelist, alloc-bump, alloc-buddy" >&2
       exit 1 ;;
esac

# The slab build is a cutdown configuration that deliberately excludes
# virtio/fs init (they need allocations larger than one slab slot), which
# in turn leaves legitimately-unused code dead under this build. Relax
# clippy's dead-code check for slab only.
CLIPPY_EXTRA=""
if [ "$ALLOCATOR" = "alloc-slab" ]; then
    CLIPPY_EXTRA="-A dead-code"
fi

echo "Global allocator set to : $ALLOCATOR";

# Unconditionally reformat to pass clippy
cargo fmt

echo ""
echo "=== Disk Image ==="
./scripts/mkdisk.sh

echo ""
echo "=== Clippy ==="
# Clippy must see the same feature set as the QEMU test run, otherwise
# cfg-gated code (e.g. virtio/fs init under alloc-slab) looks dead under
# the Cargo.toml default but live under the tested feature set (or vice
# versa), producing spurious dead_code errors.
cargo clippy --target riscv32imac-unknown-none-elf \
    --no-default-features --features "$FEATURES" \
    -- -D warnings $CLIPPY_EXTRA

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
