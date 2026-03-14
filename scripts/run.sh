#!/bin/bash
set -e

#QEMU file path
QEMU=qemu-system-riscv32

#Start QEMU
$QEMU -machine virt -bios none -device ramfb -serial stdio \
    -drive id=drive0,file=disk.img,format=raw,if=none \
    -device virtio-blk-device,drive=drive0,bus=virtio-mmio-bus.0 \
    -m 32M \
    -no-reboot \
    -kernel "$1"  # Cargo provides kernel in argument $1
