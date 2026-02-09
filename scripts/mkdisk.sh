#!/bin/bash
# Create disk image for EASE

set -e  # Exit on failure

cd "$(dirname "$0")"/..
rm -f disk.img  # Delete previous image
dd if=/dev/zero of=disk.img bs=512 count=32768   # 16384 sectors x 512 bytes = 8MB of zeros
mkfs.fat -F 16 disk.img   # FAT16
mcopy -i disk.img disk/HELLO.txt ::HELLO.txt  # Copy HELLO.txt to the image
