#!/bin/bash
# Local CI script for EASE
# Run this before committing to catch issues early
#
# Usage:
#   ./scripts/ci.sh                          # full run (default test-all)
#   ./scripts/ci.sh --test=test-sched        # focused run: only test-sched
#                                            # in the QEMU stage; skips host
#                                            # tests, miri, and docs for fast
#                                            # iteration on one feature.
#   ./scripts/ci.sh --scheduler=sched-stride # (currently the only option)
#   ./scripts/ci.sh --paint-stack            # also paint stacks and print
#                                            # high-watermarks in the QEMU
#                                            # stage; off by default
#   ./scripts/ci.sh --trace                  # build the QEMU stage with the
#                                            # `trace` feature (scheduler
#                                            # trace points + panic dump);
#                                            # off by default
#   ./scripts/ci.sh --help                   # this message

set -e

TEST_SET="test-all"
PAINT_STACK=0
TRACE=0

for arg in "$@"; do
    case "$arg" in
        --test=*)      TEST_SET="${arg#*=}" ;;
        --paint-stack) PAINT_STACK=1 ;;
        --trace)       TRACE=1 ;;
        --help|-h)
            sed -n '2,19p' "$0"
            exit 0 ;;
        *)
            echo "error: unknown option '$arg' (try --help)" >&2
            exit 1 ;;
    esac
done

# Focused mode: a non-default --test skips orthogonal slow stages (host
# tests, miri, docs) so you can iterate rapidly on one feature. Pass
# nothing for the full run.
FOCUSED=0
[ "$TEST_SET" != "test-all" ] && FOCUSED=1

# Anchor to the crate root (this script's parent dir) so CI can be invoked
# from anywhere, e.g. the project root. Done after arg parsing so --help's
# `sed … "$0"` still resolves against the original CWD.
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

FEATURES="$TEST_SET"

# Stack painting + high-watermark printing is an opt-in diagnostic (the
# `paint-stack` feature). It's scoped to the RISC-V build — the watermark
# code is QEMU-only — so host tests, miri and docs keep the base feature
# set. Off unless --paint-stack is passed.
RISCV_FEATURES="$FEATURES"
[ $PAINT_STACK -eq 1 ] && RISCV_FEATURES="$RISCV_FEATURES paint-stack"

# Scheduler tracing (the `trace` feature) is likewise a QEMU-only diagnostic:
# it relies on the panic-handler dump and percpu/arch reads, so it's scoped
# to the RISC-V build and kept out of host tests, miri and docs. Off unless
# --trace is passed.
[ $TRACE -eq 1 ] && RISCV_FEATURES="$RISCV_FEATURES trace"

# The bump and freelist allocator modules are kept in-tree as reference
# implementations (with host_tests) but aren't wired into the kernel
# binary, so their code is legitimately dead from the kernel's point of
# view. Relax clippy's dead-code check.
CLIPPY_EXTRA="-A dead-code"

echo "Test set                : $TEST_SET"
[ $FOCUSED -eq 1 ] && echo "Focused mode            : skipping host tests, miri, docs"
[ $PAINT_STACK -eq 1 ] && echo "Stack painting          : on (printing high-watermarks in QEMU stage)"
[ $TRACE -eq 1 ] && echo "Tracing                 : on (trace feature in QEMU stage)"

# Unconditionally reformat to pass clippy
cargo fmt

echo ""
echo "=== Disk Image ==="
./scripts/mkdisk.sh

echo ""
echo "=== Clippy ==="
# Clippy must see the same feature set as the QEMU test run, otherwise
# cfg-gated code looks dead under the Cargo.toml default but live under
# the tested feature set (or vice versa), producing spurious dead_code
# errors.
cargo clippy --target riscv32imac-unknown-none-elf \
    --no-default-features --features "$RISCV_FEATURES" \
    -- -D warnings $CLIPPY_EXTRA

echo ""
echo "=== QEMU Tests ==="
cargo test --bin ease --no-default-features --features "$RISCV_FEATURES"

if [ $FOCUSED -eq 1 ]; then
    echo ""
    echo "✓ Focused checks passed (host tests, miri, docs, benchmarks skipped)"
    exit 0
fi

echo ""
echo "=== QEMU Benchmarks ==="
# `bench` is the SOLE gate for benchmark #[test_case]s (not ANDed with
# any area feature), so this compiles and runs ONLY the benchmarks — not
# the functional suite — so it doesn't re-run (or re-mutate) the FS
# tests. Regression gates assert on per-thread cpu cycles (src/bench.rs),
# which hold steady under QEMU wall-clock variance. Fresh disk because
# the virtio benchmark writes blocks.
# Focused benchmark iteration: ./scripts/ci.sh --test=bench
./scripts/mkdisk.sh
cargo test --bin ease --no-default-features --features "bench"

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
