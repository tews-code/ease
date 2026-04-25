#!/bin/bash
# Local CI script for EASE
# Run this before committing to catch issues early

set -e  # Exit immediately on any failure

ALLOCATOR="${1:-alloc-kalloc}"
TEST_SET="${2:-${TEST_SET:-test-all}}"
FEATURES="$TEST_SET,$ALLOCATOR"

case "$ALLOCATOR" in
    alloc-slab|alloc-freelist|alloc-bump|alloc-buddy|alloc-kalloc) ;;
    *) echo "error: unknown allocator '$ALLOCATOR'" >&2
       echo "       expected one of: alloc-slab, alloc-freelist, alloc-bump, alloc-buddy, alloc-tier" >&2
       exit 1 ;;
esac

# Allocator modules (bump, freelist, slab, buddy, tier) are declared
# unconditionally so the tier can pull in slab and buddy. That means every
# allocator-specific build has dead code in the inactive allocators —
# legitimate, not a regression. Relax clippy's dead-code check for all
# allocator builds. Additionally, the slab build excludes virtio/fs init
# (allocations larger than one slot), which leaves more code dead.
CLIPPY_EXTRA="-A dead-code"

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
# Host tests run on native target, not RISC-V (which has no std).
# Same feature set as the QEMU run so test-* gating selects the same
# host_tests modules: e.g. test-alloc selects allocator host_tests,
# test-collections selects collection host_tests.
# Note: Only --lib works because --tests also compiles the binary which has RISC-V asm
HOST_TARGET=$(rustc --version --verbose | grep host | cut -d' ' -f2)
cargo test --package ease --lib --target "$HOST_TARGET" \
    --no-default-features --features "$FEATURES"
# TODO: Enable when binary is target-conditional
# cargo test --package ease --tests --target "$HOST_TARGET"

echo ""
echo "=== Miri (host) ==="
if rustup +nightly component list --installed 2>/dev/null | grep -q '^miri'; then
    cargo +nightly miri test --package ease --lib --target "$HOST_TARGET" \
        --no-default-features --features "$FEATURES"
else
    echo "skipped: 'rustup +nightly component add miri' to enable"
fi

echo ""
echo "=== Documentation ==="
cargo doc --no-deps --target riscv32imac-unknown-none-elf

echo ""
echo "✓ All checks passed!"
