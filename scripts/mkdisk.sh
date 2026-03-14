#!/bin/bash
# Create disk image for EASE

set -e  # Exit on failure

cd "$(dirname "$0")"/..
rm -f disk.img  # Delete previous image
dd if=/dev/zero of=disk.img bs=512 count=32768   # 32768 sectors x 512 bytes = 16MB of zeros
mkfs.fat -F 16 disk.img   # FAT16
mcopy -i disk.img disk/HELLO.txt ::HELLO.txt
mcopy -i disk.img disk/EMPTY.TXT ::EMPTY.TXT
mcopy -i disk.img disk/SHORT ::SHORT
mcopy -i disk.img disk/LONGNAME.END ::LONGNAME.END
mcopy -i disk.img disk/RP-008373-DS-2-rp2350-datasheet.pdf ::RP-008373-DS-2-rp2350-datasheet.pdf

