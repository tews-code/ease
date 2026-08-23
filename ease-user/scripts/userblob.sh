#!/bin/bash
set -e

# `{BASH_SOURCE[0]}` gives the path of this script
# dirname strips any trailing / or changes to . if no slashes
# `cd ... && pwd` is a trick to first change to that directory and then
# print that working directory - setting the starting point
# Outer `cd` then changes to that constructed dir
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TARGET_DIR="target/riscv32imac-unknown-none-elf/debug"

for src in src/bin/*.rs; do
    bin=$(basename "$src" .rs)
    elf="$TARGET_DIR/$bin"
    blob="$TARGET_DIR/$bin.bin"
    rust-objcopy -O binary "$elf" "$blob"

    size=$(stat -c %s "$blob")
    [ "$size" -eq 6144 ] || {
        echo "error: $blob is $size bytes, expected 6144" >&2
        exit 1
    }
done
