#!/bin/bash
set -e

# `{BASH_SOURCE[0]}` gives the path of this script
# dirname strips any trailing / or changes to . if no slashes
# `cd ... && pwd` is a trick to first change to that directory and then
# print that working directory - setting the starting point
# Outer `cd` then changes to that constructed dir
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TARGET_DIR="target/riscv32imac-unknown-none-elf/debug"
LAYOUT="../ease-abi/memory-shared-qemu.x"

# Read `__user_<name>_size = <value>;` from the shared linker script and print
# it in bytes. Accepts decimal or 0x hex, with an optional K or M suffix.
layout_size() {
    local raw
    raw=$(sed -n "s/^[[:space:]]*__user_$1_size[[:space:]]*=[[:space:]]*\([^;]*\);.*/\1/p" "$LAYOUT")
    raw=${raw//[[:space:]]/}
    case "$raw" in
        *K) echo $(( ${raw%K} * 1024 )) ;;
        *M) echo $(( ${raw%M} * 1024 * 1024 )) ;;
        "") echo "error: __user_$1_size not found in $LAYOUT" >&2; exit 1 ;;
        *) echo $(( raw )) ;;
    esac
}

# The flat binary holds .text and .data (bss is NOLOAD), each padded to its window
text_size=$(layout_size text)
data_size=$(layout_size data)
expected=$(( text_size + data_size ))

for src in src/bin/*.rs; do
    bin=$(basename "$src" .rs)
    elf="$TARGET_DIR/$bin"
    blob="$TARGET_DIR/$bin.bin"
    rust-objcopy -O binary "$elf" "$blob"

    size=$(stat -c %s "$blob")
    [ "$size" -eq "$expected" ] || {
        echo "error: $blob is $size bytes, expected $expected (text + data windows in $LAYOUT)" >&2
        exit 1
    }
done
