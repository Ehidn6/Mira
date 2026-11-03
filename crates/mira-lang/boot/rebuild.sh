#!/bin/sh
# Developer-only regeneration. Normal Cargo builds use the checked-in blob.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
as --32 multiboot.S -o "$tmp/boot.o"
ld -m elf_i386 -Ttext 0x100000 -e _start "$tmp/boot.o" -o "$tmp/boot.elf"
objcopy -O binary --only-section=.text "$tmp/boot.elf" "$tmp/multiboot.bin"
cp "$tmp/multiboot.bin" multiboot.bin
chmod 644 multiboot.bin
