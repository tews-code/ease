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
#   ./scripts/ci.sh --test=bench             # benchmarks only: the bench
#                                            # tables and regression gates
#                                            # (pinned to the host's fast
#                                            # cores by run.sh)
#   ./scripts/ci.sh --scheduler=sched-stride # (currently the only option)
#   ./scripts/ci.sh --paint-stack            # also paint stacks and print
#                                            # high-watermarks in the QEMU
#                                            # stage; off by default
#   ./scripts/ci.sh --trace                  # build the QEMU stage with the
#                                            # `trace` feature (scheduler
#                                            # trace points + panic dump);
#                                            # off by default
#   ./scripts/ci.sh --irqsoff                # build the QEMU stage with the
#                                            # `irqsoff` feature (interrupts-
#                                            # off tracer, report at end of
#                                            # run and on panic); off by default
#   ./scripts/ci.sh --fat16                  # build the FAT16 (superfloppy)
#                                            # test disk instead of the default
#                                            # FAT32 (MBR) image (see mkdisk.sh)
#   ./scripts/ci.sh --miri                   # also run the host tests under
#                                            # miri (needs the nightly miri
#                                            # component); off by default
#   ./scripts/ci.sh --all                    # the full run, then miri, then
#                                            # the QEMU stage again under each
#                                            # of --trace, --irqsoff and
#                                            # --paint-stack in turn
#   ./scripts/ci.sh --help                   # this message

set -e

TEST_SET="test-all"
PAINT_STACK=0
TRACE=0
IRQSOFF=0
MIRI=0
ALL=0
FS_TYPE="fat32"

for arg in "$@"; do
    case "$arg" in
        --test=*)      TEST_SET="${arg#*=}" ;;
        --paint-stack) PAINT_STACK=1 ;;
        --trace)       TRACE=1 ;;
        --irqsoff)     IRQSOFF=1 ;;
        --miri)        MIRI=1 ;;
        --all)         ALL=1; MIRI=1 ;;
        --fat16)       FS_TYPE="fat16" ;;
        --help|-h)
            sed -n '2,/^$/p' "$0"
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

# The FS volume tests are disk-specific: the default builds the FAT32 (MBR) test
# disk (see mkdisk.sh) and selects its `fat32` gate; `--fat16` builds the FAT16
# superfloppy and selects `fat16`. This feature rides along with whatever
# TEST_SET is in effect (test-all, a focused test-*, etc.).
FS_FEATURE="fat32"
[ "$FS_TYPE" = "fat16" ] && FS_FEATURE="fat16"
FEATURES="$TEST_SET $FS_FEATURE"

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

# Interrupts-off tracing (the `irqsoff` feature) hooks the interrupt-disable
# doors and trap entry, and prints a per-hart report at the end of the QEMU
# run (and on panic). Same scoping as trace. Off unless --irqsoff is passed.
[ $IRQSOFF -eq 1 ] && RISCV_FEATURES="$RISCV_FEATURES irqsoff"

# Opt-in diagnostics that no default run compiles. Checked every full run so
# they cannot bit-rot unnoticed (the trace feature did, once).
DIAG_FEATURES="paint-stack trace irqsoff profile"

# The diagnostics --all *runs* (not just compiles), one QEMU stage each, so a
# failure is attributed to a single feature. `profile` is compile-checked
# only: it has no functional suite of its own.
ALL_RUN_FEATURES="trace irqsoff paint-stack"

# The bump and freelist allocator modules are kept in-tree as reference
# implementations (with host_tests) but aren't wired into the kernel
# binary, so their code is legitimately dead from the kernel's point of
# view. Relax clippy's dead-code check.
CLIPPY_EXTRA="-A dead-code"

echo "Test set                : $TEST_SET"
echo "Disk image              : $FS_TYPE (feature: $FS_FEATURE)"
[ $FOCUSED -eq 1 ] && echo "Focused mode            : skipping host tests, miri, docs"
[ $PAINT_STACK -eq 1 ] && echo "Stack painting          : on (printing high-watermarks in QEMU stage)"
[ $TRACE -eq 1 ] && echo "Tracing                 : on (trace feature in QEMU stage)"
[ $IRQSOFF -eq 1 ] && echo "Interrupts-off tracing  : on (irqsoff feature in QEMU stage)"
[ $MIRI -eq 1 ] && [ $FOCUSED -eq 0 ] && echo "Miri                    : on (host tests under miri)"
[ $ALL -eq 1 ] && echo "All diagnostics         : on (QEMU stage re-run under: $ALL_RUN_FEATURES)"

