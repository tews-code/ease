#!/bin/bash
set -e

#QEMU file path
QEMU=qemu-system-riscv32

# Anchor disk.img to the crate root (this script's parent dir), where
# mkdisk.sh creates it, so QEMU finds it regardless of the runner's CWD.
CRATE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ELF="$1"
FLASH_BIN="$(dirname "$ELF")/flash.bin"

# Display policy: test binaries (cargo test builds them under deps/) run
# headless — the GUI repaint path costs >50% of QEMU's CPU and distorts
# the timing tests (see project notes, 2026-06-05 investigation).
# Interactive `cargo run` gets a window. Override with EASE_DISPLAY=none
# or EASE_DISPLAY=<backend>. Keep the window unscaled (1:1) — bilinear
# scaling is the slow path.
case "${EASE_DISPLAY:-auto}" in
    auto) case "$ELF" in
              */deps/*) DISPLAY_ARG="-display none" ;;
              *)        DISPLAY_ARG="" ;;
          esac ;;
    none) DISPLAY_ARG="-display none" ;;
    *)    DISPLAY_ARG="-display ${EASE_DISPLAY}" ;;
esac

# We pad to 0x22000000 because QEMU virt requires 32MB
rust-objcopy -O binary \
    --pad-to=0x22000000 \
    --gap-fill=0xff \
    "$ELF" "$FLASH_BIN"

#Start QEMU
# QEMU virt requires 32MB even though we model 16MB
$QEMU -machine virt -bios none -device ramfb $DISPLAY_ARG -serial stdio \
    -drive id=drive0,file="$CRATE_ROOT/disk.img",format=raw,if=none \
    -device virtio-blk-device,drive=drive0,bus=virtio-mmio-bus.0 \
    -drive if=pflash,unit=0,format=raw,file="$FLASH_BIN",readonly=on \
    -m 32M \
    -smp 2 \
    -no-reboot \
    -kernel "$ELF"  # Cargo provides kernel in argument $1
