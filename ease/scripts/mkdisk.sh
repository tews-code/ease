#!/bin/bash
# Create disk image for EASE
#
# Usage:
#   ./mkdisk.sh          # partitionless FAT16 superfloppy (default)
#   ./mkdisk.sh fat16    # same
#   ./mkdisk.sh fat32    # MBR + single FAT32 (LBA) partition
#
# FAT16 is a "superfloppy": the BPB sits at sector 0 with no partition table,
# which is what the kernel mounts today. FAT32 is MBR-partitioned with one
# FAT32 partition at LBA 2048; it will NOT mount until the kernel gains FAT32
# support (find_partition currently rejects it as Fat32Detected). It exists so
# that FAT32 work can iterate against a real image via `ci.sh --fat32`.

set -e  # Exit on failure

cd "$(dirname "$0")"/..

FS_TYPE="${1:-fat16}"

# Copy the shared test-file set into an mtools image spec:
#   $1 = "disk.img"               (superfloppy)
#      | "disk.img@@<byte-offset>" (a partition within the image)
copy_test_files() {
    local img="$1"
    mcopy -i "$img" disk/HELLO.txt ::HELLO.txt
    mcopy -i "$img" disk/EMPTY.TXT ::EMPTY.TXT
    mcopy -i "$img" disk/SHORT ::SHORT
    mcopy -i "$img" disk/LONGNAME.END ::LONGNAME.END
    mcopy -i "$img" disk/BIG.TXT ::BIG.TXT
    mcopy -i "$img" disk/DELETE.ME ::DELETE.ME
    mcopy -i "$img" disk/RP-008373-DS-2-rp2350-datasheet.pdf ::RP-008373-DS-2-rp2350-datasheet.pdf
}

rm -f disk.img  # Delete previous image

case "$FS_TYPE" in
    fat16)
        dd if=/dev/zero of=disk.img bs=512 count=32768   # 32768 x 512 = 16MB of zeros
        mkfs.fat -F 16 disk.img   # FAT16, whole disk (no partition table)
        copy_test_files disk.img
        ;;
    fat32)
        # FAT32 needs >= 65525 clusters, so size generously: 196608 x 512 = 96MB.
        dd if=/dev/zero of=disk.img bs=512 count=196608
        # One FAT32 (LBA, type 0x0c) partition starting at sector 2048 (1 MiB).
        # Heredoc body must stay at column 0 (sfdisk script input).
        sfdisk disk.img <<'PARTITION'
label: dos
start=2048, type=0c
PARTITION
        # Format that partition in place, then copy files in at its byte offset.
        mkfs.fat -F 32 --offset 2048 disk.img
        copy_test_files "disk.img@@$((2048 * 512))"
        ;;
    *)
        echo "mkdisk.sh: unknown filesystem type '$FS_TYPE' (want: fat16 | fat32)" >&2
        exit 1
        ;;
esac