# One QEMU stage: clippy, then the test binary, both with the given feature
# set. Clippy must see the same feature set as the QEMU test run, otherwise
# cfg-gated code looks dead under the Cargo.toml default but live under the
# tested feature set (or vice versa), producing spurious dead_code errors.
# Fresh disk first: the FS tests mutate it.
qemu_stage() {
    local features="$1"
    local label="$2"

    echo ""
    echo "=== Disk Image $label==="
    ./scripts/mkdisk.sh "$FS_TYPE"

    echo ""
    echo "=== Clippy $label==="
    cargo clippy --target riscv32imac-unknown-none-elf \
        --no-default-features --features "$features" \
        -- -D warnings $CLIPPY_EXTRA

    echo ""
    echo "=== QEMU Tests $label==="
    cargo test --bin ease --no-default-features --features "$features"
}

# Unconditionally reformat to pass clippy
cargo fmt

echo ""
echo "=== User Programs ==="
# The kernel `include_bytes!`s the user program blobs from
# ../ease-user/target/riscv32imac-unknown-none-elf/debug/*.bin, so they must
# exist (and be fresh) before any kernel compile below. ease-user is excluded
# from the workspace (it links against its own user-window linker script), so
# it gets its own cargo invocation here; userblob.sh then objcopies each ELF
# in src/bin/ to a flat binary and checks it is the fixed program size.
(cd ../ease-user && cargo build && ./scripts/userblob.sh)

qemu_stage "$RISCV_FEATURES" ""

if [ $FOCUSED -eq 0 ]; then
    echo ""
    echo "=== Diagnostic Features Check ==="
    # Compile-only, with every opt-in diagnostic on top of the tested set.
    cargo clippy --target riscv32imac-unknown-none-elf \
        --no-default-features --features "$FEATURES $DIAG_FEATURES" \
        -- -D warnings $CLIPPY_EXTRA
fi

# The --all diagnostic re-runs, one QEMU stage per feature on top of the
# tested set. Also honoured in focused mode (e.g. --all --test=test-sched
# re-runs just that suite under each diagnostic).
all_diagnostic_runs() {
    local diag
    for diag in $ALL_RUN_FEATURES; do
        qemu_stage "$FEATURES $diag" "($diag) "
    done
}

if [ $FOCUSED -eq 1 ]; then
    [ $ALL -eq 1 ] && all_diagnostic_runs
    echo ""
    echo "✓ Focused checks passed (host tests, miri, docs, benchmarks skipped)"
    exit 0
fi

echo ""
echo "=== QEMU Benchmarks ==="
# `bench` is the SOLE gate for benchmark #[test_case]s (not ANDed with
# any area feature), so this compiles and runs ONLY the benchmarks — not
# the functional suite — so it doesn't re-run (or re-mutate) the FS
# tests. Most regression gates assert on per-thread cpu cycles
# (src/bench.rs), which hold steady under QEMU wall-clock variance. The FS
# benchmarks (src/fs/bench.rs) instead gate on deterministic block-I/O
# counts, which catch a FAT-cache or read/write-amplification regression
# directly. All of these run here under `set -e`, so a regression fails CI.
# Fresh disk because the virtio and FS benchmarks write blocks.
# Focused benchmark iteration: ./scripts/ci.sh --test=bench
./scripts/mkdisk.sh "$FS_TYPE"
# Pass the FS feature (fat16/fat32) alongside `bench` so the format-specific
# benchmark asserts compile and run against the matching disk. `bench` is still
# the sole gate for the benchmark test_cases — the FS feature only selects
# which per-format bound is checked (it does NOT pull in test-fs functional
# tests, which are gated separately).
cargo test --bin ease --no-default-features --features "bench $FS_FEATURE"

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

if [ $MIRI -eq 1 ]; then
    echo ""
    echo "=== Miri (host) ==="
    # Opt-in (--miri or --all): it is the slowest host stage. Asked for
    # explicitly, so a missing component is an error, not a skip.
    if ! rustup +nightly component list --installed 2>/dev/null | grep -q '^miri'; then
        echo "error: miri requested but not installed: 'rustup +nightly component add miri'" >&2
        exit 1
    fi
    cargo +nightly miri test --package ease --lib --target "$HOST_TARGET" \
        --no-default-features --features "$FEATURES"
fi

echo ""
echo "=== Documentation ==="
cargo doc --no-deps --target riscv32imac-unknown-none-elf

[ $ALL -eq 1 ] && all_diagnostic_runs

echo ""
echo "✓ All checks passed!"
