#!/bin/bash
# Build the user programs and boot EASE in QEMU with them
#
# Usage:
#   ./scripts/run-user.sh              # build ease-user blobs, then cargo run
#   ./scripts/run-user.sh --release    # extra args are passed to `cargo run`
#
# The kernel `include_bytes!`s the blobs from
# ../ease-user/target/riscv32imac-unknown-none-elf/debug/*.bin, and ease-user
# is outside the workspace, so a plain `cargo run` never rebuilds them. This
# script does that step first so edits to ease-user/src/bin/*.rs take effect.
# run.sh's env vars (EASE_DISPLAY, EASE_QEMU_ARGS, ...) still apply.
set -e

# Anchor to the ease crate root (this script's parent dir), like ci.sh
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== User Programs ==="
(cd ../ease-user && cargo build && ./scripts/userblob.sh)

echo "=== Kernel ==="
cargo run "$@"
