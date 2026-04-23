#!/bin/bash
set -e

#QEMU file path
QEMU=qemu-system-riscv32

ELF="$1"
FLASH_BIN="$(dirname "$ELF")/flash.bin"

rust-objcopy -O binary \
    --only-section=.text --only-section=.rodata \
    --pad-to=0x22000000 --gap-fill=0xff \
    "$ELF" "$FLASH_BIN"

#Start QEMU
$QEMU -machine virt -bios none -device ramfb -serial stdio \
    -drive id=drive0,file=disk.img,format=raw,if=none \
    -device virtio-blk-device,drive=drive0,bus=virtio-mmio-bus.0 \
    -drive if=pflash,unit=0,format=raw,file="$FLASH_BIN",readonly=on \
    -m 32M \
    -smp 2 \
    -no-reboot \
    -kernel "$ELF"  # Cargo provides kernel in argument $1
